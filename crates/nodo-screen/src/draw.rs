//! Drawing the screen in the terminal panels design (H3): boxed panels with their title in the
//! border, large numbers, block-character bars and sparklines, on pure black.

use std::time::Instant;

use nodo_panels::{age, Status};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use crate::app::{button_spans, cells_rect, grid_area, App, Mode, BUTTONS};
use crate::bigtext;

/// The screen's colours: the TUI's, on black so that unlit OLED pixels stay off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub ground: Color,
    pub fg: Color,
    pub bright: Color,
    pub dim: Color,
    pub accent: Color,
    pub ok: Color,
    pub err: Color,
    pub info: Color,
    pub border: Color,
    pub track: Color,
}

/// (red, green, blue) for each colour, in the order of the console palette entries 0 to 7.
pub const PALETTE: [(u8, u8, u8); 8] = [
    (0x00, 0x00, 0x00), // 0 ground
    (0xFF, 0x8A, 0x7A), // 1 err
    (0x6F, 0xD6, 0xB3), // 2 ok
    (0xF2, 0xA3, 0x3A), // 3 accent
    (0x86, 0xBE, 0xFF), // 4 info
    (0x3A, 0x40, 0x45), // 5 border and bar track
    (0x80, 0x88, 0x8E), // 6 dim
    (0xD7, 0xDA, 0xDC), // 7 fg
];

impl Theme {
    /// For a terminal with true colour.
    pub fn truecolor() -> Self {
        let c = |i: usize| Color::Rgb(PALETTE[i].0, PALETTE[i].1, PALETTE[i].2);
        Theme {
            ground: c(0),
            fg: c(7),
            bright: Color::Rgb(0xF4, 0xF5, 0xF6),
            dim: c(6),
            accent: c(3),
            ok: c(2),
            err: c(1),
            info: c(4),
            border: c(5),
            track: Color::Rgb(0x2A, 0x2F, 0x33),
        }
    }

    /// For the Linux console with a 512-glyph font, which leaves eight colours. The program sets
    /// entries 0 to 7 to [`PALETTE`] when it starts ([`palette_sequence`]).
    pub fn console() -> Self {
        let c = |i: u8| Color::Indexed(i);
        Theme {
            ground: c(0),
            fg: c(7),
            bright: c(7),
            dim: c(6),
            accent: c(3),
            ok: c(2),
            err: c(1),
            info: c(4),
            border: c(5),
            track: c(5),
        }
    }

    fn status(&self, status: Option<Status>) -> Color {
        match status {
            Some(Status::Ok) => self.ok,
            Some(Status::Warn) => self.accent,
            Some(Status::Error) => self.err,
            Some(Status::Idle) => self.dim,
            Some(Status::Busy) | None => self.info,
        }
    }
}

/// The Linux console's escape for setting palette entries: `ESC ] P n rrggbb`.
pub fn palette_sequence() -> String {
    PALETTE.iter().enumerate().map(|(i, (r, g, b))| format!("\x1b]P{i:X}{r:02X}{g:02X}{b:02X}")).collect()
}

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// One row of block characters for `values`, the newest on the right, scaled to their own range.
pub fn sparkline(values: &[f64], width: usize) -> String {
    let values = &values[values.len().saturating_sub(width)..];
    let (lo, hi) = values.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    values
        .iter()
        .map(|v| {
            let level = if hi > lo { ((v - lo) / (hi - lo) * 7.0).round() as usize } else { 3 };
            SPARK[level.min(7)]
        })
        .collect()
}

/// Draws the whole screen.
pub fn draw(app: &App, area: Rect, buf: &mut Buffer, theme: &Theme, host: &str, clock: &str, now: Instant) {
    buf.set_style(area, Style::default().bg(theme.ground).fg(theme.fg));
    // Top bar: the device and the time, as in the design.
    buf.set_string(area.x + 1, area.y, "●", Style::default().fg(theme.accent));
    buf.set_string(area.x + 3, area.y, host, Style::default().fg(theme.fg));
    let clock_x = (area.x + area.width).saturating_sub(clock.chars().count() as u16 + 1);
    buf.set_string(clock_x, area.y, clock, Style::default().fg(theme.dim));

    let grid = grid_area(area);
    for (index, placement) in app.layout.placements.iter().enumerate() {
        let rect = cells_rect(grid, placement.x, placement.y, placement.w, placement.h);
        // A one-cell gutter between panels, as the design spaces its boxes.
        let rect = Rect { x: rect.x, y: rect.y, width: rect.width.saturating_sub(1), height: rect.height.saturating_sub(1) };
        let selected = app.mode == Mode::Edit && app.selected == Some(index);
        draw_panel(app, index, rect, buf, theme, selected, now);
    }
    if app.layout.placements.is_empty() {
        let text = if app.offered.is_empty() { "No panels yet" } else { "Every panel is hidden. Press e or hold the screen to arrange." };
        buf.set_string(grid.x + 2, grid.y + 1, text, Style::default().fg(theme.dim));
    }
    draw_footer(app, area, buf, theme);
}

fn draw_footer(app: &App, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let y = area.y + area.height.saturating_sub(1);
    if app.mode == Mode::View {
        return;
    }
    let key = Style::default().fg(theme.ground).bg(theme.accent).add_modifier(Modifier::BOLD);
    for ((_, label), (_, x, _)) in BUTTONS.iter().zip(button_spans()) {
        buf.set_string(area.x + x, y, label, key);
    }
    let hint_x = area.x + button_spans().last().map_or(0, |(_, _, end)| end + 3);
    let hint = app.message.clone().unwrap_or_else(|| "drag to move · drag a corner to resize · hjkl move  HJKL resize  Tab select".into());
    let style = if app.message.is_some() { Style::default().fg(theme.accent) } else { Style::default().fg(theme.dim) };
    buf.set_stringn(hint_x, y, &hint, area.width.saturating_sub(hint_x) as usize, style);
}

fn draw_panel(app: &App, index: usize, rect: Rect, buf: &mut Buffer, theme: &Theme, selected: bool, now: Instant) {
    if rect.width < 4 || rect.height < 3 {
        return;
    }
    let placement = &app.layout.placements[index];
    let offered = app.offered(&placement.key);
    let state = app.states.get(&placement.key);
    let output = state.and_then(|s| s.last.as_ref());
    let stale = state.is_some_and(|s| s.is_stale());
    let status = output.and_then(|o| o.status);

    let border = if selected {
        theme.accent
    } else {
        match status {
            Some(Status::Warn) => theme.accent,
            Some(Status::Error) => theme.err,
            _ => theme.border,
        }
    };
    let title = offered.map_or(placement.key.panel.as_str(), |o| o.title.as_str());
    let mut spans = vec![Span::styled(format!(" {title} "), Style::default().fg(if selected { theme.accent } else { theme.fg }).add_modifier(Modifier::BOLD))];
    if placement.key.package != nodo_panels::BUILTIN_PACKAGE {
        spans.push(Span::styled(format!("{} ", placement.key.package), Style::default().fg(theme.dim)));
    }
    let block = Block::bordered().border_style(Style::default().fg(border)).title(Line::from(spans));
    let inner = block.inner(rect);
    block.render(rect, buf);
    if selected {
        let size = format!(" {}×{} ", placement.w, placement.h);
        let x = (rect.x + rect.width).saturating_sub(size.chars().count() as u16 + 1);
        buf.set_string(x, rect.y, size, Style::default().fg(theme.ground).bg(theme.accent));
        buf.set_string(rect.x + rect.width - 1, rect.y + rect.height - 1, "◢", Style::default().fg(theme.accent));
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let Some(output) = output else {
        let text = match (state.and_then(|s| s.failure.as_deref()), offered.map(|o| &o.script)) {
            (_, Some(Err(reason))) => reason.clone(),
            (Some(failure), _) => failure.to_string(),
            _ => format!("waiting for {}", placement.key.package),
        };
        let failed = state.is_some_and(|s| s.failure.is_some()) || offered.is_some_and(|o| o.script.is_err());
        let style = Style::default().fg(if failed { theme.err } else { theme.dim });
        wrap(buf, inner, &text, style);
        return;
    };

    let dim_all = stale;
    let fg = if dim_all { theme.dim } else { theme.bright };
    let mut y = inner.y;
    let bottom = inner.y + inner.height;
    let rows_left = |y: u16| bottom.saturating_sub(y);

    // The large value, drawn big when there is room for it.
    if let Some(big) = &output.big {
        let (large, rest) = bigtext::split(big);
        let unit = [rest.trim(), output.unit.as_deref().unwrap_or("")].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
        let big_color = if dim_all { theme.dim } else { match status { Some(Status::Warn) => theme.accent, Some(Status::Error) => theme.err, Some(Status::Idle) => theme.dim, _ => theme.bright } };
        let large_width = bigtext::width(large.trim_end());
        if !large.trim().is_empty() && inner.height >= 5 && large_width < inner.width {
            for (i, line) in bigtext::render(large.trim_end()).iter().enumerate() {
                buf.set_string(inner.x, y + i as u16, line, Style::default().fg(big_color));
            }
            let unit_x = inner.x + large_width + 1;
            buf.set_stringn(unit_x, y + 2, &unit, inner.width.saturating_sub(large_width + 1) as usize, Style::default().fg(theme.dim));
            y += bigtext::HEIGHT;
        } else {
            buf.set_stringn(inner.x, y, big, inner.width as usize, Style::default().fg(big_color).add_modifier(Modifier::BOLD));
            let used = big.chars().count() as u16 + 1;
            if used < inner.width {
                let unit_only = output.unit.as_deref().unwrap_or("");
                buf.set_stringn(inner.x + used, y, unit_only, (inner.width - used) as usize, Style::default().fg(theme.dim));
            }
            y += 1;
        }
    }

    // Keep the last row for the note, the stale age or nothing.
    let footer_row = output.note.is_some() || stale;
    let reserve = u16::from(footer_row);

    if let Some(bar) = output.bar {
        if rows_left(y) > reserve && inner.height >= 4 {
            let filled = (bar * f64::from(inner.width)).round() as usize;
            let full: String = "█".repeat(filled.min(inner.width as usize));
            let rest: String = "█".repeat(inner.width as usize - full.chars().count());
            let color = if dim_all { theme.dim } else { theme.status(status) };
            buf.set_string(inner.x, y, &full, Style::default().fg(color));
            buf.set_string(inner.x + full.chars().count() as u16, y, &rest, Style::default().fg(theme.track));
            y += 1;
        }
    }

    let spark = !output.series.is_empty() && inner.width >= 24;
    let spark_rows = u16::from(spark);
    for (label, value) in &output.rows {
        if rows_left(y) <= reserve + spark_rows || inner.height < 4 {
            break;
        }
        buf.set_stringn(inner.x, y, label, inner.width as usize, Style::default().fg(theme.dim));
        let value_width = value.chars().count() as u16;
        let label_width = label.chars().count() as u16 + 1;
        if value_width + label_width <= inner.width {
            buf.set_string(inner.x + inner.width - value_width, y, value, Style::default().fg(fg));
        }
        y += 1;
    }

    if spark && rows_left(y) > reserve {
        let line = sparkline(&output.series, inner.width as usize);
        let row = bottom - 1 - reserve;
        let color = if dim_all { theme.dim } else { theme.status(status) };
        buf.set_string(inner.x + inner.width - line.chars().count() as u16, row.max(y), &line, Style::default().fg(color));
    }

    if footer_row {
        let text = if stale {
            let since = state.and_then(|s| s.last_ok).map_or_else(String::new, |t| age(now.duration_since(t)));
            format!("stale · {since}")
        } else {
            output.note.clone().unwrap_or_default()
        };
        buf.set_stringn(inner.x, bottom - 1, &text, inner.width as usize, Style::default().fg(theme.dim));
    }
}

/// Wraps a message over the panel's rows, cutting what does not fit.
fn wrap(buf: &mut Buffer, area: Rect, text: &str, style: Style) {
    let width = area.width as usize;
    let mut line = String::new();
    let mut y = area.y;
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            buf.set_stringn(area.x, y, &line, width, style);
            y += 1;
            line.clear();
            if y >= area.y + area.height {
                return;
            }
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if y < area.y + area.height {
        buf.set_stringn(area.x, y, &line, width, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::offered;
    use nodo_panels::{Layout, PanelOutput};
    use std::time::Duration;

    fn text(buf: &Buffer) -> String {
        let area = buf.area;
        (area.y..area.y + area.height)
            .map(|y| (area.x..area.x + area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn screen(app: &App) -> String {
        let area = Rect::new(0, 0, 120, 33);
        let mut buf = Buffer::empty(area);
        draw(app, area, &mut buf, &Theme::truecolor(), "monokulo-nodo", "thu 13 nov 14:32", Instant::now());
        text(&buf)
    }

    fn sync_output() -> PanelOutput {
        PanelOutput {
            big: Some("61%".into()),
            unit: Some("synced".into()),
            bar: Some(0.61),
            rows: vec![("height".into(), "2121400 / 3478000".into()), ("peers".into(), "32 out".into())],
            series: vec![10.0, 20.0, 30.0, 61.0],
            status: Some(Status::Busy),
            ..PanelOutput::default()
        }
    }

    fn app_with(output: Option<PanelOutput>, failure: Option<&str>) -> App {
        let mut node = offered("cuprated", [2, 1], [4, 2], [4, 2]);
        node.title = "Monero node".into();
        let offered = vec![node];
        let mut layout = Layout::default();
        layout.place_all(&nodo_panels::size_rules(&offered));
        let now = Instant::now();
        let mut app = App::new(offered, layout, now);
        let key = app.layout.placements[0].key.clone();
        let state = app.states.get_mut(&key).unwrap();
        if let Some(output) = output {
            state.record(Ok(output), now, Duration::from_secs(10));
        }
        if let Some(failure) = failure {
            state.record(Err(failure.into()), now, Duration::from_secs(10));
        }
        app
    }

    #[test]
    fn a_panel_looks_like_the_design() {
        let out = screen(&app_with(Some(sync_output()), None));
        assert!(out.contains("● monokulo-nodo"), "{out}");
        assert!(out.contains("thu 13 nov 14:32"));
        assert!(out.contains("┌ Monero node cuprated ─"), "title set into the border:\n{out}");
        assert!(out.contains("█▀▀ ▄█  ▀ █"), "61% drawn large:\n{out}");
        assert!(out.contains("synced"));
        assert!(out.contains("height") && out.contains("2121400 / 3478000"));
        assert!(out.contains("▁▂▄█"), "sparkline:\n{out}");
        assert!(!out.contains("[ done ]"), "no footer while viewing");
    }

    #[test]
    fn waiting_stale_and_failed_panels_say_so() {
        assert!(screen(&app_with(None, None)).contains("waiting for cuprated"));
        let stale = screen(&app_with(Some(sync_output()), Some("the service timed out")));
        assert!(stale.contains("stale · 0 s"), "{stale}");
        let failed = screen(&app_with(None, Some("the panel returned unknown field 'x'")));
        assert!(failed.contains("the panel returned unknown field 'x'"), "{failed}");
    }

    #[test]
    fn arranging_shows_the_selection_and_buttons() {
        let mut app = app_with(Some(sync_output()), None);
        app.key(crossterm::event::KeyCode::Char('e'), crossterm::event::KeyModifiers::NONE);
        let out = screen(&app);
        assert!(out.contains(" 4×2 "), "size badge:\n{out}");
        assert!(out.contains("[ + add ]") && out.contains("[ x hide ]") && out.contains("[ done ]"));
    }

    #[test]
    fn sparklines_scale_to_their_range() {
        assert_eq!(sparkline(&[1.0, 2.0, 3.0, 8.0], 10), "▁▂▃█");
        assert_eq!(sparkline(&[5.0, 5.0], 10), "▄▄", "flat");
        assert_eq!(sparkline(&[1.0, 2.0, 3.0, 8.0], 2), "▁█", "newest kept, rescaled");
        assert!(palette_sequence().starts_with("\x1b]P0000000\x1b]P1FF8A7A"));
    }
}
