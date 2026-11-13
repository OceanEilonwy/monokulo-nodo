//! The screen's state and what keys and touches do to it. Nothing here draws or does I/O.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use nodo_panels::layout::{COLUMNS, ROWS};
use nodo_panels::{Layout, Offered, PanelKey, PanelState, SizeRule};
use ratatui::layout::Rect;

/// How long a press must last to start arranging.
pub const LONG_PRESS: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    View,
    Edit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Add,
    Hide,
    Done,
}

/// The footer's buttons and the columns each occupies, from the left edge.
pub const BUTTONS: [(Button, &str); 3] = [(Button::Add, "[ + add ]"), (Button::Hide, "[ x hide ]"), (Button::Done, "[ done ]")];

pub fn button_spans() -> Vec<(Button, u16, u16)> {
    let mut x = 1u16;
    BUTTONS
        .iter()
        .map(|(button, label)| {
            let start = x;
            x += label.chars().count() as u16 + 2;
            (*button, start, start + label.chars().count() as u16)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drag {
    /// The finger's cell relative to the panel's top-left cell.
    Move { dx: u8, dy: u8 },
    Resize,
}

pub struct App {
    pub offered: Vec<Offered>,
    pub rules: BTreeMap<PanelKey, SizeRule>,
    pub layout: Layout,
    pub states: BTreeMap<PanelKey, PanelState>,
    pub mode: Mode,
    pub selected: Option<usize>,
    /// A short line for the footer, such as why an action did nothing.
    pub message: Option<String>,
    /// The layout changed and should be saved when arranging ends.
    pub layout_changed: bool,
    drag: Option<Drag>,
    press: Option<Instant>,
}

/// Where the grid sits in the terminal: one row for the top bar above, one for the footer below.
pub fn grid_area(size: Rect) -> Rect {
    Rect { x: size.x, y: size.y + 1, width: size.width, height: size.height.saturating_sub(2) }
}

/// The terminal rectangle of grid cells `x..x+w`, `y..y+h`. Cells share out the remainder, so
/// the grid always fills the area.
pub fn cells_rect(area: Rect, x: u8, y: u8, w: u8, h: u8) -> Rect {
    let col = |i: u8| area.x + (u32::from(area.width) * u32::from(i) / u32::from(COLUMNS)) as u16;
    let row = |i: u8| area.y + (u32::from(area.height) * u32::from(i) / u32::from(ROWS)) as u16;
    Rect { x: col(x), y: row(y), width: col(x + w) - col(x), height: row(y + h) - row(y) }
}

/// The grid cell under a terminal position.
pub fn cell_at(area: Rect, column: u16, row: u16) -> Option<(u8, u8)> {
    if column < area.x || row < area.y || column >= area.x + area.width || row >= area.y + area.height {
        return None;
    }
    let x = (u32::from(column - area.x) * u32::from(COLUMNS) / u32::from(area.width)) as u8;
    let y = (u32::from(row - area.y) * u32::from(ROWS) / u32::from(area.height)) as u8;
    Some((x.min(COLUMNS - 1), y.min(ROWS - 1)))
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
            drag: None,
            press: None,
        }
    }

    pub fn offered(&self, key: &PanelKey) -> Option<&Offered> {
        self.offered.iter().find(|o| &o.key == key)
    }

    fn start_editing(&mut self) {
        self.mode = Mode::Edit;
        self.message = None;
        if self.selected.is_none() && !self.layout.placements.is_empty() {
            self.selected = Some(0);
        }
    }

    /// Leaves arranging. True when the layout should be saved.
    fn finish_editing(&mut self) -> bool {
        self.mode = Mode::View;
        self.selected = None;
        self.drag = None;
        self.message = None;
        std::mem::take(&mut self.layout_changed)
    }

    fn rule_of(&self, index: usize) -> Option<SizeRule> {
        self.layout.placements.get(index).and_then(|p| self.rules.get(&p.key)).copied()
    }

    fn changed(&mut self, ok: bool, refused: &str) {
        if ok {
            self.layout_changed = true;
            self.message = None;
        } else {
            self.message = Some(refused.to_string());
        }
    }

    fn act(&mut self, button: Button) -> bool {
        match button {
            Button::Done => return self.finish_editing(),
            Button::Hide => {
                if let Some(index) = self.selected {
                    self.layout.hide(index);
                    self.layout_changed = true;
                    self.selected = if self.layout.placements.is_empty() { None } else { Some(index.min(self.layout.placements.len() - 1)) };
                }
            }
            Button::Add => {
                let hidden: Vec<PanelKey> = self.layout.hidden(&self.rules).into_iter().cloned().collect();
                if hidden.is_empty() {
                    self.message = Some("every panel is already on the screen".into());
                } else if let Some(key) = hidden.iter().find(|key| self.layout.add(key, &self.rules)) {
                    self.layout_changed = true;
                    self.selected = self.layout.placements.iter().position(|p| &p.key == key);
                    self.message = None;
                } else {
                    self.message = Some("no free space: hide or shrink a panel first".into());
                }
            }
        }
        false
    }

    /// Handles a key. True when the layout should be saved now.
    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.mode == Mode::View {
            if matches!(code, KeyCode::Char('e')) {
                self.start_editing();
            }
            return false;
        }
        let count = self.layout.placements.len();
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        match code {
            KeyCode::Enter | KeyCode::Esc => return self.finish_editing(),
            KeyCode::Char('a') => return self.act(Button::Add),
            KeyCode::Char('x') => return self.act(Button::Hide),
            KeyCode::Tab | KeyCode::Right | KeyCode::Down if count > 0 => {
                self.selected = Some(self.selected.map_or(0, |i| (i + 1) % count));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Up if count > 0 => {
                self.selected = Some(self.selected.map_or(0, |i| (i + count - 1) % count));
            }
            KeyCode::Char(c) => {
                let Some(index) = self.selected else { return false };
                let resize = c.is_ascii_uppercase() || (shift && c.is_ascii_lowercase());
                let delta = match c.to_ascii_lowercase() {
                    'h' => (-1, 0),
                    'l' => (1, 0),
                    'k' => (0, -1),
                    'j' => (0, 1),
                    _ => return false,
                };
                if resize {
                    if let Some(rule) = self.rule_of(index) {
                        let ok = self.layout.try_resize(index, delta.0, delta.1, &rule);
                        self.changed(ok, "that size is outside this panel's range or overlaps another");
                    }
                } else {
                    let ok = self.layout.try_move(index, delta.0, delta.1);
                    self.changed(ok, "another panel or the edge is in the way");
                }
            }
            _ => {}
        }
        false
    }

    /// A finger touched the screen at a terminal position.
    pub fn touch_down(&mut self, size: Rect, column: u16, row: u16, now: Instant) -> bool {
        if self.mode == Mode::View {
            self.press = Some(now);
            return false;
        }
        if row == size.y + size.height.saturating_sub(1) {
            if let Some((button, ..)) = button_spans().into_iter().find(|(_, a, b)| column >= *a && column < *b) {
                return self.act(button);
            }
            return false;
        }
        let area = grid_area(size);
        let Some((x, y)) = cell_at(area, column, row) else { return false };
        match self.layout.at(x, y) {
            Some(index) => {
                self.selected = Some(index);
                let p = &self.layout.placements[index];
                let rect = cells_rect(area, p.x, p.y, p.w, p.h);
                // The bottom-right corner, two columns by one row, is the resize handle.
                let corner = column + 2 >= rect.x + rect.width && row + 1 >= rect.y + rect.height;
                self.drag = Some(if corner { Drag::Resize } else { Drag::Move { dx: x - p.x, dy: y - p.y } });
            }
            None => self.selected = None,
        }
        false
    }

    pub fn touch_move(&mut self, size: Rect, column: u16, row: u16) {
        let (Some(index), Some(drag)) = (self.selected, self.drag) else { return };
        let Some((x, y)) = cell_at(grid_area(size), column, row) else { return };
        match drag {
            Drag::Move { dx, dy } => {
                let (Some(nx), Some(ny)) = (x.checked_sub(dx), y.checked_sub(dy)) else { return };
                let p = &self.layout.placements[index];
                if (p.x, p.y) != (nx, ny) {
                    let ok = self.layout.try_move_to(index, nx, ny);
                    self.changed(ok, "another panel or the edge is in the way");
                }
            }
            Drag::Resize => {
                let p = self.layout.placements[index].clone();
                let (Some(w), Some(h)) = ((x + 1).checked_sub(p.x), (y + 1).checked_sub(p.y)) else { return };
                if (w, h) != (p.w, p.h) && w > 0 && h > 0 {
                    if let Some(rule) = self.rule_of(index) {
                        let ok = self.layout.try_resize_to(index, w, h, &rule);
                        self.changed(ok, "that size is outside this panel's range or overlaps another");
                    }
                }
            }
        }
    }

    pub fn touch_up(&mut self) {
        self.press = None;
        self.drag = None;
    }

    /// Called regularly: a press held long enough starts arranging.
    pub fn tick(&mut self, now: Instant) {
        if let Some(started) = self.press {
            if self.mode == Mode::View && now.duration_since(started) >= LONG_PRESS {
                self.press = None;
                self.start_editing();
            }
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
        let offered = vec![offered("a", [2, 1], [4, 2], [4, 2]), offered("b", [2, 1], [4, 2], [2, 1])];
        let mut layout = Layout::default();
        layout.place_all(&nodo_panels::size_rules(&offered));
        App::new(offered, layout, Instant::now())
    }

    const SIZE: Rect = Rect { x: 0, y: 0, width: 120, height: 33 };

    #[test]
    fn the_grid_fills_the_terminal() {
        let area = grid_area(SIZE);
        assert_eq!(area, Rect { x: 0, y: 1, width: 120, height: 31 });
        assert_eq!(cells_rect(area, 0, 0, 8, 4), area);
        assert_eq!(cells_rect(area, 4, 2, 4, 2), Rect { x: 60, y: 16, width: 60, height: 16 });
        assert_eq!(cell_at(area, 119, 31), Some((7, 3)));
        assert_eq!(cell_at(area, 10, 0), None, "the top bar is not the grid");
    }

    #[test]
    fn keys_arrange_the_panels() {
        let mut app = app();
        assert!(!app.key(KeyCode::Char('l'), KeyModifiers::NONE), "nothing moves while viewing");
        app.key(KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Edit);
        assert_eq!(app.selected, Some(0));
        app.key(KeyCode::Char('l'), KeyModifiers::NONE);
        assert!(app.message.is_some(), "b is in the way");
        app.key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!((app.layout.placements[0].x, app.layout.placements[0].y), (0, 1));
        app.key(KeyCode::Char('H'), KeyModifiers::SHIFT);
        assert_eq!(app.layout.placements[0].w, 3);
        app.key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.layout.placements.len(), 1);
        app.key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(app.layout.placements.len(), 2);
        assert!(app.key(KeyCode::Enter, KeyModifiers::NONE), "saves when done");
        assert_eq!(app.mode, Mode::View);
    }

    #[test]
    fn touch_holds_drags_and_resizes() {
        let mut app = app();
        let t0 = Instant::now();
        app.touch_down(SIZE, 10, 5, t0);
        app.tick(t0 + Duration::from_millis(500));
        assert_eq!(app.mode, Mode::View, "not yet");
        app.tick(t0 + LONG_PRESS);
        assert_eq!(app.mode, Mode::Edit, "a long press starts arranging");
        app.touch_up();
        // b sits at grid (4, 0), 2x1: columns 60..90, rows 1..8. Drag it down a row.
        app.touch_down(SIZE, 65, 3, t0);
        assert_eq!(app.selected, Some(1));
        app.touch_move(SIZE, 65, 11);
        assert_eq!((app.layout.placements[1].x, app.layout.placements[1].y), (4, 1));
        app.touch_up();
        // Its bottom-right corner is now column 89, row 15: drag it to make b 4x1... then 4x2.
        app.touch_down(SIZE, 89, 15, t0);
        app.touch_move(SIZE, 119, 15);
        assert_eq!((app.layout.placements[1].w, app.layout.placements[1].h), (4, 1));
        app.touch_move(SIZE, 119, 23);
        assert_eq!((app.layout.placements[1].w, app.layout.placements[1].h), (4, 2));
        app.touch_up();
        // The footer's done button saves.
        let (_, done_start, _) = button_spans()[2];
        assert!(app.touch_down(SIZE, done_start + 1, 32, t0));
        assert_eq!(app.mode, Mode::View);
    }
}
