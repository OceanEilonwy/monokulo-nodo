//! Installing a package from a feed: review, approval, install files, then the service.
//!
//! The review downloads, verifies and inspects the package and installs nothing. The install
//! repeats all of that and proceeds only for the same version asking for exactly the permission
//! lines the user approved, so an approval cannot be stretched to cover anything else, whatever
//! the client sends.

use std::path::{Path, PathBuf};

use nodo_api::{
    ApiError, InstallReport, InstallReview, InstallStep, PermissionApproval, Response, Secret,
    StepState,
};
use nodo_feed::BlockReason;
use nodo_gen::{Layout, Profile};
use nodo_install::{InstallError, Outcome, Permissions, Reviewed};
use nodo_manifest::Manifest;

use crate::feeds::{shown_key, FeedState, InstalledRecord};
use crate::helper::{lock, Helper};
use crate::manage::failed;
use crate::system::{System, WriteOptions};

const MANIFEST: WriteOptions = WriteOptions {
    mode: 0o644,
    group: None,
};

/// How an install that could not proceed is told to the client.
pub(crate) fn refusal(package: &str, version: &str, error: InstallError) -> Result<Response, ApiError> {
    match error {
        InstallError::Rejected(violations) => Ok(Response::InstallRejected {
            package: package.to_owned(),
            version: version.to_owned(),
            reasons: violations.iter().map(ToString::to_string).collect(),
        }),
        InstallError::Signature(e) => Ok(Response::InstallRejected {
            package: package.to_owned(),
            version: version.to_owned(),
            reasons: vec![format!("its signature or contents do not match the feed: {e}")],
        }),
        other => Err(failed(other)),
    }
}

pub(crate) fn block_text(reason: &BlockReason) -> String {
    match reason {
        BlockReason::UnknownFeed { .. } => "this feed is not trusted on this device".to_owned(),
        BlockReason::KeyChanged {
            pinned, offered, ..
        } => format!(
            "the feed's signing key changed from {} to {}",
            shown_key(&pinned.to_string()),
            shown_key(&offered.to_string())
        ),
        BlockReason::Downgrade {
            highest, offered, ..
        } => format!("{offered} is older than {highest}, which was installed before"),
        BlockReason::UncomparableVersion { version, .. } => {
            format!("the version {version} cannot be compared, so it cannot be shown to be newer")
        }
    }
}

pub(crate) fn blocked(package: &str, reasons: &[BlockReason]) -> Response {
    Response::InstallBlocked {
        package: package.to_owned(),
        reasons: reasons.iter().map(block_text).collect(),
        key_changed: reasons
            .iter()
            .any(|r| matches!(r, BlockReason::KeyChanged { .. })),
    }
}

fn step(name: &str, note: impl Into<String>, state: StepState) -> InstallStep {
    InstallStep {
        name: name.to_owned(),
        note: note.into(),
        state,
    }
}

const STEPS: [&str; 8] = [
    "Download package",
    "Verify feed signature",
    "Check package contents",
    "Install files",
    "Create sandbox and firewall rules",
    "Set up encrypted storage",
    "Configure settings",
    "Start and run health check",
];

struct Progress {
    report: InstallReport,
}

impl Progress {
    fn new(pkgver: &str) -> Self {
        Progress {
            report: InstallReport {
                pkgver: pkgver.to_owned(),
                steps: STEPS
                    .iter()
                    .map(|n| step(n, "", StepState::Pending))
                    .collect(),
                details: Vec::new(),
                problem: None,
            },
        }
    }

    fn set(&mut self, index: usize, state: StepState, note: impl Into<String>) {
        let s = &mut self.report.steps[index];
        s.state = state;
        s.note = note.into();
    }

    fn fail(mut self, index: usize, problem: String) -> InstallReport {
        self.set(index, StepState::Failed, "failed");
        self.report.problem = Some(problem);
        self.report
    }
}

/// The installed manifest must be the one that was inspected, byte for byte in meaning.
fn installed_manifest(text: &[u8], inspected: &Manifest) -> Result<(), String> {
    let text = std::str::from_utf8(text).map_err(|_| "the manifest is not UTF-8".to_owned())?;
    let parsed = Manifest::parse(text).map_err(|e| e.to_string())?;
    if parsed != *inspected {
        return Err("the installed manifest differs from the inspected one".to_owned());
    }
    Ok(())
}

impl<S: System> Helper<S> {
    pub(crate) fn app_dir(&self, id: &str) -> Result<PathBuf, ApiError> {
        Layout::new(id, Profile::App, false)
            .map_err(failed)?
            .app_dir
            .map(PathBuf::from)
            .ok_or_else(|| failed("an app has no directory"))
    }

    pub(crate) fn kept_dir(&self, id: &str) -> PathBuf {
        self.manage.paths.kept.join(id)
    }

    /// The previous approval, when the package came from this feed. A package installed from
    /// another feed belongs to that feed, and two feeds never share an id.
    fn previous_approval(
        state: &FeedState,
        feed: &str,
        package: &str,
    ) -> Result<Option<Permissions>, ApiError> {
        match state.installed.get(package) {
            Some(record) if record.feed != feed => Err(ApiError::Failed(format!(
                "{package} is installed from the feed {}, so another feed cannot replace it",
                record.feed
            ))),
            Some(_) => Ok(state.approved.get(package).cloned()),
            None => Ok(None),
        }
    }

    fn refuse_base_id(&self, package: &str) -> Result<(), ApiError> {
        let system = self.paths.system_manifests.join(format!("{package}.toml"));
        if self.sys.exists(&system) {
            return Err(ApiError::Failed(format!(
                "{package} is part of the base system and cannot come from a feed"
            )));
        }
        Ok(())
    }

    pub(crate) fn review_install(&self, feed: &str, package: &str) -> Result<Response, ApiError> {
        nodo_gen::validate::package_id(package)
            .map_err(|_| ApiError::NotFound("invalid package id".to_owned()))?;
        self.refuse_base_id(package)?;
        let mut state = self.read_feeds()?;
        let record = state.feed(feed)?.clone();
        if !record.enabled {
            return Err(ApiError::Failed(format!("the feed {feed} is disabled")));
        }
        let previous = Self::previous_approval(&state, feed, package)?;
        let version = record
            .index
            .iter()
            .find(|e| e.name == package)
            .map(|e| e.version.clone())
            .unwrap_or_default();
        // Review works on a copy, so nothing it could do to the pins is ever kept.
        let reviewed = self.manage.packages.review(
            &mut state.pins,
            &record.address,
            package,
            previous.as_ref(),
        );
        let (pkgver, permissions, added, manifest) = match reviewed {
            Ok(Reviewed::Blocked(reasons)) => return Ok(blocked(package, &reasons)),
            Ok(Reviewed::Ready {
                pkgver,
                permissions,
                added,
                manifest,
            }) => (pkgver, permissions, added, manifest),
            Err(e) => return refusal(package, &version, e),
        };
        let first_install = !state.installed.contains_key(package);
        let encrypted = manifest.storage.encrypted;
        Ok(Response::InstallReview(InstallReview {
            feed: feed.to_owned(),
            package: package.to_owned(),
            pkgver,
            permissions: permissions.summary(),
            added: added.iter().map(ToString::to_string).collect(),
            first_install,
            encrypted_storage: encrypted,
            needs_passphrase: encrypted && !self.sys.exists(&self.volume_location(package)?.file),
        }))
    }

    pub(crate) fn install_from_feed(
        &self,
        feed: &str,
        package: &str,
        approval: &PermissionApproval,
        passphrase: Option<&Secret>,
    ) -> Result<Response, ApiError> {
        nodo_gen::validate::package_id(package)
            .map_err(|_| ApiError::NotFound("invalid package id".to_owned()))?;
        self.refuse_base_id(package)?;
        let _work = lock(&self.manage.work);
        if self.installs_refused()? {
            return Err(ApiError::Failed(
                "the system disk is low on space, so installs wait until there is more".into(),
            ));
        }
        let state = self.read_feeds()?;
        let record = state.feed(feed)?.clone();
        if !record.enabled {
            return Err(ApiError::Failed(format!("the feed {feed} is disabled")));
        }
        let previous = Self::previous_approval(&state, feed, package)?;
        let first_install = !state.installed.contains_key(package);
        let mut pins = state.pins.clone();

        let reviewed = self.manage.packages.review(
            &mut pins,
            &record.address,
            package,
            previous.as_ref(),
        );
        let (pkgver, permissions, manifest) = match reviewed {
            Ok(Reviewed::Blocked(reasons)) => return Ok(blocked(package, &reasons)),
            Ok(Reviewed::Ready {
                pkgver,
                permissions,
                manifest,
                ..
            }) => (pkgver, permissions, manifest),
            Err(e) => return refusal(package, &approval.pkgver, e),
        };
        if approval.pkgver != pkgver || approval.permissions != permissions.summary() {
            return Err(ApiError::NeedsConfirmation(format!(
                "{package} now offers {pkgver} or asks for something other than what you \
                 approved. Review it again."
            )));
        }
        if manifest.storage.encrypted
            && passphrase.is_none()
            && !self.sys.exists(&self.volume_location(package)?.file)
        {
            return Err(ApiError::NeedsConfirmation(format!(
                "{package} keeps encrypted storage, so installing it needs the device passphrase"
            )));
        }

        let mut progress = Progress::new(&pkgver);
        let installed = self.manage.packages.install(
            &mut pins,
            &record.address,
            package,
            &permissions,
            &pkgver,
            &self.kept_dir(package),
        );
        let kept = match installed {
            Ok((Outcome::Installed { .. }, kept)) => kept,
            Ok((Outcome::Blocked(reasons), _)) => return Ok(blocked(package, &reasons)),
            Ok((Outcome::NeedsApproval { .. }, _)) => {
                return Err(ApiError::NeedsConfirmation(format!(
                    "{package} asks for something other than what you approved. Review it again."
                )))
            }
            Err(e @ (InstallError::Rejected(_) | InstallError::Signature(_))) => {
                return refusal(package, &pkgver, e)
            }
            Err(e) => return Ok(Response::InstallReport(progress.fail(0, e.to_string()))),
        };
        progress.set(0, StepState::Done, "");
        progress.set(1, StepState::Done, "matches the pinned key");
        progress.set(2, StepState::Done, "no root scripts, paths allowed");
        let app_dir = self.app_dir(package)?;
        progress.set(3, StepState::Done, app_dir.display().to_string());
        let version = pkgver
            .strip_prefix(&format!("{package}-"))
            .unwrap_or(&pkgver)
            .to_owned();

        // Recorded at once: the files are on disk, and the pins have moved.
        self.with_feeds(|state| {
            state.pins = pins.clone();
            state.approved.insert(package.to_owned(), permissions.clone());
            let old_kept = state.installed.get(package).and_then(|r| r.kept.clone());
            state.installed.insert(
                package.to_owned(),
                InstalledRecord {
                    feed: feed.to_owned(),
                    version: version.clone(),
                    kept: kept.clone().or(old_kept),
                },
            );
            Ok(())
        })?;

        if let Err(problem) = self.place_manifest(package, &app_dir, &manifest) {
            let report = progress.fail(3, problem);
            return Ok(Response::InstallReport(self.undo_first_install(
                package,
                first_install,
                report,
            )));
        }
        progress
            .report
            .details
            .push(format!("created user _{package}"));
        if let Err(e) = self.install_package(package, passphrase, Some(feed)) {
            let index = match e {
                ApiError::BadPassphrase => 5,
                _ => 4,
            };
            let report = progress.fail(index, e.to_string());
            return Ok(Response::InstallReport(self.undo_first_install(
                package,
                first_install,
                report,
            )));
        }
        progress.set(4, StepState::Done, "");
        if manifest.storage.encrypted {
            progress.set(5, StepState::Done, "opens with the device passphrase");
        } else {
            progress.set(5, StepState::Skipped, "not used");
        }
        if manifest.settings.is_some() {
            progress.set(6, StepState::Done, "");
        } else {
            progress.set(6, StepState::Skipped, "no settings");
        }
        let _ = self.refresh_health_config();
        let probe = match manifest.health.as_ref().map(crate::health::probe_for) {
            Some(Ok(probe)) => Some(probe),
            Some(Err(e)) => return Ok(Response::InstallReport(progress.fail(7, e))),
            None => None,
        };
        match self.wait_healthy(package, probe.as_ref()) {
            Ok(()) => progress.set(7, StepState::Done, "healthy"),
            Err(reason) => {
                // The package stays installed, so its log can be read and its settings fixed.
                let report = progress.fail(
                    7,
                    format!("{package} is installed but did not pass its health check: {reason}"),
                );
                return Ok(Response::InstallReport(report));
            }
        }
        Ok(Response::InstallReport(progress.report))
    }

    /// Puts the inspected manifest where the service generator reads it.
    pub(crate) fn place_manifest(
        &self,
        package: &str,
        app_dir: &Path,
        inspected: &Manifest,
    ) -> Result<(), String> {
        let source = app_dir.join(nodo_feed::MANIFEST_FILE);
        let bytes = self
            .sys
            .read_file(&source)
            .map_err(|e| e.0)?
            .ok_or_else(|| format!("{} is missing after install", source.display()))?;
        installed_manifest(&bytes, inspected)?;
        self.sys
            .ensure_dir(&self.paths.app_manifests, 0o755, None)
            .map_err(|e| e.0)?;
        let target = self.paths.app_manifests.join(format!("{package}.toml"));
        self.sys
            .atomic_write(&target, &bytes, MANIFEST)
            .map_err(|e| e.0)
    }

    /// A failed first install removes the package files and manifest it added. The service user
    /// and the id's claim stay, so the install can simply be tried again.
    fn undo_first_install(
        &self,
        package: &str,
        first_install: bool,
        mut report: InstallReport,
    ) -> InstallReport {
        if !first_install {
            return report;
        }
        let enabled = self.paths.service_dir.join(package);
        if self.sys.read_link(&enabled).is_some() {
            let _ = self.sys.remove_file(&enabled);
        }
        let _ = self
            .sys
            .remove_file(&self.paths.app_manifests.join(format!("{package}.toml")));
        let removed = self.manage.packages.remove(package);
        let _ = self.with_feeds(|state| {
            state.installed.remove(package);
            state.approved.remove(package);
            Ok(())
        });
        let note = match removed {
            Ok(()) => "The package files were removed again.".to_owned(),
            Err(e) => format!("Removing the package files failed too: {e}"),
        };
        report.details.push(note);
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_inspected_manifest_is_accepted() {
        let text = "api_version = 1\nid = \"demo\"\nversion = \"1\"\nexec = [\"bin/x\"]\n";
        let inspected = Manifest::parse(text).unwrap();
        assert!(installed_manifest(text.as_bytes(), &inspected).is_ok());
        let other = text.replace("bin/x", "bin/y");
        assert!(installed_manifest(other.as_bytes(), &inspected).is_err());
        assert!(installed_manifest(b"\xff", &inspected).is_err());
    }
}
