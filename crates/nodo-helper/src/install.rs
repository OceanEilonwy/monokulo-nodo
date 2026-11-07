//! Writes generated service files and loads the firewall.
//!
//! `nodo_api::Request` has no install action yet. The package manager will reach this once it
//! exists, and until then nothing outside the crate can trigger it from a client.

use std::path::Path;

use nodo_api::ApiError;
use nodo_gen::{generate, Artifacts, Profile, ResolvedGrants};
use nodo_manifest::Manifest;

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
        let id = &manifest.id;
        let _guard = lock(&self.service_lock);

        let pending = self.paths.nft_dir.join(format!("{id}.nft.pending"));
        self.sys
            .atomic_write(&pending, artifacts.nft.as_bytes(), CONFIG)?;
        let checked = self.sys.nft_check(&pending);
        let _ = self.sys.remove_file(&pending);
        checked?;

        for dir in &artifacts.directories {
            self.sys
                .ensure_dir(Path::new(&dir.path), dir.mode, dir.owner.as_deref())?;
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
        Ok(artifacts)
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
