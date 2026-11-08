//! Feeds, package installs, updates, health and backups: the requests that manage what is
//! installed, and the state they keep.

use std::path::PathBuf;
use std::sync::Mutex;

use nodo_api::{ApiError, Request, Response};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::helper::Helper;
use crate::pkgsys::{PackageBackend, RealPackages};
use crate::system::{System, WriteOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagePaths {
    /// Feed pins, feed records, approvals and what each package was installed from.
    pub feeds: PathBuf,
    /// The auto-update policy, holds, history and the last check's candidates.
    pub updates: PathBuf,
    /// One directory per package with the verified package files kept for rollback.
    pub kept: PathBuf,
    /// Copies of a package's data taken just before an update.
    pub rollback: PathBuf,
    pub health_config: PathBuf,
    pub health_report: PathBuf,
    /// Left by the boot hook when the device fell back to its previous system.
    pub boot_fallback: PathBuf,
    pub backups: PathBuf,
    /// Where a restore stages files before moving them into place.
    pub restore_staging: PathBuf,
    pub secrets: PathBuf,
    pub onion: PathBuf,
    /// Caddy's storage: certificates and their keys, ACME accounts and the local CA.
    pub caddy_data: PathBuf,
    /// Rendered settings of apps, one directory per id.
    pub app_config: PathBuf,
    pub volume_root: PathBuf,
    /// Where a removable drive is mounted while a backup request uses it. Root only.
    pub usb_mount: PathBuf,
}

impl Default for ManagePaths {
    fn default() -> Self {
        ManagePaths {
            feeds: "/var/lib/nodo/feeds.toml".into(),
            updates: "/var/lib/nodo/updates".into(),
            kept: "/var/lib/nodo/packages".into(),
            rollback: "/var/lib/nodo/rollback".into(),
            health_config: "/etc/nodo/health.toml".into(),
            health_report: "/run/nodo/health.json".into(),
            boot_fallback: "/run/nodo/boot-fallback".into(),
            backups: "/var/lib/nodo/backups".into(),
            restore_staging: "/var/lib/nodo".into(),
            secrets: "/var/lib/nodo/secrets".into(),
            onion: "/var/lib/tor".into(),
            caddy_data: "/var/lib/caddy/data/caddy".into(),
            app_config: "/etc/apps".into(),
            volume_root: nodo_gen::VOLUME_ROOT.into(),
            usb_mount: "/run/nodo/usb".into(),
        }
    }
}

pub struct Manage {
    pub paths: ManagePaths,
    pub packages: Box<dyn PackageBackend>,
    /// Removable drives for backups.
    pub media: Box<dyn crate::usb::RemovableMedia>,
    /// The system snapshot update client.
    pub snapshots: Box<dyn crate::sysupdate::SnapshotClient>,
    /// Held across a whole install, update or restore, so they never interleave.
    pub(crate) work: Mutex<()>,
    /// Held while the feed or update state is read, changed and written back.
    pub(crate) state: Mutex<()>,
}

impl Default for Manage {
    fn default() -> Self {
        Manage {
            paths: ManagePaths::default(),
            packages: Box::new(RealPackages::default()),
            media: Box::new(crate::usb::RealMedia),
            snapshots: Box::new(crate::sysupdate::RealSnapshots::default()),
            work: Mutex::new(()),
            state: Mutex::new(()),
        }
    }
}

pub(crate) const PRIVATE: WriteOptions = WriteOptions {
    mode: 0o600,
    group: None,
};

pub(crate) fn failed(e: impl std::fmt::Display) -> ApiError {
    ApiError::Failed(e.to_string())
}

impl<S: System> Helper<S> {
    pub fn with_packages(mut self, packages: Box<dyn PackageBackend>) -> Self {
        self.manage.packages = packages;
        self
    }

    pub fn with_media(mut self, media: Box<dyn crate::usb::RemovableMedia>) -> Self {
        self.manage.media = media;
        self
    }

    pub fn with_snapshots(mut self, snapshots: Box<dyn crate::sysupdate::SnapshotClient>) -> Self {
        self.manage.snapshots = snapshots;
        self
    }

    pub fn with_manage_paths(mut self, paths: ManagePaths) -> Self {
        self.manage.paths = paths;
        self
    }

    pub(crate) fn load_state<T: DeserializeOwned + Default>(
        &self,
        path: &std::path::Path,
    ) -> Result<T, ApiError> {
        match self.sys.read_file(path)? {
            None => Ok(T::default()),
            Some(bytes) => {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| failed(format!("{} is not UTF-8", path.display())))?;
                toml::from_str(text).map_err(|e| failed(format!("{}: {e}", path.display())))
            }
        }
    }

    pub(crate) fn save_state<T: Serialize>(
        &self,
        path: &std::path::Path,
        value: &T,
    ) -> Result<(), ApiError> {
        let text = toml::to_string(value).map_err(failed)?;
        if let Some(dir) = path.parent() {
            self.sys.ensure_dir(dir, 0o755, None)?;
        }
        self.sys.atomic_write(path, text.as_bytes(), PRIVATE)?;
        Ok(())
    }

    /// The requests this module owns. Anything else is not implemented here.
    pub(crate) fn handle_managed(&self, uid: u32, request: Request) -> Result<Response, ApiError> {
        match request {
            Request::ListFeeds => self.list_feeds(),
            Request::ProbeFeed { address } => self.probe_feed(&address),
            Request::AddFeed {
                address,
                fingerprint,
            } => self.add_feed(&address, &fingerprint),
            Request::RefreshFeeds => self.refresh_feeds(),
            Request::SetFeedEnabled { name, enabled } => self.set_feed_enabled(&name, enabled),
            Request::RemoveFeed { name } => self.remove_feed(&name),
            Request::FeedKeyChange { name } => self.feed_key_change(&name),
            Request::TrustFeedKey { name, confirmation } => {
                self.trust_feed_key(&name, &confirmation)
            }
            Request::Catalog => self.catalog(),
            Request::ReviewInstall { feed, package } => self.review_install(&feed, &package),
            Request::InstallPackage {
                feed,
                package,
                approval,
                passphrase,
            } => self.install_from_feed(&feed, &package, &approval, passphrase.as_ref()),
            Request::ListUpdates => self.list_updates(),
            Request::UpdateStatus => self.update_status(),
            Request::CheckUpdates => self.check_updates().and_then(|()| self.update_status()),
            Request::ReviewUpdate { package } => self.review_update(&package),
            Request::ApplyUpdates { updates } => self.apply_updates(&updates),
            Request::SetHold {
                package,
                version,
                held,
            } => self.set_hold(&package, &version, held),
            Request::DismissUpdate { package } => self.dismiss_update(&package),
            Request::GetAutoUpdate => self.get_auto_update(),
            Request::SetAutoUpdate(prefs) => self.set_auto_update(&prefs),
            Request::RunAutoUpdate => {
                if uid != 0 {
                    return Err(ApiError::Denied);
                }
                self.run_auto_update()
            }
            Request::Health => self.health_summary().map(Response::Health),
            Request::ListBackups => self.list_backups(),
            Request::CreateBackup {
                device_passphrase,
                backup_passphrase,
            } => self.create_backup(device_passphrase.as_ref(), &backup_passphrase),
            Request::InspectBackup { name, passphrase } => self.inspect_backup(&name, &passphrase),
            Request::RestoreBackup {
                name,
                passphrase,
                new_device_passphrase,
                typed,
            } => self.restore_backup(&name, &passphrase, new_device_passphrase.as_ref(), &typed),
            Request::DeleteBackup { name } => self.delete_backup(&name),
            Request::ListUsbDrives => self.list_usb_drives(),
            Request::ListUsbBackups { device } => self.list_usb_backups(&device),
            Request::CreateUsbBackup {
                device,
                device_passphrase,
                backup_passphrase,
            } => self.create_usb_backup(&device, device_passphrase.as_ref(), &backup_passphrase),
            Request::InspectUsbBackup {
                device,
                name,
                passphrase,
            } => self.inspect_usb_backup(&device, &name, &passphrase),
            Request::RestoreUsbBackup {
                device,
                name,
                passphrase,
                new_device_passphrase,
                typed,
            } => self.restore_usb_backup(
                &device,
                &name,
                &passphrase,
                new_device_passphrase.as_ref(),
                &typed,
            ),
            Request::ApplyUpdatesWithPassphrase {
                updates,
                passphrase,
            } => self.apply_updates_with(&updates, Some(&passphrase)),
            Request::SystemUpdateStatus => self.system_update_status().map(Response::SystemUpdate),
            Request::CheckSystemUpdate => self.check_system_update().map(Response::SystemUpdate),
            Request::ApplySystemUpdate { serial } => {
                self.apply_system_update(serial, false).map(Response::SystemUpdateApplied)
            }
            Request::SetSystemAutoUpdate { enabled } => self.set_system_auto_update(enabled),
            Request::RunSystemUpdate => {
                if uid != 0 {
                    return Err(ApiError::Denied);
                }
                self.run_system_update()
            }
            Request::UpdateStorageGrowth => self.update_storage_growth(),
            Request::ListRollbacks => self.list_rollbacks(),
            Request::RollbackPackage { package, to } => self.rollback_package(&package, &to),
            other => Err(ApiError::NotImplemented(format!(
                "{} is not connected yet",
                nodo_api::action_name(&other)
            ))),
        }
    }
}

#[cfg(test)]
#[path = "manage_tests.rs"]
mod tests;
