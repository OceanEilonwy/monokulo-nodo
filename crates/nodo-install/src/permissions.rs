//! What a package may do, in a form the user approves and the installer compares across updates.

use std::collections::BTreeSet;
use std::fmt;

use nodo_manifest::{Manifest, Outbound, Protocol, Scope};
use serde::{Deserialize, Serialize};

/// One thing the user agrees to. Reduced protection is a permission too, so that moving from
/// encrypted to plain storage counts as asking for more.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Permission {
    /// Uses another package or a role, such as `monero-daemon`.
    Needs { name: String },
    /// Offers a role that other packages may come to rely on.
    Provides { name: String },
    OutboundInternet,
    Inbound { port: u16, protocol: String, scope: String },
    /// A site the user can expose. Wherever the user puts it, the whole site is served.
    Web { name: String, multi_address: bool },
    UnencryptedStorage,
    /// The GPU's render nodes, for compute through Vulkan. The service joins the `gpu` group and
    /// its sandbox gets `/dev/dri/renderD*`, never the display's card nodes.
    Gpu,
    /// A system package the package needs installed, without its version constraint.
    SystemPackage { name: String },
    /// Another package of the same feed that installing this one sets up too (`[bundle]`). Each
    /// asks for its own permissions in its own review.
    BundleMember { name: String },
    /// A grant the bundle install records: `consumer` may read `provider`'s secret or non-public
    /// `setting`.
    BundleGrant { consumer: String, provider: String, setting: String },
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
            Permission::UnencryptedStorage => f.write_str("keeps its data unencrypted"),
            Permission::Gpu => f.write_str("uses the GPU for computation (Vulkan)"),
            Permission::SystemPackage { name } => write!(f, "needs the system package {name}"),
            Permission::BundleMember { name } => write!(f, "sets up {name} with it"),
            Permission::BundleGrant { consumer, provider, setting } => {
                write!(f, "lets {consumer} read the setting {setting} of {provider}")
            }
        }
    }
}

/// Everything one package version asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    pub granted: BTreeSet<Permission>,
    // Approvals stored by older helpers also have an `admin_paths` table, from when sites could
    // hide paths from public addresses. Unknown keys are ignored here, so those files still read.
    /// Panels the package shows on the device's screen. Shown in the review but not a permission:
    /// a panel only reads what the package's own service already serves on loopback
    /// (docs/SCREEN.md), so a version that adds one needs no new approval. Left out of the stored
    /// file when there are none, and ignored by helpers that predate panels.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub panels: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
        for web in &manifest.web {
            granted.insert(Permission::Web { name: web.name.clone(), multi_address: web.multi_address });
        }
        if !manifest.storage.encrypted {
            granted.insert(Permission::UnencryptedStorage);
        }
        if manifest.devices.gpu {
            granted.insert(Permission::Gpu);
        }
        if let Some(bundle) = &manifest.bundle {
            for name in &bundle.members {
                granted.insert(Permission::BundleMember { name: name.clone() });
            }
            for grant in &bundle.grants {
                granted.insert(Permission::BundleGrant {
                    consumer: grant.consumer.clone(),
                    provider: grant.provider.clone(),
                    setting: grant.setting.clone(),
                });
            }
        }
        for dependency in run_depends {
            granted.insert(Permission::SystemPackage { name: dependency_name(dependency) });
        }
        Permissions { granted, panels: manifest.panels.len() }
    }

    /// What `self` asks for beyond `approved`. Empty means no new approval is needed.
    pub fn added_since(&self, approved: &Permissions) -> Vec<Permission> {
        self.granted.difference(&approved.granted).cloned().collect()
    }

    /// One line per permission, for the approval screen.
    pub fn summary(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.granted.iter().map(ToString::to_string).collect();
        match self.panels {
            0 => {}
            1 => lines.push("shows a panel on the screen, reading its own service".to_string()),
            n => lines.push(format!("shows {n} panels on the screen, reading its own service")),
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

    const WEB: &str = "[[web]]\nname = \"web\"\nupstream = \"127.0.0.1:8081\"\n";

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
            ]
        );
    }

    #[test]
    fn a_bundle_head_lists_its_members_and_grants() {
        let m = manifest(
            "[storage]\nencrypted = true\n[bundle]\nmembers = [\"relay\", \"server\"]\n\
             [[bundle.grants]]\nconsumer = \"server\"\nprovider = \"relay\"\nsetting = \"credential\"\n\
             [[bundle.ask]]\npackage = \"relay\"\nsetting = \"domain\"\nlabel = \"Your domain\"\n",
        );
        let p = Permissions::of(&m, &[]);
        assert_eq!(
            p.summary(),
            [
                "sets up relay with it",
                "sets up server with it",
                "lets server read the setting credential of relay"
            ]
        );
        // A later version that adds a grant asks again.
        let more = manifest(
            "[storage]\nencrypted = true\n[bundle]\nmembers = [\"relay\", \"server\"]\n\
             [[bundle.grants]]\nconsumer = \"server\"\nprovider = \"relay\"\nsetting = \"credential\"\n\
             [[bundle.grants]]\nconsumer = \"shop\"\nprovider = \"server\"\nsetting = \"code\"\n",
        );
        let added = Permissions::of(&more, &[]).added_since(&p);
        assert_eq!(
            added.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["lets shop read the setting code of server"]
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
                "needs = [\"tor\"]\n[network]\noutbound = \"internet\"\n[[network.inbound]]\nport = 9000\nprotocol = \"tcp\"\nscope = \"lan\"\n[[web]]\nname = \"web\"\nupstream = \"127.0.0.1:8081\"\n",
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
            ]
        );
        assert!(old.added_since(&grown).is_empty(), "going back to the old version asks for nothing new");
    }

    #[test]
    fn the_gpu_is_a_permission_and_asking_for_it_later_is_new() {
        let without = Permissions::of(&manifest("[storage]\nencrypted = true\n"), &[]);
        let with = Permissions::of(&manifest("[storage]\nencrypted = true\n[devices]\ngpu = true\n"), &[]);
        assert!(without.summary().is_empty());
        assert_eq!(with.summary(), ["uses the GPU for computation (Vulkan)"]);
        assert_eq!(with.added_since(&without), [Permission::Gpu]);
        assert!(without.added_since(&with).is_empty(), "giving the GPU up asks for nothing");
        assert!(with.added_since(&with).is_empty());
        let text = toml::to_string(&with).unwrap();
        assert!(text.contains("kind = \"gpu\""), "{text}");
        assert_eq!(toml::from_str::<Permissions>(&text).unwrap(), with);
    }

    #[test]
    fn panels_are_listed_but_need_no_approval() {
        let base = "health = { tcp = { port = 8081 } }\n[storage]\nencrypted = true\n";
        let panel = "[[panels]]\nid = \"a\"\ntitle = \"A\"\nscript = \"a.lua\"\nrefresh_secs = 10\n\
            min_size = [2, 1]\nmax_size = [2, 1]\nsource = { http = { url = \"http://127.0.0.1:8081/\" } }\n";
        let with = Permissions::of(&manifest(&format!("{base}{panel}")), &[]);
        let without = Permissions::of(&manifest(base), &[]);
        assert_eq!(with.summary(), ["shows a panel on the screen, reading its own service"]);
        assert!(with.added_since(&without).is_empty(), "adding a panel asks for nothing");
        // Approvals stored without panels still read, and none are written when there are none.
        let stored = toml::to_string(&without).unwrap();
        assert!(!stored.contains("panels"), "{stored}");
        assert_eq!(toml::from_str::<Permissions>(&toml::to_string(&with).unwrap()).unwrap(), with);
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
    fn old_admin_paths_ask_for_nothing_and_old_approvals_still_read() {
        // A manifest that still lists admin paths gives the same permissions as one that does not:
        // nothing in a review mentions them, and dropping them from an update asks for nothing.
        let old = manifest(&format!("[storage]\nencrypted = true\n{WEB}admin_paths = [\"/admin\", \"/admin/*\"]\n"));
        let new = manifest(&format!("[storage]\nencrypted = true\n{WEB}"));
        let (old, new) = (Permissions::of(&old, &[]), Permissions::of(&new, &[]));
        assert_eq!(old, new);
        assert_eq!(new.summary(), ["serves the site 'web'"]);
        assert!(new.added_since(&old).is_empty());
        // An approval an older helper stored, with its admin paths table, still reads, and the table
        // asks for nothing.
        let stored = "[[granted]]\nkind = \"web\"\nname = \"web\"\nmulti_address = false\n\n\
                      [admin_paths]\nweb = [\"/admin\", \"/admin/*\"]\n";
        let approved: Permissions = toml::from_str(stored).unwrap();
        assert_eq!(approved, new);
        assert!(new.added_since(&approved).is_empty());
        assert!(!toml::to_string(&approved).unwrap().contains("admin"));
    }

    #[test]
    fn round_trips_through_toml() {
        let p = Permissions::of(&manifest(&format!("needs = [\"monero-daemon\"]\n{WEB}")), &["glibc>=2.41_1".into()]);
        let text = toml::to_string(&p).unwrap();
        assert_eq!(toml::from_str::<Permissions>(&text).unwrap(), p);
    }
}
