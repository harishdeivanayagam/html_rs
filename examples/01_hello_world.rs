//! 01 — Hello World
//!
//! The smallest html.rs app: static markup, Tailwind classes, no state.
//!
//! Run: `cargo run --bin 01_hello_world`
//! Web: `trunk serve web/01_hello_world.html`

use html_rs::{launch, ui};

fn main() {
    launch(|| {
        ui! {
            <div class="w-full h-full flex items-center justify-center bg-slate-900">
                <div class="flex flex-col items-center gap-4 p-12 bg-slate-800 rounded-2xl border border-slate-700">
                    <p class="text-4xl font-bold text-white">"Hello, html.rs!"</p>
                    <p class="text-lg text-slate-400">
                        "Native desktop apps in Rust — winit + wgpu, no webview."
                    </p>
                    <div class="flex gap-2 mt-2">
                        <p class="px-3 py-1 bg-slate-900 rounded-md text-sm text-blue-400">"wgpu"</p>
                        <p class="px-3 py-1 bg-slate-900 rounded-md text-sm text-emerald-400">"signals"</p>
                        <p class="px-3 py-1 bg-slate-900 rounded-md text-sm text-amber-400">"tailwind"</p>
                    </div>
                </div>
            </div>
        }
    })
    .title("Hello World — html.rs")
    .size(720.0, 480.0)
    .run();
}
