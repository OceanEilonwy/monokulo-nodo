//! Thresholds and limits. The helper writes `/etc/nodo/health.toml` from installed manifests,
//! since it knows which services write to which filesystem.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// Free space below the larger of an absolute amount and a share of the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Threshold {
    pub bytes: u64,
    #[serde(default)]
    pub percent: f64,
}

impl Threshold {
    pub fn of(&self, total: u64) -> u64 {
        let share = (total as f64 * self.percent.clamp(0.0, 100.0) / 100.0) as u64;
        self.bytes.max(share)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filesystem {
    pub name: String,
    pub path: PathBuf,
    /// Below this the filesystem is low: Home warns, and installs and updates wait.
    pub warn: Threshold,
    /// Below this the services listed are stopped before a write can fail.
    pub stop: Threshold,
    /// Stopped services start again only once free space exceeds `stop` plus this, so a
    /// service that writes a little at startup does not flap.
    pub resume_margin: Threshold,
    #[serde(default)]
    pub services: Vec<String>,
    /// A data drive that is not mounted is reported instead of measured, because measuring the
    /// directory underneath would report the system disk.
    #[serde(default)]
    pub require_mount: bool,
    /// The system disk. When it is low, installs are refused and rotated logs are pruned.
    #[serde(default)]
    pub system: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogConfig {
    /// Each holds svlogd directories, one level down, like `/var/log/cuprated`.
    pub roots: Vec<PathBuf>,
    /// svlogd's `s`: bytes per file before rotating.
    pub file_bytes: u64,
    /// svlogd's `n`: rotated files kept.
    pub files: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryLimit {
    pub user: String,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    /// The cgroup v2 mount.
    pub cgroup_root: PathBuf,
    #[serde(default)]
    pub limits: BTreeMap<String, MemoryLimit>,
}

/// An abstract Unix socket that is allowed to listen without a warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedSocket {
    /// As listed, starting with `@`. A trailing `*` matches any rest of the name.
    pub name: String,
    /// When set, the socket is expected only while every process holding it runs as this
    /// user, so another user cannot take the name first and pass unnoticed.
    #[serde(default)]
    pub user: Option<String>,
}

impl ExpectedSocket {
    pub fn matches_name(&self, name: &str) -> bool {
        match self.name.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == self.name,
        }
    }
}

/// Abstract Unix sockets the sandbox may fail to keep services away from (see
/// [`crate::sockets`]). The base system expects none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbstractSocketConfig {
    #[serde(default)]
    pub expected: Vec<ExpectedSocket>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub interval_secs: u64,
    pub sv_dir: PathBuf,
    pub state_path: PathBuf,
    pub report_path: PathBuf,
    pub filesystems: Vec<Filesystem>,
    pub logs: LogConfig,
    pub memory: MemoryConfig,
    #[serde(default)]
    pub abstract_sockets: AbstractSocketConfig,
}

impl Default for Config {
    /// Sized for the NanoPC-T6N: a small eMMC system partition and a large NVMe chain drive.
    /// cuprated writes in batches during sync, so the chain drive keeps several GiB spare.
    fn default() -> Self {
        Config {
            interval_secs: 10,
            sv_dir: "/etc/sv".into(),
            state_path: "/var/lib/nodo/health/state.toml".into(),
            report_path: "/run/nodo/health.json".into(),
            filesystems: vec![
                Filesystem {
                    name: "system".into(),
                    path: "/".into(),
                    warn: Threshold { bytes: GIB, percent: 15.0 },
                    stop: Threshold { bytes: 384 * MIB, percent: 5.0 },
                    resume_margin: Threshold { bytes: 128 * MIB, percent: 1.0 },
                    services: Vec::new(),
                    require_mount: false,
                    system: true,
                },
                Filesystem {
                    name: "data".into(),
                    path: "/media/monero".into(),
                    warn: Threshold { bytes: 20 * GIB, percent: 5.0 },
                    stop: Threshold { bytes: 4 * GIB, percent: 1.0 },
                    resume_margin: Threshold { bytes: 2 * GIB, percent: 0.5 },
                    services: vec!["cuprated".into()],
                    require_mount: true,
                    system: false,
                },
            ],
            logs: LogConfig { roots: vec!["/var/log".into(), "/var/log/apps".into()], file_bytes: MIB, files: 5 },
            memory: MemoryConfig { cgroup_root: "/sys/fs/cgroup".into(), limits: BTreeMap::new() },
            abstract_sockets: AbstractSocketConfig::default(),
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        let cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        for fs in &self.filesystems {
            if fs.stop.bytes == 0 && fs.stop.percent <= 0.0 {
                return Err(format!("{}: a zero stop threshold protects nothing", fs.name));
            }
            for svc in &fs.services {
                if svc.is_empty() || svc.contains('/') || svc.starts_with('.') {
                    return Err(format!("{}: bad service name {svc:?}", fs.name));
                }
            }
        }
        for expected in &self.abstract_sockets.expected {
            if !expected.name.starts_with('@') || expected.name == "@*" {
                return Err(format!("expected abstract socket {:?} must start with @ and name something", expected.name));
            }
        }
        if self.logs.file_bytes < 4096 || self.logs.files == 0 {
            return Err("log files must be at least 4 KiB, and at least one kept".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_take_the_larger_bound() {
        let t = Threshold { bytes: 4 * GIB, percent: 1.0 };
        assert_eq!(t.of(100 * GIB), 4 * GIB);
        assert_eq!(t.of(2000 * GIB), 20 * GIB);
        assert_eq!(Threshold { bytes: 0, percent: 500.0 }.of(10), 10, "percent is clamped");
    }

    #[test]
    fn default_round_trips_and_validates() {
        let cfg = Config::default();
        let text = toml::to_string(&cfg).unwrap();
        assert_eq!(Config::parse(&text).unwrap(), cfg);
    }

    #[test]
    fn refuses_useless_or_unsafe_values() {
        let mut cfg = Config::default();
        cfg.filesystems[1].stop = Threshold { bytes: 0, percent: 0.0 };
        assert!(cfg.validate().is_err());
        let mut cfg = Config::default();
        cfg.filesystems[1].services.push("../../bin".into());
        assert!(cfg.validate().is_err());
        assert!(Config::parse("interval_secs = 1\nbogus = 2\n").is_err());
        for name in ["/run/x.sock", "@*", ""] {
            let mut cfg = Config::default();
            cfg.abstract_sockets.expected.push(ExpectedSocket { name: name.into(), user: None });
            assert!(cfg.validate().is_err(), "{name:?}");
        }
    }

    #[test]
    fn expects_no_abstract_sockets_by_default_and_reads_older_files() {
        let cfg = Config::default();
        assert!(cfg.abstract_sockets.expected.is_empty());
        let mut text = toml::to_string(&cfg).unwrap();
        let start = text.find("[abstract_sockets]").unwrap();
        text.truncate(start);
        assert_eq!(Config::parse(&text).unwrap(), cfg, "a file from before this section still parses");
    }

    #[test]
    fn expected_names_match_exactly_or_by_prefix() {
        let exact = ExpectedSocket { name: "@foo".into(), user: None };
        assert!(exact.matches_name("@foo") && !exact.matches_name("@foobar") && !exact.matches_name("@fo"));
        let prefix = ExpectedSocket { name: "@foo/*".into(), user: Some("root".into()) };
        assert!(prefix.matches_name("@foo/1") && prefix.matches_name("@foo/") && !prefix.matches_name("@foo"));
    }
}
