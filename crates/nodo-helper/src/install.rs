//! Writes generated service files, creates the service user, loads the firewall and enables
//! the service.
//!
//! `Request::InstallService` reaches this for a manifest already in a root-owned manifest
//! directory. Placing manifests and app files there is the package manager's job.

use std::path::Path;

use nodo_api::{ApiError, Response, Secret};
use nodo_gen::{generate, service_user, Artifacts, Profile, ResolvedGrants, VOLUME_ROOT};
use nodo_manifest::Manifest;

use crate::apply::Write;
use crate::helper::{lock, Helper};
use crate::system::{System, WriteOptions};

const SCRIPT: WriteOptions = WriteOptions {
    mode: 0o755,
    group: None,
};
const CONFIG: WriteOptions = WriteOptions {
    mode: 0o644,
    group: None,
};
/// Manifests cannot ask for a size yet, so every encrypted volume gets this much.
const DEFAULT_VOLUME_SIZE: u64 = 256 * 1024 * 1024;

impl<S: System> Helper<S> {
    /// The fragment is checked under another name first, because the main ruleset includes
    /// every `*.nft` in one transaction. A single bad fragment would fail the whole firewall load.
    pub fn install_service(
        &self,
        manifest: &Manifest,
        profile: Profile,
        grants: &ResolvedGrants,
    ) -> Result<Artifacts, ApiError> {
        let artifacts =
            generate(manifest, profile, grants).map_err(|e| ApiError::Failed(e.to_string()))?;
        let _guard = lock(&self.service_lock);
        self.write_service(&manifest.id, &artifacts)?;
        Ok(artifacts)
    }

    pub(crate) fn install_by_id(
        &self,
        id: &str,
        passphrase: Option<&Secret>,
    ) -> Result<Response, ApiError> {
        self.install_package(id, passphrase, None)
    }

    /// Installs an app whose manifest the package manager has put in place: claims the id,
    /// records the feed it came from, creates its user, writes the service with the access its
    /// settings' references imply, writes its settings and enables it.
    ///
    /// An id is refused when its user, service directory or enabled link already exists and the
    /// helper did not create them, so a package can never take over an existing account or
    /// service such as `sshd`.
    pub(crate) fn install_package(
        &self,
        id: &str,
        passphrase: Option<&Secret>,
        feed: Option<&str>,
    ) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        if package.profile == Profile::System {
            return Err(ApiError::Failed(format!(
                "{id} is part of the base system and is installed with it"
            )));
        }
        let user = service_user(id).map_err(|e| ApiError::Failed(e.to_string()))?;
        let layout = package.layout()?;
        let volume = self.volume_location(id)?;
        let encrypted = package.manifest.storage.encrypted;
        let needs_volume = encrypted && !self.sys.exists(&volume.file);
        if needs_volume && passphrase.is_none() {
            return Err(ApiError::Failed(format!(
                "{id} keeps encrypted storage, so installing it needs the device passphrase"
            )));
        }
        if encrypted && !needs_volume && self.is_locked(&package)? {
            return Err(ApiError::Locked(format!(
                "{id} keeps encrypted storage, which opens with the device passphrase"
            )));
        }
        let marker = self.paths.installed.join(id);
        let service = self.paths.sv_dir.join(id);
        let enabled = self.paths.service_dir.join(id);

        let _guard = lock(&self.service_lock);
        let claimed = self.sys.exists(&marker);
        if !claimed {
            let clash = if self.sys.user_exists(&user) {
                Some(format!("the user {user} already exists"))
            } else if self.sys.exists(&service) || self.sys.read_link(&service).is_some() {
                Some(format!("{} already exists", service.display()))
            } else if self.sys.exists(&enabled) || self.sys.read_link(&enabled).is_some() {
                Some(format!("{} already exists", enabled.display()))
            } else {
                None
            };
            if let Some(clash) = clash {
                return Err(ApiError::Failed(format!(
                    "the id {id} is already in use on this device: {clash}"
                )));
            }
        }
        // Planned before anything changes, so settings that do not resolve or a sealed secret
        // that cannot be written refuse the install cleanly.
        let (packages, _) = self.packages()?;
        let plan = match package.manifest.settings {
            Some(_) => {
                Some(self.with_settings(&packages, |ctx| self.settings.plan(ctx, &package, &[]))?)
            }
            None => None,
        };
        let inputs = match &plan {
            Some(plan) => plan.service.clone(),
            None => self.with_settings(&packages, |ctx| self.settings.service(ctx, &package))?,
        };
        // Claimed before anything is created, so a failure part way leaves an install that
        // can be retried rather than a user that now blocks the id. A reinstall keeps the
        // feed it already recorded unless a new one is given.
        if !claimed || feed.is_some() {
            self.record_feed(id, feed.or(self.feed_of(id).as_deref()))?;
        }
        let artifacts = self
            .service_artifacts(&package, &inputs)?
            .ok_or_else(|| ApiError::Failed(format!("{id} could not be claimed")))?;
        if !self.sys.user_exists(&user) {
            self.sys.create_user(&user, Path::new(&layout.data_dir))?;
        }
        if let (true, Some(passphrase)) = (needs_volume, passphrase) {
            self.create_volume(id, passphrase, DEFAULT_VOLUME_SIZE)?;
        }
        self.write_service(id, &artifacts)?;
        // Settings, including generated secrets, exist before the service first starts.
        for file in plan.map(|plan| plan.files).unwrap_or_default() {
            Write::from(file).apply(&self.sys)?;
        }
        match self.sys.read_link(&enabled) {
            Some(target) if target == service => {}
            Some(_) => {
                return Err(ApiError::Failed(format!(
                    "{} points somewhere else",
                    enabled.display()
                )))
            }
            None => self.sys.symlink(&service, &enabled)?,
        }
        Ok(Response::Ok)
    }

    fn write_service(&self, id: &str, artifacts: &Artifacts) -> Result<(), ApiError> {
        let pending = self.paths.nft_dir.join(format!("{id}.nft.pending"));
        self.sys
            .atomic_write(&pending, artifacts.nft.as_bytes(), CONFIG)?;
        let checked = self.sys.nft_check(&pending);
        let _ = self.sys.remove_file(&pending);
        checked?;

        for dir in &artifacts.directories {
            let path = Path::new(&dir.path);
            // Inside a volume only while it is mounted. Opening it creates them otherwise.
            if path.starts_with(VOLUME_ROOT) && !self.volume_mounted_for(path) {
                continue;
            }
            self.sys.ensure_dir(path, dir.mode, dir.owner.as_deref())?;
        }
        let service = self.paths.sv_dir.join(id);
        self.sys.ensure_dir(&service.join("log"), 0o755, None)?;
        self.sys.ensure_dir(&service.join("control"), 0o755, None)?;
        self.sys
            .atomic_write(&service.join("run"), artifacts.run.as_bytes(), SCRIPT)?;
        self.sys.atomic_write(
            &service.join("log/run"),
            artifacts.log_run.as_bytes(),
            SCRIPT,
        )?;
        self.sys.atomic_write(
            &service.join("control/t"),
            artifacts.control_t.as_bytes(),
            SCRIPT,
        )?;

        let fragment = self.paths.nft_dir.join(format!("{id}.nft"));
        let previous = self.sys.read_file(&fragment)?;
        self.sys
            .atomic_write(&fragment, artifacts.nft.as_bytes(), CONFIG)?;
        if let Err(e) = self.sys.nft_load(&self.paths.nft_main) {
            let _ = match previous {
                Some(bytes) => self.sys.atomic_write(&fragment, &bytes, CONFIG),
                None => self.sys.remove_file(&fragment),
            };
            return Err(e.into());
        }
        Ok(())
    }

    /// Whether the volume a path inside `VOLUME_ROOT` belongs to is mounted.
    fn volume_mounted_for(&self, path: &Path) -> bool {
        path.strip_prefix(VOLUME_ROOT)
            .ok()
            .and_then(|inside| inside.components().next())
            .is_some_and(|id| self.sys.is_mounted(&Path::new(VOLUME_ROOT).join(id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, SHOP};

    fn manifest() -> Manifest {
        Manifest::parse(SHOP).unwrap()
    }

    fn install(h: &Helper<FakeSystem>) -> Result<Artifacts, ApiError> {
        h.install_service(&manifest(), Profile::App, &ResolvedGrants::default())
    }

    #[test]
    fn writes_every_file_and_loads_the_main_ruleset() {
        let h = helper(FakeSystem::default());
        let artifacts = install(&h).unwrap();
        assert_eq!(h.sys.get("/sv/shop/run").unwrap(), artifacts.run.as_bytes());
        assert_eq!(
            h.sys.get("/sv/shop/log/run").unwrap(),
            artifacts.log_run.as_bytes()
        );
        assert_eq!(
            h.sys.get("/sv/shop/control/t").unwrap(),
            artifacts.control_t.as_bytes()
        );
        assert_eq!(
            h.sys.get("/nft/shop.nft").unwrap(),
            artifacts.nft.as_bytes()
        );
        assert!(h
            .sys
            .calls()
            .contains(&"nft load /nft/main.conf".to_owned()));
    }

    #[test]
    fn rejected_fragment_never_reaches_the_included_directory() {
        let h = helper(FakeSystem::default());
        h.sys.fail("nft check");
        assert!(install(&h).is_err());
        assert_eq!(h.sys.get("/nft/shop.nft"), None);
        assert_eq!(h.sys.get("/nft/shop.nft.pending"), None);
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("nft load")));
    }

    #[test]
    fn failed_load_restores_the_previous_fragment() {
        let h = helper(FakeSystem::default());
        h.sys.put("/nft/shop.nft", b"old rules");
        h.sys.fail("nft load");
        assert!(install(&h).is_err());
        assert_eq!(h.sys.get("/nft/shop.nft").unwrap(), b"old rules");
    }

    #[test]
    fn failed_first_load_removes_the_fragment() {
        let h = helper(FakeSystem::default());
        h.sys.fail("nft load");
        assert!(install(&h).is_err());
        assert_eq!(h.sys.get("/nft/shop.nft"), None);
    }

    fn by_id(h: &Helper<FakeSystem>) -> Result<Response, ApiError> {
        h.handle(
            1000,
            nodo_api::Request::InstallService {
                id: "shop".into(),
                passphrase: None,
            },
        )
    }

    fn installed() -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        helper(sys)
    }

    #[test]
    fn install_by_id_creates_the_user_then_enables_the_service() {
        let h = installed();
        assert_eq!(by_id(&h), Ok(Response::Ok));
        assert!(h.sys.has_user("_shop"));
        assert_eq!(h.sys.link("/service/shop").as_deref(), Some("/sv/shop"));
        assert!(h.sys.get("/sv/shop/run").is_some());
        let calls = h.sys.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle)).unwrap();
        assert!(at("write /s/installed/shop") < at("useradd _shop"));
        assert!(at("useradd _shop") < at("mkdir /var/lib/apps/shop"));
        assert!(at("write /sv/shop/run") < at("symlink /service/shop"));
    }

    #[test]
    fn reinstall_by_the_helper_is_allowed() {
        let h = installed();
        by_id(&h).unwrap();
        assert_eq!(by_id(&h), Ok(Response::Ok));
    }

    #[test]
    fn ids_already_in_use_are_refused_without_changes() {
        for setup in [
            |sys: &FakeSystem| sys.add_user("_shop"),
            |sys: &FakeSystem| sys.put("/sv/shop/run", b"#!/bin/sh"),
            |sys: &FakeSystem| sys.add_link("/service/shop", "/etc/sv/other"),
        ] {
            let h = installed();
            setup(&h.sys);
            let Err(ApiError::Failed(message)) = by_id(&h) else {
                panic!()
            };
            assert!(message.contains("already in use"), "{message}");
            assert!(
                !h.sys
                    .calls()
                    .iter()
                    .any(|c| c.starts_with("write") || c.starts_with("useradd")),
                "{:?}",
                h.sys.calls()
            );
        }
    }

    #[test]
    fn failed_install_can_be_retried() {
        let h = installed();
        h.sys.fail_once("nft check");
        assert!(by_id(&h).is_err());
        assert!(h.sys.has_user("_shop"));
        assert_eq!(by_id(&h), Ok(Response::Ok));
    }

    #[test]
    fn base_system_and_unknown_ids_are_not_installed() {
        let sys = FakeSystem::default();
        sys.add_system_manifest(SHOP);
        let h = helper(sys);
        assert!(matches!(by_id(&h), Err(ApiError::Failed(_))));
        assert!(matches!(
            h.handle(
                1000,
                nodo_api::Request::InstallService {
                    id: "sshd".into(),
                    passphrase: None,
                }
            ),
            Err(ApiError::NotFound(_))
        ));
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("useradd")));
    }

    fn encrypted_install(pass: Option<&str>) -> (Helper<FakeSystem>, Result<Response, ApiError>) {
        let sys = FakeSystem::default();
        sys.add_manifest(crate::fake::ENCRYPTED);
        let h = helper(sys);
        h.init_device(&Secret::new("correct horse battery"))
            .unwrap();
        let result = h.handle(
            1000,
            nodo_api::Request::InstallService {
                id: "vault".into(),
                passphrase: pass.map(Secret::new),
            },
        );
        (h, result)
    }

    #[test]
    fn encrypted_package_gets_its_volume_at_install() {
        let (h, result) = encrypted_install(Some("correct horse battery"));
        assert_eq!(result, Ok(Response::Ok));
        assert!(h.sys.mounted("/run/nodo/volumes/vault"));
        let calls = h.sys.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle)).unwrap();
        assert!(at("useradd _vault") < at("luks format"));
        assert!(at("mkdir /run/nodo/volumes/vault/data 750 _vault") < at("write /sv/vault/run"));
        assert!(at("chown /run/nodo/volumes/vault root 711") < at("write /sv/vault/run"));
        // Logs go inside the volume, never to the system disk.
        assert!(
            !calls.iter().any(|c| c.contains("/var/log/apps/vault")),
            "{calls:?}"
        );
        assert!(calls.contains(&"mkdir /run/nodo/volumes/vault/log 750 _vault".to_owned()));
    }

    #[test]
    fn an_encrypted_volume_goes_on_the_data_drive_when_chosen() {
        let sys = FakeSystem::default();
        sys.add_manifest(crate::fake::ENCRYPTED);
        sys.put("/s/bindings.toml", b"data_drive = [\"vault\"]\n");
        let h = helper(sys);
        h.init_device(&Secret::new("correct horse battery"))
            .unwrap();
        let install = || {
            h.handle(
                1000,
                nodo_api::Request::InstallService {
                    id: "vault".into(),
                    passphrase: Some(Secret::new("correct horse battery")),
                },
            )
        };
        // Never underneath an absent drive's mount point.
        assert!(matches!(install(), Err(ApiError::Failed(m)) if m.contains("not mounted")));
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("allocate")));
        h.sys.set_mounted("/media/monero");
        assert_eq!(install(), Ok(Response::Ok));
        assert!(h
            .sys
            .calls()
            .iter()
            .any(|c| c.starts_with("allocate /media/monero/.nodo-volumes/vault.luks")));
        assert_eq!(h.sys.get("/v/vault.luks"), None);
        // After a reboot without the drive, unlock waits for it rather than looking elsewhere.
        h.sys.reboot();
        let unlock = h.handle(
            1000,
            nodo_api::Request::Unlock {
                passphrase: Secret::new("correct horse battery"),
            },
        );
        assert!(
            matches!(unlock, Err(ApiError::Failed(m)) if m.contains("waiting for the data drive"))
        );
        h.sys.set_mounted("/media/monero");
        assert!(h
            .handle(
                1000,
                nodo_api::Request::Unlock {
                    passphrase: Secret::new("correct horse battery"),
                },
            )
            .is_ok());
        assert!(h.sys.mounted("/run/nodo/volumes/vault"));
    }

    #[test]
    fn an_encrypted_package_is_not_reinstalled_while_locked() {
        let (h, result) = encrypted_install(Some("correct horse battery"));
        result.unwrap();
        h.sys.reboot();
        let again = h.handle(
            1000,
            nodo_api::Request::InstallService {
                id: "vault".into(),
                passphrase: None,
            },
        );
        assert!(matches!(again, Err(ApiError::Locked(_))), "{again:?}");
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("write")));
    }

    #[test]
    fn encrypted_install_needs_the_right_passphrase_and_counts_failures() {
        let (h, result) = encrypted_install(None);
        assert!(matches!(result, Err(ApiError::Failed(_))));
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("useradd")));
        let (h, result) = encrypted_install(Some("wrong horse battery"));
        assert_eq!(result, Err(ApiError::BadPassphrase));
        assert_eq!(
            h.sys.sleeps().len(),
            1,
            "guesses through install are delayed too"
        );
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("luks")));
    }

    #[test]
    fn hostile_manifest_writes_nothing() {
        let h = helper(FakeSystem::default());
        let mut bad = manifest();
        bad.id = "../../etc/cron.d/x".to_owned();
        assert!(h
            .install_service(&bad, Profile::App, &ResolvedGrants::default())
            .is_err());
        assert!(h.sys.calls().is_empty(), "{:?}", h.sys.calls());
    }
}
