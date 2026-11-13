//! The screen's state and what keys, touches and clicks do to it. Nothing here draws or does I/O.
//!
//! The design is `docs/design/tui/ScreenTilesOled*.dc.html` and `ScreenShutdown*.dc.html`:
//! - viewing: tiles, with ☰ Arrange and ⏻ Shut down in the top bar;
//! - arranging: drag a tile to move it, drag its ⇲ corner to resize it, with Hide (then Undo),
//!   + Add panel and Save in the top bar;
//! - adding: a list of the panels not on the screen;
//! - shutting down: a confirmation, then each service stopping, then "Safe to unplug".
//!
//! Every touch target is at least 6 columns by 3 rows, about 96 pixels square on the T6N's panel.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use nodo_panels::layout::{COLUMNS, ROWS};
use nodo_panels::{Layout, Offered, PanelKey, PanelState, Placement, SizeRule};
use nodo_power::Event as PowerEvent;
use ratatui::layout::Rect;

/// How long a press must last to start arranging.
pub const LONG_PRESS: Duration = Duration::from_secs(1);
/// How long a hidden panel can be brought back with Undo.
pub const UNDO_FOR: Duration = Duration::from_secs(8);
/// Rows the top bar takes: its buttons are three rows tall.
pub const BANNER_ROWS: u16 = 3;
/// The resize grip in a selected tile's bottom-right corner.
pub const GRIP: (u16, u16) = (6, 3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    View,
    Edit,
    /// The list of panels to add, opened from arranging.
    Picker,
    /// "Shut down the Nodo?"
    Confirm,
    ShuttingDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Arrange,
    ShutDown,
    Hide,
    /// Takes Hide's place for a few seconds after a panel is hidden.
    Undo,
    Add,
    Save,
    /// Closes the list of panels to add.
    Cancel,
    /// The confirmation's two buttons.
    KeepRunning,
    ConfirmShutDown,
    /// Back to the panels after a shutdown that could not start.
    Back,
}

/// What the caller has to do after an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    None,
    SaveLayout,
    PowerOff,
}

/// A panel hidden a moment ago, which Undo puts back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hidden {
    pub index: usize,
    pub placement: Placement,
    pub until: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragKind {
    /// The grabbed cell relative to the tile's top-left cell, and the grabbed terminal position
    /// relative to the tile's top-left corner, so that the tile follows the finger without a jump.
    Move { dx: u8, dy: u8, column: u16, row: u16 },
    Resize,
}

/// A tile being dragged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drag {
    pub index: usize,
    pub kind: DragKind,
    /// Where the finger is now, in terminal cells.
    pub finger: (u16, u16),
    /// Where the tile would land if let go now; None when that place is taken or outside its sizes.
    pub target: Option<Placement>,
    /// The finger has left the tile's own place, so letting go is a move and not a tap.
    pub moved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Stopping,
    Stopped,
    /// Did not stop in time; runit stops it as the device turns off.
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub name: String,
    pub state: StepState,
    pub millis: Option<u64>,
}

/// What the shutdown has done so far, from nodo-power's progress lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shutdown {
    pub services: Vec<Step>,
    pub total: usize,
    pub syncing: bool,
    pub synced: bool,
    /// Everything is stopped and written: safe to unplug.
    pub off: bool,
    pub error: Option<String>,
}

impl Shutdown {
    pub fn stopped(&self) -> usize {
        self.services.iter().filter(|s| s.state != StepState::Stopping).count()
    }
}

pub struct App {
    pub offered: Vec<Offered>,
    pub rules: BTreeMap<PanelKey, SizeRule>,
    pub layout: Layout,
    pub states: BTreeMap<PanelKey, PanelState>,
    pub mode: Mode,
    pub selected: Option<usize>,
    /// A short line for the top bar, such as why an action did nothing.
    pub message: Option<String>,
    /// The layout changed and should be saved when arranging ends.
    pub layout_changed: bool,
    pub undo: Option<Hidden>,
    pub drag: Option<Drag>,
    /// The free cell tapped last while arranging: + Add panel puts the panel there.
    pub add_at: Option<(u8, u8)>,
    /// The highlighted row in the list of panels to add, for the keyboard.
    pub picker_row: usize,
    pub shutdown: Shutdown,
    press: Option<Instant>,
}

/// Where the grid sits in the terminal: below the top bar, to the bottom.
pub fn grid_area(size: Rect) -> Rect {
    Rect { x: size.x, y: size.y + BANNER_ROWS, width: size.width, height: size.height.saturating_sub(BANNER_ROWS) }
}

/// The terminal rectangle of grid cells `x..x+w`, `y..y+h`. Cells share out the remainder, so
/// the grid always fills the area.
pub fn cells_rect(area: Rect, x: u8, y: u8, w: u8, h: u8) -> Rect {
    let col = |i: u8| area.x + (u32::from(area.width) * u32::from(i) / u32::from(COLUMNS)) as u16;
    let row = |i: u8| area.y + (u32::from(area.height) * u32::from(i) / u32::from(ROWS)) as u16;
    Rect { x: col(x), y: row(y), width: col(x + w) - col(x), height: row(y + h) - row(y) }
}

/// A tile's box inside its cells: one column and one row of gutter to the right and below.
pub fn tile_rect(cells: Rect) -> Rect {
    Rect { width: cells.width.saturating_sub(1), height: cells.height.saturating_sub(1), ..cells }
}

/// The resize grip of a tile drawn in `tile`.
pub fn grip_rect(tile: Rect) -> Rect {
    let w = GRIP.0.min(tile.width);
    let h = GRIP.1.min(tile.height);
    Rect { x: tile.x + tile.width - w, y: tile.y + tile.height - h, width: w, height: h }
}

/// The grid cell under a terminal position.
pub fn cell_at(area: Rect, column: u16, row: u16) -> Option<(u8, u8)> {
    if !contains(area, column, row) {
        return None;
    }
    let x = (u32::from(column - area.x) * u32::from(COLUMNS) / u32::from(area.width)) as u8;
    let y = (u32::from(row - area.y) * u32::from(ROWS) / u32::from(area.height)) as u8;
    Some((x.min(COLUMNS - 1), y.min(ROWS - 1)))
}

/// The nearest grid cell to a terminal position, for a finger dragged past the grid's edge.
fn nearest_cell(area: Rect, column: u16, row: u16) -> (u8, u8) {
    let column = column.clamp(area.x, area.x + area.width.saturating_sub(1));
    let row = row.clamp(area.y, area.y + area.height.saturating_sub(1));
    cell_at(area, column, row).unwrap_or((0, 0))
}

pub fn contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x && column < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// One row of the list of panels to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerRow {
    /// A package's name above its panels.
    Package(String),
    Panel(PanelKey),
}

impl App {
    pub fn new(offered: Vec<Offered>, layout: Layout, now: Instant) -> Self {
        let rules = nodo_panels::size_rules(&offered);
        let states = offered.iter().map(|o| (o.key.clone(), PanelState::new(now))).collect();
        App {
            offered,
            rules,
            layout,
            states,
            mode: Mode::View,
            selected: None,
            message: None,
            layout_changed: false,
            undo: None,
            drag: None,
            add_at: None,
            picker_row: 0,
            shutdown: Shutdown::default(),
            press: None,
        }
    }

    pub fn offered(&self, key: &PanelKey) -> Option<&Offered> {
        self.offered.iter().find(|o| &o.key == key)
    }

    pub fn title_of(&self, key: &PanelKey) -> String {
        self.offered(key).map_or_else(|| key.panel.clone(), |o| o.title.clone())
    }

    /// Panels that can be shown but are not, in order.
    pub fn hidden(&self) -> Vec<PanelKey> {
        self.layout.hidden(&self.rules).into_iter().cloned().collect()
    }

    pub fn rule_of(&self, index: usize) -> Option<SizeRule> {
        self.layout.placements.get(index).and_then(|p| self.rules.get(&p.key)).copied()
    }

    /// The layout was changed by another copy of the screen (say, one over SSH). Taken only while
    /// viewing, so that it never pulls the layout from under someone arranging.
    pub fn replace_layout(&mut self, layout: Layout) -> bool {
        if self.mode != Mode::View {
            return false;
        }
        self.layout = layout;
        true
    }

    /// The top bar's buttons and their rectangles, right-aligned.
    pub fn banner_buttons(&self, size: Rect) -> Vec<(Button, Rect)> {
        let list: Vec<(Button, u16)> = match self.mode {
            Mode::View => vec![(Button::Arrange, 15), (Button::ShutDown, 16)],
            Mode::Edit => {
                let mut list = Vec::new();
                if let Some(hidden) = &self.undo {
                    let title = self.title_of(&hidden.placement.key).chars().count() as u16;
                    list.push((Button::Undo, title + 22));
                } else {
                    list.push((Button::Hide, 12));
                }
                list.push((Button::Add, 19));
                list.push((Button::Save, 12));
                list
            }
            Mode::Picker => vec![(Button::Cancel, 14)],
            Mode::Confirm | Mode::ShuttingDown => Vec::new(),
        };
        let mut right = (size.x + size.width).saturating_sub(1);
        let mut rects: Vec<(Button, Rect)> = list
            .into_iter()
            .rev()
            .map(|(button, width)| {
                let x = right.saturating_sub(width);
                right = x.saturating_sub(2);
                (button, Rect { x, y: size.y, width, height: BANNER_ROWS.min(size.height) })
            })
            .collect();
        rects.reverse();
        rects
    }

    /// The confirmation's and the shutdown screen's buttons.
    pub fn screen_buttons(&self, size: Rect) -> Vec<(Button, Rect)> {
        let (w, h) = (36u16, 7u16);
        let y = size.y + size.height / 2 + 2;
        let centre = size.x + size.width / 2;
        match self.mode {
            Mode::Confirm => vec![
                (Button::KeepRunning, Rect { x: centre.saturating_sub(w + 2), y, width: w, height: h }),
                (Button::ConfirmShutDown, Rect { x: centre + 2, y, width: w, height: h }),
            ],
            Mode::ShuttingDown if self.shutdown.error.is_some() => {
                vec![(Button::Back, Rect { x: centre.saturating_sub(w / 2), y: size.y + size.height.saturating_sub(h + 2), width: w, height: h })]
            }
            _ => Vec::new(),
        }
    }

    /// The rows of the list of panels to add: each package's name, then its panels, three rows
    /// tall. Rows that do not fit are left out.
    pub fn picker_rows(&self, size: Rect) -> Vec<(PickerRow, Rect)> {
        let mut rows = Vec::new();
        let mut y = size.y + BANNER_ROWS + 1;
        let bottom = size.y + size.height;
        let mut package = None;
        for key in self.hidden() {
            if package.as_deref() != Some(key.package.as_str()) {
                if y + 1 + 3 > bottom {
                    break;
                }
                package = Some(key.package.clone());
                rows.push((PickerRow::Package(key.package.clone()), Rect { x: size.x + 1, y, width: size.width.saturating_sub(2), height: 1 }));
                y += 1;
            }
            if y + 3 > bottom {
                break;
            }
            rows.push((PickerRow::Panel(key.clone()), Rect { x: size.x + 1, y, width: size.width.saturating_sub(2), height: 3 }));
            y += 4;
        }
        rows
    }

    fn start_editing(&mut self) {
        self.mode = Mode::Edit;
        self.message = None;
        self.selected = None;
        self.undo = None;
        self.drag = None;
        self.add_at = None;
        self.press = None;
    }

    /// Leaves arranging.
    fn finish_editing(&mut self) -> Effect {
        self.mode = Mode::View;
        self.selected = None;
        self.message = None;
        self.undo = None;
        self.drag = None;
        self.add_at = None;
        if std::mem::take(&mut self.layout_changed) {
            Effect::SaveLayout
        } else {
            Effect::None
        }
    }

    fn changed(&mut self, ok: bool, refused: &str) {
        if ok {
            self.layout_changed = true;
            self.message = None;
            self.undo = None;
        } else {
            self.message = Some(refused.to_string());
        }
    }

    fn open_picker(&mut self) {
        if self.hidden().is_empty() {
            self.message = Some("every panel is already on the screen".into());
        } else {
            self.mode = Mode::Picker;
            self.picker_row = 0;
            self.message = None;
        }
    }

    fn add_panel(&mut self, key: &PanelKey) {
        let added = match self.add_at.take() {
            Some((x, y)) => self.layout.add_covering(key, &self.rules, x, y),
            None => self.layout.add(key, &self.rules),
        };
        self.mode = Mode::Edit;
        if added {
            self.changed(true, "");
            self.selected = self.layout.placements.iter().position(|p| &p.key == key);
        } else {
            self.message = Some("no free space: hide or shrink a tile first".into());
        }
    }

    /// Applies one step of the shutdown's progress.
    pub fn power_event(&mut self, event: PowerEvent) {
        let s = &mut self.shutdown;
        let finish = |s: &mut Shutdown, service: &str, millis: u64, state: StepState| {
            if let Some(step) = s.services.iter_mut().find(|x| x.name == service) {
                step.millis = Some(millis);
                step.state = state;
            }
        };
        match event {
            PowerEvent::Services(n) => s.total = n,
            PowerEvent::Stopping(name) => s.services.push(Step { name, state: StepState::Stopping, millis: None }),
            PowerEvent::Stopped { service, millis } => finish(s, &service, millis, StepState::Stopped),
            PowerEvent::TimedOut { service, millis } => finish(s, &service, millis, StepState::TimedOut),
            PowerEvent::Syncing => s.syncing = true,
            PowerEvent::Synced { .. } => s.synced = true,
            PowerEvent::Off => s.off = true,
            PowerEvent::Error(text) => s.error = Some(text),
        }
    }

    /// The connection to nodo-power ended. Without "off" first, the shutdown did not happen.
    pub fn power_closed(&mut self, error: Option<String>) {
        if !self.shutdown.off && self.shutdown.error.is_none() {
            self.shutdown.error = Some(error.unwrap_or_else(|| "the shutdown service stopped answering".into()));
        }
    }

    /// Does what a button does.
    pub fn act(&mut self, button: Button, now: Instant) -> Effect {
        match button {
            Button::Arrange => self.start_editing(),
            Button::ShutDown => {
                self.mode = Mode::Confirm;
                self.press = None;
            }
            Button::KeepRunning => self.mode = Mode::View,
            Button::ConfirmShutDown => {
                self.mode = Mode::ShuttingDown;
                self.shutdown = Shutdown::default();
                return Effect::PowerOff;
            }
            Button::Back => {
                self.mode = Mode::View;
                self.shutdown = Shutdown::default();
            }
            Button::Save => return self.finish_editing(),
            Button::Cancel => {
                self.mode = Mode::Edit;
                self.add_at = None;
            }
            Button::Add => self.open_picker(),
            Button::Hide => match self.selected {
                Some(index) if index < self.layout.placements.len() => {
                    let placement = self.layout.placements[index].clone();
                    self.layout.hide(index);
                    self.layout_changed = true;
                    self.selected = None;
                    self.message = None;
                    self.undo = Some(Hidden { index, placement, until: now + UNDO_FOR });
                }
                _ => self.message = Some("tap a tile first, then Hide".into()),
            },
            Button::Undo => {
                if let Some(hidden) = self.undo.take() {
                    if self.layout.restore(hidden.index, hidden.placement) {
                        self.selected = Some(hidden.index.min(self.layout.placements.len() - 1));
                        self.message = None;
                    } else {
                        self.message = Some("its space has been taken: use + Add panel".into());
                    }
                }
            }
        }
        Effect::None
    }

    /// Handles a key.
    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers, now: Instant) -> Effect {
        match self.mode {
            Mode::View => match code {
                KeyCode::Char('e') => return self.act(Button::Arrange, now),
                KeyCode::Char('p') => return self.act(Button::ShutDown, now),
                _ => {}
            },
            Mode::Confirm => match code {
                KeyCode::Enter | KeyCode::Char('y') => return self.act(Button::ConfirmShutDown, now),
                KeyCode::Esc | KeyCode::Char('n') => return self.act(Button::KeepRunning, now),
                _ => {}
            },
            Mode::ShuttingDown => {
                if self.shutdown.error.is_some() && matches!(code, KeyCode::Esc | KeyCode::Enter) {
                    return self.act(Button::Back, now);
                }
            }
            Mode::Picker => {
                let hidden = self.hidden();
                match code {
                    KeyCode::Esc => return self.act(Button::Cancel, now),
                    KeyCode::Down | KeyCode::Tab if !hidden.is_empty() => self.picker_row = (self.picker_row + 1) % hidden.len(),
                    KeyCode::Up | KeyCode::BackTab if !hidden.is_empty() => self.picker_row = (self.picker_row + hidden.len() - 1) % hidden.len(),
                    KeyCode::Enter => {
                        if let Some(key) = hidden.get(self.picker_row).cloned() {
                            self.add_panel(&key);
                        }
                    }
                    _ => {}
                }
            }
            Mode::Edit => return self.edit_key(code, modifiers, now),
        }
        Effect::None
    }

    fn edit_key(&mut self, code: KeyCode, modifiers: KeyModifiers, now: Instant) -> Effect {
        let count = self.layout.placements.len();
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        match code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('s') => return self.finish_editing(),
            KeyCode::Char('a') => return self.act(Button::Add, now),
            KeyCode::Char('x') => return self.act(Button::Hide, now),
            KeyCode::Char('u') => return self.act(Button::Undo, now),
            KeyCode::Tab | KeyCode::Right | KeyCode::Down if count > 0 => {
                self.selected = Some(self.selected.map_or(0, |i| (i + 1) % count));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Up if count > 0 => {
                self.selected = Some(self.selected.map_or(count - 1, |i| (i + count - 1) % count));
            }
            KeyCode::Char(c) => {
                let Some(index) = self.selected else { return Effect::None };
                let resize = c.is_ascii_uppercase() || (shift && c.is_ascii_lowercase());
                let delta = match c.to_ascii_lowercase() {
                    'h' => (-1, 0),
                    'l' => (1, 0),
                    'k' => (0, -1),
                    'j' => (0, 1),
                    _ => return Effect::None,
                };
                if resize {
                    if let Some(rule) = self.rule_of(index) {
                        let ok = self.layout.try_resize(index, delta.0, delta.1, &rule);
                        self.changed(ok, "that size is outside this panel's range or overlaps another");
                    }
                } else {
                    let ok = self.layout.try_move(index, delta.0, delta.1);
                    self.changed(ok, "another tile or the edge is in the way");
                }
            }
            _ => {}
        }
        Effect::None
    }

    fn button_at(buttons: Vec<(Button, Rect)>, column: u16, row: u16) -> Option<Button> {
        buttons.into_iter().find(|(_, rect)| contains(*rect, column, row)).map(|(b, _)| b)
    }

    /// A finger (or the mouse's left button) went down at a terminal position.
    pub fn touch_down(&mut self, size: Rect, column: u16, row: u16, now: Instant) -> Effect {
        match self.mode {
            Mode::View => {
                if let Some(button) = Self::button_at(self.banner_buttons(size), column, row) {
                    return self.act(button, now);
                }
                self.press = Some(now);
            }
            Mode::Confirm | Mode::ShuttingDown => {
                if let Some(button) = Self::button_at(self.screen_buttons(size), column, row) {
                    return self.act(button, now);
                }
            }
            Mode::Picker => {
                if let Some(button) = Self::button_at(self.banner_buttons(size), column, row) {
                    return self.act(button, now);
                }
                let tapped = self.picker_rows(size).into_iter().find_map(|(kind, rect)| match kind {
                    PickerRow::Panel(key) if contains(rect, column, row) => Some(key),
                    _ => None,
                });
                if let Some(key) = tapped {
                    self.add_panel(&key);
                }
            }
            Mode::Edit => return self.edit_down(size, column, row, now),
        }
        Effect::None
    }

    fn edit_down(&mut self, size: Rect, column: u16, row: u16, now: Instant) -> Effect {
        if row < size.y + BANNER_ROWS {
            return match Self::button_at(self.banner_buttons(size), column, row) {
                Some(button) => self.act(button, now),
                None => Effect::None,
            };
        }
        let grid = grid_area(size);
        let Some((x, y)) = cell_at(grid, column, row) else { return Effect::None };
        // The selected tile's grip resizes it.
        if let Some(index) = self.selected {
            let p = &self.layout.placements[index];
            if contains(grip_rect(tile_rect(cells_rect(grid, p.x, p.y, p.w, p.h))), column, row) {
                self.drag = Some(Drag { index, kind: DragKind::Resize, finger: (column, row), target: None, moved: false });
                return Effect::None;
            }
        }
        match self.layout.at(x, y) {
            Some(index) => {
                let p = &self.layout.placements[index];
                let rect = cells_rect(grid, p.x, p.y, p.w, p.h);
                let kind = DragKind::Move { dx: x - p.x, dy: y - p.y, column: column - rect.x, row: row - rect.y };
                self.selected = Some(index);
                self.add_at = None;
                self.message = None;
                self.drag = Some(Drag { index, kind, finger: (column, row), target: None, moved: false });
            }
            None => {
                self.selected = None;
                self.add_at = Some((x, y));
                self.message = if self.hidden().is_empty() { None } else { Some("+ Add panel puts the next panel here".into()) };
            }
        }
        Effect::None
    }

    /// The finger moved while down.
    pub fn touch_move(&mut self, size: Rect, column: u16, row: u16) {
        if self.mode != Mode::Edit {
            return;
        }
        let Some(mut drag) = self.drag.take() else { return };
        let Some(current) = self.layout.placements.get(drag.index).cloned() else { return };
        drag.finger = (column, row);
        let (x, y) = nearest_cell(grid_area(size), column, row);
        let (candidate, rule) = match drag.kind {
            DragKind::Move { dx, dy, .. } => {
                let ax = x.saturating_sub(dx).min(COLUMNS - current.w);
                let ay = y.saturating_sub(dy).min(ROWS - current.h);
                (Placement { x: ax, y: ay, ..current.clone() }, None)
            }
            DragKind::Resize => {
                let rule = self.rule_of(drag.index);
                let (min, max) = rule.map_or(([1, 1], [COLUMNS, ROWS]), |r| (r.min, r.max));
                let w = (x + 1).saturating_sub(current.x).clamp(min[0], max[0]);
                let h = (y + 1).saturating_sub(current.y).clamp(min[1], max[1]);
                (Placement { w, h, ..current.clone() }, rule)
            }
        };
        if candidate != current {
            drag.moved = true;
        }
        drag.target = self.layout.allows(drag.index, &candidate, rule.as_ref()).then_some(candidate);
        self.message = if drag.moved && drag.target.is_none() { Some("it does not fit there".into()) } else { None };
        self.drag = Some(drag);
    }

    /// The finger lifted: a drag lands where it was shown.
    pub fn touch_up(&mut self) {
        self.press = None;
        let Some(drag) = self.drag.take() else { return };
        if !drag.moved {
            return;
        }
        match drag.target {
            Some(target) if Some(&target) != self.layout.placements.get(drag.index) => {
                let rule = self.rule_of(drag.index);
                let ok = self.layout.try_set(drag.index, target, rule.as_ref());
                self.changed(ok, "it does not fit there");
            }
            Some(_) => {}
            None => self.message = Some("it does not fit there: it went back".into()),
        }
    }

    /// Called regularly: a press held long enough starts arranging, and Undo runs out.
    pub fn tick(&mut self, now: Instant) {
        if let Some(started) = self.press {
            if self.mode == Mode::View && now.duration_since(started) >= LONG_PRESS {
                self.start_editing();
            }
        }
        if self.undo.as_ref().is_some_and(|u| now >= u.until) {
            self.undo = None;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nodo_manifest::PanelSource;

    pub fn offered(package: &str, min: [u8; 2], max: [u8; 2], initial: [u8; 2]) -> Offered {
        Offered {
            key: PanelKey::new(package, "p"),
            title: package.to_string(),
            refresh: Duration::from_secs(10),
            rule: SizeRule { min, max, initial },
            source: PanelSource::Device {},
            script: Ok(String::new()),
        }
    }

    fn app() -> App {
        let offered = vec![offered("a", [2, 1], [4, 2], [4, 2]), offered("b", [2, 1], [4, 2], [2, 1]), offered("c", [1, 1], [2, 2], [2, 1])];
        let mut layout = Layout::default();
        layout.place_all(&nodo_panels::size_rules(&offered));
        layout.hide(2);
        App::new(offered, layout, Instant::now())
    }

    const SIZE: Rect = Rect { x: 0, y: 0, width: 120, height: 33 };

    fn centre(rect: Rect) -> (u16, u16) {
        (rect.x + rect.width / 2, rect.y + rect.height / 2)
    }

    fn button(app: &App, button: Button) -> Rect {
        app.banner_buttons(SIZE).into_iter().chain(app.screen_buttons(SIZE)).find(|(b, _)| *b == button).unwrap_or_else(|| panic!("{button:?} not shown")).1
    }

    fn tap(app: &mut App, which: Button) -> Effect {
        let (c, r) = centre(button(app, which));
        let effect = app.touch_down(SIZE, c, r, Instant::now());
        app.touch_up();
        effect
    }

    fn cell_centre(x: u8, y: u8) -> (u16, u16) {
        centre(cells_rect(grid_area(SIZE), x, y, 1, 1))
    }

    fn drag(app: &mut App, from: (u16, u16), to: (u16, u16)) {
        app.touch_down(SIZE, from.0, from.1, Instant::now());
        app.touch_move(SIZE, to.0, to.1);
        app.touch_up();
    }

    #[test]
    fn the_grid_fills_the_terminal_below_the_top_bar() {
        let area = grid_area(SIZE);
        assert_eq!(area, Rect { x: 0, y: 3, width: 120, height: 30 });
        assert_eq!(cells_rect(area, 0, 0, 8, 4), area);
        assert_eq!(cell_at(area, 119, 32), Some((7, 3)));
        assert_eq!(cell_at(area, 10, 1), None, "the top bar is not the grid");
    }

    #[test]
    fn every_touch_target_is_at_least_six_by_three() {
        let mut app = app();
        let check = |app: &App| {
            for (b, rect) in app.banner_buttons(SIZE).into_iter().chain(app.screen_buttons(SIZE)) {
                assert!(rect.width >= 6 && rect.height >= 3, "{b:?} is {rect:?}");
                assert!(rect.x + rect.width <= SIZE.width, "{b:?} leaves the screen");
            }
        };
        check(&app);
        tap(&mut app, Button::Arrange);
        check(&app);
        tap(&mut app, Button::Add);
        assert_eq!(app.mode, Mode::Picker);
        check(&app);
        for (row, rect) in app.picker_rows(SIZE) {
            if let PickerRow::Panel(_) = row {
                assert!(rect.height >= 3);
            }
        }
        let tile = tile_rect(cells_rect(grid_area(SIZE), 0, 0, 1, 1));
        assert_eq!((grip_rect(tile).width, grip_rect(tile).height), GRIP, "even the smallest tile has a full grip");
        app.mode = Mode::Confirm;
        check(&app);
    }

    #[test]
    fn hold_or_the_arrange_button_starts_arranging() {
        let mut app = app();
        let t0 = Instant::now();
        app.touch_down(SIZE, 10, 10, t0);
        app.tick(t0 + Duration::from_millis(500));
        assert_eq!(app.mode, Mode::View, "not yet");
        app.tick(t0 + LONG_PRESS);
        assert_eq!(app.mode, Mode::Edit, "a long press starts arranging");
        app.touch_up();
        assert_eq!(tap(&mut app, Button::Save), Effect::None, "nothing changed, nothing to save");
        assert_eq!(app.mode, Mode::View);
        tap(&mut app, Button::Arrange);
        assert_eq!(app.mode, Mode::Edit, "the button starts at once");
    }

    #[test]
    fn drag_a_tile_to_move_it_and_its_grip_to_resize_it() {
        let mut app = app();
        tap(&mut app, Button::Arrange);
        // a is 4x2 at (0,0), b is 2x1 at (4,0). Drag b by its left cell to the bottom right.
        drag(&mut app, cell_centre(4, 0), cell_centre(6, 3));
        assert_eq!(app.selected, Some(1));
        let b = &app.layout.placements[1];
        assert_eq!((b.x, b.y), (6, 3), "snapped to the grid");
        // Dragging it onto a is refused and it stays.
        drag(&mut app, cell_centre(6, 3), cell_centre(1, 0));
        assert_eq!((app.layout.placements[1].x, app.layout.placements[1].y), (6, 3));
        assert!(app.message.as_deref().is_some_and(|m| m.contains("went back")));
        // Move it to (4, 2), then pull its grip to the bottom-right corner.
        drag(&mut app, cell_centre(6, 3), cell_centre(4, 2));
        let p = app.layout.placements[1].clone();
        assert_eq!((p.x, p.y), (4, 2));
        let grip = centre(grip_rect(tile_rect(cells_rect(grid_area(SIZE), p.x, p.y, p.w, p.h))));
        app.touch_down(SIZE, grip.0, grip.1, Instant::now());
        app.touch_move(SIZE, 119, 32);
        assert!(app.drag.as_ref().is_some_and(|d| d.target.is_some()), "the landing place is shown while dragging");
        app.touch_up();
        let p = &app.layout.placements[1];
        assert_eq!((p.w, p.h), (4, 2), "grew to its max_size");
        assert!(app.layout_changed);
        assert_eq!(tap(&mut app, Button::Save), Effect::SaveLayout);
    }

    #[test]
    fn hide_offers_undo_and_add_opens_the_picker() {
        let mut app = app();
        let t0 = Instant::now();
        tap(&mut app, Button::Arrange);
        let (c, r) = cell_centre(4, 0);
        app.touch_down(SIZE, c, r, t0);
        app.touch_up();
        tap(&mut app, Button::Hide);
        assert_eq!(app.layout.placements.len(), 1);
        assert!(app.banner_buttons(SIZE).iter().any(|(b, _)| *b == Button::Undo));
        assert!(!app.banner_buttons(SIZE).iter().any(|(b, _)| *b == Button::Hide), "Undo takes Hide's place");
        tap(&mut app, Button::Undo);
        assert_eq!(app.layout.placements.len(), 2, "back where it was");
        // Tap a free cell, then add c from the picker: it goes there.
        let (c, r) = cell_centre(7, 3);
        app.touch_down(SIZE, c, r, t0);
        app.touch_up();
        assert_eq!(app.add_at, Some((7, 3)));
        tap(&mut app, Button::Add);
        let rows = app.picker_rows(SIZE);
        assert_eq!(rows[0].0, PickerRow::Package("c".into()));
        let (_, rect) = rows.iter().find(|(r, _)| matches!(r, PickerRow::Panel(_))).unwrap();
        let (c, r) = centre(*rect);
        app.touch_down(SIZE, c, r, t0);
        assert_eq!(app.mode, Mode::Edit);
        let added = app.layout.placements.iter().find(|p| p.key.package == "c").unwrap();
        assert_eq!((added.x + added.w - 1, added.y), (7, 3), "placed over the tapped cell");
        tap(&mut app, Button::Add);
        assert_eq!(app.mode, Mode::Edit, "nothing left to add");
        assert!(app.message.is_some());
    }

    #[test]
    fn keys_still_arrange() {
        let mut app = app();
        let now = Instant::now();
        app.key(KeyCode::Char('e'), KeyModifiers::NONE, now);
        app.key(KeyCode::Tab, KeyModifiers::NONE, now);
        app.key(KeyCode::Char('j'), KeyModifiers::NONE, now);
        assert_eq!(app.layout.placements[0].y, 1);
        app.key(KeyCode::Char('H'), KeyModifiers::SHIFT, now);
        assert_eq!(app.layout.placements[0].w, 3);
        app.key(KeyCode::Char('x'), KeyModifiers::NONE, now);
        app.key(KeyCode::Char('u'), KeyModifiers::NONE, now);
        assert_eq!(app.layout.placements.len(), 2);
        app.key(KeyCode::Char('a'), KeyModifiers::NONE, now);
        assert_eq!(app.mode, Mode::Picker);
        app.key(KeyCode::Enter, KeyModifiers::NONE, now);
        assert_eq!(app.layout.placements.len(), 3);
        assert_eq!(app.key(KeyCode::Enter, KeyModifiers::NONE, now), Effect::SaveLayout);
    }

    #[test]
    fn shutting_down_needs_a_confirmation_and_follows_progress() {
        let mut app = app();
        let now = Instant::now();
        app.key(KeyCode::Char('p'), KeyModifiers::NONE, now);
        assert_eq!(app.mode, Mode::Confirm);
        assert_eq!(app.key(KeyCode::Esc, KeyModifiers::NONE, now), Effect::None);
        assert_eq!(app.mode, Mode::View, "cancelled");
        tap(&mut app, Button::ShutDown);
        assert_eq!(app.mode, Mode::Confirm, "one tap only asks");
        assert_eq!(tap(&mut app, Button::ConfirmShutDown), Effect::PowerOff);
        for line in ["services 2", "stopping cuprated", "stopping mk-pay", "stopped mk-pay 300", "timeout cuprated 300000", "syncing", "synced 4"] {
            app.power_event(PowerEvent::parse(line).unwrap());
        }
        assert_eq!(app.shutdown.stopped(), 2);
        assert_eq!(app.shutdown.services[0].state, StepState::TimedOut);
        assert!(!app.shutdown.off);
        app.power_event(PowerEvent::Off);
        app.power_closed(None);
        assert!(app.shutdown.off && app.shutdown.error.is_none(), "safe to unplug");
        // A shutdown service that is not there says so and offers a way back.
        let mut other = self::app();
        other.mode = Mode::ShuttingDown;
        other.power_closed(Some("connection refused".into()));
        assert_eq!(tap(&mut other, Button::Back), Effect::None);
        assert_eq!(other.mode, Mode::View);
    }

    #[test]
    fn another_copys_layout_is_taken_only_while_viewing() {
        let mut app = app();
        let mut other = app.layout.clone();
        other.hide(0);
        assert!(app.replace_layout(other));
        assert_eq!(app.layout.placements.len(), 1);
        app.key(KeyCode::Char('e'), KeyModifiers::NONE, Instant::now());
        assert!(!app.replace_layout(Layout::default()), "not while arranging");
    }
}
