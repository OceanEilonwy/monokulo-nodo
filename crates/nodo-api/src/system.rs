//! Types for system snapshot updates and for updates that grow encrypted storage.
//!
//! System updates move the base system to the channel's newest signed snapshot through the
//! helper, which runs `nodo-sysupdate`. They are never automatic unless the owner turns that on,
//! and even then a snapshot that needs a restart waits for the owner.

use serde::{Deserialize, Serialize};

/// The channel's newest snapshot, newer than the one installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemSnapshot {
    pub serial: u64,
    pub snapshot: String,
    /// It replaces the kernel, libc or init, so the device restarts to finish it.
    pub needs_restart: bool,
    /// The publisher pulled the snapshot this device is on. Applying this one moves this
    /// project's packages back to its versions.
    pub withdraws_current: bool,
}

/// What one system update did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemUpdateResult {
    pub at: u64,
    pub serial: u64,
    pub snapshot: String,
    /// `name: old -> new`, one per package.
    pub changed: Vec<String>,
    pub restarted: Vec<String>,
    /// Services that changed and keep running the old version until their next restart, such
    /// as the helper itself.
    pub deferred: Vec<String>,
    pub needs_restart: bool,
    /// The withdrawn snapshot the device left, when packages moved back.
    pub withdrew: Option<String>,
    /// Applied by the schedule rather than by the owner.
    pub automatic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemUpdateStatus {
    /// Why this device cannot update its system, such as no update server being set. `None`
    /// when it can.
    pub unavailable: Option<String>,
    /// The serial of the last applied snapshot, or the image's own.
    pub current_serial: u64,
    pub current_snapshot: Option<String>,
    pub available: Option<SystemSnapshot>,
    pub last_check: Option<u64>,
    /// Why the last check or update failed, if it did.
    pub last_error: Option<String>,
    /// Off by default. When off, the schedule only checks and reports.
    pub automatic: bool,
    /// What the schedule will do with the available snapshot, in words.
    pub verdict: String,
    pub last_applied: Option<SystemUpdateResult>,
}

/// An available package update whose new version asks for a larger encrypted volume. Growing
/// it needs the device passphrase, given with `Request::ApplyUpdatesWithPassphrase`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageGrowth {
    pub package: String,
    pub to: String,
    pub from_bytes: u64,
    pub to_bytes: u64,
}
