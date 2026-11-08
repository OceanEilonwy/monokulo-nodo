//! Updates of packages installed from feeds, by hand and automatically.
//!
//! A check refreshes the feeds and inspects each newer version, so an update that asks for more,
//! comes with a changed key or fails the install checks is known before anyone selects it. The
//! rules of `nodo_update::auto` decide what runs unattended. Every update keeps the previous
//! version's verified file and a copy of the package's data, and a failed install or health
//! check puts both back and holds that version.

use std::collections::BTreeMap;
use std::path::PathBuf;

use nodo_api::{
    ApiError, AutoUpdatePrefs, InstallReview, PermissionApproval, Response, UpdateApproval,
    UpdateCandidate, UpdateEvent, UpdateEventKind, UpdateInfo, UpdateOutcome, UpdateResult,
    UpdateStatus,
};
use nodo_feed::{compare_versions, BlockReason};
use nodo_gen::{volume_mount, Layout, Profile};
use nodo_install::{InstallError, Outcome, Permissions, Reviewed};
use nodo_update::auto::{
    self, AutoPolicy, Candidate, CheckFrequency, Checks, Conditions, Device, Failure, Installer,
    Origin, Schedule, UpdateState, Verdict, Wait, Window,
};
use serde::{Deserialize, Serialize};

use crate::apps::block_text;
use crate::helper::{lock, Helper};
use crate::system::{SvAction, System};

const NODE_ROLE: &str = "monero-daemon";
const HISTORY_LEN: usize = 50;
/// Inside an encrypted volume, so the copy of its data stays encrypted.
const IN_VOLUME_COPY: &str = ".nodo-rollback";
/// Room left over after copying a package's data, so the copy never fills the disk.
const COPY_MARGIN: u64 = 256 * 1024 * 1024;

/// What the last check found for one package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CandidateRecord {
    pub package: String,
    pub feed: String,
    pub from: String,
    pub to: String,
    pub node: bool,
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub key_changed: bool,
    #[serde(default)]
    pub checks_failed: Option<String>,
}

impl CandidateRecord {
    fn pkgver(&self) -> String {
        format!("{}-{}", self.package, self.to)
    }

    fn candidate(&self) -> Candidate {
        Candidate {
            package: self.package.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            origin: if self.node { Origin::Node } else { Origin::App },
            new_permissions: !self.added.is_empty(),
            key_changed: self.key_changed,
            checks: match &self.checks_failed {
                Some(reason) => Checks::Failed(reason.clone()),
                None => Checks::Passed,
            },
            needs_restart: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Candidates {
    #[serde(default)]
    list: Vec<CandidateRecord>,
}

fn frequency_name(f: CheckFrequency) -> &'static str {
    match f {
        CheckFrequency::Daily => "Daily",
        CheckFrequency::Weekly => "Weekly",
        CheckFrequency::Manual => "Manual",
    }
}

fn hhmm(minute: u16) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

fn parse_hhmm(text: &str) -> Option<u16> {
    let (h, m) = text.split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

fn wait_text(wait: Wait) -> &'static str {
    match wait {
        Wait::OutsideWindow => "waiting for the update window",
        Wait::Locked => "waiting until the device is unlocked",
        Wait::DiskLow => "waiting until the disk has more space",
    }
}

fn verdict_text(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Install => "installs in the next run".to_owned(),
        Verdict::NotAutomatic => "waits for you".to_owned(),
        Verdict::Waiting(wait) => wait_text(*wait).to_owned(),
        Verdict::NeedsYou(blockers) => {
            let reasons: Vec<String> = blockers
                .iter()
                .map(|b| match b {
                    auto::Blocker::NewPermissions => "asks for new permissions".to_owned(),
                    auto::Blocker::KeyChanged => "the feed's key changed".to_owned(),
                    auto::Blocker::FailedChecks(r) => format!("failed the install checks: {r}"),
                    auto::Blocker::Held => "this version is on hold".to_owned(),
                    auto::Blocker::RestartNeeded => "needs a system restart".to_owned(),
                })
                .collect();
            format!("waits for you: {}", reasons.join(", "))
        }
    }
}

fn event(e: &auto::Event) -> UpdateEvent {
    let (package, from, to, kind) = match &e.outcome {
        auto::Outcome::Updated { package, from, to } => (package, from, to, UpdateEventKind::Updated),
        auto::Outcome::RolledBack {
            package,
            from,
            to,
            reason,
        } => (
            package,
            from,
            to,
            UpdateEventKind::RolledBack {
                reason: reason.clone(),
            },
        ),
        auto::Outcome::RollbackFailed {
            package,
            from,
            to,
            reason,
            rollback_error,
        } => (
            package,
            from,
            to,
            UpdateEventKind::RollbackFailed {
                reason: reason.clone(),
                rollback_error: rollback_error.clone(),
            },
        ),
        auto::Outcome::Postponed { package, wait } => (
            package,
            &String::new(),
            &String::new(),
            UpdateEventKind::Postponed {
                wait: wait_text(*wait).to_owned(),
            },
        ),
    };
    UpdateEvent {
        at: e.at,
        package: package.clone(),
        from: from.clone(),
        to: to.clone(),
        kind,
    }
}

fn result_of(outcome: &auto::Outcome, log: Vec<String>) -> UpdateResult {
    let e = event(&auto::Event {
        at: 0,
        outcome: outcome.clone(),
    });
    let outcome = match e.kind {
        UpdateEventKind::Updated => UpdateOutcome::Updated,
        UpdateEventKind::RolledBack { reason } => UpdateOutcome::RolledBack {
            reason,
            recovered: true,
            rollback_error: None,
        },
        UpdateEventKind::RollbackFailed {
            reason,
            rollback_error,
        } => UpdateOutcome::RolledBack {
            reason,
            recovered: false,
            rollback_error: Some(rollback_error),
        },
        UpdateEventKind::Postponed { wait } => UpdateOutcome::Postponed(wait),
    };
    UpdateResult {
        package: e.package,
        from: e.from,
        to: e.to,
        outcome,
        log,
    }
}

/// The same bookkeeping `nodo_update::auto::run` does, for updates the user started.
fn record(state: &mut UpdateState, at: u64, outcome: auto::Outcome) {
    let event = auto::Event { at, outcome };
    if matches!(
        event.outcome,
        auto::Outcome::RolledBack { .. } | auto::Outcome::RollbackFailed { .. }
    ) {
        state.attention.push(event.clone());
    }
    state.history.push(event);
    if state.history.len() > HISTORY_LEN {
        let excess = state.history.len() - HISTORY_LEN;
        state.history.drain(..excess);
    }
}

/// Runs one update on the device. Holds what a rollback needs between the calls.
struct Updater<'a, S: System> {
    helper: &'a Helper<S>,
    /// Approvals given in this request for updates that ask for more.
    approvals: BTreeMap<String, Permissions>,
    /// The previous version's kept file, by package.
    previous: BTreeMap<String, PathBuf>,
    /// The last log lines of a failed package, for the report.
    logs: BTreeMap<String, Vec<String>>,
}

fn fail(e: impl std::fmt::Display) -> Failure {
    Failure(e.to_string())
}

impl<S: System> Updater<'_, S> {
    fn data_copy(&self, id: &str, encrypted: bool) -> Result<(PathBuf, PathBuf, Option<&'static str>), Failure> {
        let h = self.helper;
        if encrypted {
            let mount = PathBuf::from(volume_mount(id).map_err(fail)?);
            Ok((mount.clone(), mount.join(IN_VOLUME_COPY), Some(IN_VOLUME_COPY)))
        } else {
            let layout = Layout::new(id, Profile::App).map_err(fail)?;
            Ok((PathBuf::from(layout.data_dir), h.manage.paths.rollback.join(id), None))
        }
    }

    fn stop(&self, id: &str, timeout: u32) -> Result<(), Failure> {
        self.helper
            .sys
            .sv(SvAction::ForceStop, id, timeout.saturating_add(5))
            .map_err(fail)
    }

    fn healthy(&mut self, id: &str) -> Result<(), Failure> {
        let h = self.helper;
        let package = h.package(id).map_err(fail)?;
        h.sys.sv(SvAction::Start, id, 30).map_err(fail)?;
        let probe = match package.manifest.health.as_ref().map(crate::health::probe_for) {
            Some(Ok(probe)) => Some(probe),
            Some(Err(e)) => return Err(Failure(e)),
            None => None,
        };
        h.wait_healthy(id, probe.as_ref()).map_err(|reason| {
            if let Ok(Response::Log(chunk)) = h.read_log(id, 8) {
                self.logs.insert(id.to_owned(), chunk.lines);
            }
            Failure(format!("{id} did not pass its health check: {reason}"))
        })
    }

    fn installed_manifest(&self, id: &str) -> Result<nodo_manifest::Manifest, Failure> {
        let h = self.helper;
        let path = h.app_dir(id).map_err(fail)?.join(nodo_feed::MANIFEST_FILE);
        let bytes = h
            .sys
            .read_file(&path)
            .map_err(fail)?
            .ok_or_else(|| Failure(format!("{} is missing", path.display())))?;
        let text = String::from_utf8(bytes).map_err(|_| Failure("manifest is not UTF-8".into()))?;
        let manifest = nodo_manifest::Manifest::parse(&text).map_err(fail)?;
        if manifest.id != id {
            return Err(Failure("the manifest names another package".into()));
        }
        Ok(manifest)
    }
}

impl<S: System> Installer for Updater<'_, S> {
    fn install(&mut self, c: &Candidate) -> Result<(), Failure> {
        let h = self.helper;
        let id = c.package.as_str();
        let state = h.read_feeds().map_err(fail)?;
        let record = state
            .installed
            .get(id)
            .ok_or_else(|| Failure(format!("{id} was not installed from a feed")))?;
        let feed = state.feed(&record.feed).map_err(fail)?;
        let previous = record
            .kept
            .clone()
            .filter(|p| h.manage.packages.kept_exists(p))
            .ok_or_else(|| Failure(format!("there is no copy of {id} {} to roll back to", c.from)))?;
        let approved = match self.approvals.get(id) {
            Some(p) => p.clone(),
            None => state
                .approved
                .get(id)
                .cloned()
                .ok_or_else(|| Failure(format!("{id} has no approved permissions")))?,
        };
        let package = h.package(id).map_err(fail)?;
        let encrypted = package.manifest.storage.encrypted;
        let (data, copy, skip) = self.data_copy(id, encrypted)?;
        if encrypted && !h.sys.is_mounted(&data) {
            return Err(Failure(format!("{id}'s encrypted storage is locked")));
        }
        let size = h.manage.packages.tree_size(&data);
        let free = h.manage.packages.free_bytes(&data);
        if size.saturating_add(COPY_MARGIN) > free {
            return Err(Failure(format!(
                "there is not enough free space to keep a copy of {id}'s data for a rollback"
            )));
        }

        // From here on a failure is rolled back, which also starts the service again.
        self.previous.insert(id.to_owned(), previous);
        self.stop(id, package.manifest.stop_timeout_secs)?;
        h.manage.packages.remove_tree(&copy).map_err(Failure)?;
        if h.sys.exists(&data) || h.manage.packages.tree_size(&data) > 0 {
            h.manage.packages.copy_tree(&data, &copy, skip).map_err(Failure)?;
        }

        let mut pins = state.pins.clone();
        let pkgver = format!("{id}-{}", c.to);
        let installed = h.manage.packages.install(
            &mut pins,
            &feed.address,
            id,
            &approved,
            &pkgver,
            &h.kept_dir(id),
        );
        let (permissions, kept) = match installed {
            Ok((Outcome::Installed { permissions, .. }, kept)) => (permissions, kept),
            Ok((Outcome::NeedsApproval { .. }, _)) => {
                return Err(Failure(format!("{pkgver} asks for permissions that were not approved")))
            }
            Ok((Outcome::Blocked(reasons), _)) => {
                let text: Vec<String> = reasons.iter().map(block_text).collect();
                return Err(Failure(text.join("; ")));
            }
            Err(e) => return Err(fail(e)),
        };
        let version = c.to.clone();
        h.with_feeds(|state| {
            state.pins = pins.clone();
            state.approved.insert(id.to_owned(), permissions.clone());
            if let Some(record) = state.installed.get_mut(id) {
                record.version = version.clone();
                record.kept = kept.clone();
            }
            Ok(())
        })
        .map_err(fail)?;
        let manifest = self.installed_manifest(id)?;
        h.place_manifest(id, &h.app_dir(id).map_err(fail)?, &manifest)
            .map_err(Failure)?;
        h.install_by_id(id, None).map_err(fail)?;
        Ok(())
    }

    fn health_check(&mut self, c: &Candidate) -> Result<(), Failure> {
        self.healthy(&c.package)?;
        let h = self.helper;
        let package = h.package(&c.package).map_err(fail)?;
        let (_, copy, _) = self.data_copy(&c.package, package.manifest.storage.encrypted)?;
        let _ = h.manage.packages.remove_tree(&copy);
        if let Some(previous) = self.previous.get(&c.package) {
            let current = h.read_feeds().ok().and_then(|s| s.installed.get(&c.package).and_then(|r| r.kept.clone()));
            if current.as_ref() != Some(previous) {
                h.manage.packages.remove_kept(previous);
            }
        }
        Ok(())
    }

    fn rollback(&mut self, c: &Candidate) -> Result<(), Failure> {
        let h = self.helper;
        let id = c.package.as_str();
        // Refused before anything changed: the old version is still running untouched.
        let Some(previous) = self.previous.get(id).cloned() else {
            return Ok(());
        };
        let timeout = h.package(id).map(|p| p.manifest.stop_timeout_secs).unwrap_or(30);
        let _ = self.stop(id, timeout);
        h.manage.packages.reinstall(&previous).map_err(fail)?;
        let manifest = self.installed_manifest(id)?;
        h.place_manifest(id, &h.app_dir(id).map_err(fail)?, &manifest)
            .map_err(Failure)?;
        let from = c.from.clone();
        h.with_feeds(|state| {
            if let Some(record) = state.installed.get_mut(id) {
                record.version = from.clone();
                record.kept = Some(previous.clone());
            }
            Ok(())
        })
        .map_err(fail)?;
        h.install_by_id(id, None).map_err(fail)?;
        let _ = self.stop(id, manifest.stop_timeout_secs);
        let (data, copy, skip) = self.data_copy(id, manifest.storage.encrypted)?;
        if h.sys.exists(&copy) || h.manage.packages.tree_size(&copy) > 0 {
            h.manage.packages.restore_tree(&copy, &data, skip).map_err(Failure)?;
            let _ = h.manage.packages.remove_tree(&copy);
        }
        self.healthy(id)
            .map_err(|Failure(reason)| Failure(format!("{} came back but is not healthy: {reason}", c.from)))
    }
}

struct Live<'a, S: System>(&'a Helper<S>);

impl<S: System> Device for Live<'_, S> {
    fn conditions(&self) -> Conditions {
        let h = self.0;
        Conditions {
            locked: h.device_locked(),
            disk_low: h.installs_refused().unwrap_or(true),
            minute_of_day: h.manage.packages.local_minute(),
        }
    }
}

impl<S: System> Helper<S> {
    fn updates_path(&self, name: &str) -> PathBuf {
        self.manage.paths.updates.join(name)
    }

    fn policy(&self) -> Result<AutoPolicy, ApiError> {
        self.load_state(&self.updates_path("policy.toml"))
    }

    fn update_state(&self) -> Result<UpdateState, ApiError> {
        self.load_state(&self.updates_path("state.toml"))
    }

    fn save_update_state(&self, state: &UpdateState) -> Result<(), ApiError> {
        self.save_state(&self.updates_path("state.toml"), state)
    }

    fn candidates(&self) -> Result<Vec<CandidateRecord>, ApiError> {
        Ok(self
            .load_state::<Candidates>(&self.updates_path("candidates.toml"))?
            .list)
    }

    fn save_candidates(&self, list: Vec<CandidateRecord>) -> Result<(), ApiError> {
        self.save_state(&self.updates_path("candidates.toml"), &Candidates { list })
    }

    /// Locked while the device key exists and an installed package's volume is not open.
    pub(crate) fn device_locked(&self) -> bool {
        if !self.sys.exists(&self.paths.keystore) {
            return false;
        }
        let Ok((packages, _)) = self.packages() else {
            return true;
        };
        packages
            .iter()
            .filter(|p| p.manifest.storage.encrypted)
            .any(|p| match volume_mount(p.id()) {
                Ok(mount) => !self.sys.is_mounted(std::path::Path::new(&mount)),
                Err(_) => true,
            })
    }

    /// Refreshes the feeds, then inspects each newer version of every installed feed package.
    pub(crate) fn check_updates(&self) -> Result<(), ApiError> {
        self.refresh_feeds()?;
        let state = self.read_feeds()?;
        let mut list = Vec::new();
        for (id, installed) in &state.installed {
            let Some(feed) = state.feeds.get(&installed.feed).filter(|f| f.enabled) else {
                continue;
            };
            let node = self
                .package(id)
                .map(|p| p.manifest.provides.iter().any(|r| r == NODE_ROLE))
                .unwrap_or(false);
            let index = if feed.offered_key.is_some() {
                &feed.offered_index
            } else {
                &feed.index
            };
            let Some(entry) = index.iter().find(|e| e.name == *id) else {
                continue;
            };
            if compare_versions(&entry.version, &installed.version) != Some(std::cmp::Ordering::Greater) {
                continue;
            }
            let mut record = CandidateRecord {
                package: id.clone(),
                feed: installed.feed.clone(),
                from: installed.version.clone(),
                to: entry.version.clone(),
                node,
                added: Vec::new(),
                key_changed: feed.offered_key.is_some(),
                checks_failed: None,
            };
            if !record.key_changed {
                let mut pins = state.pins.clone();
                let approved = state.approved.get(id);
                match self.manage.packages.review(&mut pins, &feed.address, id, approved) {
                    Ok(Reviewed::Ready { added, pkgver, .. }) => {
                        record.added = added.iter().map(ToString::to_string).collect();
                        if pkgver != record.pkgver() {
                            record.checks_failed = Some(format!("the feed now offers {pkgver}"));
                        }
                    }
                    Ok(Reviewed::Blocked(reasons)) => {
                        record.key_changed = reasons.iter().any(|r| matches!(r, BlockReason::KeyChanged { .. }));
                        if !record.key_changed {
                            let text: Vec<String> = reasons.iter().map(block_text).collect();
                            record.checks_failed = Some(text.join("; "));
                        }
                    }
                    Err(e) => record.checks_failed = Some(e.to_string()),
                }
            }
            list.push(record);
        }
        self.save_candidates(list)?;
        let _guard = lock(&self.manage.state);
        let mut updates = self.update_state()?;
        updates.last_check = Some(self.manage.packages.now());
        self.save_update_state(&updates)
    }

    pub(crate) fn list_updates(&self) -> Result<Response, ApiError> {
        let updates = self.update_state()?;
        Ok(Response::Updates(
            self.candidates()?
                .iter()
                .map(|c| self.update_info(c, &updates))
                .collect(),
        ))
    }

    fn update_info(&self, c: &CandidateRecord, updates: &UpdateState) -> UpdateInfo {
        UpdateInfo {
            package: c.package.clone(),
            from: c.from.clone(),
            to: c.to.clone(),
            feed: c.feed.clone(),
            held: updates.holds.is_held(&c.package, &c.to),
            needs_new_permissions: !c.added.is_empty(),
            needs_restart: false,
        }
    }

    pub(crate) fn update_status(&self) -> Result<Response, ApiError> {
        let policy = self.policy()?;
        let updates = self.update_state()?;
        let conditions = Live(self).conditions();
        let candidates = self
            .candidates()?
            .iter()
            .map(|c| {
                let verdict = auto::decide(&policy, &updates.holds, &c.candidate(), &conditions);
                UpdateCandidate {
                    info: self.update_info(c, &updates),
                    added: c.added.clone(),
                    key_changed: c.key_changed,
                    checks_failed: c.checks_failed.clone(),
                    automatic: policy.is_automatic(&c.package),
                    verdict: verdict_text(&verdict),
                }
            })
            .collect();
        Ok(Response::UpdateStatus(UpdateStatus {
            candidates,
            last_check: updates.last_check,
            attention: updates.attention.iter().map(event).collect(),
            history: updates.history.iter().map(event).collect(),
            next_window: (!policy.packages.is_empty()).then(|| hhmm(policy.schedule.window.start())),
        }))
    }

    pub(crate) fn review_update(&self, package: &str) -> Result<Response, ApiError> {
        let candidate = self
            .candidates()?
            .into_iter()
            .find(|c| c.package == package)
            .ok_or_else(|| ApiError::NotFound(format!("no update for {package}")))?;
        let state = self.read_feeds()?;
        let feed = state.feed(&candidate.feed)?;
        let mut pins = state.pins.clone();
        match self
            .manage
            .packages
            .review(&mut pins, &feed.address, package, state.approved.get(package))
        {
            Ok(Reviewed::Ready {
                pkgver,
                permissions,
                added,
                manifest,
            }) => Ok(Response::InstallReview(InstallReview {
                feed: candidate.feed.clone(),
                package: package.to_owned(),
                pkgver,
                permissions: permissions.summary(),
                added: added.iter().map(ToString::to_string).collect(),
                first_install: false,
                encrypted_storage: manifest.storage.encrypted,
                needs_passphrase: false,
            })),
            Ok(Reviewed::Blocked(reasons)) => Ok(crate::apps::blocked(package, &reasons)),
            Err(e) => crate::apps::refusal(package, &candidate.to, e),
        }
    }

    /// Why one selected update cannot go ahead, or the permissions it was approved with.
    fn check_selected(
        &self,
        selected: &UpdateApproval,
        candidate: Option<&CandidateRecord>,
        updates: &UpdateState,
    ) -> Result<Option<Permissions>, String> {
        let c = candidate.ok_or("there is no update for it. Check for updates again.")?;
        if c.to != selected.to {
            return Err(format!("the update is now to {}. Check it again.", c.to));
        }
        if updates.holds.is_held(&c.package, &c.to) {
            return Err("this version is on hold. Release it first.".into());
        }
        if c.key_changed {
            return Err("the feed's signing key changed. Decide about the key first.".into());
        }
        if let Some(reason) = &c.checks_failed {
            return Err(format!("it failed the install checks: {reason}"));
        }
        if c.added.is_empty() {
            return Ok(None);
        }
        let approval: &PermissionApproval = selected
            .approval
            .as_ref()
            .ok_or("it asks for new permissions, which need your approval")?;
        let state = self.read_feeds().map_err(|e| e.to_string())?;
        let feed = state.feed(&c.feed).map_err(|e| e.to_string())?;
        let mut pins = state.pins.clone();
        match self
            .manage
            .packages
            .review(&mut pins, &feed.address, &c.package, state.approved.get(&c.package))
        {
            Ok(Reviewed::Ready {
                pkgver,
                permissions,
                ..
            }) if pkgver == approval.pkgver
                && pkgver == c.pkgver()
                && permissions.summary() == approval.permissions =>
            {
                Ok(Some(permissions))
            }
            Ok(_) => Err("it asks for something other than what you approved. Review it again.".into()),
            Err(e @ InstallError::Rejected(_)) => Err(e.to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub(crate) fn apply_updates(&self, selected: &[UpdateApproval]) -> Result<Response, ApiError> {
        let _work = lock(&self.manage.work);
        let candidates = self.candidates()?;
        let mut updates = self.update_state()?;
        let mut results = Vec::new();
        let mut updater = Updater {
            helper: self,
            approvals: BTreeMap::new(),
            previous: BTreeMap::new(),
            logs: BTreeMap::new(),
        };
        let disk_low = self.installs_refused()?;
        let mut run = Vec::new();
        for s in selected {
            let c = candidates.iter().find(|c| c.package == s.package);
            let refused = if disk_low {
                Err("the system disk is low on space, so updates wait".to_owned())
            } else {
                self.check_selected(s, c, &updates)
            };
            let refused = refused.and_then(|a| c.map(|c| (a, c)).ok_or_else(String::new));
            match refused {
                Ok((approval, c)) => {
                    if let Some(p) = approval {
                        updater.approvals.insert(c.package.clone(), p);
                    }
                    run.push(c.clone());
                }
                Err(reason) => results.push(UpdateResult {
                    package: s.package.clone(),
                    from: c.map(|c| c.from.clone()).unwrap_or_default(),
                    to: s.to.clone(),
                    outcome: UpdateOutcome::Refused(reason),
                    log: Vec::new(),
                }),
            }
        }
        // The node restarts last, so its dependents restart once.
        run.sort_by_key(|c| c.node);
        let now = self.manage.packages.now();
        for c in &run {
            let candidate = c.candidate();
            let outcome = apply_one(&mut updater, &candidate);
            if !matches!(outcome, auto::Outcome::Updated { .. }) {
                updates.holds.hold(&c.package, &c.to);
            }
            record(&mut updates, now, outcome.clone());
            let log = updater.logs.remove(&c.package).unwrap_or_default();
            results.push(result_of(&outcome, log));
        }
        {
            let _guard = lock(&self.manage.state);
            self.save_update_state(&updates)?;
        }
        self.forget_applied(&results)?;
        let _ = self.refresh_health_config();
        Ok(Response::UpdateResults(results))
    }

    /// Updated packages are no longer candidates.
    fn forget_applied(&self, results: &[UpdateResult]) -> Result<(), ApiError> {
        let done: Vec<&str> = results
            .iter()
            .filter(|r| r.outcome == UpdateOutcome::Updated)
            .map(|r| r.package.as_str())
            .collect();
        if done.is_empty() {
            return Ok(());
        }
        let list = self
            .candidates()?
            .into_iter()
            .filter(|c| !done.contains(&c.package.as_str()))
            .collect();
        self.save_candidates(list)
    }

    pub(crate) fn set_hold(&self, package: &str, version: &str, held: bool) -> Result<Response, ApiError> {
        let _guard = lock(&self.manage.state);
        let mut updates = self.update_state()?;
        if held {
            updates.holds.hold(package, version);
        } else {
            updates.holds.release(package, version);
        }
        self.save_update_state(&updates)?;
        Ok(Response::Ok)
    }

    pub(crate) fn dismiss_update(&self, package: &str) -> Result<Response, ApiError> {
        let _guard = lock(&self.manage.state);
        let mut updates = self.update_state()?;
        updates.dismiss(package);
        self.save_update_state(&updates)?;
        Ok(Response::Ok)
    }

    pub(crate) fn get_auto_update(&self) -> Result<Response, ApiError> {
        let policy = self.policy()?;
        Ok(Response::AutoUpdate(AutoUpdatePrefs {
            frequency: frequency_name(policy.schedule.check).to_owned(),
            window_start: hhmm(policy.schedule.window.start()),
            window_end: hhmm(policy.schedule.window.end()),
            enabled: policy.packages.into_iter().collect(),
        }))
    }

    /// Only packages installed from a feed can be automatic. Anything left out is off.
    pub(crate) fn set_auto_update(&self, prefs: &AutoUpdatePrefs) -> Result<Response, ApiError> {
        let check = match prefs.frequency.to_ascii_lowercase().as_str() {
            "daily" => CheckFrequency::Daily,
            "weekly" => CheckFrequency::Weekly,
            "manual" => CheckFrequency::Manual,
            other => return Err(ApiError::Failed(format!("unknown check frequency {other}"))),
        };
        let window = match (parse_hhmm(&prefs.window_start), parse_hhmm(&prefs.window_end)) {
            (Some(start), Some(end)) => Window::new(start, end),
            _ => None,
        }
        .ok_or_else(|| ApiError::Failed("the window needs two different HH:MM times".into()))?;
        let installed = self.read_feeds()?.installed;
        let mut policy = AutoPolicy {
            schedule: Schedule { check, window },
            packages: Default::default(),
        };
        for id in &prefs.enabled {
            if !installed.contains_key(id) {
                return Err(ApiError::Failed(format!(
                    "{id} was not installed from a feed, so it cannot update automatically"
                )));
            }
            policy.set(id, true);
        }
        let _guard = lock(&self.manage.state);
        self.save_state(&self.updates_path("policy.toml"), &policy)?;
        Ok(Response::Ok)
    }

    /// One scheduled run: checks when due, then installs what the policy allows.
    pub(crate) fn run_auto_update(&self) -> Result<Response, ApiError> {
        let policy = self.policy()?;
        let now = self.manage.packages.now();
        let due = policy
            .schedule
            .check_due(self.update_state()?.last_check, now);
        if due && !policy.packages.is_empty() {
            self.check_updates()?;
        }
        let _work = lock(&self.manage.work);
        let mut updates = self.update_state()?;
        let candidates: Vec<Candidate> = self.candidates()?.iter().map(CandidateRecord::candidate).collect();
        let mut updater = Updater {
            helper: self,
            approvals: BTreeMap::new(),
            previous: BTreeMap::new(),
            logs: BTreeMap::new(),
        };
        let outcomes = auto::run(&policy, &mut updates, &candidates, &Live(self), &mut updater, now);
        {
            let _guard = lock(&self.manage.state);
            self.save_update_state(&updates)?;
        }
        let results: Vec<UpdateResult> = outcomes
            .iter()
            .map(|o| {
                let log = match o {
                    auto::Outcome::RolledBack { package, .. } | auto::Outcome::RollbackFailed { package, .. } => {
                        updater.logs.remove(package).unwrap_or_default()
                    }
                    _ => Vec::new(),
                };
                result_of(o, log)
            })
            .collect();
        self.forget_applied(&results)?;
        if !results.is_empty() {
            let _ = self.refresh_health_config();
        }
        Ok(Response::UpdateResults(results))
    }
}

fn apply_one<S: System>(updater: &mut Updater<'_, S>, c: &Candidate) -> auto::Outcome {
    let failure = match updater.install(c).and_then(|()| updater.health_check(c)) {
        Ok(()) => {
            return auto::Outcome::Updated {
                package: c.package.clone(),
                from: c.from.clone(),
                to: c.to.clone(),
            }
        }
        Err(Failure(reason)) => reason,
    };
    match updater.rollback(c) {
        Ok(()) => auto::Outcome::RolledBack {
            package: c.package.clone(),
            from: c.from.clone(),
            to: c.to.clone(),
            reason: failure,
        },
        Err(Failure(rollback_error)) => auto::Outcome::RollbackFailed {
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
    use super::*;

    #[test]
    fn times_are_device_hh_mm() {
        assert_eq!(parse_hhmm("03:00"), Some(180));
        assert_eq!(parse_hhmm("23:59"), Some(1439));
        for bad in ["24:00", "3:00", "03:60", "ab:cd", ""] {
            assert_eq!(parse_hhmm(bad), None, "{bad}");
        }
        assert_eq!(hhmm(185), "03:05");
    }
}
