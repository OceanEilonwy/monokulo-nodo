//! Colours from the design: near-black ground and an amber accent.
//!
//! Every state is also carried by a symbol (✓ ✕ ▲ ◆ ● ○ ◉ ▸), so colour only reinforces it. That
//! is what lets the monochrome theme stay usable.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub ground: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub ok: Color,
    pub err: Color,
    pub info: Color,
    pub border: Color,
    /// Surfaces, darkest to lightest: inset fields sit below the background, bars and cards rise
    /// above it, and selected or open things are raised further. Steps stay small to keep it calm.
    pub field: Color,
    pub bar: Color,
    pub card: Color,
    pub raised: Color,
    /// Buttons get rounded ends (Powerline caps U+E0B6 and U+E0B4), which need a Nerd Font.
    pub rounded: bool,
    mono: bool,
}

impl Theme {
    pub const fn truecolor() -> Self {
        Self {
            ground: Color::Rgb(0x10, 0x12, 0x14),
            fg: Color::Rgb(0xD7, 0xDA, 0xDC),
            dim: Color::Rgb(0xA3, 0xAA, 0xB0),
            accent: Color::Rgb(0xF2, 0xA3, 0x3A),
            ok: Color::Rgb(0x6F, 0xD6, 0xB3),
            err: Color::Rgb(0xFF, 0x8A, 0x7A),
            info: Color::Rgb(0x86, 0xBE, 0xFF),
            border: Color::Rgb(0x54, 0x5C, 0x63),
            field: Color::Rgb(0x0B, 0x0D, 0x0F),
            bar: Color::Rgb(0x17, 0x1A, 0x1D),
            card: Color::Rgb(0x1D, 0x21, 0x25),
            raised: Color::Rgb(0x2C, 0x32, 0x38),
            rounded: false,
            mono: false,
        }
    }

    pub const fn ansi256() -> Self {
        Self {
            ground: Color::Indexed(233),
            fg: Color::Indexed(253),
            dim: Color::Indexed(248),
            accent: Color::Indexed(215),
            ok: Color::Indexed(115),
            err: Color::Indexed(210),
            info: Color::Indexed(111),
            border: Color::Indexed(240),
            field: Color::Indexed(232),
            bar: Color::Indexed(234),
            card: Color::Indexed(235),
            raised: Color::Indexed(237),
            rounded: false,
            mono: false,
        }
    }

    pub const fn mono() -> Self {
        Self {
            ground: Color::Reset,
            fg: Color::Reset,
            dim: Color::Reset,
            accent: Color::Reset,
            ok: Color::Reset,
            err: Color::Reset,
            info: Color::Reset,
            border: Color::Reset,
            field: Color::Reset,
            bar: Color::Reset,
            card: Color::Reset,
            raised: Color::Reset,
            rounded: false,
            mono: true,
        }
    }

    /// SSH clients vary, and an RGB escape on a 256-colour terminal gives wrong colours, so RGB
    /// is only used when the terminal says it supports it.
    pub fn detect(colorterm: Option<&str>, no_color: bool) -> Self {
        if no_color {
            Self::mono()
        } else if matches!(colorterm, Some("truecolor" | "24bit")) {
            Self::truecolor()
        } else {
            Self::ansi256()
        }
    }

    pub fn with_rounded(mut self, rounded: bool) -> Self {
        self.rounded = rounded && !self.mono;
        self
    }

    /// Whether this theme is the no-colour one, where surfaces cannot show and selection uses
    /// reverse video.
    pub fn is_mono(&self) -> bool {
        self.mono
    }

    pub fn base(&self) -> Style {
        Style::new().fg(self.fg).bg(self.ground)
    }

    pub fn border_style(&self) -> Style {
        Style::new().fg(self.border).bg(self.ground)
    }

    pub fn selected(&self) -> Style {
        if self.mono {
            Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new().fg(self.ground).bg(self.accent).add_modifier(Modifier::BOLD)
        }
    }

    pub fn n<'a>(&self, text: impl Into<std::borrow::Cow<'a, str>>) -> Span<'a> {
        Span::styled(text, Style::new().fg(self.fg))
    }

    pub fn b<'a>(&self, text: impl Into<std::borrow::Cow<'a, str>>) -> Span<'a> {
        Span::styled(text, Style::new().fg(self.fg).add_modifier(Modifier::BOLD))
    }

    pub fn d<'a>(&self, text: impl Into<std::borrow::Cow<'a, str>>) -> Span<'a> {
        Span::styled(text, Style::new().fg(self.dim))
    }

    pub fn a<'a>(&self, text: impl Into<std::borrow::Cow<'a, str>>) -> Span<'a> {
        Span::styled(text, Style::new().fg(self.accent))
    }

    pub fn o<'a>(&self, text: impl Into<std::borrow::Cow<'a, str>>) -> Span<'a> {
        Span::styled(text, Style::new().fg(self.ok))
    }

    pub fn e<'a>(&self, text: impl Into<std::borrow::Cow<'a, str>>) -> Span<'a> {
        Span::styled(text, Style::new().fg(self.err))
    }

    pub fn i<'a>(&self, text: impl Into<std::borrow::Cow<'a, str>>) -> Span<'a> {
        Span::styled(text, Style::new().fg(self.info))
    }

    pub fn s<'a>(&self, text: impl Into<std::borrow::Cow<'a, str>>) -> Span<'a> {
        Span::styled(text, self.selected())
    }
}

/// The `[tui] rounded_buttons` choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundedButtons {
    Auto,
    On,
    Off,
}

impl RoundedButtons {
    /// Reads `rounded_buttons` from the `[tui]` table of a config file's text. Anything missing or
    /// unknown is `Auto`.
    pub fn from_config(text: &str) -> Self {
        let mut in_tui = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.starts_with('[') {
                in_tui = line == "[tui]";
                continue;
            }
            if !in_tui {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "rounded_buttons" {
                    return match value.trim().trim_matches('"') {
                        "on" | "true" => Self::On,
                        "off" | "false" => Self::Off,
                        _ => Self::Auto,
                    };
                }
            }
        }
        Self::Auto
    }

    /// `Auto` turns rounded ends on only in terminals known to draw Nerd Font symbols without a
    /// patched font: kitty, WezTerm and Ghostty bundle them. `TERM` usually survives SSH, so a
    /// session from those terminals is recognised too. Anywhere else, including the Linux console,
    /// buttons keep plain flat ends, which always draw correctly.
    pub fn resolve(self, term: Option<&str>, term_program: Option<&str>, kitty: bool) -> bool {
        match self {
            Self::On => true,
            Self::Off => false,
            Self::Auto => {
                kitty
                    || matches!(term, Some("xterm-kitty" | "xterm-ghostty" | "wezterm"))
                    || matches!(term_program, Some("WezTerm" | "ghostty"))
            }
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::truecolor()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_prefers_no_color_then_truecolor_then_256() {
        assert_eq!(Theme::detect(Some("truecolor"), true), Theme::mono());
        assert_eq!(Theme::detect(Some("truecolor"), false), Theme::truecolor());
        assert_eq!(Theme::detect(Some("24bit"), false), Theme::truecolor());
        assert_eq!(Theme::detect(None, false), Theme::ansi256());
    }

    #[test]
    fn rounded_buttons_config_and_auto_detection() {
        assert_eq!(RoundedButtons::from_config("[tui]\nrounded_buttons = \"on\"\n"), RoundedButtons::On);
        assert_eq!(RoundedButtons::from_config("[tui]\nrounded_buttons = \"off\" # plain\n"), RoundedButtons::Off);
        assert_eq!(RoundedButtons::from_config("[other]\nrounded_buttons = \"on\"\n"), RoundedButtons::Auto);
        assert_eq!(RoundedButtons::from_config(""), RoundedButtons::Auto);
        assert!(RoundedButtons::Auto.resolve(Some("xterm-kitty"), None, false));
        assert!(RoundedButtons::Auto.resolve(Some("xterm-256color"), Some("WezTerm"), false));
        assert!(!RoundedButtons::Auto.resolve(Some("linux"), None, false));
        assert!(!RoundedButtons::Auto.resolve(Some("xterm-256color"), None, false));
        assert!(!RoundedButtons::Off.resolve(Some("xterm-kitty"), None, true));
        assert!(RoundedButtons::On.resolve(Some("linux"), None, false));
        assert!(!Theme::mono().with_rounded(true).rounded, "no colour means no caps to draw");
    }

    #[test]
    fn truecolor_matches_the_design_palette() {
        let t = Theme::truecolor();
        assert_eq!(t.ground, Color::Rgb(0x10, 0x12, 0x14));
        assert_eq!(t.accent, Color::Rgb(0xF2, 0xA3, 0x3A));
    }
}
