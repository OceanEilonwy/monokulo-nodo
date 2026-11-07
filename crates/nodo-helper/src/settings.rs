use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use nodo_api::{ApiError, Response};
use nodo_gen::service_user;
use nodo_manifest::Manifest;

use crate::catalog::Package;
use crate::health::probe_for;
use crate::helper::{lock, Helper};
use crate::system::{Probe, SvAction, System, WriteOptions};

const SETTINGS_FILE: &str = "settings.toml";
const MAX_KEY_LEN: usize = 128;
const MAX_VALUE_LEN: usize = 4096;
const MAX_VALUES: usize = 512;

/// Validates new values and renders the settings file.
///
/// Package settings are described in sandboxed Lua, which never runs as root. So schema
/// validation has to happen in the unprivileged Lua host, and the result is what reaches this
/// trait. The helper keeps the final say over what it writes to disk.
pub trait SettingsPolicy: Send + Sync {
    /// Returns the complete new file. `current` is the existing file, if there is one.
    fn render(
        &self,
        manifest: &Manifest,
        current: Option<&[u8]>,
        values: &[(String, String)],
    ) -> Result<Vec<u8>, String>;
}

/// Structural validation, merging new values into a flat TOML table. It knows nothing about a
/// package's schema, and it does not write config formats such as `Cuprated.toml`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FlatTomlSettings;

fn valid_key(key: &str) -> bool {
    key.len() <= MAX_KEY_LEN
        && key.starts_with(|c: char| c.is_ascii_lowercase())
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        })
}

impl SettingsPolicy for FlatTomlSettings {
    fn render(
        &self,
        _manifest: &Manifest,
        current: Option<&[u8]>,
        values: &[(String, String)],
    ) -> Result<Vec<u8>, String> {
        if values.is_empty() {
            return Err("no settings given".to_owned());
        }
        if values.len() > MAX_VALUES {
            return Err("too many settings".to_owned());
        }
        let mut seen = BTreeSet::new();
        for (key, value) in values {
            if !valid_key(key) {
                return Err(format!("invalid setting name: {}", key.escape_debug()));
            }
            if !seen.insert(key.as_str()) {
                return Err(format!("setting given twice: {key}"));
            }
            if value.len() > MAX_VALUE_LEN {
                return Err(format!("value too long for {key}"));
            }
            if value.chars().any(char::is_control) {
                return Err(format!("value for {key} contains a control character"));
            }
        }
        let mut merged: BTreeMap<String, String> = match current {
            Some(bytes) => std::str::from_utf8(bytes)
                .map_err(|_| "existing settings are not UTF-8".to_owned())
                .and_then(|text| {
                    toml::from_str(text).map_err(|_| "existing settings are corrupt".to_owned())
                })?,
            None => BTreeMap::new(),
        };
        merged.extend(values.iter().cloned());
        toml::to_string(&merged)
            .map(String::into_bytes)
            .map_err(|e| e.to_string())
    }
}

impl<S: System> Helper<S> {
    fn settings_path(&self, package: &Package) -> Result<PathBuf, ApiError> {
        Ok(Path::new(&package.layout()?.config_dir).join(SETTINGS_FILE))
    }

    /// Validate, write atomically, restart, health check, and restore on any failure past the
    /// write. A failure up to and including the write leaves the service untouched.
    pub(crate) fn apply_settings(
        &self,
        id: &str,
        values: &[(String, String)],
    ) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        if package.manifest.settings.is_none() {
            return Err(ApiError::Failed(format!("{id} has no settings")));
        }
        let _guard = lock(&self.service_lock);
        // A locked service cannot pass a health check, and rolling back would misreport why.
        if self.is_locked(&package)? {
            return Err(ApiError::Failed(format!("{id} is locked")));
        }
        let probe = package
            .manifest
            .health
            .as_ref()
            .map(probe_for)
            .transpose()
            .map_err(ApiError::Failed)?;
        let path = self.settings_path(&package)?;
        let previous = self.sys.read_file(&path)?;
        let rendered = self
            .settings
            .render(&package.manifest, previous.as_deref(), values)
            .map_err(ApiError::Failed)?;
        if previous.as_deref() == Some(rendered.as_slice()) {
            return Ok(Response::Ok);
        }
        let user = service_user(id).map_err(|e| ApiError::Failed(e.to_string()))?;
        // Sandboxes see this file through a bind, and it can hold values other users should not
        // read, so it is readable only by the package's own group.
        let options = WriteOptions {
            mode: 0o640,
            group: Some(&user),
        };
        self.sys.atomic_write(&path, &rendered, options)?;

        let reason = match self.restart_and_check(&package, probe.as_ref()) {
            Ok(()) => return Ok(Response::Ok),
            Err(reason) => reason,
        };
        self.restore(&path, previous.as_deref(), options)
            .map_err(|e| {
                ApiError::Failed(format!(
                    "{reason}. The old settings could not be restored: {e}"
                ))
            })?;
        let reason = match self.restart_and_check(&package, probe.as_ref()) {
            Ok(()) => reason,
            Err(e) => format!("{reason}. Old settings restored, but the service is unhealthy: {e}"),
        };
        Ok(Response::RolledBack { reason })
    }

    fn restart_and_check(&self, package: &Package, probe: Option<&Probe>) -> Result<(), String> {
        let wait = Self::stop_wait(package);
        self.sys
            .sv(SvAction::ForceRestart, package.id(), wait)
            .map_err(|e| format!("restart failed: {e}"))?;
        self.wait_healthy(package.id(), probe)
            .map_err(|e| format!("health check failed: {e}"))
    }

    fn restore(
        &self,
        path: &Path,
        previous: Option<&[u8]>,
        options: WriteOptions,
    ) -> Result<(), String> {
        match previous {
            Some(bytes) => self.sys.atomic_write(path, bytes, options),
            None => self.sys.remove_file(path),
        }
        .map_err(|e| e.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, ENCRYPTED, SHOP};

    const PATH: &str = "/etc/apps/shop/settings.toml";
    const OLD: &[u8] = b"color = \"blue\"\n";

    fn setup() -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.put(PATH, OLD);
        helper(sys)
    }

    fn apply(h: &Helper<FakeSystem>) -> Result<Response, ApiError> {
        h.apply_settings("shop", &[("color".to_owned(), "red".to_owned())])
    }

    fn sv_restarts(h: &Helper<FakeSystem>) -> usize {
        h.sys
            .calls()
            .iter()
            .filter(|c| c.starts_with("sv force-restart"))
            .count()
    }

    #[test]
    fn success_writes_restarts_and_checks_in_order() {
        let h = setup();
        assert_eq!(apply(&h), Ok(Response::Ok));
        assert_eq!(h.sys.get(PATH).unwrap(), b"color = \"red\"\n");
        let calls = h.sys.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle)).unwrap();
        assert!(at("write") < at("sv force-restart"));
        assert!(at("sv force-restart") < at("probe"));
        assert_eq!(sv_restarts(&h), 1);
    }

    #[test]
    fn written_file_is_only_readable_by_the_package() {
        let h = setup();
        apply(&h).unwrap();
        assert!(h
            .sys
            .calls()
            .contains(&format!("write {PATH} mode=640 group=_shop")));
    }

    #[test]
    fn unknown_package_touches_nothing() {
        let h = setup();
        assert!(matches!(
            h.apply_settings("ghost", &[]),
            Err(ApiError::NotFound(_))
        ));
        assert_eq!(sv_restarts(&h), 0);
    }

    #[test]
    fn package_without_settings_is_refused() {
        let sys = FakeSystem::default();
        sys.add_manifest(ENCRYPTED);
        sys.set_mounted("/run/nodo/volumes/vault");
        let h = helper(sys);
        assert!(matches!(
            h.apply_settings("vault", &[("a".to_owned(), "b".to_owned())]),
            Err(ApiError::Failed(_))
        ));
    }

    #[test]
    fn validation_failure_touches_nothing() {
        let h = setup();
        for values in [
            vec![],
            vec![("Bad Key".to_owned(), "x".to_owned())],
            vec![("a\nb".to_owned(), "x".to_owned())],
            vec![("a".to_owned(), "line\nbreak".to_owned())],
            vec![
                ("a".to_owned(), "1".to_owned()),
                ("a".to_owned(), "2".to_owned()),
            ],
            vec![("a".to_owned(), "x".repeat(MAX_VALUE_LEN + 1))],
        ] {
            assert!(matches!(
                h.apply_settings("shop", &values),
                Err(ApiError::Failed(_))
            ));
        }
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
        assert_eq!(sv_restarts(&h), 0);
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("write")));
    }

    #[test]
    fn hostile_value_stays_data_in_the_file() {
        let h = setup();
        let value = "\" = 1\n[evil]\nx = \"".replace('\n', " ");
        h.apply_settings("shop", &[("color".to_owned(), value.clone())])
            .unwrap();
        let text = String::from_utf8(h.sys.get(PATH).unwrap()).unwrap();
        let parsed: BTreeMap<String, String> = toml::from_str(&text).unwrap();
        assert_eq!(parsed["color"], value);
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn values_merge_with_existing_settings() {
        let h = setup();
        h.apply_settings("shop", &[("size".to_owned(), "big".to_owned())])
            .unwrap();
        let text = String::from_utf8(h.sys.get(PATH).unwrap()).unwrap();
        let parsed: BTreeMap<String, String> = toml::from_str(&text).unwrap();
        assert_eq!(parsed["color"], "blue");
        assert_eq!(parsed["size"], "big");
    }

    #[test]
    fn corrupt_existing_settings_are_not_overwritten() {
        let h = setup();
        h.sys.put(PATH, b"[[[not toml");
        assert!(matches!(apply(&h), Err(ApiError::Failed(_))));
        assert_eq!(h.sys.get(PATH).unwrap(), b"[[[not toml");
    }

    #[test]
    fn unchanged_settings_do_not_restart() {
        let h = setup();
        let values = [("color".to_owned(), "blue".to_owned())];
        h.sys.put(
            PATH,
            toml::to_string(&BTreeMap::from([("color", "blue")]))
                .unwrap()
                .as_bytes(),
        );
        assert_eq!(h.apply_settings("shop", &values), Ok(Response::Ok));
        assert_eq!(sv_restarts(&h), 0);
    }

    #[test]
    fn locked_package_is_refused_before_anything_changes() {
        let sys = FakeSystem::default();
        sys.put(
            "/m/app/vault.toml",
            b"api_version = 1\nid = \"vault\"\nversion = \"1\"\nexec = [\"a\"]\nsettings = \"s.lua\"\n[storage]\nencrypted = true\n",
        );
        let h = helper(sys);
        assert!(matches!(
            h.apply_settings("vault", &[("a".to_owned(), "b".to_owned())]),
            Err(ApiError::Failed(_))
        ));
        assert_eq!(sv_restarts(&h), 0);
    }

    #[test]
    fn write_failure_keeps_the_old_file_and_does_not_restart() {
        let h = setup();
        h.sys.fail("write");
        assert!(matches!(apply(&h), Err(ApiError::Failed(_))));
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
        assert_eq!(sv_restarts(&h), 0);
    }

    #[test]
    fn read_failure_stops_before_any_change() {
        let h = setup();
        h.sys.fail(&format!("read {PATH}"));
        assert!(matches!(apply(&h), Err(ApiError::Failed(_))));
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
        assert_eq!(sv_restarts(&h), 0);
    }

    #[test]
    fn restart_failure_restores_and_rolls_back() {
        let h = setup();
        h.sys.fail_once("sv force-restart");
        let Ok(Response::RolledBack { reason }) = apply(&h) else {
            panic!()
        };
        assert!(reason.contains("restart failed"), "{reason}");
        assert!(!reason.contains("unhealthy"), "{reason}");
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
        assert_eq!(sv_restarts(&h), 2);
    }

    #[test]
    fn failed_health_check_restores_and_rolls_back() {
        let h = setup();
        // Every attempt of the first check fails, then the restored service recovers.
        h.sys.script_probes(&[false; 5]);
        let Ok(Response::RolledBack { reason }) = apply(&h) else {
            panic!()
        };
        assert!(reason.contains("health check failed"), "{reason}");
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
        assert_eq!(sv_restarts(&h), 2);
    }

    #[test]
    fn slow_start_passes_within_the_attempt_budget() {
        let h = setup();
        h.sys.script_probes(&[false, false, false, true]);
        assert_eq!(apply(&h), Ok(Response::Ok));
        assert_eq!(sv_restarts(&h), 1);
        assert_eq!(h.sys.sleeps().len(), 3);
    }

    #[test]
    fn service_that_dies_after_restart_rolls_back() {
        let h = setup();
        h.sys
            .script_status("shop", &["down: shop: 1s, normally up, want up\n"; 5]);
        let Ok(Response::RolledBack { reason }) = apply(&h) else {
            panic!()
        };
        assert!(reason.contains("not running"), "{reason}");
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
    }

    #[test]
    fn crash_looping_service_without_a_health_check_rolls_back() {
        let sys = FakeSystem::default();
        sys.put(
            "/m/app/shop.toml",
            b"api_version = 1\nid = \"shop\"\nversion = \"1\"\nexec = [\"a\"]\nsettings = \"s.lua\"\n",
        );
        sys.put(PATH, OLD);
        sys.script_status("shop", &["run: shop: (pid 9) 0s\n"; 5]);
        let h = helper(sys);
        let Ok(Response::RolledBack { reason }) = apply(&h) else {
            panic!()
        };
        assert!(reason.contains("stayed up"), "{reason}");
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
    }

    #[test]
    fn first_settings_are_removed_on_rollback() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.script_probes(&[false; 5]);
        let h = helper(sys);
        assert!(matches!(apply(&h), Ok(Response::RolledBack { .. })));
        assert_eq!(h.sys.get(PATH), None);
    }

    #[test]
    fn failed_restore_write_is_an_error_not_a_rollback() {
        let h = setup();
        h.sys.script_probes(&[false; 5]);
        h.sys.fail_after("write", 1);
        let Err(ApiError::Failed(message)) = apply(&h) else {
            panic!()
        };
        assert!(message.contains("could not be restored"), "{message}");
        assert_eq!(sv_restarts(&h), 1, "must not restart onto unknown settings");
    }

    #[test]
    fn failed_restore_remove_is_an_error_not_a_rollback() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.script_probes(&[false; 5]);
        sys.fail("remove");
        let h = helper(sys);
        let Err(ApiError::Failed(message)) = apply(&h) else {
            panic!()
        };
        assert!(message.contains("could not be restored"), "{message}");
    }

    #[test]
    fn restart_failure_during_rollback_is_reported() {
        let h = setup();
        h.sys.fail("sv force-restart");
        let Ok(Response::RolledBack { reason }) = apply(&h) else {
            panic!()
        };
        assert!(reason.contains("restart failed"), "{reason}");
        assert!(reason.contains("unhealthy"), "{reason}");
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
    }

    #[test]
    fn health_failure_during_rollback_is_reported() {
        let h = setup();
        h.sys.script_probes(&[false; 10]);
        let Ok(Response::RolledBack { reason }) = apply(&h) else {
            panic!()
        };
        assert!(reason.contains("unhealthy"), "{reason}");
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
    }

    #[test]
    fn hostile_health_url_is_refused_before_any_change() {
        let sys = FakeSystem::default();
        sys.put(
            "/m/app/shop.toml",
            b"api_version = 1\nid = \"shop\"\nversion = \"1\"\nexec = [\"a\"]\nsettings = \"s.lua\"\n\n[health.http]\nurl = \"http://169.254.169.254/\"\n",
        );
        sys.put(PATH, OLD);
        let h = helper(sys);
        assert!(matches!(apply(&h), Err(ApiError::Failed(_))));
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
        assert_eq!(sv_restarts(&h), 0);
    }
}
