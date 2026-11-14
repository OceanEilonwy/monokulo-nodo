//! The screen's state and what touches, clicks and keys do to it. Nothing here draws or does I/O.
//!
//! The same behaviour as nodo-screen's (the terminal variant), with positions in pixels:
//! - viewing: tiles, with ☰ Arrange and ⏻ Shut down in the top bar;
//! - arranging: drag a tile to move it, drag its ⇲ corner to resize it, with Hide (then Undo),
//!   + Add panel and Save in the top bar;
//! - adding: a list of the panels not on the screen;
//! - shutting down: a confirmation, then each service stopping, then "Safe to unplug".

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use nodo_panels::layout::{COLUMNS, ROWS};
use nodo_panels::{Layout, Offered, PanelKey, PanelState, Placement, SizeRule};
use nodo_power::Event as PowerEvent;

use crate::geometry::{self, Rect};

/// How long a press must last to start arranging.
pub const LONG_PRESS: Duration = Duration::from_secs(1);
/// How long a hidden panel can be brought back with Undo.
pub const UNDO_FOR: Duration = Duration::from_secs(8);

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
    /// Opens management (the gear).
    Manage,
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
    OpenManage,
}

/// Keys, as the window reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Tab,
    BackTab,
    Left,
    Right,
    Up,
    Down,
}

/// A panel hidden a moment ago, which Undo puts back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hidden {
    pub index: usize,
    pub placement: Placement,
    pub until: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DragKind {
    /// The grabbed cell relative to the tile's top-left cell, and the grabbed point relative to
    /// the tile's top-left corner, so that the tile follows the finger without a jump.
    Move { dx: u8, dy: u8, ox: f32, oy: f32 },
    Resize,
}

/// A tile being dragged.
#[derive(Debug, Clone, PartialEq)]
pub struct Drag {
    pub index: usize,
    pub kind: DragKind,
    /// Where the finger is now.
    pub finger: (f32, f32),
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

/// One row of the list of panels to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerRow {
    /// A package's name above its panels.
    Package(String),
    Panel(PanelKey),
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
    /// The keyboard moved through the list, so its row is highlighted; a finger needs no highlight.
    pub picker_keys: bool,
    pub shutdown: Shutdown,
    press: Option<Instant>,
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
            picker_keys: false,
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

    /// The layout was changed by another copy of the screen (nodo-screen over SSH, say). Taken
    /// only while viewing, so that it never pulls the layout from under someone arranging.
    pub fn replace_layout(&mut self, layout: Layout) -> bool {
        if self.mode != Mode::View {
            return false;
        }
        self.layout = layout;
        true
    }

    /// The label a top-bar button shows, which also sets its width.
    pub fn button_label(&self, button: Button, now: Instant) -> String {
        match button {
            Button::Manage => "Manage".into(),
            Button::Arrange => "Arrange".into(),
            Button::ShutDown => "⏻ Shut down".into(),
            Button::Hide => "Hide".into(),
            Button::Undo => match &self.undo {
                Some(h) => {
                    let left = h.until.saturating_duration_since(now).as_secs_f32().ceil() as u64;
                    format!("Undo: show {} · {left} s", self.title_of(&h.placement.key))
                }
                None => "Undo".into(),
            },
            Button::Add => format!("+ Add panel {}", self.hidden().len()),
            Button::Save => "Save".into(),
            Button::Cancel => "Cancel".into(),
            Button::KeepRunning => "Cancel".into(),
            Button::ConfirmShutDown => "⏻ Shut down".into(),
            Button::Back => "Back to the panels".into(),
        }
    }

    /// The top bar's buttons and their rectangles, right-aligned.
    pub fn banner_buttons(&self, now: Instant) -> Vec<(Button, Rect)> {
        let list: Vec<(Button, f32)> = match self.mode {
            Mode::View => vec![(Button::Manage, 224.0), (Button::Arrange, 224.0), (Button::ShutDown, 260.0)],
            Mode::Edit => {
                let mut list = Vec::new();
                if self.undo.is_some() {
                    list.push((Button::Undo, geometry::button_width(&self.button_label(Button::Undo, now), 440.0)));
                } else {
                    list.push((Button::Hide, 224.0));
                }
                list.push((Button::Add, 320.0));
                list.push((Button::Save, 224.0));
                list
            }
            Mode::Picker => vec![(Button::Cancel, 320.0)],
            Mode::Confirm | Mode::ShuttingDown => Vec::new(),
        };
        let widths: Vec<f32> = list.iter().map(|(_, w)| *w).collect();
        list.into_iter().map(|(b, _)| b).zip(geometry::header_buttons(&widths)).collect()
    }

    /// The confirmation's and the shutdown screen's buttons.
    pub fn screen_buttons(&self) -> Vec<(Button, Rect)> {
        match self.mode {
            Mode::Confirm => {
                let (cancel, go) = geometry::confirm_buttons();
                vec![(Button::KeepRunning, cancel), (Button::ConfirmShutDown, go)]
            }
            Mode::ShuttingDown if self.shutdown.error.is_some() => vec![(Button::Back, geometry::back_button())],
            _ => Vec::new(),
        }
    }

    /// The rows of the list of panels to add: each package's name, then its panels. Rows that do
    /// not fit are left out.
    pub fn picker_rows(&self) -> Vec<(PickerRow, Rect)> {
        let header = geometry::header();
        let mut rows = Vec::new();
        let mut y = geometry::picker_top();
        let bottom = geometry::HEIGHT - geometry::PAD_BOTTOM;
        let mut package = None;
        for key in self.hidden() {
            if package.as_deref() != Some(key.package.as_str()) {
                if y + geometry::PICKER_HEADING + geometry::PICKER_ROW > bottom {
                    break;
                }
                package = Some(key.package.clone());
                rows.push((PickerRow::Package(key.package.clone()), Rect::new(header.x, y, header.w, geometry::PICKER_HEADING)));
                y += geometry::PICKER_HEADING;
            }
            if y + geometry::PICKER_ROW > bottom {
                break;
            }
            rows.push((PickerRow::Panel(key.clone()), Rect::new(header.x, y, header.w, geometry::PICKER_ROW)));
            y += geometry::PICKER_ROW + geometry::PICKER_ROW_GAP;
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
            self.picker_keys = false;
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
            Button::Manage => return Effect::OpenManage,
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

    /// Handles a key: the same keys as nodo-screen.
    pub fn key(&mut self, key: Key, now: Instant) -> Effect {
        match self.mode {
            Mode::View => match key {
                Key::Char('e') => return self.act(Button::Arrange, now),
                Key::Char('p') => return self.act(Button::ShutDown, now),
                _ => {}
            },
            Mode::Confirm => match key {
                Key::Enter | Key::Char('y') => return self.act(Button::ConfirmShutDown, now),
                Key::Escape | Key::Char('n') => return self.act(Button::KeepRunning, now),
                _ => {}
            },
            Mode::ShuttingDown => {
                if self.shutdown.error.is_some() && matches!(key, Key::Escape | Key::Enter) {
                    return self.act(Button::Back, now);
                }
            }
            Mode::Picker => {
                let hidden = self.hidden();
                self.picker_keys = true;
                match key {
                    Key::Escape => return self.act(Button::Cancel, now),
                    Key::Down | Key::Tab if !hidden.is_empty() => self.picker_row = (self.picker_row + 1) % hidden.len(),
                    Key::Up | Key::BackTab if !hidden.is_empty() => self.picker_row = (self.picker_row + hidden.len() - 1) % hidden.len(),
                    Key::Enter => {
                        if let Some(key) = hidden.get(self.picker_row).cloned() {
                            self.add_panel(&key);
                        }
                    }
                    _ => {}
                }
            }
            Mode::Edit => return self.edit_key(key, now),
        }
        Effect::None
    }

    fn edit_key(&mut self, key: Key, now: Instant) -> Effect {
        let count = self.layout.placements.len();
        match key {
            Key::Enter | Key::Escape | Key::Char('s') => return self.finish_editing(),
            Key::Char('a') => return self.act(Button::Add, now),
            Key::Char('x') => return self.act(Button::Hide, now),
            Key::Char('u') => return self.act(Button::Undo, now),
            Key::Tab | Key::Right | Key::Down if count > 0 => {
                self.selected = Some(self.selected.map_or(0, |i| (i + 1) % count));
            }
            Key::BackTab | Key::Left | Key::Up if count > 0 => {
                self.selected = Some(self.selected.map_or(count - 1, |i| (i + count - 1) % count));
            }
            Key::Char(c) => {
                let Some(index) = self.selected else { return Effect::None };
                let resize = c.is_ascii_uppercase();
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

    fn button_at(buttons: Vec<(Button, Rect)>, x: f32, y: f32) -> Option<Button> {
        buttons.into_iter().find(|(_, rect)| rect.contains(x, y)).map(|(b, _)| b)
    }

    /// A finger (or the mouse's left button) went down.
    pub fn touch_down(&mut self, x: f32, y: f32, now: Instant) -> Effect {
        match self.mode {
            Mode::View => {
                if let Some(button) = Self::button_at(self.banner_buttons(now), x, y) {
                    return self.act(button, now);
                }
                self.press = Some(now);
            }
            Mode::Confirm | Mode::ShuttingDown => {
                if let Some(button) = Self::button_at(self.screen_buttons(), x, y) {
                    return self.act(button, now);
                }
            }
            Mode::Picker => {
                if let Some(button) = Self::button_at(self.banner_buttons(now), x, y) {
                    return self.act(button, now);
                }
                let tapped = self.picker_rows().into_iter().find_map(|(kind, rect)| match kind {
                    PickerRow::Panel(key) if rect.contains(x, y) => Some(key),
                    _ => None,
                });
                if let Some(key) = tapped {
                    self.add_panel(&key);
                }
            }
            Mode::Edit => return self.edit_down(x, y, now),
        }
        Effect::None
    }

    fn edit_down(&mut self, x: f32, y: f32, now: Instant) -> Effect {
        if y < geometry::grid().y {
            return match Self::button_at(self.banner_buttons(now), x, y) {
                Some(button) => self.act(button, now),
                None => Effect::None,
            };
        }
        let Some((cx, cy)) = geometry::cell_at(x, y) else { return Effect::None };
        // The selected tile's grip resizes it.
        if let Some(index) = self.selected {
            let p = &self.layout.placements[index];
            if geometry::grip(geometry::cells(p.x, p.y, p.w, p.h)).contains(x, y) {
                self.drag = Some(Drag { index, kind: DragKind::Resize, finger: (x, y), target: None, moved: false });
                return Effect::None;
            }
        }
        match self.layout.at(cx, cy) {
            Some(index) => {
                let p = &self.layout.placements[index];
                let rect = geometry::cells(p.x, p.y, p.w, p.h);
                let kind = DragKind::Move { dx: cx - p.x, dy: cy - p.y, ox: x - rect.x, oy: y - rect.y };
                self.selected = Some(index);
                self.add_at = None;
                self.message = None;
                self.drag = Some(Drag { index, kind, finger: (x, y), target: None, moved: false });
            }
            None => {
                self.selected = None;
                self.add_at = Some((cx, cy));
                self.message = if self.hidden().is_empty() { None } else { Some("+ Add panel puts the next panel here".into()) };
            }
        }
        Effect::None
    }

    /// The finger moved while down.
    pub fn touch_move(&mut self, x: f32, y: f32) {
        if self.mode != Mode::Edit {
            return;
        }
        let Some(mut drag) = self.drag.take() else { return };
        let Some(current) = self.layout.placements.get(drag.index).cloned() else { return };
        drag.finger = (x, y);
        let (cx, cy) = geometry::nearest_cell(x, y);
        let (candidate, rule) = match drag.kind {
            DragKind::Move { dx, dy, .. } => {
                let ax = cx.saturating_sub(dx).min(COLUMNS - current.w);
                let ay = cy.saturating_sub(dy).min(ROWS - current.h);
                (Placement { x: ax, y: ay, ..current.clone() }, None)
            }
            DragKind::Resize => {
                let rule = self.rule_of(drag.index);
                let (min, max) = rule.map_or(([1, 1], [COLUMNS, ROWS]), |r| (r.min, r.max));
                let w = (cx + 1).saturating_sub(current.x).clamp(min[0], max[0]);
                let h = (cy + 1).saturating_sub(current.y).clamp(min[1], max[1]);
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
    use crate::geometry::{cells, grip};
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

    pub fn app() -> App {
        let offered = vec![offered("a", [2, 1], [4, 2], [4, 2]), offered("b", [2, 1], [4, 2], [2, 1]), offered("c", [1, 1], [2, 2], [2, 1])];
        let mut layout = Layout::default();
        layout.place_all(&nodo_panels::size_rules(&offered));
        layout.hide(2);
        App::new(offered, layout, Instant::now())
    }

    fn centre(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    fn button(app: &App, which: Button) -> Rect {
        let now = Instant::now();
        app.banner_buttons(now).into_iter().chain(app.screen_buttons()).find(|(b, _)| *b == which).unwrap_or_else(|| panic!("{which:?} not shown")).1
    }

    fn tap(app: &mut App, which: Button) -> Effect {
        let (x, y) = centre(button(app, which));
        let effect = app.touch_down(x, y, Instant::now());
        app.touch_up();
        effect
    }

    fn cell_centre(x: u8, y: u8) -> (f32, f32) {
        centre(cells(x, y, 1, 1))
    }

    fn drag(app: &mut App, from: (f32, f32), to: (f32, f32)) {
        app.touch_down(from.0, from.1, Instant::now());
        app.touch_move(to.0, to.1);
        app.touch_up();
    }

    #[test]
    fn every_touch_target_is_finger_sized() {
        let mut app = app();
        let check = |app: &App| {
            for (b, r) in app.banner_buttons(Instant::now()).into_iter().chain(app.screen_buttons()) {
                assert!(r.w >= geometry::MIN_TARGET && r.h >= geometry::MIN_TARGET, "{b:?} is {r:?}");
                assert!(r.right() <= geometry::WIDTH, "{b:?} leaves the screen");
            }
        };
        check(&app);
        tap(&mut app, Button::Arrange);
        check(&app);
        tap(&mut app, Button::Add);
        assert_eq!(app.mode, Mode::Picker);
        check(&app);
        for (row, r) in app.picker_rows() {
            if let PickerRow::Panel(_) = row {
                assert!(r.h >= geometry::MIN_TARGET);
            }
        }
        app.mode = Mode::Confirm;
        check(&app);
    }

    #[test]
    fn hold_or_the_arrange_button_starts_arranging() {
        let mut app = app();
        let t0 = Instant::now();
        app.touch_down(500.0, 500.0, t0);
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
        let g = centre(grip(cells(p.x, p.y, p.w, p.h)));
        app.touch_down(g.0, g.1, Instant::now());
        app.touch_move(1900.0, 1070.0);
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
        let (x, y) = cell_centre(4, 0);
        app.touch_down(x, y, t0);
        app.touch_up();
        tap(&mut app, Button::Hide);
        assert_eq!(app.layout.placements.len(), 1);
        assert!(app.banner_buttons(t0).iter().any(|(b, _)| *b == Button::Undo));
        assert!(!app.banner_buttons(t0).iter().any(|(b, _)| *b == Button::Hide), "Undo takes Hide's place");
        let hidden_at = app.undo.as_ref().unwrap().until - UNDO_FOR;
        assert_eq!(app.button_label(Button::Undo, hidden_at), "Undo: show b · 8 s");
        assert_eq!(app.button_label(Button::Undo, hidden_at + Duration::from_millis(1500)), "Undo: show b · 7 s");
        tap(&mut app, Button::Undo);
        assert_eq!(app.layout.placements.len(), 2, "back where it was");
        // Tap a free cell, then add c from the picker: it goes there.
        let (x, y) = cell_centre(7, 3);
        app.touch_down(x, y, t0);
        app.touch_up();
        assert_eq!(app.add_at, Some((7, 3)));
        tap(&mut app, Button::Add);
        let rows = app.picker_rows();
        assert_eq!(rows[0].0, PickerRow::Package("c".into()));
        let (_, rect) = rows.iter().find(|(r, _)| matches!(r, PickerRow::Panel(_))).unwrap();
        let (x, y) = centre(*rect);
        app.touch_down(x, y, t0);
        assert_eq!(app.mode, Mode::Edit);
        let added = app.layout.placements.iter().find(|p| p.key.package == "c").unwrap();
        assert_eq!((added.x + added.w - 1, added.y), (7, 3), "placed over the tapped cell");
        tap(&mut app, Button::Add);
        assert_eq!(app.mode, Mode::Edit, "nothing left to add");
        assert!(app.message.is_some());
    }

    #[test]
    fn undo_runs_out() {
        let mut app = app();
        let t0 = Instant::now();
        app.act(Button::Arrange, t0);
        app.selected = Some(0);
        app.act(Button::Hide, t0);
        app.tick(t0 + UNDO_FOR - Duration::from_millis(1));
        assert!(app.undo.is_some());
        app.tick(t0 + UNDO_FOR);
        assert!(app.undo.is_none());
    }

    #[test]
    fn keys_still_arrange() {
        let mut app = app();
        let now = Instant::now();
        app.key(Key::Char('e'), now);
        app.key(Key::Tab, now);
        app.key(Key::Char('j'), now);
        assert_eq!(app.layout.placements[0].y, 1);
        app.key(Key::Char('H'), now);
        assert_eq!(app.layout.placements[0].w, 3);
        app.key(Key::Char('x'), now);
        app.key(Key::Char('u'), now);
        assert_eq!(app.layout.placements.len(), 2);
        app.key(Key::Char('a'), now);
        assert_eq!(app.mode, Mode::Picker);
        app.key(Key::Enter, now);
        assert_eq!(app.layout.placements.len(), 3);
        assert_eq!(app.key(Key::Enter, now), Effect::SaveLayout);
    }

    #[test]
    fn shutting_down_needs_a_confirmation_and_follows_progress() {
        let mut app = app();
        let now = Instant::now();
        app.key(Key::Char('p'), now);
        assert_eq!(app.mode, Mode::Confirm);
        assert_eq!(app.key(Key::Escape, now), Effect::None);
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
        app.key(Key::Char('e'), Instant::now());
        assert!(!app.replace_layout(Layout::default()), "not while arranging");
    }
}
