//! The boot splash: the Nodo logo (the Ember band with its hex core) in Unicode half blocks on the
//! Linux text console, with a glint that crosses it now and then (docs/SCREEN.md, "Boot splash").
//!
//! The logo is a 56 x 56 grid of palette indices, made from the SVG by `tools/splash` and embedded
//! here. Each console cell shows two of its pixels, one above the other, so the logo takes 28 rows
//! of the 120 x 33 console and its pixels are square (16 x 16 on the T6N's screen).
//!
//! # Colours on the Linux console
//!
//! The console has 16 palette entries, which a program may redefine (`ESC ] P n rrggbb`), but only
//! 8 of them can be a cell's background: the 8 normal colours. Foregrounds may be any of the 16:
//! bold turns the foreground index `n` into `n + 8`. The logo's 8 colours (black and seven oranges)
//! are therefore entries 0 to 7, usable both ways. The glint's pale tone, the 9th colour, is entry
//! 8, which only a foreground can show (bold black). A cell shows it in whichever half needs it:
//! `▀` when the top pixel is pale (the foreground draws the top half), `▄` when only the bottom one
//! is, and `█` when both are. A cell never needs two pale halves on a background.

use std::fmt::Write as _;

/// Logo pixels per side.
pub const SIZE: usize = 56;
/// Console rows the logo takes.
pub const ROWS: usize = SIZE / 2;

/// Palette entries 0 to 8 as RGB: black, the seven band oranges from deepest to palest, and the
/// glint's pale tone, which only the glint uses.
pub const PALETTE: [u32; 9] = [
    0x000000, 0x7A3300, 0xA34400, 0xC24E00, 0xE85D00, 0xFF6600, 0xFF8A33, 0xFFB066, 0xFFE2C4,
];
/// The name's colour: band orange, entry 5.
pub const NAME_COLOUR: u8 = 5;
/// The status line's dim grey, shown as entry 15 (bold white), which the logo does not use.
pub const DIM: u32 = 0x8A8F94;

/// Index of the dark outline. Outlines and black never change, so the knot stays readable.
const OUTLINE: u8 = 1;
/// The glint's palest tone.
const PALE: u8 = 8;

pub type Grid = [[u8; SIZE]; SIZE];

const LOGO_TEXT: &str = include_str!("../logo.grid");

/// The embedded logo. `tools/splash` writes `logo.grid`; the tests check its shape.
pub fn logo() -> Grid {
    parse(LOGO_TEXT).expect("logo.grid is checked by the tests")
}

/// Reads 56 lines of 56 digits 0 to 7.
pub fn parse(text: &str) -> Result<Grid, String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    if lines.len() != SIZE {
        return Err(format!("{} lines, not {SIZE}", lines.len()));
    }
    let mut grid = [[0u8; SIZE]; SIZE];
    for (y, line) in lines.iter().enumerate() {
        let bytes = line.as_bytes();
        if bytes.len() != SIZE {
            return Err(format!("line {} has {} pixels, not {SIZE}", y + 1, bytes.len()));
        }
        for (x, b) in bytes.iter().enumerate() {
            match b {
                b'0'..=b'7' => grid[y][x] = b - b'0',
                _ => return Err(format!("line {} has {:?}, not a digit 0 to 7", y + 1, *b as char)),
            }
        }
    }
    Ok(grid)
}

/// The logo with the glint's centre on the diagonal `x + y = t`. Within the band each orange steps
/// up three tones at the centre (distance up to 1), two near it (up to 3) and one at its edges (up
/// to 7), up to the pale tone. `None` is the logo at rest.
pub fn frame(logo: &Grid, t: Option<i32>) -> Grid {
    let Some(t) = t else { return *logo };
    let mut out = *logo;
    for (y, row) in out.iter_mut().enumerate() {
        for (x, v) in row.iter_mut().enumerate() {
            if *v <= OUTLINE {
                continue;
            }
            let d = (x as i32 + y as i32 - t).abs();
            let step = match d {
                0..=1 => 3,
                2..=3 => 2,
                4..=7 => 1,
                _ => 0,
            };
            *v = (*v + step).min(PALE);
        }
    }
    out
}

/// Glint positions for one sweep: from before the top-left corner (-20) to past the bottom-right
/// one (132; `x + y` runs from 0 to 110), 13 frames, about 12 a second.
pub fn sweep() -> impl Iterator<Item = i32> {
    const FRAMES: i32 = 13;
    (0..FRAMES).map(|i| -20 + 152 * i / (FRAMES - 1))
}

/// One console cell: two stacked pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub top: u8,
    pub bottom: u8,
}

pub type Cells = [[Cell; SIZE]; ROWS];

pub fn cells(grid: &Grid) -> Cells {
    let mut out = [[Cell { top: 0, bottom: 0 }; SIZE]; ROWS];
    for (r, row) in out.iter_mut().enumerate() {
        for (x, cell) in row.iter_mut().enumerate() {
            *cell = Cell { top: grid[2 * r][x], bottom: grid[2 * r + 1][x] };
        }
    }
    out
}

/// The attributes and character that draw a cell on the Linux console (see the module notes).
pub fn console_cell(cell: Cell) -> (String, char) {
    let fg = |n: u8| if n >= 8 { format!("1;3{}", n - 8) } else { format!("22;3{n}") };
    match (cell.top, cell.bottom) {
        (PALE, PALE) => (format!("\x1b[{};40m", fg(PALE)), '█'),
        (top, PALE) => (format!("\x1b[{};4{top}m", fg(PALE)), '▄'),
        (top, bottom) => (format!("\x1b[{};4{bottom}m", fg(top)), '▀'),
    }
}

/// Where things go on a console of `cols` x `rows` (0-based row and column numbers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub cols: usize,
    pub rows: usize,
    pub logo_top: usize,
    pub logo_left: usize,
    pub name_row: usize,
    pub status_row: usize,
}

impl Layout {
    /// On the T6N's 120 x 33 console: the logo on rows 1 to 28, the name on row 30 and the status
    /// on row 31. A bigger console centres the same block; a smaller one keeps it at the top.
    pub fn new(cols: usize, rows: usize) -> Layout {
        const BLOCK: usize = 33;
        let top = rows.saturating_sub(BLOCK) / 2;
        Layout {
            cols,
            rows,
            logo_top: top + 1,
            logo_left: cols.saturating_sub(SIZE) / 2,
            name_row: top + 1 + ROWS + 1,
            status_row: top + 1 + ROWS + 2,
        }
    }
}

/// Sets palette entries 0 to 8 and 15 to the splash's colours.
pub fn palette_sequence() -> String {
    let mut s = String::new();
    for (i, rgb) in PALETTE.iter().enumerate() {
        let _ = write!(s, "\x1b]P{i:x}{rgb:06X}");
    }
    let _ = write!(s, "\x1b]PF{DIM:06X}");
    s
}

fn centred(s: &mut String, row: usize, cols: usize, text: &str, attrs: &str) {
    let width = text.chars().count();
    let col = cols.saturating_sub(width) / 2;
    let _ = write!(s, "\x1b[{};{}H{attrs}{text}", row + 1, col + 1);
}

/// The whole screen: palette, cleared console without a cursor, logo, name and status line.
pub fn full_draw(layout: &Layout, cells: &Cells, status: &str) -> String {
    let mut s = palette_sequence();
    s.push_str("\x1b[?25l\x1b[0m\x1b[40m\x1b[2J");
    for (r, row) in cells.iter().enumerate() {
        let _ = write!(s, "\x1b[{};{}H", layout.logo_top + r + 1, layout.logo_left + 1);
        let mut last = String::new();
        for cell in row {
            let (attrs, ch) = console_cell(*cell);
            if attrs != last {
                s.push_str(&attrs);
                last = attrs;
            }
            s.push(ch);
        }
    }
    centred(&mut s, layout.name_row, layout.cols, "n o d o", &format!("\x1b[22;3{NAME_COLOUR};40m"));
    centred(&mut s, layout.status_row, layout.cols, status, "\x1b[1;37;40m");
    s.push_str("\x1b[0m\x1b[40m");
    s
}

/// Only the cells that changed between two frames, each run of neighbours with one cursor move.
pub fn diff_draw(layout: &Layout, before: &Cells, after: &Cells) -> String {
    let mut s = String::new();
    for r in 0..ROWS {
        let mut at: Option<usize> = None;
        let mut last = String::new();
        for x in 0..SIZE {
            if before[r][x] == after[r][x] {
                continue;
            }
            if at != Some(x) {
                let _ = write!(s, "\x1b[{};{}H", layout.logo_top + r + 1, layout.logo_left + x + 1);
                last.clear();
            }
            let (attrs, ch) = console_cell(after[r][x]);
            if attrs != last {
                s.push_str(&attrs);
                last = attrs;
            }
            s.push(ch);
            at = Some(x + 1);
        }
    }
    if !s.is_empty() {
        s.push_str("\x1b[0m\x1b[40m");
    }
    s
}

/// A frame for a desktop terminal, in 24-bit colour: for checking the art without the device.
pub fn preview(grid: &Grid) -> String {
    let rgb = |n: u8| {
        let c = PALETTE[n as usize];
        (c >> 16, (c >> 8) & 0xFF, c & 0xFF)
    };
    let mut s = String::new();
    for row in cells(grid) {
        for cell in row {
            let (a, b) = (rgb(cell.top), rgb(cell.bottom));
            let _ = write!(s, "\x1b[38;2;{};{};{};48;2;{};{};{}m▀", a.0, a.1, a.2, b.0, b.1, b.2);
        }
        s.push_str("\x1b[0m\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_logo_is_56_by_56_of_the_eight_logo_colours() {
        let logo = logo();
        assert!(logo.iter().flatten().all(|&v| v <= 7));
        // The corners are black, the band has outlines and every orange.
        assert_eq!(logo[0][0], 0);
        for v in 1..=7 {
            assert!(logo.iter().flatten().any(|&p| p == v), "colour {v} is missing");
        }
    }

    #[test]
    fn parse_refuses_wrong_shapes_and_characters() {
        assert!(parse("0").is_err());
        let mut good = String::new();
        for _ in 0..SIZE {
            good.push_str(&"0".repeat(SIZE));
            good.push('\n');
        }
        assert!(parse(&good).is_ok());
        assert!(parse(&good.replacen('0', "8", 1)).unwrap_err().contains("not a digit"));
        assert!(parse(&good.replacen('0', "", 1)).unwrap_err().contains("55 pixels"));
    }

    #[test]
    fn the_glint_brightens_only_the_band_and_never_past_the_pale_tone() {
        let logo = logo();
        let lit = frame(&logo, Some(56));
        for y in 0..SIZE {
            for x in 0..SIZE {
                let (a, b) = (logo[y][x], lit[y][x]);
                if a <= OUTLINE {
                    assert_eq!(a, b, "outline or black changed at {x},{y}");
                } else {
                    assert!(b >= a && b <= PALE);
                    let d = (x as i32 + y as i32 - 56).abs();
                    let want = (a + if d <= 1 { 3 } else if d <= 3 { 2 } else if d <= 7 { 1 } else { 0 }).min(PALE);
                    assert_eq!(b, want, "at {x},{y}");
                }
            }
        }
        assert!(lit.iter().flatten().any(|&p| p == PALE));
    }

    #[test]
    fn the_sweep_starts_and_ends_off_the_logo_so_resting_redraws_nothing() {
        let logo = logo();
        let ts: Vec<i32> = sweep().collect();
        assert_eq!(ts.len(), 13);
        assert_eq!((ts[0], ts[12]), (-20, 132));
        assert_eq!(frame(&logo, Some(ts[0])), logo);
        assert_eq!(frame(&logo, Some(ts[12])), logo);
        let layout = Layout::new(120, 33);
        let rest = cells(&logo);
        assert!(diff_draw(&layout, &rest, &rest).is_empty());
    }

    #[test]
    fn a_diff_redraws_only_changed_cells() {
        let logo = logo();
        let layout = Layout::new(120, 33);
        let (a, b) = (cells(&logo), cells(&frame(&logo, Some(56))));
        let changed = a.iter().flatten().zip(b.iter().flatten()).filter(|(x, y)| x != y).count();
        let out = diff_draw(&layout, &a, &b);
        let glyphs = out.chars().filter(|c| matches!(c, '▀' | '▄' | '█')).count();
        assert_eq!(glyphs, changed);
        assert!(changed > 0 && changed < ROWS * SIZE / 2);
    }

    #[test]
    fn the_pale_tone_is_only_ever_a_foreground() {
        let (a, ch) = console_cell(Cell { top: 8, bottom: 3 });
        assert_eq!((a.as_str(), ch), ("\x1b[1;30;43m", '▀'));
        let (a, ch) = console_cell(Cell { top: 4, bottom: 8 });
        assert_eq!((a.as_str(), ch), ("\x1b[1;30;44m", '▄'));
        let (a, ch) = console_cell(Cell { top: 8, bottom: 8 });
        assert_eq!((a.as_str(), ch), ("\x1b[1;30;40m", '█'));
        let (a, ch) = console_cell(Cell { top: 6, bottom: 1 });
        assert_eq!((a.as_str(), ch), ("\x1b[22;36;41m", '▀'));
        // No cell ever asks for a background above 7.
        let logo = logo();
        for t in sweep() {
            for cell in cells(&frame(&logo, Some(t))).iter().flatten() {
                let (attrs, _) = console_cell(*cell);
                let bg = attrs.trim_end_matches('m').rsplit(';').next().unwrap().to_string();
                assert!(bg.starts_with('4') && bg[1..].parse::<u8>().unwrap() <= 7, "{attrs}");
            }
        }
    }

    #[test]
    fn on_the_t6n_console_the_logo_sits_on_rows_1_to_28_with_the_name_and_status_below() {
        let l = Layout::new(120, 33);
        assert_eq!((l.logo_top, l.logo_left, l.name_row, l.status_row), (1, 32, 30, 31));
        let s = full_draw(&l, &cells(&logo()), "starting up…");
        assert!(s.starts_with("\x1b]P0000000\x1b]P17A3300"));
        assert!(s.contains("\x1b]P8FFE2C4") && s.contains("\x1b]PF8A8F94"));
        assert!(s.contains("\x1b[?25l"));
        assert!(s.contains("\x1b[2;33H"));
        assert!(s.contains("\x1b[31;57H\x1b[22;35;40mn o d o"));
        assert!(s.contains("\x1b[32;55H\x1b[1;37;40mstarting up…"));
    }
}
