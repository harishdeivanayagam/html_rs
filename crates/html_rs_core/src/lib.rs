//! Element tree + event types. This is the data model the `ui!` macro builds
//! and the renderer paints.

mod colors;
mod diff;
mod element;
mod style;

pub use colors::{from_hex, srgb_to_linear};
pub use diff::{diff_element, equivalent, ElementChange};
pub use element::{
    child_of, click, enter, input, key, text, Child, ClickHandler, Element, EnterHandler,
    InputHandler, KeyHandler, KeyEventInfo, Node, PointerEvent, MouseButton, TextNode,
};
pub use style::{parse_classes, tag_default_classes, Decoration, Paint, StyleOut, TextAlign, BASE_FONT_SIZE, DEFAULT_BG, DEFAULT_TEXT};
