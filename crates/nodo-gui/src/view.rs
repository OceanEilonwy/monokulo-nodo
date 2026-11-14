//! Turns the screen's state into what the .slint files draw.
//!
//! Each part is built as plain data first and compared with what was last shown, so the window is
//! only touched when something visible changed: an idle screen redraws nothing and uses no CPU.

use std::rc::Rc;
use std::time::Instant;

use nodo_panels::output::Status;
use nodo_panels::{age, PanelOutput, PanelState};
use slint::{Color, Model, ModelRc, SharedString, VecModel};

use crate::geometry::{self, Rect};
use crate::state::{App, Button, DragKind, Mode, PickerRow, StepState};
use crate::{ButtonView, CellView, GhostView, MainWindow, PickerRowView, RowView, StepView, TileView};

pub mod colour {
    use slint::Color;
    pub const FG: Color = Color::from_rgb_u8(0xD7, 0xDA, 0xDC);
    pub const BRIGHT: Color = Color::from_rgb_u8(0xF4, 0xF5, 0xF6);
    pub const DIM: Color = Color::from_rgb_u8(0x80, 0x88, 0x8E);
    pub const BORDER: Color = Color::from_rgb_u8(0x3A, 0x40, 0x45);
    pub const OK: Color = Color::from_rgb_u8(0x6F, 0xD6, 0xB3);
    pub const ACCENT: Color = Color::from_rgb_u8(0xF2, 0xA3, 0x3A);
    pub const ERR: Color = Color::from_rgb_u8(0xFF, 0x8A, 0x7A);
    pub const INFO: Color = Color::from_rgb_u8(0x86, 0xBE, 0xFF);
}

use colour::*;

fn status_colour(status: Option<Status>) -> Color {
    match status {
        Some(Status::Ok) => OK,
        Some(Status::Warn) => ACCENT,
        Some(Status::Error) => ERR,
        Some(Status::Idle) => DIM,
        Some(Status::Busy) | None => INFO,
    }
}

/// "4×2".
pub fn size_text(w: u8, h: u8) -> String {
    format!("{w}×{h}")
}

/// A tile as plain data.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub rect: Rect,
    pub title: String,
    pub title_colour: Color,
    pub package: String,
    pub dot: Option<Color>,
    pub outline: Color,
    pub outline_width: f32,
    pub big: String,
    pub unit: String,
    pub big_colour: Color,
    pub compact: bool,
    pub bar: Option<(f32, Color)>,
    pub rows: Vec<(String, String)>,
    pub value_colour: Color,
    pub series: Vec<f32>,
    pub series_colour: Color,
    pub note: String,
    pub note_colour: Color,
    pub message: String,
    pub message_colour: Color,
    pub arranging: bool,
    pub selected: bool,
    pub size_text: String,
    /// 0 a panel, 1 the drawn logo, 2 the text logo.
    pub logo: i32,
    pub logo_frame: Option<i32>,
    pub logo_name: bool,
}

impl Tile {
    fn empty(rect: Rect, title: String) -> Tile {
        Tile {
            rect,
            title,
            title_colour: DIM,
            package: String::new(),
            dot: None,
            outline: BORDER,
            outline_width: 2.0,
            big: String::new(),
            unit: String::new(),
            big_colour: BRIGHT,
            compact: false,
            bar: None,
            rows: Vec::new(),
            value_colour: BRIGHT,
            series: Vec::new(),
            series_colour: INFO,
            note: String::new(),
            note_colour: DIM,
            message: String::new(),
            message_colour: DIM,
            arranging: false,
            selected: false,
            size_text: String::new(),
            logo: 0,
            logo_frame: None,
            logo_name: false,
        }
    }

    fn to_slint(&self) -> TileView {
        TileView {
            x: self.rect.x,
            y: self.rect.y,
            w: self.rect.w,
            h: self.rect.h,
            title: self.title.clone().into(),
            title_color: self.title_colour,
            package: self.package.clone().into(),
            dot: self.dot.is_some(),
            dot_color: self.dot.unwrap_or(DIM),
            outline: self.outline,
            outline_width: self.outline_width,
            big: self.big.clone().into(),
            unit: self.unit.clone().into(),
            big_color: self.big_colour,
            compact: self.compact,
            bar_shown: self.bar.is_some(),
            bar: self.bar.map_or(0.0, |b| b.0),
            bar_color: self.bar.map_or(INFO, |b| b.1),
            rows: ModelRc::new(VecModel::from(
                self.rows.iter().map(|(l, v)| RowView { label: l.clone().into(), value: v.clone().into() }).collect::<Vec<_>>(),
            )),
            value_color: self.value_colour,
            series: ModelRc::new(VecModel::from(self.series.clone())),
            series_color: self.series_colour,
            note: self.note.clone().into(),
            note_color: self.note_colour,
            message: self.message.clone().into(),
            message_color: self.message_colour,
            arranging: self.arranging,
            selected: self.selected,
            size_text: self.size_text.clone().into(),
            logo: self.logo,
            logo_image: if self.logo == 2 { crate::screenshot::text_logo(self.logo_frame) } else { slint::Image::default() },
            logo_name: self.logo_name,
        }
    }
}

// Heights in a tile's content, from the .slint file: what decides how many rows fit.
const CONTENT_TOP: f32 = 52.0;
const CONTENT_BOTTOM: f32 = 16.0;
const BIG_HEIGHT: f32 = 92.0;
const COMPACT_HEIGHT: f32 = 54.0;
const BAR_HEIGHT: f32 = 22.0;
const ROW_HEIGHT: f32 = 30.0;
const ROWS_GAP: f32 = 14.0;
const SPARK_HEIGHT: f32 = 60.0 + 44.0 + 8.0;
const NOTE_HEIGHT: f32 = 32.0;
const PACKAGE_HEIGHT: f32 = 30.0;

/// Scales a series to its own range, newest values last, as many as the width shows well.
fn spark(series: &[f64], width: f32) -> Vec<f32> {
    let fit = ((width / 14.0) as usize).max(4);
    let values = &series[series.len().saturating_sub(fit)..];
    let (lo, hi) = values.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    values
        .iter()
        .map(|v| if hi > lo { (0.12 + 0.88 * (v - lo) / (hi - lo)) as f32 } else { 0.5 })
        .collect()
}

/// A tile while viewing: what its panel last said, as much as fits its size.
fn viewing_tile(app: &App, index: usize, now: Instant) -> Tile {
    let p = &app.layout.placements[index];
    let rect = geometry::cells(p.x, p.y, p.w, p.h);
    let offered = app.offered(&p.key);
    let state: Option<&PanelState> = app.states.get(&p.key);
    let output: Option<&PanelOutput> = state.and_then(|s| s.last.as_ref());
    let stale = state.is_some_and(|s| s.is_stale());
    let failed = state.is_some_and(|s| s.failure.is_some()) || offered.is_some_and(|o| o.script.is_err());
    let status = output.and_then(|o| o.status);

    let mut t = Tile::empty(rect, app.title_of(&p.key).to_uppercase());
    if p.key == crate::state::logo_key() {
        t.title = String::new();
        t.outline_width = 0.0;
        t.logo = if app.logo.text { 2 } else { 1 };
        t.logo_frame = if app.logo.text && app.logo.shimmer { app.logo.frame } else { None };
        t.logo_name = app.logo.name;
        let _ = now;
        return t;
    }
    if p.key.package != nodo_panels::BUILTIN_PACKAGE {
        t.package = p.key.package.clone();
    }
    t.outline = match status {
        Some(Status::Warn) => ACCENT,
        Some(Status::Error) => ERR,
        _ => BORDER,
    };
    if matches!(status, Some(Status::Warn | Status::Error)) && !stale {
        t.title_colour = t.outline;
    }
    t.dot = if failed && (output.is_none() || stale) {
        Some(ERR)
    } else if output.is_some() && stale {
        Some(DIM)
    } else {
        status.map(|s| status_colour(Some(s)))
    };
    t.compact = p.h == 1;

    let Some(output) = output else {
        t.message = match (state.and_then(|s| s.failure.as_deref()), offered.map(|o| &o.script)) {
            (_, Some(Err(reason))) => reason.clone(),
            (Some(failure), _) => failure.to_string(),
            _ => format!("waiting for {}", p.key.package),
        };
        t.message_colour = if failed { ERR } else { DIM };
        return t;
    };

    let lit = |c: Color| if stale { DIM } else { c };
    t.value_colour = lit(BRIGHT);
    if let Some(big) = &output.big {
        t.big = big.clone();
        t.unit = output.unit.clone().unwrap_or_default();
        t.big_colour = lit(match status {
            Some(Status::Warn) => ACCENT,
            Some(Status::Error) => ERR,
            Some(Status::Idle) => DIM,
            _ => BRIGHT,
        });
    }
    // docs/SCREEN.md: at 2 x 1, big, unit and note; from 2 x 2 also bar and rows; from 4 x 1 also
    // series.
    if p.h >= 2 {
        if let Some(bar) = output.bar {
            t.bar = Some((bar as f32, lit(status_colour(status))));
        }
    }
    if p.w >= 4 && !output.series.is_empty() {
        t.series = spark(&output.series, rect.w - 48.0);
        t.series_colour = lit(status_colour(status));
    }
    t.note = if stale {
        let since = state.and_then(|s| s.last_ok).map_or_else(String::new, |at| age(now.duration_since(at)));
        format!("stale · {since}")
    } else {
        output.note.clone().unwrap_or_default()
    };
    if p.h >= 2 && !output.rows.is_empty() {
        let mut room = rect.h - CONTENT_TOP - CONTENT_BOTTOM - ROWS_GAP;
        if !t.big.is_empty() {
            room -= if t.compact { COMPACT_HEIGHT } else { BIG_HEIGHT };
        }
        if t.bar.is_some() {
            room -= BAR_HEIGHT;
        }
        if !t.series.is_empty() {
            room -= SPARK_HEIGHT;
        }
        if !t.note.is_empty() {
            room -= NOTE_HEIGHT;
        }
        if !t.package.is_empty() && t.series.is_empty() {
            room -= PACKAGE_HEIGHT;
        }
        let fit = (room / ROW_HEIGHT).floor().max(0.0) as usize;
        t.rows = output.rows.iter().take(fit).cloned().collect();
    }
    t
}

/// A tile while arranging: its name and size, and the grip when it is the selected one.
fn arranging_tile(app: &App, index: usize) -> Tile {
    let p = &app.layout.placements[index];
    let selected = app.selected == Some(index);
    let mut t = Tile::empty(geometry::cells(p.x, p.y, p.w, p.h), app.title_of(&p.key).to_uppercase());
    t.arranging = true;
    t.selected = selected;
    t.title_colour = if selected { ACCENT } else { FG };
    if selected {
        t.outline = ACCENT;
        t.outline_width = 4.0;
    }
    t.size_text = match (selected, app.rules.get(&p.key)) {
        (true, Some(rule)) if rule.min != rule.max => {
            format!("{} · can be {} to {}", size_text(p.w, p.h), size_text(rule.min[0], rule.min[1]), size_text(rule.max[0], rule.max[1]))
        }
        _ => size_text(p.w, p.h),
    };
    t
}

/// The tile being moved by a finger leaves its place: it is drawn as the ghost instead.
fn moving(app: &App) -> Option<usize> {
    app.drag.as_ref().filter(|d| d.moved && matches!(d.kind, DragKind::Move { .. })).map(|d| d.index)
}

pub fn tiles(app: &App, now: Instant) -> Vec<Tile> {
    let arranging = app.mode == Mode::Edit;
    let skip = moving(app);
    (0..app.layout.placements.len())
        .filter(|i| Some(*i) != skip)
        .map(|i| if arranging { arranging_tile(app, i) } else { viewing_tile(app, i, now) })
        .collect()
}

/// While arranging, every cell no tile covers, drawn dashed.
pub fn free_cells(app: &App) -> Vec<Rect> {
    if app.mode != Mode::Edit {
        return Vec::new();
    }
    let skip = moving(app);
    let mut out = Vec::new();
    for y in 0..nodo_panels::layout::ROWS {
        for x in 0..nodo_panels::layout::COLUMNS {
            match app.layout.at(x, y) {
                Some(i) if Some(i) != skip => {}
                _ => out.push(geometry::cells(x, y, 1, 1)),
            }
        }
    }
    out
}

/// Where a dragged tile would land, and the tile following the finger.
pub fn drag_marks(app: &App) -> (Option<Rect>, Option<(Rect, String, String)>) {
    let Some(drag) = app.drag.as_ref().filter(|d| d.moved && app.mode == Mode::Edit) else { return (None, None) };
    let target = drag.target.as_ref().map(|p| geometry::cells(p.x, p.y, p.w, p.h));
    let ghost = match drag.kind {
        DragKind::Move { ox, oy, .. } => app.layout.placements.get(drag.index).map(|p| {
            let r = geometry::cells(p.x, p.y, p.w, p.h);
            (Rect::new(drag.finger.0 - ox, drag.finger.1 - oy, r.w, r.h), app.title_of(&p.key).to_uppercase(), size_text(p.w, p.h))
        }),
        DragKind::Resize => None,
    };
    (target, ghost)
}

/// Arranging's line in the top bar, and whether it is a refusal.
pub fn hint(app: &App) -> (String, bool) {
    if let Some(message) = &app.message {
        return (message.clone(), true);
    }
    if let Some(drag) = app.drag.as_ref().filter(|d| d.moved) {
        if drag.target.is_some() {
            let title = app.layout.placements.get(drag.index).map(|p| app.title_of(&p.key)).unwrap_or_default();
            return (format!("release to put {title} here"), false);
        }
    }
    if let Some(hidden) = &app.undo {
        return (format!("{} hidden", app.title_of(&hidden.placement.key)), false);
    }
    ("drag a tile to move it · drag ⇲ to resize".into(), false)
}

pub fn picker_hint(app: &App) -> String {
    let n = app.hidden().len();
    let place = if app.add_at.is_some() { "it goes in the space you tapped" } else { "it goes in the first free space, or tap a free space first" };
    format!("{n} not on the screen · {place}")
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickerLine {
    pub heading: bool,
    pub rect: Rect,
    pub title: String,
    pub detail: String,
    pub sizes: String,
    pub preview: (u8, u8),
    pub highlighted: bool,
}

pub fn picker(app: &App) -> Vec<PickerLine> {
    let hidden = app.hidden();
    let highlighted = hidden.get(app.picker_row);
    app.picker_rows()
        .into_iter()
        .map(|(row, rect)| match row {
            PickerRow::Package(name) => PickerLine { heading: true, rect, title: name.to_uppercase(), detail: String::new(), sizes: String::new(), preview: (0, 0), highlighted: false },
            PickerRow::Panel(key) => {
                let offered = app.offered(&key);
                let rule = app.rules.get(&key);
                let sizes = rule.map_or_else(String::new, |r| {
                    if r.min == r.max {
                        size_text(r.min[0], r.min[1])
                    } else {
                        format!("{} to {}", size_text(r.min[0], r.min[1]), size_text(r.max[0], r.max[1]))
                    }
                });
                let detail = offered.map_or_else(String::new, |o| format!("{} · refreshes every {} s", key.panel, o.refresh.as_secs()));
                PickerLine {
                    heading: false,
                    rect,
                    title: app.title_of(&key),
                    detail,
                    sizes,
                    preview: rule.map_or((1, 1), |r| (r.initial[0], r.initial[1])),
                    highlighted: app.picker_keys && highlighted == Some(&key),
                }
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ButtonLine {
    pub rect: Rect,
    pub label: String,
    pub count: String,
    pub sub: String,
    pub kind: i32,
    pub icon: i32,
}

pub fn buttons(app: &App, now: Instant) -> Vec<ButtonLine> {
    let mut list = app.banner_buttons(now);
    list.extend(app.screen_buttons());
    list.into_iter()
        .map(|(b, rect)| {
            let (label, count, sub, kind, icon) = match b {
                Button::Manage => ("Manage".to_string(), String::new(), "", 0, 2),
                Button::LogoOptions => ("Logo options".to_string(), String::new(), "", 0, 2),
                Button::Arrange => ("Arrange".to_string(), String::new(), "", 0, 3),
                Button::ShutDown => (app.button_label(b, now), String::new(), "", 1, 0),
                Button::Hide | Button::Cancel => (app.button_label(b, now), String::new(), "", 2, 0),
                Button::Undo => (app.button_label(b, now), String::new(), "", 4, 0),
                Button::Add => ("+ Add panel".to_string(), app.hidden().len().to_string(), "", 2, 0),
                Button::Save => (app.button_label(b, now), String::new(), "", 3, 0),
                Button::KeepRunning => (app.button_label(b, now), String::new(), "keep running", 6, 0),
                Button::ConfirmShutDown => (app.button_label(b, now), String::new(), "stop everything safely", 5, 0),
                Button::Back => (app.button_label(b, now), String::new(), "", 6, 0),
            };
            ButtonLine { rect, label, count, sub: sub.to_string(), kind, icon }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct StepLine {
    pub mark: &'static str,
    pub mark_colour: Color,
    pub name: String,
    pub name_colour: Color,
    pub state: String,
    pub state_colour: Color,
    pub time: String,
}

/// The shutdown screen: its title and subtitle, a line per service, the bar and its note.
pub fn shutdown(app: &App) -> (String, String, Vec<StepLine>, f32, String) {
    let s = &app.shutdown;
    let title = if s.error.is_some() { "The Nodo did not shut down" } else { "Shutting down safely…" };
    let subtitle = match &s.error {
        Some(error) => format!("{error}. It is still running: try again, or ask over SSH."),
        None => "Do not unplug yet".to_string(),
    };
    let mut steps = Vec::new();
    for step in &s.services {
        let (mark, colour, state) = match step.state {
            StepState::Stopped => ("✓", OK, "stopped".to_string()),
            StepState::TimedOut => ("…", ACCENT, "still saving; stopped at power off".to_string()),
            StepState::Stopping if step.name == "cuprated" || step.name == "monerod" => ("…", INFO, "saving its database".to_string()),
            StepState::Stopping => ("…", INFO, "stopping".to_string()),
        };
        let time = step.millis.map_or_else(String::new, |m| format!("{:.1} s", m as f64 / 1000.0));
        let state_colour = if colour == OK { DIM } else { colour };
        steps.push(StepLine { mark, mark_colour: colour, name: step.name.clone(), name_colour: FG, state, state_colour, time });
    }
    let disk = if s.synced { ("✓", OK, "saved") } else if s.syncing { ("…", INFO, "writing") } else { ("·", DIM, "waiting") };
    steps.push(StepLine {
        mark: disk.0,
        mark_colour: disk.1,
        name: "saving files to disk".into(),
        name_colour: if disk.1 == DIM { DIM } else { FG },
        state: disk.2.into(),
        state_colour: if disk.1 == OK { DIM } else { disk.1 },
        time: String::new(),
    });
    steps.push(StepLine { mark: "·", mark_colour: DIM, name: "turning off".into(), name_colour: DIM, state: "waiting".into(), state_colour: DIM, time: String::new() });
    let done = s.stopped() + usize::from(s.synced);
    let progress = if s.total > 0 { done as f32 / (s.total + 1) as f32 } else { 0.0 };
    let waiting: Vec<&str> = s.services.iter().filter(|x| x.state == StepState::Stopping).map(|x| x.name.as_str()).collect();
    let note = if s.total == 0 {
        String::new()
    } else if s.total == s.stopped() {
        "every service stopped".to_string()
    } else if waiting.len() <= 2 && !waiting.is_empty() {
        format!("{} of {} services stopped · waiting for {}", s.stopped(), s.total, waiting.join(" and "))
    } else {
        format!("{} of {} services stopped", s.stopped(), s.total)
    };
    (title.to_string(), subtitle, steps, progress, note)
}

pub fn mode_number(app: &App) -> i32 {
    match app.mode {
        Mode::View => 0,
        Mode::Edit => 1,
        Mode::Picker => 2,
        Mode::Confirm => 3,
        Mode::ShuttingDown if app.shutdown.off => 5,
        Mode::ShuttingDown => 4,
    }
}

/// Updates `model` to `new`, touching only the rows that changed.
fn sync_rows<D: PartialEq + Clone, T: Clone + 'static>(model: &VecModel<T>, shown: &mut Vec<D>, new: Vec<D>, convert: impl Fn(&D) -> T) {
    if shown.len() != new.len() {
        model.set_vec(new.iter().map(&convert).collect::<Vec<_>>());
    } else {
        for (i, (old, now)) in shown.iter().zip(new.iter()).enumerate() {
            if old != now {
                model.set_row_data(i, convert(now));
            }
        }
    }
    *shown = new;
}

fn cell(r: &Rect) -> CellView {
    CellView { x: r.x, y: r.y, w: r.w, h: r.h }
}

/// What the window shows now, kept so that only changes are passed on.
pub struct Presenter {
    tiles: Rc<VecModel<TileView>>,
    tiles_shown: Vec<Tile>,
    cells: Rc<VecModel<CellView>>,
    cells_shown: Vec<Rect>,
    buttons: Rc<VecModel<ButtonView>>,
    buttons_shown: Vec<ButtonLine>,
    picker: Rc<VecModel<PickerRowView>>,
    picker_shown: Vec<PickerLine>,
    steps: Rc<VecModel<StepView>>,
    steps_shown: Vec<StepLine>,
}

impl Presenter {
    pub fn new(ui: &MainWindow) -> Self {
        let p = Presenter {
            tiles: Rc::new(VecModel::default()),
            tiles_shown: Vec::new(),
            cells: Rc::new(VecModel::default()),
            cells_shown: Vec::new(),
            buttons: Rc::new(VecModel::default()),
            buttons_shown: Vec::new(),
            picker: Rc::new(VecModel::default()),
            picker_shown: Vec::new(),
            steps: Rc::new(VecModel::default()),
            steps_shown: Vec::new(),
        };
        ui.set_tiles(ModelRc::from(p.tiles.clone()));
        ui.set_free_cells(ModelRc::from(p.cells.clone()));
        ui.set_buttons(ModelRc::from(p.buttons.clone()));
        ui.set_picker(ModelRc::from(p.picker.clone()));
        ui.set_steps(ModelRc::from(p.steps.clone()));
        p
    }

    /// Passes what changed since the last call to the window.
    pub fn show(&mut self, ui: &MainWindow, app: &App, now: Instant, clock: (&str, &str)) {
        macro_rules! set {
            ($get:ident, $set:ident, $value:expr) => {{
                let value = $value;
                if ui.$get() != value {
                    ui.$set(value);
                }
            }};
        }
        set!(get_mode, set_mode, mode_number(app));
        set!(get_day, set_day, SharedString::from(clock.0));
        set!(get_time, set_time, SharedString::from(clock.1));

        sync_rows(&self.tiles, &mut self.tiles_shown, tiles(app, now), Tile::to_slint);
        sync_rows(&self.cells, &mut self.cells_shown, free_cells(app), cell);

        let (target, ghost) = drag_marks(app);
        set!(get_target_shown, set_target_shown, target.is_some());
        if let Some(t) = target {
            set!(get_target, set_target, cell(&t));
        }
        let ghost = match ghost {
            Some((r, title, size)) => GhostView { shown: true, x: r.x, y: r.y, w: r.w, h: r.h, title: title.into(), size_text: size.into() },
            None => GhostView::default(),
        };
        set!(get_ghost, set_ghost, ghost);

        let (hint_text, warning) = hint(app);
        set!(get_hint, set_hint, SharedString::from(hint_text));
        set!(get_hint_is_warning, set_hint_is_warning, warning);
        set!(get_picker_hint, set_picker_hint, SharedString::from(picker_hint(app)));

        let buttons = buttons(app, now);
        let left_of_buttons = buttons.iter().filter(|b| b.rect.y < geometry::grid().y).map(|b| b.rect.x).fold(geometry::header().right(), f32::min);
        set!(get_header_text_width, set_header_text_width, left_of_buttons - geometry::PAD_X - geometry::BUTTON_GAP);
        sync_rows(&self.buttons, &mut self.buttons_shown, buttons, |b| ButtonView {
            x: b.rect.x,
            y: b.rect.y,
            w: b.rect.w,
            h: b.rect.h,
            label: b.label.clone().into(),
            count: b.count.clone().into(),
            sub: b.sub.clone().into(),
            kind: b.kind,
            icon: b.icon,
        });

        let picker = if app.mode == Mode::Picker { picker(app) } else { Vec::new() };
        sync_rows(&self.picker, &mut self.picker_shown, picker, |p| PickerRowView {
            heading: p.heading,
            x: p.rect.x,
            y: p.rect.y,
            w: p.rect.w,
            h: p.rect.h,
            title: p.title.clone().into(),
            detail: p.detail.clone().into(),
            sizes: p.sizes.clone().into(),
            pw: i32::from(p.preview.0),
            ph: i32::from(p.preview.1),
            highlighted: p.highlighted,
        });

        if app.mode == Mode::ShuttingDown {
            let (title, subtitle, steps, progress, note) = shutdown(app);
            set!(get_shutdown_title, set_shutdown_title, SharedString::from(title));
            set!(get_shutdown_subtitle, set_shutdown_subtitle, SharedString::from(subtitle));
            set!(get_shutdown_failed, set_shutdown_failed, app.shutdown.error.is_some());
            set!(get_progress, set_progress, progress);
            set!(get_progress_note, set_progress_note, SharedString::from(note));
            sync_rows(&self.steps, &mut self.steps_shown, steps, |s| StepView {
                mark: s.mark.into(),
                mark_color: s.mark_colour,
                name: s.name.clone().into(),
                name_color: s.name_colour,
                state: s.state.clone().into(),
                state_color: s.state_colour,
                time: s.time.clone().into(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::app;
    use nodo_panels::PanelKey;
    use std::time::Duration;

    #[test]
    fn a_tile_shows_what_fits_its_size() {
        let mut app = app();
        let now = Instant::now();
        let out = PanelOutput {
            big: Some("61%".into()),
            unit: Some("synced".into()),
            bar: Some(0.61),
            rows: (0..10).map(|i| (format!("r{i}"), "v".into())).collect(),
            series: vec![1.0, 2.0, 3.0],
            status: Some(Status::Busy),
            note: Some("about 9 h".into()),
            ..PanelOutput::default()
        };
        for key in ["a", "b"] {
            app.states.get_mut(&PanelKey::new(key, "p")).unwrap().record(Ok(out.clone()), now, Duration::from_secs(10));
        }
        let tiles = tiles(&app, now);
        // a is 4 x 2: bar, rows that fit above the sparkline, and the sparkline.
        assert!(tiles[0].bar.is_some() && !tiles[0].series.is_empty());
        assert!(!tiles[0].rows.is_empty() && tiles[0].rows.len() < 10, "{}", tiles[0].rows.len());
        assert!(!tiles[0].compact);
        // b is 2 x 1: big, unit and note only.
        assert!(tiles[1].compact && tiles[1].bar.is_none() && tiles[1].rows.is_empty() && tiles[1].series.is_empty());
        assert_eq!(tiles[1].note, "about 9 h");
        assert_eq!(tiles[0].package, "a");
    }

    #[test]
    fn waiting_and_failed_tiles_say_so() {
        let mut app = app();
        let now = Instant::now();
        assert_eq!(tiles(&app, now)[0].message, "waiting for a");
        app.states.get_mut(&PanelKey::new("a", "p")).unwrap().record(Err("cuprated is not answering".into()), now, Duration::from_secs(10));
        let t = &tiles(&app, now)[0];
        assert_eq!(t.message, "cuprated is not answering");
        assert_eq!(t.message_colour, ERR);
        assert_eq!(t.dot, Some(ERR));
    }

    #[test]
    fn arranging_shows_sizes_free_cells_and_the_moving_tile_as_a_ghost() {
        let mut app = app();
        let now = Instant::now();
        app.act(Button::Arrange, now);
        assert_eq!(free_cells(&app).len(), 32 - 8 - 2);
        let c = geometry::cells(4, 0, 1, 1);
        app.touch_down(c.x + 10.0, c.y + 10.0, now);
        let t = &tiles(&app, now)[1];
        assert!(t.selected && t.size_text.contains("can be 2×1 to 4×2"), "{}", t.size_text);
        let to = geometry::cells(6, 3, 1, 1);
        app.touch_move(to.x + 10.0, to.y + 10.0);
        assert_eq!(tiles(&app, now).len(), 1, "the moving tile leaves its place");
        assert_eq!(free_cells(&app).len(), 32 - 8);
        let (target, ghost) = drag_marks(&app);
        assert_eq!(target, Some(geometry::cells(6, 3, 2, 1)));
        let (ghost, title, _) = ghost.unwrap();
        assert_eq!((ghost.x, ghost.y), (to.x, to.y), "the ghost keeps the grabbed point under the finger");
        assert_eq!(title, "B");
        assert_eq!(hint(&app).0, "release to put b here");
    }

    #[test]
    fn the_shutdown_screen_lists_every_step() {
        let mut app = app();
        app.mode = Mode::ShuttingDown;
        for line in ["services 2", "stopping cuprated", "stopping mk-pay", "stopped mk-pay 300"] {
            app.power_event(nodo_power::Event::parse(line).unwrap());
        }
        let (title, subtitle, steps, progress, note) = shutdown(&app);
        assert_eq!(title, "Shutting down safely…");
        assert_eq!(subtitle, "Do not unplug yet");
        assert_eq!(steps[0].state, "saving its database");
        assert_eq!(steps[1].time, "0.3 s");
        assert_eq!(steps.len(), 4);
        assert!((progress - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(note, "1 of 2 services stopped · waiting for cuprated");
        assert_eq!(mode_number(&app), 4);
        app.power_event(nodo_power::Event::Off);
        assert_eq!(mode_number(&app), 5);
    }

    #[test]
    fn sparklines_scale_to_their_own_range() {
        assert_eq!(spark(&[5.0, 5.0], 900.0), vec![0.5, 0.5]);
        let s = spark(&[0.0, 10.0], 900.0);
        assert!((s[0] - 0.12).abs() < 1e-6 && (s[1] - 1.0).abs() < 1e-6);
        assert_eq!(spark(&(0..200).map(f64::from).collect::<Vec<_>>(), 140.0).len(), 10);
    }
}
