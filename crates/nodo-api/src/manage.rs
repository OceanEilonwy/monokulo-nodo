//! Types for feeds, package installs, updates, health and backups.
//!
//! Kept apart from the core types in `lib.rs` so that each area can grow without touching the
//! others. Every confirmation here is checked by the helper, never only by a client.

use serde::{Deserialize, Serialize};

/// What a feed's address answered, before anything is trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedProbe {
    /// The name the feed will have on this device.
    pub name: String,
    pub address: String,
    /// The signing key the user compares with what the publisher announces.
    pub fingerprint: String,
    pub packages: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledFromFeed {
    pub package: String,
    pub version: String,
}

/// A feed now signed by a key other than the pinned one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedKeyChange {
    pub feed: String,
    pub address: String,
    pub pinned: String,
    pub offered: String,
    pub affected: Vec<InstalledFromFeed>,
}

/// Trusting a new key names both keys, so it cannot be replayed for a later change, and carries
/// the feed name exactly as the user typed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyChangeConfirmation {
    pub from: String,
    pub to: String,
    pub typed_feed_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub feed: String,
    pub name: String,
    /// `version_revision`, as the feed's index lists it.
    pub version: String,
    /// The installed `version_revision`, when installed from this feed.
    pub installed: Option<String>,
    pub size_bytes: u64,
}

/// The user's approval of exactly what a package version asks for: its `pkgver` and the lines
/// of its permission summary as they were shown. The helper installs only if the package still
/// asks for precisely these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionApproval {
    pub pkgver: String,
    pub permissions: Vec<String>,
}

/// A package downloaded, verified and inspected, waiting for the user. Nothing is installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReview {
    pub feed: String,
    pub package: String,
    pub pkgver: String,
    /// Everything this version asks for, one line each. Quote these in a [`PermissionApproval`].
    pub permissions: Vec<String>,
    /// What goes beyond the previous approval. Everything, on a first install.
    pub added: Vec<String>,
    pub first_install: bool,
    pub encrypted_storage: bool,
    /// Its encrypted volume does not exist yet, so installing needs the device passphrase.
    pub needs_passphrase: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepState {
    Done,
    Failed,
    Skipped,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallStep {
    pub name: String,
    pub note: String,
    pub state: StepState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReport {
    pub pkgver: String,
    pub steps: Vec<InstallStep>,
    pub details: Vec<String>,
    /// Set when the install failed, saying why and what was undone.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateEventKind {
    Updated,
    RolledBack { reason: String },
    /// The new version failed and the old one did not come back.
    RollbackFailed { reason: String, rollback_error: String },
    Postponed { wait: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateEvent {
    pub at: u64,
    pub package: String,
    pub from: String,
    pub to: String,
    pub kind: UpdateEventKind,
}

/// An available update and what the feed gate found out about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCandidate {
    pub info: crate::UpdateInfo,
    /// Permissions beyond the approved ones, one line each.
    pub added: Vec<String>,
    pub key_changed: bool,
    /// Why the update failed the install checks, if it did.
    pub checks_failed: Option<String>,
    pub automatic: bool,
    /// What an automatic run would do with it, in words.
    pub verdict: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub candidates: Vec<UpdateCandidate>,
    pub last_check: Option<u64>,
    /// Failed updates, kept for Home until dismissed.
    pub attention: Vec<UpdateEvent>,
    pub history: Vec<UpdateEvent>,
    /// When the automatic install window next opens, as device-local `HH:MM`, if any package is
    /// automatic.
    pub next_window: Option<String>,
}

/// One update the user selected. `approval` is needed when it asks for new permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateApproval {
    pub package: String,
    pub to: String,
    pub approval: Option<PermissionApproval>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateOutcome {
    Updated,
    RolledBack {
        reason: String,
        recovered: bool,
        rollback_error: Option<String>,
    },
    /// Nothing was changed, for the reason given.
    Refused(String),
    Postponed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateResult {
    pub package: String,
    pub from: String,
    pub to: String,
    pub outcome: UpdateOutcome,
    /// The last log lines of a failed health check.
    pub log: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HealthSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthNote {
    pub severity: HealthSeverity,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskHealth {
    pub name: String,
    pub path: String,
    pub mounted: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// `Ok`, `Low` or `Critical`.
    pub level: String,
}

/// The health daemon's report and the boot notice, for Home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSummary {
    /// Whether the health daemon's report exists and is fresh. Without it nothing below is known.
    pub reporting: bool,
    pub generated_at: u64,
    /// Most severe first.
    pub notes: Vec<HealthNote>,
    pub disks: Vec<DiskHealth>,
    /// Services stopped to protect a disk. They start again by themselves.
    pub stopped: Vec<String>,
    pub refuse_installs: bool,
    pub time_synced: bool,
    /// Tenths of a degree, so the type stays comparable.
    pub temperature_decic: Option<i32>,
    /// Set when the device fell back to its previous system after a failed one.
    pub boot_fallback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupInfo {
    /// A plain file name in the helper's backup directory.
    pub name: String,
    pub size_bytes: u64,
    pub created: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupCreated {
    pub info: BackupInfo,
    pub entries: u32,
    pub has_device_key: bool,
    /// Sources that did not exist, such as a package that keeps no settings.
    pub missing: Vec<String>,
    /// Encrypted storage that was locked and so is not in the backup.
    pub locked: Vec<String>,
}

/// A verified backup's contents. Reading it needs the backup passphrase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupContents {
    pub name: String,
    pub entries: u32,
    pub has_device_key: bool,
    /// Packages whose settings it holds.
    pub settings: Vec<String>,
    /// Packages whose encrypted storage it holds.
    pub storage: Vec<String>,
    pub onion_keys: bool,
    /// Services the restore stops while it writes, and starts again after.
    pub services_to_stop: Vec<String>,
}

/// The word the user types to confirm a restore, which overwrites settings, secrets and storage.
pub const RESTORE_WORD: &str = "restore";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    pub skipped: Vec<String>,
    pub stopped: Vec<String>,
    pub restarted: Vec<String>,
}
