//! The structured report the Home screen reads from `/run/nodo/health.json`.

use std::path::Path;

use serde::Serialize;

use crate::config::AbstractSocketConfig;
use crate::disk::Level;
use crate::memory::MemoryStatus;
use crate::sockets::{self, Kind, Owner};
use crate::sys::System;
use crate::time::TimeStatus;

pub const REPORT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiskReport {
    pub name: String,
    pub path: String,
    pub mounted: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub level: Level,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoppedService {
    pub service: String,
    pub filesystem: String,
    pub since: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogReport {
    pub bytes: u64,
    pub dirs: usize,
    pub pruned_files: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attention {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryTotals {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// An abstract Unix socket accepting connections or datagrams, see [`crate::sockets`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AbstractSocket {
    pub name: String,
    pub kind: Kind,
    pub inode: u64,
    /// Empty when no process holding it could be read.
    pub owners: Vec<Owner>,
    /// Listed in the configuration's `abstract_sockets.expected`.
    pub expected: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AbstractSockets {
    /// `/proc/net/unix` could be read.
    pub scanned: bool,
    pub landlock_abi: Option<i64>,
    /// The kernel has Landlock scoping (ABI 6, Linux 6.12), so nodo-confine keeps every
    /// sandboxed service away from abstract sockets created outside its own sandbox.
    pub sandbox_blocks: bool,
    pub kernel: Option<String>,
    pub listeners: Vec<AbstractSocket>,
}

impl AbstractSockets {
    pub fn unexpected(&self) -> impl Iterator<Item = &AbstractSocket> {
        self.listeners.iter().filter(|l| !l.expected)
    }
}

/// Lists the abstract sockets listening now and marks the ones the configuration expects.
pub fn abstract_sockets(cfg: &AbstractSocketConfig, sys: &dyn System) -> AbstractSockets {
    let landlock_abi = sys.landlock_abi();
    let kernel = sys
        .read("/proc/sys/kernel/osrelease".as_ref())
        .ok()
        .flatten()
        .map(|k| sockets::escape(k.trim().as_bytes()));
    let base = AbstractSockets {
        landlock_abi,
        sandbox_blocks: landlock_abi.is_some_and(|abi| abi >= nodo_confine::LANDLOCK_SCOPE_ABI),
        kernel,
        ..AbstractSockets::default()
    };
    let Some(text) = sys.read_bytes("/proc/net/unix".as_ref()).ok().flatten() else { return base };
    let found = sockets::parse(&text);
    let mut owners = sys.socket_owners(&found.iter().map(|l| l.inode).collect());
    let listeners = found
        .into_iter()
        .map(|l| {
            let owners = owners.remove(&l.inode).unwrap_or_default();
            let expected = cfg.expected.iter().any(|e| {
                e.matches_name(&l.name)
                    && e.user.as_ref().is_none_or(|u| !owners.is_empty() && owners.iter().all(|o| &o.user == u))
            });
            AbstractSocket { name: l.name, kind: l.kind, inode: l.inode, owners, expected }
        })
        .collect();
    AbstractSockets { scanned: true, listeners, ..base }
}

/// Unexpected listeners named on Home one by one before the rest are counted.
const NAMED_SOCKETS: usize = 3;

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(max.saturating_sub(3)).collect();
    short.push_str("...");
    short
}

/// Who holds a socket, in words.
pub fn owner_text(owners: &[Owner]) -> String {
    let Some(first) = owners.first() else { return "a process that could not be found".to_owned() };
    let mut text =
        format!("{} (pid {}) as user {}", shorten(&first.process, 32), first.pid, shorten(&first.user, 32));
    match owners.len() {
        1 => {}
        2 => text.push_str(" and 1 more process"),
        n => text.push_str(&format!(" and {} more processes", n - 1)),
    }
    text
}

fn socket_attention(s: &AbstractSockets) -> Vec<Attention> {
    let unexpected: Vec<_> = s.unexpected().collect();
    if unexpected.is_empty() {
        return Vec::new();
    }
    let verdict = if s.sandbox_blocks {
        "This kernel's sandbox keeps services away from it.".to_owned()
    } else {
        let kernel = s.kernel.as_deref().map(|k| format!(" ({})", shorten(k, 40))).unwrap_or_default();
        format!("This kernel{kernel} cannot stop sandboxed services that use Unix sockets, such as Caddy, from connecting to it.")
    };
    let mut out: Vec<Attention> = unexpected
        .iter()
        .take(NAMED_SOCKETS)
        .map(|l| Attention {
            severity: Severity::Warning,
            message: format!(
                "An unexpected program listens on the abstract Unix socket {}: {}. {verdict}",
                shorten(&l.name, 60),
                owner_text(&l.owners)
            ),
        })
        .collect();
    if unexpected.len() > NAMED_SOCKETS {
        out.push(Attention {
            severity: Severity::Warning,
            message: format!(
                "{} more unexpected abstract Unix sockets are open. nodo-healthd --abstract-sockets lists them all.",
                unexpected.len() - NAMED_SOCKETS
            ),
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub version: u32,
    pub generated_at: u64,
    pub disks: Vec<DiskReport>,
    /// Stopped by this daemon to protect a filesystem. They start again on their own.
    pub stopped: Vec<StoppedService>,
    /// The system disk is low: installs and updates must wait.
    pub refuse_installs: bool,
    pub memory: Option<MemoryTotals>,
    pub services_memory: Vec<MemoryStatus>,
    pub logs: LogReport,
    pub time: TimeStatus,
    pub temperature_c: Option<f64>,
    pub ssd_temperature_c: Option<f64>,
    pub uptime_secs: Option<u64>,
    pub abstract_sockets: AbstractSockets,
    /// Most severe first.
    pub attention: Vec<Attention>,
}

impl Report {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("reports always serialize")
    }
}

pub fn meminfo(sys: &dyn System) -> Option<MemoryTotals> {
    let text = sys.read("/proc/meminfo".as_ref()).ok().flatten()?;
    let kib = |name: &str| {
        text.lines().find_map(|l| l.strip_prefix(name)?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok())
    };
    Some(MemoryTotals { total_bytes: kib("MemTotal:")? * 1024, available_bytes: kib("MemAvailable:")? * 1024 })
}

/// The hottest thermal zone, in °C. The board's zones are the SoC package, its CPU clusters, the
/// GPU and the NPU, and the kernel slows whichever block is hot, so the hottest is the one that
/// matters; zone 0 alone (the package) reads cooler than a busy CPU cluster. The screen's Device
/// panel shows the same number (`nodo_panels::source::device`).
pub fn temperature(sys: &dyn System) -> Option<f64> {
    let root = Path::new("/sys/class/thermal");
    sys.list(root)
        .ok()?
        .into_iter()
        .filter(|entry| entry.name.starts_with("thermal_zone"))
        .filter_map(|entry| milli_degrees(sys, &root.join(&entry.name).join("temp")))
        .max()
        .map(|milli| milli as f64 / 1000.0)
}

/// The SSD's own sensor, in °C, through the kernel's NVMe hwmon driver (`CONFIG_NVME_HWMON`): the
/// hottest NVMe drive when there are several, none when no drive reports one.
pub fn ssd_temperature(sys: &dyn System) -> Option<f64> {
    let root = Path::new("/sys/class/hwmon");
    sys.list(root)
        .ok()?
        .into_iter()
        .map(|entry| root.join(&entry.name))
        .filter(|dir| sys.read(&dir.join("name")).ok().flatten().is_some_and(|name| name.trim() == "nvme"))
        .filter_map(|dir| milli_degrees(sys, &dir.join("temp1_input")))
        .max()
        .map(|milli| milli as f64 / 1000.0)
}

fn milli_degrees(sys: &dyn System, path: &Path) -> Option<i64> {
    sys.read(path).ok().flatten()?.trim().parse().ok()
}

pub fn uptime(sys: &dyn System) -> Option<u64> {
    let text = sys.read("/proc/uptime".as_ref()).ok().flatten()?;
    text.split_whitespace().next()?.parse::<f64>().ok().map(|s| s as u64)
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

pub fn attention(
    disks: &[DiskReport],
    stopped: &[StoppedService],
    mem: &[MemoryStatus],
    time: &TimeStatus,
    sockets: &AbstractSockets,
) -> Vec<Attention> {
    let mut out = Vec::new();
    for d in disks {
        let message = match (d.mounted, d.level) {
            (false, _) => Some((Severity::Warning, format!("The {} drive is not mounted at {}.", d.name, d.path))),
            (true, Level::Critical) => {
                let names: Vec<_> =
                    stopped.iter().filter(|s| s.filesystem == d.name).map(|s| s.service.as_str()).collect();
                let what = if names.is_empty() { String::new() } else { format!(" {} stopped to protect its data.", names.join(", ")) };
                Some((
                    Severity::Critical,
                    format!("The {} disk is almost full ({} free).{what} It restarts once space is freed.", d.name, gib(d.free_bytes)),
                ))
            }
            (true, Level::Low) => Some((
                Severity::Warning,
                format!("The {} disk is low on space ({} free). Updates wait until there is more.", d.name, gib(d.free_bytes)),
            )),
            (true, Level::Ok) => None,
        };
        if let Some((severity, message)) = message {
            out.push(Attention { severity, message });
        }
    }
    for m in mem.iter().filter(|m| m.oom_kills > 0) {
        out.push(Attention {
            severity: Severity::Warning,
            message: format!("{} ran out of its memory limit {} times.", m.service, m.oom_kills),
        });
    }
    out.extend(socket_attention(sockets));
    if !time.synced {
        out.push(Attention { severity: Severity::Info, message: "The clock is not synchronised yet.".into() });
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.severity));
    out
}

#[cfg(test)]
mod temperature_tests {
    use super::{ssd_temperature, temperature};
    use crate::sys::fake::FakeSystem;

    #[test]
    fn the_hottest_thermal_zone_counts_not_zone_zero() {
        let sys = FakeSystem::default();
        sys.put("/sys/class/thermal/thermal_zone0/temp", "48000\n");
        sys.put("/sys/class/thermal/thermal_zone1/temp", "61500\n");
        sys.put("/sys/class/thermal/thermal_zone2/temp", "garbage\n");
        sys.put("/sys/class/thermal/cooling_device0/cur_state", "0\n");
        assert_eq!(temperature(&sys), Some(61.5));
    }

    #[test]
    fn no_thermal_zone_means_no_temperature() {
        let sys = FakeSystem::default();
        sys.put("/sys/class/thermal/cooling_device0/cur_state", "0\n");
        assert_eq!(temperature(&sys), None);
        assert_eq!(ssd_temperature(&sys), None);
    }

    #[test]
    fn the_ssd_reads_only_nvme_hwmon_sensors() {
        let sys = FakeSystem::default();
        sys.put("/sys/class/hwmon/hwmon0/name", "cpu_thermal\n");
        sys.put("/sys/class/hwmon/hwmon0/temp1_input", "90000\n");
        sys.put("/sys/class/hwmon/hwmon1/name", "nvme\n");
        sys.put("/sys/class/hwmon/hwmon1/temp1_input", "44850\n");
        assert_eq!(ssd_temperature(&sys), Some(44.85));
    }
}
