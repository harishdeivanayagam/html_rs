//! # html.rs — native HTML for the desktop
//!
//! Build native cross-platform desktop apps with **winit + wgpu** using
//! HTML-like syntax, Tailwind classes and signals-based reactivity.
//! No webview, no JS engine, no prebuilt browser — the `ui!` macro compiles
//! your markup into an element tree that a custom wgpu renderer paints.
//!
//! ```ignore
//! use html_rs::{launch, ui};
//!
//! fn main() {
//!     let count = html_rs::Signal::new(0);
//!     launch(move || {
//!         ui! {
//!             <div class="w-full h-full flex items-center justify-center bg-slate-900">
//!                 <p class="text-2xl text-white">"Counter: "{count.get()}</p>
//!                 <button class="px-4 py-2 bg-blue-600 rounded-lg text-white"
//!                         onclick={move || count.update(|c| *c += 1)}>
//!                     "Count (+)"
//!                 </button>
//!             </div>
//!         }
//!     })
//!     .title("Counter")
//!     .run();
//! }
//! ```
//!
//! Run natively with `cargo run --example 02_counter_app`, or on the web with
//! Trunk: `trunk serve 02_counter_app`.

pub use html_rs_core::{
    child_of, click, enter, input, key, text, Child, Element, EnterHandler, InputHandler,
    KeyEventInfo, KeyHandler, MouseButton, Node, PointerEvent, TextNode,
};
pub use html_rs_macro::ui;
pub use html_rs_render as render;
pub use html_rs_signals::{batch, create_effect, create_memo, set_flush_hook, untrack, Effect, Signal};

mod runtime;
pub use runtime::{launch, App};
