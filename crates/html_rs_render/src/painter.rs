//! The layout + paint pass: converts an [`Element`](html_rs_core::Element) tree
//! into a flat [`DrawList`] (quads, shaped text, hit targets) using taffy for
//! flexbox layout and cosmic-text for shaping/measuring.

use html_rs_core::{Element, Node, StyleOut, TextAlign, BASE_FONT_SIZE, DEFAULT_TEXT};
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

/// A shaped text run ready for glyphon.
pub struct TextItem {
    pub buffer: glyphon::Buffer,
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

struct Item {
    path: Vec<u32>,
    #[allow(dead_code)] // kept for debugging hit-test paths
    tag: &'static str,
    /// `None` for text leaf nodes.
    el: Option<Element>,
    text: Option<TextSpec>,
    /// For text children: the *parent* element's style (color/size/decoration inherit).
    style: StyleOut,
    taffy_id: taffy::NodeId,
    parent: Option<usize>,
    is_text_leaf: bool,
}

fn spec_from_style(style: &StyleOut, content: String) -> TextSpec {
    TextSpec {
        content,
        font_size: style.font_size.unwrap_or(BASE_FONT_SIZE),
        weight: style.font_weight.unwrap_or(400),
    }
}

/// Builds the taffy tree + flat item list from an element tree (preorder).
fn build_tree(
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
            match child {
                Node::Element(child_el) => {
                    path.push(i as u32);
                    let id = build_tree(child_el, path, Some(my_idx), items, tree, scale);
                    path.pop();
                    child_ids.push(id);
                }
                Node::Text(t) => {
                    let spec = spec_from_style(&style, t.content.clone());
                    let id = tree
                        .new_leaf_with_context(taffy::Style::default(), spec.clone())
                        .expect("taffy new_leaf");
                    child_ids.push(id);
                    items.push(Item {
                        path: {
                            let mut p = path.clone();
                            p.push(i as u32);
                            p
                        },
                        tag: "text",
                        el: None,
                        text: Some(spec),
                        style: style.clone(),
                        taffy_id: id,
                        parent: Some(my_idx),
                        is_text_leaf: true,
                    });
                }
            }
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

/// Lays out and paints an element tree into a [`DrawList`].
///
/// `width`/`height` are the window size in **physical** px; `scale` scales all
/// CSS-px values (class numbers).
pub fn paint(
    root: &Element,
    width: f32,
    height: f32,
    scale: f32,
    font_system: &mut glyphon::FontSystem,
    focus_path: Option<&[u32]>,
    hovered_path: Option<&[u32]>,
) -> DrawList {
    let mut tree = TaffyTree::<TextSpec>::new();
    let mut items: Vec<Item> = Vec::new();
    let root_id = build_tree(root, &mut Vec::new(), None, &mut items, &mut tree, scale);

    // The root fills the window unless it declares its own size.
    {
        let mut root_style = match tree.style(root_id) {
            Ok(s) => s.clone(),
            Err(_) => taffy::Style::default(),
        };
        if root_style.size.width == Dimension::AUTO {
            root_style.size.width = Dimension::percent(1.0);
        }
        if root_style.size.height == Dimension::AUTO {
            root_style.size.height = Dimension::percent(1.0);
        }
        tree.set_style(root_id, root_style).ok();
    }

    tree.compute_layout_with_measure(
        root_id,
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
        let loc = tree.layout(item.taffy_id).map(|l| l.location).unwrap_or_default();
        abs.push(match item.parent {
            Some(p) => {
                let pa = abs[p];
                Point { x: pa.x + loc.x, y: pa.y + loc.y }
            }
            None => Point { x: loc.x, y: loc.y },
        });
    }

    let mut out = DrawList::default();
    for (idx, item) in items.iter().enumerate() {
        let Ok(layout) = tree.layout(item.taffy_id) else { continue };
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

        if let Some(el) = &item.el {
            if el.tag == "input" {
                paint_input(&mut out, item, el, x, y, w, h, focused, layout, font_system);
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
            let Some(spec) = &item.text else { continue };
            let color = paint.color.unwrap_or(DEFAULT_TEXT);
            let content_w = (w - layout.padding.left - layout.padding.right - layout.border.left - layout.border.right).max(1.0);
            let buffer = shape_text(
                font_system,
                &spec.content,
                spec.font_size,
                spec.weight,
                Some(content_w),
            );
            let text_w = buffer_max_width(&buffer);

            // Alignment box: own content box, or the parent's for text children.
            let (tx0, ty) = (
                x + layout.border.left + layout.padding.left,
                y + layout.border.top + layout.padding.top,
            );
            let align = item.style.text_align.unwrap_or(TextAlign::Left);
            let (box_x, box_w) = match item.parent {
                Some(pi) if !matches!(align, TextAlign::Left) => match items.get(pi) {
                    Some(parent) => match tree.layout(parent.taffy_id) {
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
        if let Some(el) = &item.el {
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
    out
}

fn paint_input(
    out: &mut DrawList,
    item: &Item,
    el: &Element,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    focused: bool,
    layout: &taffy::Layout,
    font_system: &mut glyphon::FontSystem,
) {
    let paint = item.style.paint(false);
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
    let font_size = item.style.font_size.unwrap_or(BASE_FONT_SIZE);
    let weight = item.style.font_weight.unwrap_or(400);
    let (content, color) = if value.is_empty() && !placeholder.is_empty() {
        (placeholder.to_string(), [0.6, 0.63, 0.69, 1.0]) // gray-400
    } else {
        (value.to_string(), paint.color.unwrap_or(DEFAULT_TEXT))
    };

    let content_w = (w - layout.padding.left - layout.padding.right - layout.border.left - layout.border.right).max(1.0);
    let buffer = shape_text(font_system, &content, font_size, weight, Some(content_w));
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

    /// Regression test for the hello-world layout: all card children must land
    /// *inside* the card's rect (this caught taffy's parent-relative `location`
    /// being used as if it were absolute).
    #[test]
    fn hello_world_children_stay_inside_card() {
        let root = Element::new("div")
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
                            .child(Element::new("p").class("px-3 py-1 bg-slate-900 rounded-md text-sm text-emerald-400").text("signals"))
                            .child(Element::new("p").class("px-3 py-1 bg-slate-900 rounded-md text-sm text-amber-400").text("tailwind")),
                    ),
            );

        let mut font_system = glyphon::FontSystem::new();
        let list = paint(&root, 1080.0, 720.0, 1.5, &mut font_system, None, None);

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

        let mut font_system = glyphon::FontSystem::new();
        let list = paint(&root, 1000.0, 500.0, 1.0, &mut font_system, None, None);

        // 4 text quads (transparent bg → no quad for children; use texts).
        assert_eq!(list.texts.len(), 4, "four text children");
        let xs: Vec<f32> = list.texts.iter().map(|t| t.x).collect();
        // First two share a column x, last two share another (right) column.
        assert!(xs[0] < xs[1], "b is in the second column");
        assert_eq!(xs[0], xs[2], "a and c share column 1");
        assert_eq!(xs[1], xs[3], "b and d share column 2");
        assert!(xs[1] > xs[0] + 100.0, "columns should be far apart");
    }
}
