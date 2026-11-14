//! System › Screen and the logo tile's options: the screen's own preferences, kept next to the
//! layout in `/var/lib/nodo-screen`, which the screen's account owns.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const TIMEOUT_LABELS: [&str; 4] = ["1 min", "2 min", "5 min", "Never"];
const TIMEOUTS: [u64; 4] = [60, 120, 300, 0];
pub const DIM_LABELS: [&str; 4] = ["Off", "10%", "20%", "40%"];
const DIMS: [u8; 4] = [0, 10, 20, 40];
pub const BRIGHTNESS_LABELS: [&str; 4] = ["40%", "60%", "80%", "100%"];
const BRIGHTNESS: [u8; 4] = [40, 60, 80, 100];

/// How the Nodo logo tile draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogoStyle {
    #[default]
    Drawn,
    /// The boot splash's own block-character logo.
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub timeout_index: usize,
    pub dim_index: usize,
    pub brightness_index: usize,
    pub logo_style: LogoStyle,
    pub logo_shimmer: bool,
    pub logo_name: bool,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { timeout_index: 1, dim_index: 2, brightness_index: 3, logo_style: LogoStyle::Drawn, logo_shimmer: true, logo_name: true, path: None }
    }
}

impl Prefs {
    pub fn load(path: PathBuf) -> Self {
        let mut prefs: Prefs = std::fs::read_to_string(&path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
        prefs.timeout_index = prefs.timeout_index.min(TIMEOUTS.len() - 1);
        prefs.dim_index = prefs.dim_index.min(DIMS.len() - 1);
        prefs.brightness_index = prefs.brightness_index.min(BRIGHTNESS.len() - 1);
        prefs.path = Some(path);
        prefs
    }

    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Ok(text) = toml::to_string(self) {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    /// When management goes back to the tiles; "Never" is a day.
    pub fn timeout(&self) -> Duration {
        match TIMEOUTS[self.timeout_index] {
            0 => Duration::from_secs(86_400),
            s => Duration::from_secs(s),
        }
    }

    /// The screen's brightness in use, in percent.
    pub fn brightness(&self) -> u8 {
        BRIGHTNESS[self.brightness_index]
    }

    /// The idle brightness in percent; 0 keeps the screen bright.
    pub fn dim_percent(&self) -> u8 {
        DIMS[self.dim_index]
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
        p.timeout_index = 3;
        p.logo_style = LogoStyle::Text;
        p.save();
        let q = Prefs::load(path.clone());
        assert_eq!(q.timeout_index, 3);
        assert_eq!(q.logo_style, LogoStyle::Text);
        std::fs::write(&path, "timeout_index = 99\n").unwrap();
        assert_eq!(Prefs::load(path).timeout_index, 3);
    }
}
