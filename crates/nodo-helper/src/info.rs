//! Read-only views of the device: overview, packages and logs.

use std::path::{Path, PathBuf};

use nodo_api::{
    ApiError, DeviceOverview, EncryptionState, LogChunk, PackageDetail, PackageInfo, Response,
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
