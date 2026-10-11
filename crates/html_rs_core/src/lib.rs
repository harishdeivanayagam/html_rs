//! Element tree + event types. This is the data model the `ui!` macro builds
//! and the renderer paints.

mod colors;
mod diff;
mod element;
mod style;
mod theme;

pub use colors::{from_hex, srgb_to_linear};
pub use diff::{diff_element, equivalent, ElementChange};
pub use element::{
    change, child_of, click, enter, input, key, text, ChangeHandler, Child, ClickHandler, Element,
    EnterHandler, InputHandler, KeyHandler, KeyEventInfo, Node, PointerEvent, MouseButton, TextNode,
};
pub use style::{parse_classes, tag_default_classes, Decoration, Paint, StyleOut, TextAlign, BASE_FONT_SIZE, DEFAULT_BG, DEFAULT_TEXT};
pub use theme::{current as current_theme, resolve_token_color, set_mode as set_theme_mode, Theme, ThemeMode};
