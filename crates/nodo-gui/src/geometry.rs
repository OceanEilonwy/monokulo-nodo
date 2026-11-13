//! Where everything sits on the 1920 x 1080 screen, in pixels.
//!
//! The design (`docs/design/tui/ScreenTilesOled*.dc.html`, `ScreenShutdown*.dc.html`) is drawn at
//! half size, 960 x 540, so every measure here is the design's doubled. Rust decides every
//! rectangle a finger can hit and hands them to the .slint files, so that what is drawn and what
//! is hit can never disagree.

use nodo_panels::layout::{COLUMNS, ROWS};

pub const WIDTH: f32 = 1920.0;
pub const HEIGHT: f32 = 1080.0;
pub const PAD_TOP: f32 = 24.0;
pub const PAD_X: f32 = 36.0;
pub const PAD_BOTTOM: f32 = 32.0;
pub const HEADER_HEIGHT: f32 = 88.0;
pub const HEADER_GAP: f32 = 20.0;
pub const BUTTON_GAP: f32 = 24.0;
pub const GRID_GAP: f32 = 16.0;
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

/// The confirmation's two buttons, 300 x 110 in the design, side by side below its text.
pub fn confirm_buttons() -> (Rect, Rect) {
    let (w, h, gap, y) = (600.0, 220.0, 56.0, 600.0);
    let centre = WIDTH / 2.0;
    (Rect::new(centre - gap / 2.0 - w, y, w, h), Rect::new(centre + gap / 2.0, y, w, h))
}

/// "Back to the panels", after a shutdown that could not start.
pub fn back_button() -> Rect {
    let (w, h) = (600.0, 140.0);
    Rect::new((WIDTH - w) / 2.0, HEIGHT - PAD_BOTTOM - h - 40.0, w, h)
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
        assert_eq!(g, Rect::new(36.0, 132.0, 1848.0, 916.0));
        assert_eq!(cell_width(), 217.0);
        assert_eq!(cell_height(), 217.0);
        assert_eq!(cells(0, 0, 8, 4), g);
        assert_eq!(cell_at(1883.0, 1047.0), Some((7, 3)));
        assert_eq!(cell_at(100.0, 60.0), None, "the top bar is not the grid");
        assert_eq!(nearest_cell(-50.0, 2000.0), (0, 3));
        // A gap belongs to the cell before it.
        assert_eq!(cell_at(36.0 + 217.0 + 8.0, 140.0), Some((0, 0)));
    }

    #[test]
    fn buttons_are_right_aligned_and_never_smaller_than_a_finger() {
        let rects = header_buttons(&[224.0, 320.0, 224.0]);
        assert_eq!(rects[2].right(), 1884.0);
        assert_eq!(rects[1].right() + BUTTON_GAP, rects[2].x);
        for r in rects {
            assert!(r.w >= MIN_TARGET && r.h >= MIN_TARGET);
        }
        assert!(button_width("Undo: show Wallet scanner · 8 s", 440.0) > 440.0);
        let (a, b) = confirm_buttons();
        assert!(a.right() < b.x && b.right() <= WIDTH);
        assert!(grip(cells(0, 0, 1, 1)).w >= MIN_TARGET);
    }
}
