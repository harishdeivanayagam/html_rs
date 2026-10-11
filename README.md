# html.rs — native HTML for the desktop

**Write UI in HTML + Tailwind. Render it natively with wgpu. No webview. No JS.**

`html.rs` is a Rust framework for building cross-platform desktop apps (macOS /
Windows / Linux) that also runs on the **web via WebAssembly**. Instead of an
egui-style immediate API, a Tauri-style webview, or an Iced-style widget tree,
you write declarative **HTML-like markup** with **Tailwind utility classes** in
a single `ui!` macro — and a custom GPU renderer paints it.

```rust
use html_rs::{launch, Signal, ui};

fn main() {
    let count = Signal::new(0i32);

    launch(move || {
        let count_val = count.get();
        ui! {
            <div class="w-full h-full flex items-center justify-center bg-slate-900">
                <div class="flex flex-col items-center gap-8 p-12 bg-slate-800 rounded-2xl">
                    <p class="text-6xl font-bold text-white">"Counter: "{count_val}</p>
                    <button
                        class="px-6 py-3 bg-blue-600 hover:bg-blue-500 text-white font-semibold rounded-xl cursor-pointer"
                        onclick={move || count.update(|c| *c += 1)}
                    >"Count (+)"</button>
                </div>
            </div>
        }
    })
    .title("Counter — html.rs")
    .size(720.0, 480.0)
    .run();
}
```

There is **no webview and no JS engine** anywhere in the stack:

- The `ui!` macro compiles your markup into an `Element` tree at compile time.
- Signals-based reactivity (like modern **JS frameworks**) re-runs your view
  closure when signals change — fine-grained, no VDOM diffing.
- Layout runs through **taffy** (the same flexbox/grid engine used by Dioxus,
  Bevy UI and others).
- Painting runs through a **custom wgpu renderer**: instanced SDF rounded-rect
  quads for boxes/borders, and **cosmic-text/glyphon** for shaped text.

---

## Getting started

```sh
# Native (macOS / Windows / Linux)
cargo run --bin 01_hello_world     # static markup + Tailwind
cargo run --bin 02_counter_app     # signals + event handlers
cargo run --bin 03_todo_app        # controlled <input>, dynamic lists

# Web (Trunk + wasm32) — same example sources, no code changes
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
trunk serve web/01_hello_world.html
```

Each example lives in `examples/` and is registered as a `[[bin]]` target (that
is what Trunk builds for the web). The `web/*.html` shells select the bin with
`<link data-trunk data-type="rust" data-bin="..."/>`.

---

## Supported HTML primitives (W3C)

`html.rs` implements a practical subset of the W3C/WHATWG HTML element model.
Tags are "semantic names over well-behaved boxes": every element is a CSS
box (content + padding + border + margin) that participates in flexbox/grid
layout, paints background/border, and can carry event handlers.

### Element tags

| Tag | Status | Behavior (per W3C/HTML semantics) |
|---|---|---|
| `<div>` | ✅ supported | Generic flow container (block-level box). |
| `<p>` | ✅ supported | Paragraph — block-level text box. |
| `<button>` | ✅ supported | Interactive box; `cursor-pointer` behavior, `onclick`. |
| `<input>` | ✅ supported | *Controlled* text field: focus ring on click, typing dispatches `oninput` (full new value), Enter dispatches `onenter`, Backspace editing supported. |
| `<span>`, `<h1>–<h6>`, `<img>`, … | 🚧 planned | Render as plain boxes today; text sizing via `text-*` classes works on any element. |

### Attributes

| Attribute | Applies to | Notes |
|---|---|---|
| `class` / `className` | all | Tailwind class string (compile-time compiled). |
| `value` | `<input>` | Controlled value; drive it from a signal. |
| `placeholder` | `<input>` | Shown when `value` is empty. |
| `onclick` | all | `move \|\| ...` or `move \|e: &PointerEvent\| ...` |
| `oninput` / `onenter` | `<input>` | `\|text: &str\|` — full new value. |
| `onkeydown` | all | `\|k: &KeyEventInfo\|` with `key`, `ctrl`, `shift`, `alt`, `meta`. |

### Text model

Text lives in text runs — string literals (`"Counter: "`) or interpolated
expressions (`{count.get()}`, any `Display` type). Text is shaped with
cosmic-text (Unicode line breaking, font fallback) and rendered by glyphon on
the same GPU pass as the boxes.

---

## Flexbox support (W3C CSS Flexible Box Layout)

The flexbox implementation is [taffy](https://github.com/DioxusLabs/taffy),
conforming to the [CSS Flexible Box Layout Module Level 1 specification](https://www.w3.org/TR/css-flexbox-1/).
Supported today through Tailwind utility classes:

| CSS property (spec) | Tailwind class | Status |
|---|---|---|
| `display: flex` | `flex` | ✅ |
| `flex-direction` | `flex-row`, `flex-col` | ✅ |
| `flex-wrap` | `flex-wrap`, `flex-nowrap` | ✅ |
| `flex` (grow/shrink/basis) | `flex-1`, `flex-grow`, `shrink-0` | ✅ |
| `justify-content` (main axis) | `justify-{start,center,end,between,around,evenly}` | ✅ |
| `align-items` (cross axis) | `items-{start,center,end,stretch}` | ✅ |
| `gap` | `gap-N`, `gap-x-N`, `gap-y-N` | ✅ |
| box model: padding / margin / border | `p*-*`, `m*-*` (+ `m-auto`), `border[-N][-color]` | ✅ |
| sizing | `w-*` / `h-*` (`N`, `full`, `auto`, `px`), `min-w-0`, `min-h-0` | ✅ |
| `overflow` | `overflow-hidden` | ✅ |

## CSS Grid support (W3C CSS Grid Layout)

We **support CSS Grid** (the
[CSS Grid Layout Module Level 1/2 specification](https://www.w3.org/TR/css-grid-1/)),
also via taffy's grid engine:

| CSS property (spec) | Tailwind class | Status |
|---|---|---|
| `display: grid` | `grid` | ✅ |
| `grid-template-columns` | `grid-cols-N` → `repeat(N, minmax(0, 1fr))` | ✅ |
| `grid-template-rows` | `grid-rows-N` | ✅ |
| `grid-column` (span) | `col-span-N` | ✅ |
| `grid-row` (span) | `row-span-N` | ✅ |
| `grid-column-start` | `col-start-N` | ✅ |
| `grid-row-start` | `row-start-N` | ✅ |
| `grid-auto-flow` | `grid-flow-{row,col,row-dense,col-dense}` | ✅ |
| `grid-auto-rows` | `auto-rows-N` | ✅ |
| `gap` (grid spacing) | `gap-N`, `gap-x-N`, `gap-y-N` | ✅ |
| `align/justify-items`, `align/justify-self`, named areas/lines, `fr` mixes | — | 🚧 planned (taffy supports them; class syntax to come) |

Example:

```rust
ui! {
    <div class="grid grid-cols-3 gap-4 w-full h-full p-8 bg-slate-900">
        <div class="col-span-2 bg-blue-600 rounded-xl">"wide card"</div>
        <div class="bg-emerald-600 rounded-xl">"tall card"</div>
        <div class="col-start-2 bg-amber-600 rounded-xl">"placed card"</div>
    </div>
}
```

### Visual properties

- Colors: the full Tailwind v3 palette (`bg-*`, `text-*`, `border-*`) plus hex
  literals `bg-[#0ea5e9]`; converted sRGB → linear on the GPU.
- Corners: `rounded{-sm,-md,-lg,-xl,-2xl,-3xl,-full,-N}` via SDF anti-aliased
  rounded rectangles.
- Text: `text-{xs…7xl}`, `font-{thin…black}`, `text-{left,center,right}`,
  `underline`, `line-through`, `opacity-N`.
- Interaction: `cursor-pointer` and `hover:` variants (re-painted while hovered).

Unknown Tailwind classes are silently ignored, so standard class strings from
the Tailwind ecosystem stay copy-pasteable.

---

## Reactivity: signals, not a VDOM

Reactivity follows the **fine-grained signals model popularized by modern JS
frameworks** — reactive state that tracks its readers, and re-runs only the
code that depends on it:

```rust
let count = Signal::new(0i32);          // Signal<T> is Copy — capture freely
count.get();                            // tracked read
count.set(5);                           // write → notify
count.update(|c| *c += 1);              // mutate in place
let doubled = html_rs::create_memo(move || count.get() * 2);
html_rs::batch(|| { /* many writes, one flush */ });
```

- Your `launch(move || ui! { ... })` closure runs inside a single **render
  effect**: every signal read inside it is tracked, and any write schedules
  exactly one re-render (writes inside a `batch` flush once).
- Event handlers mutate signals; the framework re-runs the view, rebuilds the
  element tree, and repaints only what a frame needs.
- `create_effect` / `create_memo` give you general-purpose reactive primitives.

## Architecture

```
crates/html_rs_signals   fine-grained signals runtime (thread-local, Copy signals)
crates/html_rs_core      Element/Node tree, event types, Tailwind class compiler
crates/html_rs_macro     the ui! proc macro (hand-rolled HTML parser, no syn/quote)
crates/html_rs_render    taffy layout → wgpu quads (WGSL SDF) + glyphon text
src/                     facade crate: re-exports + winit runtime + launch()
examples/                desktop + web examples (registered as bins)
web/                     Trunk HTML shells (one per example)
```

- **Layout**: taffy flexbox/grid → physical-pixel boxes.
- **Paint**: one instanced draw call for rounded-rect quads (premultiplied
  alpha, sRGB→linear) + glyphon text pass.
- **Hit testing**: painter's algorithm — topmost-last wins; click/focus/hover
  dispatch from the runtime to `onclick` / `oninput` handlers.

## License

MIT — see [LICENSE](LICENSE).

---

<div align="center">

Built with ♥ by [harishdeivanayagam](https://github.com/harishdeivanayagam)

</div>

