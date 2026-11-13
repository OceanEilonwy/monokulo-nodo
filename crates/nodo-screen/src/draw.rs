//! Drawing the screen in the OLED tile design (`docs/design/tui/ScreenTilesOled*.dc.html` and
//! `ScreenShutdown*.dc.html`): design A's tile grid made dark. Tiles are thin rounded outlines on
//! pure black, so that unlit OLED pixels stay off; only the values, bars and outlines are lit.
//!
//! Every glyph drawn here is in the screen's console font, nodo-screen-16x32 (Terminus with the
//! missing ones added by `packages/nodo-tools/files/screen-font/make-font.py`); [`GLYPHS`] lists
//! the added ones and a test keeps the two in step.

use std::time::Instant;

use nodo_panels::layout::{COLUMNS, ROWS};
use nodo_panels::{age, PanelOutput, PanelState, Status};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::app::{cells_rect, grid_area, grip_rect, tile_rect, App, Button, DragKind, Mode, PickerRow, StepState, BANNER_ROWS};
use crate::bigtext;

/// The glyphs this module draws beyond plain ASCII and what Terminus ter-v32n already has. A test
/// checks that make-font.py draws each of them.
#[cfg_attr(not(test), allow(dead_code))]
pub const GLYPHS: &str = "╭╮╰╯┄┆━┃┏┓┗┛▀▄▁▂▃▅▆▇●⇲⏻✓☰▸";

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
    (0x86, 0xBE, 0xFF), // 4 info, and Save
    (0x3A, 0x40, 0x45), // 5 outlines, free spaces and bar tracks
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Rounded,
    Heavy,
    Dashed,
}

/// A box outline in `rect`.
fn outline(buf: &mut Buffer, rect: Rect, edge: Edge, style: Style) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let (tl, tr, bl, br, h, v) = match edge {
        Edge::Rounded => ("╭", "╮", "╰", "╯", "─", "│"),
        Edge::Heavy => ("┏", "┓", "┗", "┛", "━", "┃"),
        Edge::Dashed => ("╭", "╮", "╰", "╯", "┄", "┆"),
    };
    let (right, bottom) = (rect.x + rect.width - 1, rect.y + rect.height - 1);
    for x in rect.x + 1..right {
        buf.set_string(x, rect.y, h, style);
        buf.set_string(x, bottom, h, style);
    }
    for y in rect.y + 1..bottom {
        buf.set_string(rect.x, y, v, style);
        buf.set_string(right, y, v, style);
    }
    buf.set_string(rect.x, rect.y, tl, style);
    buf.set_string(right, rect.y, tr, style);
    buf.set_string(rect.x, bottom, bl, style);
    buf.set_string(right, bottom, br, style);
}

fn centred(buf: &mut Buffer, rect: Rect, y: u16, text: &str, style: Style) {
    let width = text.chars().count() as u16;
    let x = rect.x + rect.width.saturating_sub(width) / 2;
    buf.set_stringn(x, y, text, rect.width as usize, style);
}

#[derive(Debug, Clone, Copy)]
enum Look {
    /// An outline in `edge` with text in `text`.
    Outline { edge: Color, text: Color },
    /// Solid `fill` with black text.
    Filled { fill: Color },
}

/// A button: a box with its label on the middle row.
fn button(buf: &mut Buffer, rect: Rect, label: &str, detail: Option<&str>, look: Look, theme: &Theme) {
    let (label_style, detail_style) = match look {
        Look::Outline { edge, text } => {
            outline(buf, rect, Edge::Rounded, Style::default().fg(edge));
            (Style::default().fg(text).add_modifier(Modifier::BOLD), Style::default().fg(theme.dim))
        }
        Look::Filled { fill } => {
            buf.set_style(rect, Style::default().bg(fill));
            outline(buf, rect, Edge::Rounded, Style::default().fg(fill).bg(fill));
            let on = Style::default().fg(theme.ground).bg(fill);
            (on.add_modifier(Modifier::BOLD), on)
        }
    };
    let inner = Rect { x: rect.x + 1, width: rect.width.saturating_sub(2), ..rect };
    match detail {
        Some(detail) if rect.height >= 4 => {
            let top = rect.y + (rect.height - 2) / 2;
            centred(buf, inner, top, label, label_style);
            centred(buf, inner, top + 1, detail, detail_style);
        }
        _ => centred(buf, inner, rect.y + rect.height / 2, label, label_style),
    }
}

/// Draws the whole screen.
pub fn draw(app: &App, area: Rect, buf: &mut Buffer, theme: &Theme, host: &str, clock: &str, now: Instant) {
    buf.set_style(area, Style::default().bg(theme.ground).fg(theme.fg));
    match app.mode {
        Mode::Confirm => return draw_confirm(app, area, buf, theme),
        Mode::ShuttingDown => return draw_shutdown(app, area, buf, theme),
        Mode::Picker => return draw_picker(app, area, buf, theme),
        Mode::View | Mode::Edit => {}
    }
    draw_banner(app, area, buf, theme, host, clock, now);
    let grid = grid_area(area);
    if app.mode == Mode::Edit {
        draw_arranging(app, grid, buf, theme);
    } else {
        for (index, placement) in app.layout.placements.iter().enumerate() {
            let rect = tile_rect(cells_rect(grid, placement.x, placement.y, placement.w, placement.h));
            draw_tile(app, index, rect, buf, theme, now);
        }
    }
    if app.layout.placements.is_empty() {
        let text = if app.offered.is_empty() {
            "No panels yet"
        } else if app.mode == Mode::Edit {
            "Every panel is hidden: tap + Add panel"
        } else {
            "Every panel is hidden. Tap ☰ Arrange to add some."
        };
        buf.set_string(grid.x + 2, grid.y + 1, text, Style::default().fg(theme.dim));
    }
}

fn draw_banner(app: &App, area: Rect, buf: &mut Buffer, theme: &Theme, host: &str, clock: &str, now: Instant) {
    let y = area.y + 1;
    let buttons = app.banner_buttons(area);
    let first_x = buttons.first().map_or(area.x + area.width, |(_, r)| r.x);
    if app.mode == Mode::View {
        buf.set_string(area.x + 1, y, host, Style::default().fg(theme.fg).add_modifier(Modifier::BOLD));
        let clock_x = first_x.saturating_sub(clock.chars().count() as u16 + 3);
        buf.set_string(clock_x, y, clock, Style::default().fg(theme.fg));
    } else {
        buf.set_string(area.x + 1, y, " Arrange ", Style::default().fg(theme.ground).bg(theme.accent).add_modifier(Modifier::BOLD));
        let (text, color) = match &app.message {
            Some(message) => (message.as_str(), theme.accent),
            None if app.drag.as_ref().is_some_and(|d| d.moved) => ("let go to put it there", theme.dim),
            None if app.undo.is_some() => ("hidden: Undo brings it back", theme.dim),
            None if app.selected.is_some() => ("drag a tile to move it · drag ⇲ to resize", theme.dim),
            None => ("tap a tile, then drag it", theme.dim),
        };
        let room = first_x.saturating_sub(area.x + 12 + 2);
        buf.set_stringn(area.x + 12, y, text, room as usize, Style::default().fg(color));
    }
    for (which, rect) in buttons {
        let outline_fg = Look::Outline { edge: theme.fg, text: theme.fg };
        match which {
            Button::Arrange => button(buf, rect, "☰ Arrange", None, Look::Outline { edge: theme.border, text: theme.fg }, theme),
            Button::ShutDown => button(buf, rect, "⏻ Shut down", None, Look::Outline { edge: theme.err, text: theme.err }, theme),
            Button::Hide => {
                let look = if app.selected.is_some() { outline_fg } else { Look::Outline { edge: theme.track, text: theme.border } };
                button(buf, rect, "Hide", None, look, theme);
            }
            Button::Undo => {
                let hidden = app.undo.as_ref();
                let title = hidden.map_or_else(String::new, |h| app.title_of(&h.placement.key));
                let left = hidden.map_or(0, |h| h.until.saturating_duration_since(now).as_secs_f64().ceil() as u64);
                button(buf, rect, &format!("Undo: show {title} · {left} s"), None, Look::Filled { fill: theme.accent }, theme);
            }
            Button::Add => {
                let n = app.hidden().len();
                let look = if n > 0 { outline_fg } else { Look::Outline { edge: theme.track, text: theme.border } };
                button(buf, rect, &format!("+ Add panel {n}"), None, look, theme);
            }
            Button::Save => button(buf, rect, "Save", None, Look::Filled { fill: theme.info }, theme),
            Button::Cancel => button(buf, rect, "Cancel", None, outline_fg, theme),
            Button::KeepRunning | Button::ConfirmShutDown | Button::Back => {}
        }
    }
}

/// Arranging: free cells as grey dashed boxes, tiles as outlines with their name and size, the
/// selected tile in orange with its grip, and a drag's landing place.
fn draw_arranging(app: &App, grid: Rect, buf: &mut Buffer, theme: &Theme) {
    let drag = app.drag.as_ref().filter(|d| d.moved);
    let moving = drag.filter(|d| matches!(d.kind, DragKind::Move { .. })).map(|d| d.index);
    for y in 0..ROWS {
        for x in 0..COLUMNS {
            let covered = app.layout.at(x, y).is_some_and(|i| Some(i) != moving);
            if covered {
                continue;
            }
            let color = if app.add_at == Some((x, y)) { theme.dim } else { theme.track };
            outline(buf, tile_rect(cells_rect(grid, x, y, 1, 1)), Edge::Dashed, Style::default().fg(color));
        }
    }
    for (index, p) in app.layout.placements.iter().enumerate() {
        if Some(index) == moving {
            continue;
        }
        let rect = tile_rect(cells_rect(grid, p.x, p.y, p.w, p.h));
        draw_tile_arranging(app, index, rect, buf, theme);
    }
    let Some(drag) = drag else { return };
    if let Some(target) = &drag.target {
        let rect = tile_rect(cells_rect(grid, target.x, target.y, target.w, target.h));
        outline(buf, rect, Edge::Dashed, Style::default().fg(theme.accent));
        if drag.kind == DragKind::Resize {
            centred(buf, rect, rect.y + rect.height / 2, &format!("{}×{}", target.w, target.h), Style::default().fg(theme.accent).add_modifier(Modifier::BOLD));
        }
    }
    // The tile itself follows the finger, as a dim outline.
    if let (DragKind::Move { column, row, .. }, Some(p)) = (drag.kind, app.layout.placements.get(drag.index)) {
        let size = tile_rect(cells_rect(grid, p.x, p.y, p.w, p.h));
        let x = drag.finger.0.saturating_sub(column).clamp(grid.x, (grid.x + grid.width).saturating_sub(size.width));
        let y = drag.finger.1.saturating_sub(row).clamp(grid.y, (grid.y + grid.height).saturating_sub(size.height));
        let ghost = Rect { x, y, ..size };
        for gy in ghost.y..ghost.y + ghost.height {
            for gx in ghost.x..ghost.x + ghost.width {
                buf.set_string(gx, gy, " ", Style::default().bg(theme.ground));
            }
        }
        outline(buf, ghost, Edge::Rounded, Style::default().fg(theme.dim));
        let title = app.title_of(&p.key).to_uppercase();
        buf.set_stringn(ghost.x + 2, ghost.y + 1, &title, ghost.width.saturating_sub(4) as usize, Style::default().fg(theme.fg).add_modifier(Modifier::BOLD));
        buf.set_string(ghost.x + 2, ghost.y + 2, format!("{}×{}", p.w, p.h), Style::default().fg(theme.dim));
    }
}

fn draw_tile_arranging(app: &App, index: usize, rect: Rect, buf: &mut Buffer, theme: &Theme) {
    if rect.width < 4 || rect.height < 3 {
        return;
    }
    let p = &app.layout.placements[index];
    let selected = app.selected == Some(index);
    let title = app.title_of(&p.key).to_uppercase();
    let room = rect.width.saturating_sub(4) as usize;
    if !selected {
        outline(buf, rect, Edge::Rounded, Style::default().fg(theme.border));
        buf.set_stringn(rect.x + 2, rect.y + 1, &title, room, Style::default().fg(theme.fg).add_modifier(Modifier::BOLD));
        buf.set_stringn(rect.x + 2, rect.y + 2, format!("{}×{}", p.w, p.h), room, Style::default().fg(theme.dim));
        return;
    }
    let accent = Style::default().fg(theme.accent);
    outline(buf, rect, Edge::Heavy, accent);
    buf.set_stringn(rect.x + 2, rect.y + 1, &title, room, accent.add_modifier(Modifier::BOLD));
    let range = app.rule_of(index).map_or_else(String::new, |r| format!(" · can be {}×{} to {}×{}", r.min[0], r.min[1], r.max[0], r.max[1]));
    buf.set_stringn(rect.x + 2, rect.y + 2, format!("{}×{}{range}", p.w, p.h), room, accent);
    let grip = grip_rect(rect);
    if rect.height >= 5 {
        buf.set_stringn(rect.x + 2, rect.y + rect.height - 2, "drag to move", (grip.x.saturating_sub(rect.x + 3)) as usize, Style::default().fg(theme.dim));
    }
    buf.set_style(grip, Style::default().bg(theme.accent).fg(theme.ground));
    for y in grip.y..grip.y + grip.height {
        for x in grip.x..grip.x + grip.width {
            buf.set_string(x, y, " ", Style::default().bg(theme.accent));
        }
    }
    centred(buf, grip, grip.y + grip.height / 2, "⇲", Style::default().fg(theme.ground).bg(theme.accent).add_modifier(Modifier::BOLD));
}

fn tile_outline_color(theme: &Theme, status: Option<Status>) -> Color {
    match status {
        Some(Status::Warn) => theme.accent,
        Some(Status::Error) => theme.err,
        _ => theme.border,
    }
}

fn draw_tile(app: &App, index: usize, rect: Rect, buf: &mut Buffer, theme: &Theme, now: Instant) {
    if rect.width < 6 || rect.height < 3 {
        return;
    }
    let placement = &app.layout.placements[index];
    let offered = app.offered(&placement.key);
    let state = app.states.get(&placement.key);
    let output = state.and_then(|s| s.last.as_ref());
    let stale = state.is_some_and(|s| s.is_stale());
    let failed = state.is_some_and(|s| s.failure.is_some()) || offered.is_some_and(|o| o.script.is_err());
    let status = output.and_then(|o| o.status);

    outline(buf, rect, Edge::Rounded, Style::default().fg(tile_outline_color(theme, status)));
    // The package's name, small, at the bottom right inside the outline, as the design labels tiles.
    let inner = Rect { x: rect.x + 2, y: rect.y + 1, width: rect.width.saturating_sub(4), height: rect.height.saturating_sub(2) };
    let mut body_bottom = inner.y + inner.height;
    if placement.key.package != nodo_panels::BUILTIN_PACKAGE && inner.height >= 4 {
        let name = &placement.key.package;
        let x = (inner.x + inner.width).saturating_sub(name.chars().count() as u16);
        buf.set_stringn(x.max(inner.x), body_bottom - 1, name, inner.width as usize, Style::default().fg(theme.dim));
        body_bottom -= 1;
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Title row: the tile's name in capitals, dim, and a status dot on the right.
    let title = app.title_of(&placement.key).to_uppercase();
    buf.set_stringn(inner.x, inner.y, &title, inner.width.saturating_sub(2) as usize, Style::default().fg(theme.dim).add_modifier(Modifier::BOLD));
    let dot = if failed && (output.is_none() || stale) {
        Some(theme.err)
    } else if output.is_some() && stale {
        Some(theme.dim)
    } else {
        output.and_then(|o| o.status).map(|s| theme.status(Some(s)))
    };
    if let Some(color) = dot {
        buf.set_string(inner.x + inner.width - 1, inner.y, "●", Style::default().fg(color));
    }
    let body = Rect { y: inner.y + 1, height: body_bottom.saturating_sub(inner.y + 1), ..inner };
    if body.height == 0 {
        return;
    }
    let Some(output) = output else {
        let text = match (state.and_then(|s| s.failure.as_deref()), offered.map(|o| &o.script)) {
            (_, Some(Err(reason))) => reason.clone(),
            (Some(failure), _) => failure.to_string(),
            _ => format!("waiting for {}", placement.key.package),
        };
        wrap(buf, body, &text, Style::default().fg(if failed { theme.err } else { theme.dim }));
        return;
    };
    draw_values(output, state, body, buf, theme, stale, now);
}

fn draw_values(output: &PanelOutput, state: Option<&PanelState>, body: Rect, buf: &mut Buffer, theme: &Theme, stale: bool, now: Instant) {
    let status = output.status;
    let fg = if stale { theme.dim } else { theme.bright };
    let mut y = body.y;
    let bottom = body.y + body.height;
    let rows_left = |y: u16| bottom.saturating_sub(y);
    // Keep the last row for the note, the stale age or nothing.
    let footer_row = output.note.is_some() || stale;
    let reserve = u16::from(footer_row);

    if let Some(big) = &output.big {
        let (large, rest) = bigtext::split(big);
        let unit = [rest.trim(), output.unit.as_deref().unwrap_or("")].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" ");
        let big_color = if stale {
            theme.dim
        } else {
            match status {
                Some(Status::Warn) => theme.accent,
                Some(Status::Error) => theme.err,
                Some(Status::Idle) => theme.dim,
                _ => theme.bright,
            }
        };
        let large_width = bigtext::width(large.trim_end());
        if !large.trim().is_empty() && rows_left(y) > bigtext::HEIGHT + reserve && large_width < body.width {
            for (i, line) in bigtext::render(large.trim_end()).iter().enumerate() {
                buf.set_string(body.x, y + i as u16, line, Style::default().fg(big_color));
            }
            buf.set_stringn(body.x + large_width + 1, y + 2, &unit, body.width.saturating_sub(large_width + 1) as usize, Style::default().fg(theme.dim));
            y += bigtext::HEIGHT;
        } else {
            buf.set_stringn(body.x, y, big, body.width as usize, Style::default().fg(big_color).add_modifier(Modifier::BOLD));
            let used = big.chars().count() as u16 + 1;
            if used < body.width {
                buf.set_stringn(body.x + used, y, output.unit.as_deref().unwrap_or(""), (body.width - used) as usize, Style::default().fg(theme.dim));
            }
            y += 1;
        }
    }

    // A thin bar, as the design's: lit for the done part, a dark track for the rest.
    if let Some(bar) = output.bar {
        if rows_left(y) > reserve + 1 {
            y += 1;
            let filled = ((bar * f64::from(body.width)).round() as usize).min(body.width as usize);
            let color = if stale { theme.dim } else { theme.status(status) };
            buf.set_string(body.x, y, "━".repeat(filled), Style::default().fg(color));
            buf.set_string(body.x + filled as u16, y, "━".repeat(body.width as usize - filled), Style::default().fg(theme.track));
            y += 1;
        }
    }

    let spark = !output.series.is_empty() && body.width >= 24;
    let spark_rows = u16::from(spark);
    if !output.rows.is_empty() && rows_left(y) > reserve + spark_rows + 1 {
        y += 1;
    }
    for (label, value) in &output.rows {
        if rows_left(y) <= reserve + spark_rows {
            break;
        }
        buf.set_stringn(body.x, y, label, body.width as usize, Style::default().fg(theme.dim));
        let value_width = value.chars().count() as u16;
        if value_width + (label.chars().count() as u16) < body.width {
            buf.set_string(body.x + body.width - value_width, y, value, Style::default().fg(fg));
        }
        y += 1;
    }

    if spark && rows_left(y) > reserve {
        let line = sparkline(&output.series, body.width as usize);
        let row = (bottom - 1 - reserve).max(y);
        let color = if stale { theme.dim } else { theme.status(status) };
        buf.set_string(body.x + body.width - line.chars().count() as u16, row, &line, Style::default().fg(color));
    }

    if footer_row {
        let text = if stale {
            let since = state.and_then(|s| s.last_ok).map_or_else(String::new, |t| age(now.duration_since(t)));
            format!("stale · {since}")
        } else {
            output.note.clone().unwrap_or_default()
        };
        buf.set_stringn(body.x, bottom - 1, &text, body.width as usize, Style::default().fg(theme.dim));
    }
}

/// A small picture of a size on the 8 x 4 grid, two text rows tall: each character is two grid
/// rows, its top half in the foreground colour and its bottom half in the background colour.
fn size_picture(buf: &mut Buffer, x: u16, y: u16, w: u8, h: u8, theme: &Theme) {
    for pair in 0..ROWS / 2 {
        for column in 0..COLUMNS {
            let lit = |row: u8| if column < w && row < h { theme.fg } else { theme.track };
            let style = Style::default().fg(lit(pair * 2)).bg(lit(pair * 2 + 1));
            buf.set_string(x + u16::from(column), y + u16::from(pair), "▀", style);
        }
    }
}

fn draw_picker(app: &App, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let y = area.y + 1;
    buf.set_string(area.x + 1, y, "Add a panel", Style::default().fg(theme.bright).add_modifier(Modifier::BOLD));
    let hidden = app.hidden();
    let where_ = if app.add_at.is_some() { "it goes where you tapped" } else { "it goes in the first free space" };
    let hint = format!("{} not on the screen · {where_}", hidden.len());
    let buttons = app.banner_buttons(area);
    let room = buttons.first().map_or(area.width, |(_, r)| r.x).saturating_sub(area.x + 15);
    buf.set_stringn(area.x + 15, y, &hint, room as usize, Style::default().fg(theme.dim));
    for (_, rect) in buttons {
        button(buf, rect, "Cancel", None, Look::Outline { edge: theme.fg, text: theme.fg }, theme);
    }
    let rows = app.picker_rows(area);
    let shown = rows.iter().filter(|(r, _)| matches!(r, PickerRow::Panel(_))).count();
    for (row, rect) in &rows {
        match row {
            PickerRow::Package(name) => {
                buf.set_string(rect.x, rect.y, name.to_uppercase(), Style::default().fg(theme.dim).add_modifier(Modifier::BOLD));
            }
            PickerRow::Panel(key) => {
                let highlighted = hidden.get(app.picker_row) == Some(key);
                let rule = app.rules.get(key);
                let (w, h) = rule.map_or((1, 1), |r| (r.initial[0], r.initial[1]));
                size_picture(buf, rect.x + 1, rect.y + 1, w, h, theme);
                let title_style = Style::default().fg(if highlighted { theme.accent } else { theme.bright }).add_modifier(Modifier::BOLD);
                buf.set_string(rect.x + 12, rect.y + 1, app.title_of(key), title_style);
                let sizes = rule.map_or_else(String::new, |r| format!("{}×{} to {}×{}", r.min[0], r.min[1], r.max[0], r.max[1]));
                buf.set_string(rect.x + 12, rect.y + 2, format!("{} · {sizes}", key.package), Style::default().fg(theme.dim));
                let add = Rect { x: (rect.x + rect.width).saturating_sub(14), y: rect.y, width: 14, height: 3 };
                button(buf, add, "+ Add", None, Look::Outline { edge: theme.fg, text: theme.fg }, theme);
                for x in rect.x..rect.x + rect.width {
                    buf.set_string(x, rect.y + 3, "─", Style::default().fg(theme.track));
                }
            }
        }
    }
    if shown < hidden.len() {
        let y = rows.last().map_or(area.y + BANNER_ROWS + 1, |(_, r)| r.y + r.height + 1);
        buf.set_string(area.x + 1, y.min(area.y + area.height - 1), format!("and {} more", hidden.len() - shown), Style::default().fg(theme.dim));
    }
}

fn draw_confirm(app: &App, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let mid = area.y + area.height / 2;
    centred(buf, area, mid.saturating_sub(7), "Shut down the Nodo?", Style::default().fg(theme.bright).add_modifier(Modifier::BOLD));
    let dim = Style::default().fg(theme.dim);
    centred(buf, area, mid.saturating_sub(5), "Every service stops and saves its data first: cuprated can take a few minutes while it syncs.", dim);
    centred(buf, area, mid.saturating_sub(4), "Unplug only when the screen says Safe to unplug.", dim);
    centred(buf, area, mid.saturating_sub(2), "To start it again, unplug it and plug it back in.", dim);
    for (which, rect) in app.screen_buttons(area) {
        match which {
            Button::KeepRunning => button(buf, rect, "Cancel", Some("keep running"), Look::Outline { edge: theme.fg, text: theme.fg }, theme),
            Button::ConfirmShutDown => button(buf, rect, "⏻ Shut down", Some("stop everything safely"), Look::Filled { fill: theme.err }, theme),
            _ => {}
        }
    }
}

fn draw_shutdown(app: &App, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let s = &app.shutdown;
    if s.off {
        // Drawn last: the panel may stay lit after the device is off.
        let mid = area.y + area.height / 2;
        let ring = Rect { x: area.x + area.width / 2 - 4, y: mid.saturating_sub(7), width: 9, height: 5 };
        outline(buf, ring, Edge::Rounded, Style::default().fg(theme.ok));
        centred(buf, ring, ring.y + 2, "✓", Style::default().fg(theme.ok).add_modifier(Modifier::BOLD));
        centred(buf, area, mid, "Safe to unplug", Style::default().fg(theme.ok).add_modifier(Modifier::BOLD));
        centred(buf, area, mid + 2, "Every service stopped and its data is saved.", Style::default().fg(theme.dim));
        centred(buf, area, mid + 3, "The screen may stay lit: the Nodo is off.", Style::default().fg(theme.dim));
        centred(buf, area, mid + 5, "To start it again, plug it back in.", Style::default().fg(theme.dim));
        return;
    }
    let top = area.y + 3;
    let title = if s.error.is_some() { "The Nodo did not shut down" } else { "Shutting down safely…" };
    centred(buf, area, top, title, Style::default().fg(theme.bright).add_modifier(Modifier::BOLD));
    match &s.error {
        Some(error) => centred(buf, area, top + 2, &format!("{error}. It is still running: try again, or ask over SSH."), Style::default().fg(theme.err)),
        None => centred(buf, area, top + 2, "Do not unplug yet", Style::default().fg(theme.err).add_modifier(Modifier::BOLD)),
    }
    let width = 70u16.min(area.width.saturating_sub(4));
    let list_x = area.x + (area.width - width) / 2;
    let mut lines: Vec<(&str, Color, String, String, String)> = Vec::new();
    for step in &s.services {
        let (mark, color, state) = match step.state {
            StepState::Stopped => ("✓", theme.ok, "stopped".to_string()),
            StepState::TimedOut => ("…", theme.accent, "still saving; stopped at power off".to_string()),
            StepState::Stopping if step.name == "cuprated" || step.name == "monerod" => ("…", theme.info, "saving its database".to_string()),
            StepState::Stopping => ("…", theme.info, "stopping".to_string()),
        };
        let time = step.millis.map_or_else(String::new, |m| format!("{:.1} s", m as f64 / 1000.0));
        lines.push((mark, color, step.name.clone(), state, time));
    }
    let all_stopped = s.total > 0 && s.stopped() == s.total;
    let disk = if s.synced { ("✓", theme.ok, "saved") } else if s.syncing { ("…", theme.info, "writing") } else { ("·", theme.dim, "waiting") };
    lines.push((disk.0, disk.1, "saving files to disk".into(), disk.2.into(), String::new()));
    lines.push(("·", theme.dim, "turning off".into(), "waiting".into(), String::new()));
    let box_rect = Rect { x: list_x, y: top + 4, width, height: (lines.len() as u16 + 2).min(area.height.saturating_sub(top + 8)) };
    outline(buf, box_rect, Edge::Rounded, Style::default().fg(theme.border));
    for (i, (mark, color, name, state, time)) in lines.iter().enumerate() {
        let y = box_rect.y + 1 + i as u16;
        if y >= box_rect.y + box_rect.height - 1 {
            break;
        }
        buf.set_string(list_x + 2, y, mark, Style::default().fg(*color));
        let name_color = if *color == theme.dim { theme.dim } else { theme.fg };
        buf.set_string(list_x + 5, y, name, Style::default().fg(name_color));
        let right = list_x + width - 2;
        buf.set_string(right.saturating_sub(time.chars().count() as u16), y, time, Style::default().fg(theme.dim));
        let state_x = right.saturating_sub(9 + state.chars().count() as u16);
        let state_color = if *color == theme.ok { theme.dim } else { *color };
        buf.set_string(state_x, y, state, Style::default().fg(state_color));
    }
    let bar_y = box_rect.y + box_rect.height + 1;
    if s.total > 0 && bar_y < area.y + area.height {
        let done = s.stopped() + usize::from(s.synced);
        let filled = (usize::from(width) * done / (s.total + 1)).min(usize::from(width));
        buf.set_string(list_x, bar_y, "━".repeat(filled), Style::default().fg(theme.info));
        buf.set_string(list_x + filled as u16, bar_y, "━".repeat(usize::from(width) - filled), Style::default().fg(theme.track));
        let waiting: Vec<&str> = s.services.iter().filter(|x| x.state == StepState::Stopping).map(|x| x.name.as_str()).collect();
        let note = if all_stopped {
            "every service stopped".to_string()
        } else if waiting.len() <= 2 {
            format!("{} of {} services stopped · waiting for {}", s.stopped(), s.total, waiting.join(" and "))
        } else {
            format!("{} of {} services stopped", s.stopped(), s.total)
        };
        centred(buf, area, bar_y + 2, &note, Style::default().fg(theme.dim));
    }
    for (which, rect) in app.screen_buttons(area) {
        if which == Button::Back {
            button(buf, rect, "Back to the panels", None, Look::Outline { edge: theme.fg, text: theme.fg }, theme);
        }
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
pub(crate) mod tests {
    use super::*;
    use crate::app::tests::offered;
    use crate::app::Effect;
    use crossterm::event::{KeyCode, KeyModifiers};
    use nodo_panels::{Layout, PanelOutput};
    use std::time::Duration;

    pub fn text(buf: &Buffer) -> String {
        let area = buf.area;
        (area.y..area.y + area.height)
            .map(|y| (area.x..area.x + area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn screen(app: &App) -> String {
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
            rows: vec![("height".into(), "2121400 / 3478000".into()), ("left".into(), "about 9 h".into())],
            series: vec![10.0, 20.0, 30.0, 61.0],
            status: Some(Status::Busy),
            ..PanelOutput::default()
        }
    }

    /// The design's screen: the node, payments, device and a few small tiles, with two hidden.
    pub fn demo() -> App {
        let mut list = vec![
            ("cuprated", "Monero node", [2, 1], [4, 2], [4, 2]),
            ("mk-pay", "Payments today", [2, 1], [4, 2], [4, 2]),
            ("mk-bell", "Bell", [1, 1], [2, 1], [2, 1]),
            ("mk-lws", "Wallet scanner", [1, 1], [4, 1], [2, 1]),
            ("nodo", "Device", [2, 1], [4, 2], [2, 2]),
            ("mk-push-relay", "Push relay", [2, 1], [4, 1], [4, 1]),
            ("nodo-updates", "Updates", [2, 1], [4, 1], [2, 1]),
            ("tor", "Tor", [1, 1], [2, 1], [2, 1]),
        ]
        .into_iter()
        .map(|(package, title, min, max, initial)| {
            let mut o = offered(package, min, max, initial);
            o.title = title.into();
            o
        })
        .collect::<Vec<_>>();
        let rules = nodo_panels::size_rules(&list);
        let mut layout = Layout::default();
        for (package, x, y, w, h) in [("cuprated", 0, 0, 4, 2), ("mk-pay", 4, 0, 4, 2), ("mk-bell", 0, 2, 2, 1), ("mk-lws", 2, 2, 2, 1), ("nodo", 4, 2, 2, 2), ("mk-push-relay", 0, 3, 4, 1)] {
            let key = nodo_panels::PanelKey::new(package, "p");
            assert!(rules.contains_key(&key));
            layout.placements.push(nodo_panels::Placement { key, x, y, w, h });
        }
        let now = Instant::now();
        let outputs = [
            ("cuprated", sync_output()),
            ("mk-pay", PanelOutput { big: Some("4.82".into()), unit: Some("XMR".into()), rows: vec![("paid".into(), "37".into()), ("waiting".into(), "2".into())], series: vec![3.0, 5.0, 2.0, 7.0, 9.0, 6.0, 11.0, 13.0, 10.0, 8.0, 12.0, 6.0], status: Some(Status::Ok), ..PanelOutput::default() }),
            ("mk-bell", PanelOutput { big: Some("Ringing".into()), note: Some("last 0.8 s ago".into()), status: Some(Status::Ok), ..PanelOutput::default() }),
            ("mk-lws", PanelOutput { big: Some("214".into()), unit: Some("accounts".into()), note: Some("0 blocks behind".into()), status: Some(Status::Ok), ..PanelOutput::default() }),
            ("nodo", PanelOutput { big: Some("52 °C".into()), bar: Some(0.27), rows: vec![("ssd".into(), "541 GB / 2.0 TB".into()), ("mem".into(), "6.1 / 31 GB".into()), ("up".into(), "3 d 4 h".into())], status: Some(Status::Ok), ..PanelOutput::default() }),
            ("mk-push-relay", PanelOutput { big: Some("3".into()), unit: Some("devices".into()), note: Some("last push 4 min ago".into()), status: Some(Status::Ok), ..PanelOutput::default() }),
        ];
        list.truncate(list.len());
        let mut app = App::new(list, layout, now);
        for (package, output) in outputs {
            let state = app.states.get_mut(&nodo_panels::PanelKey::new(package, "p")).unwrap();
            state.record(Ok(output), now, Duration::from_secs(10));
        }
        app
    }

    #[test]
    fn the_everyday_screen_looks_like_the_design() {
        let out = screen(&demo());
        assert!(out.contains("monokulo-nodo"), "{out}");
        assert!(out.contains("thu 13 nov 14:32"));
        assert!(out.contains("☰ Arrange") && out.contains("⏻ Shut down"), "both buttons in the top bar:\n{out}");
        assert!(out.contains("╭") && out.contains("╯"), "rounded tiles:\n{out}");
        assert!(out.contains("MONERO NODE"));
        assert!(out.contains("█▀▀ ▄█  ▀ █"), "61% drawn large:\n{out}");
        assert!(out.contains("height") && out.contains("2121400 / 3478000"));
        assert!(out.contains("━━━"), "a thin bar");
        assert!(out.contains("▁▂▄█"), "sparkline:\n{out}");
        assert!(out.contains("cuprated"), "package name on the tile");
    }

    #[test]
    fn arranging_dragging_adding_and_hiding_look_like_the_design() {
        let mut app = demo();
        let now = Instant::now();
        app.key(KeyCode::Char('e'), KeyModifiers::NONE, now);
        app.selected = Some(4);
        let out = screen(&app);
        assert!(out.contains(" Arrange ") && out.contains("Hide") && out.contains("+ Add panel 2") && out.contains("Save"), "{out}");
        assert!(out.contains("┏") && out.contains("⇲"), "selected tile is heavy with a grip:\n{out}");
        assert!(out.contains("┄"), "free spaces are dashed:\n{out}");
        assert!(out.contains("can be 2×1 to 4×2"));
        assert!(!out.contains("move here"), "no labels on free spaces");
        app.key(KeyCode::Char('x'), KeyModifiers::NONE, now);
        assert!(screen(&app).contains("Undo: show Device · 8 s"));
        assert_eq!(app.key(KeyCode::Char('a'), KeyModifiers::NONE, now), Effect::None);
        let out = screen(&app);
        assert!(out.contains("Add a panel") && out.contains("Updates") && out.contains("+ Add") && out.contains("Cancel"), "{out}");
        assert!(out.contains("NODO-UPDATES") || out.contains("TOR"), "grouped by package:\n{out}");
    }

    #[test]
    fn waiting_stale_and_failed_tiles_say_so() {
        let mut app = demo();
        let key = nodo_panels::PanelKey::new("cuprated", "p");
        let now = Instant::now();
        app.states.get_mut(&key).unwrap().record(Err("cuprated timed out".into()), now, Duration::from_secs(10));
        assert!(screen(&app).contains("stale · 0 s"));
        let mut fresh = demo();
        fresh.states.insert(key.clone(), nodo_panels::PanelState::new(now));
        assert!(screen(&fresh).contains("waiting for cuprated"));
        fresh.states.get_mut(&key).unwrap().record(Err("cuprated is not answering (connection refused)".into()), now, Duration::from_secs(10));
        assert!(screen(&fresh).contains("cuprated is not answering"), "{}", screen(&fresh));
    }

    #[test]
    fn shutting_down_looks_like_the_design() {
        let mut app = demo();
        let now = Instant::now();
        app.key(KeyCode::Char('p'), KeyModifiers::NONE, now);
        let out = screen(&app);
        assert!(out.contains("Shut down the Nodo?") && out.contains("keep running") && out.contains("stop everything safely"), "{out}");
        app.key(KeyCode::Enter, KeyModifiers::NONE, now);
        for line in ["services 3", "stopping cuprated", "stopping mk-pay", "stopping sshd", "stopped mk-pay 1100", "stopped sshd 200"] {
            app.power_event(nodo_power::Event::parse(line).unwrap());
        }
        let out = screen(&app);
        assert!(out.contains("Do not unplug yet") && out.contains("saving its database") && out.contains("waiting for cuprated"), "{out}");
        app.power_event(nodo_power::Event::Off);
        assert!(screen(&app).contains("Safe to unplug"));
    }

    #[test]
    fn every_glyph_drawn_is_in_the_console_font() {
        // The font script adds exactly these; everything else drawn must be ASCII or already in
        // Terminus ter-v32n (box lines, █, ·, …, ×, °).
        let script = include_str!("../../../packages/nodo-tools/files/screen-font/make-font.py");
        for c in GLYPHS.chars() {
            assert!(script.contains(&format!("\"{c}\":")), "make-font.py does not draw {c}");
        }
        let terminus = "─│┌┐└┘█·…×°";
        let mut app = demo();
        let now = Instant::now();
        let mut screens = vec![screen(&app)];
        app.key(KeyCode::Char('e'), KeyModifiers::NONE, now);
        app.selected = Some(0);
        screens.push(screen(&app));
        app.key(KeyCode::Char('a'), KeyModifiers::NONE, now);
        screens.push(screen(&app));
        app.key(KeyCode::Esc, KeyModifiers::NONE, now);
        app.key(KeyCode::Enter, KeyModifiers::NONE, now);
        app.key(KeyCode::Char('p'), KeyModifiers::NONE, now);
        screens.push(screen(&app));
        for out in screens {
            for c in out.chars().filter(|c| !c.is_ascii()) {
                assert!(GLYPHS.contains(c) || terminus.contains(c), "{c:?} is not in the console font");
            }
        }
    }

    /// Writes each state of the design as text, for reviewing changes:
    /// `NODO_SCREEN_SNAPSHOTS=dir cargo test -p nodo-screen snapshots`.
    #[test]
    fn snapshots() {
        let Some(dir) = std::env::var_os("NODO_SCREEN_SNAPSHOTS") else { return };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let size = Rect::new(0, 0, 120, 33);
        let now = Instant::now();
        let save = |name: &str, app: &App| std::fs::write(dir.join(format!("{name}.txt")), screen(app) + "\n").unwrap();
        let mut app = demo();
        save("1-normal", &app);
        app.key(KeyCode::Char('e'), KeyModifiers::NONE, now);
        let device = cells_rect(grid_area(size), 4, 2, 1, 1);
        app.touch_down(size, device.x + 3, device.y + 3, now);
        app.touch_up();
        save("2-arrange", &app);
        let to = cells_rect(grid_area(size), 6, 2, 1, 1);
        app.touch_down(size, device.x + 3, device.y + 3, now);
        app.touch_move(size, to.x + 6, to.y + 4);
        save("3-drag", &app);
        app.touch_up();
        app.key(KeyCode::Char('x'), KeyModifiers::NONE, now);
        save("4-hidden-undo", &app);
        app.key(KeyCode::Char('a'), KeyModifiers::NONE, now);
        save("5-add", &app);
        app.key(KeyCode::Esc, KeyModifiers::NONE, now);
        app.key(KeyCode::Char('u'), KeyModifiers::NONE, now);
        app.key(KeyCode::Enter, KeyModifiers::NONE, now);
        app.key(KeyCode::Char('p'), KeyModifiers::NONE, now);
        save("6-shutdown-confirm", &app);
        app.key(KeyCode::Enter, KeyModifiers::NONE, now);
        for line in ["services 6", "stopping cuprated", "stopping mk-bell", "stopping mk-lws", "stopping mk-pay", "stopping mk-push-relay", "stopping sshd", "stopped mk-push-relay 400", "stopped mk-bell 600", "stopped mk-pay 1100", "stopped mk-lws 2300", "stopped sshd 200"] {
            app.power_event(nodo_power::Event::parse(line).unwrap());
        }
        save("7-shutdown-progress", &app);
        app.power_event(nodo_power::Event::Off);
        save("8-safe-to-unplug", &app);
    }

    #[test]
    fn sparklines_scale_to_their_range() {
        assert_eq!(sparkline(&[1.0, 2.0, 3.0, 8.0], 10), "▁▂▃█");
        assert_eq!(sparkline(&[5.0, 5.0], 10), "▄▄", "flat");
        assert_eq!(sparkline(&[1.0, 2.0, 3.0, 8.0], 2), "▁█", "newest kept, rescaled");
        assert!(palette_sequence().starts_with("\x1b]P0000000\x1b]P1FF8A7A"));
    }
}
