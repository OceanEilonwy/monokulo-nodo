//! Version ordering that matches `xbps_cmpver`.
//!
//! The downgrade check is only sound if it agrees with what xbps will consider newer, so this
//! follows xbps' `lib/external/dewey.c` rather than a general version scheme. The one deliberate
//! difference is that xbps lets a large number overflow an `int`, which could make a lower version
//! appear higher. Here such a version does not compare at all, and callers treat that as blocked.

use std::cmp::Ordering;

const ALPHA: i64 = -3;
const BETA: i64 = -2;
const RC: i64 = -1;
const DOT: i64 = 0;
/// Longer strings are not plausible versions and would only cost memory.
const MAX_VERSION_LEN: usize = 128;

const MODIFIERS: [(&str, i64); 6] =
    [("alpha", ALPHA), ("beta", BETA), ("pre", RC), ("rc", RC), ("pl", DOT), (".", DOT)];

struct Parsed {
    components: Vec<i64>,
    revision: i64,
}

fn read_number(text: &[u8]) -> Option<(i64, usize)> {
    let digits = text.iter().take_while(|b| b.is_ascii_digit()).count();
    let mut value: i32 = 0;
    for byte in &text[..digits] {
        value = value.checked_mul(10)?.checked_add(i32::from(byte - b'0'))?;
    }
    Some((i64::from(value), digits))
}

fn parse(version: &str) -> Option<Parsed> {
    if version.len() > MAX_VERSION_LEN {
        return None;
    }
    let mut parsed = Parsed { components: Vec::new(), revision: 0 };
    let mut rest = version.as_bytes();
    while let Some(first) = rest.first() {
        if first.is_ascii_digit() {
            let (value, used) = read_number(rest)?;
            parsed.components.push(value);
            rest = &rest[used..];
            continue;
        }
        let modifier = MODIFIERS
            .iter()
            .find(|(name, _)| rest.len() >= name.len() && rest[..name.len()].eq_ignore_ascii_case(name.as_bytes()));
        if let Some((name, value)) = modifier {
            parsed.components.push(*value);
            rest = &rest[name.len()..];
        } else if *first == b'_' {
            let (value, used) = read_number(&rest[1..])?;
            parsed.revision = value;
            rest = &rest[1 + used..];
        } else if first.is_ascii_alphabetic() {
            parsed.components.push(DOT);
            parsed.components.push(i64::from(first.to_ascii_lowercase() - b'a') + 1);
            rest = &rest[1..];
        } else if matches!(first, b'+' | b'~') {
            // Punctuation that xbps skips without comparing.
            rest = &rest[1..];
        } else {
            return None;
        }
    }
    Some(parsed)
}

/// Compares two `version_revision` strings. `None` means at least one cannot be compared reliably.
pub fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    let left = parse(left)?;
    let right = parse(right)?;
    let longest = left.components.len().max(right.components.len());
    for index in 0..longest {
        let a = left.components.get(index).copied().unwrap_or(0);
        let b = right.components.get(index).copied().unwrap_or(0);
        match a.cmp(&b) {
            Ordering::Equal => {}
            other => return Some(other),
        }
    }
    Some(left.revision.cmp(&right.revision))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_like_xbps() {
        for (lower, higher) in [
            ("1.0_1", "1.0_2"),
            ("1.0_9", "1.0_10"),
            ("1.0_2", "1.1_1"),
            ("1.9_1", "1.10_1"),
            ("1.0alpha_1", "1.0beta_1"),
            ("1.0beta_1", "1.0rc_1"),
            ("1.0pre_1", "1.0_1"),
            ("1.0rc1_1", "1.0_1"),
            ("1.0rc1_1", "1.0rc2_1"),
            ("1.0_1", "1.0a_1"),
            ("1.0a_1", "1.0b_1"),
            ("1.0_1", "1.0.1_1"),
            ("1_1", "2_1"),
            ("0.9.9_1", "1_1"),
        ] {
            assert_eq!(compare_versions(lower, higher), Some(Ordering::Less), "{lower} < {higher}");
            assert_eq!(compare_versions(higher, lower), Some(Ordering::Greater), "{higher} > {lower}");
        }
    }

    #[test]
    fn treats_equivalent_versions_as_equal() {
        for (a, b) in [("1.0_1", "1.0_1"), ("1.0", "1.0_0"), ("1.0_1", "1.0.0_1"), ("1.0pl_1", "1.0_1"), ("1.0RC1_1", "1.0rc1_1")] {
            assert_eq!(compare_versions(a, b), Some(Ordering::Equal), "{a} == {b}");
        }
    }

    #[test]
    fn refuses_versions_that_would_overflow_in_xbps() {
        assert_eq!(compare_versions("4294967296_1", "1_1"), None, "wraps to 0 in a C int");
        assert_eq!(compare_versions("1_99999999999", "1_1"), None);
        assert_eq!(compare_versions("2147483647_1", "1_1"), Some(Ordering::Greater), "the largest int is fine");
    }

    /// Every pair here was compared by the real `xbps-uhelper cmpver`, so a difference in the
    /// downgrade check's ordering shows up as a failure rather than a silent disagreement.
    #[test]
    fn agrees_with_real_xbps_uhelper_cmpver() {
        let recorded = include_str!("../tests/fixtures/cmpver.txt");
        let mut checked = 0;
        for line in recorded.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [left, op, right] = fields[..] else { panic!("bad line {line:?}") };
            let expected = match op {
                "<" => Ordering::Less,
                "=" => Ordering::Equal,
                ">" => Ordering::Greater,
                _ => panic!("bad operator in {line:?}"),
            };
            assert_eq!(compare_versions(left, right), Some(expected), "{line}");
            checked += 1;
        }
        assert!(checked >= 40, "only {checked} pairs");
    }

    #[test]
    fn refuses_garbage() {
        for bad in ["1.0-1", "1 0", "1;0", "é", "1\n"] {
            assert_eq!(compare_versions(bad, "1_1"), None, "{bad:?}");
        }
        assert_eq!(compare_versions(&"1".repeat(MAX_VERSION_LEN + 1), "1_1"), None);
    }
}
