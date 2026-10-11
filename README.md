# html_rs — Native apps in HTML + Rust

**Write UI in HTML + Tailwind. Render it natively with wgpu. No webview. No JS.**

`html.rs` is a Rust framework for building cross-platform desktop apps (macOS /
Windows / Linux) that also runs on the **web via WebAssembly**. Instead of an
egui-style immediate API, a Tauri-style webview, or an Iced-style widget tree,
you write declarative **HTML-like markup** with **Tailwind utility classes** in
a single `ui!` macro — and a custom GPU renderer paints it.

```rust
use html_rs::{launch, State, ui};

fn main() {
    let count = State::new(0i32);

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
- State-based reactivity (like modern **JS frameworks**) re-runs your view
  closure when state changes — fine-grained updates, no virtual DOM and no
  DOM patching; repaints diff trees to re-layout only what changed.
- Layout runs through **taffy** (the same flexbox/grid engine used by Dioxus,
  Bevy UI and others).
- Painting runs through a **custom wgpu renderer**: instanced SDF rounded-rect
  quads for boxes/borders, and **cosmic-text/glyphon** for shaped text.

---

## Getting started

```sh
# Native (macOS / Windows / Linux)
cargo run --bin 01_hello_world     # static markup + Tailwind
cargo run --bin 02_counter_app     # state + event handlers
cargo run --bin 03_todo_app        # controlled <input>, dynamic lists
cargo run --bin 04_all_elements   # every tag + components + store in one gallery

# Web (Trunk + wasm32) — same example sources, no code changes
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
# Use --release for interactive testing — debug wasm is 10-50x slower.
trunk serve --release web/01_hello_world.html
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
| `<button>` | ✅ supported | Interactive box; shadcn default variant (primary fill, `hover:bg-primary/90`), `cursor-pointer`, `onclick`. |
| `<input>` | ✅ supported | Text field (default), `type="search"` pill variant, `type="checkbox"` and `type="radio"` controls (styled box/circle, `checked` attr toggles accent fill); focus ring on click, typing dispatches `oninput` (full new value), Enter dispatches `onenter`, Backspace editing supported. Blinking caret while focused; the placeholder hides on focus; double-click (or Ctrl/Cmd+A) selects all — typing or Backspace then replaces the selection. |
| `<textarea>` | ✅ supported | Multi-line text box (shadcn textarea look, default `h-20`): typed text wraps and is kept by the runtime (uncontrolled); `value` attr makes it controlled. `oninput`/`onenter` fire while typing (Enter inserts a newline instead of dispatching `onenter`). Blinking caret, placeholder hides on focus, double-click selects all. |
| `<ul>` / `<ol>` / `<li>` | ✅ supported | Bulleted (`•`) and numbered (`1.`, `2.`, …) lists — markers are drawn in the list's left gutter (`pl-6` tag default). |
| `<img>` | ✅ supported | Decodes local image files (PNG/JPEG/GIF/WebP/BMP) into GPU textures, aspect-preserved fit inside `w-*`/`h-*` classes; remote (`http(s)`) and `data:` URLs are not fetched — on wasm32 nothing is loaded from disk — so those (or unreadable files) render the `alt` text as a placeholder. |
| `<a>` | ✅ supported | Link-styled text box (`underline`, link color, `cursor-pointer`). `href` is carried as data — no navigation is performed. |
| `<select>` / `<option>` | ✅ supported | Dropdown-styled box showing the `value` attr (or the first `<option>`'s text) plus a `▾` chevron; clicking opens a themed option menu, clicking a row (or `onchange`) commits the selection. |
| `<dialog>` | ✅ supported | Real modal: hidden unless the `open` attribute is present; when open it dims the page, floats above everything and hugs its content (auto height), centered. |
| `<table>` / `<thead>` / `<tbody>` / `<tfoot>` / `<tr>` / `<td>` / `<th>` | ✅ supported | Table model on flex primitives: table is a column of rows, `td`/`th` are equal-width cells (`th` bold). |
| `<h1>`–`<h6>` | ✅ supported | Headings with browser-like scale (`h1` = `text-4xl font-bold` down to `h6` = `text-base font-semibold`) + margins. |
| `<p>` | ✅ supported | Paragraph — block-level text box. |
| `<text>` | ✅ supported | Plain run of text (default size, small margin). |
| `<label>` | ✅ supported | Inline label row for inputs (`flex items-center gap-2`, `text-sm`, pointer cursor). |
| `<blockquote>` | ✅ supported | Quoted text with a left accent bar and muted color. |
| `<code>` / `<kbd>` | ✅ supported | Monospace-styled inline boxes (`code`: tinted background; `kbd`: bordered keycap). |
| `<nav>` | ✅ supported | Horizontal navigation row (flex row, centered items). |
| `<span>` | ✅ supported | Inline text run (selectable like all text tags, I-beam cursor); `text-*` sizing/weight classes work on any element. |

### Attributes

| Attribute | Applies to | Notes |
|---|---|---|
| `class` / `className` | all | Tailwind class string (compile-time compiled). |
| `value` | `<input>`, `<select>` | Controlled value; drive it from state. For `<select>` it picks the shown option. Without a `value` attr, `<input>`/`<textarea>` are uncontrolled — typed text is kept by the runtime and survives repaints. |
| `placeholder` | `<input>`, `<textarea>` | Shown while unfocused when `value` is empty; hides on focus. |
| `selectable` | text tags | Set `selectable="false"` to opt a text element out of selection (I-beam cursor, double-click select-all, copy). Selectable by default. |
| `src` / `alt` | `<img>` | File path to decode + placeholder text when unavailable. |
| `href` | `<a>` | Carried as data; not navigated. |
| `type` | `<input>` | `text` (default), `search`, `checkbox`, `radio`. |
| `checked` | `<input type="checkbox\|radio">` | Checked state (any value ≠ `false` means checked). |
| `open` | `<dialog>` | Presence makes the dialog visible + centered (any value ≠ `false`). |
| `selected` | `<option>` | Carried as data (option display is app-driven). |
| `onclick` | all | `move \|\| ...` or `move \|e: &PointerEvent\| ...` |
| `oninput` / `onenter` | `<input>`, `<textarea>` | `\|text: &str\|` — full new value. In `<textarea>`, Enter inserts a newline instead of dispatching `onenter`. |
| `onkeydown` | all | `\|k: &KeyEventInfo\|` with `key`, `ctrl`, `shift`, `alt`, `meta`. |

### Selection & clipboard

Browser-style text interaction is built in:

- `<input>`/`<textarea>`: blinking caret (reset while typing), focus ring on
  click, double-click selects all (highlight + Ctrl/Cmd+A too); typing or
  Backspace replaces the selection.
- Static text (`<p>`, `<h1>`–`<h6>`, `<span>`, `<text>`, `<a>`, `<li>`,
  `<blockquote>`, `<code>`, `<kbd>`, table cells): I-beam cursor on hover,
  double-click selects the element's text with a highlight band. Opt out per
  element with `selectable="false"`.
- Clipboard: Ctrl/Cmd+C copies the focused input's value or the selected text,
  Ctrl/Cmd+X cuts (input), Ctrl/Cmd+V pastes at the caret (replacing a
  selection). Native uses the system clipboard; web uses the async Clipboard API.

Several tags ship with browser-style default classes (e.g. `<ul>` gets
`flex-col gap-1 pl-6`, `<select>` gets a bordered pill, `<code>`/`<kbd>` are
monospace) — see `tag_default_classes` in `html_rs_core`. Your own classes are
appended after the defaults, so they win.

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

## Reactivity: fine-grained state, not a VDOM

Reactivity follows the **fine-grained state model popularized by modern JS
frameworks** — reactive state that tracks its readers, and re-runs only the
code that depends on it:

```rust
let count = State::new(0i32);           // State<T> is Copy — capture freely
count.get();                            // tracked read
count.set(5);                           // write → notify
count.update(|c| *c += 1);              // mutate in place
let doubled = html_rs::create_memo(move || count.get() * 2);
html_rs::batch(|| { /* many writes, one flush */ });
```

- Your `launch(move || ui! { ... })` closure runs inside a single **render
  effect**: every state read inside it is tracked, and any write schedules
  exactly one re-render (writes inside a `batch` flush once).
- Event handlers mutate state; the framework re-runs the view, diffs it
  against the previous tree, and surgically repatches only what changed.
- `create_effect` / `create_memo` give you general-purpose reactive primitives.

## Architecture

```
crates/html_rs_state     fine-grained state runtime (thread-local, Copy state)
crates/html_rs_core      Element/Node tree, diff engine, event types, Tailwind class compiler
crates/html_rs_macro     the ui! proc macro (hand-rolled HTML parser, no syn/quote)
crates/html_rs_render    taffy layout → wgpu quads (WGSL SDF) + glyphon text
src/                     facade crate: re-exports + winit runtime + launch()
examples/                desktop + web examples (registered as bins)
web/                     Trunk HTML shells (one per example)
```

- **Layout**: taffy flexbox/grid → physical-pixel boxes. The taffy tree is
  **persistent**: each repaint diffs the new element tree against the previous
  one and only re-lays-out changed subtrees (`set_style` / `set_node_context`
  for in-place patches, teardown + rebuild for structural changes).
- **Paint**: one instanced draw call for rounded-rect quads (premultiplied
  alpha, sRGB→linear), a second textured pass for `<img>` (local files decoded
  via the `image` crate; remote URLs/wasm fall back to `alt` text), and the
  glyphon text pass. Open `<dialog>`s and `<select>` menus are painted in a
  final top pass, so they float above elements that come later in tree order.
  Shaped text buffers are cached per
  node and reused while content/size/wrap are unchanged; on the GPU, only
  quad instances that actually changed are re-uploaded.
- **Hit testing**: painter's algorithm — topmost-last wins; click/focus/hover
  dispatch from the runtime to `onclick` / `oninput` handlers.
- **Skip**: a state write that produces a structurally identical tree
  (`html_rs_core::equivalent`) triggers no repaint at all.

## License

MIT — see [LICENSE](LICENSE).

---

<div align="center">

Built with ♥ by [harishdeivanayagam](https://github.com/harishdeivanayagam)

</div>

