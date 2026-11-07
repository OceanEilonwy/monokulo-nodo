use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use nodo_api::{ApiError, Request, Response};
use zeroize::Zeroizing;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Manifests from the signed base image. These win over apps with the same id.
    pub system_manifests: PathBuf,
    pub app_manifests: PathBuf,
    pub sv_dir: PathBuf,
    pub nft_dir: PathBuf,
    pub nft_main: PathBuf,
    pub keystore: PathBuf,
    pub volumes: PathBuf,
    pub exposure_state: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            system_manifests: "/usr/share/nodo/manifests".into(),
            app_manifests: "/var/lib/nodo/manifests".into(),
            sv_dir: "/etc/sv".into(),
            nft_dir: "/etc/nftables.d".into(),
            nft_main: "/etc/nftables.conf".into(),
            keystore: "/var/lib/nodo/keys/device.key".into(),
            volumes: "/var/lib/nodo/volumes".into(),
            exposure_state: "/var/lib/nodo/exposure.toml".into(),
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
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            health_attempts: 60,
            health_interval: Duration::from_secs(2),
            stable_secs: 5,
            unlock_backoff_base: Duration::from_secs(2),
            unlock_backoff_max: Duration::from_secs(60),
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
        match request {
            // Wrapped before anything else, so the passphrase is wiped on the denied path too.
            Request::Unlock { passphrase } => {
                let passphrase = Zeroizing::new(passphrase);
                self.authorize(uid)?;
                self.unlock(&passphrase)
            }
            Request::ListServices => {
                self.authorize(uid)?;
                self.list_services()
            }
            Request::StartService { id } => {
                self.authorize(uid)?;
                self.start_service(&id)
            }
            Request::StopService { id } => {
                self.authorize(uid)?;
                self.stop_service(&id)
            }
            Request::RestartService { id } => {
                self.authorize(uid)?;
                self.restart_service(&id)
            }
            Request::ApplySettings { package, values } => {
                self.authorize(uid)?;
                self.apply_settings(&package, &values)
            }
            Request::SetExposure {
                package,
                site,
                exposure,
            } => {
                self.authorize(uid)?;
                self.set_exposure(&package, &site, exposure)
            }
        }
    }
}

/// The name of a request, for logs. Logging the request itself would write the passphrase and
/// setting values into them.
pub(crate) fn action_name(request: &Request) -> &'static str {
    match request {
        Request::ListServices => "list-services",
        Request::StartService { .. } => "start-service",
        Request::StopService { .. } => "stop-service",
        Request::RestartService { .. } => "restart-service",
        Request::Unlock { .. } => "unlock",
        Request::ApplySettings { .. } => "apply-settings",
        Request::SetExposure { .. } => "set-exposure",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, SHOP};

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
        let requests = [
            Request::ListServices,
            Request::StartService { id: "shop".into() },
            Request::StopService { id: "shop".into() },
            Request::RestartService { id: "shop".into() },
            Request::Unlock {
                passphrase: "x".into(),
            },
            Request::ApplySettings {
                package: "shop".into(),
                values: vec![],
            },
            Request::SetExposure {
                package: "shop".into(),
                site: "web".into(),
                exposure: Default::default(),
            },
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
    fn action_names_never_contain_request_values() {
        let name = action_name(&Request::Unlock {
            passphrase: "hunter2".into(),
        });
        assert_eq!(name, "unlock");
    }
}
