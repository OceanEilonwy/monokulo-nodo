//! One pass of the health daemon.
//!
//! Disk protection works in three layers. A service is stopped through runit, which runs its
//! `control/t` so it shuts down cleanly while there is still room to finish its writes. A `down`
//! file keeps runit from starting it again after a reboot while the disk is still full. And the
//! next tick stops it again if anything starts it anyway. Only services this daemon stopped are
//! started again, so a service the user stopped stays stopped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::disk::{evaluate, Level};
use crate::report::{self, DiskReport, LogReport, Report, StoppedService, REPORT_VERSION};
use crate::sys::{SvAction, System};
use crate::{logs, memory, time};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    pub filesystem: String,
    pub since: u64,
}

/// Persisted, so a restarted daemon still knows which services it stopped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub levels: BTreeMap<String, Level>,
    #[serde(default)]
    pub stopped: BTreeMap<String, Stop>,
}

impl State {
    pub fn load(sys: &dyn System, path: &Path) -> State {
        sys.read(path).ok().flatten().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }
}

/// `run:` means up. `want up` covers a service between crash-loop restarts. A service the user
/// stopped shows neither.
fn wanted_up(status: &str) -> bool {
    (status.starts_with("run:") && !status.contains("want down")) || status.contains("want up")
}

fn down_file(cfg: &Config, service: &str) -> PathBuf {
    cfg.sv_dir.join(service).join("down")
}

pub fn tick(cfg: &Config, state: &mut State, sys: &dyn System) -> Report {
    let now = sys.now();
    let before = state.clone();
    let mut disks = Vec::new();
    let mut critical_services: BTreeMap<&str, &str> = BTreeMap::new();
    let mut unmeasured: Vec<&str> = Vec::new();
    let mut system_level = Level::Ok;

    for fs in &cfg.filesystems {
        let mounted = !fs.require_mount || sys.is_mountpoint(&fs.path);
        let space = if mounted { sys.space(&fs.path) } else { None };
        let Some(space) = space else {
            unmeasured.extend(fs.services.iter().map(String::as_str));
            disks.push(DiskReport {
                name: fs.name.clone(),
                path: fs.path.display().to_string(),
                mounted: false,
                total_bytes: 0,
                free_bytes: 0,
                level: Level::Ok,
            });
            continue;
        };
        let previous = state.levels.get(&fs.name).copied().unwrap_or_default();
        let level = evaluate(fs, space, previous);
        state.levels.insert(fs.name.clone(), level);
        if fs.system {
            system_level = system_level.max(level);
        }
        if level == Level::Critical {
            for svc in &fs.services {
                critical_services.entry(svc).or_insert(&fs.name);
            }
        }
        disks.push(DiskReport {
            name: fs.name.clone(),
            path: fs.path.display().to_string(),
            mounted: true,
            total_bytes: space.total,
            free_bytes: space.available,
            level,
        });
    }

    for (svc, fs_name) in &critical_services {
        let status = sys.sv_status(svc).unwrap_or_default();
        let ours = state.stopped.contains_key(*svc);
        if !wanted_up(&status) && !ours {
            continue;
        }
        let _ = sys.write(&down_file(cfg, svc), "");
        if wanted_up(&status) {
            let _ = sys.sv(SvAction::Down, svc);
        }
        state
            .stopped
            .entry((*svc).to_owned())
            .or_insert(Stop { filesystem: (*fs_name).to_owned(), since: now });
    }

    let resumable: Vec<String> = state
        .stopped
        .keys()
        .filter(|svc| !critical_services.contains_key(svc.as_str()) && !unmeasured.contains(&svc.as_str()))
        .cloned()
        .collect();
    for svc in resumable {
        if sys.remove(&down_file(cfg, &svc)).is_ok() && sys.sv(SvAction::Up, &svc).is_ok() {
            state.stopped.remove(&svc);
        }
    }

    let log_summary = logs::enforce(&cfg.logs, system_level, sys);
    let services_memory = memory::apply(&cfg.memory, sys);
    let time = time::status(sys);
    let stopped: Vec<StoppedService> = state
        .stopped
        .iter()
        .map(|(service, s)| StoppedService { service: service.clone(), filesystem: s.filesystem.clone(), since: s.since })
        .collect();
    let abstract_sockets = report::abstract_sockets(&cfg.abstract_sockets, sys);
    let attention = report::attention(&disks, &stopped, &services_memory, &time, &abstract_sockets);

    if *state != before {
        if let Ok(text) = toml::to_string(state) {
            if let Some(dir) = cfg.state_path.parent() {
                let _ = sys.mkdir(dir);
            }
            let _ = sys.write(&cfg.state_path, &text);
        }
    }

    let report = Report {
        version: REPORT_VERSION,
        generated_at: now,
        disks,
        stopped,
        refuse_installs: system_level != Level::Ok,
        memory: report::meminfo(sys),
        services_memory,
        logs: LogReport { bytes: log_summary.bytes, dirs: log_summary.dirs, pruned_files: log_summary.pruned.len() },
        time,
        temperature_c: report::temperature(sys),
        uptime_secs: report::uptime(sys),
        abstract_sockets,
        attention,
    };
    if let Some(dir) = cfg.report_path.parent() {
        let _ = sys.mkdir(dir);
    }
    let _ = sys.write(&cfg.report_path, &report.to_json());
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Threshold;
    use crate::report::Severity;
    use crate::sys::fake::FakeSystem;

    const MIB: u64 = 1 << 20;

    fn cfg() -> Config {
        let mut cfg = Config::default();
        cfg.filesystems[0].warn = Threshold { bytes: 200 * MIB, percent: 0.0 };
        cfg.filesystems[0].stop = Threshold { bytes: 100 * MIB, percent: 0.0 };
        cfg.filesystems[0].resume_margin = Threshold { bytes: 50 * MIB, percent: 0.0 };
        cfg.filesystems[0].services = vec!["bell".into()];
        cfg.filesystems[1].warn = Threshold { bytes: 200 * MIB, percent: 0.0 };
        cfg.filesystems[1].stop = Threshold { bytes: 100 * MIB, percent: 0.0 };
        cfg.filesystems[1].resume_margin = Threshold { bytes: 50 * MIB, percent: 0.0 };
        cfg
    }

    fn sys() -> FakeSystem {
        let sys = FakeSystem::default();
        sys.set_space("/", 8000 * MIB, 4000 * MIB);
        sys.set_space("/media/monero", 1000 * MIB, 500 * MIB);
        sys.status.borrow_mut().insert("cuprated".into(), "run: cuprated: (pid 9) 60s".into());
        sys.status.borrow_mut().insert("bell".into(), "run: bell: (pid 10) 60s".into());
        sys.outputs.borrow_mut().insert("chronyc".into(), "Leap status     : Normal\nStratum         : 2\n".into());
        sys
    }

    fn data_free(sys: &FakeSystem, mib: u64) {
        sys.set_space("/media/monero", 1000 * MIB, mib * MIB);
    }

    #[test]
    fn healthy_disks_touch_no_service() {
        let sys = sys();
        let mut state = State::default();
        let r = tick(&cfg(), &mut state, &sys);
        assert!(r.stopped.is_empty() && !r.refuse_installs);
        assert!(!sys.calls().iter().any(|c| c.starts_with("sv Down") || c.starts_with("sv Up")));
        assert!(sys.get("/run/nodo/health.json").unwrap().contains("\"version\": 1"));
    }

    #[test]
    fn stops_the_node_before_the_data_disk_fills_and_restarts_it_after() {
        let sys = sys();
        let cfg = cfg();
        let mut state = State::default();
        data_free(&sys, 150);
        let r = tick(&cfg, &mut state, &sys);
        assert_eq!(r.disks[1].level, Level::Low);
        assert!(r.stopped.is_empty(), "low only warns");

        data_free(&sys, 99);
        let r = tick(&cfg, &mut state, &sys);
        assert!(sys.calls().contains(&"sv Down cuprated".to_owned()));
        assert!(sys.exists("/etc/sv/cuprated/down"), "survives a reboot");
        assert!(!sys.calls().contains(&"sv Down bell".to_owned()), "services on other disks keep running");
        assert_eq!(r.stopped[0].service, "cuprated");
        assert_eq!(r.attention[0].severity, Severity::Critical);
        assert!(r.attention[0].message.contains("cuprated stopped"));

        // Freeing a little is not enough: the margin prevents flapping.
        data_free(&sys, 120);
        let r = tick(&cfg, &mut state, &sys);
        assert_eq!(r.stopped.len(), 1);
        assert!(!sys.calls().contains(&"sv Up cuprated".to_owned()));

        data_free(&sys, 150);
        let r = tick(&cfg, &mut state, &sys);
        assert!(r.stopped.is_empty());
        assert!(sys.calls().contains(&"sv Up cuprated".to_owned()));
        assert!(!sys.exists("/etc/sv/cuprated/down"));
    }

    #[test]
    fn stops_again_if_something_restarts_it_while_full() {
        let sys = sys();
        let cfg = cfg();
        let mut state = State::default();
        data_free(&sys, 10);
        tick(&cfg, &mut state, &sys);
        sys.status.borrow_mut().insert("cuprated".into(), "run: cuprated: (pid 11) 1s".into());
        sys.calls.borrow_mut().clear();
        tick(&cfg, &mut state, &sys);
        assert!(sys.calls().contains(&"sv Down cuprated".to_owned()));
    }

    #[test]
    fn stops_a_crash_looping_service_too() {
        let sys = sys();
        sys.status.borrow_mut().insert("cuprated".into(), "down: cuprated: 0s, normally up, want up".into());
        data_free(&sys, 10);
        let r = tick(&cfg(), &mut State::default(), &sys);
        assert_eq!(r.stopped.len(), 1);
        assert!(sys.exists("/etc/sv/cuprated/down"));
    }

    #[test]
    fn a_service_the_user_stopped_is_left_alone() {
        let sys = sys();
        let cfg = cfg();
        let mut state = State::default();
        sys.status.borrow_mut().insert("cuprated".into(), "down: cuprated: 50s, normally up".into());
        data_free(&sys, 10);
        let r = tick(&cfg, &mut state, &sys);
        assert!(r.stopped.is_empty());
        data_free(&sys, 900);
        tick(&cfg, &mut state, &sys);
        assert!(!sys.calls().iter().any(|c| c.contains("cuprated")), "{:?}", sys.calls());
    }

    #[test]
    fn a_restarted_daemon_remembers_what_it_stopped() {
        let sys = sys();
        let cfg = cfg();
        let mut state = State::default();
        data_free(&sys, 10);
        tick(&cfg, &mut state, &sys);
        let mut reloaded = State::load(&sys, &cfg.state_path);
        assert_eq!(reloaded.stopped.len(), 1);
        data_free(&sys, 900);
        tick(&cfg, &mut reloaded, &sys);
        assert!(sys.calls().contains(&"sv Up cuprated".to_owned()));
    }

    #[test]
    fn an_unmounted_data_drive_is_reported_and_keeps_its_services_stopped() {
        let sys = sys();
        let cfg = cfg();
        let mut state = State::default();
        data_free(&sys, 10);
        tick(&cfg, &mut state, &sys);
        sys.mounts.borrow_mut().remove(Path::new("/media/monero"));
        let r = tick(&cfg, &mut state, &sys);
        assert!(!r.disks[1].mounted);
        assert_eq!(r.stopped.len(), 1, "cannot tell whether there is room, so it stays stopped");
        assert!(r.attention.iter().any(|a| a.message.contains("not mounted")));
    }

    #[test]
    fn a_low_system_disk_refuses_installs_and_a_full_one_stops_its_services() {
        let sys = sys();
        let cfg = cfg();
        let mut state = State::default();
        sys.set_space("/", 8000 * MIB, 150 * MIB);
        let r = tick(&cfg, &mut state, &sys);
        assert!(r.refuse_installs);
        assert!(r.stopped.is_empty());
        sys.set_space("/", 8000 * MIB, 50 * MIB);
        let r = tick(&cfg, &mut state, &sys);
        assert_eq!(r.stopped.iter().map(|s| s.service.as_str()).collect::<Vec<_>>(), ["bell"]);
        assert!(!sys.calls().contains(&"sv Down cuprated".to_owned()), "chain data is on its own drive");
    }

    #[test]
    fn a_service_on_two_full_disks_waits_for_both() {
        let sys = sys();
        let mut cfg = cfg();
        cfg.filesystems[0].services.push("cuprated".into());
        let mut state = State::default();
        sys.set_space("/", 8000 * MIB, 50 * MIB);
        data_free(&sys, 10);
        tick(&cfg, &mut state, &sys);
        data_free(&sys, 900);
        let r = tick(&cfg, &mut state, &sys);
        assert!(r.stopped.iter().any(|s| s.service == "cuprated"));
        sys.set_space("/", 8000 * MIB, 4000 * MIB);
        let r = tick(&cfg, &mut state, &sys);
        assert!(r.stopped.is_empty());
    }

    #[test]
    fn wanted_up_reads_runit_status() {
        assert!(wanted_up("run: x: (pid 1) 5s; run: log: (pid 2) 5s"));
        assert!(wanted_up("down: x: 0s, normally up, want up"));
        assert!(!wanted_up("down: x: 9s, normally up"));
        assert!(!wanted_up("run: x: (pid 1) 5s, want down"));
        assert!(!wanted_up(""));
    }

    const UNIX_HEADER: &str = "Num       RefCount Protocol Flags    Type St Inode Path\n";

    fn listen(sys: &FakeSystem, entries: &[(u64, &str)]) {
        let mut text = UNIX_HEADER.to_owned();
        text.push_str("0000000000000000: 00000002 00000000 00010000 0001 01 7 /run/nodo/helper.sock\n");
        for (inode, name) in entries {
            text.push_str(&format!("0000000000000000: 00000002 00000000 00010000 0001 01 {inode} {name}\n"));
        }
        sys.put("/proc/net/unix", &text);
    }

    fn owner(pid: u32, process: &str, user: &str) -> crate::sockets::Owner {
        crate::sockets::Owner { pid, process: process.into(), uid: 1000, user: user.into() }
    }

    fn socket_notes(r: &Report) -> Vec<&str> {
        r.attention.iter().filter(|a| a.message.contains("abstract Unix socket")).map(|a| a.message.as_str()).collect()
    }

    #[test]
    fn an_unexpected_abstract_listener_is_named_on_home_and_cleared_when_it_stops() {
        let sys = sys();
        let cfg = cfg();
        let mut state = State::default();
        sys.put("/proc/sys/kernel/osrelease", "6.1.99-rk3588\n");
        sys.landlock.set(Some(4));
        listen(&sys, &[]);
        let r = tick(&cfg, &mut state, &sys);
        assert!(r.abstract_sockets.scanned && r.abstract_sockets.listeners.is_empty());
        assert!(socket_notes(&r).is_empty());

        listen(&sys, &[(4242, "@nodo-e2e-probe")]);
        sys.owners.borrow_mut().insert(4242, vec![owner(321, "python3", "nobody")]);
        let r = tick(&cfg, &mut state, &sys);
        let notes = socket_notes(&r);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("@nodo-e2e-probe") && notes[0].contains("python3 (pid 321) as user nobody"),
            "{}",
            notes[0]
        );
        assert!(notes[0].contains("This kernel (6.1.99-rk3588) cannot stop"), "{}", notes[0]);
        assert_eq!(r.attention.iter().find(|a| a.message == notes[0]).unwrap().severity, Severity::Warning);
        assert!(sys.get("/run/nodo/health.json").unwrap().contains("\"name\": \"@nodo-e2e-probe\""));

        sys.landlock.set(Some(6));
        let r = tick(&cfg, &mut state, &sys);
        assert!(socket_notes(&r)[0].ends_with("This kernel's sandbox keeps services away from it."));

        listen(&sys, &[]);
        let r = tick(&cfg, &mut state, &sys);
        assert!(socket_notes(&r).is_empty());
    }

    #[test]
    fn expected_listeners_stay_quiet_unless_another_user_holds_the_name() {
        use crate::config::ExpectedSocket;
        let sys = sys();
        let mut cfg = cfg();
        cfg.abstract_sockets.expected = vec![
            ExpectedSocket { name: "@any-user".into(), user: None },
            ExpectedSocket { name: "@root-only/*".into(), user: Some("root".into()) },
        ];
        listen(&sys, &[(1, "@any-user"), (2, "@root-only/a"), (3, "@root-only/b"), (4, "@root-only/c")]);
        sys.owners.borrow_mut().insert(1, vec![owner(10, "daemon", "_shop")]);
        sys.owners.borrow_mut().insert(2, vec![owner(11, "daemon", "root")]);
        sys.owners.borrow_mut().insert(3, vec![owner(12, "daemon", "root"), owner(13, "squatter", "_shop")]);
        let r = tick(&cfg, &mut State::default(), &sys);
        let expected: Vec<bool> = r.abstract_sockets.listeners.iter().map(|l| l.expected).collect();
        assert_eq!(expected, [true, true, false, false], "a name bound to root but held by another user, or by no one known");
        let notes = socket_notes(&r);
        assert_eq!(notes.len(), 2);
        assert!(notes[0].contains("@root-only/b") && notes[0].contains("daemon (pid 12) as user root and 1 more process"));
        assert!(notes[1].contains("@root-only/c") && notes[1].contains("a process that could not be found"));
    }

    #[test]
    fn many_unexpected_listeners_are_counted_after_the_first_few() {
        let sys = sys();
        listen(&sys, &[(1, "@a"), (2, "@b"), (3, "@c"), (4, "@d"), (5, "@e")]);
        let r = tick(&cfg(), &mut State::default(), &sys);
        let notes = socket_notes(&r);
        assert_eq!(notes.len(), 4);
        assert!(notes[3].starts_with("2 more unexpected abstract Unix sockets"));
        assert!(notes.iter().all(|n| n.len() < 300), "the helper keeps 300 characters");
    }

    #[test]
    fn an_unreadable_socket_list_raises_nothing() {
        let r = tick(&cfg(), &mut State::default(), &sys());
        assert!(!r.abstract_sockets.scanned);
        assert!(socket_notes(&r).is_empty());
    }
}
