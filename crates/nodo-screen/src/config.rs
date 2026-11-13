//! `/etc/nodo/screen.toml`: how the touchscreen's axes map onto the console, and when the
//! backlight dims.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Touch {
    #[serde(default = "yes")]
    pub swap_xy: bool,
    #[serde(default = "yes")]
    pub invert_x: bool,
    #[serde(default)]
    pub invert_y: bool,
}

fn yes() -> bool {
    true
}

/// The Nodo's panel sits on its side, left edge up, so its native x runs down the console and its
/// native y runs right to left.
impl Default for Touch {
    fn default() -> Self {
        Touch { swap_xy: true, invert_x: true, invert_y: false }
    }
}

/// `[backlight]`: dim to `dim_percent` after `dim_after_secs` without input; 0 seconds never dims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Backlight {
    #[serde(default = "two_minutes")]
    pub dim_after_secs: u64,
    #[serde(default = "twenty")]
    pub dim_percent: u8,
}

fn two_minutes() -> u64 {
    120
}

fn twenty() -> u8 {
    20
}

impl Default for Backlight {
    fn default() -> Self {
        Backlight { dim_after_secs: two_minutes(), dim_percent: twenty() }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub touch: Touch,
    #[serde(default)]
    pub backlight: Backlight,
}

impl Config {
    /// A missing file means the defaults; a broken one is reported and the defaults used.
    pub fn load(path: &std::path::Path) -> (Config, Option<String>) {
        match std::fs::read_to_string(path) {
            Err(_) => (Config::default(), None),
            Ok(text) => match toml::from_str(&text) {
                Ok(config) => (config, None),
                Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
            },
        }
    }
}

impl Touch {
    /// Maps a raw position, each axis already scaled to 0..=1, to the console's 0..=1.
    pub fn map(&self, x: f64, y: f64) -> (f64, f64) {
        let (mut u, mut v) = if self.swap_xy { (y, x) } else { (x, y) };
        if self.invert_x {
            u = 1.0 - u;
        }
        if self.invert_y {
            v = 1.0 - v;
        }
        (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_matches_the_panel_on_its_side() {
        let t = Touch::default();
        // The panel's native top-left corner is the console's top-right.
        assert_eq!(t.map(0.0, 0.0), (1.0, 0.0));
        // Its native bottom-left is the console's top-left.
        assert_eq!(t.map(0.0, 1.0), (0.0, 0.0));
        assert_eq!(toml::from_str::<Config>("[touch]\nswap_xy = false\n").unwrap().touch.map(0.25, 0.5), (0.75, 0.5));
        assert!(toml::from_str::<Config>("[touch]\nrotate = 90\n").is_err());
    }

    #[test]
    fn backlight_settings_default_to_two_minutes_and_twenty_percent() {
        assert_eq!(Config::default().backlight, Backlight { dim_after_secs: 120, dim_percent: 20 });
        let c: Config = toml::from_str("[backlight]\ndim_after_secs = 0\n").unwrap();
        assert_eq!(c.backlight, Backlight { dim_after_secs: 0, dim_percent: 20 });
        assert!(toml::from_str::<Config>("[backlight]\ndim_percent = 300\n").is_err(), "not a u8");
    }
}
