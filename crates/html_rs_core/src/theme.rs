//! The **shadcn/ui Slate theme**: semantic color tokens (the CSS-variable
//! equivalents `--background`, `--primary`, …) resolved at class-compile
//! time, in light and dark variants.
//!
//! Components (`<button>`, `<select>`, `<input>`, `<dialog>`, …) pick up
//! their look from token classes like `bg-primary` / `text-muted-foreground`
//! via their tag defaults, so an app is themed by default and any token
//! class can still be overwritten with a plain Tailwind class
//! (`class="bg-blue-600"` on a button wins over `bg-primary`).
//!
//! Token names mirror shadcn: `background`, `foreground`, `card`,
//! `card-foreground`, `popover`, `popover-foreground`, `primary`,
//! `primary-foreground`, `secondary`, `secondary-foreground`, `muted`,
//! `muted-foreground`, `accent`, `accent-foreground`, `destructive`,
//! `destructive-foreground`, `border`, `input`, `ring`.

use crate::style::Rgba;

/// Light or dark palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ThemeMode {
    /// The default palette (matches shadcn's dark slate look).
    #[default]
    Dark,
    Light,
}

impl ThemeMode {
    /// Parses `"light"` / `"dark"` (case-insensitive). Unknown values fall
    /// back to [`ThemeMode::Dark`].
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "light" => ThemeMode::Light,
            _ => ThemeMode::Dark,
        }
    }
}

impl From<&str> for ThemeMode {
    fn from(s: &str) -> Self {
        ThemeMode::parse(s)
    }
}

impl From<String> for ThemeMode {
    fn from(s: String) -> Self {
        ThemeMode::parse(&s)
    }
}

/// The semantic color tokens of the shadcn Slate theme (sRGB floats).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub background: Rgba,
    pub foreground: Rgba,
    pub card: Rgba,
    pub card_foreground: Rgba,
    pub popover: Rgba,
    pub popover_foreground: Rgba,
    pub primary: Rgba,
    pub primary_foreground: Rgba,
    pub secondary: Rgba,
    pub secondary_foreground: Rgba,
    pub muted: Rgba,
    pub muted_foreground: Rgba,
    pub accent: Rgba,
    pub accent_foreground: Rgba,
    pub destructive: Rgba,
    pub destructive_foreground: Rgba,
    pub border: Rgba,
    pub input: Rgba,
    pub ring: Rgba,
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

impl Theme {
    /// shadcn Slate, light variant.
    pub const fn light() -> Self {
        Self {
            background: rgb(255, 255, 255), // white
            foreground: rgb(2, 8, 23),      // hsl(222.2 84% 4.9%)
            card: rgb(255, 255, 255),
            card_foreground: rgb(2, 8, 23),
            popover: rgb(255, 255, 255),
            popover_foreground: rgb(2, 8, 23),
            primary: rgb(15, 23, 42),       // slate-900
            primary_foreground: rgb(248, 250, 252), // slate-50
            secondary: rgb(241, 245, 249),  // slate-100
            secondary_foreground: rgb(15, 23, 42),
            muted: rgb(241, 245, 249),
            muted_foreground: rgb(100, 116, 139), // slate-500
            accent: rgb(241, 245, 249),
            accent_foreground: rgb(15, 23, 42),
            destructive: rgb(239, 68, 68),  // red-500
            destructive_foreground: rgb(248, 250, 252),
            border: rgb(226, 232, 240),     // slate-200
            input: rgb(226, 232, 240),
            ring: rgb(2, 8, 23),
        }
    }

    /// shadcn Slate, dark variant (the default).
    pub const fn dark() -> Self {
        Self {
            background: rgb(2, 8, 23),      // hsl(222.2 84% 4.9%)
            foreground: rgb(248, 250, 252), // slate-50
            card: rgb(2, 8, 23),
            card_foreground: rgb(248, 250, 252),
            popover: rgb(2, 8, 23),
            popover_foreground: rgb(248, 250, 252),
            primary: rgb(248, 250, 252),    // slate-50
            primary_foreground: rgb(15, 23, 42), // slate-900
            secondary: rgb(30, 41, 59),     // slate-800
            secondary_foreground: rgb(248, 250, 252),
            muted: rgb(30, 41, 59),
            muted_foreground: rgb(148, 163, 184), // slate-400
            accent: rgb(30, 41, 59),
            accent_foreground: rgb(248, 250, 252),
            destructive: rgb(127, 29, 29),  // red-900
            destructive_foreground: rgb(248, 250, 252),
            border: rgb(30, 41, 59),
            input: rgb(30, 41, 59),
            ring: rgb(203, 213, 225),       // slate-300
        }
    }
}

thread_local! {
    static MODE: std::cell::Cell<ThemeMode> = const { std::cell::Cell::new(ThemeMode::Dark) };
}

/// The active theme mode (per-thread; the runtime is single-threaded).
pub fn mode() -> ThemeMode {
    MODE.with(|m| m.get())
}

/// Switches the active theme. Call before `launch().run()` (the `App`
/// builder's `.light_mode(..)` does this for you).
pub fn set_mode(m: ThemeMode) {
    MODE.with(|c| c.set(m));
}

/// The active theme's tokens.
pub fn current() -> &'static Theme {
    match mode() {
        ThemeMode::Light => &LIGHT,
        ThemeMode::Dark => &DARK,
    }
}

static LIGHT: Theme = Theme::light();
static DARK: Theme = Theme::dark();

/// Resolves a shadcn token name (`"primary"`, `"muted-foreground"`) to the
/// active theme's color. Plain palette colors are handled by
/// [`crate::colors::resolve_color`]; this only answers token names.
pub fn resolve_token_color(name: &str) -> Option<Rgba> {
    let t = current();
    let c = match name {
        "background" => t.background,
        "foreground" => t.foreground,
        "card" => t.card,
        "card-foreground" => t.card_foreground,
        "popover" => t.popover,
        "popover-foreground" => t.popover_foreground,
        "primary" => t.primary,
        "primary-foreground" => t.primary_foreground,
        "secondary" => t.secondary,
        "secondary-foreground" => t.secondary_foreground,
        "muted" => t.muted,
        "muted-foreground" => t.muted_foreground,
        "accent" => t.accent,
        "accent-foreground" => t.accent_foreground,
        "destructive" => t.destructive,
        "destructive-foreground" => t.destructive_foreground,
        "border" => t.border,
        "input" => t.input,
        "ring" => t.ring,
        _ => return None,
    };
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_is_default() {
        assert_eq!(mode(), ThemeMode::Dark);
        assert_eq!(current().background, rgb(2, 8, 23));
    }

    #[test]
    fn mode_switch() {
        set_mode(ThemeMode::Light);
        assert_eq!(current().background, rgb(255, 255, 255));
        assert_eq!(current().primary, rgb(15, 23, 42));
        set_mode(ThemeMode::Dark);
        assert_eq!(current().primary, rgb(248, 250, 252));
    }

    #[test]
    fn parse_mode() {
        assert_eq!(ThemeMode::parse("light"), ThemeMode::Light);
        assert_eq!(ThemeMode::parse("DARK"), ThemeMode::Dark);
        assert_eq!(ThemeMode::parse("nonsense"), ThemeMode::Dark);
    }

    #[test]
    fn token_lookup() {
        assert_eq!(resolve_token_color("muted-foreground"), Some(rgb(148, 163, 184)));
        assert_eq!(resolve_token_color("slate"), None);
        set_mode(ThemeMode::Light);
        assert_eq!(resolve_token_color("primary"), Some(rgb(15, 23, 42)));
        set_mode(ThemeMode::Dark);
    }
}
