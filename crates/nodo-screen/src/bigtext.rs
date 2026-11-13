//! Large numbers drawn with half-block characters: each glyph is 3 cells wide and 3 rows tall,
//! from a 3 x 5 bitmap. That is how a terminal panel shows a value as large as the designs do.

/// Rows of a glyph, top to bottom, three bits each (left is the high bit).
fn glyph(c: char) -> Option<[u8; 5]> {
    Some(match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '%' => [0b101, 0b001, 0b010, 0b100, 0b101],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        ',' => [0b000, 0b000, 0b000, 0b010, 0b100],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        ' ' => [0; 5],
        _ => return None,
    })
}

pub const HEIGHT: u16 = 3;

/// Splits `text` into the leading part that can be drawn large and the rest, which is drawn small
/// beside it: `"52 °C"` becomes `("52 ", "°C")`.
pub fn split(text: &str) -> (&str, &str) {
    let end = text.char_indices().find(|(_, c)| glyph(*c).is_none()).map_or(text.len(), |(i, _)| i);
    (&text[..end], &text[end..])
}

/// Columns the large text takes.
pub fn width(text: &str) -> u16 {
    let n = text.chars().count() as u16;
    if n == 0 {
        0
    } else {
        n * 4 - 1
    }
}

/// The three lines of `text` drawn large. Characters without a glyph must already be split off.
pub fn render(text: &str) -> [String; 3] {
    let mut lines = [String::new(), String::new(), String::new()];
    for (index, c) in text.chars().enumerate() {
        let rows = glyph(c).unwrap_or([0; 5]);
        // Six half rows: the bitmap's five and a blank one.
        let half = |r: usize| if r < 5 { rows[r] } else { 0 };
        for (line, out) in lines.iter_mut().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            let (top, bottom) = (half(line * 2), half(line * 2 + 1));
            for bit in (0..3).rev() {
                let t = top >> bit & 1 == 1;
                let b = bottom >> bit & 1 == 1;
                out.push(match (t, b) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                });
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_digits_large() {
        let [a, b, c] = render("61%");
        assert_eq!(a, "█▀▀ ▄█  ▀ █");
        assert_eq!(b, "█▀█  █  ▄▀ ");
        assert_eq!(c, "▀▀▀ ▀▀▀ ▀ ▀");
        assert_eq!(width("61%"), 11);
        assert_eq!(a.chars().count(), 11);
    }

    #[test]
    fn splits_off_what_has_no_glyph() {
        assert_eq!(split("52 °C"), ("52 ", "°C"));
        assert_eq!(split("4.82"), ("4.82", ""));
        assert_eq!(split("€312"), ("", "€312"));
    }
}
