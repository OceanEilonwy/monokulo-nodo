//! Where everything sits on the 1920 x 1080 screen, in pixels.
//!
//! The design (`docs/design/device-console/*.dc.html`) is drawn at half size, 960 x 540, so every
//! measure here is the design's doubled. Rust decides every
//! rectangle a finger can hit and hands them to the .slint files, so that what is drawn and what
//! is hit can never disagree.

use nodo_panels::layout::{COLUMNS, ROWS};

pub const WIDTH: f32 = 1920.0;
pub const HEIGHT: f32 = 1080.0;
pub const PAD_TOP: f32 = 32.0;
pub const PAD_X: f32 = 40.0;
pub const PAD_BOTTOM: f32 = 32.0;
/// The glance screen's header: 56 px in the design.
pub const HEADER_HEIGHT: f32 = 112.0;
pub const HEADER_GAP: f32 = 24.0;
/// Between the header's buttons: 10 px in the design.
pub const BUTTON_GAP: f32 = 20.0;
pub const GRID_GAP: f32 = 24.0;
/// An icon-only header button (Manage, Arrange, Lock, Power): 60 x 56 in the design.
pub const ICON_BUTTON: f32 = 120.0;
/// The resize grip in a selected tile's bottom-right corner: 52 px in the design.
pub const GRIP: f32 = 104.0;
/// The smallest touch target anywhere, which the tests hold every button to: about 9 mm on the
/// T6N's panel.
#[cfg(test)]
pub const MIN_TARGET: f32 = 88.0;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// The top bar.
pub fn header() -> Rect {
    Rect::new(PAD_X, PAD_TOP, WIDTH - 2.0 * PAD_X, HEADER_HEIGHT)
}

/// The 8 x 4 grid, below the top bar.
pub fn grid() -> Rect {
    let y = PAD_TOP + HEADER_HEIGHT + HEADER_GAP;
    Rect::new(PAD_X, y, WIDTH - 2.0 * PAD_X, HEIGHT - PAD_BOTTOM - y)
}

pub fn cell_width() -> f32 {
    (grid().w - GRID_GAP * f32::from(COLUMNS - 1)) / f32::from(COLUMNS)
}

pub fn cell_height() -> f32 {
    (grid().h - GRID_GAP * f32::from(ROWS - 1)) / f32::from(ROWS)
}

/// The rectangle a tile at cells `x..x+w`, `y..y+h` fills, gaps between its own cells included.
pub fn cells(x: u8, y: u8, w: u8, h: u8) -> Rect {
    let g = grid();
    let (cw, ch) = (cell_width(), cell_height());
    Rect::new(
        g.x + f32::from(x) * (cw + GRID_GAP),
        g.y + f32::from(y) * (ch + GRID_GAP),
        f32::from(w) * cw + f32::from(w.saturating_sub(1)) * GRID_GAP,
        f32::from(h) * ch + f32::from(h.saturating_sub(1)) * GRID_GAP,
    )
}

/// The grid cell under a point, gaps counting towards the cell before them.
pub fn cell_at(px: f32, py: f32) -> Option<(u8, u8)> {
    let g = grid();
    if !g.contains(px, py) {
        return None;
    }
    Some(nearest_cell(px, py))
}

/// The nearest grid cell to a point, for a finger dragged past the grid's edge.
pub fn nearest_cell(px: f32, py: f32) -> (u8, u8) {
    let g = grid();
    let x = ((px - g.x) / (cell_width() + GRID_GAP)).floor().clamp(0.0, f32::from(COLUMNS - 1));
    let y = ((py - g.y) / (cell_height() + GRID_GAP)).floor().clamp(0.0, f32::from(ROWS - 1));
    (x as u8, y as u8)
}

/// The resize grip of a tile drawn in `tile`.
pub fn grip(tile: Rect) -> Rect {
    let w = GRIP.min(tile.w);
    let h = GRIP.min(tile.h);
    Rect::new(tile.right() - w, tile.bottom() - h, w, h)
}

/// A top-bar button's width for its label: 26 px JetBrains Mono advances 15.6 px a character.
pub fn button_width(label: &str, min: f32) -> f32 {
    (label.chars().count() as f32 * 15.6 + 64.0).max(min).ceil()
}

/// Lays buttons of the given widths along the top bar, right-aligned.
pub fn header_buttons(widths: &[f32]) -> Vec<Rect> {
    let h = header();
    let mut right = h.right();
    let mut rects: Vec<Rect> = widths
        .iter()
        .rev()
        .map(|w| {
            let r = Rect::new(right - w, h.y, *w, h.h);
            right = r.x - BUTTON_GAP;
            r
        })
        .collect();
    rects.reverse();
    rects
}

/// The power screen's three choices (PowerOff.dc.html): Cancel, Restart and Shut down, each
/// 230 x 132 in the design, side by side in a 720 wide row below the question.
pub fn confirm_buttons() -> [Rect; 3] {
    let (row, gap, h, y) = (1440.0, 28.0, 264.0, 590.0);
    let w = (row - 2.0 * gap) / 3.0;
    let x = (WIDTH - row) / 2.0;
    [0.0, 1.0, 2.0].map(|i| Rect::new(x + i * (w + gap), y, w, h))
}

/// "Back to the panels", after a shutdown that could not start.
pub fn back_button() -> Rect {
    let (w, h) = (600.0, 140.0);
    Rect::new((WIDTH - w) / 2.0, HEIGHT - PAD_BOTTOM - h - 40.0, w, h)
}

/// The console's own touch targets, drawn by `ui/console.slint` at these sizes (Rust sets its
/// `Touch` global from them at start), so the tests below hold what a finger meets.
pub mod console {
    /// A rail item, the orange pill when selected: 76 x 76 in the design.
    pub const RAIL_ITEM: f32 = 152.0;
    /// Buttons: 52 px tall in the design.
    pub const BUTTON: f32 = 104.0;
    /// A row of the Device lists: 52 px.
    pub const ROW: f32 = 104.0;
    /// A row of the services list: 62 px.
    pub const SERVICE_ROW: f32 = 124.0;
    /// The on/off switch: 72 x 44.
    pub const SWITCH_W: f32 = 144.0;
    pub const SWITCH_H: f32 = 88.0;
    /// A "Turn display off after" choice: 44 px tall.
    pub const SEGMENT: f32 = 88.0;
    /// A key of the passphrase keyboards: 66 px tall (58 on the Unlock design, grown to fill the screen).
    pub const KEY_UNLOCK: f32 = 132.0;
    pub const KEY_TEXT: f32 = 132.0;
    /// A key of the PIN pad: 92 px.
    pub const PIN_KEY: f32 = 184.0;
    #[cfg(test)]
    /// Keys are ten across the 1920 px screen, 16 px apart inside 32 px margins.
    pub const KEY_WIDTH: f32 = (super::WIDTH - 64.0 - 9.0 * 16.0) / 10.0;
    /// The Show/Hide eye inside the passphrase box: 48 px.
    pub const EYE: f32 = 96.0;
    /// The "Manage via web" chip in a page's header: its QR, 52 px.
    pub const WEB_CHIP: f32 = 104.0;
}

/// The list of panels to add: a package's name, then its panels, each a tall row.
pub const PICKER_HEADING: f32 = 56.0;
pub const PICKER_ROW: f32 = 136.0;
pub const PICKER_ROW_GAP: f32 = 4.0;

pub fn picker_top() -> f32 {
    grid().y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_fills_the_screen_below_the_top_bar_in_square_cells() {
        let g = grid();
        assert_eq!(g, Rect::new(40.0, 168.0, 1840.0, 880.0));
        assert_eq!(cell_width(), 209.0);
        assert_eq!(cell_height(), 202.0);
        assert_eq!(cells(0, 0, 8, 4), g);
        assert_eq!(cell_at(1879.0, 1047.0), Some((7, 3)));
        assert_eq!(cell_at(100.0, 60.0), None, "the top bar is not the grid");
        assert_eq!(nearest_cell(-50.0, 2000.0), (0, 3));
        // A gap belongs to the cell before it.
        assert_eq!(cell_at(40.0 + 209.0 + 8.0, 180.0), Some((0, 0)));
    }

    #[test]
    fn buttons_are_right_aligned_and_never_smaller_than_a_finger() {
        let rects = header_buttons(&[224.0, 320.0, 224.0]);
        assert_eq!(rects[2].right(), 1880.0);
        assert_eq!(rects[1].right() + BUTTON_GAP, rects[2].x);
        for r in rects {
            assert!(r.w >= MIN_TARGET && r.h >= MIN_TARGET);
        }
        assert!(button_width("Undo: show Wallet scanner · 8 s", 440.0) > 440.0);
        let [a, b, c] = confirm_buttons();
        assert!(a.right() < b.x && b.right() < c.x && c.right() <= WIDTH);
        for r in [a, b, c, back_button()] {
            assert!(r.w >= MIN_TARGET && r.h >= MIN_TARGET);
        }
        assert!(grip(cells(0, 0, 1, 1)).w >= MIN_TARGET);
    }

    #[test]
    fn every_console_target_is_finger_sized() {
        use console::*;
        for (name, size) in [
            ("rail item", RAIL_ITEM),
            ("button", BUTTON),
            ("row", ROW),
            ("service row", SERVICE_ROW),
            ("switch", SWITCH_W),
            ("switch", SWITCH_H),
            ("segment", SEGMENT),
            ("unlock key", KEY_UNLOCK),
            ("key", KEY_TEXT),
            ("key width", KEY_WIDTH),
            ("pin key", PIN_KEY),
            ("eye", EYE),
            ("web chip", WEB_CHIP),
            ("header button", ICON_BUTTON),
        ] {
            assert!(size >= MIN_TARGET, "a {name} is {size} px");
        }
        // The five "Turn display off after" choices share the Device page's right column, about
        // 740 px wide inside its card.
        let column = (WIDTH - 208.0 - 2.0 * 52.0 - 32.0) / 2.0 - 2.0 * 32.0;
        assert!((column - 4.0 * 12.0) / 5.0 >= MIN_TARGET, "{column}");
    }
}
