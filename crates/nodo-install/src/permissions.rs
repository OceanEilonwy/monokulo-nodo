//! What a package may do, in a form the user approves and the installer compares across updates.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use nodo_manifest::{Manifest, Outbound, Protocol, Scope};
use serde::{Deserialize, Serialize};

/// One thing the user agrees to. Reduced protection is a permission too, so that moving from
/// encrypted to plain storage, or making an admin path public, counts as asking for more.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Permission {
    /// Uses another package or a role, such as `monero-daemon`.
    Needs { name: String },
    /// Offers a role that other packages may come to rely on.
    Provides { name: String },
    OutboundInternet,
    Inbound { port: u16, protocol: String, scope: String },
    /// A site the user can expose. Paths the package does not mark as admin can become public.
    Web { name: String, multi_address: bool },
    /// A path that was admin only in an approved version and no longer is.
    AdminPathOpened { web: String, path: String },
    UnencryptedStorage,
    /// A system package the package needs installed, without its version constraint.
    SystemPackage { name: String },
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Permission::Needs { name } => write!(f, "uses {name}"),
            Permission::Provides { name } => write!(f, "provides {name} to other packages"),
            Permission::OutboundInternet => f.write_str("connects to the internet"),
            Permission::Inbound { port, protocol, scope } => {
                write!(f, "accepts {protocol} connections on port {port} from the {scope}")
            }
            Permission::Web { name, multi_address } => {
                write!(f, "serves the site '{name}'")?;
                if *multi_address {
                    f.write_str(" at several addresses")?;
                }
                Ok(())
            }
            Permission::AdminPathOpened { web, path } => write!(f, "no longer keeps {path} of '{web}' admin only"),
            Permission::UnencryptedStorage => f.write_str("keeps its data unencrypted"),
            Permission::SystemPackage { name } => write!(f, "needs the system package {name}"),
        }
    }
}

/// Everything one package version asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    pub granted: BTreeSet<Permission>,
    /// Admin paths per site. Kept apart because removing one widens access.
    #[serde(default)]
    pub admin_paths: BTreeMap<String, BTreeSet<String>>,
}

/// The package name of an xbps dependency such as `glibc>=2.41_1` or `libfoo-1.2_1`.
fn dependency_name(pattern: &str) -> String {
    let end = pattern.find(['<', '>', '=', '*', '?', '[']).unwrap_or(pattern.len());
    let name = &pattern[..end];
    if end == pattern.len() {
        // `name-version_revision` form: the version follows the last hyphen and has a revision.
        if let Some((base, version)) = name.rsplit_once('-') {
            if version.contains('_') && version.starts_with(|c: char| c.is_ascii_digit()) {
                return base.to_string();
            }
        }
    }
    name.to_string()
}

fn protocol(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Tcp => "tcp",
        Protocol::Udp => "udp",
    }
}

fn scope(scope: Scope) -> &'static str {
    match scope {
        Scope::Local => "device",
        Scope::Lan => "local network",
        Scope::Internet => "internet",
    }
}

impl Permissions {
    pub fn of(manifest: &Manifest, run_depends: &[String]) -> Self {
        let mut granted = BTreeSet::new();
        for name in &manifest.needs {
            granted.insert(Permission::Needs { name: name.clone() });
        }
        for name in &manifest.provides {
            granted.insert(Permission::Provides { name: name.clone() });
        }
        if manifest.network.outbound == Outbound::Internet {
            granted.insert(Permission::OutboundInternet);
        }
        for rule in &manifest.network.inbound {
            granted.insert(Permission::Inbound {
                port: rule.port,
                protocol: protocol(rule.protocol).to_string(),
                scope: scope(rule.scope).to_string(),
            });
        }
        let mut admin_paths = BTreeMap::new();
        for web in &manifest.web {
            granted.insert(Permission::Web { name: web.name.clone(), multi_address: web.multi_address });
            admin_paths.insert(web.name.clone(), web.admin_paths.iter().cloned().collect());
        }
        if !manifest.storage.encrypted {
            granted.insert(Permission::UnencryptedStorage);
        }
        for dependency in run_depends {
            granted.insert(Permission::SystemPackage { name: dependency_name(dependency) });
        }
        Permissions { granted, admin_paths }
    }

    /// What `self` asks for beyond `approved`. Empty means no new approval is needed.
    pub fn added_since(&self, approved: &Permissions) -> Vec<Permission> {
        let mut added: Vec<Permission> = self.granted.difference(&approved.granted).cloned().collect();
        for (web, paths) in &approved.admin_paths {
            let Some(now) = self.admin_paths.get(web) else { continue };
            for path in paths.difference(now) {
                added.push(Permission::AdminPathOpened { web: web.clone(), path: path.clone() });
            }
        }
        added
    }

    /// One line per permission, for the approval screen.
    pub fn summary(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.granted.iter().map(ToString::to_string).collect();
        for (web, paths) in &self.admin_paths {
            if !paths.is_empty() {
                let list: Vec<&str> = paths.iter().map(String::as_str).collect();
                lines.push(format!("keeps {} of '{web}' admin only", list.join(", ")));
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: &str) -> Manifest {
        Manifest::parse(&format!("api_version = 1\nid = \"shop\"\nversion = \"1\"\nexec = [\"bin/shop\"]\n{extra}"))
            .unwrap()
    }

    const WEB: &str = "[[web]]\nname = \"web\"\nupstream = \"127.0.0.1:8081\"\nadmin_paths = [\"/admin\", \"/admin/*\"]\n";

    #[test]
    fn summarises_a_manifest() {
        let m = manifest(&format!("needs = [\"monero-daemon\"]\n[storage]\nencrypted = true\n[network]\noutbound = \"internet\"\n{WEB}"));
        let p = Permissions::of(&m, &["glibc>=2.41_1".into(), "libstdc++>=4.4.0_1".into()]);
        assert_eq!(
            p.summary(),
            [
                "uses monero-daemon",
                "connects to the internet",
                "serves the site 'web'",
                "needs the system package glibc",
                "needs the system package libstdc++",
                "keeps /admin, /admin/* of 'web' admin only",
            ]
        );
    }

    #[test]
    fn an_identical_update_needs_no_approval() {
        let m = manifest(WEB);
        let p = Permissions::of(&m, &[]);
        assert!(p.added_since(&p).is_empty());
    }

    #[test]
    fn growth_is_reported_and_shrinking_is_not() {
        let old = Permissions::of(&manifest(&format!("[storage]\nencrypted = true\n{WEB}")), &[]);
        let grown = Permissions::of(
            &manifest(
                "needs = [\"tor\"]\n[network]\noutbound = \"internet\"\n[[network.inbound]]\nport = 9000\nprotocol = \"tcp\"\nscope = \"lan\"\n[[web]]\nname = \"web\"\nupstream = \"127.0.0.1:8081\"\nadmin_paths = [\"/admin/*\"]\n",
            ),
            &["libfoo-1.0_1".into()],
        );
        assert_eq!(
            grown.added_since(&old),
            [
                Permission::Needs { name: "tor".into() },
                Permission::OutboundInternet,
                Permission::Inbound { port: 9000, protocol: "tcp".into(), scope: "local network".into() },
                Permission::UnencryptedStorage,
                Permission::SystemPackage { name: "libfoo".into() },
                Permission::AdminPathOpened { web: "web".into(), path: "/admin".into() },
            ]
        );
        assert!(old.added_since(&grown).is_empty(), "going back to the old version asks for nothing new");
    }

    #[test]
    fn dependency_names_drop_their_constraints() {
        for (pattern, name) in [
            ("glibc>=2.41_1", "glibc"),
            ("libstdc++>=4.4.0_1", "libstdc++"),
            ("foo<2", "foo"),
            ("foo-1.0_1", "foo"),
            ("python3-foo", "python3-foo"),
            ("foo-bar-2.3_4", "foo-bar"),
            ("foo-*", "foo-"),
        ] {
            assert_eq!(dependency_name(pattern), name, "{pattern}");
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let p = Permissions::of(&manifest(&format!("needs = [\"monero-daemon\"]\n{WEB}")), &["glibc>=2.41_1".into()]);
        let text = toml::to_string(&p).unwrap();
        assert_eq!(toml::from_str::<Permissions>(&text).unwrap(), p);
    }
}
