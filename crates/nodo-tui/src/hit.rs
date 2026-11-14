//! Mouse support: what each screen lets a click do, and where those things are on screen.
//!
//! Screens name their click targets by the text they draw (a tab's label, a row's name, a
//! button's label) in the order they appear. After drawing, each target is found in the buffer at
//! or after the previous one, so the rectangle that is hit is always exactly what was drawn, at
//! any terminal size and however the layout wrapped. crossterm reports left clicks, drags and
//! the scroll wheel and pointer moves; a move over a target draws it with a hover fill, which only
//! ever reinforces what the keyboard can already reach.

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::Tab;

/// What a click does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Click {
    Tab(Tab),
    /// The same as pressing this key on the current screen.
    Key(KeyCode),
    /// The same as pressing Ctrl and this key: actions that must not reach a text box, such as
    /// saving a form while a field is being typed in.
    Ctrl(char),
    /// Moves the screen's selection or focus to `index`, then presses `then`, if any.
    Pick { index: usize, then: Option<KeyCode> },
    /// Puts the cursor in the screen's text box without pressing anything. A box's own key (`/`
    /// for a search) would be typed into it once the box has the cursor.
    Focus,
}

/// How much of the screen around the found text counts as the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// Just the text.
    Text,
    /// The whole width of the row the text is on: list rows and choices.
    Row,
    /// A button as `v3::button` draws it: the text centred in `width` cells, with a rounded cap
    /// or a cell of padding on each side. One row, exactly the drawn button.
    Button { width: u16 },
    /// The width of a card (the page less its margins) on the text's row: rows inside a card,
    /// so the hover fill stays on the card.
    Card,
    /// The text and the cells after it up to this many columns from its start: a text box's
    /// label with the box drawn beside it.
    Field(u16),
}

/// A click target as a screen declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub find: String,
    pub area: Area,
    pub click: Click,
    /// Extra rows below the found one that also belong to it, such as a choice's description.
    pub rows_below: u16,
}

impl Target {
    pub fn text(find: impl Into<String>, click: Click) -> Self {
        Self { find: find.into(), area: Area::Text, click, rows_below: 0 }
    }

    pub fn row(find: impl Into<String>, click: Click) -> Self {
        Self { find: find.into(), area: Area::Row, click, rows_below: 0 }
    }

    /// A button just wide enough for its text and a cell each side, such as the modal's. Buttons
    /// drawn wider say how wide with `drawn`.
    pub fn button(find: impl Into<String>, click: Click) -> Self {
        let find = find.into();
        let width = find.chars().count() as u16;
        Self { find, area: Area::Button { width }, click, rows_below: 0 }
    }

    /// The `width` a button was drawn with (`v3::button`'s), so its target is the whole button.
    pub fn drawn(mut self, width: usize) -> Self {
        if let Area::Button { width: w } = &mut self.area {
            *w = width as u16;
        }
        self
    }

    pub fn card_row(find: impl Into<String>, click: Click) -> Self {
        Self { find: find.into(), area: Area::Card, click, rows_below: 0 }
    }

    /// A text box: its label, found by `find`, and the box beside it, `cells` wide in all.
    pub fn field(find: impl Into<String>, cells: u16) -> Self {
        Self { find: find.into(), area: Area::Field(cells), click: Click::Focus, rows_below: 0 }
    }

    pub fn below(mut self, rows: u16) -> Self {
        self.rows_below = rows;
        self
    }
}

/// A target resolved to where it was drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub rect: Rect,
    pub click: Click,
    /// A button's label, as found: buttons get their own hover, and a key that presses one marks
    /// it as pending (`App::pending`).
    pub button: Option<String>,
}

/// The text of one buffer row with the column each character starts at. Wide characters take two
/// cells; the cell after one is skipped.
fn row_text(buffer: &Buffer, y: u16, area: Rect) -> (Vec<char>, Vec<u16>) {
    let mut chars = Vec::new();
    let mut columns = Vec::new();
    let mut x = area.x;
    while x < area.x + area.width {
        let symbol = buffer[(x, y)].symbol();
        let width = unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
        for c in symbol.chars() {
            chars.push(c);
            columns.push(x);
        }
        if symbol.is_empty() {
            chars.push(' ');
            columns.push(x);
        }
        x += width;
    }
    (chars, columns)
}

fn find_in_row(chars: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || chars.len() < needle.len() {
        return None;
    }
    (from..=chars.len() - needle.len()).find(|&start| chars[start..start + needle.len()] == *needle)
}

/// Finds each target in the buffer, in order, each at or after the one before, and returns the
/// rectangles. A target that is not on screen (scrolled away or too small a terminal) is left out.
pub fn resolve(buffer: &Buffer, area: Rect, targets: &[Target]) -> Vec<Hit> {
    let mut hits = Vec::new();
    // (row, index in that row's chars) after the previous match.
    let mut cursor: (u16, usize) = (area.y, 0);
    let rows: Vec<(Vec<char>, Vec<u16>)> = (area.y..area.y + area.height).map(|y| row_text(buffer, y, area)).collect();
    for target in targets {
        let needle: Vec<char> = target.find.chars().collect();
        let search = |button: Option<u16>| {
            for y in cursor.0..area.y + area.height {
                let (chars, columns) = &rows[usize::from(y - area.y)];
                let mut from = if y == cursor.0 { cursor.1 } else { 0 };
                while let Some(start) = find_in_row(chars, &needle, from) {
                    let x0 = columns[start];
                    let x1 = columns[start + needle.len() - 1] + 1;
                    if button.is_none_or(|width| drawn_as_button(buffer, area, y, x0, x1, button_span(area, x0, x1, needle.len(), width))) {
                        return Some((y, start + needle.len(), x0, x1));
                    }
                    from = start + 1;
                }
            }
            None
        };
        // A button's label can also be in the text before it ("Unlock your Nodo" above Unlock), so
        // the first place it is drawn as a button wins; failing that, the first place at all.
        let found = match target.area {
            Area::Button { width } => search(Some(width)).or_else(|| search(None)),
            _ => search(None),
        };
        let Some((y, next, x0, x1)) = found else { continue };
        cursor = (y, next);
        let below = target.rows_below.min(area.y + area.height - 1 - y);
        let rect = match target.area {
            Area::Text => Rect::new(x0, y, x1 - x0, 1 + below),
            Area::Row => Rect::new(area.x, y, area.width, 1 + below),
            Area::Field(cells) => {
                let right = x0.saturating_add(cells).max(x1).min(area.x + area.width);
                Rect::new(x0, y, right - x0, 1 + below)
            }
            Area::Card => {
                let margin = (crate::view::v3::MARGIN.len() as u16).min(area.width / 4);
                Rect::new(area.x + margin, y, area.width - 2 * margin, 1 + below)
            }
            Area::Button { width } => {
                let (left, right) = button_span(area, x0, x1, needle.len(), width);
                Rect::new(left, y, right - left, 1 + below)
            }
        };
        let button = matches!(target.area, Area::Button { .. }).then(|| target.find.clone());
        hits.push(Hit { rect, click: target.click.clone(), button });
    }
    hits
}

/// The columns a button covers, from where its label was found: the label centred as
/// `v3::centred` does it in `width` cells, with one cap or padding cell each side.
fn button_span(area: Rect, x0: u16, x1: u16, len: usize, width: u16) -> (u16, u16) {
    let spare = width.saturating_sub(len as u16);
    let left = x0.saturating_sub(1 + spare / 2).max(area.x);
    let right = (x1 + 1 + spare - spare / 2).min(area.x + area.width);
    (left, right)
}

/// Whether text found at `x0..x1` on row `y` is a button's label: padded with blanks to the
/// button's width, and set apart from what is beside it, by a fill of its own (with or without
/// caps) or, without colour, by brackets.
fn drawn_as_button(buffer: &Buffer, area: Rect, y: u16, x0: u16, x1: u16, (left, right): (u16, u16)) -> bool {
    let blank = |x: u16| buffer[(x, y)].symbol() == " ";
    if !(left + 1..x0).all(blank) || !(x1..right.saturating_sub(1)).all(blank) {
        return false;
    }
    let fill = buffer[(x0, y)].bg;
    let apart = |outside: Option<u16>| outside.is_none_or(|x| buffer[(x, y)].bg != fill);
    let bracketed = buffer[(left, y)].symbol() == "[" && buffer[(right - 1, y)].symbol() == "]";
    bracketed || (apart(left.checked_sub(1).filter(|x| *x >= area.x)) && apart(Some(right).filter(|x| *x < area.x + area.width)))
}

/// The target at a cell. Where targets overlap (a button's padding over a row), the smallest wins.
pub fn hit_at(hits: &[Hit], column: u16, row: u16) -> Option<Hit> {
    let contains = |hit: &&Hit| {
        column >= hit.rect.x && column < hit.rect.x + hit.rect.width && row >= hit.rect.y && row < hit.rect.y + hit.rect.height
    };
    hits.iter().filter(contains).min_by_key(|hit| u32::from(hit.rect.width) * u32::from(hit.rect.height)).cloned()
}

/// The click at a cell.
pub fn at(hits: &[Hit], column: u16, row: u16) -> Option<Click> {
    hit_at(hits, column, row).map(|hit| hit.click)
}

/// Draws the hover fill over the target under the pointer: every surface in its rectangle one
/// step lighter, so it reads as lifted without looking selected. A button lifts its own fill
/// instead (accent, raised or danger, with its caps), so the button itself reacts. Only drawn
/// when the hovered rectangle is still a target on this frame.
pub fn paint_hover(buffer: &mut Buffer, hits: &[Hit], hover: Option<Rect>, t: &crate::theme::Theme) {
    let Some(rect) = hover else { return };
    let Some(hit) = hits.iter().find(|hit| hit.rect == rect) else { return };
    if t.is_mono() {
        return;
    }
    let area = buffer.area;
    let rows = rect.y..(rect.y + rect.height).min(area.y + area.height);
    let columns = rect.x..(rect.x + rect.width).min(area.x + area.width);
    let fills = [t.accent, t.raised, t.danger];
    let is_cap = |symbol: &str| symbol == crate::view::v3::CAP_LEFT || symbol == crate::view::v3::CAP_RIGHT;
    if hit.button.is_some() && rows.clone().any(|y| columns.clone().any(|x| fills.contains(&buffer[(x, y)].bg))) {
        for y in rows {
            for x in columns.clone() {
                let cell = &mut buffer[(x, y)];
                if fills.contains(&cell.bg) {
                    cell.set_bg(t.lift(cell.bg));
                } else if is_cap(cell.symbol()) && fills.contains(&cell.fg) {
                    cell.set_fg(t.lift(cell.fg));
                }
            }
        }
        return;
    }
    let lighter = |bg: ratatui::style::Color| {
        if bg == t.ground || bg == t.field {
            Some(t.bar)
        } else if bg == t.bar {
            Some(t.card)
        } else if bg == t.card {
            Some(t.raised)
        } else {
            None
        }
    };
    for y in rows {
        for x in columns.clone() {
            let cell = &mut buffer[(x, y)];
            if let Some(bg) = lighter(cell.bg) {
                cell.set_bg(bg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(lines: &[&str]) -> Buffer {
        Buffer::with_lines(lines.iter().copied())
    }

    #[test]
    fn targets_resolve_in_order_so_repeated_words_hit_the_right_place() {
        let b = buffer(&["  Tor        Tor link  ", "  [x] Tor              ", "     ( Save )          "]);
        let area = b.area;
        let hits = resolve(
            &b,
            area,
            &[
                Target::text("Tor link", Click::Key(KeyCode::Char('a'))),
                Target::row("Tor", Click::Pick { index: 1, then: Some(KeyCode::Char(' ')) }),
                Target::button("Save", Click::Key(KeyCode::Enter)),
            ],
        );
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].rect, Rect::new(13, 0, 8, 1));
        assert_eq!(hits[1].rect, Rect::new(0, 1, 23, 1), "the second Tor is the row after the link");
        assert_eq!(at(&hits, 7, 2), Some(Click::Key(KeyCode::Enter)));
        assert_eq!(at(&hits, 1, 1), Some(Click::Pick { index: 1, then: Some(KeyCode::Char(' ')) }));
        assert_eq!(at(&hits, 22, 0), None);
    }

    /// A row with `spans` drawn from column `x`, on a card surface.
    fn drawn(t: &crate::theme::Theme, x: u16, spans: Vec<ratatui::text::Span<'static>>) -> Buffer {
        let mut b = Buffer::empty(Rect::new(0, 0, 40, 3));
        b.set_style(b.area, ratatui::style::Style::new().bg(t.card));
        b.set_line(x, 1, &ratatui::text::Line::from(spans), 40 - x);
        b
    }

    #[test]
    fn a_button_target_is_exactly_the_drawn_button() {
        let plain = crate::theme::Theme::truecolor();
        let rounded = plain.with_rounded(true);
        for t in [plain, rounded, crate::theme::Theme::mono()] {
            // Wider than its label, as when the buttons in a row share the longest one's width.
            for (label, width) in [("Add a provider", 20), ("Save", 20), ("Save", 10), ("Remove", 6)] {
                let spans = crate::view::v3::button(&t, label, true, width, t.card);
                let cells = crate::view::v3::width_of(&spans) as u16;
                let b = drawn(&t, 5, spans);
                let hits = resolve(&b, b.area, &[Target::button(label, Click::Key(KeyCode::Enter)).drawn(width)]);
                assert_eq!(hits[0].rect, Rect::new(5, 1, cells, 1), "{label} at {width}, rounded {}", t.rounded);
                assert_eq!(hits[0].button.as_deref(), Some(label));
            }
        }
    }

    #[test]
    fn hovering_a_button_lifts_its_own_fill_and_caps() {
        let t = crate::theme::Theme::truecolor().with_rounded(true);
        for (label, primary, fill) in [("Add a provider", true, t.accent), ("Back", false, t.raised), ("!Remove", true, t.danger)] {
            let mut b = drawn(&t, 5, crate::view::v3::button(&t, label, primary, 20, t.card));
            let hits = resolve(&b, b.area, &[Target::button(label.trim_start_matches('!'), Click::Key(KeyCode::Enter)).drawn(20)]);
            let rect = hits[0].rect;
            paint_hover(&mut b, &hits, Some(rect), &t);
            assert_eq!(b[(rect.x, 1)].fg, t.lift(fill), "{label}: the left cap follows the fill");
            assert_eq!(b[(rect.x, 1)].bg, t.card, "{label}: the cap's corner stays the card");
            assert_eq!(b[(rect.x + 1, 1)].bg, t.lift(fill), "{label}: the fill is lighter");
            assert_eq!(b[(rect.x + rect.width - 1, 1)].fg, t.lift(fill));
            assert_eq!(b[(rect.x + rect.width, 1)].bg, t.card, "{label}: nothing beside the button changes");
            assert_eq!(b[(rect.x + 1, 0)].bg, t.card, "{label}: nor above it");
        }
    }

    #[test]
    fn a_target_not_drawn_is_left_out() {
        let b = buffer(&["only this"]);
        let hits = resolve(&b, b.area, &[Target::text("missing", Click::Key(KeyCode::Esc)), Target::text("this", Click::Key(KeyCode::Enter))]);
        assert_eq!(hits, vec![Hit { rect: Rect::new(5, 0, 4, 1), click: Click::Key(KeyCode::Enter), button: None }]);
    }

    #[test]
    fn a_field_takes_its_label_and_the_box_beside_it() {
        let b = buffer(&["  Search   [ monero      ]   Sources ›"]);
        let hits = resolve(&b, b.area, &[Target::field("Search", 23), Target::text("Sources ›", Click::Key(KeyCode::Char('s')))]);
        assert_eq!(hits[0].rect, Rect::new(2, 0, 23, 1));
        assert_eq!(at(&hits, 15, 0), Some(Click::Focus), "a click inside the box focuses it");
        assert_eq!(at(&hits, 30, 0), Some(Click::Key(KeyCode::Char('s'))));
    }
}
