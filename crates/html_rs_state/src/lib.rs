//! # html_rs_state
//!
//! Fine-grained reactive **state** — the same mental model as the signals in
//! modern JS frameworks, but in Rust and single-threaded (UI state never crosses threads).
//!
//! ```ignore
//! let count = State::new(0);
//!
//! create_effect(move || {
//!     println!("count is {}", count.get()); // reads are tracked
//! });
//!
//! count.set(1); // → effect re-runs automatically
//! count.update(|c| *c += 1); // → and again
//! ```
//!
//! Key properties:
//! - `State<T>` is [`Copy`] — clone it into closures freely, exactly like JS-framework state.
//! - Reads inside [`create_effect`] / [`create_memo`] are tracked automatically.
//! - Writes batch automatically: an effect re-runs at most once per "flush".
//! - [`batch`] groups many writes into a single flush; [`untrack`] reads without subscribing.

use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

/// A set of `effect_id -> ()` subscribers attached to one state cell.
type Subscribers = Rc<RefCell<HashMap<u64, ()>>>;

struct EffectNode {
    func: Box<dyn FnMut()>,
    active: bool,
    /// Subscription maps this effect currently depends on (for cleanup on re-run).
    deps: Vec<Subscribers>,
}

struct Running {
    /// `0` means "untracked frame" (pushed by [`untrack`]).
    id: u64,
    deps: Vec<Subscribers>,
}

struct Runtime {
    next_id: u64,
    /// Type-erased state cells, keyed by state id.
    cells: HashMap<u64, Box<dyn Any>>,
    effects: HashMap<u64, EffectNode>,
    running: Vec<Running>,
    running_ids: HashSet<u64>,
    /// FIFO queue of effect ids waiting to re-run.
    pending: Vec<u64>,
    pending_set: HashSet<u64>,
    flushing: bool,
    batch_depth: usize,
    flush_hook: Option<Box<dyn Fn()>>,
}

impl Runtime {
    fn new() -> Self {
        Self {
            next_id: 0,
            cells: HashMap::new(),
            effects: HashMap::new(),
            running: Vec::new(),
            running_ids: HashSet::new(),
            pending: Vec::new(),
            pending_set: HashSet::new(),
            flushing: false,
            batch_depth: 0,
            flush_hook: None,
        }
    }
}

thread_local! {
    static RUNTIME: RefCell<Runtime> = RefCell::new(Runtime::new());
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// A reactive value. `State<T>` is [`Copy`], so you can capture it in any
/// number of closures (event handlers, effects, memos) without cloning.
///
/// ```ignore
/// let count = State::new(0);
/// count.get();      // tracked read (subscribes the running effect)
/// count.get_untracked(); // untracked read
/// count.set(5);     // write → notifies subscribers
/// count.update(|c| *c += 1); // mutate in place
/// ```
pub struct State<T: 'static> {
    id: u64,
    _marker: PhantomData<fn() -> T>,
}

impl<T: 'static> Clone for State<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: 'static> Copy for State<T> {}

impl<T: 'static> PartialEq for State<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl<T: 'static> Eq for State<T> {}

impl<T: 'static> fmt::Debug for State<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "State({})", self.id)
    }
}

struct StateData<T> {
    value: RefCell<T>,
    subscribers: Subscribers,
}

impl<T: 'static> State<T> {
    /// Creates a new state holding `value`.
    pub fn new(value: T) -> Self {
        let id = RUNTIME.with(|rt| {
            let mut rt = rt.borrow_mut();
            rt.next_id += 1;
            let id = rt.next_id;
            rt.cells
                .insert(id, Box::new(StateData::<T> {
                    value: RefCell::new(value),
                    subscribers: Rc::new(RefCell::new(HashMap::new())),
                }));
            id
        });
        Self { id, _marker: PhantomData }
    }

    /// Tracked read: returns a clone of the current value and subscribes the
    /// currently-running effect (if any) to this state.
    pub fn get(&self) -> T
    where
        T: Clone,
    {
        self.track();
        self.get_untracked()
    }

    /// Untracked read: no subscription is created.
    pub fn get_untracked(&self) -> T
    where
        T: Clone,
    {
        RUNTIME.with(|rt| {
            let rt = rt.borrow();
            match rt.cells.get(&self.id) {
                Some(cell) => cell
                    .downcast_ref::<StateData<T>>()
                    .expect("state type mismatch")
                    .value
                    .borrow()
                    .clone(),
                None => panic!("state read after runtime reset"),
            }
        })
    }

    /// Subscribes the currently-running effect to this state without reading it.
    pub fn track(&self) {
        RUNTIME.with(|rt| {
            let mut rt = rt.borrow_mut();
            let subs = match rt.cells.get(&self.id) {
                Some(cell) => cell
                    .downcast_ref::<StateData<T>>()
                    .expect("state type mismatch")
                    .subscribers
                    .clone(),
                None => return,
            };
            if let Some(frame) = rt.running.last_mut() {
                if frame.id != 0 {
                    subs.borrow_mut().insert(frame.id, ());
                    frame.deps.push(subs);
                }
            }
        });
    }

    /// Overwrites the value and notifies subscribers.
    pub fn set(&self, value: T) {
        self.update(|v| *v = value);
    }

    /// Mutates the value in place and notifies subscribers (even if unchanged —
    /// html.rs re-renders are cheap and idempotent).
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        // Mutate under a short runtime borrow; `f` itself may read/write other
        // state, so it must not run under an exclusive borrow.
        {
            RUNTIME.with(|rt| {
                let rt = rt.borrow();
                let cell = rt
                    .cells
                    .get(&self.id)
                    .expect("state used after drop")
                    .downcast_ref::<StateData<T>>()
                    .expect("state type mismatch");
                f(&mut cell.value.borrow_mut());
            });
        }
        // Notify subscribers without holding the runtime borrow —
        // `queue_effect` needs `&mut` access to the runtime.
        let subs = RUNTIME.with(|rt| {
            let rt = rt.borrow();
            rt.cells
                .get(&self.id)
                .unwrap()
                .downcast_ref::<StateData<T>>()
                .unwrap()
                .subscribers
                .clone()
        });
        let ids: Vec<u64> = subs.borrow().keys().copied().collect();
        for id in ids {
            queue_effect(id);
        }
    }
}

// ---------------------------------------------------------------------------
// Effects / memos
// ---------------------------------------------------------------------------

fn queue_effect(id: u64) {
    let flush_now = RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        if rt.running_ids.contains(&id) {
            // Never re-enter the effect that is currently running because of
            // its own write — that would loop forever.
            return None;
        }
        if rt.pending_set.insert(id) {
            rt.pending.push(id);
        }
        // Outside of a batch, effects flush synchronously after the write —
        // like a JS framework's microtask flush, but immediate. Inside a batch we stay
        // queued until `batch` finishes and calls `drain_effects`.
        if rt.batch_depth == 0 && !rt.flushing {
            rt.flushing = true;
            Some(true)
        } else {
            Some(false)
        }
    });
    if flush_now == Some(true) {
        drain_effects();
    }
}

fn run_effect(id: u64) {
    // Phase 1: bookkeeping under a short borrow.
    let prep = RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        let active = match rt.effects.get(&id) {
            Some(fx) => fx.active,
            None => return None,
        };
        if !active {
            return None;
        }
        let func = std::mem::replace(&mut rt.effects.get_mut(&id).unwrap().func, Box::new(|| {}));
        let old_deps = std::mem::take(&mut rt.effects.get_mut(&id).unwrap().deps);
        rt.running.push(Running { id, deps: Vec::new() });
        rt.running_ids.insert(id);
        Some((func, old_deps))
    });
    let Some((mut func, old_deps)) = prep else { return };

    // Phase 2: unsubscribe from stale dependencies, run outside the runtime borrow
    // (the effect body freely calls `get`/`set`).
    for subs in &old_deps {
        subs.borrow_mut().remove(&id);
    }
    func();

    // Phase 3: persist the dependency set collected during the run and put the
    // effect's function back so later notifications can run it again.
    RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        rt.running_ids.remove(&id);
        if let Some(frame) = rt.running.pop() {
            if let Some(fx) = rt.effects.get_mut(&id) {
                fx.func = func;
                fx.deps = frame.deps;
            }
        }
    });
    drop(old_deps);
}

fn drain_effects() {
    loop {
        let next = RUNTIME.with(|rt| {
            let mut rt = rt.borrow_mut();
            if rt.pending.is_empty() {
                None
            } else {
                let id = rt.pending.remove(0);
                rt.pending_set.remove(&id);
                Some(id)
            }
        });
        match next {
            Some(id) => run_effect(id),
            None => break,
        }
    }
    // One flush = one repaint request for the whole batch of writes.
    RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        rt.flushing = false;
        if let Some(hook) = &rt.flush_hook {
            hook();
        }
    });
}

/// Runs `f` immediately and re-runs it whenever any state it reads changes.
/// Returns a handle; effects live for the rest of the app unless [`Effect::stop`] is called.
pub fn create_effect(f: impl FnMut() + 'static) -> Effect {
    let id = RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        rt.next_id += 1;
        let id = rt.next_id;
        rt.effects.insert(
            id,
            EffectNode { func: Box::new(f), active: true, deps: Vec::new() },
        );
        id
    });
    run_effect(id);
    Effect { id }
}

/// A live effect. Drop it (or call [`Effect::stop`]) to deactivate.
pub struct Effect {
    id: u64,
}

impl Effect {
    /// Deactivates the effect: it will never run again.
    pub fn stop(&self) {
        RUNTIME.with(|rt| {
            let mut rt = rt.borrow_mut();
            if let Some(fx) = rt.effects.get_mut(&self.id) {
                fx.active = false;
                for subs in &fx.deps {
                    subs.borrow_mut().remove(&self.id);
                }
            }
        });
    }
}

/// A derived state: recomputed lazily-but-eagerly via an internal effect
/// whenever its inputs change. Reads are tracked like any state.
///
/// ```ignore
/// let count = State::new(2);
/// let doubled = create_memo(move || count.get() * 2);
/// ```
pub fn create_memo<T: Clone + 'static>(f: impl Fn() -> T + 'static) -> State<T> {
    let out = State::new(f());
    let back = out;
    create_effect(move || {
        let v = f();
        back.set(v);
    });
    out
}

/// Groups writes so effects flush **once** at the end instead of after each write.
pub fn batch<R>(f: impl FnOnce() -> R) -> R {
    RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        rt.batch_depth += 1;
        rt.flushing = true;
    });
    let out = f();
    let flush = RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        rt.batch_depth -= 1;
        rt.batch_depth == 0
    });
    if flush {
        drain_effects();
    }
    out
}

/// Runs `f` with dependency tracking disabled.
pub fn untrack<R>(f: impl FnOnce() -> R) -> R {
    RUNTIME.with(|rt| {
        let mut rt = rt.borrow_mut();
        rt.running.push(Running { id: 0, deps: Vec::new() });
    });
    let out = f();
    RUNTIME.with(|rt| {
        rt.borrow_mut().running.pop();
    });
    out
}

/// Registers a hook called after every flush in which effects ran.
/// html.rs uses this to coalesce repaint requests.
pub fn set_flush_hook(f: impl Fn() + 'static) {
    RUNTIME.with(|rt| {
        rt.borrow_mut().flush_hook = Some(Box::new(f));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_reactivity() {
        let count = State::new(0);
        let runs = State::new(0);
        create_effect(move || {
            let _ = count.get();
            runs.update(|r| *r += 1);
        });
        assert_eq!(runs.get(), 1);
        count.set(1);
        count.set(2);
        assert_eq!(runs.get(), 3);
    }

    #[test]
    fn batch_coalesces() {
        let a = State::new(0);
        let b = State::new(0);
        let runs = State::new(0);
        create_effect(move || {
            let _ = (a.get(), b.get());
            runs.update(|r| *r += 1);
        });
        assert_eq!(runs.get(), 1);
        batch(|| {
            a.set(1);
            b.set(2);
            a.set(3);
        });
        assert_eq!(runs.get(), 2);
    }

    #[test]
    fn memo_is_derived() {
        let a = State::new(2);
        let b = create_memo(move || a.get() * 10);
        assert_eq!(b.get(), 20);
        a.set(5);
        assert_eq!(b.get(), 50);
    }

    #[test]
    fn untracked_reads_dont_subscribe() {
        let a = State::new(0);
        let runs = State::new(0);
        create_effect(move || {
            let v = untrack(|| a.get());
            runs.update(|r| *r += v + 1);
        });
        assert_eq!(runs.get(), 1);
        a.set(10);
        assert_eq!(runs.get(), 1); // not re-run
    }
}
