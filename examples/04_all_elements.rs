//! 04 — All Elements
//!
//! Every tag, one by one, in one scrolling page. The page itself is built
//! from **components** (plain fns + props structs, like React function
//! components — `ui!` flattens them via braces) and app state lives in a
//! **`Store`** — a zustand-style string-keyed global store defined once in
//! `main`.
//!
//! Tags shown: `h1`–`h6`, `p`, `text`, `a`, `button`, `label`,
//! `input` (text / search / checkbox / radio / range), `select` + `option`,
//! `ul` / `ol` / `li`, `table` / `thead` / `tbody` / `tr` / `th` / `td`,
//! `img`, `blockquote`, `code`, `kbd`, `progress`, `dialog`, `nav`.
//!
//! Everything is **themed by default** (shadcn Slate, dark): the root only
//! sets `bg-background text-foreground` and every control picks up its look
//! from theme tokens. Plain Tailwind classes still overwrite any token.
//!
//! Run: `cargo run --bin 04_all_elements`
//! Web: `trunk serve web/04_all_elements.html`

// Component functions are PascalCase on purpose (they read like React
// components); everything else keeps Rust naming.
#![allow(non_snake_case)]

use html_rs::{click, launch, ui, ClickHandler, Element, State, Store};

// ---------------------------------------------------------------------------
// Components — plain fns + props structs, like React function components
// ---------------------------------------------------------------------------

/// Props for [`Button`]. Handlers are props, exactly like `onClick` in React.
struct ButtonProps {
    label: String,
    /// "primary" | "outline" | "ghost" | "destructive" (shadcn variants).
    variant: &'static str,
    onclick: ClickHandler,
}

/// A shadcn-style button. The per-variant classes are the shadcn recipes
/// written with theme tokens.
fn Button(p: ButtonProps) -> Element {
    let variant = match p.variant {
        "outline" => "border border-input bg-background text-foreground hover:bg-accent hover:text-accent-foreground",
        "ghost" => "bg-transparent text-foreground hover:bg-accent hover:text-accent-foreground",
        "destructive" => "bg-destructive text-destructive-foreground hover:bg-destructive/90",
        _ => "bg-primary text-primary-foreground hover:bg-primary/90",
    };
    // Size/padding/rounding come from the `<button>` tag defaults (h-10,
    // px-4, py-2, rounded-md, text-sm, font-medium) so every button matches
    // bare <button>s; variants only change the colors.
    let class = variant.to_string();
    let onclick = p.onclick;
    ui! {
        <button class={class} onclick={onclick}>{p.label}</button>
    }
}

/// Props for [`Card`]: a title plus a body element (children-as-a-prop).
struct CardProps {
    title: String,
    body: Element,
}

/// A shadcn-style card.
fn Card(p: CardProps) -> Element {
    ui! {
        <div class="flex flex-col gap-3 p-6 rounded-lg border border-border bg-card text-card-foreground shadow-sm w-96">
            <p class="text-lg font-semibold">{p.title}</p>
            {p.body}
        </div>
    }
}

/// A store-backed button: writes through the global store using the key, so
/// the component needs no explicit `State` handle — just the store + key.
fn StoreButton(store: Store, key: &'static str, label: String, delta: i32) -> Element {
    let onclick = click(move |_| {
        let n: i32 = store.get(key).unwrap_or(0);
        store.set(key, n + delta);
    });
    Button(ButtonProps { label, variant: "outline", onclick })
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

fn main() {
    // The store is defined once, in main. Cells live under string keys; any
    // component that gets a clone of `store` can use the same key.
    let store = Store::new();
    store.init("count", 0i32);
    store.init("clicks", 0i32);

    // Local component state.
    let dialog_open = State::new(false);
    let checked = State::new(true);
    let radio = State::new(1usize);
    let plan = State::new("Pro".to_string());
    let volume = State::new(40i32);
    let uploaded = State::new(34i32);

    launch(move || {
        let open = dialog_open.get();
        let check = checked.get();
        let choice = radio.get();
        let plan_val = plan.get();
        let vol = volume.get();
        let up = uploaded.get();
        let count: i32 = store.get_or("count", 0);
        let clicks: i32 = store.get_or("clicks", 0);
        let store_clicks = store.clone();
        let store_reset = store.clone();

        let counter_card_body = ui! {
            <div class="flex flex-col gap-4">
                <div class="flex items-baseline gap-2">
                    <span class="text-4xl font-bold tabular-nums">{count}</span>
                    <span class="text-sm text-muted-foreground">"store key: count"</span>
                </div>
                <div class="flex items-center gap-2">
                    {StoreButton(store.clone(), "count", "− 1".to_string(), -1)}
                    {StoreButton(store.clone(), "count", "+ 1".to_string(), 1)}
                    {Button(ButtonProps {
                        label: "Reset".to_string(),
                        variant: "destructive",
                        onclick: click(move |_| {
                            store_reset.set("count", 0);
                        }),
                    })}
                </div>
                <p class="text-sm text-muted-foreground">"button clicks this session: "{clicks}</p>
            </div>
        };

        ui! {
            <div class="w-full h-full flex flex-col bg-background text-foreground">
                // Scrollable page: shrinks with the window and scrolls like a
                // browser (wheel + drawn scrollbar thumb).
                <div class="flex-1 min-h-0 overflow-auto flex flex-col gap-6 p-8 w-full">

                    <h1>"All Elements"</h1>
                    <nav class="items-center gap-4 pb-4 border-b border-border">
                        <p class="text-muted-foreground">"Every tag, one by one — themed by shadcn Slate."</p>
                        <a class="ml-auto" href="https://github.com/harishdeivanayagam/html_rs">"html.rs"</a>
                    </nav>

                    // ---- Text tags: h1…h6, p, text, a -------------------
                    <section class="flex flex-col gap-2 w-full">
                        <h2>"Text"</h2>
                        <h3>"Heading three"</h3>
                        <h4>"Heading four"</h4>
                        <h5>"Heading five"</h5>
                        <h6>"Heading six"</h6>
                        <p>"A paragraph of running text: it wraps when the box gets narrow, and every word is shaped by cosmic-text, not a browser."</p>
                        <p>"Text is selectable — double-click to select all, then copy it with Ctrl/Cmd+C."</p>
                        <p selectable="false">"This paragraph is not selectable (selectable=false)."</p>
                        <text>"A plain <text> run — no margin tricks, just the default size."</text>
                        <p>"Links too: "<a href="https://github.com/harishdeivanayagam/html_rs">"star html.rs on GitHub"</a></p>
                    </section>

                    // ---- Lists: ul / ol / li ----------------------------
                    <section class="flex flex-col gap-2 w-full">
                        <h2>"Lists"</h2>
                        <ul>
                            <li>"States, not VDOM"</li>
                            <li>"winit + wgpu, no webview"</li>
                            <li>"One markup, desktop + web"</li>
                        </ul>
                        <ol>
                            <li>"Fork the repo"</li>
                            <li>"cargo run --bin 04_all_elements"</li>
                            <li>"Ship something"</li>
                        </ol>
                    </section>

                    // ---- Buttons: tag default + component variants ------
                    <section class="flex flex-col gap-4 w-full">
                        <h2>"Buttons"</h2>
                        <div class="flex items-center gap-2 flex-wrap">
                            // Bare <button> picks up the shadcn default variant.
                            <button onclick={move |_| dialog_open.update(|o| *o = !*o)}>"Show dialog"</button>
                            {Button(ButtonProps { label: "Primary".to_string(), variant: "primary", onclick: click(move |_| { store_clicks.update("clicks", |c: &mut i32| *c += 1); }) })}
                            {Button(ButtonProps { label: "Outline".to_string(), variant: "outline", onclick: click(move |_| {}) })}
                            {Button(ButtonProps { label: "Ghost".to_string(), variant: "ghost", onclick: click(move |_| {}) })}
                            {Button(ButtonProps { label: "Destructive".to_string(), variant: "destructive", onclick: click(move |_| {}) })}
                        </div>
                    </section>

                    // ---- Inputs: label + checkbox / radio / text -------
                    <section class="flex flex-col gap-4 w-full">
                        <h2>"Inputs"</h2>
                        <label>
                            <input
                                type="checkbox"
                                checked={check}
                                onclick={move |_| checked.update(|c| *c = !*c)}
                            />
                            <p>"Enable notifications"</p>
                        </label>

                        <label>
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

                        <label class="flex flex-col gap-2">
                            <p class="text-muted-foreground">"Username"</p>
                            <input type="text" placeholder="Ada Lovelace" />
                            <input type="search" placeholder="Search…" />
                            <p class="text-muted-foreground">"Bio"</p>
                            <textarea placeholder="Tell us more…" />
                            <p class="text-xs text-muted-foreground">"Inputs keep typed text (uncontrolled); set value for controlled inputs."</p>
                        </label>
                    </section>

                    // ---- Store counter (components + global store) ------
                    <section class="flex flex-col gap-4 w-full">
                        <h2>"Components & Store"</h2>
                        {Card(CardProps { title: "Global store (zustand-style)".to_string(), body: counter_card_body })}
                    </section>

                    // ---- select / option --------------------------------
                    <section class="flex flex-col gap-2 w-full">
                        <h2>"Select"</h2>
                        <select value={plan_val.clone()} onchange={move |v: &str| plan.set(v.to_string())}>
                            <option>"Hobby"</option>
                            <option>"Pro"</option>
                            <option>"Team"</option>
                        </select>
                        <p class="text-sm text-muted-foreground">{format!("Selected plan: {plan_val}")}</p>
                    </section>

                    // ---- range + progress -------------------------------
                    <section class="flex flex-col gap-4 w-full">
                        <h2>"Range & progress"</h2>
                        <label class="flex flex-col gap-2">
                            <p class="text-sm text-muted-foreground">{format!("Volume: {vol}")}</p>
                            <input
                                type="range"
                                min="0"
                                max="100"
                                value={vol}
                                oninput={move |v: &str| volume.set(v.parse().unwrap_or(0))}
                            />
                        </label>

                        <label class="flex flex-col gap-2">
                            <p class="text-sm text-muted-foreground">{format!("Uploading… {up}%")}</p>
                            <progress value={up} max="100" />
                        </label>
                    </section>

                    // ---- table ------------------------------------------
                    <section class="flex flex-col gap-2 w-full">
                        <h2>"Table"</h2>
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
                    </section>

                    // ---- blockquote / kbd / code ------------------------
                    <section class="flex flex-col gap-4 w-full">
                        <h2>"Quotes & code"</h2>
                        <blockquote>"Nothing in HTML can't be drawn with four quads and some glyphs."</blockquote>

                        <div class="flex items-center gap-2">
                            <p class="text-muted-foreground text-sm">"Press"</p>
                            <kbd>"Ctrl"</kbd>
                            <p class="text-muted-foreground text-sm">"+"</p>
                            <kbd>"K"</kbd>
                        </div>

                        <p>"Inline code looks like " <code>"let ui = html_rs::ui;"</code></p>
                    </section>

                    // ---- img --------------------------------------------
                    <section class="flex flex-col gap-2 w-full">
                        <h2>"Image"</h2>
                        <img src="examples/assets/logo.png" alt="html.rs logo" class="w-16 h-16" />
                    </section>
                </div>

                // Outside the scroller so it floats over the whole window.
                <dialog class="w-96" open={open}>
                    <p class="text-lg font-semibold">"Hello, dialog!"</p>
                    <p class="text-sm text-muted-foreground">"A real modal: dim overlay, centered, auto height, hidden when closed."</p>
                    <div class="flex justify-end gap-2 mt-2">
                        <button
                            class="h-9 px-3"
                            onclick={move |_| dialog_open.set(false)}
                        >"Close"</button>
                    </div>
                </dialog>
            </div>
        }
    })
    .title("All Elements — html.rs")
    .size(1080.0, 720.0)
    // Default is dark; pass "light" for the light palette.
    .light_mode("dark")
    .run();
}
