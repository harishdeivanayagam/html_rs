//! Structural diffing of element trees.
//!
//! The renderer keeps the previous frame's element tree and taffy layout
//! alive. On each re-render, [`diff_element`] decides — per tree position —
//! whether a node can be **patched in place** (incremental relayout) or must
//! be **rebuilt** (structural change). [`equivalent`] answers the coarser
//! question "did the tree change at all?", letting the runtime skip the
//! repaint entirely when a state write produced identical output.

use crate::element::{Element, Node};

/// What changed between two elements occupying the same tree position
/// (matched positionally, by child index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElementChange {
    /// Identical structure, classes and text content. Event handlers may
    /// still differ (they never affect layout). The taffy node can be reused
    /// untouched.
    None,
    /// Classes or text content changed. Same taffy node is patched in place
    /// (`set_style` / `set_node_context`) and only its subtree re-layouts.
    InPlace,
    /// Tag or child *shape* changed (different tag, different child count, or
    /// an element child became text or vice versa). The subtree is rebuilt
    /// with fresh taffy nodes.
    Structure,
}

fn is_text_leaf(el: &Element) -> bool {
    el.children.len() == 1 && matches!(el.children[0], Node::Text(_))
}

/// Compares `old` and `new` at the same tree position.
///
/// Only *layout-relevant* differences are reported: attributes (e.g. an
/// `<input>`'s `value`) and event handlers never affect layout, so changing
/// them alone still yields [`ElementChange::None`] here. Use [`equivalent`]
/// when attributes matter (skip-repaint check).
pub fn diff_element(old: &Element, new: &Element) -> ElementChange {
    if old.tag != new.tag || is_text_leaf(old) != is_text_leaf(new) {
        return ElementChange::Structure;
    }

    let mut change = ElementChange::None;
    if old.classes != new.classes {
        change = ElementChange::InPlace;
    }

    if is_text_leaf(old) {
        // Both are single-text-child leaves (the is_text_leaf check above
        // guarantees they agree).
        let (Node::Text(a), Node::Text(b)) = (&old.children[0], &new.children[0]) else {
            return ElementChange::Structure;
        };
        if a.content != b.content {
            change = ElementChange::InPlace;
        }
    } else {
        // Child *shape* must match pairwise; deeper differences are resolved
        // recursively by the caller's per-child merge.
        if old.children.len() != new.children.len() {
            return ElementChange::Structure;
        }
        for (a, b) in old.children.iter().zip(&new.children) {
            match (a, b) {
                (Node::Element(_), Node::Element(_)) | (Node::Text(_), Node::Text(_)) => {}
                _ => return ElementChange::Structure,
            }
        }
    }

    change
}

/// Full structural equality: tags, classes, **attributes**, text content and
/// children — recursively. Event handlers are ignored (they are opaque
/// closures; two views over the same state always produce equivalent ones).
///
/// The runtime uses this to skip the repaint entirely when a state write
/// didn't change the rendered output (e.g. `count.set(count.get())`).
pub fn equivalent(a: &Element, b: &Element) -> bool {
    a.tag == b.tag
        && a.classes == b.classes
        && a.attrs == b.attrs
        && a.children.len() == b.children.len()
        && a.children.iter().zip(&b.children).all(|(x, y)| match (x, y) {
            (Node::Text(x), Node::Text(y)) => x.content == y.content,
            (Node::Element(x), Node::Element(y)) => equivalent(x, y),
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_elements_are_none() {
        let a = Element::new("div").class("p-4 flex").child(Element::new("p").text("hi"));
        let b = Element::new("div").class("p-4 flex").child(Element::new("p").text("hi"));
        assert_eq!(diff_element(&a, &b), ElementChange::None);
        assert!(equivalent(&a, &b));
    }

    #[test]
    fn class_change_is_in_place() {
        let a = Element::new("div").class("p-4");
        let b = Element::new("div").class("p-8");
        assert_eq!(diff_element(&a, &b), ElementChange::InPlace);
    }

    #[test]
    fn text_change_is_in_place() {
        let a = Element::new("p").text("a");
        let b = Element::new("p").text("b");
        assert_eq!(diff_element(&a, &b), ElementChange::InPlace);
    }

    #[test]
    fn child_count_change_is_structure() {
        let a = Element::new("div").child(Element::new("p").text("a"));
        let b = Element::new("div")
            .child(Element::new("p").text("a"))
            .child(Element::new("p").text("b"));
        assert_eq!(diff_element(&a, &b), ElementChange::Structure);
    }

    #[test]
    fn kind_change_is_structure_at_child_level() {
        // Top level matches (same tag, same child shape); the *child* position
        // is where the tag differs — that's where the merge rebuilds.
        let a = Element::new("div").child(Element::new("p").text("a"));
        let b = Element::new("div").child(Element::new("button").text("a"));
        assert_eq!(diff_element(&a, &b), ElementChange::None);
        let (Node::Element(ca), Node::Element(cb)) = (&a.children[0], &b.children[0]) else {
            unreachable!()
        };
        assert_eq!(diff_element(ca, cb), ElementChange::Structure);
    }

    #[test]
    fn leaf_to_plain_is_structure() {
        let a = Element::new("div").text("only");
        let b = Element::new("div")
            .text("only")
            .child(Element::new("p").text("x"));
        assert_eq!(diff_element(&a, &b), ElementChange::Structure);
    }

    #[test]
    fn attr_change_is_none_for_layout_but_not_equivalent() {
        let a = Element::new("input").attr("value", "a");
        let b = Element::new("input").attr("value", "ab");
        assert_eq!(diff_element(&a, &b), ElementChange::None);
        assert!(!equivalent(&a, &b));
    }

    #[test]
    fn equivalent_ignores_handlers() {
        let a = Element::new("button")
            .text("x")
            .on_click(crate::click(|_| {}));
        let b = Element::new("button")
            .text("x")
            .on_click(crate::click(|_| {}));
        assert!(equivalent(&a, &b));
    }
}
