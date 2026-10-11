//! The element tree — what `ui! { ... }` produces and the renderer consumes.

use std::fmt::Display;
use std::rc::Rc;

/// Which mouse button was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other,
}

/// Pointer event passed to `onclick` handlers. Coordinates are in logical pixels.
#[derive(Clone, Copy, Debug)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    pub button: MouseButton,
}

/// Keyboard event passed to `onkeydown` handlers.
#[derive(Clone, Debug)]
pub struct KeyEventInfo {
    /// Winit-style key name: `"Enter"`, `"Escape"`, `"a"`, `" "`, ...
    pub key: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

pub type ClickHandler = Rc<dyn Fn(&PointerEvent)>;
pub type InputHandler = Rc<dyn Fn(&str)>;
pub type EnterHandler = Rc<dyn Fn(&str)>;
pub type KeyHandler = Rc<dyn Fn(&KeyEventInfo)>;
/// Change callback (`onchange` on `<select>`), receiving the new value.
pub type ChangeHandler = Rc<dyn Fn(&str)>;

/// Wraps a user closure into a click handler. Used by the `ui!` macro.
pub fn click(f: impl Fn(&PointerEvent) + 'static) -> ClickHandler {
    Rc::new(f)
}
/// Wraps a user closure into an input handler (`oninput`), receiving the new text.
pub fn input(f: impl Fn(&str) + 'static) -> InputHandler {
    Rc::new(f)
}
/// Wraps a user closure into an enter handler (`onenter`), receiving the current text.
pub fn enter(f: impl Fn(&str) + 'static) -> EnterHandler {
    Rc::new(f)
}
/// Wraps a user closure into a key handler (`onkeydown`).
pub fn key(f: impl Fn(&KeyEventInfo) + 'static) -> KeyHandler {
    Rc::new(f)
}
/// Wraps a user closure into a change handler (`onchange` on `<select>`).
pub fn change(f: impl Fn(&str) + 'static) -> ChangeHandler {
    Rc::new(f)
}

/// A run of text. Styling (color, size, weight, decoration) is inherited from
/// the parent element's classes.
#[derive(Clone)]
pub struct TextNode {
    pub content: String,
}

impl TextNode {
    pub fn new(content: impl Into<String>) -> Self {
        Self { content: content.into() }
    }
}

impl Display for TextNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.content)
    }
}

impl From<&str> for TextNode {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for TextNode {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

/// A node in the element tree.
#[derive(Clone)]
pub enum Node {
    Element(Element),
    Text(TextNode),
}

impl From<Element> for Node {
    fn from(e: Element) -> Self {
        Node::Element(e)
    }
}

impl From<TextNode> for Node {
    fn from(t: TextNode) -> Self {
        Node::Text(t)
    }
}

/// An HTML-like element: tag, classes (Tailwind), attributes, children and event handlers.
///
/// The `ui!` macro is sugar over the builder methods below; you can also use
/// them directly for programmatically-built UI.
#[derive(Clone)]
pub struct Element {
    pub tag: &'static str,
    pub classes: Vec<String>,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    pub on_click: Option<ClickHandler>,
    pub on_input: Option<InputHandler>,
    pub on_enter: Option<EnterHandler>,
    pub on_key: Option<KeyHandler>,
    pub on_change: Option<ChangeHandler>,
}

impl Default for Element {
    fn default() -> Self {
        Self::new("div")
    }
}

impl Element {
    pub fn new(tag: &'static str) -> Self {
        Self {
            tag,
            classes: Vec::new(),
            attrs: Vec::new(),
            children: Vec::new(),
            on_click: None,
            on_input: None,
            on_enter: None,
            on_key: None,
            on_change: None,
        }
    }

    /// Appends Tailwind classes (space-separated).
    pub fn class(mut self, c: impl Into<String>) -> Self {
        self.classes.push(c.into());
        self
    }

    /// Sets an attribute (`value`, `placeholder`, ...).
    pub fn attr(mut self, k: &str, v: impl Into<String>) -> Self {
        self.attrs.push((k.to_string(), v.into()));
        self
    }

    /// Shorthand for `.attr("value", v)` (used for `<input>`).
    pub fn value(self, v: impl Into<String>) -> Self {
        self.attr("value", v)
    }

    /// Shorthand for `.attr("placeholder", v)`.
    pub fn placeholder(self, v: impl Into<String>) -> Self {
        self.attr("placeholder", v)
    }

    pub fn on_click(mut self, h: ClickHandler) -> Self {
        self.on_click = Some(h);
        self
    }

    pub fn on_input(mut self, h: InputHandler) -> Self {
        self.on_input = Some(h);
        self
    }

    pub fn on_enter(mut self, h: EnterHandler) -> Self {
        self.on_enter = Some(h);
        self
    }

    pub fn on_key(mut self, h: KeyHandler) -> Self {
        self.on_key = Some(h);
        self
    }

    pub fn on_change(mut self, h: ChangeHandler) -> Self {
        self.on_change = Some(h);
        self
    }

    /// Appends a child node.
    pub fn child(mut self, node: impl Into<Node>) -> Self {
        self.children.push(node.into());
        self
    }

    /// Appends a text child (`Display`-formatted).
    pub fn text(self, t: impl Display) -> Self {
        self.child(TextNode::new(t.to_string()))
    }

    /// Appends many children at once.
    pub fn children(mut self, iter: impl IntoIterator<Item = Node>) -> Self {
        self.children.extend(iter);
        self
    }

    pub fn get_attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
    }
}

/// Anything that can appear as a child in `ui!`:
/// elements, strings, numbers, and `Vec<Element>` (dynamic lists).
///
/// `impl Child for Element` (single element child),
/// `impl Child for Vec<Element>` (dynamic list child),
/// plus concrete impls for all common text-ish types, so `{count.get()}`
/// inside `ui!` renders as text.
pub trait Child {
    fn into_nodes(self) -> Vec<Node>;
}

macro_rules! text_child {
    ($($t:ty),* $(,)?) => {
        $(
            impl Child for $t {
                fn into_nodes(self) -> Vec<Node> {
                    vec![Node::Text(TextNode::new(self.to_string()))]
                }
            }
        )*
    };
}

text_child! {
    &'static str,
    String,
    std::borrow::Cow<'static, str>,
    TextNode,
    bool,
    char,
    i8, i16, i32, i64, i128, isize,
    u8, u16, u32, u64, u128, usize,
    f32, f64,
}

impl Child for Element {
    fn into_nodes(self) -> Vec<Node> {
        vec![Node::Element(self)]
    }
}

impl Child for Vec<Element> {
    fn into_nodes(self) -> Vec<Node> {
        self.into_iter().map(Node::Element).collect()
    }
}

/// Converts a `ui!` `{...}` child expression into nodes. Used by the macro.
pub fn child_of(c: impl Child) -> Vec<Node> {
    c.into_nodes()
}

/// Converts a text value into a text node. Used by the macro.
pub fn text(t: impl Display) -> TextNode {
    TextNode::new(t.to_string())
}
