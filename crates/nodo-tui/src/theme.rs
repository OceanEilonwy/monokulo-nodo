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
    fn truecolor_matches_the_design_palette() {
        let t = Theme::truecolor();
        assert_eq!(t.ground, Color::Rgb(0x10, 0x12, 0x14));
        assert_eq!(t.accent, Color::Rgb(0xF2, 0xA3, 0x3A));
    }
}
