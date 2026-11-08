//! Automatic updates: off by default, chosen per package, and only inside a scheduled window.
//!
//! Some updates always wait for the user, whatever the policy says: new permissions, a changed
//! feed key, failed install checks, a version the user put on hold, and anything needing a system
//! restart. Others wait for conditions to change: the window, a locked device, or a low disk.
//! A failed update is rolled back, its version is held so nothing retries it, and the outcome is
//! kept for the Home screen until the user dismisses it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

const MINUTES_PER_DAY: u16 = 24 * 60;
const HISTORY_LEN: usize = 50;
/// Checks drift less if "daily" means "at least 23 hours since the last check".
const CHECK_SLACK_SECS: u64 = 3600;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckFrequency {
    #[default]
    Daily,
    Weekly,
    /// Only when the user presses "check now".
    Manual,
}

/// Device-local minutes since midnight. `start == end` is not a window, and a window may cross
/// midnight, so 23:00 to 01:00 works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "(u16, u16)", into = "(u16, u16)")]
pub struct Window {
    start: u16,
    end: u16,
}

impl Window {
    pub fn new(start: u16, end: u16) -> Option<Self> {
        (start < MINUTES_PER_DAY && end < MINUTES_PER_DAY && start != end).then_some(Window { start, end })
    }

    pub fn start(&self) -> u16 {
        self.start
    }

    pub fn end(&self) -> u16 {
        self.end
    }

    pub fn contains(&self, minute: u16) -> bool {
        if self.start < self.end {
            (self.start..self.end).contains(&minute)
        } else {
            minute >= self.start || minute < self.end
        }
    }

    /// Minutes until the window next opens, zero while it is open. For "next run tonight at 03:00".
    pub fn minutes_until_open(&self, minute: u16) -> u16 {
        if self.contains(minute) {
            0
        } else {
            (self.start + MINUTES_PER_DAY - minute % MINUTES_PER_DAY) % MINUTES_PER_DAY
        }
    }
}

impl Default for Window {
    fn default() -> Self {
        Window { start: 3 * 60, end: 5 * 60 }
    }
}

impl TryFrom<(u16, u16)> for Window {
    type Error = String;

    fn try_from((start, end): (u16, u16)) -> Result<Self, String> {
        Window::new(start, end).ok_or_else(|| "a window needs two different times of day".to_owned())
    }
}

impl From<Window> for (u16, u16) {
    fn from(w: Window) -> Self {
        (w.start, w.end)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub check: CheckFrequency,
    pub window: Window,
}

impl Schedule {
    pub fn check_due(&self, last_check: Option<u64>, now: u64) -> bool {
        let period = match self.check {
            CheckFrequency::Daily => 86_400,
            CheckFrequency::Weekly => 7 * 86_400,
            CheckFrequency::Manual => return false,
        };
        match last_check {
            None => true,
            // A clock that jumped backwards would otherwise stop checks until it caught up.
            Some(last) if last > now => true,
            Some(last) => now - last >= period - CHECK_SLACK_SECS,
        }
    }
}

/// The user's choices. The default updates nothing automatically.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoPolicy {
    pub schedule: Schedule,
    #[serde(default)]
    pub packages: BTreeSet<String>,
}

impl AutoPolicy {
    pub fn set(&mut self, package: &str, enabled: bool) {
        if enabled {
            self.packages.insert(package.to_owned());
        } else {
            self.packages.remove(package);
        }
    }

    pub fn is_automatic(&self, package: &str) -> bool {
        self.packages.contains(package)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    App,
    /// The provider of `monero-daemon`. It restarts last, so its dependents restart once.
    Node,
    /// The base system snapshot.
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Checks {
    Passed,
    Failed(String),
}

/// An available update and what the feed gate found out about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub package: String,
    pub from: String,
    pub to: String,
    pub origin: Origin,
    pub new_permissions: bool,
    pub key_changed: bool,
    pub checks: Checks,
    pub needs_restart: bool,
}

/// Versions the user skipped, or that failed and were rolled back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holds {
    #[serde(default)]
    pub versions: BTreeMap<String, BTreeSet<String>>,
}

impl Holds {
    pub fn hold(&mut self, package: &str, version: &str) {
        self.versions.entry(package.to_owned()).or_default().insert(version.to_owned());
    }

    /// Only from a user's "try the update again".
    pub fn release(&mut self, package: &str, version: &str) {
        if let Some(set) = self.versions.get_mut(package) {
            set.remove(version);
            if set.is_empty() {
                self.versions.remove(package);
            }
        }
    }

    pub fn is_held(&self, package: &str, version: &str) -> bool {
        self.versions.get(package).is_some_and(|s| s.contains(version))
    }
}

/// Re-read before every install, since a long run can fill the disk or cross the window's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conditions {
    pub locked: bool,
    pub disk_low: bool,
    pub minute_of_day: u16,
}

/// Reasons an update always waits for the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Blocker {
    NewPermissions,
    KeyChanged,
    FailedChecks(String),
    Held,
    RestartNeeded,
}

/// Reasons an automatic update waits for conditions to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Wait {
    OutsideWindow,
    Locked,
    DiskLow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Install,
    /// The user has not turned on auto-update for this package. It is only announced.
    NotAutomatic,
    NeedsYou(Vec<Blocker>),
    Waiting(Wait),
}

pub fn blockers(holds: &Holds, c: &Candidate) -> Vec<Blocker> {
    let mut out = Vec::new();
    if c.new_permissions {
        out.push(Blocker::NewPermissions);
    }
    if c.key_changed {
        out.push(Blocker::KeyChanged);
    }
    if let Checks::Failed(reason) = &c.checks {
        out.push(Blocker::FailedChecks(reason.clone()));
    }
    if holds.is_held(&c.package, &c.to) {
        out.push(Blocker::Held);
    }
    if c.needs_restart {
        out.push(Blocker::RestartNeeded);
    }
    out
}

fn wait_reason(window: &Window, cond: &Conditions) -> Option<Wait> {
    if !window.contains(cond.minute_of_day) {
        Some(Wait::OutsideWindow)
    } else if cond.locked {
        Some(Wait::Locked)
    } else if cond.disk_low {
        Some(Wait::DiskLow)
    } else {
        None
    }
}

pub fn decide(policy: &AutoPolicy, holds: &Holds, c: &Candidate, cond: &Conditions) -> Verdict {
    if !policy.is_automatic(&c.package) {
        return Verdict::NotAutomatic;
    }
    let blocking = blockers(holds, c);
    if !blocking.is_empty() {
        return Verdict::NeedsYou(blocking);
    }
    match wait_reason(&policy.schedule.window, cond) {
        Some(wait) => Verdict::Waiting(wait),
        None => Verdict::Install,
    }
}

fn install_rank(origin: Origin) -> u8 {
    match origin {
        Origin::App => 0,
        Origin::System => 1,
        Origin::Node => 2,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// In install order, with the node last.
    pub install: Vec<Candidate>,
    pub verdicts: Vec<(String, Verdict)>,
}

pub fn plan(policy: &AutoPolicy, holds: &Holds, candidates: &[Candidate], cond: &Conditions) -> Plan {
    let mut install = Vec::new();
    let mut verdicts = Vec::new();
    for c in candidates {
        let verdict = decide(policy, holds, c, cond);
        if verdict == Verdict::Install {
            install.push(c.clone());
        }
        verdicts.push((c.package.clone(), verdict));
    }
    install.sort_by_key(|c| install_rank(c.origin));
    Plan { install, verdicts }
}

/// Why an install, health check or rollback failed, in words for the user and the publisher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure(pub String);

/// The install step, implemented by nodo-install. Each call must leave the package either fully
/// on the new version or fully on the old one.
pub trait Installer {
    fn install(&mut self, c: &Candidate) -> Result<(), Failure>;
    /// Restarts the package and waits for its declared health check.
    fn health_check(&mut self, c: &Candidate) -> Result<(), Failure>;
    /// Restores the previous version and its data.
    fn rollback(&mut self, c: &Candidate) -> Result<(), Failure>;
}

pub trait Device {
    fn conditions(&self) -> Conditions;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Updated { package: String, from: String, to: String },
    RolledBack { package: String, from: String, to: String, reason: String },
    /// The worst case: the new version failed and the old one did not come back.
    RollbackFailed { package: String, from: String, to: String, reason: String, rollback_error: String },
    Postponed { package: String, wait: Wait },
}

impl Outcome {
    fn needs_attention(&self) -> bool {
        matches!(self, Outcome::RolledBack { .. } | Outcome::RollbackFailed { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub at: u64,
    pub outcome: Outcome,
}

/// Everything persisted between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    #[serde(default)]
    pub last_check: Option<u64>,
    #[serde(default)]
    pub holds: Holds,
    #[serde(default)]
    pub history: Vec<Event>,
    /// Shown on Home until dismissed.
    #[serde(default)]
    pub attention: Vec<Event>,
}

impl UpdateState {
    fn record(&mut self, at: u64, outcome: Outcome) {
        let event = Event { at, outcome };
        if event.outcome.needs_attention() {
            self.attention.push(event.clone());
        }
        self.history.push(event);
        if self.history.len() > HISTORY_LEN {
            let excess = self.history.len() - HISTORY_LEN;
            self.history.drain(..excess);
        }
    }

    pub fn dismiss(&mut self, package: &str) {
        self.attention.retain(|e| match &e.outcome {
            Outcome::RolledBack { package: p, .. } | Outcome::RollbackFailed { package: p, .. } => p != package,
            _ => true,
        });
    }
}

/// One scheduled run. Installs what the policy allows, one package at a time, re-checking the
/// device before each, and stops starting new installs once a condition fails.
pub fn run(
    policy: &AutoPolicy,
    state: &mut UpdateState,
    candidates: &[Candidate],
    device: &dyn Device,
    installer: &mut dyn Installer,
    now: u64,
) -> Vec<Outcome> {
    let first = plan(policy, &state.holds, candidates, &device.conditions());
    let mut outcomes = Vec::new();
    let mut stopped: Option<Wait> = None;
    for c in &first.install {
        if stopped.is_none() {
            stopped = wait_reason(&policy.schedule.window, &device.conditions());
        }
        if let Some(wait) = stopped {
            outcomes.push(Outcome::Postponed { package: c.package.clone(), wait });
            continue;
        }
        let outcome = apply_one(installer, c);
        if !matches!(outcome, Outcome::Updated { .. }) {
            state.holds.hold(&c.package, &c.to);
        }
        state.record(now, outcome.clone());
        outcomes.push(outcome);
    }
    outcomes
}

fn apply_one(installer: &mut dyn Installer, c: &Candidate) -> Outcome {
    let failure = match installer.install(c).and_then(|()| installer.health_check(c)) {
        Ok(()) => return Outcome::Updated { package: c.package.clone(), from: c.from.clone(), to: c.to.clone() },
        Err(Failure(reason)) => reason,
    };
    match installer.rollback(c) {
        Ok(()) => Outcome::RolledBack {
            package: c.package.clone(),
            from: c.from.clone(),
            to: c.to.clone(),
            reason: failure,
        },
        Err(Failure(rollback_error)) => Outcome::RollbackFailed {
            package: c.package.clone(),
            from: c.from.clone(),
            to: c.to.clone(),
            reason: failure,
            rollback_error,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn cand(package: &str, origin: Origin) -> Candidate {
        Candidate {
            package: package.into(),
            from: "1.0_1".into(),
            to: "1.1_1".into(),
            origin,
            new_permissions: false,
            key_changed: false,
            checks: Checks::Passed,
            needs_restart: false,
        }
    }

    fn open() -> Conditions {
        Conditions { locked: false, disk_low: false, minute_of_day: 3 * 60 + 30 }
    }

    fn policy(packages: &[&str]) -> AutoPolicy {
        let mut p = AutoPolicy::default();
        for pkg in packages {
            p.set(pkg, true);
        }
        p
    }

    #[derive(Default)]
    struct FakeInstaller {
        fail_install: BTreeSet<String>,
        fail_health: BTreeSet<String>,
        fail_rollback: BTreeSet<String>,
        log: Vec<String>,
    }

    impl Installer for FakeInstaller {
        fn install(&mut self, c: &Candidate) -> Result<(), Failure> {
            self.log.push(format!("install {}", c.package));
            if self.fail_install.contains(&c.package) {
                return Err(Failure("archive did not extract".into()));
            }
            Ok(())
        }
        fn health_check(&mut self, c: &Candidate) -> Result<(), Failure> {
            self.log.push(format!("health {}", c.package));
            if self.fail_health.contains(&c.package) {
                return Err(Failure("database migration 14: table \"invoices\" already exists".into()));
            }
            Ok(())
        }
        fn rollback(&mut self, c: &Candidate) -> Result<(), Failure> {
            self.log.push(format!("rollback {}", c.package));
            if self.fail_rollback.contains(&c.package) {
                return Err(Failure("snapshot missing".into()));
            }
            Ok(())
        }
    }

    struct FixedDevice(Conditions);
    impl Device for FixedDevice {
        fn conditions(&self) -> Conditions {
            self.0
        }
    }

    /// Becomes locked after a number of reads, like a user locking the device mid-run.
    struct LocksAfter {
        reads: Cell<u32>,
        after: u32,
    }
    impl Device for LocksAfter {
        fn conditions(&self) -> Conditions {
            let n = self.reads.get();
            self.reads.set(n + 1);
            Conditions { locked: n >= self.after, ..open() }
        }
    }

    #[test]
    fn off_by_default() {
        let c = cand("moneropay", Origin::App);
        assert_eq!(decide(&AutoPolicy::default(), &Holds::default(), &c, &open()), Verdict::NotAutomatic);
        assert!(AutoPolicy::default().packages.is_empty());
    }

    #[test]
    fn opted_in_package_installs_inside_the_window() {
        let c = cand("moneropay", Origin::App);
        assert_eq!(decide(&policy(&["moneropay"]), &Holds::default(), &c, &open()), Verdict::Install);
    }

    #[test]
    fn every_blocker_waits_for_the_user_and_all_are_listed() {
        let p = policy(&["x"]);
        let base = cand("x", Origin::App);
        assert_eq!(decide(&p, &Holds::default(), &base, &open()), Verdict::Install, "control");
        type Change = Box<dyn Fn(&mut Candidate, &mut Holds)>;
        let cases: Vec<(Change, Blocker)> = vec![
            (Box::new(|c, _| c.new_permissions = true), Blocker::NewPermissions),
            (Box::new(|c, _| c.key_changed = true), Blocker::KeyChanged),
            (Box::new(|c, _| c.checks = Checks::Failed("setuid file".into())), Blocker::FailedChecks("setuid file".into())),
            (Box::new(|c, h| h.hold(&c.package, &c.to)), Blocker::Held),
            (Box::new(|c, _| c.needs_restart = true), Blocker::RestartNeeded),
        ];
        let mut all = base.clone();
        let mut all_holds = Holds::default();
        for (change, expected) in &cases {
            let mut c = base.clone();
            let mut holds = Holds::default();
            change(&mut c, &mut holds);
            assert_eq!(decide(&p, &holds, &c, &open()), Verdict::NeedsYou(vec![expected.clone()]));
            change(&mut all, &mut all_holds);
        }
        match decide(&p, &all_holds, &all, &open()) {
            Verdict::NeedsYou(list) => assert_eq!(list.len(), cases.len()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn blockers_win_over_waits_even_outside_the_window() {
        let mut c = cand("x", Origin::App);
        c.new_permissions = true;
        let cond = Conditions { minute_of_day: 12 * 60, locked: true, disk_low: true };
        assert_eq!(decide(&policy(&["x"]), &Holds::default(), &c, &cond), Verdict::NeedsYou(vec![Blocker::NewPermissions]));
    }

    #[test]
    fn waits_outside_the_window_while_locked_or_low_on_disk() {
        let p = policy(&["x"]);
        let c = cand("x", Origin::App);
        let h = Holds::default();
        let cases = [
            (Conditions { minute_of_day: 2 * 60 + 59, ..open() }, Wait::OutsideWindow),
            (Conditions { minute_of_day: 5 * 60, ..open() }, Wait::OutsideWindow),
            (Conditions { locked: true, ..open() }, Wait::Locked),
            (Conditions { disk_low: true, ..open() }, Wait::DiskLow),
        ];
        for (cond, wait) in cases {
            assert_eq!(decide(&p, &h, &c, &cond), Verdict::Waiting(wait), "{cond:?}");
        }
    }

    #[test]
    fn a_held_version_does_not_hold_the_next_one() {
        let mut holds = Holds::default();
        holds.hold("x", "1.1_1");
        let mut c = cand("x", Origin::App);
        c.to = "1.2_1".into();
        assert_eq!(decide(&policy(&["x"]), &holds, &c, &open()), Verdict::Install);
    }

    #[test]
    fn system_restart_updates_are_never_automatic() {
        let mut c = cand("system", Origin::System);
        c.needs_restart = true;
        assert_eq!(decide(&policy(&["system"]), &Holds::default(), &c, &open()), Verdict::NeedsYou(vec![Blocker::RestartNeeded]));
    }

    #[test]
    fn the_node_installs_last() {
        let p = policy(&["cuprated", "moneropay", "system"]);
        let list = [cand("cuprated", Origin::Node), cand("system", Origin::System), cand("moneropay", Origin::App)];
        let order: Vec<_> = plan(&p, &Holds::default(), &list, &open()).install.into_iter().map(|c| c.package).collect();
        assert_eq!(order, ["moneropay", "system", "cuprated"]);
    }

    #[test]
    fn a_failed_health_check_rolls_back_holds_and_reports() {
        let p = policy(&["moneropay", "bell"]);
        let mut state = UpdateState::default();
        let mut inst = FakeInstaller::default();
        inst.fail_health.insert("moneropay".into());
        let list = [cand("moneropay", Origin::App), cand("bell", Origin::App)];
        let out = run(&p, &mut state, &list, &FixedDevice(open()), &mut inst, 100);
        assert!(matches!(&out[0], Outcome::RolledBack { reason, .. } if reason.contains("invoices")));
        assert!(matches!(&out[1], Outcome::Updated { .. }), "one failure does not stop the others");
        assert!(inst.log.contains(&"rollback moneropay".to_owned()));
        assert!(!inst.log.contains(&"rollback bell".to_owned()));
        assert!(state.holds.is_held("moneropay", "1.1_1"));
        assert_eq!(state.attention.len(), 1);
        assert_eq!(state.history.len(), 2);

        // The next scheduled run must not try that version again.
        inst.log.clear();
        let out = run(&p, &mut state, &list[..1], &FixedDevice(open()), &mut inst, 200);
        assert!(out.is_empty() && inst.log.is_empty(), "{out:?} {:?}", inst.log);

        // Until the user chooses to retry.
        state.holds.release("moneropay", "1.1_1");
        state.dismiss("moneropay");
        assert!(state.attention.is_empty());
        inst.fail_health.clear();
        let out = run(&p, &mut state, &list[..1], &FixedDevice(open()), &mut inst, 300);
        assert!(matches!(&out[0], Outcome::Updated { .. }));
    }

    #[test]
    fn a_failed_install_also_rolls_back() {
        let mut inst = FakeInstaller::default();
        inst.fail_install.insert("x".into());
        let mut state = UpdateState::default();
        let out = run(&policy(&["x"]), &mut state, &[cand("x", Origin::App)], &FixedDevice(open()), &mut inst, 1);
        assert!(matches!(&out[0], Outcome::RolledBack { .. }));
        assert_eq!(inst.log, ["install x", "rollback x"], "no health check after a failed install");
    }

    #[test]
    fn a_failed_rollback_is_reported_as_such() {
        let mut inst = FakeInstaller::default();
        inst.fail_health.insert("x".into());
        inst.fail_rollback.insert("x".into());
        let mut state = UpdateState::default();
        let out = run(&policy(&["x"]), &mut state, &[cand("x", Origin::App)], &FixedDevice(open()), &mut inst, 1);
        assert!(matches!(&out[0], Outcome::RollbackFailed { rollback_error, .. } if rollback_error == "snapshot missing"));
        assert!(state.holds.is_held("x", "1.1_1"));
        assert_eq!(state.attention.len(), 1);
    }

    #[test]
    fn locking_mid_run_postpones_the_rest() {
        let p = policy(&["a", "b", "c"]);
        let list = [cand("a", Origin::App), cand("b", Origin::App), cand("c", Origin::App)];
        let mut inst = FakeInstaller::default();
        let mut state = UpdateState::default();
        // Read once for the plan and once before "a", then locked.
        let device = LocksAfter { reads: Cell::new(0), after: 2 };
        let out = run(&p, &mut state, &list, &device, &mut inst, 1);
        assert!(matches!(&out[0], Outcome::Updated { .. }));
        assert_eq!(out[1], Outcome::Postponed { package: "b".into(), wait: Wait::Locked });
        assert_eq!(out[2], Outcome::Postponed { package: "c".into(), wait: Wait::Locked });
        assert_eq!(inst.log, ["install a", "health a"]);
        assert!(!state.holds.is_held("b", "1.1_1"), "postponed is not held");
    }

    #[test]
    fn windows_can_cross_midnight() {
        let w = Window::new(23 * 60, 60).unwrap();
        assert!(w.contains(23 * 60) && w.contains(0) && w.contains(59));
        assert!(!w.contains(60) && !w.contains(22 * 60));
        assert!(Window::new(60, 60).is_none());
        assert!(Window::new(0, 1440).is_none());
        assert_eq!(Window::default().minutes_until_open(22 * 60), 5 * 60);
        assert_eq!(Window::default().minutes_until_open(4 * 60), 0);
        assert_eq!(Window::default().minutes_until_open(6 * 60), 21 * 60);
    }

    #[test]
    fn checks_are_due_by_frequency() {
        let daily = Schedule::default();
        assert!(daily.check_due(None, 0));
        assert!(!daily.check_due(Some(1000), 1000 + 3600));
        assert!(daily.check_due(Some(1000), 1000 + 86_400 - 3600));
        assert!(daily.check_due(Some(5000), 10), "a clock that went backwards still checks");
        let manual = Schedule { check: CheckFrequency::Manual, ..Schedule::default() };
        assert!(!manual.check_due(None, 0));
    }

    #[test]
    fn state_and_policy_round_trip_through_toml() {
        let mut state = UpdateState::default();
        state.holds.hold("x", "1.1_1");
        state.record(5, Outcome::RolledBack { package: "x".into(), from: "1".into(), to: "2".into(), reason: "r".into() });
        state.record(6, Outcome::Postponed { package: "y".into(), wait: Wait::DiskLow });
        let text = toml::to_string(&state).unwrap();
        assert_eq!(toml::from_str::<UpdateState>(&text).unwrap(), state);
        let p = policy(&["a"]);
        let text = toml::to_string(&p).unwrap();
        assert_eq!(toml::from_str::<AutoPolicy>(&text).unwrap(), p);
        assert!(toml::from_str::<AutoPolicy>("[schedule]\ncheck = \"Daily\"\nwindow = [60, 60]\n").is_err());
    }

    #[test]
    fn history_is_bounded() {
        let mut state = UpdateState::default();
        for i in 0..(HISTORY_LEN as u64 + 10) {
            state.record(i, Outcome::Updated { package: "x".into(), from: "1".into(), to: "2".into() });
        }
        assert_eq!(state.history.len(), HISTORY_LEN);
        assert_eq!(state.history[0].at, 10);
    }
}
