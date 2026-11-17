//! Bundles through `Helper::handle`, on a scripted feed of several packages and the real settings
//! policy, so answers are checked by each package's own Lua rules and secrets are sealed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nodo_api::{
    ApiError, BundleAnswer, BundleReport, BundleReview, PermissionApproval, Request, Response,
    Secret, SettingGrant, StepState,
};
use nodo_feed::{Decision, KeyFingerprint, PinStore};
use nodo_install::{InstallError, Outcome, Permissions, Reviewed};
use nodo_manifest::Manifest;

use crate::exposure::NoCaddyYet;
use crate::fake::{FakeSystem, TEST_PATHS};
use crate::helper::{AllowedUids, Helper, Timing};
use crate::manage::ManagePaths;
use crate::pkgsys::{FeedIndex, IndexEntry, PackageBackend};
use crate::policy::NodoSettings;

const FEED: &str = "https://feed.example/apps";
const PASS: &str = "correct horse battery";
const NOW: u64 = 2_000_000;

const RELAY: &str = "api_version = 1\nid = \"relay\"\nversion = \"1\"\nexec = [\"bin/relay\"]\nsettings = \"settings.lua\"\n[health.tcp]\nport = 9001\n";
const RELAY_LUA: &str = r#"return { api_version = 1, settings = {
    { name = "domain", type = "string", label = "Domain",
      validate = function(v) return v ~= "not-a-domain", "must be a domain name" end },
    { name = "fcm", type = "secret", label = "Service account" },
    { name = "credential", type = "string", label = "Credential", default = "relay-credential" },
} }"#;
const SERVER: &str = "api_version = 1\nid = \"server\"\nversion = \"1\"\nexec = [\"bin/server\"]\nsettings = \"settings.lua\"\n[health.tcp]\nport = 9002\n[storage]\nencrypted = true\n";
// Reads a non-public setting of the relay, which needs the grant the bundle records.
const SERVER_LUA: &str = r#"return { api_version = 1, settings = {
    { name = "relay_credential", type = "string", label = "Relay credential", default = "$(relay.credential)" },
} }"#;
const SUITE: &str = r#"api_version = 1
id = "suite"
version = "1"
exec = ["bin/suite"]

[health.tcp]
port = 9003

[bundle]
members = ["relay", "server"]
data_drive = ["server"]

[[bundle.grants]]
consumer = "server"
provider = "relay"
setting = "credential"

[[bundle.ask]]
package = "relay"
setting = "domain"
label = "Your domain"
help = "The sites are named under it."

[[bundle.ask]]
package = "relay"
setting = "fcm"
label = "Service account"
secret = true
"#;

#[derive(Default)]
struct Inner {
    /// Name to (version, manifest).
    packages: BTreeMap<String, (String, String)>,
    log: Vec<String>,
}

#[derive(Clone, Default)]
struct Feed(Arc<Mutex<Inner>>);

type Ready = (String, Permissions, Vec<nodo_install::Permission>, Manifest);

impl Feed {
    fn publish(&self, name: &str, version: &str, manifest: &str) {
        self.0
            .lock()
            .unwrap()
            .packages
            .insert(name.into(), (version.into(), manifest.into()));
    }

    fn withdraw(&self, name: &str) {
        self.0.lock().unwrap().packages.remove(name);
    }

    fn log(&self) -> Vec<String> {
        self.0.lock().unwrap().log.clone()
    }

    fn ready(
        &self,
        pins: &PinStore,
        address: &str,
        package: &str,
        approved: Option<&Permissions>,
    ) -> Result<Result<Ready, Reviewed>, InstallError> {
        let (version, text) = self
            .0
            .lock()
            .unwrap()
            .packages
            .get(package)
            .cloned()
            .ok_or_else(|| InstallError::NotInFeed(package.into()))?;
        let key = KeyFingerprint::new(&"a".repeat(32)).unwrap();
        if let Decision::Blocked(reasons) = pins.check_package(address, &key, package, &version) {
            return Ok(Err(Reviewed::Blocked(reasons)));
        }
        let m = Manifest::parse(&text).unwrap();
        let permissions = Permissions::of(&m, &[]);
        let added = match approved {
            Some(a) => permissions.added_since(a),
            None => permissions.granted.iter().cloned().collect(),
        };
        Ok(Ok((format!("{package}-{version}"), permissions, added, m)))
    }
}

impl PackageBackend for Feed {
    fn index(&self, _address: &str) -> Result<FeedIndex, String> {
        let inner = self.0.lock().unwrap();
        Ok(FeedIndex {
            fingerprint: KeyFingerprint::new(&"a".repeat(32)).unwrap(),
            packages: inner
                .packages
                .iter()
                .map(|(name, (version, _))| IndexEntry {
                    name: name.clone(),
                    version: version.clone(),
                    size: 100,
                })
                .collect(),
        })
    }

    fn review(
        &self,
        pins: &mut PinStore,
        address: &str,
        package: &str,
        approved: Option<&Permissions>,
    ) -> Result<Reviewed, InstallError> {
        Ok(match self.ready(pins, address, package, approved)? {
            Err(blocked) => blocked,
            Ok((pkgver, permissions, added, manifest)) => Reviewed::Ready {
                pkgver,
                permissions,
                added,
                manifest: Box::new(manifest),
            },
        })
    }

    fn install(
        &self,
        pins: &mut PinStore,
        address: &str,
        package: &str,
        approved: &Permissions,
        pkgver: &str,
        _keep: &Path,
    ) -> Result<(Outcome, Option<PathBuf>), InstallError> {
        let (found, permissions, _, _) = match self.ready(pins, address, package, Some(approved))? {
            Err(Reviewed::Blocked(reasons)) => return Ok((Outcome::Blocked(reasons), None)),
            Err(_) => unreachable!(),
            Ok(ready) => ready,
        };
        assert_eq!(found, pkgver);
        let version = found
            .strip_prefix(&format!("{package}-"))
            .unwrap()
            .to_owned();
        pins.record_installed(address, package, &version).unwrap();
        self.0.lock().unwrap().log.push(format!("install {found}"));
        Ok((
            Outcome::Installed {
                pkgver: found,
                permissions,
            },
            None,
        ))
    }

    fn reinstall(&self, _kept: &Path) -> Result<String, InstallError> {
        unreachable!()
    }

    fn inspect_kept(&self, _kept: &Path) -> Result<(String, Permissions), InstallError> {
        unreachable!()
    }

    fn remove(&self, name: &str) -> Result<(), InstallError> {
        self.0.lock().unwrap().log.push(format!("remove {name}"));
        Ok(())
    }

    fn now(&self) -> u64 {
        NOW
    }

    fn local_minute(&self) -> u16 {
        0
    }

    fn copy_tree(&self, _from: &Path, _to: &Path, _skip: Option<&str>) -> Result<(), String> {
        Ok(())
    }

    fn restore_tree(&self, _from: &Path, _to: &Path, _skip: Option<&str>) -> Result<(), String> {
        Ok(())
    }

    fn remove_tree(&self, _path: &Path) -> Result<(), String> {
        Ok(())
    }

    fn tree_size(&self, _path: &Path) -> u64 {
        1
    }

    fn free_bytes(&self, _path: &Path) -> u64 {
        1 << 40
    }

    fn kept_exists(&self, _path: &Path) -> bool {
        false
    }

    fn remove_kept(&self, _path: &Path) {}
}

fn put_report(sys: &FakeSystem) {
    let report = format!(
        r#"{{"version":1,"generated_at":{NOW},"disks":[{{"name":"system","path":"/","mounted":true,"total_bytes":8000000000,"free_bytes":3000000000,"level":"Ok"}}],"stopped":[],"refuse_installs":false,"time":{{"synced":true}},"attention":[]}}"#
    );
    sys.put("/run/nodo/health.json", report.as_bytes());
}

fn machine() -> FakeSystem {
    let sys = FakeSystem::default();
    put_report(&sys);
    // What xbps leaves in each package's directory.
    for (id, manifest, lua) in [
        ("relay", RELAY, Some(RELAY_LUA)),
        ("server", SERVER, Some(SERVER_LUA)),
        ("suite", SUITE, None),
    ] {
        sys.put(
            &format!("/opt/nodo/apps/{id}/manifest.toml"),
            manifest.as_bytes(),
        );
        if let Some(lua) = lua {
            sys.put(&format!("/opt/nodo/apps/{id}/settings.lua"), lua.as_bytes());
        }
    }
    sys
}

pub(crate) fn on(sys: FakeSystem, packages: Box<dyn PackageBackend>) -> Helper<FakeSystem> {
    Helper::new(
        sys,
        AllowedUids::new([1000]),
        Box::new(NodoSettings),
        Box::new(NoCaddyYet),
    )
    .with_paths(TEST_PATHS())
    .with_kdf(crate::keys::KdfParams {
        memory_kib: 64,
        iterations: 1,
        lanes: 1,
    })
    .with_timing(Timing {
        health_attempts: 3,
        ..Timing::default()
    })
    .with_radios(crate::radio_tests::no_radios())
    .with_packages(packages)
    .with_manage_paths(ManagePaths {
        feeds: "/s/feeds.toml".into(),
        updates: "/s/updates".into(),
        kept: "/s/packages".into(),
        rollback: "/s/rollback".into(),
        last_backup: "/s/last-backup.toml".into(),
        ..ManagePaths::default()
    })
}

/// A set-up device that restarted, so it is locked, with the feed added.
fn setup() -> (Helper<FakeSystem>, Feed) {
    let feed = Feed::default();
    feed.publish("relay", "1.0_1", RELAY);
    feed.publish("server", "1.0_1", SERVER);
    feed.publish("suite", "1.0_1", SUITE);
    let first = on(machine(), Box::new(feed.clone()));
    first.init_device(&Secret::new(PASS)).unwrap();
    first.sys.reboot();
    put_report(&first.sys);
    let h = on(first.sys, Box::new(feed.clone()));
    let Ok(Response::FeedProbe(probe)) = h.handle(
        1000,
        Request::ProbeFeed {
            address: FEED.into(),
        },
    ) else {
        panic!()
    };
    h.handle(
        1000,
        Request::AddFeed {
            address: FEED.into(),
            fingerprint: probe.fingerprint,
        },
    )
    .unwrap();
    (h, feed)
}

fn review(h: &Helper<FakeSystem>) -> BundleReview {
    match h.handle(
        1000,
        Request::ReviewBundle {
            feed: "apps".into(),
            package: "suite".into(),
        },
    ) {
        Ok(Response::BundleReview(r)) => r,
        other => panic!("{other:?}"),
    }
}

fn approvals(r: &BundleReview) -> Vec<PermissionApproval> {
    r.members
        .iter()
        .filter(|m| m.installed.is_none())
        .map(|m| PermissionApproval {
            pkgver: m.review.pkgver.clone(),
            permissions: m.review.permissions.clone(),
        })
        .collect()
}

fn grants(r: &BundleReview) -> Vec<SettingGrant> {
    r.grants.iter().map(|g| g.grant.clone()).collect()
}

// As the TUI sends a pasted key file: a paste drops its line breaks, which in JSON are only spacing.
const FCM: &str = "{  \"type\": \"service_account\",  \"private_key\": \"-----BEGIN PRIVATE KEY-----\\nMIIEv\\n\"}";

fn answers(domain: &str) -> Vec<BundleAnswer> {
    vec![
        BundleAnswer {
            package: "relay".into(),
            setting: "domain".into(),
            value: Secret::new(domain),
        },
        BundleAnswer {
            package: "relay".into(),
            setting: "fcm".into(),
            value: Secret::new(FCM),
        },
    ]
}

fn install_with(
    h: &Helper<FakeSystem>,
    approvals: Vec<PermissionApproval>,
    grants: Vec<SettingGrant>,
    answers: Vec<BundleAnswer>,
    passphrase: Option<&str>,
) -> Result<Response, ApiError> {
    h.handle(
        1000,
        Request::InstallBundle {
            feed: "apps".into(),
            package: "suite".into(),
            approvals,
            grants,
            answers,
            data_drive: vec![],
            passphrase: passphrase.map(Secret::new),
        },
    )
}

fn install(h: &Helper<FakeSystem>, domain: &str) -> BundleReport {
    let r = review(h);
    let answers = answers(domain)
        .into_iter()
        .filter(|a| !r.asks.iter().any(|q| q.setting == a.setting && q.skipped));
    let answers: Vec<BundleAnswer> = answers.collect();
    match install_with(h, approvals(&r), grants(&r), answers, Some(PASS)) {
        Ok(Response::BundleReport(report)) => report,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_review_covers_every_package_in_order_with_grants_asks_and_the_passphrase() {
    let (h, _) = setup();
    let r = review(&h);
    let order: Vec<&str> = r
        .members
        .iter()
        .map(|m| m.review.package.as_str())
        .collect();
    assert_eq!(order, ["relay", "server", "suite"]);
    assert_eq!(r.members[2].review.bundle, Some(2));
    assert!(r.members[2]
        .review
        .permissions
        .contains(&"sets up relay with it".to_owned()));
    assert!(r.members[2]
        .review
        .permissions
        .contains(&"lets server read the setting credential of relay".to_owned()));
    assert_eq!(
        r.grants[0].text,
        "server may read the setting credential of relay"
    );
    assert_eq!(r.asks.len(), 2);
    assert!(r.asks[1].secret && !r.asks[0].secret);
    assert_eq!(r.data_drive, ["server"]);
    assert!(
        r.needs_passphrase,
        "new encrypted storage, and a secret answer while locked"
    );
    // The catalog now knows the head sets up two apps.
    let Ok(Response::Catalog(catalog)) = h.handle(1000, Request::Catalog) else {
        panic!()
    };
    assert_eq!(
        catalog.iter().find(|e| e.name == "suite").unwrap().bundle,
        Some(2)
    );
    assert_eq!(
        catalog.iter().find(|e| e.name == "relay").unwrap().bundle,
        None
    );
}

#[test]
fn a_member_missing_from_the_feed_refuses_the_review() {
    let (h, feed) = setup();
    feed.withdraw("server");
    h.handle(1000, Request::RefreshFeeds).unwrap();
    let refused = h.handle(
        1000,
        Request::ReviewBundle {
            feed: "apps".into(),
            package: "suite".into(),
        },
    );
    assert!(
        matches!(&refused, Err(ApiError::Failed(m)) if m.contains("server is not in the feed")),
        "{refused:?}"
    );
}

#[test]
fn installs_in_order_with_grants_before_the_consumer_renders_and_answers_applied() {
    let (h, feed) = setup();
    let report = install(&h, "example.org");
    assert_eq!(report.problem, None, "{report:?}");
    let states: Vec<(&str, StepState)> = report
        .members
        .iter()
        .map(|m| (m.package.as_str(), m.state))
        .collect();
    assert_eq!(
        states,
        [
            ("relay", StepState::Done),
            ("server", StepState::Done),
            ("suite", StepState::Done)
        ]
    );
    assert_eq!(
        feed.log(),
        [
            "install relay-1.0_1",
            "install server-1.0_1",
            "install suite-1.0_1"
        ]
    );
    // The server's default reads the relay's non-public setting, which only resolves because the
    // grant was recorded before its settings were first planned.
    let bindings = h.load_bindings().unwrap();
    let grant = SettingGrant {
        consumer: "server".into(),
        provider: "relay".into(),
        setting: "credential".into(),
    };
    assert!(bindings.grants.contains(&grant));
    let calls = h.sys.calls();
    let at = |needle: &str| {
        calls
            .iter()
            .position(|c| c.starts_with(needle))
            .unwrap_or_else(|| panic!("{needle}: {calls:?}"))
    };
    assert!(
        at("write /s/bindings.toml") < at("write /s/settings/server.toml"),
        "{calls:?}"
    );
    let store = String::from_utf8(h.sys.get("/s/settings/relay.toml").unwrap()).unwrap();
    assert!(store.contains("example.org"), "{store}");
    let details = &report.members[0].report.as_ref().unwrap().details;
    assert!(
        details
            .iter()
            .any(|d| d.contains("set domain, fcm before its first start")),
        "{details:?}"
    );
    let details = &report.members[1].report.as_ref().unwrap().details;
    assert!(
        details
            .iter()
            .any(|d| d == "server may read the setting credential of relay"),
        "{details:?}"
    );
}

#[test]
fn a_secret_answer_is_sealed_and_never_shown_or_logged() {
    let (h, _) = setup();
    let report = install(&h, "example.org");
    assert_eq!(report.problem, None, "{report:?}");
    let store = String::from_utf8(h.sys.get("/s/settings/relay.toml").unwrap()).unwrap();
    assert!(
        store.contains("v1."),
        "sealed under the device key: {store}"
    );
    assert!(
        !store.contains("PRIVATE KEY") && !store.contains("service_account"),
        "{store}"
    );
    assert!(!format!("{report:?}").contains("PRIVATE KEY"));
    assert!(!h.sys.calls().iter().any(|c| c.contains("PRIVATE KEY")));
    let request = Request::InstallBundle {
        feed: "apps".into(),
        package: "suite".into(),
        approvals: vec![],
        grants: vec![],
        answers: answers("example.org"),
        data_drive: vec![],
        passphrase: None,
    };
    assert!(!format!("{request:?}").contains("PRIVATE KEY"));
}

#[test]
fn an_answer_the_package_refuses_stops_the_install_at_that_package() {
    let (h, feed) = setup();
    let report = install(&h, "not-a-domain");
    let states: Vec<StepState> = report.members.iter().map(|m| m.state).collect();
    assert_eq!(
        states,
        [StepState::Failed, StepState::Pending, StepState::Pending]
    );
    let problem = report.problem.unwrap();
    assert!(
        problem.starts_with("relay was not installed") && problem.contains("must be a domain name"),
        "{problem}"
    );
    let steps = &report.members[0].report.as_ref().unwrap().steps;
    assert_eq!(
        steps[6].state,
        StepState::Failed,
        "reported as a settings problem: {steps:?}"
    );
    assert_eq!(
        feed.log(),
        ["install relay-1.0_1", "remove relay"],
        "the failed first install is undone"
    );
    assert!(h.load_bindings().unwrap().grants.is_empty());
}

#[test]
fn installing_again_skips_what_is_installed_and_keeps_its_settings() {
    let (h, feed) = setup();
    // The head fails its health check: the members before it stay installed.
    h.sys.script_probes(&[true, true, false, false, false]);
    let first = install(&h, "example.org");
    assert_eq!(first.members[2].state, StepState::Failed, "{first:?}");
    assert!(first
        .problem
        .as_deref()
        .unwrap()
        .starts_with("suite was not installed"));
    let r = review(&h);
    assert_eq!(r.members[0].installed.as_deref(), Some("1.0_1"));
    assert!(
        r.asks.iter().all(|a| a.skipped),
        "the relay's questions are not asked again"
    );
    // A client that still sends the answers gets them ignored, with a note.
    let report = match install_with(
        &h,
        approvals(&r),
        grants(&r),
        answers("other.example"),
        None,
    ) {
        Ok(Response::BundleReport(report)) => report,
        other => panic!("{other:?}"),
    };
    let states: Vec<StepState> = report.members.iter().map(|m| m.state).collect();
    assert_eq!(
        states,
        [StepState::Skipped, StepState::Skipped, StepState::Skipped],
        "{report:?}"
    );
    assert!(report.members[0]
        .note
        .contains("your answers for it were not applied"));
    let store = String::from_utf8(h.sys.get("/s/settings/relay.toml").unwrap()).unwrap();
    assert!(!store.contains("other.example"), "{store}");
    assert_eq!(
        feed.log().len(),
        3,
        "nothing installed twice: {:?}",
        feed.log()
    );
}

#[test]
fn approvals_grants_and_answers_must_match_the_review() {
    let (h, feed) = setup();
    let r = review(&h);
    let mut short = approvals(&r);
    short.pop();
    let mut changed = approvals(&r);
    changed[0].permissions.clear();
    for bad in [short, changed] {
        let refused = install_with(&h, bad, grants(&r), answers("example.org"), Some(PASS));
        assert!(
            matches!(refused, Err(ApiError::NeedsConfirmation(_))),
            "{refused:?}"
        );
    }
    let refused = install_with(
        &h,
        approvals(&r),
        vec![],
        answers("example.org"),
        Some(PASS),
    );
    assert!(
        matches!(refused, Err(ApiError::NeedsConfirmation(_))),
        "grants must be the declared ones: {refused:?}"
    );
    let mut extra = answers("example.org");
    extra.push(BundleAnswer {
        package: "server".into(),
        setting: "relay_credential".into(),
        value: Secret::new("x"),
    });
    let refused = install_with(&h, approvals(&r), grants(&r), extra, Some(PASS));
    assert!(
        matches!(&refused, Err(ApiError::Failed(m)) if m.contains("not one of the settings")),
        "{refused:?}"
    );
    let refused = install_with(&h, approvals(&r), grants(&r), answers("example.org"), None);
    assert!(
        matches!(refused, Err(ApiError::NeedsConfirmation(_))),
        "the passphrase is needed: {refused:?}"
    );
    let refused = install_with(
        &h,
        approvals(&r),
        grants(&r),
        answers("example.org"),
        Some("wrong passphrase"),
    );
    assert_eq!(refused, Err(ApiError::BadPassphrase));
    assert!(
        feed.log().is_empty(),
        "nothing was installed: {:?}",
        feed.log()
    );
}

#[test]
fn the_data_drive_is_chosen_before_a_member_installs() {
    let (h, _) = setup();
    h.sys.set_mounted("/media/monero");
    let r = review(&h);
    assert!(r.data_drive_present);
    let refused = h.handle(
        1000,
        Request::InstallBundle {
            feed: "apps".into(),
            package: "suite".into(),
            approvals: approvals(&r),
            grants: grants(&r),
            answers: vec![],
            data_drive: vec!["relay".into()],
            passphrase: None,
        },
    );
    assert!(
        refused.is_err(),
        "only members the bundle offers: {refused:?}"
    );
    let result = h.handle(
        1000,
        Request::InstallBundle {
            feed: "apps".into(),
            package: "suite".into(),
            approvals: approvals(&r),
            grants: grants(&r),
            answers: answers("example.org"),
            data_drive: vec!["server".into()],
            passphrase: Some(Secret::new(PASS)),
        },
    );
    let Ok(Response::BundleReport(report)) = result else {
        panic!("{result:?}")
    };
    assert_eq!(report.problem, None, "{report:?}");
    assert!(h.load_bindings().unwrap().data_drive.contains("server"));
    assert!(
        h.sys
            .get("/media/monero/.nodo-volumes/server.luks")
            .is_some(),
        "its volume is on the drive"
    );
}
