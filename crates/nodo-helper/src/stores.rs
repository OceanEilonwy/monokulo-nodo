//! Persistent choices the user made about how packages fit together: which package provides a
//! role, which secret or non-public settings one package may read from another, and which
//! packages keep their data on the data drive. Also the feed each package was installed from.
//!
//! The choices live in one root-only file, so a change is a single atomic write that the
//! settings transaction can put back. The feed is recorded in the package's install marker,
//! which the helper writes when it claims the id.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use nodo_api::{ApiError, Response, RoleBinding, SettingGrant};
use nodo_manifest::is_valid_name;
use nodo_settings::is_valid_setting_name;
use serde::{Deserialize, Serialize};

use crate::apply::{Unit, Write};
use crate::helper::{lock, Helper};
use crate::settings::Target;
use crate::system::{System, WriteOptions};

const STORE_FILE: WriteOptions = WriteOptions {
    mode: 0o600,
    group: None,
};
const MAX_STORE: usize = 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bindings {
    /// Role to the package chosen to provide it, where several could.
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
    /// Approved reads of secret or non-public settings.
    #[serde(default)]
    pub grants: BTreeSet<SettingGrant>,
    /// Packages that keep their data, or their encrypted volume, on the data drive.
    #[serde(default)]
    pub data_drive: BTreeSet<String>,
}

/// What the install marker records.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Marker {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    feed: Option<String>,
}

fn failed(message: impl Into<String>) -> ApiError {
    ApiError::Failed(message.into())
}

/// A feed name as the installer reports it. It ends up in the TUI, so it is kept to printable
/// text of a sane length.
fn valid_feed(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

impl<S: System> Helper<S> {
    pub(crate) fn load_bindings(&self) -> Result<Bindings, ApiError> {
        let Some(bytes) = self.sys.read_file(&self.paths.bindings)? else {
            return Ok(Bindings::default());
        };
        if bytes.len() > MAX_STORE {
            return Err(failed("the bindings store is too large"));
        }
        std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| toml::from_str(text).ok())
            .ok_or_else(|| failed("the bindings store is corrupt"))
    }

    fn bindings_write(&self, bindings: &Bindings) -> Result<Write, ApiError> {
        let text = toml::to_string(bindings).map_err(|e| failed(e.to_string()))?;
        Ok(Write::new(
            self.paths.bindings.clone(),
            text.into_bytes(),
            STORE_FILE,
            0o755,
        ))
    }

    /// The feed a package was installed from, when the installer recorded one.
    pub(crate) fn feed_of(&self, id: &str) -> Option<String> {
        let bytes = self.sys.read_file(&self.paths.installed.join(id)).ok()??;
        let marker: Marker = toml::from_str(std::str::from_utf8(&bytes).ok()?).ok()?;
        marker.feed.filter(|feed| valid_feed(feed))
    }

    /// Records the feed in the install marker. Called by the installer as part of claiming the
    /// id, so a reinstall from another feed updates it.
    pub(crate) fn record_feed(&self, id: &str, feed: Option<&str>) -> Result<(), ApiError> {
        if let Some(feed) = feed {
            if !valid_feed(feed) {
                return Err(failed("invalid feed name"));
            }
        }
        let marker = Marker {
            feed: feed.map(str::to_owned),
        };
        let text = toml::to_string(&marker).map_err(|e| failed(e.to_string()))?;
        self.sys.ensure_dir(&self.paths.installed, 0o700, None)?;
        self.sys
            .atomic_write(&self.paths.installed.join(id), text.as_bytes(), STORE_FILE)?;
        Ok(())
    }

    /// The data drive mount for a package that was approved to use it.
    pub(crate) fn data_mount_of(&self, id: &str) -> Result<Option<&Path>, ApiError> {
        let Some(drive) = &self.paths.drive else {
            return Ok(None);
        };
        Ok(self
            .load_bindings()?
            .data_drive
            .contains(id)
            .then_some(drive.mount_point.as_path()))
    }

    pub(crate) fn list_role_bindings(&self) -> Result<Response, ApiError> {
        let (packages, _) = self.packages()?;
        let bindings = self.load_bindings()?;
        let mut roles: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for package in &packages {
            for role in &package.manifest.provides {
                roles
                    .entry(role.clone())
                    .or_default()
                    .push(package.id().to_owned());
            }
        }
        for role in bindings.roles.keys() {
            roles.entry(role.clone()).or_default();
        }
        Ok(Response::RoleBindings(
            roles
                .into_iter()
                .map(|(role, providers)| RoleBinding {
                    bound: bindings.roles.get(&role).cloned(),
                    role,
                    providers,
                })
                .collect(),
        ))
    }

    pub(crate) fn set_role_binding(
        &self,
        role: &str,
        package: Option<&str>,
    ) -> Result<Response, ApiError> {
        if !is_valid_name(role) {
            return Err(failed("invalid role"));
        }
        if let Some(id) = package {
            let provider = self.package(id)?;
            if !provider.manifest.provides.iter().any(|r| r == role) {
                return Err(failed(format!("{id} does not provide {role}")));
            }
        }
        let _guard = lock(&self.service_lock);
        let mut bindings = self.load_bindings()?;
        let before = bindings.clone();
        match package {
            Some(id) => bindings.roles.insert(role.to_owned(), id.to_owned()),
            None => bindings.roles.remove(role),
        };
        if bindings == before {
            return Ok(Response::Ok);
        }
        let unit = Unit::store(self.bindings_write(&bindings)?);
        self.transact(unit, || {
            self.dependent_units(&Target::Role(role.to_owned()), &[])
        })
    }

    pub(crate) fn list_grants(&self) -> Result<Response, ApiError> {
        Ok(Response::Grants(
            self.load_bindings()?.grants.into_iter().collect(),
        ))
    }

    /// Revoking a grant that a setting still uses fails when the consumer is re-rendered, which
    /// puts the grant back, so a reference must be removed before its approval.
    pub(crate) fn set_grant(
        &self,
        grant: &SettingGrant,
        approved: bool,
    ) -> Result<Response, ApiError> {
        if !is_valid_setting_name(&grant.setting) {
            return Err(failed("invalid setting name"));
        }
        if grant.consumer == grant.provider {
            return Err(failed(
                "a package does not need a grant for its own settings",
            ));
        }
        let consumer = self.package(&grant.consumer)?;
        self.package(&grant.provider)?;
        let _guard = lock(&self.service_lock);
        let mut bindings = self.load_bindings()?;
        let changed = if approved {
            bindings.grants.insert(grant.clone())
        } else {
            bindings.grants.remove(grant)
        };
        if !changed {
            return Ok(Response::Ok);
        }
        let unit = Unit::store(self.bindings_write(&bindings)?);
        self.transact(unit, || {
            let mut units = self.replan_units(std::slice::from_ref(&consumer))?;
            units.extend(
                self.dependent_units(&Target::Package(grant.consumer.clone()), &[&grant.consumer])?,
            );
            Ok(units)
        })
    }

    pub(crate) fn use_data_drive(&self, id: &str, enabled: bool) -> Result<Response, ApiError> {
        let package = self.package(id)?;
        if self.paths.drive.is_none() {
            return Err(failed("this device has no data drive"));
        }
        let _guard = lock(&self.service_lock);
        if self.sys.exists(&self.paths.installed.join(package.id())) {
            return Err(failed(format!(
                "{id} is already installed, and its data is not moved. Choose this before installing it"
            )));
        }
        let mut bindings = self.load_bindings()?;
        let changed = if enabled {
            bindings.data_drive.insert(id.to_owned())
        } else {
            bindings.data_drive.remove(id)
        };
        if changed {
            let write = self.bindings_write(&bindings)?;
            write.apply(&self.sys)?;
        }
        Ok(Response::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, SHOP};
    use nodo_api::Request;

    const NODE_A: &str = "api_version = 1\nid = \"node-a\"\nversion = \"1\"\nexec = [\"bin/a\"]\nprovides = [\"monero-daemon\"]\n";
    const NODE_B: &str = "api_version = 1\nid = \"node-b\"\nversion = \"1\"\nexec = [\"bin/b\"]\nprovides = [\"monero-daemon\"]\n";

    fn setup() -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.add_manifest(NODE_A);
        sys.add_manifest(NODE_B);
        helper(sys)
    }

    fn call(h: &Helper<FakeSystem>, request: Request) -> Result<Response, ApiError> {
        h.handle(1000, request)
    }

    #[test]
    fn role_bindings_list_every_provider_and_persist_the_choice() {
        let h = setup();
        let Ok(Response::RoleBindings(roles)) = call(&h, Request::ListRoleBindings) else {
            panic!()
        };
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].providers, ["node-a", "node-b"]);
        assert_eq!(roles[0].bound, None);
        call(
            &h,
            Request::SetRoleBinding {
                role: "monero-daemon".into(),
                package: Some("node-b".into()),
            },
        )
        .unwrap();
        assert!(h
            .sys
            .calls()
            .contains(&"write /s/bindings.toml mode=600 group=-".to_owned()));
        assert_eq!(
            h.load_bindings().unwrap().roles["monero-daemon"],
            "node-b".to_owned()
        );
        call(
            &h,
            Request::SetRoleBinding {
                role: "monero-daemon".into(),
                package: None,
            },
        )
        .unwrap();
        assert!(h.load_bindings().unwrap().roles.is_empty());
    }

    #[test]
    fn a_role_can_only_be_bound_to_a_package_providing_it() {
        let h = setup();
        for (role, package) in [
            ("monero-daemon", "shop"),
            ("monero-daemon", "ghost"),
            ("Bad Role", "node-a"),
        ] {
            assert!(call(
                &h,
                Request::SetRoleBinding {
                    role: role.into(),
                    package: Some(package.into()),
                },
            )
            .is_err());
        }
        assert_eq!(h.sys.get("/s/bindings.toml"), None);
    }

    #[test]
    fn grants_are_stored_and_revoked() {
        let h = setup();
        let grant = SettingGrant {
            consumer: "shop".into(),
            provider: "node-a".into(),
            setting: "token".into(),
        };
        call(
            &h,
            Request::SetGrant {
                grant: grant.clone(),
                approved: true,
            },
        )
        .unwrap();
        let Ok(Response::Grants(grants)) = call(&h, Request::ListGrants) else {
            panic!()
        };
        assert_eq!(grants, std::slice::from_ref(&grant));
        call(
            &h,
            Request::SetGrant {
                grant,
                approved: false,
            },
        )
        .unwrap();
        assert!(h.load_bindings().unwrap().grants.is_empty());
    }

    #[test]
    fn grants_need_installed_packages_and_a_valid_setting() {
        let h = setup();
        for (consumer, provider, setting) in [
            ("ghost", "node-a", "token"),
            ("shop", "ghost", "token"),
            ("shop", "node-a", "Bad Name"),
            ("shop", "shop", "token"),
        ] {
            let grant = SettingGrant {
                consumer: consumer.into(),
                provider: provider.into(),
                setting: setting.into(),
            };
            assert!(call(
                &h,
                Request::SetGrant {
                    grant,
                    approved: true
                }
            )
            .is_err());
        }
        assert_eq!(h.sys.get("/s/bindings.toml"), None);
    }

    #[test]
    fn the_feed_is_recorded_in_the_install_marker() {
        let h = setup();
        h.record_feed("shop", Some("community")).unwrap();
        assert_eq!(h.feed_of("shop").as_deref(), Some("community"));
        assert_eq!(
            h.package("shop").unwrap().origin(h.feed_of("shop")),
            nodo_api::Origin::Feed(Some("community".into()))
        );
        h.record_feed("shop", None).unwrap();
        assert_eq!(h.feed_of("shop"), None);
        assert!(h.record_feed("shop", Some("evil\nfeed")).is_err());
        // An old empty marker still means a helper install with no feed recorded.
        h.sys.put("/s/installed/shop", b"");
        assert_eq!(h.feed_of("shop"), None);
    }

    #[test]
    fn the_data_drive_is_chosen_before_install_only() {
        let h = setup();
        call(
            &h,
            Request::UseDataDrive {
                package: "shop".into(),
                enabled: true,
            },
        )
        .unwrap();
        assert_eq!(
            h.data_mount_of("shop").unwrap(),
            Some(Path::new("/media/monero"))
        );
        h.sys.put("/s/installed/shop", b"");
        assert!(call(
            &h,
            Request::UseDataDrive {
                package: "shop".into(),
                enabled: false,
            },
        )
        .is_err());
        assert!(h.load_bindings().unwrap().data_drive.contains("shop"));
    }

    #[test]
    fn a_corrupt_store_is_reported_not_replaced() {
        let h = setup();
        h.sys.put("/s/bindings.toml", b"roles = 7");
        assert!(call(&h, Request::ListGrants).is_err());
        assert!(call(
            &h,
            Request::SetRoleBinding {
                role: "monero-daemon".into(),
                package: Some("node-a".into()),
            },
        )
        .is_err());
        assert_eq!(h.sys.get("/s/bindings.toml").unwrap(), b"roles = 7");
    }
}
