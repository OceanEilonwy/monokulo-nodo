//! Mouse support: what each screen lets a click do, and where those things are on screen.
//!
//! Screens name their click targets by the text they draw (a tab's label, a row's name, a
//! button's label) in the order they appear. After drawing, each target is found in the buffer at
//! or after the previous one, so the rectangle that is hit is always exactly what was drawn, at
//! any terminal size and however the layout wrapped. crossterm reports left clicks, drags and
//! the scroll wheel, never hover, so nothing depends on hovering.

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
    /// Moves the screen's selection or focus to `index`, then presses `then`, if any.
    Pick { index: usize, then: Option<KeyCode> },
}

/// How much of the screen around the found text counts as the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// Just the text.
    Text,
    /// The whole width of the row the text is on: list rows and choices.
    Row,
    /// The text with the cells around it and the rows above and below: buttons, whose rounded
    /// caps and padding are part of what one aims at.
    Button,
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

    pub fn button(find: impl Into<String>, click: Click) -> Self {
        Self { find: find.into(), area: Area::Button, click, rows_below: 0 }
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
        let mut found = None;
        for y in cursor.0..area.y + area.height {
            let (chars, columns) = &rows[usize::from(y - area.y)];
            let from = if y == cursor.0 { cursor.1 } else { 0 };
            if let Some(start) = find_in_row(chars, &needle, from) {
                let x0 = columns[start];
                let end = start + needle.len() - 1;
                let x1 = columns[end] + 1;
                found = Some((y, start + needle.len(), x0, x1));
                break;
            }
        }
        let Some((y, next, x0, x1)) = found else { continue };
        cursor = (y, next);
        let below = target.rows_below.min(area.y + area.height - 1 - y);
        let rect = match target.area {
            Area::Text => Rect::new(x0, y, x1 - x0, 1 + below),
            Area::Row => Rect::new(area.x, y, area.width, 1 + below),
            Area::Button => {
                let left = x0.saturating_sub(3).max(area.x);
                let right = (x1 + 3).min(area.x + area.width);
                let top = y.saturating_sub(1).max(area.y);
                let bottom = (y + 2 + below).min(area.y + area.height);
                Rect::new(left, top, right - left, bottom - top)
            }
        };
        hits.push(Hit { rect, click: target.click.clone() });
    }
    hits
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
/// step lighter, so it reads as lifted without looking selected. Text and accent fills stay as
/// they are. Only drawn when the hovered rectangle is still a target on this frame.
pub fn paint_hover(buffer: &mut Buffer, hits: &[Hit], hover: Option<Rect>, t: &crate::theme::Theme) {
    let Some(rect) = hover else { return };
    if t.is_mono() || !hits.iter().any(|hit| hit.rect == rect) {
        return;
    }
    let area = buffer.area;
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
    for y in rect.y..(rect.y + rect.height).min(area.y + area.height) {
        for x in rect.x..(rect.x + rect.width).min(area.x + area.width) {
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

    #[test]
    fn a_target_not_drawn_is_left_out() {
        let b = buffer(&["only this"]);
        let hits = resolve(&b, b.area, &[Target::text("missing", Click::Key(KeyCode::Esc)), Target::text("this", Click::Key(KeyCode::Enter))]);
        assert_eq!(hits, vec![Hit { rect: Rect::new(5, 0, 4, 1), click: Click::Key(KeyCode::Enter) }]);
    }
}
