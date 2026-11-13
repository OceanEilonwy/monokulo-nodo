//! `/etc/nodo/screen.toml`: how the touchscreen's axes map onto the console.

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub touch: Touch,
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
}
