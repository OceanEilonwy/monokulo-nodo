//! Per-service memory limits with cgroup v2.
//!
//! runit has no cgroup support, so each tick moves every process of a service's user into
//! `<cgroup_root>/nodo/<service>`. Children inherit the cgroup, so after the first move only new
//! service starts need it. A service that runs away is then killed by the kernel inside its own
//! cgroup, instead of the OOM killer choosing the node or the helper.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::MemoryConfig;
use crate::sys::System;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoryStatus {
    pub service: String,
    pub limit_bytes: u64,
    pub current_bytes: Option<u64>,
    pub oom_kills: u64,
    pub moved: usize,
    pub error: Option<String>,
}

fn group(cfg: &MemoryConfig, service: &str) -> PathBuf {
    cfg.cgroup_root.join("nodo").join(service)
}

fn read_u64(sys: &dyn System, path: &Path) -> Option<u64> {
    sys.read(path).ok().flatten()?.trim().parse().ok()
}

fn oom_kills(sys: &dyn System, dir: &Path) -> u64 {
    let Some(text) = sys.read(&dir.join("memory.events")).ok().flatten() else { return 0 };
    text.lines()
        .find_map(|l| l.strip_prefix("oom_kill ").and_then(|n| n.trim().parse().ok()))
        .unwrap_or(0)
}

fn ensure_controller(sys: &dyn System, dir: &Path) -> Result<(), String> {
    let control = dir.join("cgroup.subtree_control");
    let enabled = sys.read(&control).ok().flatten().unwrap_or_default();
    if enabled.split_whitespace().any(|c| c == "memory") {
        return Ok(());
    }
    sys.write_in_place(&control, "+memory").map_err(|e| format!("enabling memory in {}: {e}", control.display()))
}

pub fn apply(cfg: &MemoryConfig, sys: &dyn System) -> Vec<MemoryStatus> {
    if cfg.limits.is_empty() {
        return Vec::new();
    }
    let parent = cfg.cgroup_root.join("nodo");
    let setup = ensure_controller(sys, &cfg.cgroup_root)
        .and_then(|()| sys.mkdir(&parent).map_err(|e| e.to_string()))
        .and_then(|()| ensure_controller(sys, &parent));
    cfg.limits
        .iter()
        .map(|(service, limit)| {
            let dir = group(cfg, service);
            let mut status = MemoryStatus {
                service: service.clone(),
                limit_bytes: limit.max_bytes,
                current_bytes: None,
                oom_kills: 0,
                moved: 0,
                error: setup.as_ref().err().cloned(),
            };
            if status.error.is_some() {
                return status;
            }
            let result = (|| -> Result<usize, String> {
                sys.mkdir(&dir).map_err(|e| e.to_string())?;
                let max = limit.max_bytes.to_string();
                if read_u64(sys, &dir.join("memory.max")) != Some(limit.max_bytes) {
                    sys.write_in_place(&dir.join("memory.max"), &max).map_err(|e| e.to_string())?;
                }
                // Reclaim pressure starts before the hard limit, which slows a leak before it kills.
                let high = (limit.max_bytes / 10 * 9).to_string();
                if sys.read(&dir.join("memory.high")).ok().flatten().map(|s| s.trim().to_owned()) != Some(high.clone()) {
                    sys.write_in_place(&dir.join("memory.high"), &high).map_err(|e| e.to_string())?;
                }
                let uid = sys.uid_of(&limit.user).ok_or_else(|| format!("no user {}", limit.user))?;
                if uid == 0 {
                    return Err("refusing to confine root".into());
                }
                let current = sys.read(&dir.join("cgroup.procs")).ok().flatten().unwrap_or_default();
                let inside: Vec<&str> = current.split_whitespace().collect();
                let mut moved = 0;
                for pid in sys.pids_of_uid(uid) {
                    if !inside.contains(&pid.to_string().as_str()) && sys.write_in_place(&dir.join("cgroup.procs"), &pid.to_string()).is_ok() {
                        moved += 1;
                    }
                }
                Ok(moved)
            })();
            match result {
                Ok(moved) => status.moved = moved,
                Err(e) => status.error = Some(e),
            }
            status.current_bytes = read_u64(sys, &dir.join("memory.current"));
            status.oom_kills = oom_kills(sys, &dir);
            status
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::MemoryLimit;
    use crate::sys::fake::FakeSystem;

    fn cfg() -> MemoryConfig {
        let mut limits = BTreeMap::new();
        limits.insert("bell".into(), MemoryLimit { user: "_bell".into(), max_bytes: 100 << 20 });
        MemoryConfig { cgroup_root: "/cg".into(), limits }
    }

    fn sys() -> FakeSystem {
        let mut sys = FakeSystem::default();
        sys.users.insert("_bell".into(), 990);
        sys.users.insert("root".into(), 0);
        sys.pids.insert(990, vec![41, 42]);
        sys
    }

    #[test]
    fn creates_the_group_sets_limits_and_moves_processes() {
        let sys = sys();
        let st = apply(&cfg(), &sys);
        assert_eq!(st[0].error, None);
        assert_eq!(st[0].moved, 2);
        assert_eq!(sys.get("/cg/cgroup.subtree_control").unwrap(), "+memory");
        assert_eq!(sys.get("/cg/nodo/bell/memory.max").unwrap(), "104857600");
        assert_eq!(sys.get("/cg/nodo/bell/memory.high").unwrap(), "94371840");
    }

    #[test]
    fn reports_oom_kills() {
        let sys = sys();
        sys.put("/cg/nodo/bell/memory.events", "low 0\nhigh 4\nmax 2\noom 1\noom_kill 1\n");
        assert_eq!(apply(&cfg(), &sys)[0].oom_kills, 1);
    }

    #[test]
    fn never_confines_root() {
        let sys = sys();
        let mut c = cfg();
        c.limits.get_mut("bell").unwrap().user = "root".into();
        assert!(apply(&c, &sys)[0].error.as_deref().unwrap().contains("root"));
        assert!(!sys.exists("/cg/nodo/bell/cgroup.procs"));
    }

    #[test]
    fn nothing_configured_touches_nothing() {
        let sys = sys();
        let c = MemoryConfig { cgroup_root: "/cg".into(), limits: BTreeMap::new() };
        assert!(apply(&c, &sys).is_empty());
        assert!(sys.calls().is_empty());
    }
}
