//! The v3 visual system: a tab strip on its own surface, cards as even filled rectangles, inset
//! fields, flat buttons (with rounded ends where the terminal has Nerd Font symbols) inside the
//! card they act on, and one dim line of keys at the bottom.
//!
//! Surfaces are background fills, not lines: the theme's `bar`, `card`, `field` and `raised`
//! colours. Every card row is padded to the same width, so a fill is always a rectangle. Pages
//! built here also declare their click targets (`crate::hit`).

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::layout::{Item, Page};
use super::pad;
use crate::app::{App, Tab};
use crate::hit::{Click, Target};
use crate::theme::Theme;

pub const MARGIN: &str = "    ";

/// Rows from the top that belong to the tab strip: an empty row, the tabs, an empty row.
pub const STRIP_ROWS: u16 = 3;

pub fn on(span: Span<'static>, bg: Color) -> Span<'static> {
    let style = span.style.bg(bg);
    span.style(style)
}

pub fn width_of(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref())).sum()
}

pub fn line(spans: Vec<Span<'static>>) -> Item {
    Item::Text(Line::from(spans))
}

pub fn margin(t: &Theme) -> Span<'static> {
    t.n(MARGIN)
}

pub fn quiet(t: &Theme, text: &str) -> Item {
    line(vec![margin(t), t.d(text.to_owned())])
}

pub fn heading(t: &Theme, text: &str) -> Item {
    line(vec![margin(t), t.b(text.to_owned())])
}

/// The breadcrumb trail. Each part but the last is a click target: a tab's name opens that tab,
/// anything else goes back one step.
pub fn trail(t: &Theme, parts: &[&str], targets: &mut Vec<Target>) -> Item {
    let mut spans = vec![margin(t)];
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            spans.push(t.d("  ›  "));
        }
        let last = index + 1 == parts.len();
        spans.push(if last { t.n((*part).to_owned()) } else { t.d((*part).to_owned()) });
        if !last {
            let click = Tab::ALL
                .into_iter()
                .find(|tab| tab.label() == *part && index == 0)
                .map_or(Click::Key(KeyCode::Esc), Click::Tab);
            targets.push(Target::text(*part, click));
        }
    }
    line(spans)
}

/// A card: a filled rectangle the width of the page less its margins, every row the same width,
/// three cells of padding on the left.
pub struct Card {
    pub width: usize,
    rows: Vec<Item>,
    /// Where the card's own button row starts, so fitting a short terminal keeps it.
    buttons_at: Option<usize>,
    /// The row the keyboard focus is on.
    focus_at: Option<usize>,
    surface: Color,
}

impl Card {
    pub fn new(t: &Theme, page_width: u16) -> Self {
        let width = usize::from(page_width).saturating_sub(2 * MARGIN.len());
        Card { width, rows: Vec::new(), buttons_at: None, focus_at: None, surface: t.card }
    }

    /// One row of the card. Text too wide for the card wraps at spaces onto more card rows,
    /// keeping its indent, so the fill always stays a rectangle.
    pub fn row(&mut self, t: &Theme, spans: Vec<Span<'static>>) {
        let room = self.width.saturating_sub(4);
        let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
        if width_of(&spans) <= room {
            lines[0] = spans;
        } else {
            let indent = spans.first().map_or(0, |s| s.content.len() - s.content.trim_start().len());
            let mut used = 0;
            for span in spans {
                let style = span.style;
                for (index, word) in span.content.split(' ').enumerate() {
                    let piece = if index == 0 { word.to_owned() } else { format!(" {word}") };
                    let w = unicode_width::UnicodeWidthStr::width(piece.as_str());
                    if used + w > room && used > indent {
                        lines.push(vec![Span::styled(" ".repeat(indent), style)]);
                        used = indent;
                        let trimmed = piece.trim_start().to_owned();
                        used += unicode_width::UnicodeWidthStr::width(trimmed.as_str());
                        lines.last_mut().expect("just pushed").push(Span::styled(trimmed, style));
                    } else {
                        used += w;
                        lines.last_mut().expect("not empty").push(Span::styled(piece, style));
                    }
                }
            }
        }
        for spans in lines {
            let mut out = vec![margin(t), on(t.n("   "), self.surface)];
            let used = 3 + width_of(&spans);
            out.extend(spans.into_iter().map(|s| if s.style.bg.is_none() { on(s, self.surface) } else { s }));
            out.push(on(t.n(" ".repeat(self.width.saturating_sub(used))), self.surface));
            self.rows.push(line(out));
        }
    }

    pub fn blank(&mut self, t: &Theme) {
        self.row(t, Vec::new());
    }

    /// The card's buttons, after the content they act on: right-aligned on the card's inner
    /// padding, the primary rightmost, three cells apart, followed by the card's bottom padding.
    /// Each button is a click target.
    pub fn buttons(&mut self, t: &Theme, list: &[(&str, bool, Click)], targets: &mut Vec<Target>) {
        if list.is_empty() {
            return;
        }
        self.buttons_at = Some(self.rows.len());
        let room = self.width.saturating_sub(6);
        let width = list.iter().map(|(label, _, _)| label.chars().count()).max().unwrap_or(0) + 6;
        let mut spans = Vec::new();
        let mut ordered: Vec<&(&str, bool, Click)> = list.iter().filter(|(_, p, _)| !p).collect();
        ordered.extend(list.iter().filter(|(_, p, _)| *p));
        for (index, (label, primary, click)) in ordered.into_iter().enumerate() {
            if index > 0 {
                spans.push(t.n("   "));
            }
            spans.extend(button(t, label, *primary, width, self.surface));
            targets.push(Target::button(*label, click.clone()));
        }
        let used = width_of(&spans);
        let mut out = vec![t.n(" ".repeat(room.saturating_sub(used)))];
        out.extend(spans);
        self.row(t, out);
        self.blank(t);
    }

    /// One button at a given indent inside the card, for a button that belongs to a part of the
    /// card rather than to the whole of it.
    pub fn inline_button(&mut self, t: &Theme, label: &str, indent: usize, click: Click, targets: &mut Vec<Target>) {
        let width = label.chars().count() + 6;
        let mut out = vec![t.n(" ".repeat(indent))];
        out.extend(button(t, label, true, width, self.surface));
        self.row(t, out);
        targets.push(Target::button(label, click));
        if self.buttons_at.is_none() {
            self.buttons_at = Some(self.rows.len() - 1);
        }
    }

    /// Marks the next row as the one the keyboard focus is on, so fitting keeps it in view.
    pub fn focus_here(&mut self) {
        self.focus_at = Some(self.rows.len());
    }

    /// Fits the card into `max` rows: when it is taller, rows above its buttons are cut, keeping
    /// the focused row in view, and a line says how many more there are, so the buttons stay on
    /// screen.
    pub fn fit(&mut self, t: &Theme, max: usize) {
        if self.rows.len() <= max {
            return;
        }
        let tail = self.buttons_at.map_or(0, |at| self.rows.len() - at);
        let head = max.saturating_sub(tail + 1).max(1);
        let body = self.rows.len() - tail;
        let hidden = body.saturating_sub(head);
        let start = match self.focus_at {
            Some(focus) if focus >= head => (focus + 1 - head).min(body - head),
            _ => 0,
        };
        let tail_rows: Vec<Item> = self.rows.split_off(body);
        let kept: Vec<Item> = self.rows.drain(start..start + head).collect();
        self.rows = kept;
        let at = self.rows.len();
        let arrow = if start > 0 { "↕" } else { "↓" };
        self.row(t, vec![t.d(format!("{arrow} {hidden} more — scroll or make the window taller"))]);
        self.buttons_at = self.buttons_at.map(|_| at + 1);
        self.rows.extend(tail_rows);
    }

    pub fn items(self) -> Vec<Item> {
        self.rows
    }
}

/// An inset box for typing or picking, on a card. A focused box is raised and shows the cursor.
pub fn inset(t: &Theme, value: &str, width: usize, arrow: Option<&str>, focused: bool) -> Vec<Span<'static>> {
    let bg = if focused { t.raised } else { t.field };
    let style = Style::new().fg(t.fg).bg(bg);
    let shown = if focused { format!("{value}▏") } else { value.to_owned() };
    let text = match arrow {
        Some(arrow) => format!(" {} {arrow} ", pad(&shown, width)),
        None => format!(" {} ", pad(&shown, width)),
    };
    let mut spans = vec![Span::styled(text, style)];
    if focused && t.is_mono() {
        spans.insert(0, t.n("▸"));
    }
    spans
}

pub const CAP_LEFT: &str = "\u{E0B6}";
pub const CAP_RIGHT: &str = "\u{E0B4}";

fn centred(label: &str, width: usize) -> String {
    let len = label.chars().count();
    let left = width.saturating_sub(len) / 2;
    format!("{}{label}{}", " ".repeat(left), " ".repeat(width.saturating_sub(len + left)))
}

/// A flat filled button, one row tall, `width + 2` cells wide. With `rounded`, its ends are the
/// Powerline caps in the button's colour on the surface below; otherwise plain padding.
pub fn button(t: &Theme, label: &str, primary: bool, width: usize, under: Color) -> Vec<Span<'static>> {
    if t.is_mono() {
        let text = format!("[{}]", centred(label, width));
        let style = if primary { Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD) } else { Style::new() };
        return vec![Span::styled(text, style)];
    }
    let colour = if primary { t.accent } else { t.raised };
    let ink = if primary { t.ground } else { t.fg };
    let weight = if primary { Modifier::BOLD } else { Modifier::empty() };
    let fill = Style::new().fg(ink).bg(colour).add_modifier(weight);
    if t.rounded {
        let cap = Style::new().fg(colour).bg(under);
        vec![Span::styled(CAP_LEFT, cap), Span::styled(centred(label, width), fill), Span::styled(CAP_RIGHT, cap)]
    } else {
        vec![Span::styled(centred(label, width + 2), fill)]
    }
}

/// The bottom line: only the keys, dim.
pub fn keys_line(t: &Theme, keys: &str) -> Item {
    Item::Text(Line::from(t.d(format!("{MARGIN}{keys}"))))
}

/// How many screen rows items take at width `w`, as the layout wraps them.
pub fn rows_of(items: &[Item], w: u16) -> usize {
    let w = usize::from(w.max(1));
    items
        .iter()
        .map(|item| match item {
            Item::Blank => 1,
            Item::Text(line) => width_of(&line.spans).div_ceil(w).max(1),
            Item::Row(left, right) | Item::Selected(left, right) => {
                if width_of(&left.spans) + width_of(&right.spans) + 1 > w {
                    2
                } else {
                    1
                }
            }
        })
        .sum()
}

/// A v3 page under construction: the tab strip, then content, a card, more content and the keys.
pub struct Screen3 {
    pub page: Page,
    pub before: Vec<Item>,
    pub after: Vec<Item>,
    pub targets: Vec<Target>,
}

impl Screen3 {
    pub fn new(app: &App, t: &Theme, tab: Tab, w: u16) -> Self {
        let mut targets = Vec::new();
        let page = top(app, t, tab, w, &mut targets);
        Self { page, before: Vec::new(), after: Vec::new(), targets }
    }

    /// Fits the card into the rows left and finishes the page. Targets are in drawing order: the
    /// tabs, then what comes before the card, the card, and after it.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(self, t: &Theme, mut card: Card, card_targets: Vec<Target>, after_targets: Vec<Target>, keys: &str, w: u16, h: u16) -> Page {
        let used = usize::from(STRIP_ROWS) + 1 + rows_of(&self.before, w) + rows_of(&self.after, w);
        card.fit(t, usize::from(h).saturating_sub(used));
        let mut items = self.before;
        items.extend(card.items());
        items.extend(self.after);
        let mut page = self.page.plain(items).footer(keys_line(t, keys));
        page.targets = self.targets;
        page.targets.extend(card_targets);
        page.targets.extend(after_targets);
        page
    }
}

/// The tab strip: an empty row, the tabs on the page margin with room between them, the current
/// one a flat filled rectangle padded two cells each side, and an empty row. Each tab is a click
/// target.
pub fn top(app: &App, t: &Theme, tab: Tab, w: u16, targets: &mut Vec<Target>) -> Page {
    let mut tabs = vec![t.n("  ")];
    let pending = app.pending_updates();
    for each in Tab::ALL {
        let extra = if each == Tab::Updates && pending > 0 { format!(" {pending}") } else { String::new() };
        if each == tab {
            let style = if t.is_mono() {
                Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::new().fg(t.accent).bg(t.raised).add_modifier(Modifier::BOLD)
            };
            tabs.push(Span::styled(format!("  {}{extra}  ", each.label()), style));
        } else {
            tabs.push(t.n(format!("  {}", each.label())));
            if !extra.is_empty() {
                tabs.push(t.d(extra));
            }
            tabs.push(t.n("  "));
        }
        tabs.push(t.n(" "));
        targets.push(Target::text(each.label(), Click::Tab(each)));
    }
    let row = if w >= 100 {
        Item::Row(Line::from(tabs), Line::from(vec![t.d("monokulo-nodo"), t.n(MARGIN)]))
    } else {
        Item::Text(Line::from(tabs))
    };
    Page::new(Line::default(), Line::default()).plain(vec![row, Item::Blank])
}

/// Paints the tab strip and the keys line with the bar surface. Cards and fields carry their own.
pub fn paint_strips(buffer: &mut Buffer, area: Rect, t: &Theme, footer_rows: u16) {
    let mut paint = |rows: std::ops::Range<u16>| {
        for y in rows {
            for x in area.x..area.x + area.width {
                let cell = &mut buffer[(x, y)];
                if cell.bg == t.ground {
                    cell.set_bg(t.bar);
                }
            }
        }
    };
    paint(area.y..area.y + STRIP_ROWS.min(area.height));
    paint(area.y + area.height.saturating_sub(footer_rows)..area.y + area.height);
}

/// A tick box that reads without colour.
pub fn tick(t: &Theme, on_: bool) -> Span<'static> {
    if on_ {
        t.a("[x]")
    } else {
        t.d("[ ]")
    }
}
