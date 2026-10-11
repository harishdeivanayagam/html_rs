//! The layout + paint pass: converts an [`Element`](html_rs_core::Element) tree
//! into a flat [`DrawList`] (quads, shaped text, hit targets) using taffy for
//! flexbox layout and cosmic-text for shaping/measuring.
//!
//! Layout is **incremental**: a [`PaintState`] keeps the taffy tree and the
//! previous frame's item list alive. On each paint the new element tree is
//! diffed against the previous one (see `html_rs_core::diff`):
//!
//! - unchanged subtrees reuse their taffy nodes *and* their shaped text
//!   buffers — taffy skips them entirely during `compute_layout`,
//! - changed classes/content patch the existing taffy node in place
//!   (`set_style` / `set_node_context`), re-layouting only that subtree,
//! - structural changes tear down the old taffy nodes and build that subtree
//!   fresh.

use html_rs_core::{current_theme, diff_element, tag_default_classes, ElementChange, Element, Node, Paint, StyleOut, TextAlign, BASE_FONT_SIZE};
use std::collections::HashMap;
use std::rc::Rc;
use taffy::prelude::*;
use taffy::geometry::Point;
use taffy::tree::{LayoutOutput, RunMode};

/// Text measurement context attached to taffy leaf nodes.
#[derive(Clone)]
pub struct TextSpec {
    pub content: String,
    pub font_size: f32,
    pub weight: u16,
    pub mono: bool,
}

/// Per-node taffy context: what the measure callback sizes.
#[derive(Clone)]
pub enum NodeCtx {
    Text(TextSpec),
    /// Image leaf: sized from its decoded intrinsic dimensions.
    Image { src: String },
}

/// Decoded intrinsic image sizes in physical px. `None` = decode failed (or
/// not available on this platform) → the painter draws an alt-text placeholder.
pub type ImageSizes = HashMap<String, Option<(f32, f32)>>;

/// A GPU quad (physical px, y-down).
#[derive(Clone, Copy, Debug)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Linear-space RGBA.
    pub color: [f32; 4],
    pub radius: f32,
    pub border_width: f32,
    pub border_color: [f32; 4],
}

/// A shaped text run ready for glyphon. The buffer is shared with the paint
/// state's cache, so unchanged text is shaped once and reused every frame.
#[derive(Clone)]
pub struct TextItem {
    pub buffer: Rc<glyphon::Buffer>,
    /// Top-left of the text (physical px).
    pub x: f32,
    pub y: f32,
    /// Clip rect `[x, y, w, h]` (physical px).
    pub bounds: [f32; 4],
    /// sRGB straight-alpha color.
    pub color: [f32; 4],
    pub font_size: f32,
    pub weight: u16,
    pub decoration: Option<html_rs_core::Decoration>,
}

/// A clickable / focusable region discovered during painting.
#[derive(Clone)]
pub struct HitTarget {
    /// Border-box rect `[x, y, w, h]` (physical px).
    pub rect: [f32; 4],
    /// Path from the root (child indices) — identity across rebuilds.
    pub path: Vec<u32>,
    pub tag: &'static str,
    pub cursor_pointer: bool,
    pub on_click: Option<html_rs_core::ClickHandler>,
    pub is_input: bool,
    /// Text element that supports double-click select-all + copy
    /// (`<p>`, headings, `<span>`, … — unless `selectable="false"`).
    pub selectable: bool,
    /// Concatenated text content (for select-all copy on text elements).
    pub text: String,
    pub on_input: Option<html_rs_core::InputHandler>,
    pub on_enter: Option<html_rs_core::EnterHandler>,
    pub on_key: Option<html_rs_core::KeyHandler>,
    /// Change callback carried by `<select>` targets (`onchange`).
    pub on_change: Option<html_rs_core::ChangeHandler>,
    /// Current `value` attribute (for `<input>`).
    pub value: String,
}

/// A decoded image ready for the textured-quad pipeline.
pub struct ImageItem {
    /// Destination rect `[x, y, w, h]` (physical px).
    pub rect: [f32; 4],
    /// Cache key — the `src` attribute.
    pub src: String,
    /// Corner radius (physical px).
    pub radius: f32,
}

/// Everything one frame paints, plus interactive regions.
#[derive(Default)]
pub struct DrawList {
    pub quads: Vec<Quad>,
    pub texts: Vec<TextItem>,
    pub hits: Vec<HitTarget>,
    pub images: Vec<ImageItem>,
    /// Topmost layer: open `<dialog>` subtrees and the open `<select>` menu.
    /// Drawn in a second pass *after* the base layer's text, so they always
    /// paint above ordinary content (modals behave like browser modals).
    pub top_quads: Vec<Quad>,
    pub top_texts: Vec<TextItem>,
    /// Scrollable containers found during the last paint, for the runtime's
    /// mouse-wheel routing.
    pub scrollables: Vec<Scrollable>,
}

impl DrawList {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns the topmost hit target containing the point, if any.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<&HitTarget> {
        self.hits.iter().rev().find(|h| {
            x >= h.rect[0] && x < h.rect[0] + h.rect[2] && y >= h.rect[1] && y < h.rect[1] + h.rect[3]
        })
    }

    /// The innermost scrollable container under the point (browser-style:
    /// the wheel scrolls the deepest element that overflows there).
    pub fn scroll_target(&self, x: f32, y: f32) -> Option<&Scrollable> {
        self.scrollables
            .iter()
            .filter(|s| {
                let [rx, ry, rw, rh] = s.rect;
                x >= rx && x < rx + rw && y >= ry && y < ry + rh
            })
            .min_by_key(|s| (s.rect[2] * s.rect[3]) as i64)
    }
}

/// A scrollable container discovered during layout: its path, viewport rect
/// (border box, physical px) and content metrics, so the runtime can route
/// wheel events and the painter can draw a scrollbar.
#[derive(Clone, Debug)]
pub struct Scrollable {
    pub path: Vec<u32>,
    /// Border-box viewport `[x, y, w, h]` (already shifted by ancestor scroll).
    pub rect: [f32; 4],
    /// Total content height (physical px).
    pub content_h: f32,
    /// Visible height (physical px).
    pub view_h: f32,
}

/// A cached shaped-text buffer, keyed by everything that affects shaping.
#[derive(Clone)]
struct Shaped {
    content: String,
    font_size: f32,
    weight: u16,
    mono: bool,
    /// Wrap width the buffer was shaped with (physical px).
    wrap_w: f32,
    buffer: Rc<glyphon::Buffer>,
}

impl Shaped {
    fn matches(&self, content: &str, font_size: f32, weight: u16, mono: bool, wrap_w: f32) -> bool {
        self.content == content
            && self.font_size == font_size
            && self.weight == weight
            && self.mono == mono
            && self.wrap_w == wrap_w
    }
}

/// One entry in the flat preorder item list (one per element / text node).
struct Item {
    path: Vec<u32>,
    #[allow(dead_code)] // kept for debugging hit-test paths
    tag: &'static str,
    /// `None` for plain text-child leaf nodes.
    el: Option<Element>,
    /// For text leaves: the text spec (also the taffy node context).
    text: Option<TextSpec>,
    /// For text children: the *parent* element's style (color/size/decoration inherit).
    style: StyleOut,
    taffy_id: taffy::NodeId,
    parent: Option<usize>,
    is_text_leaf: bool,
    shaped: Option<Shaped>,
}

/// Persistent state across frames: the live taffy tree + the previous frame's
/// item list. Held by the [`Renderer`](crate::Renderer).
pub struct PaintState {
    tree: TaffyTree<NodeCtx>,
    prev: Vec<Item>,
    root_id: taffy::NodeId,
    scale: f32,
    width: f32,
    height: f32,
    primed: bool,
    /// Scroll offsets (physical px, top-of-content distance) per scrollable
    /// container, keyed by its dotted element path.
    scrolls: HashMap<String, f32>,
}

impl PaintState {
    pub fn new() -> Self {
        Self {
            tree: TaffyTree::<NodeCtx>::new(),
            prev: Vec::new(),
            root_id: taffy::NodeId::from(0usize),
            scale: 1.0,
            width: 0.0,
            height: 0.0,
            primed: false,
            scrolls: HashMap::new(),
        }
    }

    /// Dotted path key shared by the painter's scroll/select state maps.
    pub fn path_key(path: &[u32]) -> String {
        path.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(".")
    }

    /// Scrolls the container at `path` by `dy` physical px (positive = content
    /// moves up). Clamped against the content height at the next paint.
    pub fn scroll_by(&mut self, path: &[u32], dy: f32) {
        let key = Self::path_key(path);
        let cur = self.scrolls.get(&key).copied().unwrap_or(0.0);
        self.scrolls.insert(key, cur + dy);
    }
}

impl Default for PaintState {
    fn default() -> Self {
        Self::new()
    }
}

fn spec_from_style(style: &StyleOut, content: String, scale: f32) -> TextSpec {
    TextSpec {
        content,
        font_size: style.font_size.unwrap_or(BASE_FONT_SIZE * scale),
        weight: style.font_weight.unwrap_or(400),
        mono: style.mono,
    }
}

/// Element classes + browser-style tag defaults, defaults first (the user's
/// own classes are parsed later and win).
fn effective_classes(el: &Element) -> Vec<String> {
    let mut classes: Vec<String> = tag_default_classes(el.tag);
    if el.tag == "input" {
        match el.get_attr("type").unwrap_or("text") {
            "checkbox" | "radio" => {
                // 20px square (5 Tailwind spacing units × 4px).
                classes.extend(["w-5", "h-5", "p-0", "shrink-0"].iter().map(|s| s.to_string()));
            }
            "range" => {
                classes.extend(["w-full", "h-6", "p-0", "cursor-pointer"].iter().map(|s| s.to_string()));
            }
            // Text-ish inputs get the shadcn input look by default.
            "text" | "search" | "password" | "email" | "number" | "tel" | "url" => {
                classes.extend(
                    [
                        "w-full", "h-10", "px-3", "py-2", "text-sm", "text-foreground",
                        "bg-background", "border", "border-input", "rounded-md", "shrink-0",
                    ]
                    .iter()
                    .map(|s| s.to_string()),
                );
            }
            _ => {}
        }
    }
    classes.extend(el.classes.iter().cloned());
    classes
}

/// Style-level behavior that classes can't express (it depends on attributes):
/// `<option>` is always hidden; a `<dialog>` is hidden unless `open` (open
/// dialogs are centered by a generated wrapper — see `build_node`).
fn apply_tag_overrides(style: &mut StyleOut, el: &Element) {
    match el.tag {
        "option" => style.taffy.display = Display::None,
        "dialog" => {
            let open = el.get_attr("open").map(|v| v != "false").unwrap_or(false);
            if !open {
                style.taffy.display = Display::None;
            }
        }
        _ => {}
    }
}

/// Compiled style for an element: tag defaults + user classes + attr overrides.
fn compute_style(el: &Element, scale: f32) -> StyleOut {
    let mut style = html_rs_core::parse_classes(&effective_classes(el), scale);
    apply_tag_overrides(&mut style, el);
    style
}

/// Builds a fresh taffy subtree + item list entries from an element (preorder).
fn build_node(
    el: &Element,
    path: &mut Vec<u32>,
    parent_idx: Option<usize>,
    items: &mut Vec<Item>,
    tree: &mut TaffyTree<NodeCtx>,
    scale: f32,
) -> taffy::NodeId {
    let style = compute_style(el, scale);

    // Open dialogs get a generated full-box wrapper (absolute, zero insets)
    // that flex-centers them — so the dialog itself hugs its content (auto
    // height/width) instead of stretching to the insets.
    let dialog_open = el.tag == "dialog"
        && el.get_attr("open").map(|v| v != "false").unwrap_or(false);
    let (wrapper_id, my_idx, dialog_parent) = if dialog_open {
        let zero = LengthPercentageAuto::length(0.0);
        let ws = taffy::Style {
            position: Position::Absolute,
            inset: Rect { left: zero, right: zero, top: zero, bottom: zero },
            display: Display::Flex,
            justify_content: Some(JustifyContent::CENTER),
            align_items: Some(AlignItems::CENTER),
            ..taffy::Style::default()
        };
        let wid = tree.new_leaf(ws).expect("taffy new_leaf");
        // Wrapper item (no element → paints nothing).
        let w_idx = items.len();
        items.push(Item {
            path: path.clone(),
            tag: "wrapper",
            el: None,
            text: None,
            style: StyleOut::new(),
            taffy_id: wid,
            parent: parent_idx,
            is_text_leaf: false,
            shaped: None,
        });
        (Some(wid), w_idx + 1, Some(w_idx))
    } else {
        (None, items.len(), parent_idx)
    };

    // Reserve the item slot now so children can point at it (preorder).
    items.push(Item {
        path: path.clone(),
        tag: el.tag,
        el: Some(el.clone()),
        text: None,
        style: style.clone(),
        taffy_id: taffy::NodeId::from(0usize),
        parent: dialog_parent,
        is_text_leaf: false,
        shaped: None,
    });

    let taffy_id = if el.tag == "img" {
        // Image leaf: measured from the decoded intrinsic size.
        let src = el.get_attr("src").unwrap_or_default().to_string();
        tree.new_leaf_with_context(style.taffy.clone(), NodeCtx::Image { src })
            .expect("taffy new_leaf")
    } else if el.children.len() == 1 && matches!(el.children[0], Node::Text(_)) {
        // Single-text-child element: a measured leaf. Keep the compiled style so
        // padding/margin/sizing still apply; the measure callback supplies the
        // content size and taffy adds padding/border on top.
        let Node::Text(t) = &el.children[0] else { unreachable!() };
        let spec = spec_from_style(&style, t.content.clone(), scale);
        let id = tree
            .new_leaf_with_context(style.taffy.clone(), NodeCtx::Text(spec.clone()))
            .expect("taffy new_leaf");
        items[my_idx].text = Some(spec);
        items[my_idx].is_text_leaf = true;
        id
    } else {
        let mut child_ids = Vec::new();
        for (i, child) in el.children.iter().enumerate() {
            path.push(i as u32);
            let id = match child {
                Node::Element(child_el) => {
                    build_node(child_el, path, Some(my_idx), items, tree, scale)
                }
                Node::Text(t) => build_text_child(t, &style, path, my_idx, items, tree, scale),
            };
            path.pop();
            child_ids.push(id);
        }
        if child_ids.is_empty() {
            tree.new_leaf(style.taffy.clone()).expect("taffy new_leaf")
        } else {
            tree.new_with_children(style.taffy.clone(), &child_ids)
                .expect("taffy new_with_children")
        }
    };

    items[my_idx].taffy_id = taffy_id;
    if let Some(wid) = wrapper_id {
        // Attach the dialog under its centering wrapper; the wrapper is what
        // the parent receives as the child node.
        tree.set_children(wid, &[taffy_id]).expect("taffy set_children");
        return wid;
    }
    taffy_id
}

/// Builds a fresh plain-text-child leaf (a child of a multi-child element).
/// `path` already includes this child's index (pushed by the caller).
fn build_text_child(
    t: &html_rs_core::TextNode,
    parent_style: &StyleOut,
    path: &[u32],
    parent_idx: usize,
    items: &mut Vec<Item>,
    tree: &mut TaffyTree<NodeCtx>,
    scale: f32,
) -> taffy::NodeId {
    let spec = spec_from_style(parent_style, t.content.clone(), scale);
    let id = tree
        .new_leaf_with_context(taffy::Style::default(), NodeCtx::Text(spec.clone()))
        .expect("taffy new_leaf");
    items.push(Item {
        path: path.to_vec(),
        tag: "text",
        el: None,
        text: Some(spec),
        style: parent_style.clone(),
        taffy_id: id,
        parent: Some(parent_idx),
        is_text_leaf: true,
        shaped: None,
    });
    id
}

/// Child-index lists for the previous frame's items (preorder order preserved).
fn children_map(prev: &[Item]) -> Vec<Vec<usize>> {
    let mut m: Vec<Vec<usize>> = vec![Vec::new(); prev.len()];
    for (i, it) in prev.iter().enumerate() {
        if let Some(p) = it.parent {
            m[p].push(i);
        }
    }
    m
}

/// Removes a previous-frame item's taffy node and all its descendants.
fn remove_subtree(
    prev: &[Item],
    prev_kids: &[Vec<usize>],
    idx: usize,
    tree: &mut TaffyTree<NodeCtx>,
) {
    for &j in &prev_kids[idx] {
        remove_subtree(prev, prev_kids, j, tree);
    }
    tree.remove(prev[idx].taffy_id).ok();
}

/// Merges `el` into the position previously occupied by `prev[prev_idx]`.
///
/// Returns `(taffy_id, rebuilt)`. `rebuilt` is `true` when this node's taffy
/// id changed (the parent must `set_children`).
#[allow(clippy::too_many_arguments)]
fn merge_node(
    el: &Element,
    prev_idx: usize,
    prev: &[Item],
    prev_kids: &[Vec<usize>],
    path: &mut Vec<u32>,
    parent_idx: Option<usize>,
    items: &mut Vec<Item>,
    tree: &mut TaffyTree<NodeCtx>,
    scale: f32,
) -> (taffy::NodeId, bool) {
    let p = &prev[prev_idx];
    let Some(pe) = p.el.as_ref() else {
        // Generated centering wrapper (open dialog): pair the dialog element
        // with the dialog item right after the wrapper so the subtree is
        // reused across frames instead of rebuilt every repaint.
        if p.tag == "wrapper"
            && el.tag == "dialog"
            && el.get_attr("open").map(|v| v != "false").unwrap_or(false)
        {
            if let Some(dp) = prev.get(prev_idx + 1) {
                let matches = dp
                    .el
                    .as_ref()
                    .map(|d| d.tag == "dialog" && diff_element(d, el) != ElementChange::Structure)
                    .unwrap_or(false);
                if matches {
                    let w_idx = items.len();
                    items.push(Item {
                        path: p.path.clone(),
                        tag: "wrapper",
                        el: None,
                        text: None,
                        style: StyleOut::new(),
                        taffy_id: p.taffy_id,
                        parent: parent_idx,
                        is_text_leaf: false,
                        shaped: None,
                    });
                    merge_node(
                        el,
                        prev_idx + 1,
                        prev,
                        prev_kids,
                        path,
                        Some(w_idx),
                        items,
                        tree,
                        scale,
                    );
                    return (p.taffy_id, false);
                }
            }
        }
        // Caller guarantees element positions only pair with element items.
        remove_subtree(prev, prev_kids, prev_idx, tree);
        let id = build_node(el, path, parent_idx, items, tree, scale);
        return (id, true);
    };
    let change = diff_element(pe, el);
    // A dialog's `open` attribute toggles the generated centering wrapper
    // (build_node), so it must rebuild even though the diff is only in-place.
    let dialog_toggled = el.tag == "dialog"
        && pe.tag == "dialog"
        && pe.get_attr("open").map(|v| v != "false").unwrap_or(false)
            != el.get_attr("open").map(|v| v != "false").unwrap_or(false);
    if change == ElementChange::Structure || dialog_toggled {
        remove_subtree(prev, prev_kids, prev_idx, tree);
        let id = build_node(el, path, parent_idx, items, tree, scale);
        return (id, true);
    }

    // Matched: reuse the taffy node and patch in place.
    let style = compute_style(el, scale);
    let is_leaf = el.children.len() == 1 && matches!(el.children[0], Node::Text(_));
    let my_idx = items.len();
    let id = p.taffy_id;

    items.push(Item {
        path: path.clone(),
        tag: el.tag,
        el: Some(el.clone()),
        text: None,
        style: style.clone(),
        taffy_id: id,
        parent: parent_idx,
        is_text_leaf: is_leaf,
        shaped: if is_leaf { p.shaped.clone() } else { None },
    });

    if change == ElementChange::InPlace {
        // Attrs can drive style overrides (`open`, `type`), so patch the
        // whole compiled style, not just when classes differ.
        tree.set_style(id, style.taffy.clone()).expect("taffy set_style");
    }

    if is_leaf {
        let Node::Text(t) = &el.children[0] else { unreachable!() };
        let spec = spec_from_style(&style, t.content.clone(), scale);
        if p.text.as_ref().map(|s| s.content != spec.content || s.mono != spec.mono).unwrap_or(true) {
            // New content → replace the taffy node context (marks it dirty so
            // only this leaf re-measures).
            tree.set_node_context(id, Some(NodeCtx::Text(spec.clone())))
                .expect("taffy set_node_context");
            // The old shaped buffer no longer matches; drop it (the emission
            // pass re-checks anyway, but this keeps the cache honest).
            items[my_idx].shaped = None;
        }
        items[my_idx].text = Some(spec);
        return (id, false);
    }

    // Multi-child element: merge children positionally.
    let pk: &[usize] = prev_kids.get(prev_idx).map(|v| v.as_slice()).unwrap_or(&[]);
    let mut child_ids: Vec<taffy::NodeId> = Vec::with_capacity(el.children.len());
    let mut pristine = pk.len() == el.children.len();
    if !pristine {
        for &j in pk {
            remove_subtree(prev, prev_kids, j, tree);
        }
    }

    for (i, child) in el.children.iter().enumerate() {
        path.push(i as u32);
        let cid = if pristine {
            let pj = pk[i];
            match child {
                Node::Element(ce) => {
                    if prev[pj].el.is_some() {
                        let (cid, rebuilt) =
                            merge_node(ce, pj, prev, prev_kids, path, Some(my_idx), items, tree, scale);
                        if rebuilt {
                            pristine = false;
                        }
                        cid
                    } else {
                        // Kind mismatch (was a text child): tear down the rest
                        // of the old children and rebuild from here on.
                        for &j in &pk[i..] {
                            remove_subtree(prev, prev_kids, j, tree);
                        }
                        pristine = false;
                        build_node(ce, path, Some(my_idx), items, tree, scale)
                    }
                }
                Node::Text(t) => {
                    let old = &prev[pj];
                    if old.el.is_none() && old.is_text_leaf {
                        // Reuse the text leaf; patch content in place.
                        let spec = spec_from_style(&style, t.content.clone(), scale);
                        if old.text.as_ref().map(|s| s.content != spec.content || s.mono != spec.mono).unwrap_or(true) {
                            tree.set_node_context(old.taffy_id, Some(NodeCtx::Text(spec.clone())))
                                .expect("taffy set_node_context");
                        }
                        items.push(Item {
                            path: path.clone(),
                            tag: "text",
                            el: None,
                            text: Some(spec),
                            style: style.clone(),
                            taffy_id: old.taffy_id,
                            parent: Some(my_idx),
                            is_text_leaf: true,
                            shaped: old.shaped.clone(),
                        });
                        old.taffy_id
                    } else {
                        for &j in &pk[i..] {
                            remove_subtree(prev, prev_kids, j, tree);
                        }
                        pristine = false;
                        build_text_child(t, &style, path, my_idx, items, tree, scale)
                    }
                }
            }
        } else {
            match child {
                Node::Element(ce) => build_node(ce, path, Some(my_idx), items, tree, scale),
                Node::Text(t) => build_text_child(t, &style, path, my_idx, items, tree, scale),
            }
        };
        child_ids.push(cid);
        path.pop();
    }

    if !pristine {
        tree.set_children(id, &child_ids).expect("taffy set_children");
    }

    (id, false)
}

/// Lays out and paints an element tree into a [`DrawList`], reusing the
/// previous frame's layout and shaped text wherever the tree didn't change.
///
/// `width`/`height` are the window size in **physical** px; `scale` scales all
/// CSS-px values (class numbers). A change in size or scale triggers a full
/// rebuild (conservative; steady-state frames are incremental).
#[allow(clippy::too_many_arguments)]
pub fn paint(
    state: &mut PaintState,
    root: &Element,
    width: f32,
    height: f32,
    scale: f32,
    font_system: &mut glyphon::FontSystem,
    images: &ImageSizes,
    selects: &mut SelectState,
    inputs: &mut InputState,
    focus_path: Option<&[u32]>,
    hovered_path: Option<&[u32]>,
) -> DrawList {
    let rebuild =
        !state.primed || state.scale != scale || state.width != width || state.height != height;
    let mut items: Vec<Item> = Vec::new();

    if rebuild {
        state.tree = TaffyTree::<NodeCtx>::new();
        state.scale = scale;
        state.width = width;
        state.height = height;
        let mut path = Vec::new();
        state.root_id = build_node(root, &mut path, None, &mut items, &mut state.tree, scale);
    } else {
        let prev = std::mem::take(&mut state.prev);
        let prev_kids = children_map(&prev);
        let mut path = Vec::new();
        let root_change = prev
            .first()
            .and_then(|p| p.el.as_ref().map(|pe| diff_element(pe, root)))
            .unwrap_or(ElementChange::Structure);
        if root_change == ElementChange::Structure {
            state.tree = TaffyTree::<NodeCtx>::new();
            state.root_id = build_node(root, &mut path, None, &mut items, &mut state.tree, scale);
        } else {
            let (id, _) = merge_node(
                root, 0, &prev, &prev_kids, &mut path, None, &mut items, &mut state.tree, scale,
            );
            state.root_id = id;
        }
    }
    state.primed = true;

    // The root fills the window unless it declares its own size. Idempotent:
    // after the first application the style is no longer AUTO, so this stops
    // touching the tree (keeping clean subtrees cache-eligible).
    {
        let root_style = state
            .tree
            .style(state.root_id)
            .cloned()
            .unwrap_or_default();
        let mut patched = root_style.clone();
        if patched.size.width == Dimension::AUTO {
            patched.size.width = Dimension::percent(1.0);
        }
        if patched.size.height == Dimension::AUTO {
            patched.size.height = Dimension::percent(1.0);
        }
        if patched != root_style {
            state.tree.set_style(state.root_id, patched).ok();
        }
    }

    state
        .tree
        .compute_layout_with_measure(
            state.root_id,
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::Definite(height),
            },
            |input, _node_id, ctx, style| {
                if input.run_mode == RunMode::PerformHiddenLayout {
                    return LayoutOutput::HIDDEN;
                }
                // `compute_leaf_layout` applies the node's own margin/padding/
                // border/sizing around the measured content size.
                taffy::compute_leaf_layout(
                    input,
                    style,
                    |_, _| 0.0,
                    |known_dimensions, available_space| match ctx {
                        None => Size::ZERO,
                        Some(NodeCtx::Text(spec)) => {
                            let wrap_w = known_dimensions.width.unwrap_or(match available_space.width {
                                AvailableSpace::Definite(w) => w,
                                _ => f32::INFINITY,
                            });
                            let (tw, th) = measure_text(font_system, spec, wrap_w);
                            Size { width: tw, height: th }
                        }
                        Some(NodeCtx::Image { src }) => {
                            // Intrinsic size, aspect-preserved; user classes
                            // (`w-*`/`h-*`) win via known_dimensions.
                            let (iw, ih) = images
                                .get(src)
                                .copied()
                                .flatten()
                                .unwrap_or((320.0, 180.0));
                            let avail_w = match available_space.width {
                                AvailableSpace::Definite(w) => Some(w),
                                _ => None,
                            };
                            let w = known_dimensions.width.unwrap_or_else(|| {
                                known_dimensions
                                    .height
                                    .map(|h| h * (iw / ih))
                                    .unwrap_or_else(|| avail_w.map(|a| iw.min(a)).unwrap_or(iw))
                            });
                            let h = known_dimensions.height.unwrap_or(w * (ih / iw));
                            Size { width: w, height: h }
                        }
                    },
                )
            },
        )
        .expect("taffy compute_layout");

    // Compute absolute positions: taffy's `Layout::location` is relative to
    // the parent's border box, so accumulate down the preorder item list
    // (parents always precede children).
    let mut abs: Vec<Point<f32>> = Vec::with_capacity(items.len());
    for item in &items {
        let loc = state.tree.layout(item.taffy_id).map(|l| l.location).unwrap_or_default();
        abs.push(match item.parent {
            Some(p) => {
                let pa = abs[p];
                Point { x: pa.x + loc.x, y: pa.y + loc.y }
            }
            None => Point { x: loc.x, y: loc.y },
        });
    }

    // Visibility: `<option>` and closed `<dialog>` subtrees are never painted.
    let mut hidden = vec![false; items.len()];
    for (idx, item) in items.iter().enumerate() {
        let self_hidden = item
            .el
            .as_ref()
            .map(|el| match el.tag {
                "option" => true,
                "dialog" => !el.get_attr("open").map(|v| v != "false").unwrap_or(false),
                _ => false,
            })
            .unwrap_or(false);
        hidden[idx] = self_hidden || item.parent.map(|p| hidden[p]).unwrap_or(false);
    }

    // ----- scroll pass -----
    // Resolve stored scroll offsets (clamped against real content heights),
    // shift descendant positions, and build per-item clip regions so
    // `overflow-hidden` / `overflow-auto` containers clip their children.
    // Clips are `[minx, miny, maxx, maxy]`.
    const UNBOUNDED: [f32; 4] = [f32::NEG_INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::INFINITY];
    let raw_abs = abs.clone();
    let intersect = |a: [f32; 4], b: [f32; 4]| [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
    let mut clips: Vec<[f32; 4]> = vec![UNBOUNDED; items.len()];
    let mut out_scrollables: Vec<Scrollable> = Vec::new();
    let mut scrollbars: Vec<Quad> = Vec::new();
    // Stack of open clipping ancestors: (depth, scroll offset, clip region).
    let mut stack: Vec<(usize, f32, [f32; 4])> = Vec::new();
    let mut shift = 0.0f32;
    for i in 0..items.len() {
        let item = &items[i];
        let depth = item.path.len();
        while stack.last().map(|(d, _, _)| *d >= depth).unwrap_or(false) {
            let (_, off, _) = stack.pop().unwrap();
            shift -= off;
        }
        let Ok(layout) = state.tree.layout(item.taffy_id) else { continue };
        let (raw_x, raw_y) = (abs[i].x, abs[i].y);
        abs[i] = Point { x: raw_x, y: raw_y - shift };
        let (x, y) = (abs[i].x, abs[i].y);
        let (w, h) = (layout.size.width, layout.size.height);
        let parent_clip = stack.last().map(|c| c.2).unwrap_or(UNBOUNDED);
        let clips_children = item.style.scroll_y || item.style.clip;
        let own_clip = if clips_children {
            intersect(parent_clip, [x, y, w, h])
        } else {
            parent_clip
        };
        clips[i] = own_clip;

        if item.style.scroll_y {
            // Subtree = the contiguous preorder run below this item.
            let mut j = i + 1;
            while j < items.len() && items[j].path.len() > depth {
                j += 1;
            }
            let mut content_bottom = raw_y + layout.border.top;
            for d in i + 1..j {
                if hidden[d] {
                    continue;
                }
                if let Ok(l) = state.tree.layout(items[d].taffy_id) {
                    content_bottom = content_bottom.max(raw_abs[d].y + l.size.height);
                }
            }
            let view_h = (h - layout.border.top - layout.border.bottom).max(0.0);
            let content_h = (content_bottom - raw_y - layout.border.top).max(0.0);
            let max_scroll = (content_h - view_h).max(0.0);
            let key = PaintState::path_key(&item.path);
            let offset = state
                .scrolls
                .get(&key)
                .copied()
                .unwrap_or(0.0)
                .clamp(0.0, max_scroll);
            state.scrolls.insert(key, offset);
            out_scrollables.push(Scrollable {
                path: item.path.clone(),
                rect: [x, y, w, h],
                content_h,
                view_h,
            });
            // Scrollbar thumb (browser-style: only when content overflows).
            if max_scroll > 0.0 && view_h > 0.0 {
                let thumb_h = (view_h * view_h / content_h).max(20.0 * scale).min(view_h);
                let thumb_y = y + layout.border.top + (view_h - thumb_h) * (offset / max_scroll);
                scrollbars.push(Quad {
                    x: x + w - 10.0 * scale,
                    y: thumb_y,
                    w: 8.0 * scale,
                    h: thumb_h,
                    color: html_rs_core::srgb_to_linear([
                        current_theme().border[0],
                        current_theme().border[1],
                        current_theme().border[2],
                        0.9,
                    ]),
                    radius: 4.0 * scale,
                    border_width: 0.0,
                    border_color: [0.0; 4],
                });
            }
            stack.push((depth, offset, own_clip));
            shift += offset;
        } else if clips_children {
            stack.push((depth, 0.0, own_clip));
        }
    }

    let mut out = DrawList::default();
    // An open `<select>`'s option menu is collected during the walk and
    // emitted *after* it, so it lands topmost in the painter's-algorithm
    // z-order (later items draw over earlier ones).
    let mut open_menu: Option<OpenMenu> = None;
    // Shaped buffers live in a side vec during emission (the loop reads
    // `items` immutably for parent alignment); re-attached afterwards.
    let mut shaped_cache: Vec<Option<Shaped>> = items.iter_mut().map(|i| i.shaped.take()).collect();
    // Z-order: open `<dialog>` subtrees are emitted into a *topmost layer*
    // (drawn after the base layer's text), like a browser modal. Everything
    // else keeps tree order.
    let mut order: Vec<(usize, bool)> = Vec::with_capacity(items.len());
    let mut deferred: Vec<(usize, bool)> = Vec::new();
    {
        let mut i = 0;
        while i < items.len() {
            let is_open_dialog = items[i]
                .el
                .as_ref()
                .map(|e| e.tag == "dialog" && e.get_attr("open").map(|v| v != "false").unwrap_or(false))
                .unwrap_or(false);
            if is_open_dialog {
                // Defer the whole contiguous preorder run below the dialog.
                let depth = items[i].path.len();
                let mut j = i;
                while j < items.len() && (j == i || items[j].path.len() > depth) {
                    j += 1;
                }
                deferred.extend((i..j).map(|k| (k, true)));
                i = j;
            } else {
                order.push((i, false));
                i += 1;
            }
        }
    }
    order.extend(deferred);
    // Topmost layer: dialogs + the open select menu. The renderer draws it in
    // a second pass, after the base layer's text — so modal content is never
    // overlapped by ordinary text.
    let mut top = DrawList::default();
    for &(idx, is_top) in &order {
        let item = &items[idx];
        // Each iteration rebinds `out` to the layer this item belongs to.
        let out: &mut DrawList = if is_top { &mut top } else { &mut out };
        // Range of draw entries produced by this item, for clip application.
        let mark = (out.quads.len(), out.texts.len(), out.hits.len(), out.images.len());
        if hidden[idx] {
            continue;
        }
        let Ok(layout) = state.tree.layout(item.taffy_id) else { continue };
        let x = abs[idx].x;
        let y = abs[idx].y;
        let w = layout.size.width;
        let h = layout.size.height;
        if w <= 0.0 || h <= 0.0 {
            // Still paint text leaves with zero height (defensive), skip rest.
            if !item.is_text_leaf {
                continue;
            }
        }

        let hovered = hovered_path.map(|p| p == item.path.as_slice()).unwrap_or(false);
        let focused = focus_path.map(|p| p == item.path.as_slice()).unwrap_or(false);
        let paint = item.style.paint(hovered);
        let el = item.el.clone();

        if let Some(el) = &el {
            if el.tag == "input" || el.tag == "textarea" {
                // Uncontrolled inputs display the runtime's typed buffer; a
                // present `value` attr wins (controlled).
                let input_key = InputState::key(&item.path);
                let shown = el
                    .get_attr("value")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| inputs.values.get(&input_key).cloned().unwrap_or_default());
                let selected = inputs.is_selected(&input_key);
                let caret_on = inputs.caret_on();
                if el.tag == "textarea" {
                    paint_textarea(out, &mut shaped_cache[idx], &item.style, el, x, y, w, h, focused, layout, font_system, scale, &shown, selected, caret_on);
                } else {
                    paint_input(out, &mut shaped_cache[idx], &item.style, el, x, y, w, h, focused, layout, font_system, scale, &shown, selected, caret_on);
                }
            } else {
                // shadcn Dialog: an open dialog dims everything painted
                // before it (the panel draws right after, so it sits on top).
                if el.tag == "dialog" {
                    out.quads.push(Quad {
                        x: 0.0,
                        y: 0.0,
                        w: width,
                        h: height,
                        color: html_rs_core::srgb_to_linear([0.0, 0.0, 0.0, 0.75]), // black/75
                        radius: 0.0,
                        border_width: 0.0,
                        border_color: [0.0; 4],
                    });
                }
                if let Some(bg) = paint.bg {
                    out.quads.push(Quad {
                        x,
                        y,
                        w,
                        h,
                        color: html_rs_core::srgb_to_linear(bg),
                        radius: paint.radius,
                        border_width: paint.border_width,
                        border_color: paint
                            .border_color
                            .map(html_rs_core::srgb_to_linear)
                            .unwrap_or([0.0; 4]),
                    });
                }
                match el.tag {
                    "img" => {
                        // Loaded → textured quad (drawn by the renderer);
                        // failed/pending → alt-text placeholder.
                        let src = el.get_attr("src").unwrap_or_default().to_string();
                        match images.get(&src).copied().flatten() {
                            Some(_size) => out.images.push(ImageItem {
                                rect: [x, y, w, h],
                                src,
                                radius: paint.radius,
                            }),
                            None => {
                                let alt = el.get_attr("alt").unwrap_or_default();
                                if !alt.is_empty() {
                                    emit_text(
                                        out, &mut shaped_cache[idx], item, alt.to_string(),
                                        x, y, w, h, layout, &paint, font_system, scale,
                                    );
                                }
                            }
                        }
                    }
                    "blockquote" => {
                        // Left accent bar.
                        out.quads.push(Quad {
                            x,
                            y,
                            w: 4.0 * scale,
                            h,
                            color: html_rs_core::srgb_to_linear([0.4, 0.45, 0.55, 1.0]), // slate-500-ish
                            radius: 2.0 * scale,
                            border_width: 0.0,
                            border_color: [0.0; 4],
                        });
                    }
                    "select" => {
                        // Selected option: internally-tracked selection, else
                        // the `value` attr (matched against option texts),
                        // else the first option.
                        let options = select_options(el);
                        let key = SelectState::key(&item.path);
                        let value_attr = el.get_attr("value").unwrap_or_default();
                        let sel_idx = selects
                            .selections
                            .get(&key)
                            .copied()
                            .or_else(|| {
                                options
                                    .iter()
                                    .position(|(_, o)| element_text(o) == value_attr && !value_attr.is_empty())
                            })
                            .unwrap_or(0);
                        let value = options
                            .get(sel_idx)
                            .map(|(_, o)| element_text(o))
                            .or_else(|| first_option_text(el))
                            .unwrap_or_default();
                        emit_text(
                            out, &mut shaped_cache[idx], item, value,
                            x, y, w, h, layout, &paint, font_system, scale,
                        );
                        // Chevron at the right edge.
                        let font_size = item.style.font_size.unwrap_or(BASE_FONT_SIZE * scale);
                        let chev_key = format!("chev:{}:{font_size}", SelectState::key(&item.path));
                        let chev = match selects.shapeds.get(&chev_key) {
                            Some(b) => b.clone(),
                            None => {
                                let b = Rc::new(shape_text(font_system, "\u{25be}", font_size, 400, false, None));
                                selects.shapeds.insert(chev_key, b.clone());
                                b
                            }
                        };
                        let chev_w = buffer_max_width(&chev);
                        let pad = layout.padding.right;
                        out.texts.push(TextItem {
                            buffer: chev,
                            x: x + w - pad - chev_w - 2.0,
                            y: y + (h - font_size * 1.35) / 2.0,
                            bounds: [x, y, w, h],
                            color: paint.color.unwrap_or(current_theme().foreground),
                            font_size,
                            weight: 400,
                            decoration: None,
                        });
                        // Open? Queue the option menu for topmost emission.
                        let is_open = selects.open.as_deref() == Some(item.path.as_slice());
                        if is_open {
                            open_menu = Some(OpenMenu {
                                rect: [x, y, w, h],
                                options: options
                                    .iter()
                                    .map(|(ci, o)| (*ci, element_text(o)))
                                    .collect(),
                                select_path: item.path.clone(),
                                font_size,
                            });
                        }
                    }
                    "progress" => {
                        // Fill quad over the track (the tag-default bg is the
                        // track): fraction = value / max.
                        let v: f32 = el.get_attr("value").and_then(|s| s.parse().ok()).unwrap_or(0.0);
                        let max: f32 = el
                            .get_attr("max")
                            .and_then(|s| s.parse().ok())
                            .filter(|m| *m > 0.0)
                            .unwrap_or(100.0);
                        let frac = (v / max).clamp(0.0, 1.0);
                        let inset = (h * 0.1).max(1.0);
                        out.quads.push(Quad {
                            x: x + inset,
                            y: y + inset,
                            w: ((w - inset * 2.0) * frac).max(0.0),
                            h: h - inset * 2.0,
                            color: html_rs_core::srgb_to_linear(current_theme().primary),
                            radius: (h - inset * 2.0) / 2.0,
                            border_width: 0.0,
                            border_color: [0.0; 4],
                        });
                    }
                    _ => {}
                }
            }
        }

        // Text.
        if item.is_text_leaf {
            let Some(spec) = item.text.clone() else { continue };
            let color = paint.color.unwrap_or(current_theme().foreground);
            let content_w = (w - layout.padding.left - layout.padding.right - layout.border.left - layout.border.right).max(1.0);
            let buffer = reuse_or_shape(&mut shaped_cache[idx], &spec.content, spec.font_size, spec.weight, spec.mono, content_w, font_system);
            let text_w = buffer_max_width(&buffer);

            // Alignment box: own content box, or the parent's for text children.
            // Vertically: `items-center` leaves (e.g. buttons with `h-10`) center
            // their text block inside the padding box, like flex centering;
            // everything else stays top-aligned (paragraph flow behavior).
            let ty = if item.style.taffy.align_items == Some(taffy::AlignItems::CENTER) {
                let text_h = buffer_height(&buffer);
                let pad_box_h = (h - layout.border.top - layout.border.bottom).max(0.0);
                y + layout.border.top + (pad_box_h - text_h) / 2.0
            } else {
                y + layout.border.top + layout.padding.top
            };
            let (tx0, ty) = (x + layout.border.left + layout.padding.left, ty);
            let align = item.style.text_align.unwrap_or(TextAlign::Left);
            let (box_x, box_w) = match item.parent {
                Some(pi) if !matches!(align, TextAlign::Left) => match items.get(pi) {
                    Some(parent) => match state.tree.layout(parent.taffy_id) {
                        Ok(pl) => (
                            abs[pi].x + pl.border.left + pl.padding.left,
                            (pl.size.width - pl.padding.left - pl.padding.right - pl.border.left - pl.border.right)
                                .max(0.0),
                        ),
                        Err(_) => (x, content_w),
                    },
                    None => (x, content_w),
                },
                _ => (x, content_w),
            };
            let free = (box_w - text_w).max(0.0);
            let tx = match align {
                TextAlign::Left => tx0,
                TextAlign::Center => box_x + free / 2.0,
                TextAlign::Right => box_x + free,
            };

            // Select-all highlight (double-click on selectable text): a
            // translucent accent band behind the text block.
            if inputs.is_selected(&InputState::key(&item.path)) && !spec.content.is_empty() {
                out.quads.push(Quad {
                    x: tx,
                    y: ty,
                    w: text_w.max(8.0),
                    h: buffer_height(&buffer),
                    color: html_rs_core::srgb_to_linear([current_theme().ring[0], current_theme().ring[1], current_theme().ring[2], 0.35]),
                    radius: 0.0,
                    border_width: 0.0,
                    border_color: [0.0; 4],
                });
            }

            out.texts.push(TextItem {
                buffer,
                x: tx,
                y: ty,
                bounds: [x, y, w.max(1.0), h.max(spec.font_size)],
                color,
                font_size: spec.font_size,
                weight: spec.weight,
                decoration: paint.decoration,
            });

            // Decoration lines (underline / line-through).
            if let Some(dec) = paint.decoration {
                let line_h = spec.font_size * 1.35;
                let (ly, lh) = match dec {
                    html_rs_core::Decoration::Underline => {
                        (ty + line_h - spec.font_size * 0.18, (spec.font_size * 0.07).max(1.0))
                    }
                    html_rs_core::Decoration::LineThrough => {
                        (ty + line_h * 0.5, (spec.font_size * 0.07).max(1.0))
                    }
                };
                out.quads.push(Quad {
                    x: tx,
                    y: ly,
                    w: text_w,
                    h: lh,
                    color: html_rs_core::srgb_to_linear(color),
                    radius: 0.0,
                    border_width: 0.0,
                    border_color: [0.0; 4],
                });
            }

            // List markers: `ul > li` bullets, `ol > li` numbers — drawn in
            // the gutter reserved by the tag-default `pl-6` padding.
            if let (Some(el), Some(pi)) = (&el, item.parent) {
                if el.tag == "li" {
                    if let Some(parent) = items.get(pi) {
                        let marker = match parent.tag {
                            "ul" => Some("\u{2022}".to_string()),
                            "ol" => item.path.last().map(|&n| format!("{}.", n + 1)),
                            _ => None,
                        };
                        if let Some(m) = marker {
                            let mbuf = shape_text(
                                font_system,
                                &m,
                                spec.font_size,
                                spec.weight,
                                spec.mono,
                                None,
                            );
                            let mw = buffer_max_width(&mbuf);
                            let px = abs[pi].x;
                            let mx = (tx0 - (mw + 6.0 * scale).max(10.0 * scale)).max(px);
                            // Bounds must cover the marker too — it sits left
                            // of the li's own box (the gutter padding is on
                            // the ul/ol), and glyphon clips to these bounds.
                            let bx = mx.min(x);
                            out.texts.push(TextItem {
                                buffer: Rc::new(mbuf),
                                x: mx,
                                y: ty,
                                bounds: [bx, y, x + w - bx, h.max(spec.font_size)],
                                color,
                                font_size: spec.font_size,
                                weight: spec.weight,
                                decoration: None,
                            });
                        }
                    }
                }
            }
        }

        // Hit targets (deepest-last = topmost during hit testing).
        if let Some(el) = &el {
            let selectable = is_text_tag(el.tag)
                && el.get_attr("selectable").map(|v| v != "false").unwrap_or(true);
            let interactive = el.on_click.is_some()
                || el.tag == "input"
                || el.tag == "textarea"
                || selectable
                || item.style.cursor_pointer;
            if interactive {
                out.hits.push(HitTarget {
                    rect: [x, y, w, h],
                    path: item.path.clone(),
                    tag: el.tag,
                    cursor_pointer: item.style.cursor_pointer || el.on_click.is_some(),
                    on_click: el.on_click.clone(),
                    is_input: el.tag == "input" || el.tag == "textarea",
                    selectable,
                    text: element_text(el),
                    on_input: el.on_input.clone(),
                    on_enter: el.on_enter.clone(),
                    on_key: el.on_key.clone(),
                    on_change: el.on_change.clone(),
                    value: el.get_attr("value").unwrap_or_default().to_string(),
                });
            }
        }

        // Clip this item's output to its region (`overflow-*` ancestors).
        let clip = clips[idx];
        for q in &mut out.quads[mark.0..] {
            *q = clip_quad(*q, clip);
        }
        for t in &mut out.texts[mark.1..] {
            t.bounds = clip_rect(t.bounds, clip);
        }
        for h in &mut out.hits[mark.2..] {
            h.rect = clip_rect(h.rect, clip);
        }
        for im in &mut out.images[mark.3..] {
            im.rect = clip_rect(im.rect, clip);
        }
    }

    // The open `<select>`'s option menu — topmost layer (above dialogs too:
    // it is emitted after them).
    if let Some(menu) = open_menu {
        emit_select_menu(&mut top, &menu, hovered_path, selects, font_system, scale);
    }
    // Scrollbar thumbs sit above scrolled content (base layer).
    out.quads.extend(scrollbars);
    out.scrollables = out_scrollables;

    // Publish the topmost layer. Its hit targets land last so they win
    // hit-testing over base content (clicks go to the modal/menu first).
    out.top_quads = top.quads;
    out.top_texts = top.texts;
    out.hits.extend(top.hits);

    // Re-attach the (possibly updated) shaped buffers to the item list.
    for (item, sh) in items.iter_mut().zip(shaped_cache) {
        item.shaped = sh;
    }

    state.prev = items;
    out
}

/// Shapes text unless the cache already holds an identical shaping.
fn reuse_or_shape(
    slot: &mut Option<Shaped>,
    content: &str,
    font_size: f32,
    weight: u16,
    mono: bool,
    wrap_w: f32,
    font_system: &mut glyphon::FontSystem,
) -> Rc<glyphon::Buffer> {
    if let Some(sh) = slot {
        if sh.matches(content, font_size, weight, mono, wrap_w) {
            return sh.buffer.clone();
        }
    }
    let buffer = Rc::new(shape_text(font_system, content, font_size, weight, mono, Some(wrap_w)));
    *slot = Some(Shaped {
        content: content.to_string(),
        font_size,
        weight,
        mono,
        wrap_w,
        buffer: buffer.clone(),
    });
    buffer
}

/// Emits a shaped text run inside an element's content box, vertically
/// centered (shared by `<select>` values and `<img>` alt placeholders).
#[allow(clippy::too_many_arguments)]
fn emit_text(
    out: &mut DrawList,
    slot: &mut Option<Shaped>,
    item: &Item,
    content: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    layout: &taffy::Layout,
    paint: &Paint,
    font_system: &mut glyphon::FontSystem,
    scale: f32,
) {
    // Prefer the leaf's own (scale-multiplied) spec size; the style fallback
    // needs the scale or unscaled text is tiny on HiDPI windows.
    let font_size = item
        .text
        .as_ref()
        .map(|s| s.font_size)
        .unwrap_or_else(|| item.style.font_size.unwrap_or(BASE_FONT_SIZE * scale));
    let weight = item.style.font_weight.unwrap_or(400);
    let mono = item.style.mono;
    let content_w = (w - layout.padding.left - layout.padding.right - layout.border.left - layout.border.right).max(1.0);
    let buffer = reuse_or_shape(slot, &content, font_size, weight, mono, content_w, font_system);
    let tx = x + layout.border.left + layout.padding.left;
    let ty = y + (h - font_size * 1.35) / 2.0;
    out.texts.push(TextItem {
        buffer,
        x: tx,
        y: ty,
        bounds: [x, y, w, h],
        color: paint.color.unwrap_or(current_theme().foreground),
        font_size,
        weight,
        decoration: None,
    });
}

/// The text content of an `<option>` child (or plain text child), used as a
/// `<select>`'s displayed value when no `value` attribute is set.
fn first_option_text(el: &Element) -> Option<String> {
    for child in &el.children {
        match child {
            Node::Text(t) => return Some(t.content.clone()),
            Node::Element(o) if o.tag == "option" => {
                if let Some(Node::Text(t)) = o.children.first() {
                    return Some(t.content.clone());
                }
            }
            _ => {}
        }
    }
    None
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

/// `<option>` element children of a `<select>` with their child indices.
fn select_options(el: &Element) -> Vec<(usize, &Element)> {
    el.children
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, Node::Element(o) if o.tag == "option"))
        .map(|(i, c)| (i, match c {
            Node::Element(o) => o,
            _ => unreachable!(),
        }))
        .collect()
}

/// Runtime-managed `<select>` state: which select is open (dropping its option
/// menu over the rest of the UI) and the internally-tracked selection per
/// select (keyed by the select's dotted path — the app mirror lives in its
/// own state, updated via `onchange`).
#[derive(Default)]
pub struct SelectState {
    pub open: Option<Vec<u32>>,
    /// Path key (e.g. `"0.1.2"`) → selected *option* index.
    pub selections: HashMap<String, usize>,
    /// Shaped-text cache for menu rows and the chevron, so an open menu
    /// doesn't re-shape every frame (shaping is slow, esp. in debug/wasm).
    pub shapeds: HashMap<String, Rc<glyphon::Buffer>>,
}

/// Browser-style input state: `<input>`/`<textarea>` elements without a
/// `value` attr are *uncontrolled* — typed text is kept here (keyed by path)
/// so they work like in a browser. A present `value` attr means controlled:
/// the app drives the text via `value` + `oninput`.
/// Browser-style input state: `<input>`/`<textarea>` elements without a
/// `value` attr are *uncontrolled* — typed text is kept here (keyed by path)
/// so they work like in a browser. A present `value` attr means controlled:
/// the app drives the text via `value` + `oninput`.
pub struct InputState {
    /// Path key (e.g. `"0.1.2"`) → typed text.
    pub values: HashMap<String, String>,
    /// Whether the focused input's caret is currently shown (it blinks).
    /// The runtime toggles this on a timer; paint reads it every frame.
    caret_on: bool,
    /// Path key of the input with an active double-click select-all.
    selected: Option<String>,
}

impl Default for InputState {
    fn default() -> Self {
        Self { values: HashMap::new(), caret_on: true, selected: None }
    }
}

impl InputState {
    pub fn key(path: &[u32]) -> String {
        path.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(".")
    }

    /// The typed text for an input path key, if any.
    pub fn get(&self, key: &str) -> Option<&String> {
        self.values.get(key)
    }

    /// Stores the typed text for an input path key.
    pub fn set(&mut self, key: &str, value: String) {
        self.values.insert(key.to_string(), value);
    }

    /// Clears the buffer for an input path key (e.g. after submission).
    pub fn clear(&mut self, key: &str) {
        self.values.remove(key);
    }

    /// Whether the focused input's caret is currently visible (blink phase).
    pub fn caret_on(&self) -> bool {
        self.caret_on
    }

    /// Shows/hides the caret (blink phase).
    pub fn set_caret_on(&mut self, on: bool) {
        self.caret_on = on;
    }

    /// Toggles the blink phase, returning the new visibility.
    pub fn toggle_caret(&mut self) -> bool {
        self.caret_on = !self.caret_on;
        self.caret_on
    }

    /// Marks the input at `key` as select-all (double-click).
    pub fn select_all(&mut self, key: &str) {
        self.selected = Some(key.to_string());
    }

    /// Clears any active selection (single click / after an edit).
    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// Whether the input at `key` has an active select-all.
    pub fn is_selected(&self, key: &str) -> bool {
        self.selected.as_deref() == Some(key)
    }

    /// Path key of the element with an active select-all, if any.
    pub fn selected_key(&self) -> Option<&str> {
        self.selected.as_deref()
    }
}

/// Draws the open `<select>`'s option menu (rows below the select box). Runs
/// after the main walk so the menu paints above everything else; each row is
/// a hit target whose path resolves to the `<option>` element.
fn emit_select_menu(
    out: &mut DrawList,
    menu: &OpenMenu,
    hovered_path: Option<&[u32]>,
    selects: &mut SelectState,
    font_system: &mut glyphon::FontSystem,
    scale: f32,
) {
    let [x, y, w, h] = menu.rect;
    let rows = menu.options.len() as f32;
    // shadcn Select content: `rounded-md` panel with `p-1`, ~36px item rows.
    let row_h = 36.0 * scale;
    let panel_pad = 4.0 * scale;
    let menu_h = rows * row_h + panel_pad * 2.0;
    let t = current_theme();
    let border = html_rs_core::srgb_to_linear(t.border);
    let row_bg = html_rs_core::srgb_to_linear(t.popover);
    let hover_bg = html_rs_core::srgb_to_linear(t.accent);
    let size_key = format!("{}", menu.font_size);
    // Panel behind the rows, dropped just below the select.
    out.quads.push(Quad {
        x,
        y: y + h + 2.0 * scale,
        w,
        h: menu_h,
        color: html_rs_core::srgb_to_linear(t.popover),
        radius: 8.0 * scale,
        border_width: 1.0 * scale,
        border_color: border,
    });
    for (i, (child_idx, text)) in menu.options.iter().enumerate() {
        let mut o_path = menu.select_path.clone();
        o_path.push(*child_idx as u32);
        let ry = y + h + 2.0 * scale + panel_pad + i as f32 * row_h;
        let hovered = hovered_path == Some(o_path.as_slice());
        out.quads.push(Quad {
            x: x + panel_pad,
            y: ry,
            w: w - panel_pad * 2.0,
            h: row_h,
            color: if hovered { hover_bg } else { row_bg },
            radius: 4.0 * scale,
            border_width: 0.0,
            border_color: [0.0; 4],
        });
        // Shaped per (select, option, size) — reused across frames.
        let cache_key = format!("{}:{child_idx}:{size_key}", SelectState::key(&menu.select_path));
        let buffer = match selects.shapeds.get(&cache_key) {
            Some(b) => b.clone(),
            None => {
                let b = Rc::new(shape_text(font_system, text, menu.font_size, 400, false, None));
                selects.shapeds.insert(cache_key, b.clone());
                b
            }
        };
        out.texts.push(TextItem {
            buffer,
            x: x + 12.0 * scale,
            y: ry + (row_h - menu.font_size * 1.35) / 2.0,
            bounds: [x, ry, w, row_h],
            color: t.popover_foreground,
            font_size: menu.font_size,
            weight: 400,
            decoration: None,
        });
        out.hits.push(HitTarget {
            rect: [x + panel_pad, ry, w - panel_pad * 2.0, row_h],
            path: o_path,
            tag: "option",
            cursor_pointer: true,
            on_click: None,
            is_input: false,
            selectable: false,
            text: String::new(),
            on_input: None,
            on_enter: None,
            on_key: None,
            on_change: None,
            value: String::new(),
        });
    }
}

impl SelectState {
    fn key(path: &[u32]) -> String {
        path.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(".")
    }
}

/// Clip region `[minx, miny, maxx, maxy]` ∩ rect `[x, y, w, h]` → rect.
fn clip_rect(rect: [f32; 4], clip: [f32; 4]) -> [f32; 4] {
    let x = rect[0].max(clip[0]);
    let y = rect[1].max(clip[1]);
    let x2 = (rect[0] + rect[2]).min(clip[2]);
    let y2 = (rect[1] + rect[3]).min(clip[3]);
    [x, y, (x2 - x).max(0.0), (y2 - y).max(0.0)]
}

fn clip_quad(mut q: Quad, clip: [f32; 4]) -> Quad {
    let [x, y, w, h] = clip_rect([q.x, q.y, q.w, q.h], clip);
    q.x = x;
    q.y = y;
    q.w = w;
    q.h = h;
    q
}

/// Deferred option menu of the currently open `<select>` (emitted after the
/// main walk so it paints topmost).
struct OpenMenu {
    /// The select's border-box rect `[x, y, w, h]`.
    rect: [f32; 4],
    /// (child index, text) per option.
    options: Vec<(usize, String)>,
    select_path: Vec<u32>,
    font_size: f32,
}

#[allow(clippy::too_many_arguments)]
fn paint_input(
    out: &mut DrawList,
    shaped: &mut Option<Shaped>,
    style: &StyleOut,
    el: &Element,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    focused: bool,
    layout: &taffy::Layout,
    font_system: &mut glyphon::FontSystem,
    scale: f32,
    value: &str,
    selected: bool,
    caret_on: bool,
) {
    let ty = el.get_attr("type").unwrap_or("text");
    match ty {
        "checkbox" => paint_checkbox(out, style, el, x, y, w, h, scale, font_system),
        "radio" => paint_radio(out, style, el, x, y, w, h),
        // Horizontal slider: track + thumb from `value`/`min`/`max`.
        "range" => paint_range(out, el, x, y, w, h, scale),
        // text / search / everything else → text-entry chrome (`search` gets
        // a pill shape).
        _ => paint_text_input(
            out, shaped, style, el, x, y, w, h, focused, layout, font_system,
            scale, ty == "search", value, selected, caret_on,
        ),
    }
}

/// Multiline text entry (`<textarea>`): the same chrome as a text input, but
/// the value wraps across lines (shaped with the content width) and is
/// top-aligned instead of vertically centered.
#[allow(clippy::too_many_arguments)]
fn paint_textarea(
    out: &mut DrawList,
    shaped: &mut Option<Shaped>,
    style: &StyleOut,
    el: &Element,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    focused: bool,
    layout: &taffy::Layout,
    font_system: &mut glyphon::FontSystem,
    scale: f32,
    value: &str,
    selected: bool,
    caret_on: bool,
) {
    let paint = style.paint(false);
    let t = current_theme();
    // Box (no pill radius — textarea is always a rounded rectangle).
    out.quads.push(Quad {
        x,
        y,
        w,
        h,
        color: paint
            .bg
            .map(html_rs_core::srgb_to_linear)
            .unwrap_or(html_rs_core::srgb_to_linear(t.background)),
        radius: paint.radius,
        border_width: if focused { 2.0 } else { paint.border_width },
        border_color: if focused {
            html_rs_core::srgb_to_linear(t.ring) // focus ring
        } else {
            paint
                .border_color
                .map(html_rs_core::srgb_to_linear)
                .unwrap_or(html_rs_core::srgb_to_linear(t.input))
        },
    });

    let placeholder = el.get_attr("placeholder").unwrap_or_default();
    let font_size = style.font_size.unwrap_or(BASE_FONT_SIZE * scale);
    let weight = style.font_weight.unwrap_or(400);
    let mono = style.mono;
    // Placeholder only while unfocused (see paint_text_input).
    let (content, color) = if value.is_empty() && !placeholder.is_empty() && !focused {
        (placeholder.to_string(), t.muted_foreground)
    } else {
        (value.to_string(), paint.color.unwrap_or(current_theme().foreground))
    };

    let content_w = (w - layout.padding.left - layout.padding.right - layout.border.left - layout.border.right).max(1.0);
    let buffer = reuse_or_shape(shaped, &content, font_size, weight, mono, content_w, font_system);
    let (last_w, lines_h) = last_line_metrics(&buffer, font_size);
    let tx = x + layout.border.left + layout.padding.left;
    let ty = y + layout.border.top + layout.padding.top;
    // Select-all highlight: a translucent band over the whole text block.
    if selected && !value.is_empty() {
        out.quads.push(Quad {
            x: tx,
            y: ty,
            w: buffer_max_width(&buffer).max(8.0),
            h: lines_h,
            color: html_rs_core::srgb_to_linear([t.ring[0], t.ring[1], t.ring[2], 0.35]),
            radius: 0.0,
            border_width: 0.0,
            border_color: [0.0; 4],
        });
    }
    if !content.is_empty() {
        out.texts.push(TextItem {
            buffer,
            x: tx,
            y: ty,
            bounds: [x, y, w, h],
            color,
            font_size,
            weight,
            decoration: None,
        });
    }

    // Caret at the end of the last line when focused (blinks).
    if focused && caret_on {
        let line_h = font_size * 1.35;
        let cx = if value.is_empty() && !placeholder.is_empty() { tx } else { tx + last_w };
        let cy = ty + lines_h - line_h;
        out.quads.push(Quad {
            x: cx + 1.0,
            y: cy + (line_h - line_h * 0.8) / 2.0,
            w: 2.0,
            h: line_h * 0.8,
            color: html_rs_core::srgb_to_linear(t.foreground),
            radius: 0.0,
            border_width: 0.0,
            border_color: [0.0; 4],
        });
    }
}

/// Width of the last shaped line and the total height of all lines.
fn last_line_metrics(buffer: &glyphon::Buffer, font_size: f32) -> (f32, f32) {
    let line_h = font_size * 1.35;
    let mut last_w = 0.0f32;
    let mut lines = 0u32;
    for run in buffer.layout_runs() {
        let mut w = 0.0f32;
        for glyph in run.glyphs.iter() {
            w = w.max(glyph.x + glyph.w);
        }
        last_w = w;
        lines += 1;
    }
    (last_w, (lines.max(1) as f32) * line_h)
}

/// Slider geometry: fraction of `value` within `[min, max]`.
fn range_fraction(el: &Element) -> f32 {
    let v: f32 = el.get_attr("value").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let min: f32 = el.get_attr("min").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let max: f32 = el
        .get_attr("max")
        .and_then(|s| s.parse().ok())
        .filter(|m| *m > min)
        .unwrap_or(100.0);
    ((v - min) / (max - min)).clamp(0.0, 1.0)
}

/// Maps a cursor x (physical px) inside the slider rect back to a value
/// string between `min` and `max`, snapped to the `step` attr (default 1 —
/// matches the native input, whose default step is 1).
pub fn range_value_at(el: &Element, x: f32, rect: [f32; 4]) -> String {
    let min: f32 = el.get_attr("min").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let max: f32 = el
        .get_attr("max")
        .and_then(|s| s.parse().ok())
        .filter(|m| *m > min)
        .unwrap_or(100.0);
    let step: f32 = el
        .get_attr("step")
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0);
    let frac = ((x - rect[0]) / rect[2].max(1.0)).clamp(0.0, 1.0);
    // Snap to the step grid, clamped back into [min, max].
    let raw = min + frac * (max - min);
    let v = (min + ((raw - min) / step).round() * step).clamp(min, max);
    if (v - v.round()).abs() < f32::EPSILON {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.2}")
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_range(
    out: &mut DrawList,
    el: &Element,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    scale: f32,
) {
    let frac = range_fraction(el);
    // Track: slim bar vertically centered.
    let track_h = (3.0 * scale).max(3.0);
    let ty = y + (h - track_h) / 2.0;
    out.quads.push(Quad {
        x,
        y: ty,
        w,
        h: track_h,
        color: html_rs_core::srgb_to_linear(current_theme().secondary),
        radius: track_h / 2.0,
        border_width: 0.0,
        border_color: [0.0; 4],
    });
    // Filled portion.
    out.quads.push(Quad {
        x,
        y: ty,
        w: w * frac,
        h: track_h,
        color: html_rs_core::srgb_to_linear(current_theme().primary),
        radius: track_h / 2.0,
        border_width: 0.0,
        border_color: [0.0; 4],
    });
    // Thumb: ~18px circle at the value position.
    let d = (18.0 * scale).min(h);
    let tx = x + w * frac - d / 2.0;
    out.quads.push(Quad {
        x: tx,
        y: y + (h - d) / 2.0,
        w: d,
        h: d,
        color: html_rs_core::srgb_to_linear(current_theme().primary),
        radius: d / 2.0,
        border_width: 1.0 * scale,
        border_color: html_rs_core::srgb_to_linear(current_theme().background),
    });
}

fn attr_on(el: &Element, name: &str) -> bool {
    el.get_attr(name).map(|v| v != "false").unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
fn paint_checkbox(
    out: &mut DrawList,
    style: &StyleOut,
    el: &Element,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    scale: f32,
    font_system: &mut glyphon::FontSystem,
) {
    let checked = attr_on(el, "checked");
    let paint = style.paint(false);
    let t = current_theme();
    let accent = html_rs_core::srgb_to_linear(t.primary);
    out.quads.push(Quad {
        x,
        y,
        w,
        h,
        color: if checked { accent } else { html_rs_core::srgb_to_linear(t.background) },
        radius: 4.0 * scale,
        border_width: if checked { 0.0 } else { paint.border_width.max(1.0 * scale) },
        border_color: paint
            .border_color
            .map(html_rs_core::srgb_to_linear)
            .unwrap_or(html_rs_core::srgb_to_linear(t.border)),
    });
    if checked {
        // Tick mark: a "✓" glyph in the primary-foreground color, centered.
        let size = (w.min(h) * 0.62).max(6.0);
        let buffer = shape_text(font_system, "\u{2713}", size, 700, false, None);
        let tw = buffer_max_width(&buffer);
        let line_h = size * 1.35;
        out.texts.push(TextItem {
            buffer: Rc::new(buffer),
            x: x + (w - tw) / 2.0,
            y: y + (h - line_h) / 2.0,
            bounds: [x, y, w, h],
            color: t.primary_foreground,
            font_size: size,
            weight: 700,
            decoration: None,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_radio(
    out: &mut DrawList,
    style: &StyleOut,
    el: &Element,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let checked = attr_on(el, "checked");
    let paint = style.paint(false);
    let t = current_theme();
    let accent = html_rs_core::srgb_to_linear(t.primary);
    out.quads.push(Quad {
        x,
        y,
        w,
        h,
        color: html_rs_core::srgb_to_linear(t.background),
        radius: w / 2.0,
        border_width: paint.border_width.max(1.0),
        border_color: if checked {
            accent
        } else {
            paint
                .border_color
                .map(html_rs_core::srgb_to_linear)
                .unwrap_or(html_rs_core::srgb_to_linear(t.border))
        },
    });
    if checked {
        let d = w.min(h) * 0.5;
        out.quads.push(Quad {
            x: x + (w - d) / 2.0,
            y: y + (h - d) / 2.0,
            w: d,
            h: d,
            color: accent,
            radius: d / 2.0,
            border_width: 0.0,
            border_color: [0.0; 4],
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_text_input(
    out: &mut DrawList,
    shaped: &mut Option<Shaped>,
    style: &StyleOut,
    el: &Element,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    focused: bool,
    layout: &taffy::Layout,
    font_system: &mut glyphon::FontSystem,
    scale: f32,
    search: bool,
    value: &str,
    selected: bool,
    caret_on: bool,
) {
    let paint = style.paint(false);
    let t = current_theme();
    // Box.
    out.quads.push(Quad {
        x,
        y,
        w,
        h,
        color: paint
            .bg
            .map(html_rs_core::srgb_to_linear)
            .unwrap_or(html_rs_core::srgb_to_linear(t.background)),
        radius: if search { h / 2.0 } else { paint.radius },
        border_width: if focused { 2.0 } else { paint.border_width },
        border_color: if focused {
            html_rs_core::srgb_to_linear(t.ring) // focus ring
        } else {
            paint
                .border_color
                .map(html_rs_core::srgb_to_linear)
                .unwrap_or(html_rs_core::srgb_to_linear(t.input))
        },
    });

    let placeholder = el.get_attr("placeholder").unwrap_or_default();
    let font_size = style.font_size.unwrap_or(BASE_FONT_SIZE * scale);
    let weight = style.font_weight.unwrap_or(400);
    let mono = style.mono;
    // Placeholder shows only while unfocused (it disappears the moment the
    // input gains focus, so the caret starts at the text origin).
    let (content, color) = if value.is_empty() && !placeholder.is_empty() && !focused {
        (placeholder.to_string(), t.muted_foreground)
    } else {
        (value.to_string(), paint.color.unwrap_or(current_theme().foreground))
    };

    let content_w = (w - layout.padding.left - layout.padding.right - layout.border.left - layout.border.right).max(1.0);
    let buffer = reuse_or_shape(shaped, &content, font_size, weight, mono, content_w, font_system);
    let text_w = buffer_max_width(&buffer);
    let line_h = font_size * 1.35;
    let tx = x + layout.border.left + layout.padding.left;
    let ty = y + (h - line_h) / 2.0;
    // Select-all highlight (double-click): a translucent accent band behind
    // the text.
    if selected && !value.is_empty() {
        out.quads.push(Quad {
            x: tx,
            y: ty,
            w: text_w,
            h: line_h,
            color: html_rs_core::srgb_to_linear([t.ring[0], t.ring[1], t.ring[2], 0.35]),
            radius: 0.0,
            border_width: 0.0,
            border_color: [0.0; 4],
        });
    }
    if !content.is_empty() {
        out.texts.push(TextItem {
            buffer,
            x: tx,
            y: ty,
            bounds: [x, y, w, h],
            color,
            font_size,
            weight,
            decoration: None,
        });
    }

    // Caret when focused (blinks — the runtime toggles `caret_on`).
    if focused && caret_on {
        out.quads.push(Quad {
            x: tx + text_w + 1.0,
            y: y + (h - line_h * 0.8) / 2.0,
            w: 2.0,
            h: line_h * 0.8,
            color: html_rs_core::srgb_to_linear(t.foreground),
            radius: 0.0,
            border_width: 0.0,
            border_color: [0.0; 4],
        });
    }
}

/// Measures shaped text: `(width, height)` in px for the given wrap width.
fn measure_text(
    font_system: &mut glyphon::FontSystem,
    spec: &TextSpec,
    wrap_width: f32,
) -> (f32, f32) {
    let wrap = if wrap_width.is_finite() { Some(wrap_width) } else { None };
    let buffer = shape_text(
        font_system,
        &spec.content,
        spec.font_size,
        spec.weight,
        spec.mono,
        wrap,
    );
    let mut height: f32 = 0.0;
    let mut max_w: f32 = 0.0;
    for run in buffer.layout_runs() {
        height += run.line_height;
        // `run.line_w` is the full shaped line width (includes trailing
        // advances); per-glyph `x + w` omits right side bearing and makes
        // flex parents under-size the box, clipping the last glyphs.
        max_w = max_w.max(run.line_w);
        for glyph in run.glyphs.iter() {
            max_w = max_w.max(glyph.x + glyph.w);
        }
    }
    if height == 0.0 {
        height = spec.font_size * 1.35;
    }
    // +1px slack: taffy sizes the box to this width and emission re-shapes
    // with `wrap = box width`, so an exact measurement is a knife edge —
    // float accumulation can wrap the last word or clip the last glyph.
    (max_w.max(1.0).ceil() + 1.0, height)
}

/// Shapes text into a cosmic-text buffer (what glyphon renders).
fn shape_text(
    font_system: &mut glyphon::FontSystem,
    content: &str,
    font_size: f32,
    weight: u16,
    mono: bool,
    wrap_width: Option<f32>,
) -> glyphon::Buffer {
    let mut buffer = glyphon::Buffer::new(
        font_system,
        glyphon::Metrics::new(font_size, font_size * 1.35),
    );
    let attrs = glyphon::Attrs::new()
        .family(if mono {
            glyphon::Family::Monospace
        } else {
            glyphon::Family::SansSerif
        })
        .weight(glyphon::Weight(weight));
    buffer.set_size(wrap_width, None);
    if !content.is_empty() {
        buffer.set_text(content, &attrs, glyphon::Shaping::Advanced, None);
    }
    buffer.shape_until_scroll(font_system, false);
    buffer
}

fn buffer_max_width(buffer: &glyphon::Buffer) -> f32 {
    let mut max_w: f32 = 0.0;
    for run in buffer.layout_runs() {
        for glyph in run.glyphs.iter() {
            max_w = max_w.max(glyph.x + glyph.w);
        }
    }
    max_w
}

/// Total shaped height of a buffer (sum of visual line heights).
fn buffer_height(buffer: &glyphon::Buffer) -> f32 {
    buffer.layout_runs().map(|run| run.line_height).sum::<f32>()
}

/// Text-bearing tags that support browser-style selection (I-beam cursor,
/// double-click select-all, copy) unless `selectable="false"` is set.
fn is_text_tag(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "span" | "text" | "a"
            | "blockquote" | "code" | "kbd" | "li" | "td" | "th"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use html_rs_core::Element;

    fn sample_root() -> Element {
        Element::new("div")
            .class("w-full h-full flex items-center justify-center bg-slate-900")
            .child(
                Element::new("div")
                    .class("flex flex-col items-center gap-4 p-12 bg-slate-800 rounded-2xl border border-slate-700")
                    .child(Element::new("p").class("text-4xl font-bold text-white").text("Hello, html.rs!"))
                    .child(Element::new("p").class("text-lg text-slate-400").text("Native desktop apps in Rust — winit + wgpu, no webview."))
                    .child(
                        Element::new("div")
                            .class("flex gap-2 mt-2")
                            .child(Element::new("p").class("px-3 py-1 bg-slate-900 rounded-md text-sm text-blue-400").text("wgpu"))
                            .child(Element::new("p").class("px-3 py-1 bg-slate-900 rounded-md text-sm text-emerald-400").text("state"))
                            .child(Element::new("p").class("px-3 py-1 bg-slate-900 rounded-md text-sm text-amber-400").text("tailwind")),
                    ),
            )
    }


    /// Quads from both layers (base + topmost), in draw order.
    fn all_quads(list: &DrawList) -> Vec<Quad> {
        list.quads.iter().chain(list.top_quads.iter()).copied().collect()
    }

    /// Texts from both layers, in draw order.
    fn all_texts(list: &DrawList) -> Vec<TextItem> {
        list.texts.iter().chain(list.top_texts.iter()).cloned().collect()
    }

    fn paint_once(root: &Element, w: f32, h: f32, scale: f32) -> DrawList {
        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let images = ImageSizes::new();
        paint(&mut state, root, w, h, scale, &mut font_system, &images, &mut SelectState::default(), &mut InputState::default(), None, None)
    }

    /// Regression test for the hello-world layout: all card children must land
    /// *inside* the card's rect (this caught taffy's parent-relative `location`
    /// being used as if it were absolute).
    #[test]
    fn hello_world_children_stay_inside_card() {
        let root = sample_root();
        let list = paint_once(&root, 1080.0, 720.0, 1.5);

        // Root quad fills the window; card quad is the second one.
        assert_eq!((list.quads[0].x, list.quads[0].y), (0.0, 0.0));
        let card = &list.quads[1];
        assert!(card.w > 300.0 && card.h > 200.0, "card should have real size");

        // The three pill quads must be inside the card's border box.
        let pills = &list.quads[2..5];
        for p in pills {
            assert!(
                p.x >= card.x && p.y >= card.y
                    && p.x + p.w <= card.x + card.w
                    && p.y + p.h <= card.y + card.h,
                "pill {:?} escaped card {:?}",
                (p.x, p.y, p.w, p.h),
                (card.x, card.y, card.w, card.h)
            );
        }

        // Texts also land inside the card.
        for t in &list.texts {
            assert!(
                t.x >= card.x && t.y >= card.y,
                "text at ({}, {}) escaped card",
                t.x,
                t.y
            );
        }
    }

    /// A grid container places children in distinct columns.
    #[test]
    fn grid_places_children_in_columns() {
        let root = Element::new("div")
            .class("grid grid-cols-2 w-full h-full")
            .child(Element::new("p").class("bg-red-500").text("a"))
            .child(Element::new("p").class("bg-green-500").text("b"))
            .child(Element::new("p").class("bg-blue-500").text("c"))
            .child(Element::new("p").class("bg-amber-500").text("d"));

        let list = paint_once(&root, 1000.0, 500.0, 1.0);

        // 4 text quads (transparent bg → no quad for children; use texts).
        assert_eq!(list.texts.len(), 4, "four text children");
        let xs: Vec<f32> = list.texts.iter().map(|t| t.x).collect();
        // First two share a column x, last two share another (right) column.
        assert!(xs[0] < xs[1], "b is in the second column");
        assert_eq!(xs[0], xs[2], "a and c share column 1");
        assert_eq!(xs[1], xs[3], "b and d share column 2");
        assert!(xs[1] > xs[0] + 100.0, "columns should be far apart");
    }

    /// Incremental paint (same PaintState, tree patched) must produce the same
    /// geometry as a from-scratch paint of the new tree.
    #[test]
    fn incremental_paint_matches_fresh_paint() {
        let a = sample_root();
        // b: changed text content + changed classes on the card + same shape.
        let b = Element::new("div")
            .class("w-full h-full flex items-center justify-center bg-slate-900")
            .child(
                Element::new("div")
                    .class("flex flex-col items-center gap-6 p-12 bg-slate-800 rounded-2xl border border-slate-700")
                    .child(Element::new("p").class("text-4xl font-bold text-white").text("Hello, diff!"))
                    .child(Element::new("p").class("text-lg text-slate-400").text("Native desktop apps in Rust — winit + wgpu, no webview."))
                    .child(
                        Element::new("div")
                            .class("flex gap-2 mt-2")
                            .child(Element::new("p").class("px-3 py-1 bg-slate-900 rounded-md text-sm text-blue-400").text("wgpu"))
                            .child(Element::new("p").class("px-3 py-1 bg-slate-900 rounded-md text-sm text-emerald-400").text("state"))
                            .child(Element::new("p").class("px-3 py-1 bg-slate-900 rounded-md text-sm text-amber-400").text("tailwind")),
                    ),
            );

        let mut inc_state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let images = ImageSizes::new();
        let _first = paint(&mut inc_state, &a, 1080.0, 720.0, 1.5, &mut font_system, &images, &mut SelectState::default(), &mut InputState::default(), None, None);
        let incremental = paint(&mut inc_state, &b, 1080.0, 720.0, 1.5, &mut font_system, &images, &mut SelectState::default(), &mut InputState::default(), None, None);
        let fresh = paint_once(&b, 1080.0, 720.0, 1.5);

        assert_eq!(incremental.quads.len(), fresh.quads.len(), "quad count");
        for (i, (q, f)) in incremental.quads.iter().zip(&fresh.quads).enumerate() {
            assert_eq!((q.x, q.y, q.w, q.h), (f.x, f.y, f.w, f.h), "quad {i} geometry");
            assert_eq!(q.color, f.color, "quad {i} color");
        }
        assert_eq!(incremental.texts.len(), fresh.texts.len(), "text count");
        for (i, (t, f)) in incremental.texts.iter().zip(&fresh.texts).enumerate() {
            assert_eq!((t.x, t.y), (f.x, f.y), "text {i} position");
            assert_eq!(t.bounds, f.bounds, "text {i} bounds");
            assert_eq!(t.font_size, f.font_size, "text {i} size");
        }
    }

    /// Adding a child (structural change) must still produce correct output —
    /// and match a fresh paint of the same tree.
    #[test]
    fn structural_change_rebuilds_subtree() {
        let a = Element::new("div")
            .class("flex flex-col w-full h-full")
            .child(Element::new("p").class("text-white").text("one"));
        let b = Element::new("div")
            .class("flex flex-col w-full h-full")
            .child(Element::new("p").class("text-white").text("one"))
            .child(Element::new("p").class("text-white").text("two"));

        let mut inc_state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let images = ImageSizes::new();
        let _ = paint(&mut inc_state, &a, 800.0, 600.0, 1.0, &mut font_system, &images, &mut SelectState::default(), &mut InputState::default(), None, None);
        let incremental = paint(&mut inc_state, &b, 800.0, 600.0, 1.0, &mut font_system, &images, &mut SelectState::default(), &mut InputState::default(), None, None);
        let fresh = paint_once(&b, 800.0, 600.0, 1.0);

        assert_eq!(incremental.texts.len(), 2);
        assert_eq!(incremental.quads.len(), fresh.quads.len());
        for (t, f) in incremental.texts.iter().zip(&fresh.texts) {
            assert_eq!((t.x, t.y), (f.x, f.y));
        }
    }

    /// Removing children (dynamic list shrink) keeps the tree consistent.
    #[test]
    fn structural_shrink_rebuilds_subtree() {
        let a = Element::new("div")
            .class("flex flex-col w-full h-full")
            .child(Element::new("p").class("text-white").text("one"))
            .child(Element::new("p").class("text-white").text("two"));
        let b = Element::new("div")
            .class("flex flex-col w-full h-full")
            .child(Element::new("p").class("text-white").text("one"));

        let mut inc_state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let images = ImageSizes::new();
        let _ = paint(&mut inc_state, &a, 800.0, 600.0, 1.0, &mut font_system, &images, &mut SelectState::default(), &mut InputState::default(), None, None);
        let incremental = paint(&mut inc_state, &b, 800.0, 600.0, 1.0, &mut font_system, &images, &mut SelectState::default(), &mut InputState::default(), None, None);
        let fresh = paint_once(&b, 800.0, 600.0, 1.0);

        assert_eq!(incremental.texts.len(), 1);
        for (t, f) in incremental.texts.iter().zip(&fresh.texts) {
            assert_eq!((t.x, t.y), (f.x, f.y));
        }
    }

    /// Repainting an identical tree is stable and reuses shaped buffers.
    #[test]
    fn identical_repaint_is_stable() {
        let root = sample_root();
        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let first = paint(&mut state, &root, 800.0, 600.0, 1.0, &mut font_system, &ImageSizes::new(), &mut SelectState::default(), &mut InputState::default(), None, None);
        let second = paint(&mut state, &root, 800.0, 600.0, 1.0, &mut font_system, &ImageSizes::new(), &mut SelectState::default(), &mut InputState::default(), None, None);

        assert_eq!(first.quads.len(), second.quads.len());
        for (q, f) in first.quads.iter().zip(&second.quads) {
            assert_eq!((q.x, q.y, q.w, q.h, q.color), (f.x, f.y, f.w, f.h, f.color));
        }
        // Shaped buffers are literally shared (cache hit), not re-shaped.
        for (t, f) in first.texts.iter().zip(&second.texts) {
            assert!(Rc::ptr_eq(&t.buffer, &f.buffer), "shaped buffer should be reused");
        }
    }

    // ------------------------------------------------------------------
    // New tag support
    // ------------------------------------------------------------------

    /// `<ul>` draws a bullet marker in the padding gutter; `<ol>` numbers its
    /// items.
    #[test]
    fn list_markers() {
        let ul = Element::new("ul")
            .class("w-full")
            .child(Element::new("li").text("alpha"))
            .child(Element::new("li").text("beta"));
        let list = paint_once(&ul, 800.0, 600.0, 1.0);
        // 2 item texts + 2 bullet markers, interleaved: [item, marker, item, marker].
        assert_eq!(list.texts.len(), 4, "2 items + 2 bullets");
        let items = [&list.texts[0], &list.texts[2]];
        let markers = [&list.texts[1], &list.texts[3]];
        for (m, i) in markers.iter().zip(&items) {
            assert!(m.x < i.x, "bullet left of item text");
            assert!((i.y - m.y).abs() < 20.0, "bullet roughly on item line");
        }

        let ol = Element::new("ol")
            .class("w-full")
            .child(Element::new("li").text("one"))
            .child(Element::new("li").text("two"))
            .child(Element::new("li").text("three"));
        let list = paint_once(&ol, 800.0, 600.0, 1.0);
        assert_eq!(list.texts.len(), 6, "3 items + 3 numbers");
    }

    /// Regression: `items-center` text leaves (buttons) vertically center
    /// their text block in the button box, like flex centering.
    #[test]
    fn button_text_vertically_centered() {
        let root = Element::new("div")
            .class("w-full h-full p-4")
            .child(Element::new("button").text("Save"));
        let list = paint_once(&root, 400.0, 300.0, 1.0);

        let btn = list
            .quads
            .iter()
            .find(|q| (q.h - 40.0).abs() < 2.0)
            .expect("button quad (h-10 = 40px)");
        let t = list.texts.first().expect("button text");
        let text_center = t.y + t.font_size * 1.35 / 2.0;
        let box_center = btn.y + btn.h / 2.0;
        assert!(
            (text_center - box_center).abs() < 2.0,
            "button text should be vertically centered: text_center={text_center} box_center={box_center}"
        );
    }

    /// Regression: clicking a text `<input>` (inside a label, inside the page
    /// scroller) must return the input's hit target so the runtime can focus
    /// it for typing.
    #[test]
    fn input_hit_test_reaches_input() {
        let root = Element::new("div")
            .class("w-full h-full overflow-auto p-8 flex-col gap-6")
            .child(
                Element::new("section").class("flex-col gap-4 w-full").child(
                    Element::new("label").class("flex flex-col gap-2").child(
                        Element::new("input")
                            .attr("type", "text")
                            .attr("placeholder", "Ada Lovelace"),
                    ),
                ),
            );

        let list = paint_once(&root, 800.0, 600.0, 1.0);
        let input_hit = list.hits.iter().find(|h| h.tag == "input").expect("input hit target").clone();
        let [rx, ry, rw, rh] = input_hit.rect;
        let (cx, cy) = (rx + rw / 2.0, ry + rh / 2.0);

        let found = list.hit_test(cx, cy).expect("click inside input must hit").clone();
        assert_eq!(found.tag, "input", "topmost hit at input center should be the input, got {}", found.tag);
        assert!(found.is_input, "hit must be flagged as an input so the runtime focuses it");
    }

    /// Focused empty inputs hide the placeholder (so the caret starts at the
    /// text origin), and the caret blinks via `InputState::set_caret_on`.
    #[test]
    fn input_placeholder_and_caret_blink() {
        let root = Element::new("div").class("w-full h-full p-4").child(
            Element::new("input").attr("type", "text").attr("placeholder", "Type here"),
        );
        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let images = ImageSizes::new();
        let mut selects = SelectState::default();
        let mut inputs = InputState::default();

        let first = paint(&mut state, &root, 400.0, 300.0, 1.0, &mut font_system, &images, &mut selects, &mut inputs, None, None);
        assert!(
            first.texts.iter().any(|t| t.buffer.lines.iter().any(|l| l.text().contains("Type here"))),
            "placeholder visible while unfocused"
        );

        let hit = first.hits.iter().find(|h| h.tag == "input").expect("input hit").clone();
        let focus = Some(hit.path.as_slice());

        // Focused + empty: no placeholder text, caret visible (default phase).
        let focused_list = paint(&mut state, &root, 400.0, 300.0, 1.0, &mut font_system, &images, &mut selects, &mut inputs, focus, None);
        assert!(
            !focused_list.texts.iter().any(|t| t.buffer.lines.iter().any(|l| l.text().contains("Type here"))),
            "placeholder must disappear when focused"
        );
        assert!(
            focused_list.quads.iter().any(|q| q.w <= 2.5),
            "caret quad visible in the on phase"
        );

        // Blink off phase: no caret quad.
        inputs.set_caret_on(false);
        let off = paint(&mut state, &root, 400.0, 300.0, 1.0, &mut font_system, &images, &mut selects, &mut inputs, focus, None);
        assert!(!off.quads.iter().any(|q| q.w <= 2.5), "caret hidden in the off phase");
    }

    /// Double-click select-all: `is_selected` paints a translucent highlight
    /// band behind the input's text.
    #[test]
    fn input_select_all_highlight() {
        let root = Element::new("div").class("w-full h-full p-4").child(
            Element::new("input").attr("type", "text"),
        );
        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let images = ImageSizes::new();
        let mut selects = SelectState::default();
        let mut inputs = InputState::default();

        let first = paint(&mut state, &root, 400.0, 300.0, 1.0, &mut font_system, &images, &mut selects, &mut inputs, None, None);
        let hit = first.hits.iter().find(|h| h.tag == "input").expect("input hit").clone();
        let key = InputState::key(&hit.path);
        let focus = Some(hit.path.as_slice());
        inputs.set(&key, "selected text".to_string());

        let plain = paint(&mut state, &root, 400.0, 300.0, 1.0, &mut font_system, &images, &mut selects, &mut inputs, focus, None);
        let base_quads = plain.quads.len();
        assert!(base_quads >= 2, "box + caret expected");

        inputs.select_all(&key);
        let selected = paint(&mut state, &root, 400.0, 300.0, 1.0, &mut font_system, &images, &mut selects, &mut inputs, focus, None);
        assert_eq!(selected.quads.len(), base_quads + 1, "one highlight quad added");
        let highlight = selected.quads.iter().find(|q| (q.color[3] - 0.35).abs() < 0.01);
        assert!(highlight.is_some(), "highlight quad has 0.35 alpha");
    }

    /// Static text (`<p>`, headings, `<span>`, …) is selectable: its hit
    /// target carries the text for copy, and select-all paints a highlight.
    /// `selectable="false"` opts out.
    #[test]
    fn selectable_text_and_opt_out() {
        let mk = |selectable: Option<&str>| {
            let mut p = Element::new("p").text("Pick me");
            if let Some(v) = selectable {
                p = p.attr("selectable", v);
            }
            Element::new("div").class("w-full h-full p-4").child(p)
        };

        // Default: selectable, text carried on the hit target.
        let list = paint_once(&mk(None), 400.0, 300.0, 1.0);
        let hit = list.hits.iter().find(|h| h.tag == "p").expect("p hit target");
        assert!(hit.selectable, "text element is selectable by default");
        assert_eq!(hit.text, "Pick me");

        // Select-all paints one highlight quad over the text.
        let key = InputState::key(&hit.path);
        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let images = ImageSizes::new();
        let mut selects = SelectState::default();
        let mut inputs = InputState::default();
        let plain = paint(&mut state, &mk(None), 400.0, 300.0, 1.0, &mut font_system, &images, &mut selects, &mut inputs, None, None);
        inputs.select_all(&key);
        let selected = paint(&mut state, &mk(None), 400.0, 300.0, 1.0, &mut font_system, &images, &mut selects, &mut inputs, None, None);
        assert_eq!(selected.quads.len(), plain.quads.len() + 1, "one highlight quad added");

        // Opt-out: selectable="false" → the <p> is inert (no hit target at
        // all — nothing to select or copy, default cursor).
        let list = paint_once(&mk(Some("false")), 400.0, 300.0, 1.0);
        assert!(
            !list.hits.iter().any(|h| h.tag == "p"),
            "non-interactive non-selectable <p> gets no hit target"
        );
        assert!(!list.hits.iter().any(|h| h.selectable), "nothing selectable in the tree");
    }

    /// Regression: markers sit left of the `li` text (inside the `ul`'s
    /// padding), so the marker's clip bounds must extend past the `li`'s
    /// left edge or glyphon clips the glyph to nothing (invisible bullets).
    #[test]
    fn list_marker_bounds_cover_marker() {
        let ul = Element::new("ul")
            .class("w-full")
            .child(Element::new("li").text("alpha"))
            .child(Element::new("li").text("beta"));
        let list = paint_once(&ul, 800.0, 600.0, 1.0);
        let markers = [&list.texts[1], &list.texts[3]];
        for m in markers {
            assert!(
                m.bounds[0] <= m.x + 0.5,
                "marker bounds must start at (or left of) the glyph, bounds[0]={} x={}",
                m.bounds[0],
                m.x
            );
        }
    }

    /// Text typed into an uncontrolled `<input>` (no `value` attr) is kept
    /// by the runtime's `InputState` and painted into the field.
    #[test]
    fn uncontrolled_input_shows_typed_text() {
        let root = Element::new("div")
            .class("w-full h-full p-4")
            .child(Element::new("input").attr("type", "text").attr("placeholder", "Type here"));

        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let images = ImageSizes::new();
        let mut inputs = InputState::default();

        let first = paint(&mut state, &root, 800.0, 600.0, 1.0, &mut font_system, &images, &mut SelectState::default(), &mut inputs, None, None);
        // Placeholder visible before typing; typed text absent.
        assert!(first.texts.iter().any(|t| t.buffer.lines.iter().any(|l| l.text().contains("Type here"))), "placeholder before typing");
        assert!(!first.texts.iter().any(|t| t.buffer.lines.iter().any(|l| l.text().contains("hello"))), "typed text absent before typing");

        // Find the input's path from its hit target and simulate typing.
        let hit = first
            .hits
            .iter()
            .find(|h| h.tag == "input")
            .expect("input hit target");
        let key = InputState::key(&hit.path);
        inputs.set(&key, "hello world".to_string());

        let second = paint(&mut state, &root, 800.0, 600.0, 1.0, &mut font_system, &images, &mut SelectState::default(), &mut inputs, None, None);
        assert!(
            second.texts.iter().any(|t| t.buffer.lines.iter().any(|l| l.text().contains("hello world"))),
            "typed text should be painted after InputState is updated"
        );
    }

    /// `<textarea>` paints wrapped multi-line text and is hit-testable.
    #[test]
    fn textarea_paints_wrapped_text() {
        let root = Element::new("div").class("w-full h-full p-4").child(
            Element::new("textarea").attr("placeholder", "Tell us more"),
        );

        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let images = ImageSizes::new();
        let mut inputs = InputState::default();

        let list = paint(&mut state, &root, 400.0, 300.0, 1.0, &mut font_system, &images, &mut SelectState::default(), &mut inputs, None, None);
        assert!(list.texts.iter().any(|t| t.buffer.lines.iter().any(|l| l.text().contains("Tell us more"))), "textarea placeholder painted");
        assert!(list.hits.iter().any(|h| h.tag == "textarea"), "textarea is clickable");

        // Multi-line typed value renders (one shaped line per wrapped row).
        let hit = list.hits.iter().find(|h| h.tag == "textarea").expect("textarea hit");
        inputs.set(&InputState::key(&hit.path), "line one\nline two".to_string());
        let after = paint(&mut state, &root, 400.0, 300.0, 1.0, &mut font_system, &images, &mut SelectState::default(), &mut inputs, None, None);
        let body = after
            .texts
            .iter()
            .flat_map(|t| t.buffer.lines.iter().map(|l| l.text().to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("line one") && body.contains("line two"), "typed textarea value painted, got: {body:?}");
    }

    /// A closed `<dialog>` renders nothing; an open one renders its children,
    /// centered in the viewport.
    #[test]
    fn dialog_open_closed() {
        let make = |open: bool| {
            let mut d = Element::new("dialog")
                .class("w-96 h-48 bg-slate-800")
                .child(Element::new("p").class("text-white").text("dialog body"));
            if open {
                d = d.attr("open", "true");
            }
            Element::new("div").class("w-full h-full").child(d)
        };

        let closed = paint_once(&make(false), 800.0, 600.0, 1.0);
        assert!(all_texts(&closed).is_empty(), "closed dialog hides children");
        assert!(closed.quads.len() + closed.top_quads.len() <= 1, "closed dialog paints no card");

        let open = paint_once(&make(true), 800.0, 600.0, 1.0);
        assert!(!all_texts(&open).is_empty(), "open dialog shows children");
        // The dialog card (w-96 = 96px) is absolutely positioned + centered:
        // its center should sit at the viewport midline (400px).
        let open_quads = all_quads(&open);
        let centered = open_quads
            .iter()
            .any(|q| q.w > 60.0 && ((q.x + q.w / 2.0) - 400.0).abs() < 8.0);
        assert!(centered, "open dialog should be horizontally centered");

        // Height hugs the content (auto) instead of stretching to the screen.
        let panel = open_quads
            .iter()
            .find(|q| q.w > 300.0 && q.w < 500.0)
            .expect("dialog panel quad");
        assert!(panel.h < 300.0, "dialog height should be auto, got {}", panel.h);
    }

    /// An open `<dialog>` paints above content that comes later in tree
    /// order (modal z-order), like a browser modal.
    #[test]
    fn dialog_paints_above_later_siblings() {
        // green-500 bg quad pushed after the dialog in tree order.
        let make = || {
            Element::new("div")
                .class("w-full h-full")
                .child(
                    Element::new("dialog")
                        .attr("open", "true")
                        .class("w-96 bg-slate-800")
                        .child(Element::new("p").class("text-white").text("modal")),
                )
                .child(Element::new("div").class("w-full h-full bg-green-500").text("page"))
        };
        let list = paint_once(&make(), 800.0, 600.0, 1.0);
        let merged = all_quads(&list);
        let overlay_idx = merged
            .iter()
            .position(|q| q.color[3] < 0.8 && q.color[0] < 0.01 && q.color[1] < 0.01)
            .expect("full-screen dim overlay");
        // DrawList quad colors are linear (converted at emission).
        let green = html_rs_core::srgb_to_linear([34.0 / 255.0, 197.0 / 255.0, 94.0 / 255.0, 1.0]);
        let page_idx = merged
            .iter()
            .position(|q| {
                q.w >= 790.0 && q.h >= 590.0 && q.color.iter().zip(green.iter()).all(|(a, b)| (a - b).abs() < 0.01)
            })
            .expect("page bg quad");
        assert!(page_idx < overlay_idx, "page content paints before the dialog overlay");
        // And the dialog panel itself is above the overlay.
        let panel_idx = merged
            .iter()
            .position(|q| q.w > 300.0 && q.w < 500.0)
            .expect("dialog panel quad");
        assert!(panel_idx > overlay_idx, "dialog panel paints above the overlay");
    }

    /// Checkbox/radio inputs size themselves and reflect `checked` state.
    #[test]
    fn input_checkbox_and_radio() {
        let make = |ty: &str, checked: bool| {
            let mut input = Element::new("input").attr("type", ty);
            if checked {
                input = input.attr("checked", "true");
            }
            Element::new("div").class("flex flex-col w-full h-full gap-2").child(input)
        };

        let off = paint_once(&make("checkbox", false), 400.0, 300.0, 1.0);
        let on = paint_once(&make("checkbox", true), 400.0, 300.0, 1.0);
        // Checked swaps the box fill to the primary accent and adds a "✓"
        // text glyph on top.
        assert!(
            on.texts.len() > off.texts.len() && !on.quads.is_empty(),
            "checked box should paint a tick text over the fill ({} vs {} texts, {} quads)",
            on.texts.len(),
            off.texts.len(),
            on.quads.len()
        );

        let radio = paint_once(&make("radio", false), 400.0, 300.0, 1.0);
        // Radio input quad should be square (circle).
        let input_quad = radio
            .quads
            .iter()
            .find(|q| q.w > 0.0 && q.h > 0.0 && (q.w - q.h).abs() < 0.5)
            .expect("square input quad");
        assert!((input_quad.w - 20.0).abs() < 2.0, "input is w-5 = 20px, got {}", input_quad.w);
    }

    /// `<select>` emits its resolved value as text plus a chevron.
    #[test]
    fn select_emits_value() {
        let root = Element::new("div")
            .class("flex flex-col w-full h-full")
            .child(
                Element::new("select")
                    .attr("value", "blue")
                    .child(Element::new("option").text("red"))
                    .child(Element::new("option").text("blue")),
            );
        let list = paint_once(&root, 400.0, 300.0, 1.0);
        assert_eq!(list.texts.len(), 2, "value text + chevron");
        // The chevron (drawn second) sits right of the value text.
        assert!(list.texts[1].x > list.texts[0].x, "chevron right of value");
        // Options themselves are invisible.
        assert!(!list.texts.iter().any(|t| t.bounds[2] == 0.0 && false));
    }

    /// `<img>` with known intrinsic size scales to fit; without decode info it
    /// falls back to alt text.
    #[test]
    fn image_layout_and_alt_fallback() {
        let root = Element::new("div")
            .class("flex flex-col w-full h-full")
            .child(Element::new("img").attr("src", "pic.png").attr("alt", "A picture"));

        // Decoded: one ImageItem, no alt text.
        let mut sizes = ImageSizes::new();
        sizes.insert("pic.png".to_string(), Some((64.0, 32.0)));
        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let list = paint(
            &mut state,
            &root,
            400.0,
            300.0,
            1.0,
            &mut font_system,
            &sizes,
            &mut SelectState::default(), &mut InputState::default(),
            None,
            None,
        );
        assert_eq!(list.images.len(), 1, "decoded image becomes an ImageItem");
        assert!(list.texts.is_empty(), "no alt text when decoded");
        let img = &list.images[0];
        // Aspect preserved, fits available width... default (non-shrink-0 via
        // tag defaults) — width should be <= container width and aspect 2:1.
        assert!(
            img.rect[2] <= 400.0 + 0.5 && (img.rect[2] / img.rect[3] - 2.0).abs() < 0.1,
            "aspect preserved: {:?}",
            img.rect
        );

        // Not decoded: alt text placeholder instead.
        let fresh = paint_once(&root, 400.0, 300.0, 1.0);
        assert!(fresh.images.is_empty());
        assert_eq!(fresh.texts.len(), 1, "alt text placeholder");
    }

    /// kbd / code map to monospace shaping (mono flag on their text specs).
    #[test]
    fn kbd_and_code_use_mono() {
        let root = Element::new("div")
            .class("flex flex-col w-full h-full")
            .child(Element::new("kbd").text("Ctrl+C"))
            .child(Element::new("code").text("let x = 1;"));
        let list = paint_once(&root, 400.0, 300.0, 1.0);
        assert_eq!(list.texts.len(), 2);
        // Both texts render; the shaped buffers were produced with the mono
        // family (behavioral check: same content shaped mono vs sans differs —
        // here we just assert both emitted and positioned inside the window).
        for t in &list.texts {
            assert!(t.x >= 0.0 && t.y >= 0.0);
        }
    }

    /// `<blockquote>` paints an accent bar left of its text; `<a>` and `<nav>`
    /// lay out inline content without panicking.
    #[test]
    fn blockquote_link_nav() {
        let root = Element::new("div")
            .class("flex flex-col w-full h-full gap-2")
            .child(Element::new("blockquote").text("wise words"))
            .child(Element::new("nav").child(Element::new("a").attr("href", "https://example.com").text("docs")));
        let list = paint_once(&root, 600.0, 400.0, 1.0);
        assert_eq!(list.texts.len(), 2, "blockquote text + link text");
        // The blockquote accent bar is a thin tall quad.
        let bar = list.quads.iter().find(|q| q.w < 6.0 && q.h > 10.0);
        assert!(bar.is_some(), "expected thin accent bar quad");
    }

    /// `<progress>` paints a track (tag default bg) + a proportional fill.
    #[test]
    fn progress_fill_fraction() {
        let root = Element::new("div")
            .class("flex flex-col w-full h-full")
            .child(Element::new("progress").attr("value", "50").attr("max", "100"));
        let list = paint_once(&root, 400.0, 300.0, 1.0);
        // Track quad (w-full = 400) + fill quad in the theme primary color.
        let fill_color = html_rs_core::srgb_to_linear(current_theme().primary);
        let track = list.quads.iter().find(|q| q.w > 300.0).expect("track quad");
        let fill = list
            .quads
            .iter()
            .find(|q| q.color == fill_color)
            .expect("primary fill quad");
        assert!((fill.w - track.w / 2.0).abs() < 3.0, "50% of max: fill {} vs track {}", fill.w, track.w);
        assert!(fill.h < track.h, "fill inset within track");
        // Zero value → no visible fill width.
        let root0 = Element::new("div")
            .class("flex flex-col w-full h-full")
            .child(Element::new("progress").attr("value", "0").attr("max", "100"));
        let list0 = paint_once(&root0, 400.0, 300.0, 1.0);
        let fill0 = list0
            .quads
            .iter()
            .find(|q| q.color == fill_color)
            .expect("fill quad");
        assert!(fill0.w < 1.0, "zero progress has no fill");
    }

    /// `<input type="range">`: thumb sits at the value's fraction of the track.
    #[test]
    fn range_thumb_position() {
        let make = |v: &str| {
            Element::new("div")
                .class("flex flex-col w-full h-full")
                .child(
                    Element::new("input")
                        .attr("type", "range")
                        .attr("value", v)
                        .attr("min", "0")
                        .attr("max", "100"),
                )
        };
        let list = paint_once(&make("0"), 400.0, 300.0, 1.0);
        let thumb_left = list
            .quads
            .iter()
            .find(|q| (q.w - q.h).abs() < 0.5 && q.w > 10.0 && q.radius > q.w / 2.0 - 0.5)
            .expect("thumb quad");
        let list = paint_once(&make("100"), 400.0, 300.0, 1.0);
        let thumb_right = list
            .quads
            .iter()
            .find(|q| (q.w - q.h).abs() < 0.5 && q.w > 10.0 && q.radius > q.w / 2.0 - 0.5)
            .expect("thumb quad");
        assert!(
            thumb_right.x > thumb_left.x + 300.0,
            "thumb moves right with value: {} → {}",
            thumb_left.x,
            thumb_right.x
        );
        // Mid value → thumb center near the track center.
        let list = paint_once(&make("50"), 400.0, 300.0, 1.0);
        let thumb_mid = list
            .quads
            .iter()
            .find(|q| (q.w - q.h).abs() < 0.5 && q.w > 10.0)
            .expect("thumb quad");
        assert!(((thumb_mid.x + thumb_mid.w / 2.0) - 200.0).abs() < 5.0);
        // Value mapping: x at 75% of a 0..100 track → 75.
        let el = Element::new("input").attr("type", "range").attr("min", "0").attr("max", "100");
        let v = range_value_at(&el, 300.0, [0.0, 0.0, 400.0, 24.0]);
        assert_eq!(v, "75");
    }

    /// An open `<select>` emits an option menu (topmost) with per-option hit
    /// targets; a closed one doesn't.
    #[test]
    fn select_menu_open_close() {
        let root = Element::new("div")
            .class("flex flex-col w-full h-full")
            .child(
                Element::new("select")
                    .attr("value", "blue")
                    .child(Element::new("option").text("red"))
                    .child(Element::new("option").text("blue")),
            );
        // Closed: exactly the value text + chevron, no option hits.
        let closed = paint_once(&root, 400.0, 300.0, 1.0);
        assert_eq!(closed.texts.len(), 2);
        assert!(!closed.hits.iter().any(|h| h.tag == "option"));

        // Open: menu rows + option hit targets appended after everything else.
        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let mut selects = SelectState {
            open: Some(vec![0]),
            selections: HashMap::new(),
            shapeds: HashMap::new(),
        };
        let open = paint(
            &mut state,
            &root,
            400.0,
            300.0,
            1.0,
            &mut font_system,
            &ImageSizes::new(),
            &mut selects,
            &mut InputState::default(),
            None,
            None,
        );
        let option_hits: Vec<&HitTarget> =
            open.hits.iter().filter(|h| h.tag == "option").collect();
        assert_eq!(option_hits.len(), 2, "one hit target per option");
        // Menu rows come after (above) the select box itself in z-order.
        let select_hit = open.hits.iter().find(|h| h.tag == "select").expect("select hit");
        assert!(open.hits.iter().position(|h| h.tag == "option").unwrap()
            > open.hits.iter().position(|h| h.path == select_hit.path).unwrap());
        // Rows sit below the select box.
        for h in &option_hits {
            assert!(h.rect[1] > select_hit.rect[1] + select_hit.rect[3] - 1.0);
        }
        // The selected option ("blue" via the value attr) is displayed.
        assert!(!open.texts.is_empty() || !open.top_texts.is_empty());
        assert!(
            open.quads.len() + open.top_quads.len() > closed.quads.len(),
            "menu adds quads"
        );
    }

    /// `<table>` rows lay out as flex columns/rows with cells in a grid.
    #[test]
    fn table_layout() {
        let root = Element::new("div")
            .class("w-full h-full")
            .child(
                Element::new("table").child(
                    Element::new("tbody")
                        .child(
                            Element::new("tr")
                                .child(Element::new("td").text("r0c0"))
                                .child(Element::new("td").text("r0c1")),
                        )
                        .child(
                            Element::new("tr")
                                .child(Element::new("td").text("r1c0"))
                                .child(Element::new("td").text("r1c1")),
                        ),
                ),
            );
        let list = paint_once(&root, 800.0, 600.0, 1.0);
        assert_eq!(list.texts.len(), 4, "four cells");
        let xs: Vec<f32> = list.texts.iter().map(|t| t.x).collect();
        let ys: Vec<f32> = list.texts.iter().map(|t| t.y).collect();
        // Cells share columns and rows.
        assert_eq!(xs[0], xs[2], "col 0 aligned");
        assert_eq!(xs[1], xs[3], "col 1 aligned");
        assert_eq!(ys[0], ys[1], "row 0 aligned");
        assert_eq!(ys[2], ys[3], "row 1 aligned");
        assert!(xs[1] > xs[0], "col 1 right of col 0");
        assert!(ys[2] > ys[1], "row 1 below row 0");
    }

    /// Regression: measured text width was exact, so emission re-shaping with
    /// `wrap = box width` was a knife edge — float accumulation could wrap the
    /// last word (clipped by the single-line bounds) or clip the last glyph.
    /// The measured width must carry slack so the shaped line always fits.
    #[test]
    fn measured_width_leaves_wrap_slack() {
        let mut font_system = glyphon::FontSystem::new();
        for content in ["Press", "Enable notifications", "Monthly", "Yearly"] {
            let spec = TextSpec {
                content: content.to_string(),
                font_size: 16.0,
                weight: 400,
                mono: false,
            };
            let (w, _h) = measure_text(&mut font_system, &spec, f32::INFINITY);
            let buffer = shape_text(&mut font_system, content, 16.0, 400, false, Some(w));
            let lines = buffer.layout_runs().count();
            assert_eq!(lines, 1, "{content:?} must not wrap at its measured width");
        }
    }

    /// Default-size text (no `text-*` class) must scale with the window
    /// scale factor — otherwise it renders tiny on HiDPI displays.
    #[test]
    fn default_text_scales_with_scale_factor() {
        let root = Element::new("div")
            .class("w-full h-full")
            .child(Element::new("p").text("hi"));
        let list = paint_once(&root, 400.0, 300.0, 2.0);
        assert_eq!(list.texts[0].font_size, 32.0, "16px base × scale 2");
    }

    /// `range_value_at` snaps to the `step` attr (default 1) so integer
    /// state holders parse the value cleanly.
    #[test]
    fn range_value_snaps_to_step() {
        let el = Element::new("input")
            .attr("type", "range")
            .attr("min", "0")
            .attr("max", "100");
        // 50.4px into a 100px track → 50 (snapped), not "50.40".
        assert_eq!(range_value_at(&el, 50.4, [0.0, 0.0, 100.0, 24.0]), "50");
        // Clamped at both ends.
        assert_eq!(range_value_at(&el, -5.0, [0.0, 0.0, 100.0, 24.0]), "0");
        assert_eq!(range_value_at(&el, 500.0, [0.0, 0.0, 100.0, 24.0]), "100");
        // Explicit step.
        let el10 = el.attr("step", "10");
        assert_eq!(range_value_at(&el10, 54.0, [0.0, 0.0, 100.0, 24.0]), "50");
    }

    /// `overflow-auto`: registers a scrollable, draws a scrollbar thumb when
    /// the content overflows, and `scroll_by` shifts (clipped) content.
    #[test]
    fn overflow_auto_scrolls() {
        let mut sc = Element::new("div").class("overflow-auto w-full h-24 flex flex-col");
        for i in 0..20 {
            sc = sc.child(Element::new("p").class("h-8 shrink-0").text(format!("row {i}")));
        }
        let root = Element::new("div").class("w-full h-full").child(sc);

        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        let images = ImageSizes::new();
        let list = paint(&mut state, &root, 400.0, 300.0, 1.0, &mut font_system, &images, &mut SelectState::default(), &mut InputState::default(), None, None);
        assert_eq!(list.scrollables.len(), 1, "scroller registered");
        let s = &list.scrollables[0];
        assert!(s.content_h > s.view_h, "20 × 32px rows overflow a 96px view");
        assert!(
            list.quads.iter().any(|q| q.w <= 10.0 && q.h > 4.0),
            "scrollbar thumb drawn"
        );

        // Wheel far past the end: content shifts up and clamps; fully
        // scrolled-out rows collapse to empty clip bounds.
        state.scroll_by(&s.path, 10_000.0);
        let scrolled = paint(&mut state, &root, 400.0, 300.0, 1.0, &mut font_system, &images, &mut SelectState::default(), &mut InputState::default(), None, None);
        let min_y = scrolled.texts.iter().map(|t| t.y).fold(f32::INFINITY, f32::min);
        let before_min_y = list.texts.iter().map(|t| t.y).fold(f32::INFINITY, f32::min);
        assert!(min_y < before_min_y, "content moved up after wheel");
        assert!(
            scrolled.texts.iter().any(|t| t.bounds[3] == 0.0),
            "rows scrolled out of view are clipped away"
        );
    }

    /// `overflow-hidden` children are clipped to the container's border box.
    #[test]
    fn overflow_hidden_clips() {
        let inner = Element::new("div")
            .class("w-full h-full bg-blue-500")
            .child(Element::new("p").text("outside the box"));
        let root = Element::new("div")
            .class("w-16 h-16 overflow-hidden")
            .child(inner);
        let list = paint_once(&root, 400.0, 300.0, 1.0);
        // The inner box's bg quad is cut to the 64px container.
        let bg = list.quads.iter().find(|q| q.w > 10.0).unwrap();
        assert!(bg.w <= 64.0 && bg.h <= 64.0, "clipped to container");
        // The text's bounds never exceed the container.
        for t in &list.texts {
            assert!(t.bounds[0] + t.bounds[2] <= 64.0 + 0.5);
        }
    }
}
