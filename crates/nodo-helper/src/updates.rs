//! Updates of packages installed from feeds, by hand and automatically.
//!
//! A check refreshes the feeds and inspects each newer version, so an update that asks for more,
//! comes with a changed key or fails the install checks is known before anyone selects it. The
//! rules of `nodo_update::auto` decide what runs unattended. Every update keeps the previous
//! version's verified file and, where the old version could not read what the new one leaves, a
//! copy of the package's data; a failed install or health check puts both back and holds that
//! version. An update refused before anything changed is reported and recorded as a refusal: the
//! old version never stopped, so nothing is put back and nothing is held.
//!
//! Data on the data drive can be far too large to copy on every update, so it is only copied when
//! the new version raises its data layout, next to the data on the drive itself. A new version
//! that declares the layout the data is already in shares it with the old one both ways, and its
//! rollback only puts the old package back onto the same data.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nodo_api::{
    ApiError, AutoUpdatePrefs, Secret, InstallReview, PermissionApproval, Response, UpdateApproval,
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
use crate::system::{SvAction, System, WriteOptions};

const NODE_ROLE: &str = "monero-daemon";
const HISTORY_LEN: usize = 50;
/// Inside an encrypted volume, so the copy of its data stays encrypted.
const IN_VOLUME_COPY: &str = ".nodo-rollback";
/// Room left over after copying a package's data, so the copy never fills the disk.
const COPY_MARGIN: u64 = 256 * 1024 * 1024;
/// The same inside an encrypted volume, which is small and holds nothing else.
const IN_VOLUME_MARGIN: u64 = 8 * 1024 * 1024;
/// At the top of the data drive, root-only: copies of data-drive apps' data for a rollback, one
/// directory per app, on the same filesystem as the data so a copy can share its blocks.
const DRIVE_COPIES: &str = ".nodo-rollback";
/// At the top of the data drive, root-only: the layout each data-drive app's data is in. Beside
/// the data, like the record inside an encrypted volume, but out of the service's reach.
const DRIVE_LAYOUTS: &str = ".nodo-layouts";
/// Before refusals were an outcome of their own, an update of a data-drive app was refused with
/// these words but recorded as a rollback, and its version held.
const LEGACY_DRIVE_REFUSAL: &str = "keeps its data on the data drive, which an update cannot copy for a rollback yet";

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
    /// The encrypted volume size the new version asks for, in bytes.
    #[serde(default)]
    pub volume_bytes: Option<u64>,
    /// The data layout the new version declares. Checks before this was recorded for every
    /// package only recorded it for encrypted ones.
    #[serde(default)]
    pub data_layout: Option<u32>,
    /// The volume must grow first, which needs the device passphrase.
    #[serde(default)]
    pub grows: bool,
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
            needs_passphrase: self.grows,
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
                    auto::Blocker::NeedsPassphrase => {
                        "needs the device passphrase to grow its encrypted storage".to_owned()
                    }
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
        auto::Outcome::NotUpdated {
            package,
            from,
            to,
            reason,
        } => (
            package,
            from,
            to,
            UpdateEventKind::NotUpdated {
                reason: reason.clone(),
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

fn refusal_event(r: &auto::Refusal) -> UpdateEvent {
    UpdateEvent {
        at: r.at,
        package: r.package.clone(),
        from: r.from.clone(),
        to: r.to.clone(),
        kind: UpdateEventKind::NotUpdated {
            reason: r.reason.clone(),
        },
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
        UpdateEventKind::NotUpdated { reason } => UpdateOutcome::Refused(reason),
    };
    UpdateResult {
        package: e.package,
        from: e.from,
        to: e.to,
        outcome,
        log,
    }
}

/// The same bookkeeping `nodo_update::auto::run` does, for updates the user started. The user
/// watched a refusal happen, so it is kept for the history but not shown again on Home.
fn record(state: &mut UpdateState, at: u64, outcome: auto::Outcome) {
    if let auto::Outcome::NotUpdated {
        package,
        from,
        to,
        reason,
    } = outcome
    {
        state.record_refusal(auto::Refusal {
            at,
            package,
            from,
            to,
            reason,
            unseen: false,
        });
        return;
    }
    state.forget_refusal(&outcome);
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

/// Helpers before refusals recorded a refused update of a data-drive app as a rollback, kept it
/// for Home and held its version. It was refused before anything changed, so nothing was rolled
/// back and the version never failed: each becomes the refusal it was, and its hold is released
/// unless that version also failed for real.
fn mend_legacy_refusals(state: &mut UpdateState) {
    let legacy = |o: &auto::Outcome| {
        matches!(o, auto::Outcome::RolledBack { reason, .. } if reason.ends_with(LEGACY_DRIVE_REFUSAL))
    };
    if !state.history.iter().chain(&state.attention).any(|e| legacy(&e.outcome)) {
        return;
    }
    state.attention.retain(|e| !legacy(&e.outcome));
    let mut found = Vec::new();
    state.history.retain(|e| match &e.outcome {
        auto::Outcome::RolledBack {
            package,
            from,
            to,
            reason,
        } if legacy(&e.outcome) => {
            found.push(auto::Refusal {
                at: e.at,
                package: package.clone(),
                from: from.clone(),
                to: to.clone(),
                reason: reason.clone(),
                unseen: false,
            });
            false
        }
        _ => true,
    });
    for refusal in found {
        let failed = state.history.iter().any(|e| match &e.outcome {
            auto::Outcome::RolledBack { package, to, .. } | auto::Outcome::RollbackFailed { package, to, .. } => {
                *package == refusal.package && *to == refusal.to
            }
            _ => false,
        });
        if !failed {
            state.holds.release(&refusal.package, &refusal.to);
        }
        // Anything that happened to the package since is newer news than this refusal.
        let superseded = state.history.iter().any(|e| {
            e.at >= refusal.at
                && match &e.outcome {
                    auto::Outcome::Updated { package, .. }
                    | auto::Outcome::RolledBack { package, .. }
                    | auto::Outcome::RollbackFailed { package, .. } => *package == refusal.package,
                    _ => false,
                }
        }) || state
            .refusals
            .iter()
            .any(|r| r.package == refusal.package && r.at >= refusal.at);
        if !superseded {
            state.record_refusal(refusal);
        }
    }
}

#[path = "updates_manual.rs"]
mod manual;
pub(crate) use manual::PreviousVersion;

/// Runs one update on the device. Holds what a rollback needs between the calls.
struct Updater<'a, S: System> {
    helper: &'a Helper<S>,
    /// Approvals given in this request for updates that ask for more.
    approvals: BTreeMap<String, Permissions>,
    /// The previous version's kept file, by package. Set once something changes, so a rollback
    /// of an update refused before that finds nothing to do.
    previous: BTreeMap<String, PathBuf>,
    /// The last log lines of a failed package, for the report.
    logs: BTreeMap<String, Vec<String>>,
    /// The new version's kept file, which a rollback removes again.
    fresh: BTreeMap<String, PathBuf>,
    /// What each update's new version asks of its storage, from the last check.
    storage: BTreeMap<String, StorageTarget>,
    /// Given with the request, for growing encrypted volumes.
    passphrase: Option<&'a Secret>,
    /// Packages going back by hand to their kept previous version, instead of a feed update.
    downgrade: BTreeMap<String, PreviousVersion>,
    /// What `prepare` checked and decided, by package, for the steps after it.
    prepared: BTreeMap<String, Prepared>,
    /// Data copies made for a rollback, by package. Only a finished copy is listed, so a rollback
    /// never puts a partial one over the data.
    copied: BTreeMap<String, DataCopy>,
}

/// What a new version asks of its storage.
#[derive(Debug, Clone, Copy, Default)]
struct StorageTarget {
    volume_bytes: Option<u64>,
    data_layout: Option<u32>,
}

/// Where a package keeps its data, which decides how an update protects it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DataHome {
    /// Inside its encrypted volume, wherever the volume file is.
    Volume,
    /// In the clear on the data drive mounted here.
    Drive(PathBuf),
    /// In the clear on the system disk.
    SystemDisk,
}

/// A copy of a package's data that a rollback puts back.
#[derive(Debug, Clone)]
struct DataCopy {
    data: PathBuf,
    copy: PathBuf,
    /// An entry at the top of `data` left out, which is where an in-place copy itself lives.
    skip: Option<&'static str>,
}

/// What an update does with a package's data, decided before anything changes.
#[derive(Debug, Clone)]
enum DataPlan {
    /// Copied once the old version has stopped, and put back by a rollback.
    Copy(DataCopy),
    /// Left as it is: the new version declares the layout the data is already in, so the old
    /// version can read whatever the new one leaves, and a rollback only puts the old package
    /// back onto the same data.
    InPlace,
}

/// Everything an update needs that was checked before anything changed.
#[derive(Debug, Clone)]
struct Prepared {
    /// The installed version's kept file, which a rollback reinstalls.
    current: PathBuf,
    feed: String,
    /// For a feed update: the feed's address and the permissions approved for the new version.
    from_feed: Option<(String, Permissions)>,
    home: DataHome,
    data: DataPlan,
    /// The layout the data is in before the update.
    layout: u32,
    stop_timeout: u32,
}

impl<'a, S: System> Updater<'a, S> {
    fn new(helper: &'a Helper<S>, records: &[CandidateRecord], passphrase: Option<&'a Secret>) -> Self {
        Updater {
            helper,
            approvals: BTreeMap::new(),
            previous: BTreeMap::new(),
            logs: BTreeMap::new(),
            fresh: BTreeMap::new(),
            storage: records
                .iter()
                .map(|r| {
                    let target = StorageTarget {
                        volume_bytes: r.volume_bytes,
                        data_layout: r.data_layout,
                    };
                    (r.package.clone(), target)
                })
                .collect(),
            passphrase,
            downgrade: BTreeMap::new(),
            prepared: BTreeMap::new(),
            copied: BTreeMap::new(),
        }
    }
}

fn fail(e: impl std::fmt::Display) -> Failure {
    Failure(e.to_string())
}

/// Sizes in a refusal, rounded up so following its advice is always enough.
fn size_words(bytes: u64) -> String {
    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;
    if bytes >= GB {
        format!("{:.1} GB", (bytes as f64 / GB as f64 * 10.0).ceil() / 10.0)
    } else {
        format!("{} MB", bytes.div_ceil(MB))
    }
}

impl<S: System> Updater<'_, S> {
    fn data_home(&self, id: &str, encrypted: bool) -> Result<DataHome, Failure> {
        if encrypted {
            return Ok(DataHome::Volume);
        }
        Ok(match self.helper.data_mount_of(id).map_err(fail)? {
            Some(mount) => DataHome::Drive(mount.to_path_buf()),
            None => DataHome::SystemDisk,
        })
    }

    /// Checked before anything else: data that is not there can be neither copied nor read.
    fn data_present(&self, id: &str, home: &DataHome) -> Result<(), Failure> {
        let h = self.helper;
        match home {
            DataHome::Volume => {
                let mount = PathBuf::from(volume_mount(id).map_err(fail)?);
                if !h.sys.is_mounted(&mount) {
                    return Err(Failure(format!("{id}'s encrypted storage is locked")));
                }
            }
            DataHome::Drive(mount) => {
                if !h.sys.is_mounted(mount) {
                    return Err(Failure(format!(
                        "the data drive is not mounted, so {id}'s data is not there. Connect the data drive, then try again."
                    )));
                }
            }
            DataHome::SystemDisk => {}
        }
        Ok(())
    }

    /// The layout the data is in. Data on the system disk is always copied, so its layout is
    /// not tracked.
    fn data_layout(&self, id: &str, home: &DataHome, installed: &nodo_manifest::Manifest) -> u32 {
        match home {
            DataHome::Volume => self.helper.data_layout_of(id),
            DataHome::Drive(mount) => self.helper.drive_layout_of(id, mount, installed),
            DataHome::SystemDisk => 1,
        }
    }

    /// Where the data is copied for a rollback, once the room for it is checked. `purpose` ends
    /// the general refusal, as in "...a copy of demo's data {purpose}".
    fn copy_plan(&self, c: &Candidate, home: &DataHome, purpose: &str) -> Result<DataPlan, Failure> {
        let h = self.helper;
        let id = c.package.as_str();
        let (copy, margin) = match home {
            DataHome::Volume => {
                let mount = PathBuf::from(volume_mount(id).map_err(fail)?);
                let copy = DataCopy {
                    copy: mount.join(IN_VOLUME_COPY),
                    data: mount,
                    skip: Some(IN_VOLUME_COPY),
                };
                (copy, IN_VOLUME_MARGIN)
            }
            DataHome::Drive(mount) => {
                let copy = DataCopy {
                    data: mount.join(id),
                    copy: mount.join(DRIVE_COPIES).join(id),
                    skip: None,
                };
                (copy, COPY_MARGIN)
            }
            DataHome::SystemDisk => {
                let layout = Layout::new(id, Profile::App, false).map_err(fail)?;
                let copy = DataCopy {
                    data: PathBuf::from(layout.data_dir),
                    copy: h.manage.paths.rollback.join(id),
                    skip: None,
                };
                (copy, COPY_MARGIN)
            }
        };
        let size = h.manage.packages.tree_size(&copy.data);
        let free = h.manage.packages.free_bytes(&copy.data);
        let need = size.saturating_add(margin);
        if need > free {
            return Err(Failure(match home {
                // Room for a full copy even where the drive can share blocks between the copy and
                // the data: the new version rewrites the data into its new layout, and each block
                // it rewrites then takes room of its own.
                DataHome::Drive(_) => format!(
                    "{id} {} moves its data to a newer layout, which {} cannot read, so a copy of the \
                     data is kept on the data drive to roll back to. That needs {} free there and \
                     {} is free. Free up {} on the data drive, then try again.",
                    c.to,
                    c.from,
                    size_words(need),
                    size_words(free),
                    size_words(need - free)
                ),
                _ => format!("there is not enough free space to keep a copy of {id}'s data {purpose}"),
            }));
        }
        Ok(DataPlan::Copy(copy))
    }

    /// The checks of a feed update, in the order the owner would want to hear about them.
    fn prepare_update(&mut self, c: &Candidate) -> Result<(), Failure> {
        let h = self.helper;
        let id = c.package.as_str();
        let state = h.read_feeds().map_err(fail)?;
        let record = state
            .installed
            .get(id)
            .ok_or_else(|| Failure(format!("{id} was not installed from a feed")))?;
        let feed = state.feed(&record.feed).map_err(fail)?;
        let current = record
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
        let home = self.data_home(id, package.manifest.storage.encrypted)?;
        self.data_present(id, &home)?;
        let target = self.storage.get(id).copied().unwrap_or_default();
        let layout = self.data_layout(id, &home, &package.manifest);
        // A version that declares an older layout than the data is in cannot read it.
        if let Some(older) = target.data_layout.filter(|l| *l < layout) {
            return Err(Failure(format!(
                "{id} {} uses data layout {older}, older than the layout {layout} its data is in, \
                 so it could not read it",
                c.to
            )));
        }
        let data = match &home {
            DataHome::Drive(_) => match target.data_layout {
                // Checked by a helper that only recorded layouts for encrypted storage.
                None => {
                    return Err(Failure(format!(
                        "the device does not know yet which data layout {id} {} uses. Check for updates again.",
                        c.to
                    )))
                }
                Some(l) if l == layout => DataPlan::InPlace,
                Some(_) => self.copy_plan(c, &home, "for a rollback")?,
            },
            _ => self.copy_plan(c, &home, "for a rollback")?,
        };
        if home == DataHome::Volume {
            if let Some(want) = target.volume_bytes {
                let have = h
                    .sys
                    .file_size(&h.volume_location(id).map_err(fail)?.file)
                    .unwrap_or(0);
                if want > have {
                    let passphrase = self.passphrase.ok_or_else(|| {
                        Failure(format!(
                            "{id} {} asks for {} MB of encrypted storage instead of {} MB. Growing it \
                             needs the device passphrase.",
                            c.to,
                            want >> 20,
                            have >> 20
                        ))
                    })?;
                    // Last, after every other check. Growing keeps every byte, and the old version
                    // runs on the larger volume just as well, so a failure here still leaves
                    // nothing to put back, and a rollback keeps the larger volume.
                    h.grow_volume(id, passphrase, want).map_err(fail)?;
                }
            }
        }
        self.prepared.insert(
            id.to_owned(),
            Prepared {
                current,
                feed: record.feed.clone(),
                from_feed: Some((feed.address.clone(), approved)),
                home,
                data,
                layout,
                stop_timeout: package.manifest.stop_timeout_secs,
            },
        );
        Ok(())
    }

    /// Stops the old version and copies its data, if the plan says to. From here on a failure
    /// is rolled back, which also starts the service again.
    fn take_over(&mut self, id: &str, p: &Prepared) -> Result<(), Failure> {
        let h = self.helper;
        self.previous.insert(id.to_owned(), p.current.clone());
        self.stop(id, p.stop_timeout)?;
        let DataPlan::Copy(plan) = &p.data else {
            return Ok(());
        };
        h.manage.packages.remove_tree(&plan.copy).map_err(Failure)?;
        if h.sys.exists(&plan.data) || h.manage.packages.tree_size(&plan.data) > 0 {
            if let Err(e) = h.manage.packages.copy_tree(&plan.data, &plan.copy, plan.skip) {
                let _ = h.manage.packages.remove_tree(&plan.copy);
                return Err(Failure(format!("copying {id}'s data for a rollback failed: {e}")));
            }
            self.copied.insert(id.to_owned(), plan.clone());
        }
        Ok(())
    }

    fn prepared(&self, id: &str) -> Result<Prepared, Failure> {
        self.prepared
            .get(id)
            .cloned()
            .ok_or_else(|| Failure(format!("the update of {id} was not checked first")))
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

    /// The version just installed must still fit what was decided before it: the feed could
    /// have changed between the check and the install. Nothing of it has started yet.
    fn still_fits(&self, id: &str, pkgver: &str, p: &Prepared, manifest: &nodo_manifest::Manifest) -> Result<(), Failure> {
        let h = self.helper;
        let new = manifest.storage.layout();
        if p.home != DataHome::SystemDisk && new < p.layout {
            return Err(Failure(format!(
                "{pkgver} uses data layout {new}, older than its data's layout {}",
                p.layout
            )));
        }
        if matches!(p.data, DataPlan::InPlace) && new != p.layout {
            return Err(Failure(format!(
                "{pkgver} uses data layout {new}, not the layout {} the check found, so it would \
                 convert its data with no copy to roll back to. Check for updates again.",
                p.layout
            )));
        }
        if p.home == DataHome::Volume {
            let have = h
                .sys
                .file_size(&h.volume_location(id).map_err(fail)?.file)
                .unwrap_or(0);
            if manifest.storage.volume_bytes() > have {
                return Err(Failure(format!(
                    "{pkgver} asks for a larger encrypted volume than the check found. Check for updates again."
                )));
            }
        }
        Ok(())
    }
}

impl<S: System> Installer for Updater<'_, S> {
    fn prepare(&mut self, c: &Candidate) -> Result<(), Failure> {
        match self.downgrade.get(&c.package).cloned() {
            Some(previous) => self.prepare_previous(c, &previous),
            None => self.prepare_update(c),
        }
    }

    fn install(&mut self, c: &Candidate) -> Result<(), Failure> {
        if self.downgrade.contains_key(&c.package) {
            return self.install_previous(c);
        }
        let h = self.helper;
        let id = c.package.as_str();
        let p = self.prepared(id)?;
        let (address, approved) = p
            .from_feed
            .clone()
            .ok_or_else(|| Failure(format!("the update of {id} has no feed")))?;
        self.take_over(id, &p)?;

        let mut pins = h.read_feeds().map_err(fail)?.pins;
        let pkgver = format!("{id}-{}", c.to);
        let installed = h.manage.packages.install(
            &mut pins,
            &address,
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
        if let Some(path) = &kept {
            self.fresh.insert(id.to_owned(), path.clone());
        }
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
        self.still_fits(id, &pkgver, &p, &manifest)?;
        h.place_manifest(id, &h.app_dir(id).map_err(fail)?, &manifest)
            .map_err(Failure)?;
        h.install_package(id, None, Some(&p.feed)).map_err(fail)?;
        Ok(())
    }

    fn health_check(&mut self, c: &Candidate) -> Result<(), Failure> {
        let id = c.package.as_str();
        self.healthy(id)?;
        let h = self.helper;
        let p = self.prepared(id)?;
        let new_layout = h.package(id).map_err(fail)?.manifest.storage.layout();
        // From now on the data is in the new version's layout.
        match &p.home {
            DataHome::Volume => h.set_data_layout(id, new_layout).map_err(fail)?,
            // Only written when it changes, so data that never changed layout needs no record.
            DataHome::Drive(mount) if new_layout != p.layout => {
                h.set_drive_layout(id, mount, new_layout).map_err(fail)?
            }
            _ => {}
        }
        if let Some(copy) = self.copied.remove(id) {
            let _ = h.manage.packages.remove_tree(&copy.copy);
        }
        self.prepared.remove(id);
        if let Some(previous) = self.previous.get(id) {
            let current = h.read_feeds().ok().and_then(|s| s.installed.get(id).and_then(|r| r.kept.clone()));
            if current.as_ref() != Some(previous) {
                if self.downgrade.contains_key(id) {
                    h.manage.packages.remove_kept(previous);
                } else {
                    // Kept one generation deep, so the user can roll back by hand.
                    self.keep_previous(id, &c.from, previous, p.layout);
                }
            }
        }
        Ok(())
    }

    fn rollback(&mut self, c: &Candidate) -> Result<(), Failure> {
        let h = self.helper;
        let id = c.package.as_str();
        // Nothing had changed yet: the old version is still running untouched.
        let Some(previous) = self.previous.get(id).cloned() else {
            return Ok(());
        };
        self.prepared.remove(id);
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
        let feed_name = h.read_feeds().ok().and_then(|s| s.installed.get(id).map(|r| r.feed.clone()));
        h.install_package(id, None, feed_name.as_deref()).map_err(fail)?;
        if let Some(fresh) = self.fresh.remove(id).filter(|f| *f != previous) {
            h.manage.packages.remove_kept(&fresh);
        }
        let _ = self.stop(id, manifest.stop_timeout_secs);
        // Data left in place stays as the new version left it, in the same layout, which the old
        // version reads.
        if let Some(copy) = self.copied.remove(id) {
            h.manage.packages.restore_tree(&copy.copy, &copy.data, copy.skip).map_err(Failure)?;
            let _ = h.manage.packages.remove_tree(&copy.copy);
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
        let mut state = self.load_state(&self.updates_path("state.toml"))?;
        // Mended on every read, so what is shown is right at once; the next save keeps it.
        mend_legacy_refusals(&mut state);
        Ok(state)
    }

    /// The layout a data-drive app's data is in: as recorded beside it, or, where nothing was
    /// recorded, the layout the installed version declares, which is the one it has been running
    /// on. A version only writes a record when it changes the layout.
    pub(crate) fn drive_layout_of(&self, id: &str, mount: &Path, installed: &nodo_manifest::Manifest) -> u32 {
        self.sys
            .read_file(&mount.join(DRIVE_LAYOUTS).join(id))
            .ok()
            .flatten()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| text.trim().parse().ok())
            .filter(|n| *n >= 1)
            .unwrap_or_else(|| installed.storage.layout())
    }

    pub(crate) fn set_drive_layout(&self, id: &str, mount: &Path, layout: u32) -> Result<(), ApiError> {
        let dir = mount.join(DRIVE_LAYOUTS);
        self.sys.ensure_dir(&dir, 0o700, None)?;
        self.sys.atomic_write(
            &dir.join(id),
            format!("{layout}\n").as_bytes(),
            WriteOptions {
                mode: 0o644,
                group: None,
            },
        )?;
        Ok(())
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
                volume_bytes: None,
                data_layout: None,
                grows: false,
            };
            if !record.key_changed {
                let mut pins = state.pins.clone();
                let approved = state.approved.get(id);
                match self.manage.packages.review(&mut pins, &feed.address, id, approved) {
                    Ok(Reviewed::Ready { added, pkgver, manifest, .. }) => {
                        record.added = added.iter().map(ToString::to_string).collect();
                        // Decides whether an update of data-drive data copies it first.
                        record.data_layout = Some(manifest.storage.layout());
                        if manifest.storage.encrypted {
                            let want = manifest.storage.volume_bytes();
                            let have = self
                                .volume_location(id)
                                .ok()
                                .and_then(|l| self.sys.file_size(&l.file))
                                .unwrap_or(0);
                            record.volume_bytes = Some(want);
                            record.grows = have > 0 && want > have;
                        }
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
            attention: updates
                .attention
                .iter()
                .map(event)
                .chain(updates.refusals.iter().filter(|r| r.unseen).map(refusal_event))
                .collect(),
            history: {
                let mut all: Vec<UpdateEvent> = updates
                    .history
                    .iter()
                    .map(event)
                    .chain(updates.refusals.iter().map(refusal_event))
                    .collect();
                all.sort_by_key(|e| e.at);
                all
            },
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
                needs_passphrase: candidate.grows,
                bundle: manifest.bundle.as_ref().map(|b| b.members.len() as u32),
            })),
            Ok(Reviewed::Blocked(reasons)) => Ok(crate::apps::blocked(package, &reasons)),
            Err(e) => crate::apps::refusal(package, &candidate.to, e),
        }
    }

    /// Updates that grow encrypted storage, so the screen can ask for the device passphrase
    /// before applying them.
    pub(crate) fn update_storage_growth(&self) -> Result<Response, ApiError> {
        let growth = self
            .candidates()?
            .into_iter()
            .filter(|c| c.grows)
            .map(|c| {
                let from_bytes = self
                    .volume_location(&c.package)
                    .ok()
                    .and_then(|l| self.sys.file_size(&l.file))
                    .unwrap_or(0);
                nodo_api::StorageGrowth {
                    to_bytes: c.volume_bytes.unwrap_or(from_bytes),
                    package: c.package,
                    to: c.to,
                    from_bytes,
                }
            })
            .collect();
        Ok(Response::StorageGrowth(growth))
    }

    /// Why one selected update cannot go ahead, or the permissions it was approved with.
    fn check_selected(
        &self,
        selected: &UpdateApproval,
        candidate: Option<&CandidateRecord>,
        updates: &UpdateState,
        passphrase: bool,
    ) -> Result<Option<Permissions>, String> {
        let c = candidate.ok_or("there is no update for it. Check for updates again.")?;
        if c.grows && !passphrase {
            return Err(
                "it asks for a larger encrypted volume. Growing it needs the device passphrase.".into(),
            );
        }
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
        self.apply_updates_with(selected, None)
    }

    /// Updates the user selected. The device passphrase is only needed, and only used, to grow
    /// an encrypted volume.
    pub(crate) fn apply_updates_with(
        &self,
        selected: &[UpdateApproval],
        passphrase: Option<&Secret>,
    ) -> Result<Response, ApiError> {
        let _work = lock(&self.manage.work);
        // Checked before anything starts: a mistyped passphrase must neither roll an update back
        // nor hold its version, it only asks again.
        if let Some(passphrase) = passphrase {
            let mut failures = lock(&self.failed_unlocks);
            let file = self.read_keystore()?;
            self.verify_passphrase(&mut failures, passphrase, &file)?;
        }
        let candidates = self.candidates()?;
        let mut updates = self.update_state()?;
        let mut results = Vec::new();
        let mut updater = Updater::new(self, &candidates, passphrase);
        let disk_low = self.installs_refused()?;
        let mut run = Vec::new();
        for s in selected {
            let c = candidates.iter().find(|c| c.package == s.package);
            let refused = if disk_low {
                Err("the system disk is low on space, so updates wait".to_owned())
            } else {
                self.check_selected(s, c, &updates, passphrase.is_some())
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
            if outcome.holds() {
                updates.holds.hold(&c.package, &c.to);
            }
            record(&mut updates, now, outcome.clone());
            let log = updater.logs.remove(&c.package).unwrap_or_default();
            results.push(result_of(&outcome, log));
        }
        if !run.is_empty() {
            self.prune_kept();
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
        } else if let Err(e) = self.refresh_feeds_if_due() {
            // Listing what the feeds offer does not depend on automatic updates being on, and a
            // feed that cannot be read records its problem; neither stops the rest of the run.
            eprintln!("nodo-helper: refreshing feeds: {e}");
        }
        let _work = lock(&self.manage.work);
        let mut updates = self.update_state()?;
        let records = self.candidates()?;
        let candidates: Vec<Candidate> = records.iter().map(CandidateRecord::candidate).collect();
        let mut updater = Updater::new(self, &records, None);
        let outcomes = auto::run(&policy, &mut updates, &candidates, &Live(self), &mut updater, now);
        if !outcomes.is_empty() {
            self.prune_kept();
        }
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
    // Refused before anything changed: there is nothing to roll back.
    if let Err(Failure(reason)) = updater.prepare(c) {
        return auto::Outcome::NotUpdated {
            package: c.package.clone(),
            from: c.from.clone(),
            to: c.to.clone(),
            reason,
        };
    }
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
