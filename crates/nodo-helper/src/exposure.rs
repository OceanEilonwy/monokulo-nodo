use std::collections::BTreeMap;

use nodo_api::{ApiError, Exposure, Response};

use crate::helper::{lock, Helper};
use crate::system::{System, WriteOptions};

/// Every site's chosen exposure, keyed by package then site.
pub type ExposureState = BTreeMap<String, BTreeMap<String, Exposure>>;

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

impl<S: System> Helper<S> {
    pub(crate) fn set_exposure(
        &self,
        id: &str,
        site: &str,
        exposure: Exposure,
    ) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        let web = package
            .manifest
            .web
            .iter()
            .find(|web| web.name == site)
            .ok_or_else(|| {
                ApiError::NotFound(format!("{id} has no site {}", site.escape_debug()))
            })?;
        let enabled = [exposure.local, exposure.onion, exposure.public];
        // An app tied to one base URL breaks when reached through a second address.
        if !web.multi_address && enabled.iter().filter(|on| **on).count() > 1 {
            return Err(ApiError::Failed(format!(
                "{site} works from a single address, so only one exposure can be on"
            )));
        }

        let _guard = lock(&self.service_lock);
        let path = &self.paths.exposure_state;
        let previous = self.sys.read_file(path)?;
        let mut state: ExposureState = match &previous {
            Some(bytes) => std::str::from_utf8(bytes)
                .ok()
                .and_then(|text| toml::from_str(text).ok())
                .ok_or_else(|| ApiError::Failed("saved exposure state is corrupt".to_owned()))?,
            None => ExposureState::new(),
        };
        state
            .entry(id.to_owned())
            .or_default()
            .insert(site.to_owned(), exposure);
        let text = toml::to_string(&state).map_err(|e| ApiError::Failed(e.to_string()))?;
        let options = WriteOptions {
            mode: 0o644,
            group: None,
        };
        self.sys.atomic_write(path, text.as_bytes(), options)?;

        if let Err(reason) = self.exposure.apply(&state) {
            // Saved state must not claim an exposure that never took effect.
            let restored = match &previous {
                Some(bytes) => self.sys.atomic_write(path, bytes, options),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, SHOP};
    use crate::settings::FlatTomlSettings;

    const STATE: &str = "/s/exposure.toml";
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

    fn setup() -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        helper(sys)
    }

    fn saved(h: &Helper<FakeSystem>) -> ExposureState {
        toml::from_str(std::str::from_utf8(&h.sys.get(STATE).unwrap()).unwrap()).unwrap()
    }

    #[test]
    fn persists_and_survives_later_changes() {
        let h = setup();
        h.set_exposure("shop", "web", BOTH).unwrap();
        assert_eq!(saved(&h)["shop"]["web"], BOTH);
        h.set_exposure("shop", "single", LOCAL).unwrap();
        let state = saved(&h);
        assert_eq!(state["shop"]["web"], BOTH);
        assert_eq!(state["shop"]["single"], LOCAL);
    }

    #[test]
    fn unknown_package_or_site_is_not_found() {
        let h = setup();
        assert!(matches!(
            h.set_exposure("ghost", "web", BOTH),
            Err(ApiError::NotFound(_))
        ));
        assert!(matches!(
            h.set_exposure("shop", "ghost", BOTH),
            Err(ApiError::NotFound(_))
        ));
        assert!(matches!(
            h.set_exposure("shop", "../../x", BOTH),
            Err(ApiError::NotFound(_))
        ));
        assert_eq!(h.sys.get(STATE), None);
    }

    #[test]
    fn single_address_site_allows_only_one_exposure() {
        let h = setup();
        assert!(matches!(
            h.set_exposure("shop", "single", BOTH),
            Err(ApiError::Failed(_))
        ));
        assert_eq!(h.sys.get(STATE), None);
        h.set_exposure("shop", "single", LOCAL).unwrap();
    }

    #[test]
    fn write_failure_is_reported() {
        let h = setup();
        h.sys.fail("write");
        assert!(matches!(
            h.set_exposure("shop", "web", BOTH),
            Err(ApiError::Failed(_))
        ));
    }

    #[test]
    fn corrupt_state_is_not_overwritten() {
        let h = setup();
        h.sys.put(STATE, b"[[[");
        assert!(matches!(
            h.set_exposure("shop", "web", BOTH),
            Err(ApiError::Failed(_))
        ));
        assert_eq!(h.sys.get(STATE).unwrap(), b"[[[");
    }

    struct FailingCaddy;
    impl ExposureApplier for FailingCaddy {
        fn apply(&self, _: &ExposureState) -> Result<(), String> {
            Err("caddy refused".to_owned())
        }
    }

    #[test]
    fn failed_apply_restores_the_previous_state() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.put(
            STATE,
            b"[shop.web]\nlocal = true\nonion = false\npublic = false\n",
        );
        let h = helper(sys);
        let h = Helper::new(
            h.sys,
            h.allowed,
            Box::new(FlatTomlSettings),
            Box::new(FailingCaddy),
        )
        .with_paths(h.paths);
        assert!(matches!(
            h.set_exposure("shop", "web", BOTH),
            Err(ApiError::Failed(_))
        ));
        assert_eq!(saved(&h)["shop"]["web"], LOCAL);
    }

    #[test]
    fn failed_first_apply_leaves_no_state() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let h = helper(sys);
        let h = Helper::new(
            h.sys,
            h.allowed,
            Box::new(FlatTomlSettings),
            Box::new(FailingCaddy),
        )
        .with_paths(h.paths);
        assert!(h.set_exposure("shop", "web", BOTH).is_err());
        assert_eq!(h.sys.get(STATE), None);
    }
}
