//! One change across several packages: write every file, restart what changed, health check,
//! and put all of it back if anything fails.
//!
//! A settings change, a role binding or a grant can change what other packages resolve, so
//! their files are planned against the new state and written in the same change. Nothing
//! restarts until every file is written and the firewall has accepted every fragment.

use std::path::{Path, PathBuf};

use nodo_api::{ApiError, Response, Rollback, RollbackStage};
use nodo_gen::{generate_resolved, Artifacts, Profile, Resolution, SECRETS_ROOT, VOLUME_ROOT};
use zeroize::Zeroizing;

use crate::catalog::Package;
use crate::health::probe_for;
use crate::helper::Helper;
use crate::settings::{Plan, PlannedFile, ServiceInputs, Target};
use crate::system::{Probe, SvAction, SysResult, System, WriteOptions};

const SCRIPT: u32 = 0o755;
const FRAGMENT: u32 = 0o644;

/// One file a change writes.
pub(crate) struct Write {
    pub path: PathBuf,
    /// Wiped on drop, because rendered configuration can hold secrets.
    pub bytes: Zeroizing<Vec<u8>>,
    pub mode: u32,
    pub group: Option<String>,
    /// Created with this mode when missing.
    pub parent_mode: u32,
}

impl Write {
    pub fn new(path: PathBuf, bytes: Vec<u8>, options: WriteOptions, parent_mode: u32) -> Write {
        Write {
            path,
            bytes: Zeroizing::new(bytes),
            mode: options.mode,
            group: options.group.map(str::to_owned),
            parent_mode,
        }
    }

    fn options(&self) -> WriteOptions<'_> {
        WriteOptions {
            mode: self.mode,
            group: self.group.as_deref(),
        }
    }

    /// Writes the file, creating its directory. In-memory settings directories are made
    /// traversable but not listable, so package names do not show there.
    pub fn apply(&self, sys: &dyn System) -> SysResult<()> {
        if let Some(dir) = self.path.parent() {
            let root = Path::new(SECRETS_ROOT);
            if let Ok(inside) = self.path.strip_prefix(root) {
                sys.ensure_dir(root, 0o711, None)?;
                let package_dir = inside.components().next().map(|c| root.join(c));
                if let Some(package_dir) = package_dir.filter(|d| d != dir && d != &self.path) {
                    sys.ensure_dir(&package_dir, 0o711, None)?;
                }
            }
            sys.ensure_dir(dir, self.parent_mode, None)?;
        }
        sys.atomic_write(&self.path, &self.bytes, self.options())
    }
}

impl From<PlannedFile> for Write {
    fn from(file: PlannedFile) -> Self {
        Write {
            path: file.path,
            bytes: file.bytes,
            mode: file.mode,
            group: file.group,
            parent_mode: file.parent_mode,
        }
    }
}

/// The files of one package, or of a store, written as part of a change.
pub(crate) struct Unit {
    /// Restarted and health checked when any of its files changed. `None` for a store.
    package: Option<(Package, Option<Probe>)>,
    writes: Vec<Write>,
    /// The package's firewall fragment, when its service files are regenerated.
    fragment: Option<PathBuf>,
    /// Directories its regenerated service needs.
    directories: Vec<nodo_gen::Directory>,
}

impl Unit {
    pub fn store(write: Write) -> Unit {
        Unit {
            package: None,
            writes: vec![write],
            fragment: None,
            directories: Vec::new(),
        }
    }
}

/// What was there before each write, for putting it back.
struct Saved {
    path: PathBuf,
    previous: Option<Zeroizing<Vec<u8>>>,
    mode: u32,
    group: Option<String>,
}

#[derive(Default)]
struct Transaction {
    saved: Vec<Saved>,
    /// Packages whose files changed, in the order they restart.
    changed: Vec<(Package, Option<Probe>)>,
    firewall_changed: bool,
}

impl<S: System> Helper<S> {
    /// The service files a package's settings imply. Only for apps this helper installed: base
    /// system services ship their own, and an app that is not installed has none to replace.
    pub(crate) fn service_artifacts(
        &self,
        package: &Package,
        inputs: &ServiceInputs,
    ) -> Result<Option<Artifacts>, ApiError> {
        if package.profile == Profile::System
            || !self.sys.exists(&self.paths.installed.join(package.id()))
        {
            return Ok(None);
        }
        let mut grants = inputs.grants.clone();
        grants.data_mount = self
            .data_mount_of(package.id())?
            .map(|mount| mount.display().to_string());
        let resolution = Resolution {
            exec: inputs.exec.clone(),
            sealed_settings: inputs.sealed,
        };
        generate_resolved(&package.manifest, package.profile, &grants, &resolution)
            .map(Some)
            .map_err(|e| ApiError::Failed(format!("{}: {e}", package.id())))
    }

    /// A package's planned settings plus its regenerated service files.
    pub(crate) fn unit_for(&self, package: &Package, plan: Plan) -> Result<Unit, ApiError> {
        let probe = package
            .manifest
            .health
            .as_ref()
            .map(probe_for)
            .transpose()
            .map_err(|e| ApiError::Failed(format!("{}: {e}", package.id())))?;
        let artifacts = self.service_artifacts(package, &plan.service)?;
        let mut writes: Vec<Write> = plan.files.into_iter().map(Write::from).collect();
        let mut fragment = None;
        let mut directories = Vec::new();
        if let Some(artifacts) = artifacts {
            let service = self.paths.sv_dir.join(package.id());
            let script = |path: PathBuf, text: &str, parent_mode| {
                Write::new(
                    path,
                    text.as_bytes().to_vec(),
                    WriteOptions {
                        mode: SCRIPT,
                        group: None,
                    },
                    parent_mode,
                )
            };
            writes.push(script(service.join("run"), &artifacts.run, 0o755));
            writes.push(script(service.join("log/run"), &artifacts.log_run, 0o755));
            writes.push(script(
                service.join("control/t"),
                &artifacts.control_t,
                0o755,
            ));
            let path = self.paths.nft_dir.join(format!("{}.nft", package.id()));
            writes.push(Write::new(
                path.clone(),
                artifacts.nft.into_bytes(),
                WriteOptions {
                    mode: FRAGMENT,
                    group: None,
                },
                0o755,
            ));
            fragment = Some(path);
            directories = artifacts.directories;
        }
        Ok(Unit {
            package: Some((package.clone(), probe)),
            writes,
            fragment,
            directories,
        })
    }

    /// Re-plans packages with no changes of their own, for a change elsewhere. A package whose
    /// storage or secrets are locked is skipped: its settings are rendered again at the next
    /// unlock, and it cannot run before that anyway.
    pub(crate) fn replan_units(&self, packages: &[Package]) -> Result<Vec<Unit>, ApiError> {
        let (all, _) = self.packages()?;
        let mut units = Vec::new();
        for package in packages {
            if package.manifest.settings.is_none() || self.is_locked(package)? {
                continue;
            }
            let plan = match self.with_settings(&all, |ctx| self.settings.plan(ctx, package, &[])) {
                Ok(plan) => plan,
                Err(ApiError::Locked(_)) => continue,
                Err(e) => return Err(prefixed(package.id(), e)),
            };
            units.push(self.unit_for(package, plan)?);
        }
        Ok(units)
    }

    /// Units for every package resolving through the target, planned against what is on disk
    /// now, so after the change's own files are written.
    pub(crate) fn dependent_units(
        &self,
        target: &Target,
        exclude: &[&str],
    ) -> Result<Vec<Unit>, ApiError> {
        let (all, _) = self.packages()?;
        let ids = self.with_settings(&all, |ctx| self.settings.dependents(ctx, target))?;
        let dependents: Vec<Package> = all
            .iter()
            .filter(|p| ids.iter().any(|id| id == p.id()) && !exclude.contains(&p.id()))
            .cloned()
            .collect();
        self.replan_units(&dependents)
    }

    /// Writes the first unit, then the units `more` plans from the result, then restarts every
    /// package whose files changed, in order. Any failure restores every file and restarts the
    /// packages already restarted, so nothing is left on a mix of old and new settings.
    pub(crate) fn transact(
        &self,
        first: Unit,
        more: impl FnOnce() -> Result<Vec<Unit>, ApiError>,
    ) -> Result<Response, ApiError> {
        let mut txn = Transaction::default();
        let first_id = first.package.as_ref().map(|(p, _)| p.id().to_owned());
        if let Err(e) = self.stage(&mut txn, first) {
            return Err(self.abandon(&txn, e));
        }
        let followers = match more() {
            Ok(units) => units,
            Err(e) => return Err(self.abandon(&txn, e)),
        };
        for unit in followers {
            if let Err(e) = self.stage(&mut txn, unit) {
                return Err(self.abandon(&txn, e));
            }
        }
        if txn.firewall_changed {
            if let Err(e) = self.sys.nft_load(&self.paths.nft_main) {
                return Err(self.abandon(&txn, e.into()));
            }
        }

        for (index, (package, probe)) in txn.changed.iter().enumerate() {
            let Err((stage, reason)) = self.restart_and_check(package, probe.as_ref()) else {
                continue;
            };
            let reason = if Some(package.id()) == first_id.as_deref() {
                reason
            } else {
                format!(
                    "{} depends on this change and failed: {reason}",
                    package.id()
                )
            };
            if let Err(problem) = self.restore_all(&txn) {
                return Err(ApiError::Failed(format!(
                    "{reason}. The old settings could not be restored: {problem}"
                )));
            }
            let problems: Vec<String> = txn.changed[..=index]
                .iter()
                .filter_map(|(package, probe)| {
                    self.restart_and_check(package, probe.as_ref())
                        .err()
                        .map(|(_, problem)| format!("{}: {problem}", package.id()))
                })
                .collect();
            let recovery_problem = (!problems.is_empty()).then(|| problems.join("; "));
            return Ok(Response::RolledBack(Rollback {
                stage,
                reason,
                recovered: recovery_problem.is_none(),
                recovery_problem,
            }));
        }
        Ok(Response::Ok)
    }

    /// Writes one unit's changed files, saving what each replaced. A new firewall fragment is
    /// checked under another name first, because the main ruleset loads every fragment in one
    /// transaction and a single bad one would fail all of them.
    fn stage(&self, txn: &mut Transaction, unit: Unit) -> Result<(), ApiError> {
        let mut previous = Vec::with_capacity(unit.writes.len());
        for write in &unit.writes {
            previous.push(self.sys.read_file(&write.path)?.map(Zeroizing::new));
        }
        let changed: Vec<bool> = unit
            .writes
            .iter()
            .zip(&previous)
            .map(|(write, old)| old.as_deref().map(Vec::as_slice) != Some(write.bytes.as_slice()))
            .collect();
        if !changed.contains(&true) {
            return Ok(());
        }
        if let Some(fragment) = &unit.fragment {
            let index = unit.writes.iter().position(|w| &w.path == fragment);
            if let Some(index) = index.filter(|i| changed[*i]) {
                let pending = fragment.with_extension("nft.pending");
                let write = &unit.writes[index];
                self.sys
                    .atomic_write(&pending, &write.bytes, write.options())?;
                let checked = self.sys.nft_check(&pending);
                let _ = self.sys.remove_file(&pending);
                checked?;
                txn.firewall_changed = true;
            }
        }
        for dir in &unit.directories {
            let path = Path::new(&dir.path);
            // Directories inside a volume exist only while it is mounted, and the helper
            // creates them when it mounts it.
            if path.starts_with(VOLUME_ROOT) {
                continue;
            }
            self.sys.ensure_dir(path, dir.mode, dir.owner.as_deref())?;
        }
        for ((write, old), changed) in unit.writes.into_iter().zip(previous).zip(changed) {
            if !changed {
                continue;
            }
            // Saved before writing, so a write that fails part way is still put back.
            txn.saved.push(Saved {
                path: write.path.clone(),
                previous: old,
                mode: write.mode,
                group: write.group.clone(),
            });
            write.apply(&self.sys)?;
        }
        if let Some(package) = unit.package {
            txn.changed.push(package);
        }
        Ok(())
    }

    /// Puts back everything written so far, before anything restarted.
    fn abandon(&self, txn: &Transaction, error: ApiError) -> ApiError {
        match self.restore_all(txn) {
            Ok(()) => error,
            Err(problem) => ApiError::Failed(format!(
                "{}. The old settings could not be restored: {problem}",
                message(&error)
            )),
        }
    }

    /// Restores in reverse order and tries every file, so one failure does not leave the rest
    /// on new settings. The firewall is reloaded when a fragment was put back.
    fn restore_all(&self, txn: &Transaction) -> Result<(), String> {
        let mut problems = Vec::new();
        for old in txn.saved.iter().rev() {
            let options = WriteOptions {
                mode: old.mode,
                group: old.group.as_deref(),
            };
            let result = match &old.previous {
                Some(bytes) => self.sys.atomic_write(&old.path, bytes, options),
                None => self.sys.remove_file(&old.path),
            };
            if let Err(e) = result {
                problems.push(format!("{}: {}", old.path.display(), e.0));
            }
        }
        if txn.firewall_changed {
            if let Err(e) = self.sys.nft_load(&self.paths.nft_main) {
                problems.push(format!("firewall: {}", e.0));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }

    fn restart_and_check(
        &self,
        package: &Package,
        probe: Option<&Probe>,
    ) -> Result<(), (RollbackStage, String)> {
        let wait = Self::stop_wait(package);
        self.sys
            .sv(SvAction::ForceRestart, package.id(), wait)
            .map_err(|e| (RollbackStage::Restart, format!("restart failed: {e}")))?;
        self.wait_healthy(package.id(), probe).map_err(|e| {
            (
                RollbackStage::HealthCheck,
                format!("health check failed: {e}"),
            )
        })
    }
}

fn message(error: &ApiError) -> String {
    match error {
        ApiError::Failed(text) => text.clone(),
        other => other.to_string(),
    }
}

fn prefixed(id: &str, error: ApiError) -> ApiError {
    ApiError::Failed(format!("{id}: {}", message(&error)))
}
