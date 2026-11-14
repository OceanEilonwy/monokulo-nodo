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
        let label_len = |label: &str| label.trim_start_matches('!').chars().count();
        let mut ordered: Vec<&(&str, bool, Click)> = list.iter().filter(|(_, p, _)| !p).collect();
        ordered.extend(list.iter().filter(|(_, p, _)| *p));
        // One width for all, the longest label's; narrower, each its own; and when even that does
        // not fit (three buttons at 80 columns), one button per row, the primary last. A button
        // is never cut by the card's wrapping.
        let equal = ordered.iter().map(|(label, _, _)| label_len(label)).max().unwrap_or(0) + 6;
        let gaps = 3 * (ordered.len() - 1);
        let total_equal = ordered.len() * (equal + 2) + gaps;
        let total_own: usize = ordered.iter().map(|(label, _, _)| label_len(label) + 6 + 2).sum::<usize>() + gaps;
        let widths: Vec<usize> = if total_equal <= room {
            vec![equal; ordered.len()]
        } else {
            ordered.iter().map(|(label, _, _)| label_len(label) + 6).collect()
        };
        if total_equal <= room || total_own <= room {
            let mut spans = Vec::new();
            for (index, ((label, primary, click), width)) in ordered.iter().zip(&widths).enumerate() {
                if index > 0 {
                    spans.push(t.n("   "));
                }
                spans.extend(button_target(t, label, *primary, *width, self.surface, click, targets));
            }
            let used = width_of(&spans);
            let mut out = vec![t.n(" ".repeat(room.saturating_sub(used)))];
            out.extend(spans);
            self.row(t, out);
        } else {
            for (index, (label, primary, click)) in ordered.iter().enumerate() {
                if index > 0 {
                    self.blank(t);
                }
                let spans = button_target(t, label, *primary, equal.min(room.saturating_sub(2)), self.surface, click, targets);
                let used = width_of(&spans);
                let mut out = vec![t.n(" ".repeat(room.saturating_sub(used)))];
                out.extend(spans);
                self.row(t, out);
            }
        }
        self.blank(t);
    }

    /// One button at a given indent inside the card, for a button that belongs to a part of the
    /// card rather than to the whole of it.
    pub fn inline_button(&mut self, t: &Theme, label: &str, indent: usize, click: Click, targets: &mut Vec<Target>) {
        let width = label.chars().count() + 6;
        let mut out = vec![t.n(" ".repeat(indent))];
        out.extend(button_target(t, label, true, width, self.surface, &click, targets));
        self.row(t, out);
        if self.buttons_at.is_none() {
            self.buttons_at = Some(self.rows.len() - 1);
        }
    }

    /// A row with one button at its right end, on the card's inner padding. When both do not fit
    /// (80 columns), the text keeps its row and the button gets its own, still right-aligned.
    pub fn row_with_button(&mut self, t: &Theme, mut spans: Vec<Span<'static>>, label: &str, primary: bool, click: Click, targets: &mut Vec<Target>) {
        let width = label.trim_start_matches('!').chars().count() + 6;
        let button = button_target(t, label, primary, width, self.surface, &click, targets);
        let room = self.width.saturating_sub(6);
        let used = width_of(&spans) + width_of(&button);
        if used + 2 > room {
            self.row(t, spans);
            let mut own = vec![t.n(" ".repeat(room.saturating_sub(width_of(&button))))];
            own.extend(button);
            self.row(t, own);
            return;
        }
        spans.push(t.n(" ".repeat(room.saturating_sub(used))));
        spans.extend(button);
        self.row(t, spans);
    }

    /// A clickable row: the row, and a target over the card's width on it, found by `find`.
    pub fn link_row(&mut self, t: &Theme, spans: Vec<Span<'static>>, find: &str, click: Click, targets: &mut Vec<Target>) {
        self.row(t, spans);
        targets.push(Target::card_row(find, click));
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

    /// The row the keyboard focus is on, if any.
    pub fn focus_row(&self) -> Option<usize> {
        self.focus_at
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

/// The click target for a text box drawn as a label padded to `label_width` and then an
/// `inset` `box_width` wide: the label and the whole box, with the marker a mono theme adds.
pub fn field_target(label: &str, label_width: usize, box_width: usize) -> Target {
    Target::field(label, (label_width + box_width + 3) as u16)
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
    let danger = label.starts_with('!');
    let label = label.trim_start_matches('!');
    if t.is_mono() {
        let text = format!("[{}]", centred(label, width));
        let style = if primary { Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD) } else { Style::new() };
        return vec![Span::styled(text, style)];
    }
    let (colour, ink) = if danger {
        (t.danger, t.err)
    } else if primary {
        (t.accent, t.ground)
    } else {
        (t.raised, t.fg)
    };
    let weight = if primary { Modifier::BOLD } else { Modifier::empty() };
    let fill = Style::new().fg(ink).bg(colour).add_modifier(weight);
    if t.rounded {
        let cap = Style::new().fg(colour).bg(under);
        vec![Span::styled(CAP_LEFT, cap), Span::styled(centred(label, width), fill), Span::styled(CAP_RIGHT, cap)]
    } else {
        vec![Span::styled(centred(label, width + 2), fill)]
    }
}

/// A button as `button` draws it, with its click target over exactly the drawn cells. While the
/// work this button started is still running (`App::pending`), it shows the hourglass in front of
/// its label, dimmed, and has no target, so it cannot be pressed twice.
pub fn button_target(t: &Theme, label: &str, primary: bool, width: usize, under: Color, click: &Click, targets: &mut Vec<Target>) -> Vec<Span<'static>> {
    if let Some((_, ticks)) = super::pending().filter(|(pending, _)| pending == click) {
        return pending_button(t, label, width, under, ticks);
    }
    targets.push(Target::button(label.trim_start_matches('!'), click.clone()).drawn(width));
    button(t, label, primary, width, under)
}

/// A pressed button whose work is running: the hourglass, turning every half second, in place of
/// the focus mark before its label, on a dim raised fill. As wide as the button it replaces.
pub fn pending_button(t: &Theme, label: &str, width: usize, under: Color, ticks: u64) -> Vec<Span<'static>> {
    let bare = label.trim_start_matches('!');
    let bare = bare.strip_prefix("▸ ").unwrap_or(bare);
    let glyph = if (ticks / (crate::app::TICKS_PER_SECOND / 2).max(1)) % 2 == 0 { "⧗" } else { "⧖" };
    let text = format!("{glyph} {bare}");
    if t.is_mono() {
        return vec![Span::styled(format!("[{}]", centred(&text, width)), Style::new().add_modifier(Modifier::DIM))];
    }
    let fill = Style::new().fg(t.dim).bg(t.raised);
    if t.rounded {
        let cap = Style::new().fg(t.raised).bg(under);
        vec![Span::styled(CAP_LEFT, cap), Span::styled(centred(&text, width), fill), Span::styled(CAP_RIGHT, cap)]
    } else {
        vec![Span::styled(centred(&text, width + 2), fill)]
    }
}

/// A button's label with the keyboard focus shown as a ▸ before it, so which button Enter presses
/// is visible in every theme, without colour too.
pub fn focused(label: &str, focus: bool) -> String {
    let danger = label.starts_with('!');
    let bare = label.trim_start_matches('!');
    match (focus, danger) {
        (true, true) => format!("!▸ {bare}"),
        (true, false) => format!("▸ {bare}"),
        (false, _) => label.to_owned(),
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
    let apps = app.data.updates.iter().filter(|u| !u.held && u.name != crate::model::SYSTEM_BASE).count();
    let nodo = app.data.updates.iter().filter(|u| !u.held && u.name == crate::model::SYSTEM_BASE).count();
    for each in Tab::ALL {
        let count = match each {
            Tab::Apps => apps,
            Tab::System => nodo,
            _ => 0,
        };
        let extra = if count > 0 { format!(" {count}") } else { String::new() };
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


/// The breadcrumb row with the page's own actions at its right end, on the content margin. When
/// they do not fit beside the breadcrumb (under 100 columns), they fold into one "⋯ More"
/// button that opens them as a list.
pub fn trail_actions(t: &Theme, parts: &[&str], actions: &[(&str, Click)], more: Click, w: u16, targets: &mut Vec<Target>) -> Item {
    let Item::Text(left) = trail(t, parts, targets) else { unreachable!("trail is one line") };
    let folded = [("⋯ More", more)];
    let shown: &[(&str, Click)] = if w >= 100 { actions } else { &folded };
    let width = shown.iter().map(|(l, _)| l.trim_start_matches('!').chars().count()).max().unwrap_or(0) + 4;
    let mut right = Vec::new();
    for (index, (label, click)) in shown.iter().enumerate() {
        if index > 0 {
            right.push(t.n("  "));
        }
        right.extend(button_target(t, label, false, width, t.ground, click, targets));
    }
    right.push(t.n(MARGIN));
    Item::Row(left, Line::from(right))
}

/// A heading with one button at the right end of its row, on the content margin.
pub fn heading_button(t: &Theme, text: &str, label: &str, click: Click, targets: &mut Vec<Target>) -> Item {
    let width = label.chars().count() + 6;
    let mut right = button_target(t, label, true, width, t.ground, &click, targets);
    right.push(t.n(MARGIN));
    Item::Row(Line::from(vec![margin(t), t.b(text.to_owned())]), Line::from(right))
}

/// A heading or quiet line with a link at the right end of its row, on the content margin.
pub fn heading_link(t: &Theme, left: Span<'static>, link: &str, click: Click, targets: &mut Vec<Target>) -> Item {
    targets.push(Target::text(link, click));
    Item::Row(Line::from(vec![margin(t), left]), Line::from(vec![t.a(link.to_owned()), t.n(MARGIN)]))
}

/// A link on its own line on the content margin.
pub fn link(t: &Theme, text: &str, click: Click, targets: &mut Vec<Target>) -> Item {
    targets.push(Target::text(text, click));
    line(vec![margin(t), t.a(text.to_owned())])
}

/// An on/off box that reads without colour.
pub fn toggle(t: &Theme, on_: bool) -> Vec<Span<'static>> {
    if on_ {
        vec![t.a("[x]"), t.n(" On ")]
    } else {
        vec![t.d("[ ]"), t.d(" Off")]
    }
}

/// A key fingerprint as groups of four, to compare by eye.
pub fn fingerprint_groups(fp: &str) -> String {
    // A "SHA256:" or similar label is not part of the key; its digits must not join the groups.
    let body = fp.rsplit_once(": ").map_or(fp, |(_, rest)| rest);
    let body = body.split_once("256:").map_or(body, |(_, rest)| rest);
    let hex: String = body.chars().filter(char::is_ascii_hexdigit).collect();
    let pieces: Vec<String> = hex.as_bytes().chunks(8).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
    pieces.join("  ")
}

/// A key with its label on one row, the value as groups (highlighted when it is the one to
/// check). When both do not fit, the value goes on the next row under the label, kept whole.
pub fn key_row(card: &mut Card, t: &Theme, label: &str, fp: &str, highlight: bool) {
    let style = if highlight { Style::new().fg(t.accent).bg(t.field).add_modifier(Modifier::BOLD) } else { Style::new().fg(t.fg) };
    let value = Span::styled(format!(" {} ", fingerprint_groups(fp)), style);
    let label = t.d(pad(label, 15));
    if 15 + width_of(std::slice::from_ref(&value)) + 4 <= card.width.saturating_sub(4) {
        card.row(t, vec![label, value]);
    } else {
        card.row(t, vec![label]);
        card.row(t, vec![t.n("  "), value]);
    }
}

/// A progress bar of whole cells: done cells filled with the accent, the rest inset.
pub fn progress(t: &Theme, fraction: f32, width: usize) -> Vec<Span<'static>> {
    let done = ((fraction.clamp(0.0, 1.0)) * width as f32).round() as usize;
    if t.is_mono() {
        return vec![t.n(format!("[{}{}]", "#".repeat(done), "-".repeat(width - done)))];
    }
    vec![Span::styled(" ".repeat(done), Style::new().bg(t.accent)), Span::styled(" ".repeat(width - done), Style::new().bg(t.field))]
}

/// Wi-Fi signal as four bars.
pub fn signal(t: &Theme, bars: usize) -> Vec<Span<'static>> {
    ["▂", "▄", "▆", "█"].iter().enumerate().map(|(i, g)| if i < bars { t.n(*g) } else { t.d("·") }).collect()
}

/// A settings field: the label column, then the control, and a quiet hint or an error under it.
pub fn setting(card: &mut Card, t: &Theme, label: &str, control: Vec<Span<'static>>, hint: Option<Span<'static>>) {
    let mut spans = vec![t.n(pad(label, 24))];
    spans.extend(control);
    card.row(t, spans);
    if let Some(hint) = hint {
        card.row(t, vec![t.n(" ".repeat(24)), hint]);
    }
    card.blank(t);
}

impl Screen3 {
    /// Several cards stacked with a blank row between them. When they do not all fit, the cards
    /// that would fall off the bottom are left out and a dim "↓ N more — scroll" row takes their
    /// place inside the list, a row clear of the keys. Targets are in drawing order: what comes
    /// before, the cards, what comes after; the caller pushes them that way.
    pub fn stack(self, t: &Theme, cards: Vec<Card>, keys: &str, w: u16, h: u16) -> Page {
        let avail = usize::from(h).saturating_sub(usize::from(STRIP_ROWS) + 1 + rows_of(&self.before, w));
        let total = cards.len();
        let mut rows: Vec<(usize, Item)> = Vec::new();
        let mut focus: Option<usize> = None;
        for (index, card) in cards.into_iter().enumerate() {
            if index > 0 {
                rows.push((index, Item::Blank));
            }
            if let Some(at) = card.focus_row() {
                focus = Some(rows.len() + at);
            }
            rows.extend(card.items().into_iter().map(|item| (index, item)));
        }
        let mut items = self.before;
        if rows.len() + rows_of(&self.after, w) <= avail {
            items.extend(rows.into_iter().map(|(_, item)| item));
            items.extend(self.after);
        } else {
            let visible = avail.saturating_sub(3).max(1);
            // The window scrolls with the keyboard focus, keeping a row of context under it.
            let start = focus.map_or(0, |f| (f + 2).saturating_sub(visible)).min(rows.len().saturating_sub(visible));
            let shown: Vec<(usize, Item)> = rows.into_iter().skip(start).collect();
            let seen = shown.iter().take(visible).map(|(card, _)| *card).max().map_or(0, |c| c + 1);
            let more_below = shown.len() > visible;
            items.extend(shown.into_iter().take(visible).map(|(_, item)| item));
            items.push(Item::Blank);
            let arrow = if start > 0 && more_below { "↕" } else if start > 0 { "↑" } else { "↓" };
            let hint = if more_below && total > seen {
                format!("{arrow} {} more — scroll", total - seen)
            } else if more_below {
                format!("{arrow} more below — scroll")
            } else {
                format!("{arrow} more above — scroll")
            };
            items.push(line(vec![margin(t), t.d(hint)]));
            items.push(Item::Blank);
        }
        let mut page = self.page.plain(items).footer(keys_line(t, keys));
        page.targets = self.targets;
        page
    }

    /// One card, fitted: like `finish`, with the targets already in `self.targets` in order.
    pub fn card(self, t: &Theme, mut card: Card, keys: &str, w: u16, h: u16) -> Page {
        let used = usize::from(STRIP_ROWS) + 1 + rows_of(&self.before, w) + rows_of(&self.after, w);
        card.fit(t, usize::from(h).saturating_sub(used));
        let mut items = self.before;
        items.extend(card.items());
        items.extend(self.after);
        let mut page = self.page.plain(items).footer(keys_line(t, keys));
        page.targets = self.targets;
        page
    }
}

/// Puts a page written with bordered panels into the v3 look: the tab strip, the breadcrumb (the
/// page's header, under its tab) with its status at the right, every panel as a card titled in
/// bold, rows kept as two columns, the selected row marked with ▸, and the footer's hints as the
/// one dim line of keys. Click targets are kept, since they are found by text.
pub fn modern(app: &App, t: &Theme, page: Page) -> Page {
    use super::layout::{Section, Tone};
    let (w, h) = super::frame_size();
    let tab = app.screen.area();
    let mut screen = Screen3::new(app, t, tab, w);
    let header: String = page.left.spans.iter().map(|s| s.content.as_ref()).collect();
    let mut parts: Vec<String> = header.split('›').map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect();
    if parts.first().map(String::as_str) == Some("Home") && tab != Tab::Home {
        parts[0] = tab.label().to_owned();
    }
    if parts.first().map(String::as_str) != Some(tab.label()) {
        parts.insert(0, tab.label().to_owned());
    }
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    let mut before_targets = Vec::new();
    let Item::Text(trail_line) = trail(t, &refs, &mut before_targets) else { unreachable!("trail is one line") };
    let mut right = page.right.clone();
    right.spans.push(t.n(MARGIN));
    screen.before = vec![Item::Blank, Item::Row(trail_line, right), Item::Blank];
    screen.targets.extend(before_targets);

    let label_width = 30usize.min(usize::from(w) / 3);
    let mut cards = Vec::new();
    let mut add = |title: Option<(&str, Tone)>, items: &[Item]| {
        let mut card = Card::new(t, w);
        card.blank(t);
        if let Some((title, tone)) = title {
            if !title.is_empty() {
                let lead = match tone {
                    Tone::Error => t.e("▲  "),
                    Tone::Accent => t.a(""),
                    Tone::Normal => t.n(""),
                };
                card.row(t, vec![lead, t.b(title.to_owned())]);
                card.blank(t);
            }
        }
        for item in items {
            match item {
                Item::Blank => card.blank(t),
                Item::Text(line) => card.row(t, line.spans.clone()),
                Item::Row(left, right) | Item::Selected(left, right) => {
                    let selected = matches!(item, Item::Selected(..));
                    let mut spans: Vec<Span<'static>> = left.spans.clone();
                    if selected {
                        card.focus_here();
                        // The ▸ marker the old pages put in place of a leading space, in the accent.
                        if let Some(first) = spans.first_mut() {
                            if let Some(rest) = first.content.strip_prefix('▸') {
                                *first = Span::styled(rest.to_owned(), first.style);
                            }
                        }
                        spans.insert(0, t.a("▸ "));
                    }
                    let used = width_of(&spans);
                    if !right.spans.is_empty() {
                        spans.push(t.n(" ".repeat(label_width.saturating_sub(used).max(2))));
                        spans.extend(right.spans.iter().cloned().map(|s| {
                            let style = s.style;
                            if style.bg.is_some() && !t.is_mono() { s.style(style.bg(t.card)) } else { s }
                        }));
                    }
                    card.row(t, spans);
                }
            }
        }
        card.blank(t);
        cards.push(card);
    };
    for section in &page.sections {
        match section {
            Section::Panel(panel) => add(Some((panel.title.as_str(), panel.tone)), &panel.items),
            Section::Columns(columns) => {
                for (_, panels) in columns {
                    for panel in panels {
                        add(Some((panel.title.as_str(), panel.tone)), &panel.items);
                    }
                }
            }
            Section::Plain(items) => add(None, items),
        }
    }
    let keys: Vec<String> = page
        .footer
        .iter()
        .map(|item| match item {
            Item::Text(line) => line.spans.iter().map(|s| s.content.as_ref()).collect::<String>(),
            Item::Row(left, right) | Item::Selected(left, right) => {
                let l: String = left.spans.iter().map(|s| s.content.as_ref()).collect();
                let r: String = right.spans.iter().map(|s| s.content.as_ref()).collect();
                if l.trim().is_empty() { r } else { format!("{}   {}", l.trim(), r) }
            }
            Item::Blank => String::new(),
        })
        .filter(|line| !line.trim().is_empty())
        .collect();
    // Only the last footer line is keys; earlier ones are notes, which go above the keys.
    let (notes, last) = match keys.split_last() {
        Some((last, notes)) => (notes.to_vec(), format!("Keys: {}", last.trim())),
        None => (Vec::new(), String::new()),
    };
    if !notes.is_empty() {
        screen.after = std::iter::once(Item::Blank).chain(notes.iter().map(|n| quiet(t, n.trim()))).collect();
    }
    let mut out = screen.stack(t, cards, &last, w, h);
    out.targets.extend(page.targets);
    out
}
