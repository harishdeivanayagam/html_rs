//! Element tree + event types. This is the data model the `ui!` macro builds
//! and the renderer paints.

mod colors;
mod element;
mod style;

pub use colors::{from_hex, srgb_to_linear};
pub use element::{
    child_of, click, enter, input, key, text, Child, ClickHandler, Element, EnterHandler,
    InputHandler, KeyHandler, KeyEventInfo, Node, PointerEvent, MouseButton, TextNode,
};
pub use style::{parse_classes, Decoration, StyleOut, TextAlign, BASE_FONT_SIZE, DEFAULT_BG, DEFAULT_TEXT};
