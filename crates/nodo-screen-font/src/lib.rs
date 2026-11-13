//! The screen's console font, nodo-screen-16x32 (docs/SCREEN.md, "The console font").
//!
//! Terminus's 512-glyph console font, ter-v32n, lacks most of what the screen draws: rounded and
//! dashed box corners, heavy lines, half and eighth blocks, a dot, and the icons on its buttons.
//! [`make`] draws those glyphs to match Terminus's own line positions (two pixels thick, centred)
//! and puts them in place of Greek and Cyrillic glyphs the screen never shows. Every other glyph
//! stays as it was.
//!
//! Terminus is under the SIL Open Font License 1.1, whose Reserved Font Name rule means a modified
//! font may not be called Terminus: this one is nodo-screen-16x32, shipped with the OFL text.

const W: usize = 16;
const H: usize = 32;
/// Terminus's light lines: rows 15-16 across, columns 7-8 down, so their centres are y 16, x 8.
const HY: f64 = 16.0;
const VX: f64 = 8.0;
const PSF2_MAGIC: u32 = 0x864A_B572;

type Bitmap = [[bool; W]; H];

fn fill(test: impl Fn(f64, f64) -> bool) -> Bitmap {
    let mut g = [[false; W]; H];
    for (y, row) in g.iter_mut().enumerate() {
        for (x, pixel) in row.iter_mut().enumerate() {
            *pixel = test(x as f64 + 0.5, y as f64 + 0.5);
        }
    }
    g
}

fn seg_dist(px: f64, py: f64, (ax, ay, bx, by): (f64, f64, f64, f64)) -> f64 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (px - (ax + t * dx)).hypot(py - (ay + t * dy))
}

fn lines(segments: &[(f64, f64, f64, f64)], width: f64) -> Bitmap {
    fill(|x, y| segments.iter().any(|s| seg_dist(x, y, *s) <= width / 2.0))
}

/// A light corner whose arms go to the right (or left) edge and the bottom (or top) edge.
fn rounded(right: bool, down: bool) -> Bitmap {
    let cx = if right { W as f64 } else { 0.0 };
    let cy = if down { H as f64 } else { 0.0 };
    let (rx, ry) = ((cx - VX).abs(), (cy - HY).abs());
    fill(|x, y| {
        let outside_arms = if right { x < VX - 1.0 } else { x > VX + 1.0 } || if down { y < HY - 1.0 } else { y > HY + 1.0 };
        if outside_arms || (right && x > cx) || (down && y > cy) {
            return false;
        }
        let inner = ((x - cx) / (rx - 1.0)).powi(2) + ((y - cy) / (ry - 1.0)).powi(2);
        let outer = ((x - cx) / (rx + 1.0)).powi(2) + ((y - cy) / (ry + 1.0)).powi(2);
        inner >= 1.0 && outer <= 1.0
    })
}

fn dashed(horizontal: bool, runs: &[(usize, usize)]) -> Bitmap {
    let mut g = [[false; W]; H];
    for &(a, b) in runs {
        if horizontal {
            for row in &mut g[15..17] {
                row[a..b].fill(true);
            }
        } else {
            for row in &mut g[a..b] {
                row[7..9].fill(true);
            }
        }
    }
    g
}

fn heavy(left: bool, right: bool, up: bool, down: bool) -> Bitmap {
    fill(|x, y| {
        let across = (14.0..=17.0).contains(&(y - 0.5)) && ((left && x <= 10.0) || (right && x >= 6.0));
        let along = (6.0..=9.0).contains(&(x - 0.5)) && ((up && y <= 18.0) || (down && y >= 14.0));
        across || along
    })
}

fn rows_from_bottom(n: usize) -> Bitmap {
    fill(|_, y| y >= (H - n) as f64)
}

fn power() -> Bitmap {
    let (cx, cy, r, width, gap) = (8.0, 17.5, 6.0, 2.0, 40.0);
    fill(|x, y| {
        let d = (x - cx).hypot(y - cy);
        // 0 degrees is straight up.
        let angle = (x - cx).atan2(cy - y).to_degrees();
        let ring = (d - r).abs() <= width / 2.0 && angle.abs() >= gap;
        ring || seg_dist(x, y, (8.0, 7.0, 8.0, 16.0)) <= 1.0
    })
}

fn triangle([(ax, ay), (bx, by), (cx, cy)]: [(f64, f64); 3]) -> Bitmap {
    let side = |px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64| (px - x2) * (y1 - y2) - (x1 - x2) * (py - y2);
    fill(|x, y| {
        let d = [side(x, y, ax, ay, bx, by), side(x, y, bx, by, cx, cy), side(x, y, cx, cy, ax, ay)];
        let neg = d.iter().any(|v| *v < 0.0);
        let pos = d.iter().any(|v| *v > 0.0);
        !(neg && pos)
    })
}

/// The glyphs added to Terminus, in the order they take the spare slots.
pub const ADDED: &str = "╭╮╰╯┄┆╌╎━┃┏┓┗┛▀▄▁▂▃▅▆▇●⇲⏻✓☰▸";

/// The bitmap drawn for one of [`ADDED`].
pub fn glyph(c: char) -> Option<Bitmap> {
    Some(match c {
        '╭' => rounded(true, true),
        '╮' => rounded(false, true),
        '╰' => rounded(true, false),
        '╯' => rounded(false, false),
        '┄' => dashed(true, &[(1, 4), (6, 10), (12, 15)]),
        '┆' => dashed(false, &[(2, 8), (13, 19), (24, 30)]),
        '╌' => dashed(true, &[(2, 6), (10, 14)]),
        '╎' => dashed(false, &[(4, 12), (20, 28)]),
        '━' => heavy(true, true, false, false),
        '┃' => heavy(false, false, true, true),
        '┏' => heavy(false, true, false, true),
        '┓' => heavy(true, false, false, true),
        '┗' => heavy(false, true, true, false),
        '┛' => heavy(true, false, true, false),
        '▀' => fill(|_, y| y < H as f64 / 2.0),
        '▄' => rows_from_bottom(16),
        '▁' => rows_from_bottom(4),
        '▂' => rows_from_bottom(8),
        '▃' => rows_from_bottom(12),
        '▅' => rows_from_bottom(20),
        '▆' => rows_from_bottom(24),
        '▇' => rows_from_bottom(28),
        '●' => fill(|x, y| (x - 8.0).hypot(y - 17.0) <= 5.5),
        '⇲' => lines(&[(3.0, 9.0, 11.0, 20.0), (11.0, 20.0, 11.0, 14.0), (11.0, 20.0, 5.0, 20.0), (2.0, 25.0, 15.0, 25.0), (14.0, 6.0, 14.0, 25.0)], 2.0),
        '⏻' => power(),
        '✓' => lines(&[(2.0, 17.0, 6.0, 22.0), (6.0, 22.0, 14.0, 9.0)], 2.5),
        '☰' => lines(&[(2.0, 10.0, 14.0, 10.0), (2.0, 16.0, 14.0, 16.0), (2.0, 22.0, 14.0, 22.0)], 2.0),
        '▸' => triangle([(5.0, 11.0), (5.0, 22.0), (12.0, 16.5)]),
        _ => return None,
    })
}

fn encode(bitmap: &Bitmap) -> Vec<u8> {
    bitmap
        .iter()
        .flat_map(|row| {
            let bits = row.iter().fold(0u16, |acc, p| (acc << 1) | u16::from(*p));
            bits.to_be_bytes()
        })
        .collect()
}

fn u32_at(data: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().expect("four bytes"))
}

/// Takes an uncompressed 16x32 PSF2 font with a Unicode table and returns it with [`ADDED`]
/// drawn in, and the characters added.
pub fn make(data: &[u8]) -> Result<(Vec<u8>, String), String> {
    if data.len() < 32 || u32_at(data, 0) != PSF2_MAGIC {
        return Err("not a PSF2 font".into());
    }
    let (header, flags, count, size, height, width) =
        (u32_at(data, 2) as usize, u32_at(data, 3), u32_at(data, 4) as usize, u32_at(data, 5) as usize, u32_at(data, 6) as usize, u32_at(data, 7) as usize);
    if (width, height, size) != (W, H, 64) || flags & 1 == 0 {
        return Err("expected a 16x32 PSF2 font with a Unicode table".into());
    }
    let table_at = header + count * size;
    if data.len() < table_at {
        return Err("the font is cut short".into());
    }
    let mut glyphs: Vec<Vec<u8>> = (0..count).map(|i| data[header + i * size..header + (i + 1) * size].to_vec()).collect();
    let mut entries: Vec<Vec<u8>> = data[table_at..].split(|b| *b == 0xFF).take(count).map(<[u8]>::to_vec).collect();
    let chars = |entry: &[u8]| -> Vec<char> { entry.split(|b| *b == 0xFE).flat_map(|seq| String::from_utf8_lossy(seq).chars().filter(|c| *c != '\u{FFFD}').collect::<Vec<_>>()).collect() };
    let present: std::collections::HashSet<char> = entries.iter().flat_map(|e| chars(e)).collect();
    let spare: Vec<usize> = (0..entries.len())
        .filter(|&i| {
            let cs = chars(&entries[i]);
            !cs.is_empty() && cs.iter().all(|c| (0x370..=0x4FF).contains(&u32::from(*c)))
        })
        .collect();
    let wanted: Vec<char> = ADDED.chars().filter(|c| !present.contains(c)).collect();
    if wanted.len() > spare.len() {
        return Err(format!("{} glyphs to add but only {} spare slots", wanted.len(), spare.len()));
    }
    for (c, &index) in wanted.iter().zip(&spare) {
        glyphs[index] = encode(&glyph(*c).expect("every added glyph is drawn"));
        entries[index] = c.to_string().into_bytes();
    }
    let mut out = data[..header].to_vec();
    for g in &glyphs {
        out.extend_from_slice(g);
    }
    for e in &entries {
        out.extend_from_slice(e);
        out.push(0xFF);
    }
    Ok((out, wanted.into_iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16x32 PSF2 font: `count` blank glyphs, the first mapped to 'A' and the rest to Greek and
    /// Cyrillic letters, with one already holding ─.
    fn font(count: usize) -> Vec<u8> {
        let mut data = Vec::new();
        for v in [PSF2_MAGIC, 0, 32, 1, count as u32, 64, 32, 16] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        data.extend(std::iter::repeat_n(0u8, count * 64));
        for i in 0..count {
            let c = match i {
                0 => 'A',
                1 => '─',
                _ => char::from_u32(0x391 + i as u32).unwrap(),
            };
            data.extend_from_slice(c.to_string().as_bytes());
            data.push(0xFF);
        }
        data
    }

    #[test]
    fn every_added_glyph_is_drawn_and_lands_in_a_spare_slot() {
        for c in ADDED.chars() {
            let g = glyph(c).unwrap();
            assert!(g.iter().flatten().any(|p| *p), "{c} is blank");
        }
        let (out, added) = make(&font(64)).unwrap();
        assert_eq!(added, ADDED);
        assert_eq!(out[..32 + 64 * 64].len(), font(64)[..32 + 64 * 64].len(), "same header and glyph count");
        let table = &out[32 + 64 * 64..];
        let entries: Vec<&[u8]> = table.split(|b| *b == 0xFF).collect();
        assert_eq!(entries[0], b"A", "other glyphs are kept");
        assert_eq!(entries[1], "─".as_bytes());
        assert_eq!(entries[2], "╭".as_bytes(), "the first spare slot takes the first added glyph");
        assert_eq!(&out[32 + 2 * 64..32 + 3 * 64], encode(&rounded(true, true)).as_slice());
    }

    #[test]
    fn corners_join_terminus_lines() {
        // ╭'s arm reaches the right edge on rows 15-16, where ─ is, and the bottom edge on columns
        // 7-8, where │ is.
        let g = rounded(true, true);
        assert!(g[15][15] && g[16][15]);
        assert!(g[31][7] && g[31][8]);
        assert!(!g[0].iter().any(|p| *p), "nothing above the arm");
    }

    #[test]
    fn bad_fonts_are_refused() {
        assert!(make(b"not a font").is_err());
        assert!(make(&font(2)).is_err(), "too few spare slots");
        let mut no_table = font(64);
        no_table[12] = 0;
        assert!(make(&no_table).is_err());
    }
}
