//! Rolling a package back by hand to the version it ran before its last update.
//!
//! A successful update keeps the previous version's verified file, one generation deep, with
//! the data layout its data was in. A rollback goes through the same engine as an update: the
//! data is copied first, the kept file is reinstalled with `xbps-install -f`, the old version
//! must pass its health check, and on failure the newer version and its data come back. The
//! version rolled back from is then held, so automatic updates do not reinstall it.
//!
//! What it refuses: data already converted to a newer layout than the old version declares
//! (it could not read it), a kept file that asks for permissions no longer approved, a locked
//! device for encrypted storage, and data kept on the data drive, which no update can copy yet.

use std::path::PathBuf;

use nodo_api::{ApiError, Response, RollbackOption, UpdateOutcome};
use nodo_update::auto::{self, Candidate, Checks, Origin};
use serde::{Deserialize, Serialize};

use super::{apply_one, fail, result_of, Failure, Updater, COPY_MARGIN, IN_VOLUME_MARGIN};
use crate::helper::{lock, Helper};
use crate::system::System;

fn first_layout() -> u32 {
    1
}

/// The version before the installed one, kept so it can be put back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PreviousVersion {
    /// `version_revision`.
    pub version: String,
    pub kept: PathBuf,
    /// The layout the data was in while this version ran.
    #[serde(default = "first_layout")]
    pub data_layout: u32,
}

impl<S: System> Updater<'_, S> {
    /// After a successful update: the version updated from becomes the one a rollback puts back,
    /// and the one before it is no longer kept.
    pub(super) fn keep_previous(&self, id: &str, version: &str, kept: &std::path::Path, data_layout: u32) {
        let h = self.helper;
        let mut dropped = None;
        let saved = h.with_feeds(|state| {
            if let Some(record) = state.installed.get_mut(id) {
                dropped = record.previous.take().map(|p| p.kept);
                record.previous = Some(PreviousVersion {
                    version: version.to_owned(),
                    kept: kept.to_owned(),
                    data_layout,
                });
            }
            Ok(())
        });
        if saved.is_err() {
            h.manage.packages.remove_kept(kept);
        }
        let current = h.read_feeds().ok().and_then(|s| s.installed.get(id).and_then(|r| r.kept.clone()));
        if let Some(old) = dropped.filter(|old| old != kept && Some(old) != current.as_ref()) {
            h.manage.packages.remove_kept(&old);
        }
    }

    /// The install step of a rollback: the kept older file instead of a download from the feed.
    pub(super) fn install_previous(&mut self, c: &Candidate, previous: &PreviousVersion) -> Result<(), Failure> {
        let h = self.helper;
        let id = c.package.as_str();
        let state = h.read_feeds().map_err(fail)?;
        let record = state
            .installed
            .get(id)
            .ok_or_else(|| Failure(format!("{id} was not installed from a feed")))?;
        let feed_name = record.feed.clone();
        let current = record
            .kept
            .clone()
            .filter(|p| h.manage.packages.kept_exists(p))
            .ok_or_else(|| Failure(format!("there is no copy of {id} {} to come back to", c.from)))?;
        if !h.manage.packages.kept_exists(&previous.kept) {
            return Err(Failure(format!("the kept copy of {id} {} is gone", c.to)));
        }
        let (found, permissions) = h.manage.packages.inspect_kept(&previous.kept).map_err(fail)?;
        if found != format!("{id}-{}", c.to) {
            return Err(Failure(format!("the kept file is {found}, not {id}-{}", c.to)));
        }
        let approved = state
            .approved
            .get(id)
            .ok_or_else(|| Failure(format!("{id} has no approved permissions")))?;
        if !permissions.added_since(approved).is_empty() {
            return Err(Failure(format!("{id} {} asks for permissions that are no longer approved", c.to)));
        }
        let package = h.package(id).map_err(fail)?;
        let encrypted = package.manifest.storage.encrypted;
        let (data, copy, skip) = self.data_copy(id, encrypted)?;
        if encrypted {
            if !h.sys.is_mounted(&data) {
                return Err(Failure(format!("{id}'s encrypted storage is locked")));
            }
            let layout = h.data_layout_of(id);
            if previous.data_layout < layout {
                return Err(Failure(format!(
                    "{id}'s data is in layout {layout}, which {} cannot read",
                    c.to
                )));
            }
        }
        let size = h.manage.packages.tree_size(&data);
        let free = h.manage.packages.free_bytes(&data);
        let margin = if encrypted { IN_VOLUME_MARGIN } else { COPY_MARGIN };
        if size.saturating_add(margin) > free {
            return Err(Failure(format!(
                "there is not enough free space to keep a copy of {id}'s data while rolling back"
            )));
        }

        // From here on a failure brings the newer version back.
        self.previous.insert(id.to_owned(), current);
        self.stop(id, package.manifest.stop_timeout_secs)?;
        h.manage.packages.remove_tree(&copy).map_err(Failure)?;
        if h.sys.exists(&data) || h.manage.packages.tree_size(&data) > 0 {
            h.manage.packages.copy_tree(&data, &copy, skip).map_err(Failure)?;
        }
        h.manage.packages.reinstall(&previous.kept).map_err(fail)?;
        let (version, kept) = (c.to.clone(), previous.kept.clone());
        h.with_feeds(|state| {
            if let Some(record) = state.installed.get_mut(id) {
                record.version = version.clone();
                record.kept = Some(kept.clone());
                record.previous = None;
            }
            Ok(())
        })
        .map_err(fail)?;
        let manifest = self.installed_manifest(id)?;
        h.place_manifest(id, &h.app_dir(id).map_err(fail)?, &manifest)
            .map_err(Failure)?;
        h.install_package(id, None, Some(&feed_name)).map_err(fail)?;
        Ok(())
    }
}

impl<S: System> Helper<S> {
    /// Removes kept package files nothing refers to any more: every file of a package that is no
    /// longer installed, and for an installed one anything beyond the copy of its installed
    /// version and of the one previous version a rollback puts back. So at most one previous
    /// version per package stays on the disk, whatever path got it there. The caller holds the
    /// work lock, so no install is part way between keeping a file and recording it.
    pub(crate) fn prune_kept(&self) {
        // Without the feed state there is no record of what is installed, and pruning against an
        // empty one would remove every kept file. Keep everything until the state is back.
        if !matches!(self.sys.read_file(&self.manage.paths.feeds), Ok(Some(_))) {
            return;
        }
        let Ok(state) = self.read_feeds() else { return };
        let packages = &self.manage.packages;
        for dir in packages.kept_entries(&self.manage.paths.kept) {
            let Some(id) = dir.file_name().and_then(|n| n.to_str()) else { continue };
            let Some(record) = state.installed.get(id) else {
                eprintln!("nodo-helper: removing the kept package files of {id}, which is not installed");
                let _ = packages.remove_tree(&dir);
                continue;
            };
            let wanted: Vec<&std::path::Path> = record
                .kept
                .iter()
                .map(PathBuf::as_path)
                .chain(record.previous.iter().map(|p| p.kept.as_path()))
                .collect();
            for file in packages.kept_entries(&dir) {
                if !wanted.contains(&file.as_path()) {
                    eprintln!("nodo-helper: removing {}, which no rollback uses", file.display());
                    packages.remove_kept(&file);
                }
            }
        }
    }

    /// Why a rollback cannot run now, if it cannot.
    fn rollback_blocked(&self, id: &str, previous: &PreviousVersion) -> Option<String> {
        if !self.manage.packages.kept_exists(&previous.kept) {
            return Some("the kept copy of that version is gone".to_owned());
        }
        if self.installs_refused().unwrap_or(true) {
            return Some("the system disk is low on space".to_owned());
        }
        let package = match self.package(id) {
            Ok(p) => p,
            Err(e) => return Some(e.to_string()),
        };
        if package.manifest.storage.encrypted {
            if self.device_locked() {
                return Some("unlock the device first".to_owned());
            }
            let layout = self.data_layout_of(id);
            if previous.data_layout < layout {
                return Some(format!(
                    "its data is in layout {layout}, which {} cannot read",
                    previous.version
                ));
            }
        } else if self.data_mount_of(id).ok().flatten().is_some() {
            return Some("its data is on the data drive, which a rollback cannot copy yet".to_owned());
        }
        None
    }

    pub(crate) fn list_rollbacks(&self) -> Result<Response, ApiError> {
        let state = self.read_feeds()?;
        let list = state
            .installed
            .iter()
            .filter_map(|(id, record)| {
                let previous = record.previous.as_ref()?;
                Some(RollbackOption {
                    package: id.clone(),
                    current: record.version.clone(),
                    previous: previous.version.clone(),
                    blocked: self.rollback_blocked(id, previous),
                })
            })
            .collect();
        Ok(Response::Rollbacks(list))
    }

    pub(crate) fn rollback_package(&self, id: &str, to: &str) -> Result<Response, ApiError> {
        let _work = lock(&self.manage.work);
        let state = self.read_feeds()?;
        let record = state
            .installed
            .get(id)
            .ok_or_else(|| ApiError::NotFound(id.escape_debug().to_string()))?;
        let previous = record
            .previous
            .clone()
            .ok_or_else(|| ApiError::Failed(format!("{id} has no earlier version kept")))?;
        if previous.version != to {
            return Err(ApiError::NeedsConfirmation(format!(
                "{id} would go back to {}, not {}",
                previous.version,
                to.escape_debug()
            )));
        }
        if let Some(reason) = self.rollback_blocked(id, &previous) {
            return Err(ApiError::Failed(format!("{id} cannot roll back now: {reason}")));
        }
        let node = self
            .package(id)
            .map(|p| p.manifest.provides.iter().any(|r| r == "monero-daemon"))
            .unwrap_or(false);
        let candidate = Candidate {
            package: id.to_owned(),
            from: record.version.clone(),
            to: previous.version.clone(),
            origin: if node { Origin::Node } else { Origin::App },
            new_permissions: false,
            key_changed: false,
            checks: Checks::Passed,
            needs_restart: false,
            needs_passphrase: false,
        };
        let mut updater = Updater::new(self, &[], None);
        updater.downgrade.insert(id.to_owned(), previous.clone());
        let outcome = apply_one(&mut updater, &candidate);
        let mut updates = self.update_state()?;
        if matches!(outcome, auto::Outcome::Updated { .. }) {
            // The version left behind is not reinstalled until the user releases it.
            updates.holds.hold(id, &candidate.from);
        } else {
            // Still on the newer version, so the older one can be tried again.
            let _ = self.with_feeds(|state| {
                if let Some(record) = state.installed.get_mut(id) {
                    if record.version == candidate.from {
                        record.previous = Some(previous.clone());
                    }
                }
                Ok(())
            });
        }
        self.prune_kept();
        super::record(&mut updates, self.manage.packages.now(), outcome.clone());
        {
            let _guard = lock(&self.manage.state);
            self.save_update_state(&updates)?;
        }
        let log = updater.logs.remove(id).unwrap_or_default();
        let result = result_of(&outcome, log);
        if result.outcome == UpdateOutcome::Updated {
            let _ = self.refresh_health_config();
        }
        Ok(Response::UpdateResults(vec![result]))
    }
}
