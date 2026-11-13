//! `/etc/nodo/screen.toml`, shared with nodo-screen: when the backlight dims. Its `[touch]` table
//! is nodo-screen's (libinput and the display's rotation turn touches for nodo-gui); it is read
//! only so that the same file stays valid for both.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_matches_the_panel_on_its_side() {
        // nodo-screen reads [touch]; nodo-gui takes the same file, so it must accept it.
        let c: Config = toml::from_str("[touch]\nswap_xy = false\n").unwrap();
        assert!(!c.touch.swap_xy);
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
