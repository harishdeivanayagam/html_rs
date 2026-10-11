//! # html_rs_macro
//!
//! Provides the `ui!` macro — JSX-like HTML syntax that compiles down to
//! `html_rs::Element` builder calls.
//!
//! ```ignore
//! ui! {
//!     <div class="flex items-center gap-2">
//!         <p class="text-white">"Counter: "{count.get()}</p>
//!         <button class="px-4 bg-blue-600" onclick={move || count.update(|c| *c += 1)}>
//!             "+"
//!         </button>
//!     </div>
//! }
//! ```

use std::fmt::Write;
use proc_macro::{Delimiter, Spacing, TokenStream, TokenTree};

type PResult<T> = Result<T, String>;

/// Supported event attributes → (element builder method, handler constructor, event type).
const EVENTS: &[(&str, &str, &str)] = &[
    ("onclick", "on_click", "PointerEvent"),
    ("oninput", "on_input", "str"),
    ("onenter", "on_enter", "str"),
    ("onkeydown", "on_key", "KeyEventInfo"),
];

#[proc_macro]
pub fn ui(input: TokenStream) -> TokenStream {
    let tokens: Vec<TokenTree> = input.into_iter().collect();
    let mut p = Parser { tokens, pos: 0 };
    match p.parse_element() {
        Ok(code) => code.parse().unwrap_or_else(|e| panic!("ui!: generated invalid code: {e}")),
        Err(msg) => {
            let msg = msg.replace('"', "'");
            format!("compile_error!(\"ui!: {msg}\");").parse().unwrap()
        }
    }
}


struct Parser {
    tokens: Vec<TokenTree>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&TokenTree> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<TokenTree> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn expect_punct(&mut self, ch: char) -> PResult<()> {
        match self.next() {
            Some(TokenTree::Punct(p)) if p.as_char() == ch => Ok(()),
            other => Err(format!("expected '{ch}', found {other:?}")),
        }
    }

    fn expect_ident(&mut self) -> PResult<String> {
        match self.next() {
            Some(TokenTree::Ident(i)) => Ok(i.to_string()),
            other => Err(format!("expected identifier, found {other:?}")),
        }
    }

    // ------------------------------------------------------------------
    // Elements
    // ------------------------------------------------------------------

    fn parse_element(&mut self) -> PResult<String> {
        self.expect_punct('<')?;
        let tag = self.expect_ident()?;
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut self_closing = false;

        loop {
            match self.peek().cloned() {
                Some(TokenTree::Punct(p)) if p.as_char() == '>' => {
                    self.pos += 1;
                    break;
                }
                Some(TokenTree::Punct(p)) if p.as_char() == '/' => {
                    self.pos += 1;
                    self.expect_punct('>')?;
                    self_closing = true;
                    break;
                }
                Some(TokenTree::Ident(_)) => {
                    let name = self.expect_ident()?;
                    self.expect_punct('=')?;
                    let value = self.parse_attr_value()?;
                    attrs.push((name, value));
                }
                other => return Err(format!("unexpected token in <{tag}> attributes: {other:?}")),
            }
        }

        if self_closing {
            return self.finish_element(&tag, &attrs, "");
        }

        // Children until `</tag>`.
        let mut children = String::new();
        let mut text_parts: Vec<String> = Vec::new();
        loop {
            match self.peek().cloned() {
                None => return Err(format!("unexpected end of input: <{tag}> is not closed")),
                Some(TokenTree::Punct(p)) if p.as_char() == '<' => {
                    // Could be a child element or the closing tag.
                    let is_close = matches!(
                        (self.tokens.get(self.pos + 1), self.tokens.get(self.pos + 2)),
                        (Some(TokenTree::Punct(a)), Some(TokenTree::Ident(_))) if a.as_char() == '/'
                    );
                    if is_close {
                        self.pos += 3; // `<`, `/`, tag
                        self.expect_punct('>')?;
                        break;
                    }
                    if !text_parts.is_empty() {
                        Self::push_child(&mut children, &self.flush_text(&mut text_parts));
                    }
                    Self::push_child(&mut children, &self.parse_element()?);
                }
                Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => {
                    self.pos += 1;
                    // An expression child. Anything `Child` works: elements,
                    // element lists, and `Display` values (rendered as text).
                    if !text_parts.is_empty() {
                        Self::push_child(&mut children, &self.flush_text(&mut text_parts));
                    }
                    Self::push_child(&mut children, &g.stream().to_string());
                }
                Some(TokenTree::Literal(_)) => {
                    let t = self.next().unwrap();
                    text_parts.push(unescape(&t.to_string()));
                }
                Some(_) => {
                    // Raw (unquoted) text: idents and puncts. Spacing is
                    // approximated; use string literals for exact text.
                    let mut raw = String::new();
                    loop {
                        match self.peek() {
                            Some(TokenTree::Punct(p)) if p.as_char() == '<' => break,
                            Some(TokenTree::Ident(_)) | Some(TokenTree::Punct(_)) => {
                                let t = self.next().unwrap();
                                raw.push_str(&t.to_string());
                                match &t {
                                    TokenTree::Punct(p) if p.spacing() == Spacing::Joint => {}
                                    _ => raw.push(' '),
                                }
                            }
                            _ => break,
                        }
                    }
                    text_parts.push(raw.trim().to_string());
                }
            }
        }
        if !text_parts.is_empty() {
            Self::push_child(&mut children, &self.flush_text(&mut text_parts));
        }

        self.finish_element(&tag, &attrs, &children)
    }

    /// Appends one child expression to the comma-separated children list.
    fn push_child(children: &mut String, item: &str) {
        if item.is_empty() {
            return;
        }
        if !children.trim_end().is_empty() {
            children.push(',');
        }
        // Wrap in `child_of` so any `Child` type flattens into `Vec<Node>`.
        children.push_str("::html_rs::child_of(");
        children.push_str(item);
        children.push(')');
    }

    fn finish_element(&mut self, tag: &str, attrs: &[(String, String)], children: &str) -> PResult<String> {
        let mut code = String::new();
        let _ = writeln!(code, "{{ let mut __el = ::html_rs::Element::new(\"{tag}\");");
        for (name, value) in attrs {
            match name.as_str() {
                "class" | "className" => {
                    let _ = writeln!(code, "__el = __el.class({value});");
                }
                "value" => {
                    let _ = writeln!(code, "__el = __el.value({value});");
                }
                "placeholder" => {
                    let _ = writeln!(code, "__el = __el.placeholder({value});");
                }
                _ => {
                    let Some((_, method, event_ty)) = EVENTS.iter().find(|(n, _, _)| *n == name) else {
                        return Err(format!("unsupported attribute `{name}` in <{tag}>"));
                    };
                    let handler = handler_code(value, event_ty);
                    let _ = writeln!(code, "__el = __el.{method}(::html_rs::{constructor}({handler}));",
                        constructor = constructor_for(event_ty));
                }
            }
        }
        if !children.is_empty() {
            let _ = writeln!(code, "__el = __el.children([{children}].into_iter().flatten().collect::<Vec<::html_rs::Node>>());");
        }
        let _ = writeln!(code, "__el }}");
        Ok(code)
    }

    fn flush_text(&mut self, parts: &mut Vec<String>) -> String {
        let parts = std::mem::take(parts);
        // Drop text runs that are pure whitespace.
        if parts.iter().all(|s| s.trim().is_empty()) {
            return String::new();
        }
        let mut fmt = String::new();
        for part in parts {
            // Escape format braces.
            fmt.push_str(&part.replace('{', "{{").replace('}', "}}"));
        }
        // A `String` implements `Child`; push_child wraps it in `child_of`.
        format!("format!(\"{fmt}\")")
    }

    // ------------------------------------------------------------------
    // Attributes
    // ------------------------------------------------------------------

    fn parse_attr_value(&mut self) -> PResult<String> {
        match self.next() {
            Some(TokenTree::Literal(lit)) => Ok(lit.to_string()),
            Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => Ok(g.stream().to_string()),
            other => Err(format!("expected attribute value (string literal or {{expr}}), found {other:?}")),
        }
    }
}

/// Builds the handler adapter. Detects whether the user closure takes an event
/// parameter (`move |e| ...` / `|e| ...`) or none (`move || ...`).
fn handler_code(value: &str, event_ty: &str) -> String {
    let ev_type = match event_ty {
        "str" => "str".to_string(),
        t => format!("::html_rs::{t}"),
    };
    let zero_arg = {
        let ts: Vec<TokenTree> = value.parse::<TokenStream>().unwrap_or_default().into_iter().collect();
        let mut i = 0;
        if matches!(ts.get(i), Some(TokenTree::Ident(id)) if id.to_string() == "move") {
            i += 1;
        }
        matches!(
            (ts.get(i), ts.get(i + 1)),
            (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(_)))
                if a.as_char() == '|' && a.spacing() == Spacing::Joint
        )
    };
    if zero_arg {
        format!("move |__e: &{ev_type}| {{ ({value})() }}")
    } else {
        format!("move |__e: &{ev_type}| {{ ({value})(__e) }}")
    }
}

fn constructor_for(event_ty: &str) -> &'static str {
    match event_ty {
        "PointerEvent" => "click",
        "str" => "input",
        "KeyEventInfo" => "key",
        _ => unreachable!(),
    }
}

/// Unescapes a Rust string literal token (`"a\nb"` → `a<newline>b`).
fn unescape(lit: &str) -> String {
    let inner = lit
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(lit);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('u') => {
                // \u{XXXX}
                let mut hex = String::new();
                for c in chars.by_ref() {
                    match c {
                        '{' => {}
                        '}' => break,
                        _ => hex.push(c),
                    }
                }
                if let Some(v) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(v);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}
