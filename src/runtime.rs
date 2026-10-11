//! The winit runtime: window lifecycle, the render effect, event dispatch and
//! focus management. Runs natively *and* on the web (wasm32 via Trunk).

use crate::{Element, Node};
use html_rs_render::{DrawList, Renderer};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{CursorIcon, Window, WindowId};

/// How often the caret blinks (on/off phase length).
const CARET_BLINK: Duration = Duration::from_millis(500);
/// Max gap + movement between two clicks that still counts as a double-click.
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_SLOP_PX: f32 = 6.0;

/// Builder returned by [`launch`].
pub struct App {
    title: String,
    width: f64,
    height: f64,
    mode: html_rs_core::ThemeMode,
    view: Box<dyn Fn() -> Element>,
}

/// Launch an html.rs app with a view function — a closure returning the
/// element tree, re-evaluated automatically whenever state it reads changes.
///
/// ```ignore
/// launch(move || ui! { <p class="text-white">"Hello"</p> }).title("Hi").run();
/// ```
pub fn launch(view: impl Fn() -> Element + 'static) -> App {
    App {
        title: "html.rs".into(),
        width: 800.0,
        height: 600.0,
        mode: html_rs_core::ThemeMode::Dark,
        view: Box::new(view),
    }
}

impl App {
    /// Window title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Initial window size (logical px).
    pub fn size(mut self, width: f64, height: f64) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Theme mode for the window: `"light"` or `"dark"` (also accepts a
    /// [`html_rs_core::ThemeMode`]). Defaults to dark. Components (buttons,
    /// selects, inputs, dialogs, …) pick up their shadcn Slate colors from
    /// this; unknown values fall back to dark.
    ///
    /// ```ignore
    /// launch(view).light_mode("light").run();
    /// ```
    pub fn light_mode(mut self, mode: impl Into<html_rs_core::ThemeMode>) -> Self {
        self.mode = mode.into();
        self
    }

    /// Builds the event loop and runs until the window is closed.
    pub fn run(self) {
        // Theme must be set before the first paint so tag defaults and
        // token classes resolve against the right palette.
        html_rs_core::set_theme_mode(self.mode);
        let shared = Rc::new(Shared {
            title: self.title,
            width: self.width,
            height: self.height,
            view: self.view,
            tree: RefCell::new(Element::new("div")),
            dirty: Cell::new(true),
            ever_rendered: Cell::new(false),
            window: RefCell::new(None),
            renderer: RefCell::new(None),
            draw: RefCell::new(DrawList::empty()),
            focus: RefCell::new(None),
            hovered: RefCell::new(None),
            drag_range: RefCell::new(None),
            modifiers: Cell::new(winit::keyboard::ModifiersState::default()),
            last_click: Cell::new(None),
        });

        // The render effect: re-runs whenever state read by the view changes,
        // rebuilding the element tree and scheduling a repaint. When the new
        // tree is structurally equivalent to the previous one (a state write
        // that changed nothing visible), the repaint is skipped entirely.
        let s = shared.clone();
        html_rs_state::create_effect(move || {
            let tree = (s.view)();
            let changed = !s.ever_rendered.get()
                || !html_rs_core::equivalent(&s.tree.borrow(), &tree);
            if changed {
                s.ever_rendered.set(true);
                *s.tree.borrow_mut() = tree;
                s.dirty.set(true);
                if let Some(w) = s.window.borrow().as_ref() {
                    w.request_redraw();
                }
            }
        });

        let event_loop = EventLoop::builder().build().expect("event loop");
        let mut handler = AppHandler { shared };
        event_loop.run_app(&mut handler).expect("event loop error");
    }
}

struct Shared {
    title: String,
    width: f64,
    height: f64,
    view: Box<dyn Fn() -> Element>,
    tree: RefCell<Element>,
    dirty: Cell<bool>,
    /// Whether the render effect has run at least once (the initial `tree`
    /// placeholder must not be mistaken for "nothing changed").
    ever_rendered: Cell<bool>,
    window: RefCell<Option<Arc<Window>>>,
    renderer: RefCell<Option<Renderer>>,
    draw: RefCell<DrawList>,
    /// Focused element path (for `<input>`).
    focus: RefCell<Option<Vec<u32>>>,
    hovered: RefCell<Option<Vec<u32>>>,
    /// Path of the `<input type="range">` currently being dragged, if any.
    drag_range: RefCell<Option<Vec<u32>>>,
    /// Last reported keyboard modifiers (winit 0.30 delivers them separately).
    modifiers: Cell<winit::keyboard::ModifiersState>,
    /// Last left-click (time, x, y in physical px) for double-click detection.
    last_click: Cell<Option<(Instant, f32, f32)>>,
}

struct AppHandler {
    shared: Rc<Shared>,
}

impl AppHandler {
    fn repaint(&self) {
        if let Some(w) = self.shared.window.borrow().as_ref() {
            w.request_redraw();
        }
    }

    /// Resolves an element by its path (child indices from the root).
    fn element_at_path(&self, path: &[u32]) -> Option<Element> {
        let mut el = self.shared.tree.borrow().clone();
        for &i in path {
            el = match el.children.get(i as usize)? {
                Node::Element(e) => e.clone(),
                Node::Text(_) => return None,
            };
        }
        Some(el)
    }

    /// Text content of the element with an active select-all, if any.
    fn selected_text(&self) -> Option<String> {
        let key = {
            let r = self.shared.renderer.borrow();
            r.as_ref()?.inputs().selected_key()?.to_string()
        };
        let path: Vec<u32> = key.split('.').filter_map(|s| s.parse().ok()).collect();
        let el = self.element_at_path(&path)?;
        Some(element_text(&el))
    }

    /// Copies `text` to the system clipboard (best effort).
    fn copy_to_clipboard(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                let _ = cb.set_text(text.to_string());
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(win) = web_sys::window() {
                let clipboard = win.navigator().clipboard();
                let _ = clipboard.write_text(text);
            }
        }
    }

    /// Reads the system clipboard and runs `on_text` with its content
    /// (asynchronously on the web).
    fn paste_from_clipboard(&self, on_text: impl FnOnce(String) + 'static) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                if let Ok(text) = cb.get_text() {
                    on_text(text);
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(win) = web_sys::window() {
                let clipboard = win.navigator().clipboard();
                let promise = clipboard.read_text();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Ok(value) = wasm_bindgen_futures::JsFuture::from(promise).await {
                        if let Some(text) = value.as_string() {
                            on_text(text);
                        }
                    }
                });
            }
        }
    }

    fn focused_element(&self) -> Option<(Element, Vec<u32>)> {
        let focus = self.shared.focus.borrow().clone()?;
        let el = self.element_at_path(&focus)?;
        Some((el, focus))
    }

    /// Whether the element at `path` is an `<input type="range">`.
    fn is_range(&self, path: &[u32]) -> bool {
        self.element_at_path(path)
            .map(|el| el.tag == "input" && el.get_attr("type") == Some("range"))
            .unwrap_or(false)
    }

    /// Whether hovering this element changes how anything paints: the
    /// element carries `hover:` classes (its own paint changes), or it is
    /// an option row of an open select menu (highlight follows the cursor).
    fn is_hoverable(&self, path: &[u32]) -> bool {
        self.element_at_path(path)
            .map(|el| {
                el.classes
                    .iter()
                    .any(|c| c.split_whitespace().any(|t| t.starts_with("hover:")))
                    || el.tag == "option"
            })
            .unwrap_or(false)
    }

    /// Rebuilds layout + draw list and renders. Only called from RedrawRequested.
    fn redraw(&self) {
        let Some(window) = self.shared.window.borrow().as_ref().cloned() else { return };
        self.shared.dirty.set(false);
        let mut renderer_guard = self.shared.renderer.borrow_mut();
        let Some(renderer) = renderer_guard.as_mut() else { return };

        let scale = window.scale_factor() as f32;
        let size = window.inner_size();
        let tree = self.shared.tree.borrow().clone();
        let focus = self.shared.focus.borrow().clone();
        let hovered = self.shared.hovered.borrow().clone();

        let list = renderer.paint(
            &tree,
            size.width as f32,
            size.height as f32,
            scale,
            focus.as_deref(),
            hovered.as_deref(),
        );

        // Clear color: root element's bg if it set one, else the default.
        if let Some(bg) = html_rs_core::parse_classes(&tree.classes, 1.0).bg {
            renderer.set_clear_color(bg);
        }

        renderer.render(&list);
        *self.shared.draw.borrow_mut() = list;
    }
}

impl ApplicationHandler<()> for AppHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.shared.window.borrow().is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.shared.title.clone())
            .with_inner_size(winit::dpi::LogicalSize::new(self.shared.width, self.shared.height));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        #[cfg(target_arch = "wasm32")]
        {
            use winit::platform::web::WindowExtWebSys;
            let canvas = window.canvas().expect("winit window has no canvas");
            let doc = web_sys::window().unwrap().document().unwrap();
            doc.body().unwrap().append_child(&canvas).ok();
            let style = canvas.style();
            style.set_property("width", "100%").ok();
            style.set_property("height", "100%").ok();
            style.set_property("display", "block").ok();
            style.set_property("outline", "none").ok();

            let shared = self.shared.clone();
            let w = window.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let renderer = Renderer::new(w).await;
                *shared.renderer.borrow_mut() = Some(renderer);
                shared.dirty.set(true);
                if let Some(win) = shared.window.borrow().as_ref() {
                    win.request_redraw();
                }
            });
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let renderer = pollster::block_on(Renderer::new(window.clone()));
            *self.shared.renderer.borrow_mut() = Some(renderer);
            self.shared.dirty.set(true);
        }

        *self.shared.window.borrow_mut() = Some(window);
        self.repaint();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let Some(window) = self.shared.window.borrow().as_ref().cloned() else { return };
        if window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                    r.resize(size.width, size.height);
                }
                self.shared.dirty.set(true);
                window.request_redraw();
            }

            WindowEvent::ScaleFactorChanged { .. } => {
                // The follow-up Resized event carries the new physical size.
                self.shared.dirty.set(true);
            }

            WindowEvent::ModifiersChanged(modifiers) => {
                self.shared.modifiers.set(modifiers.state());
            }

            WindowEvent::RedrawRequested => {
                if self.shared.dirty.get() {
                    self.redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // position is in physical px, same space as layout rects.
                let (x, y) = (position.x as f32, position.y as f32);
                LAST_CURSOR.with(|c| c.set((x, y)));

                // Live-drag a range slider: update its value while the button
                // is held (even when the cursor leaves the input's rect).
                if let Some(path) = self.shared.drag_range.borrow().clone() {
                    if let Some(hit) = self.shared.draw.borrow().hits.iter().find(|h| h.path == path) {
                        let rect = hit.rect;
                        if let Some(el) = self.element_at_path(&path) {
                            let value = html_rs_render::range_value_at(&el, x, rect);
                            if value != hit.value {
                                if let Some(h) = &hit.on_input {
                                    h(&value);
                                }
                                self.shared.dirty.set(true);
                                window.request_redraw();
                            }
                        }
                    }
                    return;
                }

                let new_hover = self.shared.draw.borrow().hit_test(x, y).map(|h| h.path.clone());
                let changed = { *self.shared.hovered.borrow() != new_hover };
                if changed {
                    let (pointer, text_cursor) = new_hover
                        .as_ref()
                        .and_then(|path| {
                            self.shared
                                .draw
                                .borrow()
                                .hits
                                .iter()
                                .rev()
                                .find(|h| h.path == *path)
                                .map(|h| (h.cursor_pointer, h.selectable))
                        })
                        .unwrap_or((false, false));
                    window.set_cursor(if pointer {
                        CursorIcon::Pointer
                    } else if text_cursor {
                        CursorIcon::Text
                    } else {
                        CursorIcon::Default
                    });
                    let old_hover = self.shared.hovered.borrow().clone();
                    let menu_open = self
                        .shared
                        .renderer
                        .borrow()
                        .as_ref()
                        .map(|r| r.selects().open.is_some())
                        .unwrap_or(false);
                    // Perf: only interactive elements repaint on hover —
                    // those with `hover:` classes, or an option row of an
                    // open select menu (its highlight follows the cursor).
                    // Hovering plain static content skips the repaint.
                    let old_styled = old_hover.as_deref().map(|p| self.is_hoverable(p)).unwrap_or(false);
                    let new_styled = new_hover.as_deref().map(|p| self.is_hoverable(p)).unwrap_or(false);
                    *self.shared.hovered.borrow_mut() = new_hover;
                    if menu_open || old_styled || new_styled {
                        self.shared.dirty.set(true);
                        window.request_redraw();
                    }
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: WinitMouseButton::Left,
                ..
            } => {
                let scale = window.scale_factor() as f32;
                let (px, py) = LAST_CURSOR.with(|c| c.get());
                // Hit-test space is physical; handler coordinates are logical.
                let (lx, ly) = (px / scale, py / scale);

                let mut repaint = false;
                let hit = self.shared.draw.borrow().hit_test(px, py).cloned();
                match hit {
                    Some(hit) => {
                        let path = hit.path.clone();
                        // Selects: click toggles the option menu.
                        if hit.tag == "select" {
                            let mut sel = self.shared.renderer.borrow_mut();
                            if let Some(r) = sel.as_mut() {
                                let open = &mut r.selects_mut().open;
                                *open = if open.as_deref() == Some(&path) { None } else { Some(path.clone()) };
                            }
                            self.shared.focus.borrow_mut().take();
                            repaint = true;
                        }
                        // Option rows in an open select menu: commit selection.
                        else if hit.tag == "option" {
                            let select_path = path[..path.len() - 1].to_vec();
                            let child_idx = *path.last().unwrap_or(&0);
                            if let Some(select_el) = self.element_at_path(&select_path) {
                                // Position of the clicked option among option children.
                                let mut opt_pos = 0usize;
                                let mut opt_value = String::new();
                                for (i, c) in select_el.children.iter().enumerate() {
                                    if let Node::Element(o) = c {
                                        if o.tag == "option" {
                                            if i as u32 == child_idx {
                                                opt_value = element_text(o);
                                                break;
                                            }
                                            opt_pos += 1;
                                        }
                                    }
                                }
                                if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                                    let st = r.selects_mut();
                                    st.selections.insert(
                                        select_path.iter().map(|p| p.to_string()).collect::<Vec<_>>().join("."),
                                        opt_pos,
                                    );
                                    st.open = None;
                                }
                                if let Some(h) = &hit.on_change {
                                    h(&opt_value);
                                }
                                repaint = true;
                            }
                        }
                        // Range sliders: start a drag + jump to click position.
                        else if hit.is_input && self.is_range(&path) {
                            *self.shared.drag_range.borrow_mut() = Some(path.clone());
                            let rect = hit.rect;
                            if let Some(el) = self.element_at_path(&path) {
                                let value = html_rs_render::range_value_at(&el, px, rect);
                                if let Some(h) = &hit.on_input {
                                    h(&value);
                                }
                            }
                            self.shared.focus.borrow_mut().take();
                            repaint = true;
                        }
                        //
                        else {
                            {
                                let mut focus = self.shared.focus.borrow_mut();
                                if hit.is_input {
                                    if focus.as_ref() != Some(&path) {
                                        *focus = Some(path.clone());
                                        repaint = true;
                                    }
                                } else if focus.take().is_some() {
                                    repaint = true;
                                }
                            }
                            // Double-click on an input or selectable text:
                            // select all. A single click clears any selection
                            // and (re)arms the double-click window.
                            if hit.is_input || hit.selectable {
                                let input_key = html_rs_render::InputState::key(&path);
                                let is_double = self
                                    .shared
                                    .last_click
                                    .get()
                                    .map(|(t, lx, ly)| {
                                        t.elapsed() < DOUBLE_CLICK_WINDOW
                                            && (lx - px) * (lx - px) + (ly - py) * (ly - py)
                                                < DOUBLE_CLICK_SLOP_PX * DOUBLE_CLICK_SLOP_PX
                                    })
                                    .unwrap_or(false);
                                if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                                    let inputs = r.inputs_mut();
                                    if is_double {
                                        inputs.select_all(&input_key);
                                        self.shared.last_click.set(None);
                                    } else {
                                        inputs.clear_selection();
                                        self.shared.last_click.set(Some((Instant::now(), px, py)));
                                    }
                                    if hit.is_input {
                                        inputs.set_caret_on(true);
                                    }
                                }
                                if hit.is_input {
                                    // Restart the caret blink timer, solid while clicking.
                                    event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + CARET_BLINK));
                                }
                                repaint = true;
                            }
                            // Clicking anything else clears the text selection.
                            else if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                                r.inputs_mut().clear_selection();
                            }
                            // Clicking anywhere else closes an open select menu.
                            if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                                if r.selects_mut().open.take().is_some() {
                                    repaint = true;
                                }
                            }
                            if let Some(handler) = hit.on_click {
                                handler(&html_rs_core::PointerEvent {
                                    x: lx,
                                    y: ly,
                                    button: html_rs_core::MouseButton::Left,
                                });
                                repaint = true;
                            }
                        }
                    }
                    None => {
                        if self.shared.focus.borrow_mut().take().is_some() {
                            repaint = true;
                        }
                        if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                            if r.selects_mut().open.take().is_some() {
                                repaint = true;
                            }
                        }
                    }
                }
                if repaint {
                    self.shared.dirty.set(true);
                    window.request_redraw();
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: WinitMouseButton::Left,
                ..
            } => {
                if self.shared.drag_range.borrow_mut().take().is_some() {
                    self.shared.dirty.set(true);
                    window.request_redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // Browser-style: the wheel scrolls the innermost scrollable
                // container under the cursor (`overflow-auto` elements).
                let (px, py) = LAST_CURSOR.with(|c| c.get());
                let Some(target) = self.shared.draw.borrow().scroll_target(px, py).cloned() else {
                    return;
                };
                let scale = window.scale_factor() as f32;
                let dy = match delta {
                    // One wheel notch ≈ 3 lines of text.
                    MouseScrollDelta::LineDelta(_, y) => y * 40.0 * scale,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                // Wheel down (negative line delta) moves content up.
                if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                    r.scroll_by(&target.path, -dy);
                }
                self.shared.dirty.set(true);
                window.request_redraw();
            }

            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                let key = key_event.logical_key.clone();
                let mods = self.shared.modifiers.get();
                let ctrl = mods.control_key();
                let meta = mods.super_key();
                let shortcut_char = matches!(&key, winit::keyboard::Key::Character(c) if {
                    let k = c.to_string().to_lowercase();
                    k == "a" || k == "c" || k == "x" || k == "v"
                });

                // All keyboard handling goes to the focused `<input>`/`<textarea>`.
                let Some((el, path)) = self.focused_element() else {
                    // With no focused input, Ctrl/Cmd+C still copies selected
                    // static text (headings, paragraphs, spans…).
                    if (ctrl || meta) && shortcut_char {
                        let key_char = match &key {
                            winit::keyboard::Key::Character(c) => c.to_string().to_lowercase(),
                            _ => String::new(),
                        };
                        if key_char == "c" {
                            if let Some(text) = self.selected_text() {
                                self.copy_to_clipboard(&text);
                            }
                        }
                    }
                    return;
                };
                let is_textarea = el.tag == "textarea";
                let input_key = html_rs_render::InputState::key(&path);
                let on_input = el.on_input.clone();
                let on_enter = el.on_enter.clone();
                let on_key = el.on_key.clone();

                // A present `value` attr means controlled (the app drives the
                // text); without one the runtime keeps the typed buffer.
                let controlled = el.get_attr("value").is_some();
                let value = el
                    .get_attr("value")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| {
                        self.shared
                            .renderer
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.inputs().get(&input_key).cloned())
                            .unwrap_or_default()
                    });

                let mut handled = false;
                let mut edited_replace_all: Option<String> = None;

                let selected = self
                    .shared
                    .renderer
                    .borrow()
                    .as_ref()
                    .map(|r| r.inputs().is_selected(&input_key))
                    .unwrap_or(false);

                // Ctrl/Cmd+A on a focused input: select all.
                if (ctrl || meta)
                    && matches!(&key, winit::keyboard::Key::Character(c) if c.to_string().eq_ignore_ascii_case("a"))
                {
                    if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                        r.inputs_mut().select_all(&input_key);
                    }
                    handled = true;
                }

                // Ctrl/Cmd+C / X / V: clipboard over the input's full value
                // (the runtime's selection granularity is select-all).
                if (ctrl || meta) && !handled {
                    if let winit::keyboard::Key::Character(c) = &key {
                        match c.to_string().to_lowercase().as_str() {
                            "c" => {
                                self.copy_to_clipboard(&value);
                                handled = true;
                            }
                            "x" => {
                                self.copy_to_clipboard(&value);
                                if !value.is_empty() {
                                    edited_replace_all = Some(String::new());
                                }
                                handled = true;
                            }
                            "v" => {
                                handled = true;
                                let shared = self.shared.clone();
                                let window = window.clone();
                                let input_key = input_key.clone();
                                let on_input = on_input.clone();
                                let was_value = value.clone();
                                self.paste_from_clipboard(move |text| {
                                    if text.is_empty() {
                                        return;
                                    }
                                    let new_value = if selected { text } else { format!("{was_value}{text}") };
                                    if !controlled {
                                        if let Some(r) = shared.renderer.borrow_mut().as_mut() {
                                            let inputs = r.inputs_mut();
                                            inputs.set(&input_key, new_value.clone());
                                            inputs.clear_selection();
                                            inputs.set_caret_on(true);
                                        }
                                    }
                                    if let Some(h) = &on_input {
                                        h(&new_value);
                                    }
                                    shared.dirty.set(true);
                                    window.request_redraw();
                                });
                            }
                            _ => {}
                        }
                    }
                }

                // Computes the edited value for this key press, if it edits.
                // An active select-all is replaced by the edit (browser-style).
                // A cut (Ctrl/Cmd+X) directly replaces the value with "".
                let mut edited: Option<String> = if let winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter) = key {
                    if is_textarea {
                        // Textarea: Enter inserts a newline, never "submits".
                        Some(if selected { "\n".into() } else { format!("{value}\n") })
                    } else {
                        if let Some(h) = &on_enter {
                            h(&value);
                        }
                        // Enter is handled for focused inputs either way.
                        handled = true;
                        None
                    }
                } else if let winit::keyboard::Key::Named(winit::keyboard::NamedKey::Backspace) = key {
                    if selected {
                        Some(String::new())
                    } else {
                        let mut s = value.clone();
                        if s.pop().is_some() {
                            Some(s)
                        } else {
                            // Nothing to delete; still swallow the key.
                            handled = true;
                            None
                        }
                    }
                } else if !ctrl && !meta {
                    match key_event.text.as_ref() {
                        Some(text) if selected => Some(text.to_string()),
                        Some(text) => Some(format!("{value}{text}")),
                        None => None,
                    }
                } else {
                    None
                };
                if edited.is_none() {
                    edited = edited_replace_all.take();
                }

                if let Some(new_value) = edited {
                    handled = true;
                    if !controlled {
                        if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                            let inputs = r.inputs_mut();
                            inputs.set(&input_key, new_value.clone());
                            inputs.clear_selection();
                            // Keep the caret solid while typing (reset blink).
                            inputs.set_caret_on(true);
                        }
                    }
                    if let Some(h) = &on_input {
                        h(&new_value);
                    }
                }

                if !handled {
                    if let Some(h) = &on_key {
                        let key_name = match &key {
                            winit::keyboard::Key::Character(c) => c.to_string(),
                            winit::keyboard::Key::Named(n) => format!("{n:?}"),
                            winit::keyboard::Key::Unidentified(_) => "Unidentified".to_string(),
                            _ => format!("{key:?}"),
                        };
                        h(&html_rs_core::KeyEventInfo {
                            key: key_name,
                            ctrl,
                            shift: mods.shift_key(),
                            alt: mods.alt_key(),
                            meta,
                        });
                    }
                }
                // The focus ring, caret and the uncontrolled input buffer all
                // live in renderer/painter state (not the element tree), so
                // mark the frame dirty explicitly — RedrawRequested skips
                // clean frames.
                self.shared.dirty.set(true);
                // Reset the blink phase while typing: the caret stays solid.
                event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + CARET_BLINK));
                window.request_redraw();
            }

            _ => {}
        }
    }

    /// Caret blink driver: while an input is focused, a timer wakes the loop
    /// twice a second to toggle the caret; with no focus the loop just waits.
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        let focused = self.shared.focus.borrow().is_some();
        if !focused {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                r.inputs_mut().toggle_caret();
            }
            self.shared.dirty.set(true);
            self.repaint();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + CARET_BLINK));
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.shared.dirty.get() {
            self.repaint();
        }
    }
}

thread_local! {
    /// Last cursor position in **physical** px (CursorMoved and MouseInput are
    /// separate events, so we remember the position between them).
    static LAST_CURSOR: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
}

/// Direct text content of an element (concatenated text children).
fn element_text(el: &Element) -> String {
    let mut s = String::new();
    for child in &el.children {
        if let Node::Text(t) = child {
            s.push_str(&t.content);
        }
    }
    s
}
