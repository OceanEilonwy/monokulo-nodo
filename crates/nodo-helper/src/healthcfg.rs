//! The health daemon's configuration, written from installed manifests, and its report for Home.
//!
//! The helper knows which services exist and where they write, so it writes
//! `/etc/nodo/health.toml`: apps write to the system disk, and the node writes to the data drive
//! when the board has one. The daemon's report is read back here, together with the boot
//! fallback notice, so the TUI gets both through the one helper connection.

use std::path::Path;

use nodo_api::{ApiError, DiskHealth, HealthNote, HealthSeverity, HealthSummary};
use nodo_gen::Profile;
use nodo_health::Config;
use serde::Deserialize;

use crate::helper::Helper;
use crate::manage::failed;
use crate::system::{SvAction, System, WriteOptions};

const NODE_ROLE: &str = "monero-daemon";
const HEALTHD: &str = "nodo-healthd";
/// A report older than this many daemon intervals means the daemon has stopped.
const STALE_INTERVALS: u64 = 6;
const CONFIG_FILE: WriteOptions = WriteOptions {
    mode: 0o644,
    group: None,
};

/// The parts of `nodo_health::Report` Home needs. Unknown fields are ignored, so the daemon can
/// add to its report without breaking the helper.
#[derive(Debug, Deserialize)]
struct Report {
    generated_at: u64,
    #[serde(default)]
    disks: Vec<Disk>,
    #[serde(default)]
    stopped: Vec<Stopped>,
    #[serde(default)]
    refuse_installs: bool,
    time: Time,
    #[serde(default)]
    temperature_c: Option<f64>,
    #[serde(default)]
    attention: Vec<Note>,
}

#[derive(Debug, Deserialize)]
struct Disk {
    name: String,
    path: String,
    mounted: bool,
    total_bytes: u64,
    free_bytes: u64,
    level: String,
}

#[derive(Debug, Deserialize)]
struct Stopped {
    service: String,
}

#[derive(Debug, Deserialize)]
struct Time {
    synced: bool,
}

#[derive(Debug, Deserialize)]
struct Note {
    severity: String,
    message: String,
}

fn severity(text: &str) -> HealthSeverity {
    match text {
        "Critical" => HealthSeverity::Critical,
        "Warning" => HealthSeverity::Warning,
        _ => HealthSeverity::Info,
    }
}

/// Text from a file root wrote, still kept short and printable before a terminal shows it.
fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect::<String>()
        .trim()
        .to_owned()
}

impl<S: System> Helper<S> {
    /// The configuration the installed packages call for.
    pub(crate) fn health_config(&self) -> Result<Config, ApiError> {
        let (packages, _) = self.packages()?;
        let mut cfg = Config::default();
        let apps: Vec<String> = packages
            .iter()
            .filter(|p| p.profile == Profile::App)
            .map(|p| p.id().to_owned())
            .collect();
        let nodes: Vec<String> = packages
            .iter()
            .filter(|p| p.manifest.provides.iter().any(|r| r == NODE_ROLE))
            .map(|p| p.id().to_owned())
            .collect();
        cfg.sv_dir = self.paths.sv_dir.clone();
        cfg.report_path = self.manage.paths.health_report.clone();
        let (mut system, mut data) = (cfg.filesystems[0].clone(), cfg.filesystems[1].clone());
        system.services = apps;
        cfg.filesystems = match &self.paths.drive {
            Some(drive) => {
                data.path = drive.mount_point.clone();
                data.services = nodes;
                vec![system, data]
            }
            None => {
                // Without a data drive the node writes to the system disk too.
                system.services.extend(nodes);
                vec![system]
            }
        };
        cfg.validate().map_err(failed)?;
        Ok(cfg)
    }

    /// Writes the configuration when it changed, and restarts the daemon so it reads it.
    pub fn refresh_health_config(&self) -> Result<(), ApiError> {
        let cfg = self.health_config()?;
        let text = toml::to_string(&cfg).map_err(failed)?;
        let path = &self.manage.paths.health_config;
        if self.sys.read_file(path)?.as_deref() == Some(text.as_bytes()) {
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            self.sys.ensure_dir(dir, 0o755, None)?;
        }
        self.sys.atomic_write(path, text.as_bytes(), CONFIG_FILE)?;
        if self.sys.read_link(&self.paths.service_dir.join(HEALTHD)).is_some() {
            self.sys.sv(SvAction::ForceRestart, HEALTHD, 15)?;
        }
        Ok(())
    }

    fn read_report(&self) -> Option<Report> {
        let bytes = self
            .sys
            .read_file(&self.manage.paths.health_report)
            .ok()??;
        serde_json::from_slice(&bytes).ok()
    }

    fn report_is_fresh(&self, report: &Report) -> bool {
        let interval = self
            .sys
            .read_file(&self.manage.paths.health_config)
            .ok()
            .flatten()
            .and_then(|b| Config::parse(&String::from_utf8_lossy(&b)).ok())
            .map(|c| c.interval_secs)
            .unwrap_or(Config::default().interval_secs)
            .max(1);
        let now = self.manage.packages.now();
        now.saturating_sub(report.generated_at) <= interval * STALE_INTERVALS
    }

    /// Whether installs and updates must wait for disk space. Without a fresh report the
    /// system disk is measured here against the same warning threshold.
    pub(crate) fn installs_refused(&self) -> Result<bool, ApiError> {
        if let Some(report) = self.read_report().filter(|r| self.report_is_fresh(r)) {
            return Ok(report.refuse_installs);
        }
        let (total, free) = self.sys.fs_usage(Path::new("/"))?;
        let warn = Config::default().filesystems[0].warn.of(total);
        Ok(free < warn)
    }

    pub(crate) fn health_summary(&self) -> Result<HealthSummary, ApiError> {
        let boot_fallback = self
            .sys
            .read_file(&self.manage.paths.boot_fallback)?
            .map(|b| printable(&String::from_utf8_lossy(&b)))
            .map(|t| {
                if t.is_empty() {
                    "The device started its previous system after the new one failed.".to_owned()
                } else {
                    t
                }
            });
        let Some(report) = self.read_report() else {
            return Ok(HealthSummary {
                reporting: false,
                generated_at: 0,
                notes: vec![HealthNote {
                    severity: HealthSeverity::Warning,
                    message: "The health monitor is not reporting, so disk protection is unknown."
                        .to_owned(),
                }],
                disks: Vec::new(),
                stopped: Vec::new(),
                refuse_installs: self.installs_refused()?,
                time_synced: false,
                temperature_decic: None,
                boot_fallback,
            });
        };
        let reporting = self.report_is_fresh(&report);
        let mut notes: Vec<HealthNote> = report
            .attention
            .iter()
            .map(|n| HealthNote {
                severity: severity(&n.severity),
                message: printable(&n.message),
            })
            .collect();
        if !reporting {
            notes.insert(
                0,
                HealthNote {
                    severity: HealthSeverity::Warning,
                    message: "The health monitor stopped reporting. What it last saw is shown."
                        .to_owned(),
                },
            );
        }
        notes.sort_by_key(|n| std::cmp::Reverse(n.severity));
        Ok(HealthSummary {
            reporting,
            generated_at: report.generated_at,
            notes,
            disks: report
                .disks
                .iter()
                .map(|d| DiskHealth {
                    name: printable(&d.name),
                    path: printable(&d.path),
                    mounted: d.mounted,
                    total_bytes: d.total_bytes,
                    free_bytes: d.free_bytes,
                    level: printable(&d.level),
                })
                .collect(),
            stopped: report.stopped.iter().map(|s| printable(&s.service)).collect(),
            refuse_installs: report.refuse_installs,
            time_synced: report.time.synced,
            temperature_decic: report.temperature_c.map(|t| (t * 10.0).round() as i32),
            boot_fallback,
        })
    }
}
