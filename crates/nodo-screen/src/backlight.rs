//! Dimming the panel's backlight when nobody is using the screen (docs/SCREEN.md, "Backlight").
//!
//! After `dim_after_secs` with no touch, click or key, the backlight goes down to `dim_percent` of
//! its maximum; any input brings it back, and that input only wakes the screen, so a tap on a dim
//! screen never presses a button. The screen never dims while someone arranges or shuts down.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::Backlight as Settings;

/// What to do with the backlight now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Full,
    Dim,
}

pub struct Dimmer {
    dim_after: Option<Duration>,
    pub percent: u8,
    last_input: Instant,
    dimmed: bool,
}

impl Dimmer {
    pub fn new(settings: &Settings, now: Instant) -> Self {
        Dimmer {
            dim_after: (settings.dim_after_secs > 0).then(|| Duration::from_secs(settings.dim_after_secs)),
            percent: settings.dim_percent.min(100),
            last_input: now,
            dimmed: false,
        }
    }

    /// Someone touched, clicked or typed. Returns the level to set, if it changes; when it does,
    /// the input woke the screen and should do nothing else.
    pub fn input(&mut self, now: Instant) -> Option<Level> {
        self.last_input = now;
        if self.dimmed {
            self.dimmed = false;
            Some(Level::Full)
        } else {
            None
        }
    }

    /// Called regularly. `may_dim` is false while arranging or shutting down.
    pub fn tick(&mut self, now: Instant, may_dim: bool) -> Option<Level> {
        if !may_dim {
            // Arranging or shutting down counts as use: start the wait again afterwards.
            self.last_input = now;
            if self.dimmed {
                self.dimmed = false;
                return Some(Level::Full);
            }
            return None;
        }
        let after = self.dim_after?;
        if !self.dimmed && now.duration_since(self.last_input) >= after {
            self.dimmed = true;
            return Some(Level::Dim);
        }
        None
    }
}

/// The panel's backlight in sysfs: the first device under `/sys/class/backlight`.
pub struct Device {
    brightness: PathBuf,
    max: u32,
}

impl Device {
    pub fn find(class: &Path) -> Option<Device> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(class).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        let dir = entries.into_iter().next()?;
        let max = std::fs::read_to_string(dir.join("max_brightness")).ok()?.trim().parse().ok().filter(|m| *m > 0)?;
        Some(Device { brightness: dir.join("brightness"), max })
    }

    pub fn value(&self, level: Level, percent: u8) -> u32 {
        match level {
            Level::Full => self.max,
            // Never quite off: a dim screen should still show it is on.
            Level::Dim => (self.max * u32::from(percent) / 100).max(1),
        }
    }

    pub fn set(&self, level: Level, percent: u8) -> std::io::Result<()> {
        std::fs::write(&self.brightness, self.value(level, percent).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(after: u64, percent: u8) -> Settings {
        Settings { dim_after_secs: after, dim_percent: percent }
    }

    #[test]
    fn dims_after_a_quiet_spell_and_wakes_on_input() {
        let t0 = Instant::now();
        let mut d = Dimmer::new(&settings(120, 20), t0);
        assert_eq!(d.tick(t0 + Duration::from_secs(119), true), None);
        assert_eq!(d.tick(t0 + Duration::from_secs(120), true), Some(Level::Dim));
        assert_eq!(d.tick(t0 + Duration::from_secs(200), true), None, "once");
        assert_eq!(d.input(t0 + Duration::from_secs(201)), Some(Level::Full), "the waking input does nothing else");
        assert_eq!(d.input(t0 + Duration::from_secs(202)), None, "the next one is a normal input");
        assert_eq!(d.tick(t0 + Duration::from_secs(321), true), None);
        assert_eq!(d.tick(t0 + Duration::from_secs(322), true), Some(Level::Dim));
    }

    #[test]
    fn never_dims_while_arranging_or_shutting_down_or_when_off() {
        let t0 = Instant::now();
        let mut d = Dimmer::new(&settings(120, 20), t0);
        assert_eq!(d.tick(t0 + Duration::from_secs(500), false), None);
        assert_eq!(d.tick(t0 + Duration::from_secs(501), true), None, "the wait starts again after arranging");
        assert_eq!(d.tick(t0 + Duration::from_secs(700), true), Some(Level::Dim));
        assert_eq!(d.tick(t0 + Duration::from_secs(701), false), Some(Level::Full), "a shutdown started over SSH lights it");
        let mut off = Dimmer::new(&settings(0, 20), t0);
        assert_eq!(off.tick(t0 + Duration::from_secs(100_000), true), None, "0 turns dimming off");
    }

    #[test]
    fn the_first_backlight_device_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let dev = dir.path().join("fde30000.dsi.0");
        std::fs::create_dir(&dev).unwrap();
        std::fs::write(dev.join("max_brightness"), "255\n").unwrap();
        std::fs::write(dev.join("brightness"), "255\n").unwrap();
        let device = Device::find(dir.path()).unwrap();
        device.set(Level::Dim, 20).unwrap();
        assert_eq!(std::fs::read_to_string(dev.join("brightness")).unwrap(), "51");
        device.set(Level::Full, 20).unwrap();
        assert_eq!(std::fs::read_to_string(dev.join("brightness")).unwrap(), "255");
        assert_eq!(device.value(Level::Dim, 0), 1, "never fully off");
        assert!(Device::find(&dir.path().join("none")).is_none());
    }
}
