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

use html_rs_core::{diff_element, ElementChange, Element, Node, StyleOut, TextAlign, BASE_FONT_SIZE, DEFAULT_TEXT};
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
}

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
pub struct HitTarget {
    /// Border-box rect `[x, y, w, h]` (physical px).
    pub rect: [f32; 4],
    /// Path from the root (child indices) — identity across rebuilds.
    pub path: Vec<u32>,
    pub tag: &'static str,
    pub cursor_pointer: bool,
    pub on_click: Option<html_rs_core::ClickHandler>,
    pub is_input: bool,
    pub on_input: Option<html_rs_core::InputHandler>,
    pub on_enter: Option<html_rs_core::EnterHandler>,
    pub on_key: Option<html_rs_core::KeyHandler>,
    /// Current `value` attribute (for `<input>`).
    pub value: String,
}

/// Everything one frame paints, plus interactive regions.
#[derive(Default)]
pub struct DrawList {
    pub quads: Vec<Quad>,
    pub texts: Vec<TextItem>,
    pub hits: Vec<HitTarget>,
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
}

/// A cached shaped-text buffer, keyed by everything that affects shaping.
#[derive(Clone)]
struct Shaped {
    content: String,
    font_size: f32,
    weight: u16,
    /// Wrap width the buffer was shaped with (physical px).
    wrap_w: f32,
    buffer: Rc<glyphon::Buffer>,
}

impl Shaped {
    fn matches(&self, content: &str, font_size: f32, weight: u16, wrap_w: f32) -> bool {
        self.content == content
            && self.font_size == font_size
            && self.weight == weight
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
    tree: TaffyTree<TextSpec>,
    prev: Vec<Item>,
    root_id: taffy::NodeId,
    scale: f32,
    width: f32,
    height: f32,
    primed: bool,
}

impl PaintState {
    pub fn new() -> Self {
        Self {
            tree: TaffyTree::<TextSpec>::new(),
            prev: Vec::new(),
            root_id: taffy::NodeId::from(0usize),
            scale: 1.0,
            width: 0.0,
            height: 0.0,
            primed: false,
        }
    }
}

impl Default for PaintState {
    fn default() -> Self {
        Self::new()
    }
}

fn spec_from_style(style: &StyleOut, content: String) -> TextSpec {
    TextSpec {
        content,
        font_size: style.font_size.unwrap_or(BASE_FONT_SIZE),
        weight: style.font_weight.unwrap_or(400),
    }
}

/// Builds a fresh taffy subtree + item list entries from an element (preorder).
fn build_node(
    el: &Element,
    path: &mut Vec<u32>,
    parent_idx: Option<usize>,
    items: &mut Vec<Item>,
    tree: &mut TaffyTree<TextSpec>,
    scale: f32,
) -> taffy::NodeId {
    let style = html_rs_core::parse_classes(&el.classes, scale);
    let my_idx = items.len();

    // Reserve the item slot now so children can point at it (preorder).
    items.push(Item {
        path: path.clone(),
        tag: el.tag,
        el: Some(el.clone()),
        text: None,
        style: style.clone(),
        taffy_id: taffy::NodeId::from(0usize),
        parent: parent_idx,
        is_text_leaf: false,
        shaped: None,
    });

    let taffy_id = if el.children.len() == 1 && matches!(el.children[0], Node::Text(_)) {
        // Single-text-child element: a measured leaf. Keep the parsed style so
        // padding/margin/sizing still apply; the measure callback supplies the
        // content size and taffy adds padding/border on top.
        let Node::Text(t) = &el.children[0] else { unreachable!() };
        let spec = spec_from_style(&style, t.content.clone());
        let id = tree
            .new_leaf_with_context(style.taffy.clone(), spec.clone())
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
                Node::Text(t) => build_text_child(t, &style, path, my_idx, items, tree),
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
    tree: &mut TaffyTree<TextSpec>,
) -> taffy::NodeId {
    let spec = spec_from_style(parent_style, t.content.clone());
    let id = tree
        .new_leaf_with_context(taffy::Style::default(), spec.clone())
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
    tree: &mut TaffyTree<TextSpec>,
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
    tree: &mut TaffyTree<TextSpec>,
    scale: f32,
) -> (taffy::NodeId, bool) {
    let p = &prev[prev_idx];
    let Some(pe) = p.el.as_ref() else {
        // Caller guarantees element positions only pair with element items.
        remove_subtree(prev, prev_kids, prev_idx, tree);
        let id = build_node(el, path, parent_idx, items, tree, scale);
        return (id, true);
    };
    let change = diff_element(pe, el);
    if change == ElementChange::Structure {
        remove_subtree(prev, prev_kids, prev_idx, tree);
        let id = build_node(el, path, parent_idx, items, tree, scale);
        return (id, true);
    }

    // Matched: reuse the taffy node and patch in place.
    let style = html_rs_core::parse_classes(&el.classes, scale);
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

    if change == ElementChange::InPlace && pe.classes != el.classes {
        tree.set_style(id, style.taffy.clone()).expect("taffy set_style");
    }

    if is_leaf {
        let Node::Text(t) = &el.children[0] else { unreachable!() };
        let spec = spec_from_style(&style, t.content.clone());
        if p.text.as_ref().map(|s| s.content != spec.content).unwrap_or(true) {
            // New content → replace the taffy node context (marks it dirty so
            // only this leaf re-measures).
            tree.set_node_context(id, Some(spec.clone())).expect("taffy set_node_context");
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
                        let spec = spec_from_style(&style, t.content.clone());
                        if old.text.as_ref().map(|s| s.content != spec.content).unwrap_or(true) {
                            tree.set_node_context(old.taffy_id, Some(spec.clone()))
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
                        build_text_child(t, &style, path, my_idx, items, tree)
                    }
                }
            }
        } else {
            match child {
                Node::Element(ce) => build_node(ce, path, Some(my_idx), items, tree, scale),
                Node::Text(t) => build_text_child(t, &style, path, my_idx, items, tree),
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
    focus_path: Option<&[u32]>,
    hovered_path: Option<&[u32]>,
) -> DrawList {
    let rebuild =
        !state.primed || state.scale != scale || state.width != width || state.height != height;
    let mut items: Vec<Item> = Vec::new();

    if rebuild {
        state.tree = TaffyTree::<TextSpec>::new();
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
            state.tree = TaffyTree::<TextSpec>::new();
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
            .map(|s| s.clone())
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
                // border/sizing around the measured text content size.
                taffy::compute_leaf_layout(
                    input,
                    style,
                    |_, _| 0.0,
                    |known_dimensions, available_space| {
                        let Some(spec) = ctx else { return Size::ZERO };
                        let wrap_w = known_dimensions.width.unwrap_or(match available_space.width {
                            AvailableSpace::Definite(w) => w,
                            _ => f32::INFINITY,
                        });
                        let (tw, th) = measure_text(font_system, spec, wrap_w);
                        Size { width: tw, height: th }
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

    let mut out = DrawList::default();
    // Shaped buffers live in a side vec during emission (the loop reads
    // `items` immutably for parent alignment); re-attached afterwards.
    let mut shaped_cache: Vec<Option<Shaped>> = items.iter_mut().map(|i| i.shaped.take()).collect();
    for (idx, item) in items.iter().enumerate() {
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
            if el.tag == "input" {
                paint_input(&mut out, &mut shaped_cache[idx], &item.style, el, x, y, w, h, focused, layout, font_system);
            } else if let Some(bg) = paint.bg {
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
        }

        // Text.
        if item.is_text_leaf {
            let Some(spec) = item.text.clone() else { continue };
            let color = paint.color.unwrap_or(DEFAULT_TEXT);
            let content_w = (w - layout.padding.left - layout.padding.right - layout.border.left - layout.border.right).max(1.0);
            let buffer = reuse_or_shape(&mut shaped_cache[idx], &spec.content, spec.font_size, spec.weight, content_w, font_system);
            let text_w = buffer_max_width(&buffer);

            // Alignment box: own content box, or the parent's for text children.
            let (tx0, ty) = (
                x + layout.border.left + layout.padding.left,
                y + layout.border.top + layout.padding.top,
            );
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
        }

        // Hit targets (deepest-last = topmost during hit testing).
        if let Some(el) = &el {
            let interactive = el.on_click.is_some() || el.tag == "input" || item.style.cursor_pointer;
            if interactive {
                out.hits.push(HitTarget {
                    rect: [x, y, w, h],
                    path: item.path.clone(),
                    tag: el.tag,
                    cursor_pointer: item.style.cursor_pointer || el.on_click.is_some(),
                    on_click: el.on_click.clone(),
                    is_input: el.tag == "input",
                    on_input: el.on_input.clone(),
                    on_enter: el.on_enter.clone(),
                    on_key: el.on_key.clone(),
                    value: el.get_attr("value").unwrap_or_default().to_string(),
                });
            }
        }
    }

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
    wrap_w: f32,
    font_system: &mut glyphon::FontSystem,
) -> Rc<glyphon::Buffer> {
    if let Some(sh) = slot {
        if sh.matches(content, font_size, weight, wrap_w) {
            return sh.buffer.clone();
        }
    }
    let buffer = Rc::new(shape_text(font_system, content, font_size, weight, Some(wrap_w)));
    *slot = Some(Shaped {
        content: content.to_string(),
        font_size,
        weight,
        wrap_w,
        buffer: buffer.clone(),
    });
    buffer
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
) {
    let paint = style.paint(false);
    // Box.
    out.quads.push(Quad {
        x,
        y,
        w,
        h,
        color: paint
            .bg
            .map(html_rs_core::srgb_to_linear)
            .unwrap_or([0.075, 0.09, 0.13, 1.0]), // slate-800-ish
        radius: paint.radius,
        border_width: if focused { 2.0 } else { paint.border_width },
        border_color: if focused {
            [0.38, 0.65, 0.98, 1.0] // blue-400 focus ring
        } else {
            paint
                .border_color
                .map(html_rs_core::srgb_to_linear)
                .unwrap_or([0.2, 0.24, 0.31, 1.0]) // slate-700-ish
        },
    });

    let value = el.get_attr("value").unwrap_or_default();
    let placeholder = el.get_attr("placeholder").unwrap_or_default();
    let font_size = style.font_size.unwrap_or(BASE_FONT_SIZE);
    let weight = style.font_weight.unwrap_or(400);
    let (content, color) = if value.is_empty() && !placeholder.is_empty() {
        (placeholder.to_string(), [0.6, 0.63, 0.69, 1.0]) // gray-400
    } else {
        (value.to_string(), paint.color.unwrap_or(DEFAULT_TEXT))
    };

    let content_w = (w - layout.padding.left - layout.padding.right - layout.border.left - layout.border.right).max(1.0);
    let buffer = reuse_or_shape(shaped, &content, font_size, weight, content_w, font_system);
    let text_w = buffer_max_width(&buffer);
    let line_h = font_size * 1.35;
    let tx = x + layout.border.left + layout.padding.left;
    let ty = y + (h - line_h) / 2.0;
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

    // Caret when focused.
    if focused {
        out.quads.push(Quad {
            x: tx + text_w + 1.0,
            y: y + (h - line_h * 0.8) / 2.0,
            w: 2.0,
            h: line_h * 0.8,
            color: [0.38, 0.65, 0.98, 1.0],
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
    let buffer = shape_text(font_system, &spec.content, spec.font_size, spec.weight, wrap);
    let mut height: f32 = 0.0;
    let mut max_w: f32 = 0.0;
    for run in buffer.layout_runs() {
        height += run.line_height;
        for glyph in run.glyphs.iter() {
            max_w = max_w.max(glyph.x + glyph.w as f32);
        }
    }
    if height == 0.0 {
        height = spec.font_size * 1.35;
    }
    (max_w.max(1.0), height)
}

/// Shapes text into a cosmic-text buffer (what glyphon renders).
fn shape_text(
    font_system: &mut glyphon::FontSystem,
    content: &str,
    font_size: f32,
    weight: u16,
    wrap_width: Option<f32>,
) -> glyphon::Buffer {
    let mut buffer = glyphon::Buffer::new(
        font_system,
        glyphon::Metrics::new(font_size, font_size * 1.35),
    );
    let attrs = glyphon::Attrs::new()
        .family(glyphon::Family::SansSerif)
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
            max_w = max_w.max(glyph.x + glyph.w as f32);
        }
    }
    max_w
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

    fn paint_once(root: &Element, w: f32, h: f32, scale: f32) -> DrawList {
        let mut state = PaintState::new();
        let mut font_system = glyphon::FontSystem::new();
        paint(&mut state, root, w, h, scale, &mut font_system, None, None)
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
        let _first = paint(&mut inc_state, &a, 1080.0, 720.0, 1.5, &mut font_system, None, None);
        let incremental = paint(&mut inc_state, &b, 1080.0, 720.0, 1.5, &mut font_system, None, None);
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
        let _ = paint(&mut inc_state, &a, 800.0, 600.0, 1.0, &mut font_system, None, None);
        let incremental = paint(&mut inc_state, &b, 800.0, 600.0, 1.0, &mut font_system, None, None);
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
        let _ = paint(&mut inc_state, &a, 800.0, 600.0, 1.0, &mut font_system, None, None);
        let incremental = paint(&mut inc_state, &b, 800.0, 600.0, 1.0, &mut font_system, None, None);
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
        let first = paint(&mut state, &root, 800.0, 600.0, 1.0, &mut font_system, None, None);
        let second = paint(&mut state, &root, 800.0, 600.0, 1.0, &mut font_system, None, None);

        assert_eq!(first.quads.len(), second.quads.len());
        for (q, f) in first.quads.iter().zip(&second.quads) {
            assert_eq!((q.x, q.y, q.w, q.h, q.color), (f.x, f.y, f.w, f.h, f.color));
        }
        // Shaped buffers are literally shared (cache hit), not re-shaped.
        for (t, f) in first.texts.iter().zip(&second.texts) {
            assert!(Rc::ptr_eq(&t.buffer, &f.buffer), "shaped buffer should be reused");
        }
    }
}
