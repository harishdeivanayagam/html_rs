//! 04 — Element Gallery
//!
//! Exercises the full element set: lists (`<ul>`/`<ol>`/`<li>`), `<img>` with
//! a real decoded PNG, links, `<select>`, a modal `<dialog>`, checkbox/radio
//! inputs, `<kbd>`, `<code>`, `<blockquote>`, tables and `<nav>`.
//!
//! Run: `cargo run --bin 04_elements`
//! Web: `trunk serve web/04_elements.html`

use html_rs::{launch, ui, State};

fn main() {
    let dialog_open = State::new(false);
    let checked = State::new(true);
    let radio = State::new(1usize);
    let selected = State::new(0usize);

    launch(move || {
        let open = dialog_open.get();
        let check = checked.get();
        let choice = radio.get();
        let sel = selected.get();

        let plan = ["Hobby", "Pro", "Team"][sel];

        ui! {
            <div class="w-full h-full bg-slate-900 p-8 overflow-hidden">
                <nav class="mb-6 pb-4 border-b border-slate-800">
                    <p class="text-slate-400 text-sm">"Element Gallery"</p>
                    <a class="text-sm" href="https://github.com/harishdeivanayagam/html_rs">"html.rs"</a>
                    <button
                        class="ml-auto px-4 py-2 bg-indigo-600 hover:bg-indigo-500 text-white text-sm rounded-lg cursor-pointer"
                        onclick={move |_| dialog_open.update(|o| *o = !*o)}
                    >"Show dialog"</button>
                </nav>

                <div class="flex gap-8 w-full">
                    <div class="flex flex-col gap-4 w-72">
                        <img src="examples/assets/logo.png" alt="html.rs logo" class="w-16 h-16" />
                        <ul>
                            <li>"Signals, not VDOM"</li>
                            <li>"winit + wgpu, no webview"</li>
                            <li>"One markup, desktop + web"</li>
                        </ul>
                        <ol>
                            <li>"Fork the repo"</li>
                            <li>"cargo run --bin 04_elements"</li>
                            <li>"Ship something"</li>
                        </ol>
                    </div>

                    <div class="flex flex-col gap-4 w-80">
                        <label class="flex items-center gap-2 text-slate-200">
                            <input
                                type="checkbox"
                                checked={check}
                                onclick={move |_| checked.update(|c| *c = !*c)}
                            />
                            <p>"Enable notifications"</p>
                        </label>

                        <label class="flex items-center gap-2 text-slate-200">
                            <input
                                type="radio"
                                checked={choice == 0}
                                onclick={move |_| radio.set(0)}
                            />
                            <p class="mr-4">"Monthly"</p>
                            <input
                                type="radio"
                                checked={choice == 1}
                                onclick={move |_| radio.set(1)}
                            />
                            <p>"Yearly"</p>
                        </label>

                        <select value={plan.to_string()}>
                            <option>"Hobby"</option>
                            <option>"Pro"</option>
                            <option>"Team"</option>
                        </select>

                        <input
                            type="search"
                            class="w-full px-4 py-2 bg-slate-800 border border-slate-700 text-slate-100"
                            placeholder="Search…"
                        />
                    </div>

                    <div class="flex flex-col gap-4 w-96">
                        <table>
                            <thead>
                                <tr>
                                    <th>"Plan"</th>
                                    <th>"Price"</th>
                                    <th>"Seats"</th>
                                </tr>
                            </thead>
                            <tbody>
                                <tr>
                                    <td>"Hobby"</td>
                                    <td>"$0"</td>
                                    <td>"1"</td>
                                </tr>
                                <tr>
                                    <td>"Pro"</td>
                                    <td>"$12"</td>
                                    <td>"5"</td>
                                </tr>
                            </tbody>
                        </table>

                        <blockquote>"Nothing in HTML can't be drawn with four quads and some glyphs."</blockquote>

                        <div class="flex items-center gap-2">
                            <p class="text-slate-400 text-sm">"Press"</p>
                            <kbd>"Ctrl"</kbd>
                            <p class="text-slate-400 text-sm">"+"</p>
                            <kbd>"K"</kbd>
                        </div>

                        <p class="text-slate-300">"Inline code looks like " <code>"let ui = html_rs::ui;"</code></p>
                    </div>
                </div>

                <dialog class="w-96" open={open}>
                    <p class="text-xl font-bold text-white">"Hello, dialog!"</p>
                    <p class="text-slate-400">"A real modal: absolute positioning, auto-margin centering, hidden when closed."</p>
                    <button
                        class="self-center px-6 py-2 bg-slate-700 hover:bg-slate-600 text-white rounded-lg cursor-pointer"
                        onclick={move |_| dialog_open.set(false)}
                    >"Close"</button>
                </dialog>
            </div>
        }
    })
    .title("Element Gallery — html.rs")
    .size(960.0, 640.0)
    .run();
}
