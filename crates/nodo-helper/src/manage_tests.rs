//! Feeds, installs, updates, health and backups through `Helper::handle`, on a scripted feed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nodo_api::{
    ApiError, AutoUpdatePrefs, KeyChangeConfirmation, PermissionApproval, Request, Response,
    Secret, UpdateApproval, UpdateOutcome,
};
use nodo_feed::{Decision, KeyFingerprint, PinStore, Violation};
use nodo_install::{InstallError, Outcome, Permissions, Reviewed};
use nodo_manifest::Manifest;

use crate::fake::{helper, FakeSystem};
use crate::helper::Helper;
use crate::manage::ManagePaths;
use crate::pkgsys::{FeedIndex, IndexEntry, PackageBackend};

const FEED: &str = "https://feed.example/apps";
const NOW: u64 = 1_000_000;

fn key(c: char) -> KeyFingerprint {
    KeyFingerprint::new(&c.to_string().repeat(32)).unwrap()
}

fn manifest(extra: &str) -> String {
    format!(
        "api_version = 1\nid = \"demo\"\nversion = \"1\"\nexec = [\"bin/demo\"]\nstop_timeout_secs = 5\n\n[health.tcp]\nport = 9000\n{extra}"
    )
}

#[derive(Clone)]
struct Pkg {
    version: String,
    manifest: String,
    rejected: bool,
}

#[derive(Default)]
struct Inner {
    feeds: BTreeMap<String, (KeyFingerprint, Vec<Pkg>)>,
    installed: BTreeMap<String, String>,
    kept: BTreeSet<PathBuf>,
    log: Vec<String>,
    minute: u16,
}

type Ready = (String, Permissions, Vec<nodo_install::Permission>, Manifest);

#[derive(Clone, Default)]
struct FakePackages(Arc<Mutex<Inner>>);

impl FakePackages {
    fn publish(&self, version: &str, manifest: &str) {
        self.publish_pkg(Pkg {
            version: version.into(),
            manifest: manifest.into(),
            rejected: false,
        });
    }

    fn publish_pkg(&self, pkg: Pkg) {
        let mut inner = self.0.lock().unwrap();
        let entry = inner
            .feeds
            .entry(FEED.into())
            .or_insert_with(|| (key('a'), Vec::new()));
        entry.1.retain(|p| p.version != pkg.version);
        entry.1.push(pkg);
    }

    fn set_key(&self, k: KeyFingerprint) {
        self.0.lock().unwrap().feeds.get_mut(FEED).unwrap().0 = k;
    }

    fn installed(&self) -> Option<String> {
        self.0.lock().unwrap().installed.get("demo").cloned()
    }

    fn log(&self) -> Vec<String> {
        self.0.lock().unwrap().log.clone()
    }

    /// The newest published version, which is what a feed index lists.
    fn latest(&self, address: &str) -> Option<(KeyFingerprint, Pkg)> {
        let inner = self.0.lock().unwrap();
        let (k, pkgs) = inner.feeds.get(address)?;
        pkgs.iter()
            .max_by(|a, b| nodo_feed::compare_versions(&a.version, &b.version).unwrap())
            .map(|p| (k.clone(), p.clone()))
    }

    fn prepare(
        &self,
        pins: &PinStore,
        address: &str,
        approved: Option<&Permissions>,
    ) -> Result<Result<Ready, Reviewed>, InstallError> {
        let (k, pkg) = self.latest(address).ok_or_else(|| InstallError::NotInFeed("demo".into()))?;
        if let Decision::Blocked(reasons) = pins.check_package(address, &k, "demo", &pkg.version) {
            return Ok(Err(Reviewed::Blocked(reasons)));
        }
        if pkg.rejected {
            return Err(InstallError::Rejected(vec![Violation::InstallScript {
                path: "./INSTALL".into(),
            }]));
        }
        let m = Manifest::parse(&pkg.manifest).unwrap();
        let permissions = Permissions::of(&m, &[]);
        let added = match approved {
            Some(a) => permissions.added_since(a),
            None => permissions.granted.iter().cloned().collect(),
        };
        Ok(Ok((format!("demo-{}", pkg.version), permissions, added, m)))
    }
}

impl PackageBackend for FakePackages {
    fn index(&self, address: &str) -> Result<FeedIndex, String> {
        let (k, pkg) = self.latest(address).ok_or("unreachable")?;
        Ok(FeedIndex {
            fingerprint: k,
            packages: vec![IndexEntry {
                name: "demo".into(),
                version: pkg.version,
                size: 100,
            }],
        })
    }

    fn review(
        &self,
        pins: &mut PinStore,
        address: &str,
        _package: &str,
        approved: Option<&Permissions>,
    ) -> Result<Reviewed, InstallError> {
        Ok(match self.prepare(pins, address, approved)? {
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
        keep: &Path,
    ) -> Result<(Outcome, Option<PathBuf>), InstallError> {
        let (found, permissions, added, _) = match self.prepare(pins, address, Some(approved))? {
            Err(Reviewed::Blocked(reasons)) => return Ok((Outcome::Blocked(reasons), None)),
            Err(_) => unreachable!(),
            Ok(ready) => ready,
        };
        if found != pkgver {
            return Err(InstallError::VersionChanged {
                expected: pkgver.into(),
                found,
            });
        }
        if !added.is_empty() {
            return Ok((
                Outcome::NeedsApproval {
                    pkgver: found,
                    permissions,
                    added,
                },
                None,
            ));
        }
        let version = found.strip_prefix("demo-").unwrap().to_owned();
        pins.record_installed(address, package, &version).unwrap();
        let kept = keep.join(format!("{found}.x86_64.xbps"));
        let mut inner = self.0.lock().unwrap();
        inner.installed.insert(package.into(), version);
        inner.kept.insert(kept.clone());
        inner.log.push(format!("install {found}"));
        Ok((Outcome::Installed { pkgver: found, permissions }, Some(kept)))
    }

    fn reinstall(&self, kept: &Path) -> Result<String, InstallError> {
        let name = kept.file_name().unwrap().to_str().unwrap();
        let pkgver = name.strip_suffix(".x86_64.xbps").unwrap().to_owned();
        let mut inner = self.0.lock().unwrap();
        inner.installed.insert(
            "demo".into(),
            pkgver.strip_prefix("demo-").unwrap().to_owned(),
        );
        inner.log.push(format!("reinstall {pkgver}"));
        Ok(pkgver)
    }

    fn remove(&self, name: &str) -> Result<(), InstallError> {
        let mut inner = self.0.lock().unwrap();
        inner.installed.remove(name);
        inner.log.push(format!("remove {name}"));
        Ok(())
    }

    fn now(&self) -> u64 {
        NOW
    }

    fn local_minute(&self) -> u16 {
        self.0.lock().unwrap().minute
    }

    fn copy_tree(&self, from: &Path, to: &Path, _skip: Option<&str>) -> Result<(), String> {
        self.0.lock().unwrap().log.push(format!("copy {} {}", from.display(), to.display()));
        Ok(())
    }

    fn restore_tree(&self, from: &Path, to: &Path, _skip: Option<&str>) -> Result<(), String> {
        self.0.lock().unwrap().log.push(format!("restore {} {}", from.display(), to.display()));
        Ok(())
    }

    fn remove_tree(&self, path: &Path) -> Result<(), String> {
        self.0.lock().unwrap().log.push(format!("rmtree {}", path.display()));
        Ok(())
    }

    fn tree_size(&self, _path: &Path) -> u64 {
        1
    }

    fn free_bytes(&self, _path: &Path) -> u64 {
        1 << 40
    }

    fn kept_exists(&self, path: &Path) -> bool {
        self.0.lock().unwrap().kept.contains(path)
    }

    fn remove_kept(&self, path: &Path) {
        self.0.lock().unwrap().kept.remove(path);
    }
}

fn test_paths() -> ManagePaths {
    ManagePaths {
        feeds: "/s/feeds.toml".into(),
        updates: "/s/updates".into(),
        kept: "/s/packages".into(),
        rollback: "/s/rollback".into(),
        health_config: "/etc/nodo/health.toml".into(),
        health_report: "/run/nodo/health.json".into(),
        boot_fallback: "/run/nodo/boot-fallback".into(),
        caddy_data: "/s/caddy/data/caddy".into(),
        ..ManagePaths::default()
    }
}

/// Room on the disk, as a fresh report from the health daemon says.
fn healthy_report(sys: &FakeSystem, refuse: bool) {
    let report = format!(
        r#"{{"version":1,"generated_at":{NOW},"disks":[{{"name":"system","path":"/","mounted":true,"total_bytes":8000000000,"free_bytes":300000000,"level":"Low"}}],"stopped":[],"refuse_installs":{refuse},"time":{{"synced":true}},"temperature_c":51.25,"attention":[{{"severity":"Warning","message":"The system disk is low on space (0.3 GB free). Updates wait until there is more."}}]}}"#
    );
    sys.put("/run/nodo/health.json", report.as_bytes());
}

fn setup() -> (Helper<FakeSystem>, FakePackages) {
    let sys = FakeSystem::default();
    healthy_report(&sys, false);
    let packages = FakePackages::default();
    packages.publish("1.0_1", &manifest(""));
    // What xbps would leave in the package's directory.
    sys.put("/opt/nodo/apps/demo/manifest.toml", manifest("").as_bytes());
    let h = helper(sys)
        .with_packages(Box::new(packages.clone()))
        .with_manage_paths(test_paths());
    (h, packages)
}

fn call(h: &Helper<FakeSystem>, request: Request) -> Result<Response, ApiError> {
    h.handle(1000, request)
}

fn add_feed(h: &Helper<FakeSystem>) -> String {
    let Ok(Response::FeedProbe(probe)) = call(h, Request::ProbeFeed { address: FEED.into() }) else {
        panic!()
    };
    assert_eq!(probe.name, "apps");
    call(
        h,
        Request::AddFeed {
            address: FEED.into(),
            fingerprint: probe.fingerprint.clone(),
        },
    )
    .unwrap();
    probe.name
}

fn review(h: &Helper<FakeSystem>) -> nodo_api::InstallReview {
    match call(
        h,
        Request::ReviewInstall {
            feed: "apps".into(),
            package: "demo".into(),
        },
    ) {
        Ok(Response::InstallReview(r)) => r,
        other => panic!("{other:?}"),
    }
}

fn approve(r: &nodo_api::InstallReview) -> PermissionApproval {
    PermissionApproval {
        pkgver: r.pkgver.clone(),
        permissions: r.permissions.clone(),
    }
}

fn install(h: &Helper<FakeSystem>, approval: PermissionApproval) -> Result<Response, ApiError> {
    call(
        h,
        Request::InstallPackage {
            feed: "apps".into(),
            package: "demo".into(),
            approval,
            passphrase: None,
        },
    )
}

fn installed() -> (Helper<FakeSystem>, FakePackages) {
    let (h, packages) = setup();
    add_feed(&h);
    let r = review(&h);
    let Ok(Response::InstallReport(report)) = install(&h, approve(&r)) else {
        panic!()
    };
    assert_eq!(report.problem, None, "{report:?}");
    (h, packages)
}

#[test]
fn a_feed_is_pinned_only_to_the_key_the_user_confirmed() {
    let (h, packages) = setup();
    let Ok(Response::FeedProbe(probe)) = call(&h, Request::ProbeFeed { address: FEED.into() }) else {
        panic!()
    };
    assert!(matches!(call(&h, Request::ListFeeds), Ok(Response::Feeds(f)) if f.is_empty()));
    packages.set_key(key('b'));
    let swapped = call(
        &h,
        Request::AddFeed {
            address: FEED.into(),
            fingerprint: probe.fingerprint.clone(),
        },
    );
    assert!(matches!(swapped, Err(ApiError::NeedsConfirmation(_))), "{swapped:?}");
    packages.set_key(key('a'));
    let Ok(Response::Feeds(feeds)) = call(
        &h,
        Request::AddFeed {
            address: FEED.into(),
            fingerprint: probe.fingerprint,
        },
    ) else {
        panic!()
    };
    assert_eq!(feeds[0].name, "apps");
    assert_eq!(feeds[0].packages, 1);
    assert!(feeds[0].fingerprint.starts_with("aa:aa:"));
}

#[test]
fn an_install_without_the_exact_approval_is_refused_and_changes_nothing() {
    let (h, packages) = setup();
    add_feed(&h);
    let r = review(&h);
    assert!(r.first_install);
    assert_eq!(r.permissions, r.added);
    h.sys.clear_calls();
    for approval in [
        PermissionApproval {
            pkgver: r.pkgver.clone(),
            permissions: vec![],
        },
        PermissionApproval {
            pkgver: "demo-0.9_1".into(),
            permissions: r.permissions.clone(),
        },
    ] {
        let result = install(&h, approval);
        assert!(matches!(result, Err(ApiError::NeedsConfirmation(_))), "{result:?}");
    }
    assert_eq!(packages.installed(), None);
    assert!(!h.sys.calls().iter().any(|c| c.starts_with("useradd")), "{:?}", h.sys.calls());
}

#[test]
fn an_approved_install_places_the_manifest_and_enables_the_service() {
    let (h, packages) = installed();
    assert_eq!(packages.installed().as_deref(), Some("1.0_1"));
    assert!(h.sys.get("/m/app/demo.toml").is_some());
    assert_eq!(h.sys.link("/service/demo").as_deref(), Some("/sv/demo"));
    assert!(h.sys.get("/etc/nodo/health.toml").is_some());
    let Ok(Response::Catalog(catalog)) = call(&h, Request::Catalog) else { panic!() };
    assert_eq!(catalog[0].installed.as_deref(), Some("1.0_1"));
}

#[test]
fn installs_wait_while_the_disk_is_low() {
    let (h, _) = setup();
    add_feed(&h);
    let r = review(&h);
    healthy_report(&h.sys, true);
    assert!(matches!(install(&h, approve(&r)), Err(ApiError::Failed(m)) if m.contains("low on space")));
}

#[test]
fn a_package_breaking_the_rules_is_rejected_without_override() {
    let (h, packages) = setup();
    add_feed(&h);
    packages.publish_pkg(Pkg {
        version: "1.0_1".into(),
        manifest: manifest(""),
        rejected: true,
    });
    let result = call(
        &h,
        Request::ReviewInstall {
            feed: "apps".into(),
            package: "demo".into(),
        },
    );
    assert!(matches!(result, Ok(Response::InstallRejected { reasons, .. }) if reasons[0].contains("install scripts")));
}

#[test]
fn a_changed_key_blocks_until_the_feed_name_is_typed() {
    let (h, packages) = installed();
    packages.set_key(key('b'));
    let Ok(Response::Feeds(feeds)) = call(&h, Request::RefreshFeeds) else { panic!() };
    assert!(feeds[0].key_changed);
    let blocked = call(
        &h,
        Request::ReviewInstall {
            feed: "apps".into(),
            package: "demo".into(),
        },
    );
    assert!(matches!(blocked, Ok(Response::InstallBlocked { key_changed: true, .. })), "{blocked:?}");
    let Ok(Response::FeedKeyChange(change)) = call(&h, Request::FeedKeyChange { name: "apps".into() }) else {
        panic!()
    };
    assert_eq!(change.affected[0].package, "demo");
    let trust = |typed: &str| {
        call(
            &h,
            Request::TrustFeedKey {
                name: "apps".into(),
                confirmation: KeyChangeConfirmation {
                    from: change.pinned.clone(),
                    to: change.offered.clone(),
                    typed_feed_name: typed.into(),
                },
            },
        )
    };
    assert!(matches!(trust("app"), Err(ApiError::NeedsConfirmation(_))));
    assert!(matches!(trust("apps"), Ok(Response::Feeds(f)) if !f[0].key_changed));
    assert!(matches!(trust("apps"), Err(ApiError::NeedsConfirmation(_))), "an approval is used once");
}

fn status(h: &Helper<FakeSystem>) -> nodo_api::UpdateStatus {
    match call(h, Request::CheckUpdates) {
        Ok(Response::UpdateStatus(s)) => s,
        other => panic!("{other:?}"),
    }
}

fn auto_on(h: &Helper<FakeSystem>) {
    call(
        h,
        Request::SetAutoUpdate(AutoUpdatePrefs {
            frequency: "daily".into(),
            window_start: "03:00".into(),
            window_end: "05:00".into(),
            enabled: vec!["demo".into()],
        }),
    )
    .unwrap();
}

fn run_auto(h: &Helper<FakeSystem>) -> Vec<nodo_api::UpdateResult> {
    // Root is the scheduler. The test helper allows uid 1000 only, so call the action itself.
    match h.run_auto_update() {
        Ok(Response::UpdateResults(r)) => r,
        other => panic!("{other:?}"),
    }
}

#[test]
fn auto_update_is_off_by_default_per_package_and_root_only() {
    let (h, packages) = installed();
    let Ok(Response::AutoUpdate(prefs)) = call(&h, Request::GetAutoUpdate) else { panic!() };
    assert!(prefs.enabled.is_empty());
    assert_eq!((prefs.window_start.as_str(), prefs.window_end.as_str()), ("03:00", "05:00"));
    packages.publish("1.1_1", &manifest(""));
    packages.0.lock().unwrap().minute = 4 * 60;
    status(&h);
    assert!(run_auto(&h).is_empty(), "nothing is automatic until chosen");
    assert_eq!(packages.installed().as_deref(), Some("1.0_1"));
    assert_eq!(h.handle(1000, Request::RunAutoUpdate), Err(ApiError::Denied));
    let unknown = call(
        &h,
        Request::SetAutoUpdate(AutoUpdatePrefs {
            frequency: "daily".into(),
            window_start: "03:00".into(),
            window_end: "05:00".into(),
            enabled: vec!["cuprated".into()],
        }),
    );
    assert!(unknown.is_err());
}

#[test]
fn auto_update_installs_inside_the_window_only() {
    let (h, packages) = installed();
    auto_on(&h);
    packages.publish("1.1_1", &manifest(""));
    packages.0.lock().unwrap().minute = 12 * 60;
    status(&h);
    assert!(run_auto(&h).is_empty());
    assert!(status(&h).candidates[0].verdict.contains("window"));
    assert_eq!(packages.installed().as_deref(), Some("1.0_1"));
    packages.0.lock().unwrap().minute = 4 * 60;
    let results = run_auto(&h);
    assert_eq!(results[0].outcome, UpdateOutcome::Updated, "{results:?}");
    assert_eq!(packages.installed().as_deref(), Some("1.1_1"));
    let log = packages.log();
    assert!(log.iter().any(|l| l.starts_with("copy /var/lib/apps/demo")), "{log:?}");
}

#[test]
fn an_update_asking_for_more_always_waits_for_the_user() {
    let (h, packages) = installed();
    auto_on(&h);
    packages.publish("1.1_1", &manifest("[network]\noutbound = \"internet\"\n"));
    packages.0.lock().unwrap().minute = 4 * 60;
    let s = status(&h);
    assert_eq!(s.candidates[0].added, ["connects to the internet"]);
    assert!(s.candidates[0].verdict.contains("new permissions"), "{}", s.candidates[0].verdict);
    assert!(run_auto(&h).is_empty());
    assert_eq!(packages.installed().as_deref(), Some("1.0_1"));

    let select = |approval| {
        call(
            &h,
            Request::ApplyUpdates {
                updates: vec![UpdateApproval {
                    package: "demo".into(),
                    to: "1.1_1".into(),
                    approval,
                }],
            },
        )
    };
    let Ok(Response::UpdateResults(r)) = select(None) else { panic!() };
    assert!(matches!(&r[0].outcome, UpdateOutcome::Refused(m) if m.contains("approval")));
    let Ok(Response::InstallReview(delta)) = call(&h, Request::ReviewUpdate { package: "demo".into() }) else {
        panic!()
    };
    assert_eq!(delta.added, ["connects to the internet"]);
    let Ok(Response::UpdateResults(r)) = select(Some(approve(&delta))) else { panic!() };
    assert_eq!(r[0].outcome, UpdateOutcome::Updated, "{r:?}");
    assert_eq!(packages.installed().as_deref(), Some("1.1_1"));
}

#[test]
fn a_failed_health_check_rolls_back_and_holds_that_version() {
    let (h, packages) = installed();
    auto_on(&h);
    packages.publish("1.1_1", &manifest(""));
    packages.0.lock().unwrap().minute = 4 * 60;
    status(&h);
    // Five failed probes for the new version, then the old one is healthy again.
    h.sys.script_probes(&[false, false, false, false, false]);
    let results = run_auto(&h);
    assert!(
        matches!(&results[0].outcome, UpdateOutcome::RolledBack { recovered: true, .. }),
        "{results:?}"
    );
    assert_eq!(packages.installed().as_deref(), Some("1.0_1"));
    assert!(packages.log().iter().any(|l| l == "reinstall demo-1.0_1"));
    assert!(packages.log().iter().any(|l| l.starts_with("restore /s/rollback/demo")));
    let s = status(&h);
    assert!(s.candidates[0].info.held);
    assert_eq!(s.attention.len(), 1);
    assert!(run_auto(&h).is_empty(), "a held version is not retried");
    call(&h, Request::DismissUpdate { package: "demo".into() }).unwrap();
    call(
        &h,
        Request::SetHold {
            package: "demo".into(),
            version: "1.1_1".into(),
            held: false,
        },
    )
    .unwrap();
    let s = status(&h);
    assert!(s.attention.is_empty() && !s.candidates[0].info.held);
}

#[test]
fn health_comes_from_the_daemons_report_and_the_boot_notice() {
    let (h, _) = setup();
    h.sys.put("/run/nodo/boot-fallback", b"trial boots of the new system ran out\n");
    let Ok(Response::Health(health)) = call(&h, Request::Health) else { panic!() };
    assert!(health.reporting);
    assert_eq!(health.disks[0].level, "Low");
    assert!(health.notes[0].message.contains("low on space"));
    assert_eq!(health.temperature_decic, Some(513));
    assert_eq!(health.boot_fallback.as_deref(), Some("trial boots of the new system ran out"));
    h.sys.remove("/run/nodo/health.json");
    let Ok(Response::Health(health)) = call(&h, Request::Health) else { panic!() };
    assert!(!health.reporting);
}

#[test]
fn the_health_config_lists_apps_on_the_system_disk_and_the_node_on_the_drive() {
    let (h, _) = installed();
    h.sys.add_system_manifest(
        "api_version = 1\nid = \"node\"\nversion = \"1\"\nexec = [\"bin/node\"]\nprovides = [\"monero-daemon\"]\n",
    );
    let cfg = h.health_config().unwrap();
    assert_eq!(cfg.filesystems[0].services, ["demo"]);
    assert_eq!(cfg.filesystems[1].services, ["node"]);
    assert_eq!(cfg.filesystems[1].path, Path::new("/media/monero"));
}

mod backups {
    use super::*;
    use std::fs;

    fn with_dir() -> (Helper<FakeSystem>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("secrets")).unwrap();
        fs::write(root.join("secrets/token"), b"top secret").unwrap();
        let paths = ManagePaths {
            backups: root.join("backups"),
            restore_staging: root.to_path_buf(),
            secrets: root.join("secrets"),
            onion: root.join("tor"),
            caddy_data: root.join("caddy/data/caddy"),
            app_config: root.join("apps"),
            volume_root: root.join("volumes"),
            ..test_paths()
        };
        let (h, _) = setup();
        (h.with_manage_paths(paths), dir)
    }

    fn create(h: &Helper<FakeSystem>, device: Option<&str>) -> Result<Response, ApiError> {
        call(
            h,
            Request::CreateBackup {
                device_passphrase: device.map(Secret::new),
                backup_passphrase: Secret::new("backup passphrase 1"),
            },
        )
    }

    fn restore(h: &Helper<FakeSystem>, name: &str, pass: &str, typed: &str) -> Result<Response, ApiError> {
        call(
            h,
            Request::RestoreBackup {
                name: name.into(),
                passphrase: Secret::new(pass),
                new_device_passphrase: Some(Secret::new("new device pass")),
                typed: typed.into(),
            },
        )
    }

    #[test]
    fn a_backup_needs_the_device_passphrase_and_includes_the_key() {
        let (h, _dir) = with_dir();
        h.init_device(&Secret::new("correct horse battery")).unwrap();
        assert!(matches!(create(&h, None), Err(ApiError::NeedsConfirmation(_))));
        assert_eq!(create(&h, Some("wrong horse battery")), Err(ApiError::BadPassphrase));
        let Ok(Response::BackupCreated(created)) = create(&h, Some("correct horse battery")) else {
            panic!()
        };
        assert!(created.has_device_key);
        let Ok(Response::Backups(list)) = call(&h, Request::ListBackups) else { panic!() };
        assert_eq!(list[0].name, created.info.name);
    }

    #[test]
    fn restore_needs_the_typed_word_and_the_right_passphrase_and_replaces_files() {
        let (h, dir) = with_dir();
        let Ok(Response::BackupCreated(created)) = create(&h, None) else { panic!() };
        let name = created.info.name;
        fs::write(dir.path().join("secrets/token"), b"changed").unwrap();
        assert!(matches!(
            restore(&h, &name, "backup passphrase 1", "yes"),
            Err(ApiError::NeedsConfirmation(_))
        ));
        assert!(restore(&h, &name, "wrong passphrase 12", "restore").is_err());
        assert_eq!(fs::read(dir.path().join("secrets/token")).unwrap(), b"changed");
        let Ok(Response::BackupContents(contents)) = call(
            &h,
            Request::InspectBackup {
                name: name.clone(),
                passphrase: Secret::new("backup passphrase 1"),
            },
        ) else {
            panic!()
        };
        assert!(!contents.has_device_key);
        let Ok(Response::Restored(report)) = restore(&h, &name, "backup passphrase 1", "restore") else {
            panic!()
        };
        assert!(report.restored.contains(&"secrets".to_owned()), "{report:?}");
        assert!(!dir.path().read_dir().unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".restore")));
    }

    #[test]
    fn certificates_and_acme_accounts_are_backed_up_and_restored() {
        let (h, dir) = with_dir();
        let cert = dir.path().join("caddy/data/caddy/certificates/acme-v02.api.letsencrypt.org-directory/shop.example.com");
        fs::create_dir_all(&cert).unwrap();
        fs::write(cert.join("shop.example.com.key"), b"private key").unwrap();
        let Ok(Response::BackupCreated(created)) = create(&h, None) else { panic!() };
        fs::remove_dir_all(dir.path().join("caddy/data")).unwrap();
        let Ok(Response::Restored(report)) = restore(&h, &created.info.name, "backup passphrase 1", "restore") else {
            panic!()
        };
        assert!(report.restored.contains(&"certificates".to_owned()), "{report:?}");
        let backup = fs::read(dir.path().join("backups").join(&created.info.name)).unwrap();
        assert!(!backup.windows(11).any(|w| w == b"private key"), "encrypted like every other secret");
    }

    #[test]
    fn a_tampered_backup_is_refused_and_nothing_changes() {
        let (h, dir) = with_dir();
        let Ok(Response::BackupCreated(created)) = create(&h, None) else { panic!() };
        let path = dir.path().join("backups").join(&created.info.name);
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 20;
        bytes[last] ^= 1;
        fs::write(&path, bytes).unwrap();
        fs::write(dir.path().join("secrets/token"), b"changed").unwrap();
        assert!(restore(&h, &created.info.name, "backup passphrase 1", "restore").is_err());
        assert_eq!(fs::read(dir.path().join("secrets/token")).unwrap(), b"changed");
        assert!(matches!(
            restore(&h, "../etc/shadow", "backup passphrase 1", "restore"),
            Err(ApiError::NotFound(_))
        ));
    }
}
