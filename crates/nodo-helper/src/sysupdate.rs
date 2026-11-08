//! System snapshot updates through the helper.
//!
//! `nodo-sysupdate` (`nodo_update::client`) does the work: it checks the channel's signed
//! manifest, verifies the repository indexes and moves the system with xbps. The helper runs it
//! as a child process, so there is one implementation of the trust rules, and keeps what the
//! screens and the schedule need in `/var/lib/nodo/updates/system.toml`: whether automatic system
//! updates are on, the last check and what it found, the last error and the last update.
//!
//! Automatic system updates are off by default. The `nodo-systemupdate` runit service asks for
//! one scheduled run every ten minutes (`RunSystemUpdate`, root only); a run checks when a check
//! is due and only reports what it found, unless the owner turned automatic system updates on.
//! Then a snapshot that needs no restart is applied inside the package update window while the
//! disk has room. A snapshot that needs a restart always waits for the owner.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use nodo_api::{ApiError, Response, SystemSnapshot, SystemUpdateResult, SystemUpdateStatus};
use nodo_update::auto::AutoPolicy;
use nodo_update::client::{ApplyReport, CheckReport, Report};
use serde::{Deserialize, Serialize};

use crate::helper::{lock, Helper};
use crate::manage::failed;
use crate::system::System;

/// How often the schedule checks for a new snapshot.
pub(crate) const CHECK_INTERVAL_SECS: u64 = 6 * 3600;
/// How soon a failed check is tried again.
pub(crate) const RETRY_SECS: u64 = 3600;
/// The helper is never restarted under its own request; it keeps running until its next restart.
const HELPER_SERVICE: &str = "nodo-helper";
const MAX_OUTPUT: usize = 64 * 1024;

/// Runs the system update client.
pub trait SnapshotClient: Send + Sync {
    /// Why system updates cannot run on this device, if they cannot.
    fn unavailable(&self) -> Option<String>;
    fn check(&self) -> Result<CheckReport, String>;
    /// Applies exactly `serial`, leaving `keep_running` running if their packages change.
    fn apply(&self, serial: u64, keep_running: &[&str]) -> Result<ApplyReport, String>;
}

/// `nodo-sysupdate` with its default configuration.
pub struct RealSnapshots {
    pub program: PathBuf,
    pub config: PathBuf,
    pub pins: PathBuf,
}

impl Default for RealSnapshots {
    fn default() -> Self {
        RealSnapshots {
            program: "/usr/bin/nodo-sysupdate".into(),
            config: "/etc/nodo/system-update.toml".into(),
            pins: "/usr/share/nodo/snapshot-pins.toml".into(),
        }
    }
}

/// The report on the last line of the client's output, or why there is none.
pub(crate) fn parse_report(stdout: &[u8], stderr: &[u8]) -> Result<Report, String> {
    let text = String::from_utf8_lossy(stdout);
    let line = text.lines().rev().find(|l| !l.trim().is_empty());
    match line.map(serde_json::from_str::<Report>) {
        Some(Ok(Report::Error(e))) => Err(e),
        Some(Ok(report)) => Ok(report),
        _ => {
            let detail: String = String::from_utf8_lossy(stderr)
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .collect::<String>()
                .trim()
                .chars()
                .take(400)
                .collect();
            Err(if detail.is_empty() { "the update client gave no answer".to_owned() } else { detail })
        }
    }
}

impl RealSnapshots {
    fn run(&self, args: &[&str]) -> Result<Report, String> {
        let out = Command::new(&self.program)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/usr/sbin:/bin:/sbin")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{}: {e}", self.program.display()))?;
        let stdout = &out.stdout[..out.stdout.len().min(MAX_OUTPUT)];
        parse_report(stdout, &out.stderr)
    }
}

impl SnapshotClient for RealSnapshots {
    fn unavailable(&self) -> Option<String> {
        if !self.program.exists() {
            return Some("the system update client is not installed".to_owned());
        }
        if !self.pins.exists() {
            return Some("this image has no system update signing keys".to_owned());
        }
        if !self.config.exists() {
            return Some(format!("no update server is set ({})", self.config.display()));
        }
        None
    }

    fn check(&self) -> Result<CheckReport, String> {
        match self.run(&["check", "--json"])? {
            Report::Check(report) => Ok(report),
            _ => Err("the update client answered something else".to_owned()),
        }
    }

    fn apply(&self, serial: u64, keep_running: &[&str]) -> Result<ApplyReport, String> {
        let serial = serial.to_string();
        let mut args = vec!["apply", "--json", "--expect-serial", serial.as_str()];
        for name in keep_running {
            args.push("--keep-running");
            args.push(name);
        }
        match self.run(&args)? {
            Report::Apply(report) => Ok(report),
            _ => Err("the update client answered something else".to_owned()),
        }
    }
}

/// What the helper remembers between requests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SystemState {
    #[serde(default)]
    pub automatic: bool,
    #[serde(default)]
    pub last_check: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub current_serial: u64,
    #[serde(default)]
    pub current_snapshot: Option<String>,
    #[serde(default)]
    pub available: Option<Available>,
    #[serde(default)]
    pub last_applied: Option<Applied>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Available {
    pub serial: u64,
    pub snapshot: String,
    pub needs_restart: bool,
    pub withdraws_current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Applied {
    pub at: u64,
    pub serial: u64,
    pub snapshot: String,
    pub changed: Vec<String>,
    pub restarted: Vec<String>,
    pub deferred: Vec<String>,
    pub needs_restart: bool,
    pub withdrew: Option<String>,
    pub automatic: bool,
}

impl Applied {
    fn result(&self) -> SystemUpdateResult {
        SystemUpdateResult {
            at: self.at,
            serial: self.serial,
            snapshot: self.snapshot.clone(),
            changed: self.changed.clone(),
            restarted: self.restarted.clone(),
            deferred: self.deferred.clone(),
            needs_restart: self.needs_restart,
            withdrew: self.withdrew.clone(),
            automatic: self.automatic,
        }
    }
}

fn hhmm(minute: u16) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// Whether the schedule should ask the server again.
pub(crate) fn check_due(state: &SystemState, now: u64) -> bool {
    let Some(last) = state.last_check else { return true };
    if last > now {
        return true;
    }
    let period = if state.last_error.is_some() { RETRY_SECS } else { CHECK_INTERVAL_SECS };
    now - last >= period
}

impl<S: System> Helper<S> {
    fn system_state_path(&self) -> PathBuf {
        self.manage.paths.updates.join("system.toml")
    }

    fn system_state(&self) -> Result<SystemState, ApiError> {
        self.load_state(&self.system_state_path())
    }

    fn save_system_state(&self, state: &SystemState) -> Result<(), ApiError> {
        self.save_state(&self.system_state_path(), state)
    }

    /// Changes the saved state under the state lock.
    fn with_system_state(&self, change: impl FnOnce(&mut SystemState)) -> Result<SystemState, ApiError> {
        let _guard = lock(&self.manage.state);
        let mut state = self.system_state()?;
        change(&mut state);
        self.save_system_state(&state)?;
        Ok(state)
    }

    fn package_window(&self) -> Result<nodo_update::auto::Window, ApiError> {
        let policy: AutoPolicy = self.load_state(&self.manage.paths.updates.join("policy.toml"))?;
        Ok(policy.schedule.window)
    }

    /// Why an automatic run would not apply the available snapshot now, or `None` when it would.
    fn automatic_wait(&self, state: &SystemState) -> Result<Option<String>, ApiError> {
        let Some(available) = &state.available else { return Ok(Some("up to date".into())) };
        if !state.automatic {
            return Ok(Some(
                "waits for you: automatic system updates are off, so the schedule only checks".into(),
            ));
        }
        if available.needs_restart {
            return Ok(Some("waits for you: it needs a restart".into()));
        }
        let window = self.package_window()?;
        if !window.contains(self.manage.packages.local_minute()) {
            return Ok(Some(format!("installs in the update window, from {}", hhmm(window.start()))));
        }
        if self.installs_refused()? {
            return Ok(Some("waiting until the disk has more space".into()));
        }
        Ok(None)
    }

    pub(crate) fn system_update_status(&self) -> Result<SystemUpdateStatus, ApiError> {
        let state = self.system_state()?;
        let unavailable = self.manage.snapshots.unavailable();
        let verdict = match (&unavailable, &state.last_check) {
            (Some(reason), _) => format!("system updates are not available: {reason}"),
            (None, None) => "not checked yet".to_owned(),
            (None, Some(_)) => self.automatic_wait(&state)?.unwrap_or_else(|| "installs in the next run".to_owned()),
        };
        Ok(SystemUpdateStatus {
            unavailable,
            current_serial: state.current_serial,
            current_snapshot: state.current_snapshot.clone(),
            available: state.available.as_ref().map(|a| SystemSnapshot {
                serial: a.serial,
                snapshot: a.snapshot.clone(),
                needs_restart: a.needs_restart,
                withdraws_current: a.withdraws_current,
            }),
            last_check: state.last_check,
            last_error: state.last_error.clone(),
            automatic: state.automatic,
            verdict,
            last_applied: state.last_applied.as_ref().map(Applied::result),
        })
    }

    /// Asks the server. A failure is recorded for the screens and returned.
    pub(crate) fn check_system_update(&self) -> Result<SystemUpdateStatus, ApiError> {
        if let Some(reason) = self.manage.snapshots.unavailable() {
            return Err(ApiError::Failed(format!("system updates are not available: {reason}")));
        }
        let now = self.manage.packages.now();
        let checked = self.manage.snapshots.check();
        self.with_system_state(|state| {
            state.last_check = Some(now);
            match &checked {
                Ok(report) => {
                    state.last_error = None;
                    state.current_serial = report.current_serial;
                    state.current_snapshot = report.current_snapshot.clone();
                    state.available = report.available.as_ref().map(|a| Available {
                        serial: a.serial,
                        snapshot: a.snapshot.clone(),
                        needs_restart: a.needs_restart,
                        withdraws_current: a.withdraws_current,
                    });
                }
                Err(e) => state.last_error = Some(e.clone()),
            }
        })?;
        checked.map_err(ApiError::Failed)?;
        self.system_update_status()
    }

    /// Applies the snapshot the last check found, if it is still `serial`.
    pub(crate) fn apply_system_update(&self, serial: u64, automatic: bool) -> Result<SystemUpdateResult, ApiError> {
        if let Some(reason) = self.manage.snapshots.unavailable() {
            return Err(ApiError::Failed(format!("system updates are not available: {reason}")));
        }
        let _work = lock(&self.manage.work);
        let state = self.system_state()?;
        match &state.available {
            Some(a) if a.serial == serial => {}
            Some(a) => {
                return Err(ApiError::NeedsConfirmation(format!(
                    "the available snapshot is now {} (serial {}). Look at it again.",
                    a.snapshot, a.serial
                )))
            }
            None => {
                return Err(ApiError::NeedsConfirmation(
                    "there is no system update to apply. Check again.".to_owned(),
                ))
            }
        }
        if self.installs_refused()? {
            return Err(ApiError::Failed("the system disk is low on space, so updates wait".to_owned()));
        }
        let now = self.manage.packages.now();
        let applied = self.manage.snapshots.apply(serial, &[HELPER_SERVICE]);
        let state = self.with_system_state(|state| match &applied {
            Ok(report) => {
                state.last_error = None;
                state.current_serial = report.serial;
                state.current_snapshot = report.snapshot.clone();
                state.available = None;
                if report.applied {
                    state.last_applied = Some(Applied {
                        at: now,
                        serial: report.serial,
                        snapshot: report.snapshot.clone().unwrap_or_default(),
                        changed: report.changed.clone(),
                        restarted: report.restarted.clone(),
                        deferred: report.deferred.clone(),
                        needs_restart: report.needs_restart,
                        withdrew: report.withdrew.clone(),
                        automatic,
                    });
                }
            }
            Err(e) => state.last_error = Some(e.clone()),
        })?;
        let report = applied.map_err(ApiError::Failed)?;
        if !report.applied {
            return Err(ApiError::Failed(format!("the system is already at serial {}", report.serial)));
        }
        let _ = self.refresh_health_config();
        state.last_applied.as_ref().map(Applied::result).ok_or_else(|| failed("the update was not recorded"))
    }

    pub(crate) fn set_system_auto_update(&self, enabled: bool) -> Result<Response, ApiError> {
        self.with_system_state(|state| state.automatic = enabled)?;
        Ok(Response::Ok)
    }

    /// One scheduled run. Checks when due; applies only when automatic system updates are on.
    pub(crate) fn run_system_update(&self) -> Result<Response, ApiError> {
        if self.manage.snapshots.unavailable().is_some() {
            return self.system_update_status().map(Response::SystemUpdate);
        }
        let now = self.manage.packages.now();
        if check_due(&self.system_state()?, now) {
            // A failed check is recorded and shown; the schedule tries again later.
            let _ = self.check_system_update();
        }
        let state = self.system_state()?;
        if let (Some(available), None) = (&state.available, self.automatic_wait(&state)?) {
            let _ = self.apply_system_update(available.serial, true);
        }
        self.system_update_status().map(Response::SystemUpdate)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use nodo_api::Request;
    use nodo_update::client::AvailableSnapshot;

    use super::*;
    use crate::fake::{helper, FakeSystem};
    use crate::manage::ManagePaths;

    const NOW: u64 = 1_000_000;

    #[derive(Default)]
    struct Inner {
        unavailable: Option<String>,
        current: u64,
        offered: Option<AvailableSnapshot>,
        fail: Option<String>,
        calls: Vec<String>,
    }

    #[derive(Clone, Default)]
    struct FakeSnapshots(Arc<Mutex<Inner>>);

    impl FakeSnapshots {
        fn offer(&self, serial: u64, needs_restart: bool, withdraws_current: bool) {
            self.0.lock().unwrap().offered = Some(AvailableSnapshot {
                serial,
                snapshot: format!("s{serial}"),
                needs_restart,
                withdraws_current,
            });
        }
        fn calls(&self) -> Vec<String> {
            self.0.lock().unwrap().calls.clone()
        }
    }

    impl SnapshotClient for FakeSnapshots {
        fn unavailable(&self) -> Option<String> {
            self.0.lock().unwrap().unavailable.clone()
        }
        fn check(&self) -> Result<CheckReport, String> {
            let mut s = self.0.lock().unwrap();
            s.calls.push("check".into());
            if let Some(e) = &s.fail {
                return Err(e.clone());
            }
            Ok(CheckReport {
                current_serial: s.current,
                current_snapshot: Some(format!("s{}", s.current)),
                available: s.offered.clone(),
            })
        }
        fn apply(&self, serial: u64, keep_running: &[&str]) -> Result<ApplyReport, String> {
            let mut s = self.0.lock().unwrap();
            s.calls.push(format!("apply {serial} keep {}", keep_running.join(",")));
            if let Some(e) = &s.fail {
                return Err(e.clone());
            }
            let offered = s.offered.take().ok_or("nothing offered")?;
            if offered.serial != serial {
                return Err(format!("the channel now offers serial {}, not {serial}. Check again.", offered.serial));
            }
            s.current = serial;
            Ok(ApplyReport {
                applied: true,
                serial,
                snapshot: Some(offered.snapshot),
                changed: vec!["nodo-tools: 0.1_1 -> 0.1_2".into()],
                restarted: vec![],
                deferred: vec!["nodo-helper".into()],
                needs_restart: offered.needs_restart,
                withdrew: offered.withdraws_current.then(|| "s5".to_owned()),
            })
        }
    }

    fn rig() -> (Helper<FakeSystem>, FakeSnapshots) {
        let sys = FakeSystem::default();
        let report = format!(
            r#"{{"version":1,"generated_at":{NOW},"disks":[],"stopped":[],"refuse_installs":false,"time":{{"synced":true}},"attention":[]}}"#
        );
        sys.put("/run/nodo/health.json", report.as_bytes());
        let snapshots = FakeSnapshots::default();
        snapshots.0.lock().unwrap().current = 5;
        let paths = ManagePaths { updates: "/s/updates".into(), ..ManagePaths::default() };
        let mut h = helper(sys).with_manage_paths(paths).with_snapshots(Box::new(snapshots.clone()));
        h.manage.packages = Box::new(Clock { now: NOW, minute: 4 * 60 });
        (h, snapshots)
    }

    /// Time and the update window's clock; nothing else of the package backend is used here.
    struct Clock {
        now: u64,
        minute: u16,
    }

    impl crate::pkgsys::PackageBackend for Clock {
        fn index(&self, _: &str) -> Result<crate::pkgsys::FeedIndex, String> {
            Err("no feeds here".into())
        }
        fn review(
            &self,
            _: &mut nodo_feed::PinStore,
            _: &str,
            _: &str,
            _: Option<&nodo_install::Permissions>,
        ) -> Result<nodo_install::Reviewed, nodo_install::InstallError> {
            unreachable!()
        }
        fn install(
            &self,
            _: &mut nodo_feed::PinStore,
            _: &str,
            _: &str,
            _: &nodo_install::Permissions,
            _: &str,
            _: &std::path::Path,
        ) -> Result<(nodo_install::Outcome, Option<PathBuf>), nodo_install::InstallError> {
            unreachable!()
        }
        fn reinstall(&self, _: &std::path::Path) -> Result<String, nodo_install::InstallError> {
            unreachable!()
        }
        fn inspect_kept(
            &self,
            _: &std::path::Path,
        ) -> Result<(String, nodo_install::Permissions), nodo_install::InstallError> {
            unreachable!()
        }
        fn remove(&self, _: &str) -> Result<(), nodo_install::InstallError> {
            unreachable!()
        }
        fn now(&self) -> u64 {
            self.now
        }
        fn local_minute(&self) -> u16 {
            self.minute
        }
        fn copy_tree(&self, _: &std::path::Path, _: &std::path::Path, _: Option<&str>) -> Result<(), String> {
            unreachable!()
        }
        fn restore_tree(&self, _: &std::path::Path, _: &std::path::Path, _: Option<&str>) -> Result<(), String> {
            unreachable!()
        }
        fn remove_tree(&self, _: &std::path::Path) -> Result<(), String> {
            Ok(())
        }
        fn tree_size(&self, _: &std::path::Path) -> u64 {
            0
        }
        fn free_bytes(&self, _: &std::path::Path) -> u64 {
            u64::MAX
        }
        fn kept_exists(&self, _: &std::path::Path) -> bool {
            false
        }
        fn remove_kept(&self, _: &std::path::Path) {}
    }

    fn status(h: &Helper<FakeSystem>) -> SystemUpdateStatus {
        match h.handle(1000, Request::SystemUpdateStatus) {
            Ok(Response::SystemUpdate(s)) => s,
            other => panic!("{other:?}"),
        }
    }

    fn run(h: &Helper<FakeSystem>) -> SystemUpdateStatus {
        // Root is the scheduler. The test helper lets uid 1000 connect only, so this skips that step.
        match h.handle_managed(0, Request::RunSystemUpdate) {
            Ok(Response::SystemUpdate(s)) => s,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn automatic_system_updates_are_off_by_default_and_the_schedule_only_checks() {
        let (h, snapshots) = rig();
        assert!(!status(&h).automatic);
        assert_eq!(status(&h).verdict, "not checked yet");
        snapshots.offer(6, false, false);
        let s = run(&h);
        assert_eq!(snapshots.calls(), ["check"], "checked, and nothing applied");
        assert_eq!(s.available.as_ref().map(|a| a.serial), Some(6));
        assert!(s.verdict.contains("automatic system updates are off"), "{}", s.verdict);
        assert_eq!(s.current_serial, 5);
        // Not due again for hours, so the next run does nothing at all.
        run(&h);
        assert_eq!(snapshots.calls(), ["check"]);
    }

    #[test]
    fn only_root_may_run_the_schedule() {
        let (h, snapshots) = rig();
        assert_eq!(h.handle(1000, Request::RunSystemUpdate), Err(ApiError::Denied));
        assert_eq!(h.handle_managed(1000, Request::RunSystemUpdate), Err(ApiError::Denied));
        assert!(snapshots.calls().is_empty());
    }

    #[test]
    fn the_owner_applies_exactly_what_was_shown_and_the_helper_keeps_running() {
        let (h, snapshots) = rig();
        snapshots.offer(6, false, false);
        let Ok(Response::SystemUpdate(checked)) = h.handle(1000, Request::CheckSystemUpdate) else { panic!() };
        assert_eq!(checked.available.unwrap().snapshot, "s6");
        assert!(matches!(
            h.handle(1000, Request::ApplySystemUpdate { serial: 7 }),
            Err(ApiError::NeedsConfirmation(_))
        ));
        assert_eq!(snapshots.calls(), ["check"], "a serial the owner did not see is never applied");
        let Ok(Response::SystemUpdateApplied(result)) = h.handle(1000, Request::ApplySystemUpdate { serial: 6 }) else {
            panic!()
        };
        assert_eq!(snapshots.calls()[1], "apply 6 keep nodo-helper");
        assert_eq!((result.serial, result.automatic), (6, false));
        assert_eq!(result.deferred, ["nodo-helper"]);
        let s = status(&h);
        assert_eq!((s.current_serial, s.available), (6, None));
        assert_eq!(s.last_applied.unwrap().changed, ["nodo-tools: 0.1_1 -> 0.1_2"]);
        assert!(matches!(
            h.handle(1000, Request::ApplySystemUpdate { serial: 6 }),
            Err(ApiError::NeedsConfirmation(_))
        ));
    }

    #[test]
    fn when_turned_on_the_schedule_applies_inside_the_window_unless_a_restart_is_needed() {
        let (h, snapshots) = rig();
        h.handle(1000, Request::SetSystemAutoUpdate { enabled: true }).unwrap();
        snapshots.offer(6, true, false);
        let s = run(&h);
        assert_eq!(snapshots.calls(), ["check"]);
        assert!(s.verdict.contains("needs a restart"), "{}", s.verdict);

        let (mut h, snapshots) = rig();
        h.handle(1000, Request::SetSystemAutoUpdate { enabled: true }).unwrap();
        snapshots.offer(6, false, false);
        // 04:00 is inside the default 03:00 to 05:00 window.
        let s = run(&h);
        assert_eq!(snapshots.calls(), ["check", "apply 6 keep nodo-helper"]);
        assert!(s.last_applied.unwrap().automatic);
        // Outside the window, a new snapshot waits.
        snapshots.offer(7, false, false);
        h.manage.packages = Box::new(Clock { now: NOW + CHECK_INTERVAL_SECS, minute: 12 * 60 });
        let s = run(&h);
        assert_eq!(snapshots.calls().len(), 3, "{:?}", snapshots.calls());
        assert!(s.verdict.contains("update window"), "{}", s.verdict);
    }

    #[test]
    fn a_withdrawal_is_shown_as_such_and_applied_like_any_update() {
        let (h, snapshots) = rig();
        snapshots.offer(6, false, true);
        let Ok(Response::SystemUpdate(s)) = h.handle(1000, Request::CheckSystemUpdate) else { panic!() };
        assert!(s.available.unwrap().withdraws_current);
        let Ok(Response::SystemUpdateApplied(result)) = h.handle(1000, Request::ApplySystemUpdate { serial: 6 }) else {
            panic!()
        };
        assert_eq!(result.withdrew.as_deref(), Some("s5"));
    }

    #[test]
    fn failures_are_recorded_retried_sooner_and_change_nothing() {
        let (mut h, snapshots) = rig();
        snapshots.0.lock().unwrap().fail = Some("signed by an unknown key".into());
        assert!(matches!(h.handle(1000, Request::CheckSystemUpdate), Err(ApiError::Failed(e)) if e.contains("unknown key")));
        let s = status(&h);
        assert_eq!(s.last_error.as_deref(), Some("signed by an unknown key"));
        assert_eq!(s.available, None);
        h.manage.packages = Box::new(Clock { now: NOW + RETRY_SECS, minute: 4 * 60 });
        run(&h);
        assert_eq!(snapshots.calls().len(), 2, "a failed check is retried after an hour");
    }

    #[test]
    fn without_a_server_nothing_runs_and_the_screen_says_why() {
        let (h, snapshots) = rig();
        snapshots.0.lock().unwrap().unavailable = Some("no update server is set".into());
        let s = run(&h);
        assert!(s.verdict.contains("no update server is set"));
        assert!(matches!(h.handle(1000, Request::CheckSystemUpdate), Err(ApiError::Failed(_))));
        assert!(snapshots.calls().is_empty());
    }

    #[test]
    fn the_client_reports_on_its_last_line() {
        let ok = parse_report(br#"{"check":{"current_serial":5,"current_snapshot":null,"available":null}}"#, b"");
        assert!(matches!(ok, Ok(Report::Check(_))));
        let refused = parse_report(br#"{"error":"the snapshot was refused: manifest expired at 2"}"#, b"");
        assert_eq!(refused.unwrap_err(), "the snapshot was refused: manifest expired at 2");
        let crashed = parse_report(b"", b"nodo-sysupdate: no update server is set\n");
        assert_eq!(crashed.unwrap_err(), "nodo-sysupdate: no update server is set");
    }
}
