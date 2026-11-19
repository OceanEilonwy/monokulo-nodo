//! Dimming the panel's backlight when nobody is using the screen (docs/SCREEN.md, "Backlight").
//!
//! After `dim_after_secs` with no touch, click or key, the backlight goes down to `dim_percent` of
//! its maximum, and after Device › Screen's "Turn display off after" it goes off. Any input brings
//! it back, and that input only wakes the screen, so a tap on a dim or dark screen never presses a
//! button. The screen never dims while someone arranges or shuts down.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::Backlight as Settings;

/// What to do with the backlight now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Full,
    Dim,
    Off,
}

pub struct Dimmer {
    dim_after: Option<Duration>,
    /// When the display goes off; `None` keeps it on.
    pub off_after: Option<Duration>,
    pub percent: u8,
    last_input: Instant,
    /// What idle has done to the backlight: Full while in use.
    level: Level,
}

impl Dimmer {
    pub fn new(settings: &Settings, now: Instant) -> Self {
        Dimmer {
            dim_after: (settings.dim_after_secs > 0).then(|| Duration::from_secs(settings.dim_after_secs)),
            off_after: None,
            percent: settings.dim_percent.min(100),
            last_input: now,
            level: Level::Full,
        }
    }

    /// Takes the backlight back to full, if idle had lowered it.
    fn wake(&mut self) -> Option<Level> {
        let was = std::mem::replace(&mut self.level, Level::Full);
        (was != Level::Full).then_some(Level::Full)
    }

    /// Someone touched, clicked or typed. Returns the level to set, if it changes; when it does,
    /// the input woke the screen and should do nothing else.
    pub fn input(&mut self, now: Instant) -> Option<Level> {
        self.last_input = now;
        self.wake()
    }

    /// Called regularly. `may_dim` is false while arranging or shutting down.
    pub fn tick(&mut self, now: Instant, may_dim: bool) -> Option<Level> {
        if !may_dim {
            // Arranging or shutting down counts as use: start the wait again afterwards.
            self.last_input = now;
            return self.wake();
        }
        let idle = now.duration_since(self.last_input);
        let next = if self.off_after.is_some_and(|after| idle >= after) {
            Level::Off
        } else if self.dim_after.is_some_and(|after| idle >= after) {
            Level::Dim
        } else {
            return None;
        };
        // Only ever down while idle: a display that went off does not light again to dim.
        let lower = matches!((self.level, next), (Level::Full, Level::Dim | Level::Off) | (Level::Dim, Level::Off));
        if lower {
            self.level = next;
            return Some(next);
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
            Level::Off => 0,
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
    fn the_display_goes_off_after_its_time_and_a_touch_lights_it() {
        let t0 = Instant::now();
        let mut d = Dimmer::new(&settings(120, 20), t0);
        d.off_after = Some(Duration::from_secs(300));
        assert_eq!(d.tick(t0 + Duration::from_secs(120), true), Some(Level::Dim));
        assert_eq!(d.tick(t0 + Duration::from_secs(300), true), Some(Level::Off));
        assert_eq!(d.tick(t0 + Duration::from_secs(400), true), None, "once");
        assert_eq!(d.input(t0 + Duration::from_secs(401)), Some(Level::Full), "the waking touch does nothing else");
        // Off before the dim time: straight to off.
        let mut quick = Dimmer::new(&settings(120, 20), t0);
        quick.off_after = Some(Duration::from_secs(60));
        assert_eq!(quick.tick(t0 + Duration::from_secs(60), true), Some(Level::Off));
        assert_eq!(quick.tick(t0 + Duration::from_secs(120), true), None, "it does not light again to dim");
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
        assert_eq!(device.value(Level::Dim, 0), 1, "dim is never fully off");
        assert_eq!(device.value(Level::Off, 20), 0);
        assert!(Device::find(&dir.path().join("none")).is_none());
    }
}
