//! The structured report the Home screen reads from `/run/nodo/health.json`.

use serde::Serialize;

use crate::disk::Level;
use crate::memory::MemoryStatus;
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
    pub uptime_secs: Option<u64>,
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

pub fn temperature(sys: &dyn System) -> Option<f64> {
    let milli: i64 = sys.read("/sys/class/thermal/thermal_zone0/temp".as_ref()).ok().flatten()?.trim().parse().ok()?;
    Some(milli as f64 / 1000.0)
}

pub fn uptime(sys: &dyn System) -> Option<u64> {
    let text = sys.read("/proc/uptime".as_ref()).ok().flatten()?;
    text.split_whitespace().next()?.parse::<f64>().ok().map(|s| s as u64)
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

pub fn attention(disks: &[DiskReport], stopped: &[StoppedService], mem: &[MemoryStatus], time: &TimeStatus) -> Vec<Attention> {
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
    if !time.synced {
        out.push(Attention { severity: Severity::Info, message: "The clock is not synchronised yet.".into() });
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.severity));
    out
}
