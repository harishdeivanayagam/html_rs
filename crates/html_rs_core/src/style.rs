//! The **Tailwind class compiler**: turns class strings like
//! `"flex flex-col items-center gap-4 bg-slate-800 rounded-xl"` into
//! a [`taffy::Style`] (layout) plus paint properties (colors, radii, text).
//!
//! Supported subset (see `.cursor/rules/html-rs-tailwind-subset.mdc`):
//! layout: flex, flex-row/col, flex-1, items-*, justify-*, gap-*, w/h-*, p/m-*, overflow-hidden
//! visual: bg-*, border[-N][-color], rounded[-sm/md/lg/xl/2xl/3xl/full/-N]
//! text:   text-{color|size}, font-{weight}, underline, line-through, text-{left,center,right}, opacity-*

use crate::colors::resolve_color;
use taffy::geometry::Point;
use taffy::prelude::*;
use taffy::style::{
    GridAutoFlow, GridPlacement, GridTemplateComponent,
    GridTemplateRepetition, MaxTrackSizingFunction, MinTrackSizingFunction, Overflow,
    RepetitionCount, TrackSizingFunction,
};
use taffy::style_helpers::{TaffyGridLine, TaffyGridSpan};

/// sRGB color as straight-alpha floats in `0..=1`.
pub type Rgba = [f32; 4];

/// Default window clear color (slate-900).
pub const DEFAULT_BG: Rgba = [
    0x0f as f32 / 255.0,
    0x17 as f32 / 255.0,
    0x2a as f32 / 255.0,
    1.0,
];
/// Default text color (slate-100).
pub const DEFAULT_TEXT: Rgba = [
    0xf1 as f32 / 255.0,
    0xf5 as f32 / 255.0,
    0xf9 as f32 / 255.0,
    1.0,
];
/// Default font size in CSS px (before scale factor).
pub const BASE_FONT_SIZE: f32 = 16.0;

/// Text decoration lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoration {
    Underline,
    LineThrough,
}

/// Text alignment inside the element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// The compiled result of a class list: layout + paint.
#[derive(Clone)]
pub struct StyleOut {
    /// Layout style for the taffy tree.
    pub taffy: taffy::Style,
    /// Background fill (sRGB).
    pub bg: Option<Rgba>,
    /// Text color (sRGB).
    pub color: Option<Rgba>,
    /// Font size in px (logical).
    pub font_size: Option<f32>,
    /// Font weight (400 = normal).
    pub font_weight: Option<u16>,
    /// Corner radius in px (logical).
    pub radius: Option<f32>,
    /// Border width in px (logical).
    pub border_width: Option<f32>,
    /// Border color (sRGB).
    pub border_color: Option<Rgba>,
    /// Text decoration.
    pub decoration: Option<Decoration>,
    /// Element opacity `0..=1`.
    pub opacity: Option<f32>,
    /// Text alignment.
    pub text_align: Option<TextAlign>,
    /// Shows a pointer cursor on hover.
    pub cursor_pointer: bool,
    /// Renders text in a monospace font (`font-mono`, `<kbd>`, `<code>`).
    pub mono: bool,
    /// Classes prefixed `hover:` — applied while the element is hovered.
    pub hover: Vec<String>,
}

impl StyleOut {
    pub fn new() -> Self {
        Self {
            taffy: taffy::Style::default(),
            bg: None,
            color: None,
            font_size: None,
            font_weight: None,
            radius: None,
            border_width: None,
            border_color: None,
            decoration: None,
            opacity: None,
            text_align: None,
            cursor_pointer: false,
            mono: false,
            hover: Vec::new(),
        }
    }

    /// Returns the resolved paint for this element, layering `hover` classes on
    /// top of the base classes when `hovered` is true.
    pub fn paint(&self, hovered: bool) -> Paint {
        let mut p = Paint {
            bg: self.bg,
            color: self.color,
            radius: self.radius.unwrap_or(0.0),
            border_width: self.border_width.unwrap_or(0.0),
            border_color: self.border_color,
            decoration: self.decoration,
            opacity: self.opacity.unwrap_or(1.0),
        };
        if hovered && !self.hover.is_empty() {
            let hov = parse_classes(&self.hover, 1.0);
            if hov.bg.is_some() {
                p.bg = hov.bg;
            }
            if hov.color.is_some() {
                p.color = hov.color;
            }
            if let Some(r) = hov.radius {
                p.radius = r;
            }
            if let Some(bw) = hov.border_width {
                p.border_width = bw;
            }
            if hov.border_color.is_some() {
                p.border_color = hov.border_color;
            }
            if let Some(o) = hov.opacity {
                p.opacity = o;
            }
        }
        p
    }
}

impl Default for StyleOut {
    fn default() -> Self {
        Self::new()
    }
}

/// The paint-relevant subset of a compiled style, after hover resolution.
#[derive(Clone, Copy, Debug)]
pub struct Paint {
    pub bg: Option<Rgba>,
    pub color: Option<Rgba>,
    pub radius: f32,
    pub border_width: f32,
    pub border_color: Option<Rgba>,
    pub decoration: Option<Decoration>,
    pub opacity: f32,
}

/// Compiles a list of class strings into a [`StyleOut`].
///
/// `scale` is the window scale factor: all px-ish values are multiplied by it
/// so layout runs in physical pixels.
pub fn parse_classes(classes: &[String], scale: f32) -> StyleOut {
    let mut out = StyleOut::new();
    for class in classes {
        for token in class.split_whitespace() {
            if let Some(h) = token.strip_prefix("hover:") {
                if !h.is_empty() {
                    out.hover.push(h.to_string());
                }
                continue;
            }
            apply(&mut out, token, scale);
        }
    }
    out
}

/// Browser-style default classes for semantic tags, applied **before** the
/// element's own classes (user classes win — later assignments overwrite).
///
/// These give the semantic tags their expected look with zero user styling:
/// lists stack vertically, links underline, `<kbd>`/`<code>` go monospace,
/// table rows/cells line up, `<select>` looks like a control, etc.
pub fn tag_default_classes(tag: &str) -> &'static [&'static str] {
    match tag {
        // Lists: vertical stack with left padding for the bullet gutter.
        "ul" | "ol" => &["flex-col", "gap-1", "pl-6", "my-2", "w-full"],
        // Links: classic link look + pointer cursor. (`href` is accepted but
        // not navigated — there is no web layer.)
        "a" => &["underline", "text-blue-400", "cursor-pointer"],
        // Images: placeholder look until (or instead of) a decoded texture.
        "img" => &["bg-slate-800", "rounded-lg", "shrink-0"],
        // Select: control-like chrome; the painter overlays the selected
        // option's text and a chevron. `option` children are hidden.
        "select" => &[
            "flex", "items-center", "border", "border-slate-700", "rounded-xl",
            "bg-slate-800", "px-4", "py-3", "cursor-pointer", "shrink-0",
        ],
        // Dialog: floating panel. Centered by the painter via absolute
        // positioning + auto margins; hidden unless the `open` attr is set.
        "dialog" => &[
            "flex-col", "gap-4", "bg-slate-800", "border", "border-slate-700",
            "rounded-2xl", "p-8",
        ],
        // Table: rows stack, cells flex equally so columns line up.
        "table" => &["flex-col", "w-full"],
        "thead" | "tbody" | "tfoot" => &["flex-col", "w-full"],
        "tr" => &["flex-row", "w-full"],
        "td" => &["flex-1", "px-3", "py-2"],
        "th" => &["flex-1", "px-3", "py-2", "font-bold", "text-left"],
        // Semantic text blocks.
        "blockquote" => &["pl-4", "my-2", "text-slate-400"],
        "nav" => &["flex-row", "items-center", "gap-4", "w-full"],
        "kbd" => &[
            "font-mono", "border", "border-slate-600", "rounded-md",
            "px-2", "py-0.5", "bg-slate-900", "text-sm",
        ],
        "code" => &["font-mono", "bg-slate-800", "rounded-md", "px-1.5", "py-0.5"],
        _ => &[],
    }
}

fn px(n: f32, scale: f32) -> LengthPercentage {
    // Tailwind spacing: `1` unit = 0.25rem = 4 CSS px, times the device scale.
    LengthPercentage::length(n * 4.0 * scale)
}

fn parse_len(suffix: &str, scale: f32) -> Option<LengthPercentage> {
    let n = parse_num(suffix)?;
    Some(px(n, scale))
}

/// Tailwind spacing: `1` = 0.25rem = 4 CSS px.
fn parse_num(s: &str) -> Option<f32> {
    if s == "px" {
        return Some(1.0);
    }
    s.parse::<f32>().ok()
}

fn apply(out: &mut StyleOut, class: &str, scale: f32) {
    // Ignore silently with a warning the first time? Silent for release builds.
    match class {
        // ----- display / flex -----
        "flex" => out.taffy.display = Display::Flex,
        "block" => out.taffy.display = Display::Block,
        "grid" => out.taffy.display = Display::Grid,
        "hidden" => out.taffy.display = Display::None,
        "flex-row" => out.taffy.flex_direction = FlexDirection::Row,
        "flex-col" => out.taffy.flex_direction = FlexDirection::Column,
        "flex-wrap" => out.taffy.flex_wrap = FlexWrap::Wrap,
        "flex-nowrap" => out.taffy.flex_wrap = FlexWrap::NoWrap,
        "flex-1" => {
            out.taffy.flex_grow = 1.0;
            out.taffy.flex_shrink = 1.0;
            out.taffy.flex_basis = Dimension::ZERO;
        }
        "flex-grow" => out.taffy.flex_grow = 1.0,
        "shrink-0" | "flex-shrink-0" => out.taffy.flex_shrink = 0.0,
        "items-start" => out.taffy.align_items = Some(AlignItems::START),
        "items-center" => out.taffy.align_items = Some(AlignItems::CENTER),
        "items-end" => out.taffy.align_items = Some(AlignItems::END),
        "items-stretch" => out.taffy.align_items = Some(AlignItems::STRETCH),
        "justify-start" => out.taffy.justify_content = Some(JustifyContent::START),
        "justify-center" => out.taffy.justify_content = Some(JustifyContent::CENTER),
        "justify-end" => out.taffy.justify_content = Some(JustifyContent::END),
        "justify-between" => out.taffy.justify_content = Some(JustifyContent::SPACE_BETWEEN),
        "justify-around" => out.taffy.justify_content = Some(JustifyContent::SPACE_AROUND),
        "justify-evenly" => out.taffy.justify_content = Some(JustifyContent::SPACE_EVENLY),
        "overflow-hidden" => {
            out.taffy.overflow = Point { x: Overflow::Hidden, y: Overflow::Hidden }
        }
        "cursor-pointer" => out.cursor_pointer = true,
        "underline" => out.decoration = Some(Decoration::Underline),
        "line-through" => out.decoration = Some(Decoration::LineThrough),
        "text-left" => out.text_align = Some(TextAlign::Left),
        "text-center" => out.text_align = Some(TextAlign::Center),
        "text-right" => out.text_align = Some(TextAlign::Right),
        "font-mono" => out.mono = true,
        "border" => out.border_width = Some(1.0 * scale),
        "font-thin" => out.font_weight = Some(100),
        "font-extralight" => out.font_weight = Some(200),
        "font-light" => out.font_weight = Some(300),
        "font-normal" => out.font_weight = Some(400),
        "font-medium" => out.font_weight = Some(500),
        "font-semibold" => out.font_weight = Some(600),
        "font-bold" => out.font_weight = Some(700),
        "font-extrabold" => out.font_weight = Some(800),
        "font-black" => out.font_weight = Some(900),
        _ => {}
    }

    // ----- gap-* -----
    if let Some(rest) = class.strip_prefix("gap-") {
        if let Some((v, axis)) = rest.strip_prefix("x-").map(|v| (v, 0)).or_else(|| rest.strip_prefix("y-").map(|v| (v, 1))) {
            if let Some(len) = parse_len(v, scale) {
                if axis == 0 {
                    out.taffy.gap.width = len;
                } else {
                    out.taffy.gap.height = len;
                }
            }
            return;
        }
        if let Some(len) = parse_len(rest, scale) {
            out.taffy.gap = Size { width: len, height: len };
        }
        return;
    }

    // ----- CSS Grid: template tracks, spans, lines, flow -----
    if let Some(rest) = class.strip_prefix("grid-cols-") {
        if let Some(n) = parse_num(rest) {
            out.taffy.grid_template_columns = fr_tracks(n);
            return;
        }
    }
    if let Some(rest) = class.strip_prefix("grid-rows-") {
        if let Some(n) = parse_num(rest) {
            out.taffy.grid_template_rows = fr_tracks(n);
            return;
        }
    }
    if let Some(rest) = class.strip_prefix("grid-flow-") {
        out.taffy.grid_auto_flow = match rest {
            "row" => GridAutoFlow::Row,
            "col" => GridAutoFlow::Column,
            "row-dense" => GridAutoFlow::RowDense,
            "col-dense" => GridAutoFlow::ColumnDense,
            _ => return,
        };
        return;
    }
    if let Some(rest) = class.strip_prefix("auto-rows-") {
        if let Some(len) = parse_len(rest, scale) {
            // Implicit rows of fixed height.
            out.taffy.grid_auto_rows = vec![TrackSizingFunction::from(len)];
            return;
        }
    }
    if let Some(rest) = class.strip_prefix("col-span-") {
        if let Some(n) = parse_num(rest) {
            out.taffy.grid_column = span_placement(n);
            return;
        }
    }
    if let Some(rest) = class.strip_prefix("row-span-") {
        if let Some(n) = parse_num(rest) {
            out.taffy.grid_row = span_placement(n);
            return;
        }
    }
    if let Some(rest) = class.strip_prefix("col-start-") {
        if let Some(n) = parse_num(rest) {
            out.taffy.grid_column = line_placement(n);
            return;
        }
    }
    if let Some(rest) = class.strip_prefix("row-start-") {
        if let Some(n) = parse_num(rest) {
            out.taffy.grid_row = line_placement(n);
            return;
        }
    }

    // ----- w-*/h-* -----
    if let Some(rest) = class.strip_prefix("w-") {
        if let Some(d) = parse_dimension(rest, scale) {
            out.taffy.size.width = d;
            return;
        }
    }
    if let Some(rest) = class.strip_prefix("h-") {
        if let Some(d) = parse_dimension(rest, scale) {
            out.taffy.size.height = d;
            return;
        }
    }
    if let Some(rest) = class.strip_prefix("min-w-") {
        if rest == "0" {
            out.taffy.min_size.width = LengthPercentageAuto::ZERO;
            return;
        }
    }
    if let Some(rest) = class.strip_prefix("min-h-") {
        if rest == "0" {
            out.taffy.min_size.height = LengthPercentageAuto::ZERO;
            return;
        }
    }

    // ----- padding / margin -----
    if let Some(rest) = class.strip_prefix('p') {
        if let Some((side, val)) = split_side(rest) {
            if let Some(len) = parse_len(val, scale) {
                apply_side(&mut out.taffy.padding, side, len);
                return;
            }
        }
    }
    if let Some(rest) = class.strip_prefix('m') {
        if let Some((side, val)) = split_side(rest) {
            if val == "auto" {
                apply_side_auto(&mut out.taffy.margin, side, LengthPercentageAuto::auto());
                return;
            }
            if let Some(n) = parse_num(val) {
                let len = LengthPercentageAuto::length(n * scale);
                apply_side_auto(&mut out.taffy.margin, side, len);
                return;
            }
        }
        if rest == "auto" {
            out.taffy.margin = Rect::auto();
            return;
        }
    }

    // ----- rounded-* -----
    if let Some(rest) = class.strip_prefix("rounded") {
        let r = match rest {
            "" => 4.0,
            "-sm" => 2.0,
            "-md" => 6.0,
            "-lg" => 8.0,
            "-xl" => 12.0,
            "-2xl" => 16.0,
            "-3xl" => 24.0,
            "-full" => 9999.0,
            _ => {
                if let Some(n) = rest.strip_prefix('-').and_then(parse_num) {
                    n
                } else {
                    return;
                }
            }
        };
        out.radius = Some(r * scale);
        return;
    }

    // ----- border-* -----
    if let Some(rest) = class.strip_prefix("border-") {
        if let Some(n) = parse_num(rest) {
            out.border_width = Some(n * scale);
            return;
        }
        if let Some(c) = resolve_color(rest) {
            out.border_color = Some(c);
            return;
        }
    }

    // ----- text-* (color first, then size) -----
    if let Some(rest) = class.strip_prefix("text-") {
        if let Some(c) = resolve_color(rest) {
            out.color = Some(c);
            return;
        }
        let size = match rest {
            "xs" => 12.0,
            "sm" => 14.0,
            "base" => 16.0,
            "lg" => 18.0,
            "xl" => 20.0,
            "2xl" => 24.0,
            "3xl" => 30.0,
            "4xl" => 36.0,
            "5xl" => 48.0,
            "6xl" => 60.0,
            "7xl" => 72.0,
            _ => return,
        };
        out.font_size = Some(size * scale);
        return;
    }

    // ----- bg-* -----
    if let Some(rest) = class.strip_prefix("bg-") {
        if let Some(c) = resolve_color(rest) {
            out.bg = Some(c);
            return;
        }
    }

    // ----- opacity-* -----
    if let Some(rest) = class.strip_prefix("opacity-") {
        if let Some(n) = parse_num(rest) {
            out.opacity = Some((n / 100.0).clamp(0.0, 1.0));
        }
    }
}

/// Splits `"-x-4"` → `('x', "4")`, `"-t-2"` → `('t', "2")`, `"-4"` → `(' ', "4")`.
/// `rest` is the class after the leading `p`/`m` tag, e.g. "x-2", "-8", "y-auto".
/// A side char followed by `-` is that side; otherwise the value applies to all sides.
fn split_side(rest: &str) -> Option<(char, &str)> {
    match rest.chars().next() {
        Some(c @ ('x' | 'y' | 't' | 'b' | 'l' | 'r')) if rest[1..].starts_with('-') => {
            Some((c, &rest[2..]))
        }
        Some('-') => Some((' ', &rest[1..])),
        _ => None,
    }
}

fn apply_side(rect: &mut Rect<LengthPercentage>, side: char, len: LengthPercentage) {
    match side {
        ' ' => *rect = Rect { left: len, right: len, top: len, bottom: len },
        'x' => {
            rect.left = len;
            rect.right = len;
        }
        'y' => {
            rect.top = len;
            rect.bottom = len;
        }
        't' => rect.top = len,
        'b' => rect.bottom = len,
        'l' => rect.left = len,
        'r' => rect.right = len,
        _ => {}
    }
}

fn apply_side_auto(rect: &mut Rect<LengthPercentageAuto>, side: char, len: LengthPercentageAuto) {
    match side {
        ' ' => *rect = Rect { left: len, right: len, top: len, bottom: len },
        'x' => {
            rect.left = len;
            rect.right = len;
        }
        'y' => {
            rect.top = len;
            rect.bottom = len;
        }
        't' => rect.top = len,
        'b' => rect.bottom = len,
        'l' => rect.left = len,
        'r' => rect.right = len,
        _ => {}
    }
}

// ----- CSS Grid helpers -----

/// `grid-cols-N` / `grid-rows-N` → Tailwind's `repeat(N, minmax(0, 1fr))`:
/// N equal tracks that never grow past their share (min 0).
fn fr_tracks<S: taffy::style::CheapCloneStr>(n: f32) -> Vec<GridTemplateComponent<S>> {
    let track = TrackSizingFunction {
        min: MinTrackSizingFunction::ZERO,
        max: MaxTrackSizingFunction::from_fr(1.0),
    };
    vec![GridTemplateComponent::Repeat(GridTemplateRepetition {
        count: RepetitionCount::Count(n.max(1.0) as u16),
        tracks: vec![track],
        line_names: Vec::new(),
    })]
}

/// `col-span-N` / `row-span-N` → span N tracks from auto-placement.
fn span_placement(n: f32) -> taffy::geometry::Line<GridPlacement> {
    Line::<GridPlacement>::from_span(n.max(1.0) as u16)
}

/// `col-start-N` / `row-start-N` → place at grid line N (1-based).
fn line_placement(n: f32) -> taffy::geometry::Line<GridPlacement> {
    Line::<GridPlacement>::from_line_index(n.max(1.0) as i16)
}

fn parse_dimension(rest: &str, scale: f32) -> Option<Dimension> {
    match rest {
        "full" => return Some(Dimension::percent(1.0)),
        "auto" => return Some(Dimension::auto()),
        "screen" => return None, // needs window size; not supported
        _ => {}
    }
    parse_num(rest).map(|n| Dimension::length(n * scale))
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::style::ExpandedMaxTrackSizingFunction;

    #[test]
    fn compiles_layout() {
        let out = parse_classes(&["flex flex-col items-center gap-4 p-8 w-full".into()], 1.0);
        assert_eq!(out.taffy.display, Display::Flex);
        assert_eq!(out.taffy.flex_direction, FlexDirection::Column);
        assert_eq!(out.taffy.align_items, Some(AlignItems::CENTER));
        assert_eq!(out.taffy.gap.height, LengthPercentage::length(16.0));
        assert_eq!(out.taffy.padding.top, LengthPercentage::length(32.0));
        assert_eq!(out.taffy.size.width, Dimension::percent(1.0));
    }

    #[test]
    fn compiles_paint() {
        let out = parse_classes(
            &["bg-blue-600 text-white rounded-xl border border-blue-400 hover:bg-blue-500".into()],
            1.0,
        );
        assert_eq!(out.bg.unwrap()[2], 0xeb as f32 / 255.0); // blue-600 #2563eb
        assert_eq!(out.color.unwrap(), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(out.radius, Some(12.0));
        assert_eq!(out.border_width, Some(1.0));
        assert_eq!(out.hover, vec!["bg-blue-500".to_string()]);
        let p = out.paint(false);
        assert_eq!(p.bg.unwrap()[0], 0x25 as f32 / 255.0);
        let ph = out.paint(true);
        assert_eq!(ph.bg.unwrap()[0], 0x3b as f32 / 255.0); // blue-500
    }

    #[test]
    fn sides() {
        let out = parse_classes(&["px-2 pt-3 mx-auto".into()], 1.0);
        assert_eq!(out.taffy.padding.left, LengthPercentage::length(8.0));
        assert_eq!(out.taffy.padding.top, LengthPercentage::length(12.0));
        assert_eq!(out.taffy.margin.left, LengthPercentageAuto::auto());
        assert_eq!(out.taffy.padding.right, LengthPercentage::length(8.0));
    }

    #[test]
    fn grid_classes() {
        let out = parse_classes(
            &["grid grid-cols-3 grid-rows-2 col-span-2 row-start-1 gap-2".into()],
            1.0,
        );
        assert_eq!(out.taffy.display, Display::Grid);
        // grid-cols-3 → repeat(3, minmax(0, 1fr))
        let cols = &out.taffy.grid_template_columns;
        assert_eq!(cols.len(), 1, "one repeat() component");
        match &cols[0] {
            GridTemplateComponent::Repeat(rep) => {
                assert_eq!(rep.count, RepetitionCount::Count(3));
                assert_eq!(rep.tracks.len(), 1);
                let track = &rep.tracks[0];
                assert_eq!(track.min, MinTrackSizingFunction::ZERO);
                assert!(matches!(track.max.expand(), ExpandedMaxTrackSizingFunction::Fr(1.0)));
            }
            other => panic!("expected repeat, got {other:?}"),
        }
        // grid-rows-2
        match &out.taffy.grid_template_rows[0] {
            GridTemplateComponent::Repeat(rep) => {
                assert_eq!(rep.count, RepetitionCount::Count(2));
            }
            other => panic!("expected repeat, got {other:?}"),
        }
        // col-span-2 → span 2 tracks
        assert_eq!(out.taffy.grid_column.start, GridPlacement::Span(2));
        assert_eq!(out.taffy.grid_column.end, GridPlacement::Auto);
        // row-start-1 → place at line 1
        assert_eq!(out.taffy.grid_row.start, GridPlacement::from_line_index(1));
        assert_eq!(out.taffy.gap.width, LengthPercentage::length(8.0));
    }
}
