//! Which packages exist. The helper only acts on these, so a request naming a base service such
//! as `sshd`, or a hostile path, is simply not found.

use std::path::{Path, PathBuf};

use nodo_api::{ApiError, Origin};
use nodo_gen::{validate, Layout, Profile};
use nodo_manifest::Manifest;

use crate::helper::Helper;
use crate::system::System;

/// An installed package's manifest, and whether it came from the base image.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub profile: Profile,
}

impl Package {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// Where its files are, before settings decide whether they render to memory.
    pub fn layout(&self) -> Result<Layout, ApiError> {
        Layout::new(self.id(), self.profile, self.manifest.storage.encrypted)
            .map_err(|e| ApiError::Failed(e.to_string()))
    }

    /// `feed` is what the installer recorded, if anything.
    pub fn origin(&self, feed: Option<String>) -> Origin {
        match self.profile {
            Profile::System => Origin::System,
            Profile::App => Origin::Feed(feed),
        }
    }

    /// Where the package's own files are: its settings code and templates.
    pub fn package_dir(&self, system_packages: &Path) -> Result<PathBuf, ApiError> {
        match self.profile {
            Profile::System => Ok(system_packages.join(self.id())),
            Profile::App => self
                .layout()?
                .app_dir
                .map(PathBuf::from)
                .ok_or_else(|| ApiError::Failed("app has no directory".to_owned())),
        }
    }
}

/// A listed package that could not be loaded. It still shows up, as failed, rather than vanish.
#[derive(Debug, Clone)]
pub(crate) struct BrokenPackage {
    pub id: String,
    pub reason: String,
}

impl<S: System> Helper<S> {
    fn load_from(&self, dir: &Path, id: &str, profile: Profile) -> Result<Option<Package>, String> {
        let path: PathBuf = dir.join(format!("{id}.toml"));
        let Some(bytes) = self.sys.read_file(&path).map_err(|e| e.0)? else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes).map_err(|_| "manifest is not UTF-8".to_owned())?;
        // Only the signed base system may use system paths and reserved ids.
        let parsed = match profile {
            Profile::System => Manifest::parse_system(&text),
            Profile::App => Manifest::parse(&text),
        };
        let manifest = parsed.map_err(|e| e.to_string())?;
        // The file name decides which service this describes. A manifest claiming another
        // package's id must not gain control of that package.
        if manifest.id != id {
            return Err("manifest id does not match its file name".to_owned());
        }
        Ok(Some(Package { manifest, profile }))
    }

    fn load(&self, id: &str) -> Result<Option<Package>, String> {
        if let Some(package) = self.load_from(&self.paths.system_manifests, id, Profile::System)? {
            return Ok(Some(package));
        }
        self.load_from(&self.paths.app_manifests, id, Profile::App)
    }

    pub(crate) fn package(&self, id: &str) -> Result<Package, ApiError> {
        if validate::package_id(id).is_err() {
            return Err(ApiError::NotFound("invalid id".to_owned()));
        }
        match self.load(id) {
            Ok(Some(package)) => Ok(package),
            Ok(None) => Err(ApiError::NotFound(id.to_owned())),
            Err(reason) => Err(ApiError::Failed(format!("{id}: {reason}"))),
        }
    }

    pub(crate) fn packages(&self) -> Result<(Vec<Package>, Vec<BrokenPackage>), ApiError> {
        let mut ids = std::collections::BTreeSet::new();
        for dir in [&self.paths.system_manifests, &self.paths.app_manifests] {
            for name in self.sys.list_dir(dir)? {
                if let Some(id) = name.strip_suffix(".toml") {
                    if validate::package_id(id).is_ok() {
                        ids.insert(id.to_owned());
                    }
                }
            }
        }
        let mut packages = Vec::new();
        let mut broken = Vec::new();
        for id in ids {
            match self.load(&id) {
                Ok(Some(package)) => packages.push(package),
                Ok(None) => {}
                Err(reason) => broken.push(BrokenPackage { id, reason }),
            }
        }
        Ok((packages, broken))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, SHOP};

    #[test]
    fn system_manifest_wins_over_an_app_with_the_same_id() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.add_system_manifest(SHOP);
        let package = helper(sys).package("shop").unwrap();
        assert_eq!(package.profile, Profile::System);
    }

    #[test]
    fn manifest_cannot_claim_another_packages_id() {
        let sys = FakeSystem::default();
        sys.put("/m/app/cuprated.toml", SHOP.as_bytes());
        let h = helper(sys);
        assert!(matches!(h.package("cuprated"), Err(ApiError::Failed(_))));
        let (packages, broken) = h.packages().unwrap();
        assert!(packages.is_empty());
        assert_eq!(broken[0].id, "cuprated");
    }

    #[test]
    fn hostile_ids_never_reach_the_filesystem() {
        let h = helper(FakeSystem::default());
        for id in ["../../etc/passwd", "a/b", "-x", "", "a b", "$(id)"] {
            assert!(matches!(h.package(id), Err(ApiError::NotFound(_))), "{id}");
        }
        assert!(h.sys.calls().is_empty(), "{:?}", h.sys.calls());
    }
}
