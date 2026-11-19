//! Device › Screen and the logo tile's options: the screen's own preferences, kept next to the
//! layout in `/var/lib/nodo-screen`, which the screen's account owns.

#![deny(clippy::let_underscore_must_use, unused_must_use)]

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// "Turn display off after", as Device › Screen offers it.
pub const DISPLAY_OFF_LABELS: [&str; 5] = ["1 min", "5 min", "15 min", "1 hour", "Never"];
const DISPLAY_OFF: [u64; 5] = [60, 300, 900, 3600, 0];
const BRIGHTNESS: [u8; 4] = [40, 60, 80, 100];
/// When the console goes back to the tiles by itself.
const IDLE_RETURN: Duration = Duration::from_secs(120);

/// How the Nodo logo tile draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogoStyle {
    #[default]
    Drawn,
    /// The boot splash's own block-character logo.
    Text,
}

/// Unknown keys, such as the earlier `timeout_index` and `dim_index`, are ignored, so a file
/// written by an older screen still loads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub display_off_index: usize,
    pub brightness_index: usize,
    pub logo_style: LogoStyle,
    pub logo_shimmer: bool,
    pub logo_name: bool,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { display_off_index: 2, brightness_index: 3, logo_style: LogoStyle::Drawn, logo_shimmer: true, logo_name: true, path: None }
    }
}

impl Prefs {
    pub fn load(path: PathBuf) -> Self {
        let mut prefs: Prefs = std::fs::read_to_string(&path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
        prefs.display_off_index = prefs.display_off_index.min(DISPLAY_OFF.len() - 1);
        prefs.brightness_index = prefs.brightness_index.min(BRIGHTNESS.len() - 1);
        prefs.path = Some(path);
        prefs
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        let tmp = path.with_extension("tmp");
        let saved = toml::to_string(self)
            .map_err(|e| e.to_string())
            .and_then(|text| std::fs::write(&tmp, text).map_err(|e| e.to_string()))
            .and_then(|()| std::fs::rename(&tmp, path).map_err(|e| e.to_string()));
        // The screen keeps working with what it has, so the log is where a failed save shows.
        if let Err(e) = saved {
            crate::log(&format!("could not save the screen preferences: {e}"));
        }
    }

    /// When the console goes back to the tiles.
    pub fn idle_return(&self) -> Duration {
        IDLE_RETURN
    }

    /// How long without a touch before the display goes off; `None` keeps it on.
    pub fn display_off(&self) -> Option<Duration> {
        match DISPLAY_OFF[self.display_off_index] {
            0 => None,
            s => Some(Duration::from_secs(s)),
        }
    }

    /// "After 15 min idle", or "Always on".
    pub fn display_off_label(&self) -> String {
        match self.display_off_index {
            i if DISPLAY_OFF[i] == 0 => "Always on".to_owned(),
            i => format!("After {} idle", DISPLAY_OFF_LABELS[i]),
        }
    }

    /// The screen's brightness in use, in percent.
    pub fn brightness(&self) -> u8 {
        BRIGHTNESS[self.brightness_index]
    }

    /// The next brightness step, round to the first after the brightest.
    pub fn step_brightness(&mut self) {
        self.brightness_index = (self.brightness_index + 1) % BRIGHTNESS.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_round_trip_and_clamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui.toml");
        let mut p = Prefs::load(path.clone());
        assert_eq!(p.display_off_label(), "After 15 min idle");
        p.display_off_index = 4;
        p.logo_style = LogoStyle::Text;
        p.save();
        let q = Prefs::load(path.clone());
        assert_eq!(q.display_off_index, 4);
        assert_eq!((q.display_off(), q.display_off_label().as_str()), (None, "Always on"));
        assert_eq!(q.logo_style, LogoStyle::Text);
        // An older file's keys are ignored, and an index out of range is clamped.
        std::fs::write(&path, "timeout_index = 1\ndim_index = 2\ndisplay_off_index = 99\n").unwrap();
        assert_eq!(Prefs::load(path).display_off_index, 4);
    }

    #[test]
    fn brightness_steps_round() {
        let mut p = Prefs::default();
        assert_eq!(p.brightness(), 100);
        p.step_brightness();
        assert_eq!(p.brightness(), 40);
    }
}
