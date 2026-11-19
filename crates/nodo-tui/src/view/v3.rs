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
use crate::app::App;
use crate::hit::{Click, Target};
use crate::theme::Theme;
use nodo_ui_core::screen::Tab;
use nodo_ui_core::words::fingerprint_groups;

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
    /// Where what the focus is on ends, when it is more than its first row: a settings field's
    /// control and help, which fitting keeps in view with its label.
    focus_end: Option<usize>,
    surface: Color,
    /// Click targets with the row each is drawn on. Targets are found by their text, so one on a
    /// row that fitting cuts must go too: left in, it finds the same text further down (every
    /// field's "+ reference") and takes the targets after it with it.
    targets: Vec<(usize, Target)>,
}

impl Card {
    pub fn new(t: &Theme, page_width: u16) -> Self {
        let width = usize::from(page_width).saturating_sub(2 * MARGIN.len());
        Card { width, rows: Vec::new(), buttons_at: None, focus_at: None, focus_end: None, surface: t.card, targets: Vec::new() }
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
    /// Each button is a click target, tied to its row.
    pub fn buttons(&mut self, t: &Theme, list: &[(&str, bool, Click)]) {
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
            let (mut spans, mut targets) = (Vec::new(), Vec::new());
            for (index, ((label, primary, click), width)) in ordered.iter().zip(&widths).enumerate() {
                if index > 0 {
                    spans.push(t.n("   "));
                }
                spans.extend(button_target(t, label, *primary, *width, self.surface, click, &mut targets));
            }
            let used = width_of(&spans);
            let mut out = vec![t.n(" ".repeat(room.saturating_sub(used)))];
            out.extend(spans);
            self.row_with(t, out, targets);
        } else {
            for (index, (label, primary, click)) in ordered.iter().enumerate() {
                if index > 0 {
                    self.blank(t);
                }
                let mut targets = Vec::new();
                let spans = button_target(t, label, *primary, equal.min(room.saturating_sub(2)), self.surface, click, &mut targets);
                let used = width_of(&spans);
                let mut out = vec![t.n(" ".repeat(room.saturating_sub(used)))];
                out.extend(spans);
                self.row_with(t, out, targets);
            }
        }
        self.blank(t);
    }

    /// A right-aligned note right above the card's buttons, such as why Save can't be used yet.
    /// Fitting a short terminal keeps it with the buttons.
    pub fn note_then_buttons(&mut self, t: &Theme, note: Vec<Span<'static>>, list: &[(&str, bool, Click)]) {
        let at = self.rows.len();
        self.row_end(t, Vec::new(), note);
        self.buttons(t, list);
        self.buttons_at = Some(at);
    }

    /// One button at a given indent inside the card, for a button that belongs to a part of the
    /// card rather than to the whole of it.
    pub fn inline_button(&mut self, t: &Theme, label: &str, indent: usize, click: Click) {
        let width = label.chars().count() + 6;
        let (mut out, mut targets) = (vec![t.n(" ".repeat(indent))], Vec::new());
        out.extend(button_target(t, label, true, width, self.surface, &click, &mut targets));
        self.row_with(t, out, targets);
        if self.buttons_at.is_none() {
            self.buttons_at = Some(self.rows.len() - 1);
        }
    }

    /// A row with one button at its right end, on the card's inner padding. When both do not fit
    /// (80 columns), the text keeps its row and the button gets its own, still right-aligned.
    /// The button is tied to whichever row it ends up on; targets for the text go on the row
    /// first (`on_row(next_row(), …)`), so they stay in drawing order.
    pub fn row_with_button(&mut self, t: &Theme, mut spans: Vec<Span<'static>>, label: &str, primary: bool, click: Click) {
        let width = label.trim_start_matches('!').chars().count() + 6;
        let mut targets = Vec::new();
        let button = button_target(t, label, primary, width, self.surface, &click, &mut targets);
        let room = self.width.saturating_sub(6);
        let used = width_of(&spans) + width_of(&button);
        if used + 2 > room {
            self.row(t, spans);
            let mut own = vec![t.n(" ".repeat(room.saturating_sub(width_of(&button))))];
            own.extend(button);
            self.row_with(t, own, targets);
            return;
        }
        spans.push(t.n(" ".repeat(room.saturating_sub(used))));
        spans.extend(button);
        self.row_with(t, spans, targets);
    }

    /// The width rows have up to the card's inner right padding, where right-aligned parts end.
    pub fn inner(&self) -> usize {
        self.width.saturating_sub(6)
    }

    /// A row with `end` right-aligned on the card's inner padding. When both do not fit, they are
    /// kept two cells apart and the row wraps as any other.
    pub fn row_end(&mut self, t: &Theme, spans: Vec<Span<'static>>, end: Vec<Span<'static>>) {
        self.row_end_with(t, spans, end, []);
    }

    /// `row_end` with click targets, tied as `row_with` ties them.
    pub fn row_end_with(&mut self, t: &Theme, mut spans: Vec<Span<'static>>, end: Vec<Span<'static>>, targets: impl IntoIterator<Item = Target>) {
        let used = width_of(&spans) + width_of(&end);
        spans.push(t.n(" ".repeat(self.inner().saturating_sub(used).max(2))));
        spans.extend(end);
        self.row_with(t, spans, targets);
    }

    /// A clickable row: the row, and a target over the card's width on it, found by `find`.
    pub fn link_row(&mut self, t: &Theme, spans: Vec<Span<'static>>, find: &str, click: Click) {
        self.row_with(t, spans, [Target::card_row(find, click)]);
    }

    /// A row with click targets on it, each tied to the row the wrapping put its text on, so
    /// it goes when that row is cut or scrolled away and is found nowhere else.
    pub fn row_with(&mut self, t: &Theme, spans: Vec<Span<'static>>, targets: impl IntoIterator<Item = Target>) {
        let start = self.rows.len();
        self.row(t, spans);
        for target in targets {
            let at = (start..self.rows.len()).find(|&row| item_text(&self.rows[row]).contains(&target.find)).unwrap_or(start);
            self.targets.push((at, target));
        }
    }

    /// A row of text with a link after it, such as "Try again": on one row when both fit, else
    /// the link on a row of its own. Wrapped with the text, a link of two words was split over
    /// two rows and was on neither as a whole, so it could not be clicked.
    pub fn row_link(&mut self, t: &Theme, mut spans: Vec<Span<'static>>, link: &str, click: Click) {
        let target = Target::text(link, click);
        if width_of(&spans) + 3 + unicode_width::UnicodeWidthStr::width(link) <= self.width.saturating_sub(4) {
            spans.extend([t.n("   "), t.a(link.to_owned())]);
            self.row_with(t, spans, [target]);
        } else {
            self.row(t, spans);
            self.row_with(t, vec![t.n("   "), t.a(link.to_owned())], [target]);
        }
    }

    /// The first row whose text has `text` in it.
    pub fn row_showing(&self, text: &str) -> Option<usize> {
        self.rows.iter().position(|item| item_text(item).contains(text))
    }

    /// The index the next row gets, for tying targets to it (`on_row`).
    pub fn next_row(&self) -> usize {
        self.rows.len()
    }

    /// Ties click targets to the row they are drawn on, so they go when fitting cuts that row.
    pub fn on_row(&mut self, row: usize, targets: impl IntoIterator<Item = Target>) {
        self.targets.extend(targets.into_iter().map(|target| (row, target)));
    }

    /// Marks the next row as the one the keyboard focus is on, so fitting keeps it in view.
    pub fn focus_here(&mut self) {
        self.focus_at = Some(self.rows.len());
    }

    /// Marks the rows since `focus_here` as all belonging to the focus, so fitting shows them
    /// together. With only the label kept in view, the field's value, on the row under it at
    /// narrow widths, was cut and the owner could not see what they were editing.
    pub fn focus_ends_here(&mut self) {
        self.focus_end = Some(self.rows.len());
    }

    /// The last row fitting must keep in view to show the focus, keeping it within `room` rows
    /// so the focus row itself always stays.
    fn focus_last(&self, room: usize) -> Option<usize> {
        let focus = self.focus_at?;
        let end = self.focus_end.filter(|end| *end > focus).unwrap_or(focus + 1);
        Some(end.min(focus + room.max(1)) - 1)
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
        let start = match self.focus_last(head) {
            Some(last) if last >= head => (last + 1 - head).min(body - head),
            _ => 0,
        };
        let tail_rows: Vec<Item> = self.rows.split_off(body);
        let kept: Vec<Item> = self.rows.drain(start..start + head).collect();
        self.rows = kept;
        // The kept rows move up to the top, and the tail comes after the "more" row.
        self.targets = std::mem::take(&mut self.targets)
            .into_iter()
            .filter_map(|(row, target)| match row {
                _ if row >= body => Some((row - body + head + 1, target)),
                _ if (start..start + head).contains(&row) => Some((row - start, target)),
                _ => None,
            })
            .collect();
        let at = self.rows.len();
        let arrow = if start > 0 { "↕" } else { "↓" };
        self.row(t, vec![t.d(format!("{arrow} {hidden} more — scroll or make the window taller"))]);
        self.buttons_at = self.buttons_at.map(|_| at + 1);
        self.rows.extend(tail_rows);
    }

    pub fn items(self) -> Vec<Item> {
        self.rows
    }

    /// The rows and the targets tied to them, with each target's row, in drawing order. Each
    /// target is found only on its row's text, wherever the row ends up on screen.
    pub fn into_parts(self) -> (Vec<Item>, Vec<(usize, Target)>) {
        let mut targets: Vec<(usize, Target)> = self
            .targets
            .into_iter()
            .filter_map(|(row, target)| self.rows.get(row).map(|item| (row, target.on_line(item_text(item)))))
            .collect();
        // Stable, so targets on one row keep the left-to-right order they were tied in.
        targets.sort_by_key(|(row, _)| *row);
        (self.rows, targets)
    }

    /// The row the keyboard focus is on, if any.
    pub fn focus_row(&self) -> Option<usize> {
        self.focus_at
    }

    /// The rows the focus takes, from its row, when it was marked as more than one.
    pub fn focus_rows(&self) -> usize {
        match (self.focus_at, self.focus_end) {
            (Some(at), Some(end)) if end > at => end - at,
            _ => 1,
        }
    }
}

/// A card row's text, trimmed, as the screen shows it.
fn item_text(item: &Item) -> String {
    match item {
        Item::Text(line) => line.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim().to_owned(),
        _ => String::new(),
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

/// The hourglass that marks running work, turning every half second.
pub fn hourglass(ticks: u64) -> &'static str {
    if (ticks / (crate::app::TICKS_PER_SECOND / 2).max(1)) % 2 == 0 { "⧗" } else { "⧖" }
}

/// A pressed button whose work is running: the hourglass, turning every half second, in place of
/// the focus mark before its label, on a dim raised fill. As wide as the button it replaces.
pub fn pending_button(t: &Theme, label: &str, width: usize, under: Color, ticks: u64) -> Vec<Span<'static>> {
    let bare = label.trim_start_matches('!');
    let bare = bare.strip_prefix("▸ ").unwrap_or(bare);
    let text = format!("{} {bare}", hourglass(ticks));
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
    /// Rows the notices above the keys take (`super::notes`).
    notes: usize,
}

impl Screen3 {
    pub fn new(app: &App, t: &Theme, tab: Tab, w: u16) -> Self {
        let mut targets = Vec::new();
        let page = top(app, t, tab, w, &mut targets);
        let notes = rows_of(&super::notes(app, t), w);
        Self { page, before: Vec::new(), after: Vec::new(), targets, notes }
    }

    /// Rows below the page's content: the notices and the keys, as they wrap at `w`. Counted as
    /// one row, a keys line that wraps at 60 columns pushed the card's buttons off the screen.
    pub(crate) fn bottom_rows(&self, t: &Theme, keys: &str, w: u16) -> usize {
        self.notes + rows_of(&[keys_line(t, keys)], w)
    }

    /// Fits the card into the rows left and finishes the page. Targets are in drawing order: the
    /// tabs and what comes before the card (`self.targets`), the card's own (tied to its rows, so
    /// those of rows fitting cut go too), then `after_targets`.
    pub fn finish(self, t: &Theme, card: Card, after_targets: Vec<Target>, keys: &str, w: u16, h: u16) -> Page {
        let mut page = self.card(t, card, keys, w, h);
        page.targets.extend(after_targets);
        page
    }
}

/// The tab strip: an empty row, the tabs on the page margin with room between them, the current
/// one a flat filled rectangle padded two cells each side, and an empty row. Each tab is a click
/// target.
pub fn top(app: &App, t: &Theme, tab: Tab, w: u16, targets: &mut Vec<Target>) -> Page {
    let mut tabs = vec![t.n("  ")];
    for each in Tab::ALL {
        let count = nodo_ui_core::words::tab_badge(&app.data, each);
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

/// Cells between a settings field's left column and its control.
const SETTING_GAP: usize = 3;

/// A settings field: the label with its help or error wrapped under it in the left column, and
/// the control right-aligned on the card's inner padding. A label too long to sit beside the
/// control keeps its row whole (click targets find it by its text) and the control takes the
/// next row, so the two never overlap; so does a wide control that would leave the help a
/// column too narrow to read (80 columns).
pub fn setting(card: &mut Card, t: &Theme, label: &str, control: Vec<Span<'static>>, hint: Option<Span<'static>>) {
    setting_aside(card, t, label, control, Vec::new(), hint);
}

/// Where `setting_aside` drew a field's label and its aside, for tying their click targets.
pub struct SettingRows {
    pub label: usize,
    /// The label's text on each of its rows, from `label`: one, unless it is wider than the card.
    pub label_lines: Vec<String>,
    pub aside: Option<usize>,
}

/// A settings field with `aside`, such as a link, right-aligned under its control: on the help's
/// first row when both fit there, otherwise on a row of its own under the control.
pub fn setting_aside(card: &mut Card, t: &Theme, label: &str, control: Vec<Span<'static>>, aside: Vec<Span<'static>>, hint: Option<Span<'static>>) -> SettingRows {
    let room = card.inner();
    // The help lines up with the label's text, after the focus marker's column.
    let indent = label.chars().take_while(|c| matches!(c, ' ' | '▸')).count();
    let (mark, bare): (String, &str) = (label.chars().take(indent).collect(), label.trim_start_matches([' ', '▸']));
    let mut rows = SettingRows { label: card.next_row(), label_lines: vec![bare.to_owned()], aside: None };
    let left = room.saturating_sub(width_of(&control) + SETTING_GAP);
    let beside = unicode_width::UnicodeWidthStr::width(label) <= left && left >= room * 2 / 5;
    if beside {
        card.row_end(t, vec![t.n(label.to_owned())], control);
    } else {
        // A label wider than the card (labels may be 80 bytes) wraps under itself, after the
        // marker, and says what is on each row: clicks find a label by its text, and the whole
        // label is on no one row.
        let lines = super::layout::wrap_line(&Line::from(bare.to_owned()), room.saturating_sub(indent).max(12));
        rows.label_lines = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        for (index, text) in rows.label_lines.iter().enumerate() {
            let lead = if index == 0 { mark.clone() } else { " ".repeat(indent) };
            card.row(t, vec![t.n(format!("{lead}{text}"))]);
        }
        card.row_end(t, Vec::new(), control);
    }
    // Beside a control the help keeps to the left column, one measure for every field so the
    // form reads as a column; under a control it has the card's width.
    let column = if beside { left.min(room * 3 / 5) } else { room };
    let help: Vec<Vec<Span<'static>>> = hint
        .map(|hint| {
            super::layout::wrap_line(&Line::from(hint), column.saturating_sub(indent).max(12))
                .into_iter()
                .map(|wrapped| {
                    let mut spans = vec![t.n(" ".repeat(indent))];
                    spans.extend(wrapped.spans);
                    spans
                })
                .collect()
        })
        .unwrap_or_default();
    let mut aside = (!aside.is_empty()).then_some(aside);
    let first_fits = |first: &[Span<'static>], aside: &[Span<'static>]| width_of(first) + SETTING_GAP + width_of(aside) <= room;
    if let Some(side) = aside.take_if(|side| help.first().is_none_or(|first| !first_fits(first, side))) {
        rows.aside = Some(card.next_row());
        card.row_end(t, Vec::new(), side);
    }
    for (index, spans) in help.into_iter().enumerate() {
        match aside.take().filter(|_| index == 0) {
            Some(side) => {
                rows.aside = Some(card.next_row());
                card.row_end(t, spans, side);
            }
            None => card.row(t, spans),
        }
    }
    card.blank(t);
    rows
}

impl Screen3 {
    /// Several cards stacked with a blank row between them. When they do not all fit, the cards
    /// that would fall off the bottom are left out and a dim "↓ N more — scroll" row takes their
    /// place inside the list, a row clear of the keys. Targets are in drawing order: what comes
    /// before, the cards, what comes after; the caller pushes them that way. Targets tied to a
    /// card's rows (`Card::on_row`) follow `self.targets` and are dropped with rows scrolled away.
    pub fn stack(self, t: &Theme, cards: Vec<Card>, keys: &str, w: u16, h: u16) -> Page {
        let avail = usize::from(h).saturating_sub(usize::from(STRIP_ROWS) + self.bottom_rows(t, keys, w) + rows_of(&self.before, w));
        let total = cards.len();
        let mut rows: Vec<(usize, Item)> = Vec::new();
        let mut card_targets: Vec<(usize, Target)> = Vec::new();
        let mut focus: Option<(usize, usize)> = None;
        for (index, card) in cards.into_iter().enumerate() {
            if index > 0 {
                rows.push((index, Item::Blank));
            }
            if let Some(at) = card.focus_row() {
                focus = Some((rows.len() + at, card.focus_rows()));
            }
            let (card_rows, targets) = card.into_parts();
            let offset = rows.len();
            card_targets.extend(targets.into_iter().map(|(row, target)| (offset + row, target)));
            rows.extend(card_rows.into_iter().map(|item| (index, item)));
        }
        let mut items = self.before;
        let mut shown_rows = 0..rows.len();
        if rows.len() + rows_of(&self.after, w) <= avail {
            items.extend(rows.into_iter().map(|(_, item)| item));
            items.extend(self.after);
        } else {
            let visible = avail.saturating_sub(3).max(1);
            // The window scrolls with the keyboard focus, keeping a row of context under it.
            let start = focus
                .map_or(0, |(f, span)| (f + span.min(visible.saturating_sub(1)).max(1) + 1).saturating_sub(visible))
                .min(rows.len().saturating_sub(visible));
            shown_rows = start..start + visible;
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
        page.targets.extend(card_targets.into_iter().filter(|(row, _)| shown_rows.contains(row)).map(|(_, target)| target));
        page
    }

    /// One card, fitted: like `finish`, with the targets already in `self.targets` in order and
    /// then those tied to the card's rows that fitting kept.
    pub fn card(self, t: &Theme, mut card: Card, keys: &str, w: u16, h: u16) -> Page {
        let used = usize::from(STRIP_ROWS) + self.bottom_rows(t, keys, w) + rows_of(&self.before, w) + rows_of(&self.after, w);
        card.fit(t, usize::from(h).saturating_sub(used));
        let (rows, card_targets) = card.into_parts();
        let mut items = self.before;
        items.extend(rows);
        items.extend(self.after);
        let mut page = self.page.plain(items).footer(keys_line(t, keys));
        page.targets = self.targets;
        page.targets.extend(card_targets.into_iter().map(|(_, target)| target));
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
    // The old page's targets go on the card row that shows their text, so they scroll with it;
    // one not on a card row stays a plain target after the cards.
    let mut loose = Vec::new();
    for target in page.targets {
        match cards.iter().enumerate().find_map(|(index, card)| card.row_showing(&target.find).map(|row| (index, row))) {
            Some((index, row)) => cards[index].on_row(row, [target]),
            None => loose.push(target),
        }
    }
    let mut out = screen.stack(t, cards, &last, w, h);
    out.targets.extend(loose);
    out
}
