//! 02 — Counter App
//!
//! Demonstrates signals: reads inside `ui!` are tracked, writes (`set` /
//! `update`) rebuild the view automatically.
//!
//! Run: `cargo run --bin 02_counter_app`
//! Web: `trunk serve web/02_counter_app.html`

use html_rs::{launch, Signal, ui};

fn main() {
    // `Signal<i32>` is Copy — capture it in the view and every handler.
    let count = Signal::new(0);

    launch(move || {
        ui! {
            <div class="w-full h-full flex items-center justify-center bg-slate-900">
                <div class="flex flex-col items-center gap-8 p-12 bg-slate-800 rounded-2xl border border-slate-700">
                    <p class="text-6xl font-bold text-white">"Counter: "{count.get()}</p>
                    <div class="flex items-center gap-4">
                        <button
                            class="px-6 py-3 bg-slate-600 hover:bg-slate-500 text-white text-lg font-semibold rounded-xl cursor-pointer"
                            onclick={move || count.update(|c| *c -= 1)}
                        >"Count (−)"</button>
                        <button
                            class="px-6 py-3 bg-blue-600 hover:bg-blue-500 text-white text-lg font-semibold rounded-xl cursor-pointer"
                            onclick={move || count.update(|c| *c += 1)}
                        >"Count (+)"</button>
                        <button
                            class="px-6 py-3 bg-rose-600 hover:bg-rose-500 text-white text-lg font-semibold rounded-xl cursor-pointer"
                            onclick={move || count.set(0)}
                        >"Reset"</button>
                    </div>
                    <p class="text-slate-500 text-sm">"state lives in a Signal — no re-render loop, no diffing"</p>
                </div>
            </div>
        }
    })
    .title("Counter — html.rs")
    .size(720.0, 480.0)
    .run();
}
