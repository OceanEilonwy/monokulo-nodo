//! Read-only views of the device: overview, packages and logs.

use std::path::{Path, PathBuf};

use nodo_api::{
    ApiError, CpuStatus, DeviceOverview, EncryptionState, LogChunk, PackageDetail, PackageInfo,
    Response,
};
use nodo_manifest::{Protocol, Scope};

use crate::catalog::Package;
use crate::helper::Helper;
use crate::system::System;

/// The helper's own log, as its runit log service writes it.
pub const HELPER_LOG_SOURCE: &str = "nodo-helper";
const HELPER_LOG: &str = "/var/log/nodo-helper/current";
const MAX_LOG_LINES: u32 = 2000;
const MAX_LINE_CHARS: usize = 1000;
/// Logs are read whole, so a runaway one is refused rather than loaded.
const MAX_LOG_BYTES: usize = 16 * 1024 * 1024;
/// The thermal zones looked at for the CPU's temperature, from `thermal_zone0`.
const MAX_THERMAL_ZONES: u32 = 32;

fn first_line(bytes: Option<Vec<u8>>) -> String {
    bytes
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_owned()
        })
        .unwrap_or_default()
}

/// `MemTotal:  123 kB` style fields.
fn meminfo_kib(text: &str, field: &str) -> u64 {
    text.lines()
        .find_map(|line| line.strip_prefix(field)?.strip_prefix(':'))
        .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

/// The one-minute load average of `/proc/loadavg` (`0.42 0.30 0.25 1/123 4567`), times 1000.
fn load_average_milli(line: &str) -> u32 {
    let Some(first) = line.split_whitespace().next() else {
        return 0;
    };
    let (whole, fraction) = first.split_once('.').unwrap_or((first, ""));
    let whole: u32 = whole.parse().unwrap_or(0);
    // Three digits of the fraction, padded: `.4` is 400, `.425` is 425, `.4256` is 425.
    let digits: String = fraction.chars().chain("000".chars()).take(3).collect();
    let fraction: u32 = digits.parse().unwrap_or(0);
    whole.saturating_mul(1000).saturating_add(fraction)
}

/// The cores in a kernel CPU list such as `0-3,6,8-9` (`/sys/devices/system/cpu/online`).
fn cpu_list_count(list: &str) -> u32 {
    list.trim()
        .split(',')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('-') {
            Some((low, high)) => match (low.parse::<u32>(), high.parse::<u32>()) {
                (Ok(low), Ok(high)) if high >= low => high - low + 1,
                _ => 0,
            },
            None => u32::from(part.parse::<u32>().is_ok()),
        })
        .fold(0, u32::saturating_add)
}

/// Keeps a log line from steering the terminal that shows it. Escape sequences and other
/// control characters are dropped.
pub(crate) fn sanitize_line(line: &str) -> String {
    line.chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .take(MAX_LINE_CHARS)
        .collect()
}

/// Counts usable keys. Comments and blank lines do not count.
pub(crate) fn count_keys(text: &str) -> u32 {
    let count = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn info(package: &Package, feed: Option<String>) -> PackageInfo {
    let m = &package.manifest;
    PackageInfo {
        id: m.id.clone(),
        version: m.version.clone(),
        origin: package.origin(feed),
        roles: m.provides.clone(),
        needs: m.needs.clone(),
        encrypted_storage: m.storage.encrypted,
        has_settings: m.settings.is_some(),
        owner_page: m.owner_page.as_ref().map(|p| p.title.clone()),
        bundle: m
            .bundle
            .as_ref()
            .map(|b| b.members.clone())
            .unwrap_or_default(),
    }
}

impl<S: System> Helper<S> {
    pub(crate) fn encryption_state(&self) -> Result<EncryptionState, ApiError> {
        if !self.sys.exists(&self.paths.keystore) {
            return Ok(EncryptionState::NotSetUp);
        }
        // Sealed secrets need the key in memory, which a reboot or a helper restart loses.
        if self.secrets_key().is_none() {
            return Ok(EncryptionState::Locked);
        }
        let (packages, _) = self.packages()?;
        for package in &packages {
            if self.is_locked(package)? {
                return Ok(EncryptionState::Locked);
            }
        }
        Ok(EncryptionState::Unlocked)
    }

    pub(crate) fn overview(&self) -> Result<Response, ApiError> {
        let read = |path: &str| self.sys.read_file(Path::new(path)).ok().flatten();
        let meminfo =
            String::from_utf8_lossy(&read("/proc/meminfo").unwrap_or_default()).into_owned();
        let uptime_secs = first_line(read("/proc/uptime"))
            .split('.')
            .next()
            .and_then(|secs| secs.parse().ok())
            .unwrap_or(0);
        let (disk_total_bytes, disk_free_bytes) =
            self.sys.fs_usage(Path::new("/")).unwrap_or((0, 0));
        let ssh_keys = self
            .sys
            .read_file(&self.paths.authorized_keys)?
            .map(|bytes| count_keys(&String::from_utf8_lossy(&bytes)))
            .unwrap_or(0);
        Ok(Response::Overview(DeviceOverview {
            hostname: first_line(read("/proc/sys/kernel/hostname")),
            kernel: first_line(read("/proc/sys/kernel/osrelease")),
            uptime_secs,
            memory_total_kib: meminfo_kib(&meminfo, "MemTotal"),
            memory_available_kib: meminfo_kib(&meminfo, "MemAvailable"),
            disk_total_bytes,
            disk_free_bytes,
            encryption: self.encryption_state()?,
            ssh_keys,
            drive: self.drive_status(),
        }))
    }

    /// How busy the processor is and how warm it runs. Whatever cannot be read is left out
    /// rather than refused: no load reads as idle, no core list as one core, no zone as no
    /// temperature.
    pub(crate) fn cpu_status(&self) -> Result<Response, ApiError> {
        let read = |path: &str| self.sys.read_file(Path::new(path)).ok().flatten();
        let load_average_milli = load_average_milli(&first_line(read("/proc/loadavg")));
        let cores = cpu_list_count(&first_line(read("/sys/devices/system/cpu/online"))).max(1);
        // Zones are numbered from 0 without gaps; the T6N has a handful. Read by number, as
        // listing `/sys/class/thermal` gives symbolic links that only some readers follow.
        let temperature_millicelsius = (0..MAX_THERMAL_ZONES)
            .filter_map(|zone| {
                first_line(read(&format!("/sys/class/thermal/thermal_zone{zone}/temp")))
                    .parse::<i32>()
                    .ok()
            })
            .max();
        Ok(Response::Cpu(CpuStatus {
            load_average_milli,
            cores,
            temperature_millicelsius,
        }))
    }

    pub(crate) fn list_packages(&self) -> Result<Response, ApiError> {
        let (packages, _) = self.packages()?;
        Ok(Response::Packages(
            packages
                .iter()
                .map(|p| info(p, self.feed_of(p.id())))
                .collect(),
        ))
    }

    pub(crate) fn package_detail(&self, id: &str) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        let m = &package.manifest;
        let inbound = m
            .network
            .inbound
            .iter()
            .map(|rule| {
                let protocol = match rule.protocol {
                    Protocol::Tcp => "tcp",
                    Protocol::Udp => "udp",
                };
                let scope = match rule.scope {
                    Scope::Local => "local",
                    Scope::Lan => "lan",
                    Scope::Internet => "internet",
                };
                format!("{}/{protocol} {scope}", rule.port)
            })
            .collect();
        Ok(Response::PackageDetail(PackageDetail {
            info: info(&package, self.feed_of(id)),
            sites: m.web.iter().map(|web| web.name.clone()).collect(),
            inbound,
            outbound_internet: matches!(m.network.outbound, nodo_manifest::Outbound::Internet),
            public_settings: Vec::new(),
        }))
    }

    fn log_path(&self, source: &str) -> Result<PathBuf, ApiError> {
        if source == HELPER_LOG_SOURCE {
            return Ok(HELPER_LOG.into());
        }
        let package = self.package(source)?;
        Ok(Path::new(&package.layout()?.log_dir).join("current"))
    }

    pub(crate) fn log_sources(&self) -> Result<Response, ApiError> {
        let (packages, _) = self.packages()?;
        let mut sources: Vec<String> = packages.iter().map(|p| p.id().to_owned()).collect();
        sources.push(HELPER_LOG_SOURCE.to_owned());
        Ok(Response::LogSources(sources))
    }

    pub(crate) fn read_log(&self, source: &str, max_lines: u32) -> Result<Response, ApiError> {
        let path = self.log_path(source)?;
        let bytes = self.sys.read_file(&path)?.unwrap_or_default();
        if bytes.len() > MAX_LOG_BYTES {
            return Err(ApiError::Failed(format!(
                "{source}'s log is too large to show"
            )));
        }
        let text = String::from_utf8_lossy(&bytes);
        let all: Vec<&str> = text.lines().collect();
        let wanted = max_lines.min(MAX_LOG_LINES) as usize;
        let start = all.len().saturating_sub(wanted);
        Ok(Response::Log(LogChunk {
            source: source.to_owned(),
            lines: all[start..]
                .iter()
                .map(|line| sanitize_line(line))
                .collect(),
            truncated: start > 0,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, ENCRYPTED, SHOP};

    #[test]
    fn overview_reads_the_machine() {
        let sys = FakeSystem::default();
        sys.put("/proc/sys/kernel/hostname", b"nodo\n");
        sys.put("/proc/sys/kernel/osrelease", b"6.12.1_1\n");
        sys.put("/proc/uptime", b"3600.52 100.00\n");
        sys.put(
            "/proc/meminfo",
            b"MemTotal:  16000 kB\nMemFree: 1 kB\nMemAvailable:  8000 kB\n",
        );
        sys.put(
            "/root/.ssh/authorized_keys",
            b"# c\nssh-ed25519 AAAA a\n\nssh-ed25519 AAAB b\n",
        );
        let h = helper(sys);
        let Ok(Response::Overview(o)) = h.overview() else {
            panic!()
        };
        assert_eq!(o.hostname, "nodo");
        assert_eq!(o.kernel, "6.12.1_1");
        assert_eq!(o.uptime_secs, 3600);
        assert_eq!((o.memory_total_kib, o.memory_available_kib), (16000, 8000));
        assert_eq!((o.disk_total_bytes, o.disk_free_bytes), (1000, 400));
        assert_eq!(o.encryption, EncryptionState::NotSetUp);
        assert_eq!(o.ssh_keys, 2);
    }

    #[test]
    fn cpu_status_reads_load_cores_and_the_hottest_zone() {
        let sys = FakeSystem::default();
        sys.put("/proc/loadavg", b"2.57 1.20 0.90 3/456 7890\n");
        sys.put("/sys/devices/system/cpu/online", b"0-3,6,8-9\n");
        sys.put("/sys/class/thermal/thermal_zone0/temp", b"48125\n");
        sys.put("/sys/class/thermal/thermal_zone1/temp", b"52500\n");
        sys.put("/sys/class/thermal/thermal_zone2/temp", b"not a number\n");
        sys.put("/sys/class/thermal/cooling_device0/cur_state", b"1\n");
        let h = helper(sys);
        let Ok(Response::Cpu(cpu)) = h.cpu_status() else {
            panic!()
        };
        assert_eq!(
            cpu,
            CpuStatus {
                load_average_milli: 2570,
                cores: 7,
                temperature_millicelsius: Some(52500)
            }
        );
    }

    #[test]
    fn cpu_status_without_the_files_is_idle_one_core_and_no_temperature() {
        let h = helper(FakeSystem::default());
        let Ok(Response::Cpu(cpu)) = h.cpu_status() else {
            panic!()
        };
        assert_eq!(
            cpu,
            CpuStatus {
                load_average_milli: 0,
                cores: 1,
                temperature_millicelsius: None
            }
        );
    }

    #[test]
    fn load_and_core_lists_parse_as_the_kernel_writes_them() {
        assert_eq!(load_average_milli("0.4 0 0"), 400);
        assert_eq!(load_average_milli("12.3456 0 0"), 12345);
        assert_eq!(load_average_milli("3"), 3000);
        assert_eq!(load_average_milli(""), 0);
        assert_eq!(cpu_list_count("0"), 1);
        assert_eq!(cpu_list_count("0-7"), 8);
        assert_eq!(cpu_list_count("0,2-3"), 3);
        assert_eq!(cpu_list_count("x,3-1"), 0);
    }

    #[test]
    fn encryption_state_follows_keystore_and_mounts() {
        let sys = FakeSystem::default();
        sys.add_manifest(ENCRYPTED);
        let h = helper(sys);
        assert_eq!(h.encryption_state(), Ok(EncryptionState::NotSetUp));
        h.sys.put("/k/device.key", b"x");
        assert_eq!(h.encryption_state(), Ok(EncryptionState::Locked));
        h.sys.set_mounted("/run/nodo/volumes/vault");
        assert_eq!(
            h.encryption_state(),
            Ok(EncryptionState::Locked),
            "the secrets key is not in memory"
        );
        h.remember_device_key(&[1; 32]);
        assert_eq!(h.encryption_state(), Ok(EncryptionState::Unlocked));
    }

    #[test]
    fn packages_and_detail_come_from_manifests() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let h = helper(sys);
        let Ok(Response::Packages(list)) = h.list_packages() else {
            panic!()
        };
        assert_eq!(list.len(), 1);
        assert!(list[0].has_settings);
        let Ok(Response::PackageDetail(detail)) = h.package_detail("shop") else {
            panic!()
        };
        assert_eq!(detail.sites, ["web", "single"]);
        assert!(matches!(
            h.package_detail("../x"),
            Err(ApiError::NotFound(_))
        ));
    }

    #[test]
    fn logs_are_tailed_and_cannot_carry_escape_sequences() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.put(
            "/var/log/apps/shop/current",
            b"one\ntwo\x1b[2J\nthree\x07\n",
        );
        let h = helper(sys);
        let Ok(Response::Log(chunk)) = h.read_log("shop", 2) else {
            panic!()
        };
        assert_eq!(chunk.lines, ["two[2J", "three"]);
        assert!(chunk.truncated);
        let Ok(Response::Log(all)) = h.read_log("shop", 100) else {
            panic!()
        };
        assert_eq!(all.lines.len(), 3);
        assert!(!all.truncated);
    }

    #[test]
    fn log_sources_are_packages_and_the_helper_only() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let h = helper(sys);
        let Ok(Response::LogSources(sources)) = h.log_sources() else {
            panic!()
        };
        assert_eq!(sources, ["shop", "nodo-helper"]);
        for bad in ["../../etc/shadow", "sshd", "/etc/passwd"] {
            assert!(h.read_log(bad, 10).is_err(), "{bad}");
        }
    }
}
