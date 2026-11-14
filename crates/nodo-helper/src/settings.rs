//! Applying settings: write the planned files, restart, health check, and roll back on failure.
//!
//! What to write is a [`SettingsPolicy`]'s decision. This module only guarantees that the
//! service, and every package whose settings follow from it, either runs on all of the new
//! files or is put back on all of the old ones.

use std::path::PathBuf;

use nodo_api::{ApiError, ReferenceOption, Response, SettingChange, SettingPreview, SettingsForm};
use nodo_gen::ResolvedGrants;
use zeroize::Zeroizing;

use crate::catalog::Package;
use crate::health::probe_for;
use crate::helper::{lock, Helper, Paths};
use crate::secrets::SecretsKey;
use crate::stores::Bindings;
use crate::system::System;

/// Upper bound on how many changes one request may carry.
pub const MAX_CHANGES: usize = 512;

/// One file a settings change produces.
pub struct PlannedFile {
    pub path: PathBuf,
    /// Wiped on drop, because rendered configuration can hold secrets.
    pub bytes: Zeroizing<Vec<u8>>,
    pub mode: u32,
    /// Group that may read the file. `None` leaves it as root's.
    pub group: Option<String>,
    /// Created with this mode when missing.
    pub parent_mode: u32,
}

/// What a policy may look at. Reads go through the same [`System`] as everything else.
pub struct SettingsContext<'a> {
    pub sys: &'a dyn System,
    pub paths: &'a Paths,
    /// Every installed package, for resolving references between them.
    pub packages: &'a [Package],
    /// Present between an unlock and the helper's exit. Without it, sealed secrets can be
    /// neither read nor written.
    pub secrets: Option<&'a SecretsKey>,
    /// Whether the device passphrase exists, which decides how a missing key is explained.
    pub set_up: bool,
    /// Role bindings and approved grants, which resolution follows.
    pub bindings: &'a Bindings,
}

/// What a package's settings decide about its service, beyond its own files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceInputs {
    /// The command line with references substituted. `None` runs the manifest's own.
    pub exec: Option<Vec<String>>,
    /// Loopback ports and sockets of other packages that its references reach. A reference that
    /// goes away takes its access with it, because this is computed afresh every time.
    pub grants: ResolvedGrants,
    /// Its settings hold secrets sealed under the device key, so they render to memory.
    pub sealed: bool,
}

/// Every file a change produces, in the order to write them, and what it means for the service.
pub struct Plan {
    pub files: Vec<PlannedFile>,
    pub service: ServiceInputs,
}

/// Something settings can resolve through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Package(String),
    Role(String),
}

/// Turns typed changes into files, and describes settings for the form.
///
/// Package settings are described in sandboxed Lua that never runs as root, so a policy that
/// needs Lua must use [`System::settings_worker`]. The helper keeps the final say over what it
/// writes and when.
pub trait SettingsPolicy: Send + Sync {
    fn form(&self, ctx: &SettingsContext, package: &Package) -> Result<SettingsForm, ApiError>;

    /// Every file the change produces, in the order to write them. Must not change anything.
    fn plan(
        &self,
        ctx: &SettingsContext,
        package: &Package,
        changes: &[SettingChange],
    ) -> Result<Plan, ApiError>;

    /// Service inputs of a package without settings, whose command line may still reference
    /// other packages.
    fn service(
        &self,
        _ctx: &SettingsContext,
        _package: &Package,
    ) -> Result<ServiceInputs, ApiError> {
        Ok(ServiceInputs::default())
    }

    /// Packages whose resolved settings depend on the target, directly or through a chain.
    fn dependents(
        &self,
        _ctx: &SettingsContext,
        _target: &Target,
    ) -> Result<Vec<String>, ApiError> {
        Ok(Vec::new())
    }

    /// Seals secrets still stored in clear, such as those written before sealing existed, and
    /// returns how many. Needs the secrets key.
    fn seal_stored(&self, _ctx: &SettingsContext) -> Result<usize, ApiError> {
        Ok(0)
    }

    /// Every setting `package` could reference in the other installed packages.
    fn references(
        &self,
        _ctx: &SettingsContext,
        _package: &Package,
    ) -> Result<Vec<ReferenceOption>, ApiError> {
        Ok(Vec::new())
    }

    /// What saving one change would resolve to and imply. Must not change anything.
    fn preview(
        &self,
        _ctx: &SettingsContext,
        _package: &Package,
        _change: &SettingChange,
    ) -> Result<SettingPreview, ApiError> {
        Err(ApiError::Failed("previews are not available".into()))
    }

    /// The resolved value of a package's public setting, as text.
    fn public_value(
        &self,
        _ctx: &SettingsContext,
        _package: &Package,
        _public_name: &str,
    ) -> Result<Option<String>, ApiError> {
        Ok(None)
    }
}

impl<S: System> Helper<S> {
    /// Runs `f` with everything a policy may read: packages, bindings and, when unlocked, the
    /// secrets key.
    pub(crate) fn with_settings<T>(
        &self,
        packages: &[Package],
        f: impl FnOnce(&SettingsContext) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let bindings = self.load_bindings()?;
        let key = self.secrets_key();
        let ctx = SettingsContext {
            sys: &self.sys,
            paths: &self.paths,
            packages,
            secrets: key.as_deref(),
            set_up: self.sys.exists(&self.paths.keystore),
            bindings: &bindings,
        };
        f(&ctx)
    }

    pub(crate) fn list_references(&self, id: &str) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        let (packages, _) = self.packages()?;
        let options = self.with_settings(&packages, |ctx| self.settings.references(ctx, &package))?;
        Ok(Response::References(options))
    }

    pub(crate) fn preview_setting(&self, id: &str, change: &SettingChange) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        if package.manifest.settings.is_none() {
            return Err(ApiError::Failed(format!("{id} has no settings")));
        }
        let (packages, _) = self.packages()?;
        let preview = self.with_settings(&packages, |ctx| self.settings.preview(ctx, &package, change))?;
        Ok(Response::SettingPreview(preview))
    }

    pub(crate) fn settings_form(&self, id: &str) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        if package.manifest.settings.is_none() {
            return Err(ApiError::Failed(format!("{id} has no settings")));
        }
        let (packages, _) = self.packages()?;
        let form = self.with_settings(&packages, |ctx| self.settings.form(ctx, &package))?;
        Ok(Response::SettingsForm(form))
    }

    /// Validate, write atomically, restart, health check, and restore on any failure past the
    /// writes. Packages referencing this one are re-rendered in the same change, and a failure
    /// in any of them puts every package back.
    pub(crate) fn apply_settings(
        &self,
        id: &str,
        changes: &[SettingChange],
    ) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        if package.manifest.settings.is_none() {
            return Err(ApiError::Failed(format!("{id} has no settings")));
        }
        if changes.is_empty() {
            return Err(ApiError::Failed("no settings given".to_owned()));
        }
        if changes.len() > MAX_CHANGES {
            return Err(ApiError::Failed("too many settings".to_owned()));
        }
        let _guard = lock(&self.service_lock);
        // A locked service cannot pass a health check, and rolling back would misreport why.
        if self.is_locked(&package)? {
            return Err(ApiError::Locked(format!(
                "{id} keeps encrypted storage, which opens with the device passphrase"
            )));
        }
        package
            .manifest
            .health
            .as_ref()
            .map(probe_for)
            .transpose()
            .map_err(ApiError::Failed)?;
        let (packages, _) = self.packages()?;
        let plan =
            self.with_settings(&packages, |ctx| self.settings.plan(ctx, &package, changes))?;
        let unit = self.unit_for(&package, plan)?;
        self.transact(unit, || {
            self.dependent_units(&Target::Package(id.to_owned()), &[id])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, ENCRYPTED, SHOP, STORE};
    use nodo_api::{Rollback, RollbackStage, SettingInput, SettingLiteral};

    const PATH: &str = "/etc/apps/shop/settings.toml";
    const OLD: &[u8] = b"color = \"blue\"\n";

    fn setup() -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.put(PATH, OLD);
        helper(sys)
    }

    fn text(name: &str, value: &str) -> SettingChange {
        SettingChange {
            name: name.into(),
            value: SettingInput::Literal(SettingLiteral::Text(value.into())),
        }
    }

    fn apply(h: &Helper<FakeSystem>) -> Result<Response, ApiError> {
        h.apply_settings("shop", &[text("color", "red")])
    }

    fn rolled_back(result: Result<Response, ApiError>) -> Rollback {
        match result {
            Ok(Response::RolledBack(rollback)) => rollback,
            other => panic!("expected a rollback, got {other:?}"),
        }
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
    fn every_planned_file_is_written_with_its_own_permissions() {
        let h = setup();
        apply(&h).unwrap();
        let calls = h.sys.calls();
        assert!(calls.contains(&format!("write {PATH} mode=640 group=_shop")));
        assert!(calls.contains(&format!("write {STORE} mode=600 group=-")));
    }

    #[test]
    fn unknown_package_touches_nothing() {
        let h = setup();
        assert!(matches!(
            h.apply_settings("ghost", &[text("a", "b")]),
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
            h.apply_settings("vault", &[text("a", "b")]),
            Err(ApiError::Failed(_))
        ));
    }

    #[test]
    fn empty_or_oversized_changes_touch_nothing() {
        let h = setup();
        let many: Vec<_> = (0..=MAX_CHANGES)
            .map(|i| text(&format!("k{i}"), "v"))
            .collect();
        for changes in [vec![], many] {
            assert!(matches!(
                h.apply_settings("shop", &changes),
                Err(ApiError::Failed(_))
            ));
        }
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("write")));
    }

    #[test]
    fn policy_refusal_touches_nothing() {
        let h = setup();
        // The test policy refuses a value containing "refuse".
        assert!(matches!(
            h.apply_settings("shop", &[text("color", "refuse")]),
            Err(ApiError::Failed(_))
        ));
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
        assert_eq!(sv_restarts(&h), 0);
    }

    #[test]
    fn unchanged_settings_do_not_restart() {
        let h = setup();
        h.apply_settings("shop", &[text("color", "red")]).unwrap();
        h.sys.clear_calls();
        assert_eq!(
            h.apply_settings("shop", &[text("color", "red")]),
            Ok(Response::Ok)
        );
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
            h.apply_settings("vault", &[text("a", "b")]),
            Err(ApiError::Locked(_))
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
    fn second_write_failure_restores_the_first_file() {
        let h = setup();
        h.sys.fail_after(&format!("write {PATH}"), 0);
        assert!(matches!(apply(&h), Err(ApiError::Failed(_))));
        assert_eq!(
            h.sys.get(STORE),
            None,
            "the store written first is removed again"
        );
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
        let rollback = rolled_back(apply(&h));
        assert_eq!(rollback.stage, RollbackStage::Restart);
        assert!(rollback.reason.contains("restart failed"), "{rollback:?}");
        assert!(rollback.recovered);
        assert_eq!(rollback.recovery_problem, None);
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
        assert_eq!(h.sys.get(STORE), None);
        assert_eq!(sv_restarts(&h), 2);
    }

    #[test]
    fn failed_health_check_restores_and_rolls_back() {
        let h = setup();
        // Every attempt of the first check fails, then the restored service recovers.
        h.sys.script_probes(&[false; 5]);
        let rollback = rolled_back(apply(&h));
        assert_eq!(rollback.stage, RollbackStage::HealthCheck);
        assert!(
            rollback.reason.contains("health check failed"),
            "{rollback:?}"
        );
        assert!(rollback.recovered);
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
        let rollback = rolled_back(apply(&h));
        assert!(rollback.reason.contains("not running"), "{rollback:?}");
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
        let rollback = rolled_back(apply(&h));
        assert!(rollback.reason.contains("stayed up"), "{rollback:?}");
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
    }

    #[test]
    fn first_settings_are_removed_on_rollback() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.script_probes(&[false; 5]);
        let h = helper(sys);
        rolled_back(apply(&h));
        assert_eq!(h.sys.get(PATH), None);
        assert_eq!(h.sys.get(STORE), None);
    }

    #[test]
    fn failed_restore_write_is_an_error_not_a_rollback() {
        let h = setup();
        h.sys.script_probes(&[false; 5]);
        h.sys.fail_after(&format!("write {PATH}"), 1);
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
        let rollback = rolled_back(apply(&h));
        assert!(rollback.reason.contains("restart failed"), "{rollback:?}");
        assert!(!rollback.recovered);
        assert!(rollback
            .recovery_problem
            .unwrap()
            .contains("restart failed"));
        assert_eq!(h.sys.get(PATH).unwrap(), OLD);
    }

    #[test]
    fn health_failure_during_rollback_is_reported() {
        let h = setup();
        h.sys.script_probes(&[false; 10]);
        let rollback = rolled_back(apply(&h));
        assert!(!rollback.recovered);
        assert!(rollback
            .recovery_problem
            .unwrap()
            .contains("health check failed"));
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
