//! Installing a suite of apps in one go (`[bundle]` in the head's manifest).
//!
//! The review is every package's own review, from the same feed, plus the grants, the questions
//! and the data drive choices the head declares. The install repeats the reviews under the work
//! lock and installs the members in order, then the head, each exactly as a single install would,
//! with what the bundle adds: the data drive chosen first, the grants it reads through recorded
//! before its settings first resolve, and the owner's answers applied as setting changes, checked
//! by the package's own rules. The first failure stops it; what was installed before stays, and
//! installing the bundle again skips those.

use std::collections::BTreeSet;

use nodo_api::{
    ApiError, BundleAnswer, BundleAsk, BundleGrant, BundleMember, BundleMemberReport, BundleReport,
    BundleReview, PermissionApproval, Response, Secret, SettingChange, SettingGrant, SettingInput,
    SettingLiteral, StepState,
};
use nodo_manifest::Manifest;

use crate::helper::{lock, Helper};
use crate::system::System;

/// What a bundle adds to one package's install.
#[derive(Default)]
pub(crate) struct InstallExtras<'a> {
    /// Setting values applied before its first start.
    pub changes: &'a [SettingChange],
    /// Grants with this package as the consumer, recorded before its settings first resolve.
    pub grants: &'a [SettingGrant],
    /// Keep its data on the data drive. Chosen before anything is created.
    pub data_drive: bool,
}

/// The line the review shows for a grant.
fn grant_text(grant: &SettingGrant) -> String {
    format!(
        "{} may read the setting {} of {}",
        grant.consumer, grant.setting, grant.provider
    )
}

fn declared_grants(manifest: &Manifest) -> Vec<SettingGrant> {
    manifest
        .bundle
        .iter()
        .flat_map(|b| &b.grants)
        .map(|g| SettingGrant {
            consumer: g.consumer.clone(),
            provider: g.provider.clone(),
            setting: g.setting.clone(),
        })
        .collect()
}

impl<S: System> Helper<S> {
    pub(crate) fn review_bundle(&self, feed: &str, package: &str) -> Result<Response, ApiError> {
        Ok(match self.bundle_review(feed, package)? {
            Ok((review, _)) => Response::BundleReview(review),
            Err(refused) => refused,
        })
    }

    /// The review, with the head's manifest, or the answer of the first package that cannot
    /// install (blocked or rejected), which names that package.
    fn bundle_review(
        &self,
        feed: &str,
        package: &str,
    ) -> Result<Result<(BundleReview, Manifest), Response>, ApiError> {
        let (head, manifest) = match self.review_one(feed, package)? {
            Ok(reviewed) => reviewed,
            Err(refused) => return Ok(Err(refused)),
        };
        let Some(bundle) = manifest.bundle.clone() else {
            return Err(ApiError::Failed(format!(
                "{package} is not a bundle; install it on its own"
            )));
        };
        let state = self.read_feeds()?;
        let record = state.feed(feed)?;
        let installed_version = |id: &str| {
            state
                .installed
                .get(id)
                .filter(|r| r.feed == feed)
                .map(|r| r.version.clone())
        };
        let mut members = Vec::new();
        for member in &bundle.members {
            if !record.index.iter().any(|e| &e.name == member) {
                return Err(ApiError::Failed(format!(
                    "{member} is not in the feed {feed}, so {package} cannot be installed. The feed may still be \
                     publishing it; look again later."
                )));
            }
            match self.review_one(feed, member)? {
                Ok((review, _)) => members.push(BundleMember {
                    review,
                    installed: installed_version(member),
                }),
                Err(refused) => return Ok(Err(refused)),
            }
        }
        members.push(BundleMember {
            installed: installed_version(package),
            review: head,
        });
        let skipped = |id: &str| {
            members
                .iter()
                .any(|m| m.review.package == id && m.installed.is_some())
        };
        let asks: Vec<BundleAsk> = bundle
            .ask
            .iter()
            .map(|a| BundleAsk {
                package: a.package.clone(),
                setting: a.setting.clone(),
                label: a.label.clone(),
                help: a.help.clone(),
                secret: a.secret,
                skipped: skipped(&a.package),
            })
            .collect();
        let data_drive: Vec<String> = bundle
            .data_drive
            .iter()
            .filter(|id| !skipped(id))
            .cloned()
            .collect();
        let drive = self.paths.drive.as_ref();
        let data_drive_present = drive.is_some_and(|d| self.sys.is_mounted(&d.mount_point));
        // A volume made on the data drive is a new one there, whatever exists on the system disk.
        let new_volume_on_drive = |m: &BundleMember| {
            m.review.encrypted_storage
                && data_drive.contains(&m.review.package)
                && drive.is_some_and(|d| {
                    !self.sys.exists(
                        &d.mount_point
                            .join(".nodo-volumes")
                            .join(format!("{}.luks", m.review.package)),
                    )
                })
        };
        let secret_answers = asks.iter().any(|a| a.secret && !a.skipped);
        let needs_passphrase = members
            .iter()
            .filter(|m| m.installed.is_none())
            .any(|m| m.review.needs_passphrase || new_volume_on_drive(m))
            || (secret_answers && self.secrets_key().is_none());
        let grants = declared_grants(&manifest)
            .into_iter()
            .map(|grant| BundleGrant {
                text: grant_text(&grant),
                grant,
            })
            .collect();
        let review = BundleReview {
            feed: feed.to_owned(),
            package: package.to_owned(),
            members,
            grants,
            asks,
            data_drive,
            data_drive_present,
            needs_passphrase,
        };
        Ok(Ok((review, manifest)))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn install_bundle(
        &self,
        feed: &str,
        package: &str,
        approvals: &[PermissionApproval],
        grants: &[SettingGrant],
        answers: &[BundleAnswer],
        data_drive: &[String],
        passphrase: Option<&Secret>,
    ) -> Result<Response, ApiError> {
        nodo_gen::validate::package_id(package)
            .map_err(|_| ApiError::NotFound("invalid package id".to_owned()))?;
        let _work = lock(&self.manage.work);
        if self.installs_refused()? {
            return Err(ApiError::Failed(
                "the system disk is low on space, so installs wait until there is more".into(),
            ));
        }
        let (review, manifest) = match self.bundle_review(feed, package)? {
            Ok(reviewed) => reviewed,
            Err(refused) => return Ok(refused),
        };
        let declared = declared_grants(&manifest);
        if grants != declared.as_slice() {
            return Err(ApiError::NeedsConfirmation(format!(
                "{package} declares other grants than the ones you approved. Review it again."
            )));
        }
        for id in data_drive {
            if !review.data_drive.contains(id) {
                return Err(ApiError::Failed(format!(
                    "{package} does not offer to keep {id}'s data on the data drive"
                )));
            }
        }
        if !data_drive.is_empty() && self.paths.drive.is_none() {
            return Err(ApiError::Failed("this device has no data drive".to_owned()));
        }
        // Every answer is for a question the bundle asks, once.
        let mut answered = BTreeSet::new();
        for answer in answers {
            let asked = review
                .asks
                .iter()
                .any(|a| a.package == answer.package && a.setting == answer.setting);
            if !asked {
                return Err(ApiError::Failed(format!(
                    "{}.{} is not one of the settings {package} asks for",
                    answer.package, answer.setting
                )));
            }
            if !answered.insert((answer.package.as_str(), answer.setting.as_str())) {
                return Err(ApiError::Failed(format!(
                    "{}.{} is answered twice",
                    answer.package, answer.setting
                )));
            }
        }
        // One approval per package to install, each exactly as its review showed it.
        let to_install: Vec<&BundleMember> = review
            .members
            .iter()
            .filter(|m| m.installed.is_none())
            .collect();
        let approval_for = |member: &BundleMember| {
            approvals
                .iter()
                .find(|a| {
                    a.pkgver == member.review.pkgver && a.permissions == member.review.permissions
                })
                .cloned()
        };
        if approvals.len() != to_install.len()
            || to_install.iter().any(|m| approval_for(m).is_none())
        {
            return Err(ApiError::NeedsConfirmation(format!(
                "{package} or an app it sets up now offers another version or asks for something other than \
                 what you approved. Review it again."
            )));
        }
        match passphrase {
            // Checked once, like an unlock, so a secret answer can be sealed and a wrong
            // passphrase stops everything before anything installs.
            Some(passphrase) => {
                let device_key = {
                    let mut failures = lock(&self.failed_unlocks);
                    let file = self.read_keystore()?;
                    self.verify_passphrase(&mut failures, passphrase, &file)?
                };
                self.remember_device_key(&device_key);
            }
            None if review.needs_passphrase => {
                return Err(ApiError::NeedsConfirmation(format!(
                    "installing {package} needs the device passphrase, for new encrypted storage or a secret setting"
                )))
            }
            None => {}
        }

        let mut reports = Vec::new();
        let mut problem = None;
        for member in &review.members {
            let id = member.review.package.as_str();
            if problem.is_some() {
                reports.push(BundleMemberReport {
                    package: id.to_owned(),
                    state: StepState::Pending,
                    note: "not reached".to_owned(),
                    report: None,
                });
                continue;
            }
            let own_answers: Vec<&BundleAnswer> =
                answers.iter().filter(|a| a.package == id).collect();
            let own_grants: Vec<SettingGrant> = declared
                .iter()
                .filter(|g| g.consumer == id)
                .cloned()
                .collect();
            if let Some(version) = &member.installed {
                let mut note = format!(
                    "{version} is installed already, so it was skipped and its settings were kept"
                );
                if !own_answers.is_empty() {
                    note.push_str("; your answers for it were not applied");
                }
                match self.grant_installed(&own_grants) {
                    Ok(()) => {}
                    Err(e) => {
                        let text = format!("{id}: approving its grants failed: {e}");
                        reports.push(BundleMemberReport {
                            package: id.to_owned(),
                            state: StepState::Failed,
                            note: text.clone(),
                            report: None,
                        });
                        problem = Some(text);
                        continue;
                    }
                }
                reports.push(BundleMemberReport {
                    package: id.to_owned(),
                    state: StepState::Skipped,
                    note,
                    report: None,
                });
                continue;
            }
            let changes: Vec<SettingChange> = own_answers
                .iter()
                .map(|answer| {
                    let secret = review
                        .asks
                        .iter()
                        .any(|a| a.package == id && a.setting == answer.setting && a.secret);
                    SettingChange {
                        name: answer.setting.clone(),
                        value: if secret {
                            SettingInput::Secret(answer.value.clone())
                        } else {
                            SettingInput::Literal(SettingLiteral::Text(
                                answer.value.expose().to_owned(),
                            ))
                        },
                    }
                })
                .collect();
            let extras = InstallExtras {
                changes: &changes,
                grants: &own_grants,
                data_drive: data_drive.iter().any(|d| d == id),
            };
            let approval = approval_for(member).expect("checked above");
            let outcome = self.install_from_feed_locked(feed, id, &approval, passphrase, &extras);
            let (state, note, report) = match outcome {
                Ok(Response::InstallReport(report)) => match report.problem.clone() {
                    None => (StepState::Done, "installed".to_owned(), Some(report)),
                    Some(why) => (StepState::Failed, why, Some(report)),
                },
                Ok(
                    Response::InstallBlocked { reasons, .. }
                    | Response::InstallRejected { reasons, .. },
                ) => (StepState::Failed, reasons.join("; "), None),
                Ok(other) => (
                    StepState::Failed,
                    format!("unexpected answer: {}", response_name(&other)),
                    None,
                ),
                Err(e) => (StepState::Failed, e.to_string(), None),
            };
            if state == StepState::Failed {
                problem = Some(format!("{id} was not installed: {note}"));
            }
            reports.push(BundleMemberReport {
                package: id.to_owned(),
                state,
                note,
                report,
            });
        }
        Ok(Response::BundleReport(BundleReport {
            package: package.to_owned(),
            members: reports,
            problem,
        }))
    }

    /// Approves grants for a consumer installed before the bundle, re-rendering it as `SetGrant`
    /// does. Grants already approved change nothing.
    fn grant_installed(&self, grants: &[SettingGrant]) -> Result<(), ApiError> {
        let have = self.load_bindings()?.grants;
        for grant in grants.iter().filter(|g| !have.contains(*g)) {
            if let Response::RolledBack(rollback) = self.set_grant(grant, true)? {
                return Err(ApiError::Failed(rollback.reason));
            }
        }
        Ok(())
    }

    /// Records grants for a package about to be installed, before its settings first resolve.
    /// Returns the ones that were new, so a failed first install can take them back.
    pub(crate) fn record_grants(
        &self,
        grants: &[SettingGrant],
    ) -> Result<Vec<SettingGrant>, ApiError> {
        if grants.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = lock(&self.service_lock);
        let mut bindings = self.load_bindings()?;
        let added: Vec<SettingGrant> = grants
            .iter()
            .filter(|g| bindings.grants.insert((*g).clone()))
            .cloned()
            .collect();
        if !added.is_empty() {
            self.bindings_write(&bindings)?.apply(&self.sys)?;
        }
        Ok(added)
    }

    pub(crate) fn forget_grants(&self, grants: &[SettingGrant]) -> Result<(), ApiError> {
        if grants.is_empty() {
            return Ok(());
        }
        let _guard = lock(&self.service_lock);
        let mut bindings = self.load_bindings()?;
        for grant in grants {
            bindings.grants.remove(grant);
        }
        self.bindings_write(&bindings)?.apply(&self.sys)?;
        Ok(())
    }

    /// Keeps a package's data on the data drive, as `UseDataDrive` does, for a package whose
    /// manifest is not in place yet.
    pub(crate) fn choose_data_drive(&self, id: &str) -> Result<(), ApiError> {
        if self.paths.drive.is_none() {
            return Err(ApiError::Failed("this device has no data drive".to_owned()));
        }
        let _guard = lock(&self.service_lock);
        if self.sys.exists(&self.paths.installed.join(id)) {
            return Err(ApiError::Failed(format!(
                "{id} is already installed, and its data is not moved"
            )));
        }
        let mut bindings = self.load_bindings()?;
        if bindings.data_drive.insert(id.to_owned()) {
            self.bindings_write(&bindings)?.apply(&self.sys)?;
        }
        Ok(())
    }

    /// Checks setting changes against the package's own rules without writing anything.
    pub(crate) fn plan_settings(
        &self,
        id: &str,
        changes: &[SettingChange],
    ) -> Result<(), ApiError> {
        let package = self.package(id)?;
        if package.manifest.settings.is_none() {
            return Err(ApiError::Failed(format!(
                "{id} has no settings, so the answers for it cannot be applied"
            )));
        }
        let (packages, _) = self.packages()?;
        self.with_settings(&packages, |ctx| self.settings.plan(ctx, &package, changes))
            .map(|_| ())
    }
}

/// A response's name for an error message. Its contents are never printed.
fn response_name(response: &Response) -> &'static str {
    match response {
        Response::Ok => "ok",
        _ => "another kind of answer",
    }
}

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;
