//! What a package may do, in a form the user approves and the installer compares across updates.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use nodo_manifest::{AddressKind, Manifest, Outbound, PathRule, Protocol, Scope};
use serde::{Deserialize, Serialize};

/// One thing the user agrees to. Reduced protection is a permission too, so that moving from
/// encrypted to plain storage, or serving a path where a rule used to block it, counts as asking for
/// more.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Permission {
    /// Uses another package or a role, such as `monero-daemon`.
    Needs { name: String },
    /// Offers a role that other packages may come to rely on.
    Provides { name: String },
    OutboundInternet,
    Inbound { port: u16, protocol: String, scope: String },
    /// A site the user can expose. Wherever the user puts it, the whole site is served, except for
    /// the paths its rules keep off some kinds of address.
    Web { name: String, multi_address: bool },
    /// A path of a site that an approved version blocked at these kinds of address and this one
    /// serves there.
    PathOpened { web: String, path: String, addresses: Vec<String> },
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
            Permission::PathOpened { web, path, addresses } => {
                write!(f, "no longer keeps {path} of '{web}' off {} addresses", addresses.join(", "))
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
    /// Each site's path rules, by name, for the sites that have any. Kept apart from `granted`
    /// because a rule protects the owner: what matters across updates is whether a path is served
    /// where an approved version blocked it. Left out of the stored file when there are none, so
    /// approvals stored before rules existed still compare equal.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub path_rules: BTreeMap<String, BTreeMap<String, PathRule>>,
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

/// The kinds of address each path of one site is blocked at, from all of its rules together.
fn blocked_by_path(rules: &BTreeMap<String, PathRule>) -> BTreeMap<&str, BTreeSet<AddressKind>> {
    let mut blocked: BTreeMap<&str, BTreeSet<AddressKind>> = BTreeMap::new();
    for rule in rules.values() {
        blocked.entry(rule.path.as_str()).or_default().extend(rule.blocked());
    }
    blocked
}

fn address_list(kinds: impl IntoIterator<Item = AddressKind>) -> String {
    kinds.into_iter().map(AddressKind::as_str).collect::<Vec<_>>().join(", ")
}

/// The review line for one rule, in the form the manifest writes it.
fn rule_line(web: &str, name: &str, rule: &PathRule) -> String {
    let path = &rule.path;
    match &rule.allowed_on {
        Some(allowed) if allowed.is_empty() => format!("{name}: keeps {path} of '{web}' off every address"),
        Some(allowed) => {
            let allowed: BTreeSet<AddressKind> = allowed.iter().copied().collect();
            format!("{name}: serves {path} of '{web}' only on {} addresses", address_list(allowed))
        }
        None => format!("{name}: keeps {path} of '{web}' off {} addresses", address_list(rule.blocked())),
    }
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
        let mut path_rules = BTreeMap::new();
        for web in &manifest.web {
            granted.insert(Permission::Web { name: web.name.clone(), multi_address: web.multi_address });
            if !web.paths.is_empty() {
                path_rules.insert(web.name.clone(), web.paths.clone());
            }
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
        Permissions { granted, path_rules, panels: manifest.panels.len() }
    }

    /// What `self` asks for beyond `approved`. Empty means no new approval is needed.
    /// A path rule that blocks fewer kinds of address than the approved version's, or is gone,
    /// counts; a site that is gone serves nothing, so its rules do not.
    pub fn added_since(&self, approved: &Permissions) -> Vec<Permission> {
        let mut added: Vec<Permission> = self.granted.difference(&approved.granted).cloned().collect();
        let empty = BTreeMap::new();
        for (web, rules) in &approved.path_rules {
            if !self.granted.iter().any(|p| matches!(p, Permission::Web { name, .. } if name == web)) {
                continue;
            }
            let now = blocked_by_path(self.path_rules.get(web).unwrap_or(&empty));
            for (path, kinds) in blocked_by_path(rules) {
                let opened: Vec<String> = match now.get(path) {
                    Some(still) => kinds.difference(still).map(|kind| kind.as_str().to_string()).collect(),
                    None => kinds.iter().map(|kind| kind.as_str().to_string()).collect(),
                };
                if !opened.is_empty() {
                    added.push(Permission::PathOpened { web: web.clone(), path: path.to_string(), addresses: opened });
                }
            }
        }
        added
    }

    /// One line per permission, for the approval screen.
    pub fn summary(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.granted.iter().map(ToString::to_string).collect();
        for (web, rules) in &self.path_rules {
            lines.extend(rules.iter().map(|(name, rule)| rule_line(web, name, rule)));
        }
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

    const VAULT: &str = "[storage]\nencrypted = true\n[[web]]\nname = \"vault\"\nupstream = \"127.0.0.1:8081\"\n";

    fn with_rules(rules: &[String]) -> Permissions {
        Permissions::of(&manifest(&format!("{VAULT}{}", rules.concat())), &[])
    }

    /// `[web.paths.<name>]` with `form`, which is `allowed_on = [...]` or `blocked_on = [...]`.
    fn rule(name: &str, path: &str, form: &str) -> String {
        format!("[web.paths.{name}]\npath = \"{path}\"\n{form}\n")
    }

    fn admin() -> String {
        rule("admin", "/admin", "blocked_on = [\"onion\", \"public\"]")
    }

    fn metrics() -> String {
        rule("metrics", "/metrics", "allowed_on = [\"local\"]")
    }

    fn lines(added: &[Permission]) -> Vec<String> {
        added.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn path_rules_are_listed_one_line_each() {
        let p = with_rules(&[admin(), metrics(), rule("export", "/export", "allowed_on = []")]);
        assert_eq!(
            p.summary(),
            [
                "serves the site 'vault'",
                "admin: keeps /admin of 'vault' off public, onion addresses",
                "export: keeps /export of 'vault' off every address",
                "metrics: serves /metrics of 'vault' only on local addresses",
            ]
        );
        assert!(p.added_since(&p).is_empty());
        let text = toml::to_string(&p).unwrap();
        assert_eq!(toml::from_str::<Permissions>(&text).unwrap(), p);
    }

    #[test]
    fn adding_or_tightening_rules_asks_for_nothing() {
        let none = with_rules(&[]);
        let admin = with_rules(&[admin()]);
        assert!(admin.added_since(&none).is_empty(), "a new rule protects more");
        let tighter = with_rules(&[rule("admin", "/admin", "allowed_on = []")]);
        assert!(tighter.added_since(&admin).is_empty(), "blocking more kinds protects more");
        // The same protection under another name or form is no change.
        let renamed = with_rules(&[rule("panel", "/admin", "allowed_on = [\"local\"]")]);
        assert!(renamed.added_since(&admin).is_empty());
    }

    #[test]
    fn loosening_or_removing_a_rule_asks_again() {
        let approved = with_rules(&[admin(), metrics()]);
        let looser = with_rules(&[rule("admin", "/admin", "blocked_on = [\"public\"]"), metrics()]);
        assert_eq!(
            looser.added_since(&approved),
            [Permission::PathOpened { web: "vault".into(), path: "/admin".into(), addresses: vec!["onion".into()] }]
        );
        assert_eq!(lines(&looser.added_since(&approved)), ["no longer keeps /admin of 'vault' off onion addresses"]);

        let opened_admin = ["no longer keeps /admin of 'vault' off public, onion addresses"];
        assert_eq!(lines(&with_rules(&[metrics()]).added_since(&approved)), opened_admin, "a rule removed");
        assert_eq!(
            lines(&with_rules(&[]).added_since(&approved)),
            [opened_admin[0], "no longer keeps /metrics of 'vault' off public, onion addresses"]
        );
        let moved = with_rules(&[rule("admin", "/manage", "blocked_on = [\"onion\", \"public\"]"), metrics()]);
        assert_eq!(lines(&moved.added_since(&approved)), opened_admin, "a rule moved to another path");

        // Two rules for one path add up, so dropping one opens only what the other did not block.
        let again = rule("again", "/admin", "blocked_on = [\"public\"]");
        let (both, one) = (with_rules(&[admin(), again.clone()]), with_rules(&[again]));
        assert_eq!(lines(&one.added_since(&both)), ["no longer keeps /admin of 'vault' off onion addresses"]);
        assert!(both.added_since(&one).is_empty());

        // A site that is gone serves nothing, so its rules ask for nothing.
        let without_site = Permissions::of(&manifest("[storage]\nencrypted = true\n"), &[]);
        assert!(without_site.added_since(&approved).is_empty());
    }

    #[test]
    fn approvals_stored_before_path_rules_still_read() {
        // A site without rules gives the permissions it gave before rules existed, and nothing is
        // stored for rules.
        let plain = with_rules(&[]);
        let stored = toml::to_string(&plain).unwrap();
        assert!(!stored.contains("path_rules"), "{stored}");
        let old = "[[granted]]\nkind = \"web\"\nname = \"vault\"\nmulti_address = false\n";
        let approved: Permissions = toml::from_str(old).unwrap();
        assert_eq!(approved, plain);
        // An update that adds rules to an approval stored without them asks for nothing.
        assert!(with_rules(&[admin()]).added_since(&approved).is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let p = Permissions::of(&manifest(&format!("needs = [\"monero-daemon\"]\n{WEB}")), &["glibc>=2.41_1".into()]);
        let text = toml::to_string(&p).unwrap();
        assert_eq!(toml::from_str::<Permissions>(&text).unwrap(), p);
    }
}
