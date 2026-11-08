//! Bounds service logs on the system disk.
//!
//! Every service logs through svlogd, which rotates by its directory's `config` file. The service
//! user owns that directory and could rewrite `config`, so it is checked and restored every tick.
//! When the system disk runs low, rotated files are pruned, oldest first; `current` is never
//! touched, so the latest messages survive.

use std::path::PathBuf;

use crate::config::LogConfig;
use crate::disk::Level;
use crate::sys::{SvAction, System};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogSummary {
    pub bytes: u64,
    pub dirs: usize,
    pub pruned: Vec<PathBuf>,
    pub reconfigured: Vec<String>,
}

/// A log directory and the runit service whose `log/` writes to it.
struct LogDir {
    path: PathBuf,
    service: String,
}

fn svlogd_dirs(cfg: &LogConfig, sys: &dyn System) -> Vec<LogDir> {
    let mut out = Vec::new();
    for root in &cfg.roots {
        let Ok(entries) = sys.list(root) else { continue };
        for entry in entries.into_iter().filter(|e| e.is_dir) {
            let path = root.join(&entry.name);
            let Ok(files) = sys.list(&path) else { continue };
            if files.iter().any(|f| f.name == "current" && f.is_file) {
                out.push(LogDir { path, service: entry.name });
            }
        }
    }
    out
}

/// svlogd names rotated files `@<tai64n>.s` or `.u`, so names sort by age.
fn is_rotated(name: &str) -> bool {
    name.starts_with('@') && (name.ends_with(".s") || name.ends_with(".u"))
}

pub fn wanted_config(cfg: &LogConfig) -> String {
    format!("s{}\nn{}\n", cfg.file_bytes, cfg.files)
}

pub fn enforce(cfg: &LogConfig, system_level: Level, sys: &dyn System) -> LogSummary {
    let wanted = wanted_config(cfg);
    let mut summary = LogSummary::default();
    for dir in svlogd_dirs(cfg, sys) {
        summary.dirs += 1;
        let config = dir.path.join("config");
        if sys.read(&config).ok().flatten().as_deref() != Some(wanted.as_str())
            && sys.write(&config, &wanted).is_ok()
        {
            let _ = sys.sv(SvAction::HupLog, &dir.service);
            summary.reconfigured.push(dir.service.clone());
        }
        let Ok(mut files) = sys.list(&dir.path) else { continue };
        files.retain(|f| f.is_file);
        files.sort_by(|a, b| a.name.cmp(&b.name));
        let rotated: Vec<_> = files.iter().filter(|f| is_rotated(&f.name)).collect();
        let keep = match system_level {
            Level::Ok => rotated.len(),
            Level::Low => 1,
            Level::Critical => 0,
        };
        let prune = rotated.len().saturating_sub(keep);
        for file in &rotated[..prune] {
            let path = dir.path.join(&file.name);
            if sys.remove(&path).is_ok() {
                summary.pruned.push(path);
            }
        }
        summary.bytes += files
            .iter()
            .filter(|f| !summary.pruned.contains(&dir.path.join(&f.name)))
            .map(|f| f.size)
            .sum::<u64>();
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::fake::FakeSystem;

    fn cfg() -> LogConfig {
        LogConfig { roots: vec!["/var/log".into(), "/var/log/apps".into()], file_bytes: 1 << 20, files: 5 }
    }

    fn setup() -> FakeSystem {
        let sys = FakeSystem::default();
        sys.put_sized("/var/log/cuprated/current", 10);
        sys.put_sized("/var/log/cuprated/@400000006000000000000000.s", 100);
        sys.put_sized("/var/log/cuprated/@400000006100000000000000.s", 200);
        sys.put_sized("/var/log/cuprated/@400000006200000000000000.u", 300);
        sys.put_sized("/var/log/apps/bell/current", 5);
        sys.put("/var/log/wtmp", "");
        sys
    }

    #[test]
    fn writes_config_and_signals_svlogd_once() {
        let sys = setup();
        let s = enforce(&cfg(), Level::Ok, &sys);
        assert_eq!(s.dirs, 2, "only svlogd directories, not plain files or the apps root");
        assert_eq!(sys.get("/var/log/cuprated/config").unwrap(), "s1048576\nn5\n");
        assert!(sys.calls().contains(&"sv HupLog cuprated".to_owned()));
        assert!(sys.calls().contains(&"sv HupLog bell".to_owned()));
        assert_eq!(s.bytes, 615);
        assert!(s.pruned.is_empty());

        let before = sys.calls().len();
        let s = enforce(&cfg(), Level::Ok, &sys);
        assert!(s.reconfigured.is_empty());
        assert_eq!(sys.calls().len(), before, "nothing rewritten when already right");
    }

    #[test]
    fn restores_config_a_service_rewrote() {
        let sys = setup();
        enforce(&cfg(), Level::Ok, &sys);
        sys.put("/var/log/apps/bell/config", "s999999999\nn999\n");
        let s = enforce(&cfg(), Level::Ok, &sys);
        assert_eq!(s.reconfigured, ["bell"]);
        assert_eq!(sys.get("/var/log/apps/bell/config").unwrap(), "s1048576\nn5\n");
    }

    #[test]
    fn prunes_oldest_rotated_files_when_the_system_disk_is_low() {
        let sys = setup();
        let s = enforce(&cfg(), Level::Low, &sys);
        assert_eq!(s.pruned.len(), 2);
        assert!(!sys.exists("/var/log/cuprated/@400000006000000000000000.s"));
        assert!(sys.exists("/var/log/cuprated/@400000006200000000000000.u"), "newest rotated file kept");
        let s = enforce(&cfg(), Level::Critical, &sys);
        assert_eq!(s.pruned.len(), 1);
        assert!(sys.exists("/var/log/cuprated/current"), "current is never pruned");
    }
}
