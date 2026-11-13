//! Feeds, installs, updates, health and backups through the helper.
//!
//! Kept apart from `helper_backend`, which only forwards to these functions. Every approval and
//! typed confirmation the user gives is passed through unchanged, because the helper checks them
//! again and refuses anything else.

use std::time::{SystemTime, UNIX_EPOCH};

use nodo_api::{
    ApiError, BackupContents, BackupCreated, BackupInfo, CatalogEntry, HealthSeverity, HealthSummary, InstallReview,
    KeyChangeConfirmation, PermissionApproval, Request, Response, RestoreReport, SystemUpdateResult,
    SystemUpdateStatus, UpdateApproval, UpdateEventKind, UpdateOutcome, UpdateStatus,
};

use crate::helper_backend::HelperBackend;
use crate::model::*;
use crate::secret::Secret;

/// What the last answers said, for requests that need it again.
#[derive(Default)]
pub struct Cache {
    pub status: Option<UpdateStatus>,
    /// The system snapshot status, whose available serial an update quotes back.
    pub system: Option<SystemUpdateStatus>,
    /// Updates that grow encrypted storage, by package, in words.
    pub growth: Vec<(String, String)>,
}

fn api(secret: &Secret) -> nodo_api::Secret {
    nodo_api::Secret::new(secret.expose())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn ago(at: u64) -> String {
    match now().saturating_sub(at) {
        s if s < 60 => "just now".to_owned(),
        s if s < 3600 => format!("{} ago", counted(s / 60, "minute")),
        s if s < 2 * 86400 => format!("{} ago", counted(s / 3600, "hour")),
        s => format!("{} ago", counted(s / 86400, "day")),
    }
}

fn size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1e6)
    } else {
        format!("{} KB", bytes.div_ceil(1000))
    }
}

/// `version_revision` without the package name, from a `pkgver`.
fn version_of(pkgver: &str, name: &str) -> String {
    pkgver.strip_prefix(&format!("{name}-")).unwrap_or(pkgver).to_owned()
}

fn text(result: Result<Response, ApiError>) -> Result<Response, String> {
    result.map_err(|e| e.to_string())
}

pub fn catalog_entries(b: &HelperBackend) -> Result<Vec<CatalogEntry>, ApiError> {
    match b.ask(Request::Catalog)? {
        Response::Catalog(list) => Ok(list),
        _ => Ok(Vec::new()),
    }
}

/// Whether an enabled feed was never read: a default feed on a new device, before the helper's
/// own first read has finished.
pub fn unread_feeds(list: &[nodo_api::FeedInfo]) -> bool {
    list.iter().any(|f| f.enabled && f.last_checked.is_none())
}

/// Installed packages first, as the helper knows them, then what the feeds offer. A feed that
/// was never read is read first, so a new device does not show an empty list.
pub fn catalog(b: &HelperBackend, installed: Vec<nodo_api::PackageInfo>) -> Vec<PackageSummary> {
    if let Ok(Response::Feeds(list)) = b.ask(Request::ListFeeds) {
        if unread_feeds(&list) {
            let _ = b.ask(Request::RefreshFeeds);
        }
    }
    let entries = match catalog_entries(b) {
        Ok(entries) => entries,
        Err(e) => {
            b.missing(Area::Catalog, &e);
            Vec::new()
        }
    };
    let mut out: Vec<PackageSummary> = Vec::new();
    for p in installed {
        let from_feed = entries.iter().find(|e| e.name == p.id && e.installed.is_some());
        let feed = match (&p.origin, from_feed) {
            (_, Some(e)) => e.feed.clone(),
            (nodo_api::Origin::System, None) => "system".to_owned(),
            (nodo_api::Origin::Feed(Some(f)), None) => f.clone(),
            (nodo_api::Origin::Feed(None), None) => "app".to_owned(),
        };
        let status = match from_feed {
            Some(e) if e.installed.as_deref() != Some(e.version.as_str()) => PackageStatus::UpdateReady,
            _ => PackageStatus::Installed,
        };
        out.push(PackageSummary {
            id: PackageId::new(&feed, &p.id),
            status,
            version: from_feed.and_then(|e| e.installed.clone()).unwrap_or(p.version),
            size: from_feed.map(|e| size(e.size_bytes)).unwrap_or_default(),
            tagline: if p.roles.is_empty() { String::new() } else { format!("Provides {}", p.roles.join(", ")) },
            description: Vec::new(),
            needs: p.needs.into_iter().map(|name| Need { name, met: true, source: None }).collect(),
        });
    }
    for e in entries {
        if out.iter().any(|p| p.id.name == e.name) {
            continue;
        }
        out.push(PackageSummary {
            id: PackageId::new(&e.feed, &e.name),
            status: PackageStatus::Available,
            version: e.version,
            size: size(e.size_bytes),
            tagline: format!("From the feed {}", e.feed),
            description: Vec::new(),
            needs: Vec::new(),
        });
    }
    out
}

/// A package the feeds offer and this device has not installed.
pub fn available_detail(b: &HelperBackend, id: &PackageId) -> Option<PackageDetail> {
    let entry = catalog_entries(b).ok()?.into_iter().find(|e| e.feed == id.feed && e.name == id.name)?;
    Some(PackageDetail {
        id: id.clone(),
        installed: false,
        signature_verified: false,
        about: format!(
            "Offered by the feed {}. Press i to download and check it. You then see everything it asks for before anything installs.",
            id.feed
        ),
        version: entry.version,
        size: size(entry.size_bytes),
        license: String::new(),
        needs: Vec::new(),
        public_settings: Vec::new(),
        asks: Vec::new(),
        encrypted_storage: false,
    })
}

fn ask_for(line: &str, new: bool) -> Ask {
    // The GPU opens the graphics driver to the package, a large part of the kernel.
    let hardware = line.contains("the GPU");
    let risky = ["internet", "accepts", "no longer keeps", "unencrypted", "provides"];
    let level =
        if hardware || risky.iter().any(|r| line.contains(r)) { AskLevel::Caution } else { AskLevel::Normal };
    let mut text = line.to_owned();
    if let Some(first) = text.get(..1) {
        text = first.to_uppercase() + &text[1..];
    }
    let note = match (new, level) {
        (true, _) => "New".to_owned(),
        (false, AskLevel::Caution) if hardware => "Hardware access".to_owned(),
        (false, AskLevel::Caution) => "Broad access".to_owned(),
        _ => "Standard".to_owned(),
    };
    Ask { level, text, note }
}

/// The review screen's data. `only_added` shows just what an update adds, as the design does.
fn review_data(id: &PackageId, r: &InstallReview, only_added: bool) -> ReviewData {
    let mut asks: Vec<Ask> = r
        .permissions
        .iter()
        .filter(|line| !only_added || r.added.contains(line))
        .map(|line| ask_for(line, !r.first_install && r.added.contains(line)))
        .collect();
    if r.encrypted_storage && !only_added {
        asks.push(Ask {
            level: AskLevel::Encrypted,
            text: "Keep data in encrypted storage".to_owned(),
            note: "Locks at reboot".to_owned(),
        });
    }
    ReviewData {
        id: id.clone(),
        version: version_of(&r.pkgver, &r.package),
        third_party: true,
        asks,
        guarantees: vec![
            "Cannot read other packages' data or secrets".to_owned(),
            "Cannot run setup scripts as root or replace system packages".to_owned(),
        ],
        approval: Some(PermissionApproval { pkgver: r.pkgver.clone(), permissions: r.permissions.clone() }),
        needs_passphrase: r.needs_passphrase,
    }
}

fn rejection(id: &PackageId, version: String, reasons: Vec<String>, evidence: &str) -> InstallGate {
    InstallGate::Rejected(RejectionData {
        id: id.clone(),
        version,
        violations: reasons
            .into_iter()
            .map(|rule| Violation { passed: false, rule, evidence: evidence.to_owned() })
            .collect(),
    })
}

pub fn install_gate(b: &HelperBackend, id: &PackageId) -> InstallGate {
    match b.ask(Request::ReviewInstall { feed: id.feed.clone(), package: id.name.clone() }) {
        Ok(Response::InstallReview(r)) => InstallGate::Review(review_data(id, &r, false)),
        Ok(Response::InstallRejected { version, reasons, .. }) => rejection(id, version, reasons, ""),
        Ok(Response::InstallBlocked { reasons, key_changed, .. }) => {
            let evidence =
                if key_changed { "Open Feeds to decide about the new key." } else { "Nothing was installed." };
            rejection(id, String::new(), reasons, evidence)
        }
        Ok(other) => rejection(id, String::new(), vec![format!("unexpected reply: {other:?}")], ""),
        Err(e) => rejection(id, String::new(), vec![e.to_string()], "Nothing was downloaded or changed."),
    }
}

fn snapshot(id: &PackageId, report: nodo_api::InstallReport) -> InstallSnapshot {
    let steps = report
        .steps
        .into_iter()
        .map(|s| {
            let (state, note) = match s.state {
                nodo_api::StepState::Done => (StepState::Done, s.note),
                nodo_api::StepState::Skipped => (StepState::Done, s.note),
                nodo_api::StepState::Failed => (StepState::Failed, s.note),
                nodo_api::StepState::Pending => (StepState::Pending, s.note),
            };
            InstallStep { name: s.name, note, state }
        })
        .collect();
    InstallSnapshot {
        id: id.clone(),
        version: version_of(&report.pkgver, &id.name),
        steps,
        details: report.details,
        outcome: match report.problem {
            None => InstallOutcome::Succeeded,
            Some(problem) => InstallOutcome::Failed(problem),
        },
    }
}

pub fn start_install(
    b: &HelperBackend,
    id: &PackageId,
    approval: Option<&PermissionApproval>,
    passphrase: Option<&Secret>,
) -> Result<InstallSnapshot, String> {
    let approval = approval.ok_or("Review what the package asks for first.")?.clone();
    let request = Request::InstallPackage {
        feed: id.feed.clone(),
        package: id.name.clone(),
        approval,
        passphrase: passphrase.map(api),
    };
    match text(b.ask(request))? {
        Response::InstallReport(report) => Ok(snapshot(id, report)),
        Response::InstallRejected { reasons, .. } | Response::InstallBlocked { reasons, .. } => {
            Err(format!("Not installed: {}", reasons.join("; ")))
        }
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

pub fn feeds(b: &HelperBackend) -> Vec<Feed> {
    let list = match b.ask(Request::ListFeeds) {
        Ok(Response::Feeds(list)) => list,
        Ok(_) => return Vec::new(),
        Err(e) => {
            b.missing(Area::Feeds, &e);
            return Vec::new();
        }
    };
    let entries = catalog_entries(b).unwrap_or_default();
    list.into_iter()
        .map(|f| {
            let installed_from =
                entries.iter().filter(|e| e.feed == f.name && e.installed.is_some()).map(|e| e.name.clone()).collect();
            let checked = f.last_checked.as_deref().and_then(|t| t.parse().ok()).map(ago).unwrap_or_default();
            Feed {
                packages: f.packages,
                key_state: if f.key_changed { KeyState::Changed } else { KeyState::Pinned },
                checked,
                status: match (f.enabled, f.key_changed, f.problem) {
                    (false, _, _) => FeedStatus::Disabled,
                    (true, true, _) => FeedStatus::Blocked,
                    (true, false, Some(problem)) => FeedStatus::Unreachable(problem),
                    (true, false, None) => FeedStatus::Healthy,
                },
                address: f.address,
                fingerprint: f.fingerprint,
                added: String::new(),
                installed_from,
                name: f.name,
            }
        })
        .collect()
}

pub fn probe_feed(b: &HelperBackend, address: &str) -> Result<FeedProbe, String> {
    match text(b.ask(Request::ProbeFeed { address: address.to_owned() }))? {
        Response::FeedProbe(p) => Ok(FeedProbe {
            name: p.name,
            address: p.address,
            packages: p.packages,
            signed: true,
            fingerprint: p.fingerprint,
        }),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

/// The key the user compared is sent back, and the helper pins only that key.
pub fn trust_feed(b: &HelperBackend, probe: &FeedProbe) -> Result<(), String> {
    let request = Request::AddFeed { address: probe.address.clone(), fingerprint: probe.fingerprint.clone() };
    text(b.ask(request)).map(|_| ())
}

pub fn toggle_feed(b: &HelperBackend, name: &str) {
    if let Ok(Response::Feeds(list)) = b.ask(Request::ListFeeds) {
        if let Some(f) = list.iter().find(|f| f.name == name) {
            let _ = b.ask(Request::SetFeedEnabled { name: name.to_owned(), enabled: !f.enabled });
        }
    }
}

pub fn key_change(b: &HelperBackend, name: &str) -> Option<KeyChange> {
    let Ok(Response::FeedKeyChange(c)) = b.ask(Request::FeedKeyChange { name: name.to_owned() }) else {
        return None;
    };
    Some(KeyChange {
        feed: c.feed,
        trusted: c.pinned,
        now_signing: c.offered,
        affected: c
            .affected
            .into_iter()
            .map(|a| (format!("{} {}", a.package, a.version), "Keeps running. No updates.".to_owned()))
            .collect(),
    })
}

pub fn trust_new_key(b: &HelperBackend, change: &KeyChange, typed: &str) -> Result<(), String> {
    let confirmation = KeyChangeConfirmation {
        from: change.trusted.clone(),
        to: change.now_signing.clone(),
        typed_feed_name: typed.to_owned(),
    };
    text(b.ask(Request::TrustFeedKey { name: change.feed.clone(), confirmation })).map(|_| ())
}

fn load_status(b: &HelperBackend, request: Request) -> Option<UpdateStatus> {
    match b.ask(request) {
        Ok(Response::UpdateStatus(status)) => {
            b.manage.borrow_mut().status = Some(status.clone());
            Some(status)
        }
        Ok(_) => None,
        Err(e) => {
            b.missing(Area::Updates, &e);
            None
        }
    }
}

fn mb(bytes: u64) -> String {
    format!("{} MB", bytes >> 20)
}

/// What the helper says about system updates, kept for the apply that quotes its serial.
fn load_system(b: &HelperBackend, request: Request) -> Option<SystemUpdateStatus> {
    let status = match b.ask(request) {
        Ok(Response::SystemUpdate(status)) => Some(status),
        _ => None,
    };
    b.manage.borrow_mut().system = status.clone();
    status
}

/// The system snapshot as a row next to the packages, when a newer one is available.
fn system_row(status: &SystemUpdateStatus) -> Option<UpdateItem> {
    let available = status.available.as_ref()?;
    let from = status.current_snapshot.clone().unwrap_or_else(|| format!("serial {}", status.current_serial));
    let note = if available.withdraws_current {
        UpdateNote::Withdrawal(from.clone())
    } else if available.needs_restart {
        UpdateNote::RestartNeeded
    } else {
        UpdateNote::Ready
    };
    Some(UpdateItem {
        name: SYSTEM_BASE.to_owned(),
        from,
        to: available.snapshot.clone(),
        source: "tested snapshot".to_owned(),
        third_party: false,
        note,
        held: false,
        preselected: true,
        grows: None,
    })
}

/// The Updates screen's line about the system, or `None` when system updates are not set up.
pub fn system_note(b: &HelperBackend) -> Option<String> {
    let cache = b.manage.borrow();
    let status = cache.system.as_ref()?;
    if status.unavailable.is_some() {
        return None;
    }
    let current = status.current_snapshot.clone().unwrap_or_else(|| format!("serial {}", status.current_serial));
    let mut line = format!("System: snapshot {current}");
    if let Some(e) = &status.last_error {
        line.push_str(&format!(". The last check failed: {e}"));
    } else if let Some(at) = status.last_check {
        line.push_str(&format!(", checked {}", ago(at)));
    }
    line.push_str(if status.automatic {
        ". Automatic system updates are on."
    } else {
        ". Automatic system updates are off, so checks only notify."
    });
    Some(line)
}

pub fn updates(b: &HelperBackend) -> Vec<UpdateItem> {
    let system = load_system(b, Request::SystemUpdateStatus);
    let growth: Vec<(String, String)> = match b.ask(Request::UpdateStorageGrowth) {
        Ok(Response::StorageGrowth(list)) => list
            .into_iter()
            .map(|g| (g.package, format!("{} to {} of encrypted storage", mb(g.from_bytes), mb(g.to_bytes))))
            .collect(),
        _ => Vec::new(),
    };
    b.manage.borrow_mut().growth = growth.clone();
    let mut rows: Vec<UpdateItem> = system.as_ref().and_then(system_row).into_iter().collect();
    let Some(status) = load_status(b, Request::UpdateStatus) else {
        return rows;
    };
    let packages = status
        .candidates
        .into_iter()
        .map(|c| {
            let note = if c.key_changed {
                UpdateNote::Blocked("Feed signing key changed".to_owned())
            } else if let Some(reason) = c.checks_failed {
                UpdateNote::Blocked(format!("Fails install checks: {reason}"))
            } else if !c.added.is_empty() {
                UpdateNote::NewPermission(c.added.join(", "))
            } else if c.info.held {
                UpdateNote::Skipped
            } else {
                UpdateNote::Ready
            };
            let grows = growth.iter().find(|(name, _)| *name == c.info.package).map(|(_, words)| words.clone());
            UpdateItem {
                grows,
                name: c.info.package,
                from: c.info.from,
                to: c.info.to,
                source: c.info.feed,
                third_party: true,
                preselected: note == UpdateNote::Ready,
                note,
                held: c.info.held,
            }
        });
    rows.extend(packages);
    rows
}

pub fn check_updates(b: &HelperBackend) {
    load_status(b, Request::CheckUpdates);
    // A failed system check is recorded by the helper and shown in the system line.
    let _ = b.ask(Request::CheckSystemUpdate);
}

/// The system update the user selected, as a result row.
fn apply_system(b: &HelperBackend) -> UpdateResult {
    let serial = b.manage.borrow().system.as_ref().and_then(|s| s.available.as_ref()).map(|a| a.serial);
    let Some(serial) = serial else {
        return UpdateResult {
            name: SYSTEM_BASE.to_owned(),
            applied: false,
            version: String::new(),
            note: "There is no system update to apply. Check again.".to_owned(),
        };
    };
    match b.ask(Request::ApplySystemUpdate { serial }) {
        Ok(Response::SystemUpdateApplied(result)) => system_result(&result),
        Ok(other) => UpdateResult {
            name: SYSTEM_BASE.to_owned(),
            applied: false,
            version: String::new(),
            note: format!("unexpected reply: {other:?}"),
        },
        Err(e) => UpdateResult { name: SYSTEM_BASE.to_owned(), applied: false, version: String::new(), note: e.to_string() },
    }
}

pub fn system_result(result: &SystemUpdateResult) -> UpdateResult {
    let mut note = match result.changed.len() {
        0 => "Applied, nothing changed".to_owned(),
        1 => "Applied, 1 package changed".to_owned(),
        n => format!("Applied, {n} packages changed"),
    };
    if let Some(withdrawn) = &result.withdrew {
        note.push_str(&format!(", left withdrawn {withdrawn}"));
    }
    if result.needs_restart {
        note.push_str(", restart when ready");
    } else if !result.deferred.is_empty() {
        note.push_str(&format!(", {} uses it after a restart", result.deferred.join(", ")));
    }
    UpdateResult { name: SYSTEM_BASE.to_owned(), applied: true, version: result.snapshot.clone(), note }
}

fn candidate_to(b: &HelperBackend, name: &str) -> Option<String> {
    let cache = b.manage.borrow();
    let status = cache.status.as_ref()?;
    status.candidates.iter().find(|c| c.info.package == name).map(|c| c.info.to.clone())
}

pub fn update_review(b: &HelperBackend, name: &str) -> Option<ReviewData> {
    let Ok(Response::InstallReview(r)) = b.ask(Request::ReviewUpdate { package: name.to_owned() }) else {
        return None;
    };
    Some(review_data(&PackageId::new(&r.feed, name), &r, true))
}

/// Package updates first, then the system snapshot, so a package update never runs on a system
/// that changed under it within the same batch.
pub fn apply_updates(
    b: &HelperBackend,
    names: &[String],
    approvals: &[(String, PermissionApproval)],
    passphrase: Option<&Secret>,
) -> UpdateReport {
    let packages: Vec<String> = names.iter().filter(|n| *n != SYSTEM_BASE).cloned().collect();
    let mut report = if packages.is_empty() {
        UpdateReport { results: Vec::new(), failure: None }
    } else {
        apply_packages(b, &packages, approvals, passphrase)
    };
    if names.iter().any(|n| n == SYSTEM_BASE) {
        report.results.push(apply_system(b));
        report.results.sort_by_key(|r| !r.applied);
    }
    report
}

fn apply_packages(
    b: &HelperBackend,
    names: &[String],
    approvals: &[(String, PermissionApproval)],
    passphrase: Option<&Secret>,
) -> UpdateReport {
    let updates = names
        .iter()
        .map(|name| UpdateApproval {
            package: name.clone(),
            to: candidate_to(b, name).unwrap_or_default(),
            approval: approvals.iter().find(|(n, _)| n == name).map(|(_, a)| a.clone()),
        })
        .collect();
    let request = match passphrase {
        Some(p) => Request::ApplyUpdatesWithPassphrase { updates, passphrase: api(p) },
        None => Request::ApplyUpdates { updates },
    };
    let results = match b.ask(request) {
        Ok(Response::UpdateResults(results)) => results,
        Ok(other) => {
            return UpdateReport {
                results: vec![UpdateResult {
                    name: String::new(),
                    applied: false,
                    version: String::new(),
                    note: format!("unexpected reply: {other:?}"),
                }],
                failure: None,
            }
        }
        Err(e) => {
            let results = names
                .iter()
                .map(|n| UpdateResult { name: n.clone(), applied: false, version: String::new(), note: e.to_string() })
                .collect();
            return UpdateReport { results, failure: None };
        }
    };
    let mut report = UpdateReport { results: Vec::new(), failure: None };
    for r in results {
        let (applied, version, note) = match &r.outcome {
            UpdateOutcome::Updated => (true, r.to.clone(), "Applied".to_owned()),
            UpdateOutcome::RolledBack { .. } => (false, format!("still on {}", r.from), format!("{} rolled back", r.to)),
            UpdateOutcome::Refused(reason) | UpdateOutcome::Postponed(reason) => {
                (false, format!("still on {}", r.from), reason.clone())
            }
        };
        if let (UpdateOutcome::RolledBack { reason, recovered, rollback_error }, None) = (&r.outcome, &report.failure) {
            report.failure = Some(UpdateFailure {
                name: r.package.clone(),
                headline: format!("{} {} failed its health check", r.package, r.to),
                kept_version: r.from.clone(),
                restored: if *recovered {
                    format!("Version {} and its data were restored.", r.from)
                } else {
                    format!(
                        "Version {} did not come back: {}",
                        r.from,
                        rollback_error.clone().unwrap_or_default()
                    )
                },
                running: *recovered,
                log_line: r.log.last().cloned().unwrap_or_else(|| reason.clone()),
                verdict: "This looks like a package problem. Your settings were not the cause.".to_owned(),
            });
        }
        report.results.push(UpdateResult { name: r.package, applied, version, note });
    }
    report.results.sort_by_key(|r| !r.applied);
    report
}

pub fn set_held(b: &HelperBackend, name: &str, held: bool) {
    if let Some(version) = candidate_to(b, name) {
        let _ = b.ask(Request::SetHold { package: name.to_owned(), version, held });
    }
}

pub fn auto_update(b: &HelperBackend) -> AutoUpdatePrefs {
    let prefs = match b.ask(Request::GetAutoUpdate) {
        Ok(Response::AutoUpdate(prefs)) => prefs,
        Ok(_) => return AutoUpdatePrefs::default(),
        Err(e) => {
            b.missing(Area::AutoUpdate, &e);
            return AutoUpdatePrefs::default();
        }
    };
    let next_window = b.manage.borrow().status.as_ref().and_then(|s| s.next_window.clone());
    let mut packages: Vec<AutoUpdateEntry> = catalog_entries(b)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.installed.is_some())
        .map(|e| {
            let mut entry = AutoUpdateEntry::new(&e.name, &e.feed, true, "Off means notify only");
            entry.enabled = prefs.enabled.contains(&e.name);
            entry
        })
        .collect();
    // The system joins the list only where it can update, and stays off unless turned on.
    let system = b.manage.borrow().system.clone().filter(|s| s.unavailable.is_none());
    if let Some(system) = system {
        let mut entry = AutoUpdateEntry::new(SYSTEM_BASE, "tested snapshot", false, "Off means notify only");
        entry.enabled = system.automatic;
        packages.push(entry);
    }
    AutoUpdatePrefs {
        frequency: prefs.frequency,
        frequencies: vec!["Daily".to_owned(), "Weekly".to_owned(), "Manual".to_owned()],
        window_start: prefs.window_start,
        window_end: prefs.window_end,
        next_run: match next_window {
            Some(at) => format!("Next run at {at}"),
            None => "Auto-update is off".to_owned(),
        },
        packages,
    }
}

pub fn save_auto_update(b: &HelperBackend, prefs: &AutoUpdatePrefs) -> Result<(), String> {
    let request = Request::SetAutoUpdate(nodo_api::AutoUpdatePrefs {
        frequency: prefs.frequency.clone(),
        window_start: prefs.window_start.clone(),
        window_end: prefs.window_end.clone(),
        enabled: prefs.packages.iter().filter(|p| p.enabled && p.name != SYSTEM_BASE).map(|p| p.name.clone()).collect(),
    });
    text(b.ask(request))?;
    if let Some(system) = prefs.packages.iter().find(|p| p.name == SYSTEM_BASE) {
        text(b.ask(Request::SetSystemAutoUpdate { enabled: system.enabled }))?;
    }
    Ok(())
}

pub fn document(b: &HelperBackend, kind: &DocKind) -> Option<Document> {
    match kind {
        DocKind::AutoUpdateHistory => {
            let status = load_status(b, Request::UpdateStatus)?;
            let mut lines: Vec<String> = status
                .history
                .iter()
                .rev()
                .map(|e| {
                    let what = match &e.kind {
                        UpdateEventKind::Updated => format!("updated {} → {}", e.from, e.to),
                        UpdateEventKind::RolledBack { reason } => format!("{} rolled back: {reason}", e.to),
                        UpdateEventKind::RollbackFailed { reason, rollback_error } => {
                            format!("{} failed ({reason}) and {} did not come back: {rollback_error}", e.to, e.from)
                        }
                        UpdateEventKind::Postponed { wait } => format!("postponed, {wait}"),
                    };
                    format!("{}  {}  {what}", ago(e.at), e.package)
                })
                .collect();
            if lines.is_empty() {
                lines.push("No automatic updates have run yet.".to_owned());
            }
            Some(Document { title: "Auto-update history".to_owned(), lines })
        }
        DocKind::FeedPackages(feed) => {
            let lines = catalog_entries(b)
                .ok()?
                .into_iter()
                .filter(|e| &e.feed == feed)
                .map(|e| format!("{} {}  {}", e.name, e.version, size(e.size_bytes)))
                .collect();
            Some(Document { title: format!("Packages in {feed}"), lines })
        }
        _ => None,
    }
}

pub fn health(b: &HelperBackend) -> Option<HealthSummary> {
    match b.ask(Request::Health) {
        Ok(Response::Health(h)) => Some(h),
        _ => None,
    }
}

/// Home's lines from the health monitor, the boot notice and failed updates.
pub fn attention(health: Option<&HealthSummary>, status: Option<&UpdateStatus>) -> Vec<Attention> {
    let mut out = Vec::new();
    if let Some(h) = health {
        if let Some(text) = &h.boot_fallback {
            out.push(Attention {
                level: AttentionLevel::Warning,
                text: format!("The last system update did not start, so the previous system is running. {text}"),
            });
        }
        for note in &h.notes {
            let level = match note.severity {
                HealthSeverity::Critical => AttentionLevel::Critical,
                HealthSeverity::Warning => AttentionLevel::Warning,
                HealthSeverity::Info => AttentionLevel::Info,
            };
            out.push(Attention { level, text: note.message.clone() });
        }
    }
    for e in status.map(|s| s.attention.as_slice()).unwrap_or_default() {
        let (level, text) = match &e.kind {
            UpdateEventKind::RolledBack { .. } => (
                AttentionLevel::Warning,
                format!("{} {} failed, so {} is back and the update is on hold. See Updates.", e.package, e.to, e.from),
            ),
            UpdateEventKind::RollbackFailed { .. } => (
                AttentionLevel::Critical,
                format!("{} {} failed and {} did not come back. Press `l` to read its log.", e.package, e.to, e.from),
            ),
            _ => continue,
        };
        out.push(Attention { level, text });
    }
    out
}

/// Device rows from the health report, where it adds to what the overview already shows.
pub fn health_rows(health: Option<&HealthSummary>) -> Vec<(String, String)> {
    let Some(h) = health else { return Vec::new() };
    let mut rows = Vec::new();
    if let Some(t) = h.temperature_decic {
        rows.push(("Temp".to_owned(), format!("{:.0} °C", f64::from(t) / 10.0)));
    }
    if !h.stopped.is_empty() {
        rows.push(("Stopped".to_owned(), format!("{} (disk full)", h.stopped.join(", "))));
    }
    rows
}

/// What the health monitor says about the data drive, for the data drive screen.
pub fn drive_checks(health: Option<&HealthSummary>, mount_point: &str) -> Vec<Check> {
    let Some(h) = health else {
        return vec![Check {
            state: CheckState::Failed,
            text: "Disk protection".to_owned(),
            note: "the health monitor is not reporting".to_owned(),
        }];
    };
    let mut checks = Vec::new();
    if let Some(d) = h.disks.iter().find(|d| d.path == mount_point) {
        let (state, note) = match (d.mounted, d.level.as_str()) {
            (false, _) => (CheckState::Failed, "not measured while unmounted, its services stay stopped".to_owned()),
            (true, "Ok") => (CheckState::Passed, format!("{:.1} GB free", d.free_bytes as f64 / 1e9)),
            (true, level) => (CheckState::Failed, format!("{level}: {:.1} GB free", d.free_bytes as f64 / 1e9)),
        };
        checks.push(Check { state, text: "Free space".to_owned(), note });
    }
    if !h.stopped.is_empty() {
        checks.push(Check {
            state: CheckState::Failed,
            text: "Stopped to protect the disk".to_owned(),
            note: h.stopped.join(", "),
        });
    }
    checks
}

pub fn backups(b: &HelperBackend) -> Result<Vec<BackupInfo>, String> {
    match text(b.ask(Request::ListBackups))? {
        Response::Backups(list) => Ok(list),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

pub fn create_backup(b: &HelperBackend, device: Option<&Secret>, backup: &Secret) -> Result<BackupCreated, String> {
    let request = Request::CreateBackup { device_passphrase: device.map(api), backup_passphrase: api(backup) };
    match text(b.ask(request))? {
        Response::BackupCreated(created) => Ok(created),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

pub fn inspect_backup(b: &HelperBackend, name: &str, passphrase: &Secret) -> Result<BackupContents, String> {
    match text(b.ask(Request::InspectBackup { name: name.to_owned(), passphrase: api(passphrase) }))? {
        Response::BackupContents(contents) => Ok(contents),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

pub fn usb_drives(b: &HelperBackend) -> Result<Vec<nodo_api::UsbDrive>, String> {
    match text(b.ask(Request::ListUsbDrives))? {
        Response::UsbDrives(list) => Ok(list),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

pub fn usb_backups(b: &HelperBackend, device: &str) -> Result<nodo_api::UsbBackupList, String> {
    match text(b.ask(Request::ListUsbBackups { device: device.to_owned() }))? {
        Response::UsbBackups(list) => Ok(list),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

pub fn create_usb_backup(
    b: &HelperBackend,
    device: &str,
    device_passphrase: Option<&Secret>,
    backup: &Secret,
) -> Result<BackupCreated, String> {
    let request = Request::CreateUsbBackup {
        device: device.to_owned(),
        device_passphrase: device_passphrase.map(api),
        backup_passphrase: api(backup),
    };
    match text(b.ask(request))? {
        Response::BackupCreated(created) => Ok(created),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

pub fn inspect_usb_backup(b: &HelperBackend, device: &str, name: &str, passphrase: &Secret) -> Result<BackupContents, String> {
    let request =
        Request::InspectUsbBackup { device: device.to_owned(), name: name.to_owned(), passphrase: api(passphrase) };
    match text(b.ask(request))? {
        Response::BackupContents(contents) => Ok(contents),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

pub fn restore_usb_backup(
    b: &HelperBackend,
    device: &str,
    name: &str,
    passphrase: &Secret,
    new_device: Option<&Secret>,
    typed: &str,
) -> Result<RestoreReport, String> {
    let request = Request::RestoreUsbBackup {
        device: device.to_owned(),
        name: name.to_owned(),
        passphrase: api(passphrase),
        new_device_passphrase: new_device.map(api),
        typed: typed.to_owned(),
    };
    match text(b.ask(request))? {
        Response::Restored(report) => Ok(report),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

pub fn restore_backup(
    b: &HelperBackend,
    name: &str,
    passphrase: &Secret,
    new_device: Option<&Secret>,
    typed: &str,
) -> Result<RestoreReport, String> {
    let request = Request::RestoreBackup {
        name: name.to_owned(),
        passphrase: api(passphrase),
        new_device_passphrase: new_device.map(api),
        typed: typed.to_owned(),
    };
    match text(b.ask(request))? {
        Response::Restored(report) => Ok(report),
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system(available: Option<nodo_api::SystemSnapshot>) -> SystemUpdateStatus {
        SystemUpdateStatus {
            unavailable: None,
            current_serial: 6,
            current_snapshot: Some("2026.12".into()),
            available,
            last_check: Some(now()),
            last_error: None,
            automatic: false,
            verdict: String::new(),
            last_applied: None,
        }
    }

    fn snapshot(needs_restart: bool, withdraws_current: bool) -> nodo_api::SystemSnapshot {
        nodo_api::SystemSnapshot { serial: 7, snapshot: "2026.13".into(), needs_restart, withdraws_current }
    }

    #[test]
    fn the_system_snapshot_is_a_row_beside_the_packages_only_when_one_is_available() {
        assert_eq!(system_row(&system(None)), None);
        let row = system_row(&system(Some(snapshot(false, false)))).unwrap();
        assert_eq!((row.name.as_str(), row.from.as_str(), row.to.as_str()), (SYSTEM_BASE, "2026.12", "2026.13"));
        assert_eq!((row.note, row.source.as_str()), (UpdateNote::Ready, "tested snapshot"));
        assert_eq!(system_row(&system(Some(snapshot(true, false)))).unwrap().note, UpdateNote::RestartNeeded);
        assert_eq!(
            system_row(&system(Some(snapshot(true, true)))).unwrap().note,
            UpdateNote::Withdrawal("2026.12".into()),
            "a withdrawal is what the owner most needs to know"
        );
    }

    #[test]
    fn a_system_update_result_says_what_changed_and_what_waits() {
        let mut result = SystemUpdateResult {
            at: 0,
            serial: 7,
            snapshot: "2026.13".into(),
            changed: vec!["nodo-tools: 0.1.0_2 -> 0.1.0_3".into()],
            restarted: vec![],
            deferred: vec!["nodo-helper".into()],
            needs_restart: false,
            withdrew: None,
            automatic: false,
        };
        let row = system_result(&result);
        assert!(row.applied);
        assert_eq!(row.note, "Applied, 1 package changed, nodo-helper uses it after a restart");
        result.withdrew = Some("2026.12".into());
        result.needs_restart = true;
        assert_eq!(system_result(&result).note, "Applied, 1 package changed, left withdrawn 2026.12, restart when ready");
    }

    #[test]
    fn ago_reads_as_plain_english() {
        assert_eq!(ago(now()), "just now");
        assert_eq!(ago(now() - 90), "1 minute ago");
        assert_eq!(ago(now() - 3700), "1 hour ago");
        assert_eq!(ago(now() - 3 * 86_400), "3 days ago");
    }

    fn review(first_install: bool) -> InstallReview {
        InstallReview {
            feed: "f".into(),
            package: "demo".into(),
            pkgver: "demo-1.1_1".into(),
            permissions: vec!["uses monero-daemon".into(), "connects to the internet".into()],
            added: vec!["connects to the internet".into()],
            first_install,
            encrypted_storage: true,
            needs_passphrase: true,
        }
    }

    #[test]
    fn the_approval_quotes_every_line_the_helper_showed() {
        let data = review_data(&PackageId::new("f", "demo"), &review(true), false);
        let approval = data.approval.unwrap();
        assert_eq!(approval.pkgver, "demo-1.1_1");
        assert_eq!(approval.permissions, ["uses monero-daemon", "connects to the internet"]);
        assert_eq!(data.version, "1.1_1");
        assert!(data.needs_passphrase);
        assert_eq!(data.asks[1].level, AskLevel::Caution);
        assert_eq!(data.asks.last().unwrap().level, AskLevel::Encrypted);
    }

    #[test]
    fn the_gpu_is_shown_with_caution_and_new_on_an_update() {
        let mut r = review(true);
        r.permissions.push("uses the GPU for computation (Vulkan)".into());
        let data = review_data(&PackageId::new("f", "demo"), &r, false);
        let gpu = data.asks.iter().find(|a| a.text.contains("GPU")).unwrap();
        assert_eq!(gpu.text, "Uses the GPU for computation (Vulkan)");
        assert_eq!(gpu.level, AskLevel::Caution);
        assert_eq!(gpu.note, "Hardware access");
        assert!(data.approval.unwrap().permissions.contains(&"uses the GPU for computation (Vulkan)".to_string()));

        r.first_install = false;
        r.added = vec!["uses the GPU for computation (Vulkan)".into()];
        let data = review_data(&PackageId::new("f", "demo"), &r, true);
        assert_eq!(data.asks.len(), 1);
        assert_eq!(data.asks[0].note, "New");
        assert_eq!(data.approval.unwrap().permissions.len(), 3);
    }

    #[test]
    fn an_update_review_shows_only_what_it_adds_and_still_approves_everything() {
        let data = review_data(&PackageId::new("f", "demo"), &review(false), true);
        assert_eq!(data.asks.len(), 1);
        assert_eq!(data.asks[0].note, "New");
        assert_eq!(data.approval.unwrap().permissions.len(), 2);
    }

    #[test]
    fn home_hears_about_failed_updates_and_the_boot_fallback() {
        let health = HealthSummary {
            reporting: true,
            generated_at: 0,
            notes: vec![nodo_api::HealthNote { severity: HealthSeverity::Critical, message: "full".into() }],
            disks: Vec::new(),
            stopped: vec!["cuprated".into()],
            refuse_installs: true,
            time_synced: true,
            temperature_decic: Some(512),
            boot_fallback: Some("trial boots ran out".into()),
        };
        let status = UpdateStatus {
            candidates: Vec::new(),
            last_check: None,
            attention: vec![nodo_api::UpdateEvent {
                at: 0,
                package: "demo".into(),
                from: "1.0_1".into(),
                to: "1.1_1".into(),
                kind: UpdateEventKind::RolledBack { reason: "x".into() },
            }],
            history: Vec::new(),
            next_window: None,
        };
        let lines = attention(Some(&health), Some(&status));
        assert_eq!(lines.len(), 3);
        assert!(lines[0].text.contains("previous system"));
        assert_eq!(lines[1].level, AttentionLevel::Critical);
        assert!(lines[2].text.contains("on hold"));
        assert_eq!(health_rows(Some(&health))[0].1, "51 °C");
    }
}
