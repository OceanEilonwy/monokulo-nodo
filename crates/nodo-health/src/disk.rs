//! Free-space levels with hysteresis.

use serde::{Deserialize, Serialize};

use crate::config::Filesystem;
use crate::sys::Space;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Level {
    #[default]
    Ok,
    /// Warn, and hold back installs and updates.
    Low,
    /// Stop the services writing here.
    Critical,
}

pub fn evaluate(fs: &Filesystem, space: Space, previous: Level) -> Level {
    let stop = fs.stop.of(space.total);
    let warn = fs.warn.of(space.total).max(stop);
    let resume = stop.saturating_add(fs.resume_margin.of(space.total));
    if space.available < stop || (previous == Level::Critical && space.available < resume) {
        Level::Critical
    } else if space.available < warn {
        Level::Low
    } else {
        Level::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Threshold;

    fn fs() -> Filesystem {
        Filesystem {
            name: "data".into(),
            path: "/d".into(),
            warn: Threshold { bytes: 100, percent: 0.0 },
            stop: Threshold { bytes: 40, percent: 0.0 },
            resume_margin: Threshold { bytes: 20, percent: 0.0 },
            services: vec!["cuprated".into()],
            require_mount: true,
            system: false,
        }
    }

    fn at(available: u64, previous: Level) -> Level {
        evaluate(&fs(), Space { total: 1000, available }, previous)
    }

    #[test]
    fn levels_follow_free_space() {
        assert_eq!(at(500, Level::Ok), Level::Ok);
        assert_eq!(at(100, Level::Ok), Level::Ok);
        assert_eq!(at(99, Level::Ok), Level::Low);
        assert_eq!(at(40, Level::Ok), Level::Low);
        assert_eq!(at(39, Level::Ok), Level::Critical);
        assert_eq!(at(0, Level::Ok), Level::Critical);
    }

    #[test]
    fn critical_holds_until_the_resume_margin_is_free() {
        assert_eq!(at(45, Level::Critical), Level::Critical);
        assert_eq!(at(59, Level::Critical), Level::Critical);
        assert_eq!(at(60, Level::Critical), Level::Low);
        assert_eq!(at(45, Level::Low), Level::Low, "the margin applies only on the way up");
    }
}
