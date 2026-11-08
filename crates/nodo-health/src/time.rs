//! Time sync status from chrony. Manifest expiry and TLS both depend on a correct clock.

use serde::Serialize;

use crate::sys::System;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimeStatus {
    pub synced: bool,
    pub stratum: Option<u32>,
    /// Positive when the system clock is ahead.
    pub offset_secs: Option<f64>,
    pub source: Option<String>,
    /// Set when chrony could not be asked at all.
    pub error: Option<String>,
}

/// Parses `chronyc -n tracking`.
pub fn parse_tracking(text: &str) -> TimeStatus {
    let field = |name: &str| {
        text.lines().find_map(|l| {
            let (key, value) = l.split_once(':')?;
            (key.trim() == name).then(|| value.trim().to_owned())
        })
    };
    let leap = field("Leap status");
    let stratum = field("Stratum").and_then(|s| s.parse().ok());
    let offset_secs = field("System time").and_then(|s| {
        let mut words = s.split_whitespace();
        let value: f64 = words.next()?.parse().ok()?;
        let _seconds = words.next()?;
        match words.next()? {
            "fast" => Some(value),
            "slow" => Some(-value),
            _ => None,
        }
    });
    let source = field("Reference ID").and_then(|s| {
        let name = s.split_once('(')?.1.trim_end_matches(')').trim().to_owned();
        (!name.is_empty()).then_some(name)
    });
    // Stratum 0 with a leap status of "Normal" never happens for a synced clock, so require both.
    let synced = leap.as_deref() == Some("Normal") && stratum.is_some_and(|s: u32| (1..16).contains(&s));
    let error = leap.is_none().then(|| "unexpected chronyc output".to_owned());
    TimeStatus { synced, stratum, offset_secs, source, error }
}

pub fn status(sys: &dyn System) -> TimeStatus {
    match sys.command("chronyc", &["-n", "tracking"]) {
        Some(text) => parse_tracking(&text),
        None => TimeStatus {
            synced: false,
            stratum: None,
            offset_secs: None,
            source: None,
            error: Some("chronyd is not answering".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNCED: &str = "Reference ID    : C0A80101 (192.168.1.1)
Stratum         : 3
Ref time (UTC)  : Sun Nov 08 10:00:00 2026
System time     : 0.000123456 seconds slow of NTP time
Last offset     : -0.000010000 seconds
Leap status     : Normal
";

    // Captured from the offline KVM dev VM.
    const UNSYNCED: &str = "Reference ID    : 00000000 ()
Stratum         : 0
Ref time (UTC)  : Thu Jan 01 00:00:00 1970
System time     : 0.000000000 seconds fast of NTP time
Last offset     : +0.000000000 seconds
Leap status     : Not synchronised
";

    #[test]
    fn reads_a_synced_clock() {
        let s = parse_tracking(SYNCED);
        assert!(s.synced);
        assert_eq!(s.stratum, Some(3));
        assert_eq!(s.offset_secs, Some(-0.000123456));
        assert_eq!(s.source.as_deref(), Some("192.168.1.1"));
        assert_eq!(s.error, None);
    }

    #[test]
    fn reads_an_unsynced_clock() {
        let s = parse_tracking(UNSYNCED);
        assert!(!s.synced);
        assert_eq!(s.stratum, Some(0));
        assert_eq!(s.source, None);
    }

    #[test]
    fn garbage_is_not_synced() {
        let s = parse_tracking("506 Cannot talk to daemon\n");
        assert!(!s.synced);
        assert!(s.error.is_some());
        assert!(!parse_tracking(&SYNCED.replace("Stratum         : 3", "Stratum         : 0")).synced);
    }
}
