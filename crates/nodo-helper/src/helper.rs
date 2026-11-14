use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use nodo_api::{ApiError, Request, Response};

use crate::exposure::ExposureApplier;
use crate::keys::KdfParams;
use crate::settings::SettingsPolicy;
use crate::system::System;

/// The users who may talk to the helper. Nobody is allowed by default, root included, so an
/// empty configuration fails closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowedUids(BTreeSet<u32>);

impl AllowedUids {
    pub fn new(uids: impl IntoIterator<Item = u32>) -> Self {
        AllowedUids(uids.into_iter().collect())
    }

    pub fn allows(&self, uid: u32) -> bool {
        self.0.contains(&uid)
    }
}

/// A data drive listed in `/etc/fstab`, which services such as the node keep their data on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveConfig {
    pub device: PathBuf,
    pub mount_point: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Manifests from the signed base image. These win over apps with the same id.
    pub system_manifests: PathBuf,
    pub app_manifests: PathBuf,
    /// Settings files and templates of base image packages, one directory per id.
    pub system_packages: PathBuf,
    pub sv_dir: PathBuf,
    /// runit's directory of enabled services.
    pub service_dir: PathBuf,
    pub nft_dir: PathBuf,
    pub nft_main: PathBuf,
    pub keystore: PathBuf,
    pub volumes: PathBuf,
    pub exposure_state: PathBuf,
    pub public_hosts: PathBuf,
    /// Stored setting values, one root-only file per package.
    pub settings_store: PathBuf,
    /// One marker per service the helper installed, so a reinstall is told apart from a clash.
    pub installed: PathBuf,
    pub authorized_keys: PathBuf,
    pub drive: Option<DriveConfig>,
    /// Role bindings, approved grants and data drive choices, in one root-only file.
    pub bindings: PathBuf,
    /// The screen PIN's salted hash, root-only. Missing means the screen can only look.
    pub screen_pin: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            system_manifests: "/usr/share/nodo/manifests".into(),
            app_manifests: "/var/lib/nodo/manifests".into(),
            system_packages: "/usr/share/nodo/packages".into(),
            sv_dir: "/etc/sv".into(),
            service_dir: "/var/service".into(),
            nft_dir: "/etc/nftables.d".into(),
            nft_main: "/etc/nftables.conf".into(),
            keystore: "/var/lib/nodo/keys/device.key".into(),
            volumes: "/var/lib/nodo/volumes".into(),
            exposure_state: "/var/lib/nodo/exposure.toml".into(),
            public_hosts: "/var/lib/nodo/public-hosts.toml".into(),
            settings_store: "/var/lib/nodo/settings".into(),
            installed: "/var/lib/nodo/installed".into(),
            authorized_keys: "/root/.ssh/authorized_keys".into(),
            drive: None,
            bindings: "/var/lib/nodo/bindings.toml".into(),
            screen_pin: "/var/lib/nodo/screen-pin".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timing {
    pub health_attempts: u32,
    pub health_interval: Duration,
    /// A service with no declared health check counts as healthy after staying up this long.
    pub stable_secs: u64,
    pub unlock_backoff_base: Duration,
    pub unlock_backoff_max: Duration,
    /// How long the screen stays unlocked for changes after its last change.
    pub screen_idle: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            health_attempts: 60,
            health_interval: Duration::from_secs(2),
            stable_secs: 5,
            unlock_backoff_base: Duration::from_secs(2),
            unlock_backoff_max: Duration::from_secs(60),
            screen_idle: Duration::from_secs(5 * 60),
        }
    }
}

pub struct Helper<S: System> {
    pub(crate) sys: S,
    pub(crate) allowed: AllowedUids,
    pub(crate) paths: Paths,
    pub(crate) timing: Timing,
    pub(crate) kdf: KdfParams,
    pub(crate) settings: Box<dyn SettingsPolicy>,
    pub(crate) exposure: Box<dyn ExposureApplier>,
    /// Serializes actions that change services, so two restarts cannot interleave. Listing stays
    /// outside it and keeps working during a long restart.
    pub(crate) service_lock: Mutex<()>,
    /// Counts consecutive failures. Held across the delay, so parallel guesses wait in line and
    /// each pays for its own attempt.
    pub(crate) failed_unlocks: Mutex<u32>,
    /// The most recent read-only drive check, which a repair must quote.
    pub(crate) last_drive_check: Mutex<Option<DriveCheck>>,
    /// Feeds, installs, updates, health and backups.
    pub(crate) manage: crate::manage::Manage,
    /// Opens sealed secrets. Only in memory, from an unlock until the helper exits.
    pub(crate) secrets: Mutex<Option<std::sync::Arc<crate::secrets::SecretsKey>>>,
    /// Wi-Fi and Bluetooth.
    pub(crate) radios: crate::radio::Radios,
    /// Whether the touchscreen may make changes right now.
    pub(crate) screen: crate::screen::ScreenGate,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DriveCheck {
    pub id: u64,
    pub clean: bool,
    pub at: std::time::Instant,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<S: System> Helper<S> {
    pub fn new(
        sys: S,
        allowed: AllowedUids,
        settings: Box<dyn SettingsPolicy>,
        exposure: Box<dyn ExposureApplier>,
    ) -> Self {
        Helper {
            sys,
            allowed,
            paths: Paths::default(),
            timing: Timing::default(),
            kdf: KdfParams::default(),
            settings,
            exposure,
            service_lock: Mutex::new(()),
            failed_unlocks: Mutex::new(0),
            last_drive_check: Mutex::new(None),
            manage: crate::manage::Manage::default(),
            secrets: Mutex::new(None),
            radios: crate::radio::Radios::default(),
            screen: crate::screen::ScreenGate::default(),
        }
    }

    pub fn with_paths(mut self, paths: Paths) -> Self {
        self.paths = paths;
        self
    }

    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }

    pub fn with_kdf(mut self, kdf: KdfParams) -> Self {
        self.kdf = kdf;
        self
    }

    pub fn allows(&self, uid: u32) -> bool {
        self.allowed.allows(uid)
    }

    fn authorize(&self, uid: u32) -> Result<(), ApiError> {
        if self.allows(uid) {
            Ok(())
        } else {
            Err(ApiError::Denied)
        }
    }

    pub fn handle(&self, uid: u32, request: Request) -> Result<Response, ApiError> {
        // Secrets in the request are wiped when it drops, on the denied path too.
        self.authorize(uid)?;
        if matches!(
            request,
            Request::ScreenStatus | Request::ScreenUnlock { .. } | Request::ScreenLock | Request::SetScreenPin { .. }
        ) {
            return self.screen_request_from_owner(request);
        }
        self.handle_trusted(uid, request)
    }

    /// Answers a request from a caller already allowed to send it: an allowed uid on the main
    /// socket, or the screen within what [`crate::screen`] lets it do.
    pub(crate) fn handle_trusted(&self, uid: u32, request: Request) -> Result<Response, ApiError> {
        match request {
            Request::Unlock { passphrase } => self.unlock(&passphrase),
            Request::InitEncryption { passphrase } => {
                self.init_device(&passphrase).map(|()| Response::Ok)
            }
            Request::ChangePassphrase { current, new } => self.change_passphrase(&current, &new),
            Request::ListServices => self.list_services(),
            Request::StartService { id } => self.start_service(&id),
            Request::StopService { id } => self.stop_service(&id),
            Request::RestartService { id } => self.restart_service(&id),
            Request::ApplySettings { package, changes } => self.apply_settings(&package, &changes),
            Request::SettingsForm { package } => self.settings_form(&package),
            Request::SetExposure {
                site,
                exposure,
                confirmation,
            } => self.set_exposure(&site, exposure, confirmation.as_ref()),
            Request::SetPublicHost { site, host } => self.set_public_host(&site, &host),
            Request::ListSites => self.list_sites(),
            Request::DeviceOverview => self.overview(),
            Request::ListPackages => self.list_packages(),
            Request::PackageDetail { id } => self.package_detail(&id),
            Request::LogSources => self.log_sources(),
            Request::ReadLog { source, max_lines } => self.read_log(&source, max_lines),
            Request::DriveStatus => Ok(Response::Drive(self.drive_status())),
            Request::CheckDrive => self.check_drive(),
            Request::RepairDrive(confirmation) => self.repair_drive(confirmation),
            Request::MountDrive => self.mount_drive(),
            Request::InstallService { id, passphrase } => {
                self.install_by_id(&id, passphrase.as_ref())
            }
            Request::ListSshKeys => self.list_ssh_keys(),
            Request::AddSshKey { line } => self.add_ssh_key(&line),
            Request::RemoveSshKey { fingerprint } => self.remove_ssh_key(&fingerprint),
            // Owned by other parts of the system that do not exist yet. Saying so is better than
            // inventing data, and the TUI shows it as such.
            Request::NodeStatus => self.node_status(),
            Request::ListRoleBindings => self.list_role_bindings(),
            Request::SetRoleBinding { role, package } => {
                self.set_role_binding(&role, package.as_deref())
            }
            Request::ListGrants => self.list_grants(),
            Request::ListReferences { package } => self.list_references(&package),
            Request::PreviewSetting { package, change } => self.preview_setting(&package, &change),
            Request::SetGrant { grant, approved } => self.set_grant(&grant, approved),
            Request::UseDataDrive { package, enabled } => self.use_data_drive(&package, enabled),
            request @ (Request::CheckReachability { .. }
            | Request::ListProviders
            | Request::WebStatus
            | Request::ListDnsProviders
            | Request::SetDnsProvider { .. }
            | Request::TestDnsProvider
            | Request::ListDomainProviders
            | Request::AddDomainProvider { .. }
            | Request::RemoveDomainProvider { .. }
            | Request::RefreshDomainProvider { .. }
            | Request::AddProviderDomain { .. }
            | Request::RetireStaleRecords
            | Request::CheckReachabilityDetail { .. }
            | Request::AcmeSettings
            | Request::SetAcmeSettings(_)
            | Request::SetDns01Credentials { .. }
            | Request::CertificateStatus
            | Request::CheckLocalNames
            | Request::LocalSiteStatus) => self.web_request(request),
            Request::OnionStatus => self.onion_status(),
            Request::SetNodeOnion { enabled } => self.set_node_onion(enabled),
            Request::WifiStatus => self.wifi_status().map(Response::Wifi),
            Request::SetWifiEnabled { enabled } => self.set_wifi_enabled(enabled),
            Request::ScanWifi => self.scan_wifi(),
            Request::JoinWifi(join) => self.join_wifi(&join),
            Request::ForgetWifi { ssid } => self.forget_wifi(&ssid),
            Request::BluetoothStatus => self.bluetooth_status().map(Response::Bluetooth),
            Request::SetBluetoothEnabled { enabled } => self.set_bluetooth_enabled(enabled),
            Request::ScanBluetooth => self.scan_bluetooth(),
            Request::PairBluetooth { address } => self.pair_bluetooth(&address),
            Request::ConnectBluetooth { address, connect } => self.connect_bluetooth(&address, connect),
            Request::RemoveBluetooth { address } => self.remove_bluetooth(&address),
            // Only root may ask, which is what the nodo-localnet service runs as.
            Request::LocalAddressChanged if uid != 0 => Err(ApiError::Denied),
            Request::LocalAddressChanged => self.check_local_address().map(|_| Response::Ok),
            other => self.handle_managed(uid, other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, SHOP};
    use nodo_api::Secret;

    #[test]
    fn empty_policy_denies_everyone_including_root() {
        let policy = AllowedUids::default();
        assert!(!policy.allows(0));
        assert!(!policy.allows(1000));
    }

    #[test]
    fn every_request_is_denied_for_an_unknown_uid() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let h = helper(sys);
        let site = nodo_api::SiteId {
            package: "shop".into(),
            site: "web".into(),
        };
        let requests = [
            Request::ListServices,
            Request::StartService { id: "shop".into() },
            Request::StopService { id: "shop".into() },
            Request::RestartService { id: "shop".into() },
            Request::Unlock {
                passphrase: Secret::new("x"),
            },
            Request::ApplySettings {
                package: "shop".into(),
                changes: vec![],
            },
            Request::SetExposure {
                site: site.clone(),
                exposure: Default::default(),
                confirmation: None,
            },
            Request::InitEncryption {
                passphrase: Secret::new("x"),
            },
            Request::ChangePassphrase {
                current: Secret::new("x"),
                new: Secret::new("y"),
            },
            Request::DeviceOverview,
            Request::ListPackages,
            Request::PackageDetail { id: "shop".into() },
            Request::SettingsForm {
                package: "shop".into(),
            },
            Request::ListReferences {
                package: "shop".into(),
            },
            Request::PreviewSetting {
                package: "shop".into(),
                change: nodo_api::SettingChange {
                    name: "label".into(),
                    value: nodo_api::SettingInput::Reset,
                },
            },
            Request::LogSources,
            Request::ReadLog {
                source: "shop".into(),
                max_lines: 5,
            },
            Request::DriveStatus,
            Request::CheckDrive,
            Request::RepairDrive(nodo_api::RepairConfirmation {
                check_id: 1,
                accept_data_loss_risk: true,
            }),
            Request::MountDrive,
            Request::ListSites,
            Request::SetPublicHost {
                site,
                host: "a.example".into(),
            },
            Request::InstallService {
                id: "shop".into(),
                passphrase: None,
            },
            Request::ListSshKeys,
            Request::AddSshKey { line: "x".into() },
            Request::RemoveSshKey {
                fingerprint: "x".into(),
            },
            Request::ListFeeds,
        ];
        for request in requests {
            assert_eq!(
                h.handle(4242, request.clone()),
                Err(ApiError::Denied),
                "{request:?}"
            );
        }
        assert!(h.sys.calls().is_empty(), "{:?}", h.sys.calls());
    }

    #[test]
    fn only_root_may_report_a_local_address_change() {
        let h = helper(FakeSystem::default());
        assert_eq!(h.handle(1000, Request::LocalAddressChanged), Err(ApiError::Denied));
        assert!(matches!(h.handle(1000, Request::LocalSiteStatus), Err(ApiError::NotImplemented(_))));
        // The test helper's applier has no web side, so the check finds nothing to do.
        assert_eq!(h.check_local_address(), Ok(false));
    }

    #[test]
    fn missing_subsystems_say_so_instead_of_inventing_data() {
        let h = helper(FakeSystem::default());
        assert!(matches!(
            h.handle(1000, Request::ListProviders),
            Err(ApiError::NotImplemented(_))
        ));
    }
}
