use std::collections::BTreeMap;
use std::path::Path;

use nodo_api::{ApiError, Exposure, PublicConfirmation, Response, SiteId, SiteInfo};
use nodo_manifest::WebService;
use serde::{de::DeserializeOwned, Serialize};

use crate::catalog::Package;
use crate::helper::{lock, Helper};
use crate::system::{System, WriteOptions};

/// Every site's chosen exposure, keyed by package then site.
pub type ExposureState = BTreeMap<String, BTreeMap<String, Exposure>>;

/// The host name the user brought for each site, keyed like [`ExposureState`].
pub type PublicHosts = BTreeMap<String, BTreeMap<String, String>>;

/// Puts exposure state into effect. The Caddy crate implements this.
pub trait ExposureApplier: Send + Sync {
    fn apply(&self, state: &ExposureState) -> Result<(), String>;
}

/// Persists state and changes nothing else, until the Caddy implementation exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCaddyYet;

impl ExposureApplier for NoCaddyYet {
    fn apply(&self, _state: &ExposureState) -> Result<(), String> {
        Ok(())
    }
}

const STATE_FILE: WriteOptions = WriteOptions {
    mode: 0o644,
    group: None,
};

/// A DNS name of at least two labels. Lowercase only, so the typed confirmation compares
/// exactly with what was stored.
pub(crate) fn valid_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    host.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && !labels
            .last()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
}

fn find_site<'a>(package: &'a Package, site: &str) -> Result<&'a WebService, ApiError> {
    package
        .manifest
        .web
        .iter()
        .find(|web| web.name == site)
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "{} has no site {}",
                package.id(),
                site.escape_debug()
            ))
        })
}

impl<S: System> Helper<S> {
    fn load_toml<T: DeserializeOwned + Default>(
        &self,
        path: &Path,
    ) -> Result<(T, Option<Vec<u8>>), ApiError> {
        let previous = self.sys.read_file(path)?;
        let value = match &previous {
            Some(bytes) => std::str::from_utf8(bytes)
                .ok()
                .and_then(|text| toml::from_str(text).ok())
                .ok_or_else(|| ApiError::Failed(format!("{} is corrupt", path.display())))?,
            None => T::default(),
        };
        Ok((value, previous))
    }

    fn save_toml<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), ApiError> {
        let text = toml::to_string(value).map_err(|e| ApiError::Failed(e.to_string()))?;
        if let Some(dir) = path.parent() {
            self.sys.ensure_dir(dir, 0o755, None)?;
        }
        self.sys.atomic_write(path, text.as_bytes(), STATE_FILE)?;
        Ok(())
    }

    fn saved_host(&self, site: &SiteId) -> Result<Option<String>, ApiError> {
        let (hosts, _): (PublicHosts, _) = self.load_toml(&self.paths.public_hosts)?;
        Ok(hosts
            .get(&site.package)
            .and_then(|sites| sites.get(&site.site))
            .cloned())
    }

    pub(crate) fn set_exposure(
        &self,
        site: &SiteId,
        exposure: Exposure,
        confirmation: Option<&PublicConfirmation>,
    ) -> Result<Response, ApiError> {
        let package = self.package(&site.package)?;
        let web = find_site(&package, &site.site)?;
        let enabled = [exposure.local, exposure.onion, exposure.public];
        // An app tied to one base URL breaks when reached through a second address.
        if !web.multi_address && enabled.iter().filter(|on| **on).count() > 1 {
            return Err(ApiError::Failed(format!(
                "{} works from a single address, so only one exposure can be on",
                site.site
            )));
        }

        let _guard = lock(&self.service_lock);
        let path = &self.paths.exposure_state;
        let (mut state, previous): (ExposureState, _) = self.load_toml(path)?;
        let was_public = state
            .get(&site.package)
            .and_then(|sites| sites.get(&site.site))
            .is_some_and(|old| old.public);
        if exposure.public && !was_public {
            let Some(host) = self.saved_host(site)? else {
                return Err(ApiError::NeedsConfirmation(
                    "choose the public host name before going public".to_owned(),
                ));
            };
            if confirmation.map(|c| c.typed_host.as_str()) != Some(host.as_str()) {
                return Err(ApiError::NeedsConfirmation(format!(
                    "type {host} exactly to confirm going public"
                )));
            }
        }
        state
            .entry(site.package.clone())
            .or_default()
            .insert(site.site.clone(), exposure);
        self.save_toml(path, &state)?;

        if let Err(reason) = self.exposure.apply(&state) {
            // Saved state must not claim an exposure that never took effect.
            let restored = match &previous {
                Some(bytes) => self.sys.atomic_write(path, bytes, STATE_FILE),
                None => self.sys.remove_file(path),
            };
            return Err(ApiError::Failed(match restored {
                Ok(()) => format!("could not apply exposure: {reason}"),
                Err(e) => format!(
                    "could not apply exposure: {reason}. The old state could not be restored: {e}"
                ),
            }));
        }
        Ok(Response::Ok)
    }

    /// A public site keeps its host, because the user confirmed going public for that name.
    pub(crate) fn set_public_host(&self, site: &SiteId, host: &str) -> Result<Response, ApiError> {
        let package = self.package(&site.package)?;
        find_site(&package, &site.site)?;
        if !valid_host(host) {
            return Err(ApiError::Failed(
                "the host must be a full lowercase domain name, such as shop.example.com"
                    .to_owned(),
            ));
        }
        let _guard = lock(&self.service_lock);
        let (state, _): (ExposureState, _) = self.load_toml(&self.paths.exposure_state)?;
        let public = state
            .get(&site.package)
            .and_then(|sites| sites.get(&site.site))
            .is_some_and(|e| e.public);
        let (mut hosts, _): (PublicHosts, _) = self.load_toml(&self.paths.public_hosts)?;
        let current = hosts.get(&site.package).and_then(|s| s.get(&site.site));
        if public && current.map(String::as_str) != Some(host) {
            return Err(ApiError::Failed(
                "take the site off its public address before changing the host".to_owned(),
            ));
        }
        hosts
            .entry(site.package.clone())
            .or_default()
            .insert(site.site.clone(), host.to_owned());
        self.save_toml(&self.paths.public_hosts, &hosts)?;
        Ok(Response::Ok)
    }

    pub(crate) fn list_sites(&self) -> Result<Response, ApiError> {
        let (packages, _) = self.packages()?;
        let (state, _): (ExposureState, _) = self.load_toml(&self.paths.exposure_state)?;
        let (hosts, _): (PublicHosts, _) = self.load_toml(&self.paths.public_hosts)?;
        let mut sites = Vec::new();
        for package in &packages {
            let locked = self.is_locked(package).unwrap_or(false);
            for web in &package.manifest.web {
                let id = SiteId {
                    package: package.id().to_owned(),
                    site: web.name.clone(),
                };
                sites.push(SiteInfo {
                    exposure: state
                        .get(&id.package)
                        .and_then(|s| s.get(&id.site))
                        .copied()
                        .unwrap_or_default(),
                    public_host: hosts
                        .get(&id.package)
                        .and_then(|s| s.get(&id.site))
                        .cloned(),
                    multi_address: web.multi_address,
                    locked,
                    id,
                });
            }
        }
        Ok(Response::Sites(sites))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, TestPolicy, SHOP};

    const STATE: &str = "/s/exposure.toml";
    const HOST: &str = "shop.example.com";
    const BOTH: Exposure = Exposure {
        local: true,
        onion: false,
        public: true,
    };
    const LOCAL: Exposure = Exposure {
        local: true,
        onion: false,
        public: false,
    };
    const PUBLIC: Exposure = Exposure {
        local: false,
        onion: false,
        public: true,
    };

    fn site(name: &str) -> SiteId {
        SiteId {
            package: "shop".into(),
            site: name.into(),
        }
    }

    fn typed(host: &str) -> PublicConfirmation {
        PublicConfirmation {
            typed_host: host.into(),
        }
    }

    fn setup() -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let h = helper(sys);
        h.set_public_host(&site("web"), HOST).unwrap();
        h.set_public_host(&site("single"), "one.example.com")
            .unwrap();
        h
    }

    fn saved(h: &Helper<FakeSystem>) -> ExposureState {
        toml::from_str(std::str::from_utf8(&h.sys.get(STATE).unwrap()).unwrap()).unwrap()
    }

    fn go(h: &Helper<FakeSystem>, name: &str, exposure: Exposure) -> Result<Response, ApiError> {
        let host = if name == "web" {
            HOST
        } else {
            "one.example.com"
        };
        h.set_exposure(&site(name), exposure, Some(&typed(host)))
    }

    #[test]
    fn persists_and_survives_later_changes() {
        let h = setup();
        go(&h, "web", BOTH).unwrap();
        assert_eq!(saved(&h)["shop"]["web"], BOTH);
        go(&h, "single", LOCAL).unwrap();
        let state = saved(&h);
        assert_eq!(state["shop"]["web"], BOTH);
        assert_eq!(state["shop"]["single"], LOCAL);
    }

    #[test]
    fn unknown_package_or_site_is_not_found() {
        let h = setup();
        for (package, name) in [("ghost", "web"), ("shop", "ghost"), ("shop", "../../x")] {
            let id = SiteId {
                package: package.into(),
                site: name.into(),
            };
            assert!(matches!(
                h.set_exposure(&id, LOCAL, None),
                Err(ApiError::NotFound(_))
            ));
        }
        assert_eq!(h.sys.get(STATE), None);
    }

    #[test]
    fn single_address_site_allows_only_one_exposure_whatever_the_client_sends() {
        let h = setup();
        for exposure in [
            BOTH,
            Exposure {
                local: true,
                onion: true,
                public: false,
            },
        ] {
            assert!(matches!(
                go(&h, "single", exposure),
                Err(ApiError::Failed(_))
            ));
        }
        assert_eq!(h.sys.get(STATE), None);
        go(&h, "single", PUBLIC).unwrap();
    }

    #[test]
    fn going_public_needs_the_typed_host() {
        let h = setup();
        for confirmation in [
            None,
            Some(typed("")),
            Some(typed("SHOP.example.com")),
            Some(typed("evil.example.com")),
        ] {
            assert!(matches!(
                h.set_exposure(&site("web"), BOTH, confirmation.as_ref()),
                Err(ApiError::NeedsConfirmation(_))
            ));
        }
        assert_eq!(h.sys.get(STATE), None);
        assert_eq!(
            h.set_exposure(&site("web"), BOTH, Some(&typed(HOST))),
            Ok(Response::Ok)
        );
    }

    #[test]
    fn local_and_onion_and_going_offline_need_no_confirmation() {
        let h = setup();
        h.set_exposure(&site("web"), LOCAL, None).unwrap();
        go(&h, "web", BOTH).unwrap();
        // Staying public while changing the rest, then turning public off.
        h.set_exposure(&site("web"), PUBLIC, None).unwrap();
        h.set_exposure(&site("web"), Exposure::default(), None)
            .unwrap();
        assert_eq!(saved(&h)["shop"]["web"], Exposure::default());
    }

    #[test]
    fn going_public_without_a_host_is_refused() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let h = helper(sys);
        assert!(matches!(
            h.set_exposure(&site("web"), PUBLIC, Some(&typed(HOST))),
            Err(ApiError::NeedsConfirmation(_))
        ));
    }

    #[test]
    fn host_names_are_validated_and_frozen_while_public() {
        let h = setup();
        for bad in [
            "",
            "localhost",
            "Shop.example.com",
            "a..b",
            "-a.com",
            "a.com.",
            "1.2.3.4",
            "a b.com",
            "a_b.com",
        ] {
            assert!(h.set_public_host(&site("web"), bad).is_err(), "{bad}");
        }
        go(&h, "web", PUBLIC).unwrap();
        assert!(h
            .set_public_host(&site("web"), "other.example.com")
            .is_err());
        assert!(h.set_public_host(&site("web"), HOST).is_ok());
    }

    #[test]
    fn sites_list_every_web_entry_with_its_state() {
        let h = setup();
        go(&h, "web", BOTH).unwrap();
        let Ok(Response::Sites(sites)) = h.list_sites() else {
            panic!()
        };
        assert_eq!(sites.len(), 2);
        let web = sites.iter().find(|s| s.id.site == "web").unwrap();
        assert_eq!(web.exposure, BOTH);
        assert_eq!(web.public_host.as_deref(), Some(HOST));
        assert!(web.multi_address);
        let single = sites.iter().find(|s| s.id.site == "single").unwrap();
        assert_eq!(single.exposure, Exposure::default());
    }

    #[test]
    fn write_failure_is_reported() {
        let h = setup();
        h.sys.fail("write /s/exposure");
        assert!(matches!(go(&h, "web", BOTH), Err(ApiError::Failed(_))));
    }

    #[test]
    fn corrupt_state_is_not_overwritten() {
        let h = setup();
        h.sys.put(STATE, b"[[[");
        assert!(matches!(go(&h, "web", BOTH), Err(ApiError::Failed(_))));
        assert_eq!(h.sys.get(STATE).unwrap(), b"[[[");
    }

    struct FailingCaddy;
    impl ExposureApplier for FailingCaddy {
        fn apply(&self, _: &ExposureState) -> Result<(), String> {
            Err("caddy refused".to_owned())
        }
    }

    fn with_failing_caddy(h: Helper<FakeSystem>) -> Helper<FakeSystem> {
        Helper::new(
            h.sys,
            h.allowed,
            Box::new(TestPolicy),
            Box::new(FailingCaddy),
        )
        .with_paths(h.paths)
    }

    #[test]
    fn failed_apply_restores_the_previous_state() {
        let h = setup();
        h.sys.put(
            STATE,
            b"[shop.web]\nlocal = true\nonion = false\npublic = false\n",
        );
        let h = with_failing_caddy(h);
        assert!(matches!(go(&h, "web", BOTH), Err(ApiError::Failed(_))));
        assert_eq!(saved(&h)["shop"]["web"], LOCAL);
    }

    #[test]
    fn failed_first_apply_leaves_no_state() {
        let h = with_failing_caddy(setup());
        assert!(go(&h, "web", BOTH).is_err());
        assert_eq!(h.sys.get(STATE), None);
    }
}
