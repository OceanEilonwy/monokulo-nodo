//! splash-grid: renders the Nodo logo's SVG to the 56 x 56 palette grid that nodo-splash draws
//! with half blocks on the text console. See README.md.
//!
//! At 56 pixels across, the knot's dark outlines would be thinner than a pixel and vanish, so they
//! are drawn thicker (stroke width 1.21 becomes 2.0 of the 100-unit view box) before shrinking.
//! The logo is rendered at 8 x 8 times the grid over black, every rendered pixel is matched to the
//! nearest of the eight colours, and each 8 x 8 block becomes one grid pixel: outline when at least
//! 30% of it is outline, black when at least half is black, otherwise the most common orange.

use std::process::ExitCode;

const SIZE: u32 = 56;
const SCALE: u32 = 8;
/// Black, the outline and the six band oranges: nodo-splash's palette entries 0 to 7.
const PALETTE: [[u8; 3]; 8] = [
    [0x00, 0x00, 0x00],
    [0x7A, 0x33, 0x00],
    [0xA3, 0x44, 0x00],
    [0xC2, 0x4E, 0x00],
    [0xE8, 0x5D, 0x00],
    [0xFF, 0x66, 0x00],
    [0xFF, 0x8A, 0x33],
    [0xFF, 0xB0, 0x66],
];
const OUTLINE_WIDTH: (&str, &str) = ("stroke-width=\"1.21\"", "stroke-width=\"2.0\"");

const USAGE: &str = "usage: splash-grid LOGO.svg OUT.grid [--check]";

fn nearest(rgb: [u8; 3]) -> u8 {
    let dist = |p: &[u8; 3]| -> u32 {
        (0..3)
            .map(|i| (i32::from(rgb[i]) - i32::from(p[i])).pow(2) as u32)
            .sum()
    };
    // The first of equally near colours wins.
    let mut best = 0;
    for (i, p) in PALETTE.iter().enumerate() {
        if dist(p) < dist(&PALETTE[best]) {
            best = i;
        }
    }
    best as u8
}

fn render(svg: &str) -> Result<resvg::tiny_skia::Pixmap, String> {
    let svg = svg.replace(OUTLINE_WIDTH.0, OUTLINE_WIDTH.1);
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default())
        .map_err(|e| e.to_string())?;
    let side = SIZE * SCALE;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(side, side).ok_or("no pixmap")?;
    pixmap.fill(resvg::tiny_skia::Color::BLACK);
    let size = tree.size();
    let transform = resvg::tiny_skia::Transform::from_scale(
        side as f32 / size.width(),
        side as f32 / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(pixmap)
}

/// One grid pixel from the colours of its block, in the order they were met.
fn block(colours: &[u8]) -> u8 {
    let total = colours.len() as f64;
    let count = |c: u8| colours.iter().filter(|&&v| v == c).count();
    if count(1) as f64 >= 0.3 * total {
        return 1;
    }
    if count(0) as f64 >= 0.5 * total {
        return 0;
    }
    // The most common orange; among equals, the one met first.
    let mut best: Option<(u8, usize)> = None;
    let mut seen = Vec::new();
    for &c in colours {
        if c <= 1 || seen.contains(&c) {
            continue;
        }
        seen.push(c);
        let n = count(c);
        if best.is_none_or(|(_, m)| n > m) {
            best = Some((c, n));
        }
    }
    best.map_or(1, |(c, _)| c)
}

fn grid(pixmap: &resvg::tiny_skia::Pixmap) -> String {
    let side = pixmap.width();
    let data = pixmap.data();
    let at = |x: u32, y: u32| {
        let i = ((y * side + x) * 4) as usize;
        nearest([data[i], data[i + 1], data[i + 2]])
    };
    let mut out = String::new();
    for gy in 0..SIZE {
        for gx in 0..SIZE {
            let mut colours = Vec::with_capacity((SCALE * SCALE) as usize);
            for dx in 0..SCALE {
                for dy in 0..SCALE {
                    colours.push(at(gx * SCALE + dx, gy * SCALE + dy));
                }
            }
            out.push(char::from(b'0' + block(&colours)));
        }
        out.push('\n');
    }
    out
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (svg_path, out_path, check) = match args.as_slice() {
        [svg, out] => (svg, out, false),
        [svg, out, flag] if flag == "--check" => (svg, out, true),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let made = match std::fs::read_to_string(svg_path)
        .map_err(|e| e.to_string())
        .and_then(|s| render(&s))
    {
        Ok(pixmap) => grid(&pixmap),
        Err(e) => {
            eprintln!("splash-grid: {svg_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if check {
        let old = std::fs::read_to_string(out_path).unwrap_or_default();
        let differ = old
            .lines()
            .zip(made.lines())
            .flat_map(|(a, b)| a.bytes().zip(b.bytes()))
            .filter(|(a, b)| a != b)
            .count();
        if old == made {
            println!("{out_path} matches {svg_path}");
            return ExitCode::SUCCESS;
        }
        println!("{out_path} differs from {svg_path} in {differ} pixels");
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(out_path, made) {
        eprintln!("splash-grid: {out_path}: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_prefer_outline_then_black_then_the_commonest_orange() {
        let mut b = vec![0u8; 64];
        b[..20].fill(1);
        assert_eq!(block(&b), 1);
        let mut b = vec![0u8; 64];
        b[..19].fill(1);
        assert_eq!(block(&b), 0);
        let mut b = vec![5u8; 64];
        b[..10].fill(6);
        assert_eq!(block(&b), 5);
        // Equal counts: the first met wins.
        let mut b = vec![0u8; 64];
        b[..16].fill(4);
        b[16..32].fill(6);
        b[32..48].fill(1);
        assert_eq!(block(&b), 4);
    }

    #[test]
    fn colours_match_the_nearest_palette_entry() {
        assert_eq!(nearest([0, 0, 0]), 0);
        assert_eq!(nearest([0xFF, 0x66, 0x00]), 5);
        assert_eq!(nearest([0xF0, 0x60, 0x00]), 4);
        assert_eq!(nearest([0x80, 0x30, 0x00]), 1);
    }
}
