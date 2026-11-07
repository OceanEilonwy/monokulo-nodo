//! A small layout engine for the design's vocabulary: panels stacked or in columns, rows with a
//! left and a right part, and a footer that is always visible.
//!
//! It exists so that fitting is measurable. Every wrap and clip is counted in `FitReport`, which
//! lets the tests prove that nothing is lost at 100x32. On smaller terminals panels shrink, the
//! selected line stays in view, and the footer with any confirm buttons stays on screen.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

const COLUMNS_MIN_WIDTH: u16 = 80;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FitReport {
    /// Lines that were cut off, or whole panels dropped.
    pub clipped_lines: usize,
    /// Rows whose right part had to move onto its own line.
    pub broken_rows: usize,
}

impl FitReport {
    pub fn fits(&self) -> bool {
        *self == Self::default()
    }

    fn add(&mut self, other: FitReport) {
        self.clipped_lines += other.clipped_lines;
        self.broken_rows += other.broken_rows;
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    /// Wraps at word boundaries.
    Text(Line<'static>),
    /// Left and right justified. The right part moves to the next line if it cannot fit.
    Row(Line<'static>, Line<'static>),
    /// A full-width highlighted row.
    Selected(Line<'static>, Line<'static>),
    Blank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Accent,
    Error,
}

#[derive(Debug, Clone)]
pub struct Panel {
    pub title: String,
    pub tone: Tone,
    pub items: Vec<Item>,
    pub grow: bool,
}

impl Panel {
    pub fn new(title: impl Into<String>, items: Vec<Item>) -> Self {
        Self { title: title.into(), tone: Tone::Normal, items, grow: false }
    }

    pub fn accent(title: impl Into<String>, items: Vec<Item>) -> Self {
        Self { tone: Tone::Accent, ..Self::new(title, items) }
    }

    pub fn error(title: impl Into<String>, items: Vec<Item>) -> Self {
        Self { tone: Tone::Error, ..Self::new(title, items) }
    }

    pub fn grow(mut self) -> Self {
        self.grow = true;
        self
    }
}

#[derive(Debug, Clone)]
pub enum Section {
    Panel(Panel),
    /// Columns with relative widths, each holding panels stacked top to bottom.
    Columns(Vec<(u16, Vec<Panel>)>),
    /// Unboxed lines between panels.
    Plain(Vec<Item>),
}

#[derive(Debug, Clone)]
pub struct Page {
    pub left: Line<'static>,
    pub right: Line<'static>,
    pub sections: Vec<Section>,
    /// Pinned to the bottom, because this is where the buttons and key hints live.
    pub footer: Vec<Item>,
}

impl Page {
    pub fn new(left: Line<'static>, right: Line<'static>) -> Self {
        Self { left, right, sections: Vec::new(), footer: Vec::new() }
    }

    pub fn panel(mut self, panel: Panel) -> Self {
        self.sections.push(Section::Panel(panel));
        self
    }

    pub fn columns(mut self, columns: Vec<(u16, Vec<Panel>)>) -> Self {
        self.sections.push(Section::Columns(columns));
        self
    }

    pub fn plain(mut self, items: Vec<Item>) -> Self {
        self.sections.push(Section::Plain(items));
        self
    }

    pub fn footer(mut self, item: Item) -> Self {
        self.footer.push(item);
        self
    }
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

/// A physical screen line after wrapping.
struct Physical {
    left: Line<'static>,
    right: Option<Line<'static>>,
    selected: bool,
}

/// Greedy word wrap that keeps span styles and a line's leading indent.
pub fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    if line_width(line) <= width || width == 0 {
        return vec![line.clone()];
    }
    let mut chars: Vec<(char, Style)> = Vec::new();
    for span in &line.spans {
        chars.extend(span.content.chars().map(|c| (c, span.style)));
    }
    let indent = chars.iter().take_while(|(c, _)| *c == ' ').count().min(width / 2);
    let mut lines = Vec::new();
    let mut start = 0;
    let mut first = true;
    while start < chars.len() {
        let prefix = if first { 0 } else { indent };
        let room = width - prefix;
        let mut used = 0;
        let mut end = start;
        let mut last_space = None;
        while end < chars.len() {
            let w = chars[end].0.width().unwrap_or(0);
            if used + w > room {
                break;
            }
            if chars[end].0 == ' ' {
                last_space = Some(end);
            }
            used += w;
            end += 1;
        }
        let at_end = end >= chars.len();
        let split = match (at_end, last_space) {
            (true, _) => end,
            (false, _) if chars[end].0 == ' ' => end,
            (false, Some(space)) if space > start => space,
            // A single word wider than the line is cut. The guard keeps this loop advancing.
            _ => end.max(start + 1),
        };
        lines.push(styled_line(&chars[start..split], prefix));
        start = split;
        while start < chars.len() && chars[start].0 == ' ' {
            start += 1;
        }
        first = false;
    }
    lines
}

fn styled_line(chars: &[(char, Style)], indent: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent)));
    }
    let mut current = String::new();
    let mut style = chars.first().map_or_else(Style::new, |(_, style)| *style);
    for (c, next) in chars {
        if *next != style {
            spans.push(Span::styled(std::mem::take(&mut current), style));
            style = *next;
        }
        current.push(*c);
    }
    if !current.is_empty() {
        spans.push(Span::styled(current, style));
    }
    Line::from(spans)
}

fn physical_lines(items: &[Item], width: usize, report: &mut FitReport) -> (Vec<Physical>, Option<usize>) {
    let mut lines = Vec::new();
    let mut focus = None;
    for item in items {
        match item {
            Item::Blank => lines.push(Physical { left: Line::default(), right: None, selected: false }),
            Item::Text(line) => {
                for wrapped in wrap_line(line, width) {
                    lines.push(Physical { left: wrapped, right: None, selected: false });
                }
            }
            Item::Row(left, right) | Item::Selected(left, right) => {
                let selected = matches!(item, Item::Selected(..));
                if selected {
                    focus = Some(lines.len());
                }
                let right_width = line_width(right);
                if line_width(left) + right_width + usize::from(right_width > 0) <= width {
                    lines.push(Physical { left: left.clone(), right: Some(right.clone()), selected });
                } else {
                    report.broken_rows += 1;
                    for wrapped in wrap_line(left, width) {
                        lines.push(Physical { left: wrapped, right: None, selected });
                    }
                    if right_width > 0 {
                        for wrapped in wrap_line(right, width.saturating_sub(2)) {
                            let mut spans = vec![Span::raw("  ")];
                            spans.extend(wrapped.spans);
                            lines.push(Physical { left: Line::from(spans), right: None, selected });
                        }
                    }
                }
            }
        }
    }
    (lines, focus)
}

fn draw_physical(frame: &mut Frame, area: Rect, t: &Theme, y: u16, line: &Physical) {
    let buf = frame.buffer_mut();
    let row = Rect::new(area.x, y, area.width, 1);
    if line.selected {
        buf.set_style(row, t.selected());
    }
    let restyle = |source: &Line<'static>| -> Line<'static> {
        if line.selected {
            Line::from(
                source
                    .spans
                    .iter()
                    .map(|span| Span::styled(span.content.clone(), t.selected()))
                    .collect::<Vec<_>>(),
            )
        } else {
            source.clone()
        }
    };
    buf.set_line(area.x, y, &restyle(&line.left), area.width);
    if let Some(right) = &line.right {
        let width = line_width(right) as u16;
        if width > 0 && width <= area.width {
            buf.set_line(area.x + area.width - width, y, &restyle(right), width);
        }
    }
}

/// Draws lines into `area`, keeping the focused line visible when they do not all fit.
fn draw_lines(
    frame: &mut Frame,
    area: Rect,
    t: &Theme,
    lines: &[Physical],
    focus: Option<usize>,
    report: &mut FitReport,
) {
    let height = area.height as usize;
    if height == 0 {
        report.clipped_lines += lines.len();
        return;
    }
    let start = match focus {
        Some(focus) if focus >= height => (focus + 1 - height).min(lines.len().saturating_sub(height)),
        _ => 0,
    };
    let hidden_below = lines.len().saturating_sub(start + height);
    let hidden = start + hidden_below;
    report.clipped_lines += hidden;
    let focus_in_last_row = focus.is_some_and(|focus| focus == start + height - 1);
    for (offset, line) in lines.iter().skip(start).take(height).enumerate() {
        let y = area.y + offset as u16;
        // With one row, the content itself is worth more than a note that something is hidden.
        if height > 1 && hidden_below > 0 && offset == height - 1 && !focus_in_last_row {
            // One more line is replaced by the marker itself, so it is included in the count.
            let more = format!("↓ {} more, enlarge the terminal to see them", hidden_below + 1);
            frame.buffer_mut().set_stringn(area.x, y, more, area.width as usize, Style::new().fg(t.dim));
        } else {
            draw_physical(frame, area, t, y, line);
        }
    }
}

fn panel_height(panel: &Panel, width: u16) -> u16 {
    let mut scratch = FitReport::default();
    let inner = width.saturating_sub(4) as usize;
    let (lines, _) = physical_lines(&panel.items, inner, &mut scratch);
    lines.len() as u16 + 2
}

fn draw_panel(frame: &mut Frame, area: Rect, t: &Theme, panel: &Panel, report: &mut FitReport) {
    if area.height < 2 || area.width < 4 {
        report.clipped_lines += panel.items.len().max(1);
        return;
    }
    let title_style = match panel.tone {
        Tone::Normal => t.b(""),
        Tone::Accent => t.a(""),
        Tone::Error => t.e(""),
    }
    .style
    .bg(t.ground);
    let border = match panel.tone {
        Tone::Normal => t.border_style(),
        Tone::Accent => t.border_style().fg(t.accent),
        Tone::Error => t.border_style().fg(t.err),
    };
    let title = Span::styled(format!(" {} ", panel.title), title_style.add_modifier(ratatui::style::Modifier::BOLD));
    Block::new().borders(Borders::ALL).border_style(border).title(title).render(area, frame.buffer_mut());
    let inner = Rect::new(area.x + 2, area.y + 1, area.width - 4, area.height - 2);
    let (lines, focus) = physical_lines(&panel.items, inner.width as usize, report);
    draw_lines(frame, inner, t, &lines, focus, report);
}

/// Shares `total` rows between blocks. Over budget, the tallest shrinks first; under budget, the
/// spare rows go to blocks marked to grow.
fn allocate(desired: &[u16], grow: &[bool], total: u16) -> Vec<u16> {
    let mut heights = desired.to_vec();
    let mut sum: u32 = heights.iter().map(|h| u32::from(*h)).sum();
    while sum > u32::from(total) {
        let Some(tallest) = (0..heights.len()).max_by_key(|&i| heights[i]) else {
            break;
        };
        if heights[tallest] == 0 {
            break;
        }
        // A panel below three rows has no room for content, so it gives up its rows entirely.
        let shrink = if heights[tallest] <= 3 { heights[tallest] } else { 1 };
        heights[tallest] -= shrink;
        sum -= u32::from(shrink);
    }
    let spare = (u32::from(total)).saturating_sub(sum) as u16;
    let growers: Vec<usize> = (0..heights.len()).filter(|&i| grow[i]).collect();
    if !growers.is_empty() {
        let share = spare / growers.len() as u16;
        let mut extra = spare % growers.len() as u16;
        for &i in &growers {
            heights[i] += share + u16::from(extra > 0);
            extra = extra.saturating_sub(1);
        }
    }
    heights
}

fn draw_stack(frame: &mut Frame, area: Rect, t: &Theme, panels: &[Panel], report: &mut FitReport) {
    let desired: Vec<u16> = panels.iter().map(|panel| panel_height(panel, area.width)).collect();
    let grow: Vec<bool> = panels.iter().map(|panel| panel.grow).collect();
    let heights = allocate(&desired, &grow, area.height);
    let mut y = area.y;
    for (panel, height) in panels.iter().zip(heights) {
        draw_panel(frame, Rect::new(area.x, y, area.width, height), t, panel, report);
        y += height;
    }
}

fn column_widths(columns: &[(u16, Vec<Panel>)], width: u16) -> Vec<u16> {
    let total: u32 = columns.iter().map(|(weight, _)| u32::from(*weight)).sum();
    let mut widths: Vec<u16> =
        columns.iter().map(|(weight, _)| (u32::from(*weight) * u32::from(width) / total.max(1)) as u16).collect();
    let used: u16 = widths.iter().sum();
    if let Some(last) = widths.last_mut() {
        *last += width - used;
    }
    widths
}

fn flatten(sections: Vec<Section>, width: u16) -> Vec<Section> {
    if width >= COLUMNS_MIN_WIDTH {
        return sections;
    }
    // Columns stop being readable when narrow, so they become one stack.
    sections
        .into_iter()
        .flat_map(|section| match section {
            Section::Columns(columns) => columns
                .into_iter()
                .flat_map(|(_, panels)| panels)
                .map(Section::Panel)
                .collect::<Vec<_>>(),
            other => vec![other],
        })
        .collect()
}

fn section_height(section: &Section, width: u16) -> u16 {
    match section {
        Section::Panel(panel) => panel_height(panel, width),
        Section::Columns(columns) => column_widths(columns, width)
            .into_iter()
            .zip(columns)
            .map(|(column_width, (_, panels))| {
                panels.iter().map(|panel| panel_height(panel, column_width)).sum::<u16>()
            })
            .max()
            .unwrap_or(0),
        Section::Plain(items) => {
            let mut scratch = FitReport::default();
            physical_lines(items, width as usize, &mut scratch).0.len() as u16
        }
    }
}

fn section_grows(section: &Section) -> bool {
    match section {
        Section::Panel(panel) => panel.grow,
        Section::Columns(columns) => {
            columns.iter().any(|(_, panels)| panels.iter().any(|panel| panel.grow))
        }
        Section::Plain(_) => false,
    }
}

/// Draws a whole page and reports what did not fit.
pub fn draw_page(frame: &mut Frame, area: Rect, t: &Theme, page: Page) -> FitReport {
    let mut report = FitReport::default();
    let header = vec![Item::Row(page.left, page.right)];
    let (header_lines, _) = physical_lines(&header, area.width as usize, &mut report);
    let (footer_lines, _) = physical_lines(&page.footer, area.width as usize, &mut report);
    // The header can only take a third of the screen, whatever wraps. The footer gets what it
    // asks for, since it holds the buttons and the keys that make a screen usable.
    let header_height = (header_lines.len() as u16).min(area.height / 3);
    let footer_height = (footer_lines.len() as u16).min(area.height - header_height);
    let header_area = Rect::new(area.x, area.y, area.width, header_height);
    draw_lines(frame, header_area, t, &header_lines, None, &mut report);
    let footer_y = area.y + area.height - footer_height;
    draw_lines(frame, Rect::new(area.x, footer_y, area.width, footer_height), t, &footer_lines, None, &mut report);

    let body_height = area.height - header_height - footer_height;
    let sections = flatten(page.sections, area.width);
    let desired: Vec<u16> = sections.iter().map(|section| section_height(section, area.width)).collect();
    let grow: Vec<bool> = sections.iter().map(section_grows).collect();
    let heights = allocate(&desired, &grow, body_height);
    let mut y = area.y + header_height;
    for (section, height) in sections.iter().zip(heights) {
        let block = Rect::new(area.x, y, area.width, height);
        match section {
            Section::Panel(panel) => draw_panel(frame, block, t, panel, &mut report),
            Section::Columns(columns) => {
                let mut x = area.x;
                for (column_width, (_, panels)) in column_widths(columns, area.width).into_iter().zip(columns) {
                    draw_stack(frame, Rect::new(x, y, column_width, height), t, panels, &mut report);
                    x += column_width;
                }
            }
            Section::Plain(items) => {
                let mut local = FitReport::default();
                let (lines, focus) = physical_lines(items, block.width as usize, &mut local);
                draw_lines(frame, block, t, &lines, focus, &mut local);
                report.add(local);
            }
        }
        y += height;
    }
    report
}

/// A centred box over the page, for overlays and modals.
pub fn draw_boxed(frame: &mut Frame, area: Rect, t: &Theme, title: &str, accent: bool, items: Vec<Item>) {
    let width = (area.width.saturating_sub(8)).clamp(20, 76).min(area.width);
    let mut report = FitReport::default();
    let (lines, _) = physical_lines(&items, width.saturating_sub(4) as usize, &mut report);
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    Clear.render(rect, frame.buffer_mut());
    frame.buffer_mut().set_style(rect, t.base());
    let panel = Panel { title: title.to_owned(), tone: if accent { Tone::Accent } else { Tone::Normal }, items, grow: false };
    draw_panel(frame, rect, t, &panel, &mut report);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn wrap_breaks_on_words_and_keeps_indent() {
        let line = Line::from("  alpha beta gamma delta");
        let wrapped = wrap_line(&line, 12);
        let texts: Vec<String> = wrapped.iter().map(text).collect();
        assert_eq!(texts, ["  alpha beta", "  gamma", "  delta"]);
    }

    #[test]
    fn wrap_terminates_on_a_word_wider_than_the_line() {
        let wrapped = wrap_line(&Line::from("abcdefghij"), 4);
        let texts: Vec<String> = wrapped.iter().map(text).collect();
        assert_eq!(texts, ["abcd", "efgh", "ij"]);
    }

    #[test]
    fn allocate_shrinks_the_tallest_and_grows_marked_panels() {
        assert_eq!(allocate(&[10, 4], &[false, false], 12), [8, 4]);
        assert_eq!(allocate(&[4, 4], &[false, true], 10), [4, 6]);
    }
}
