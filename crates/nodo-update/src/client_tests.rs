use std::cell::RefCell;
use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

use super::*;
use crate::manifest::{KeyStatement, PublicKey, RepoEntry, SignatureFile, KEY_STATEMENT_CONTEXT, MANIFEST_CONTEXT};
use crate::trust::Pins;

/// A real index signed by xbps-rindex, with the key xbps shows as d7:3e:73:fc:...
const REPODATA: &[u8] = include_bytes!("../../nodo-feed/tests/fixtures/signed-feed/x86_64-repodata");
const FINGERPRINT: &str = "d7:3e:73:fc:ea:e5:23:56:da:c9:28:cd:ba:2d:0d:87";

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn state() -> TrustState {
    TrustState::from_pins(&Pins {
        channel: "stable".into(),
        keys: [PublicKey::from(&key(1))].into(),
        threshold: 1,
        min_serial: 3,
    })
}

fn manifest(serial: u64) -> SnapshotManifest {
    let digest = hex::encode(Sha256::digest(REPODATA));
    SnapshotManifest {
        format: 1,
        channel: "stable".into(),
        serial,
        snapshot: format!("s{serial}"),
        published: 1000,
        expires: 2000,
        needs_restart: false,
        repo: vec![
            RepoEntry { arch: "x86_64".into(), name: "void".into(), path: format!("snapshots/s{serial}/current"), repodata_sha256: digest.clone() },
            RepoEntry { arch: "x86_64".into(), name: "nodo".into(), path: format!("snapshots/s{serial}/nodo/x86_64"), repodata_sha256: digest },
        ],
        packages: vec![PackagePin { name: "demo".into(), version: "1.1_1".into() }],
        withdraws: vec![],
    }
}

/// Files by path. The manifest is signed with `signer`, or not at all.
#[derive(Default)]
struct Server {
    files: BTreeMap<String, Vec<u8>>,
}

impl Server {
    fn publish(&mut self, m: &SnapshotManifest, signer: Option<&SigningKey>) {
        let body = m.to_toml().into_bytes();
        for repo in &m.repo {
            self.files.insert(format!("{}/x86_64-repodata", repo.path), REPODATA.to_vec());
        }
        let path = manifest_path(&m.channel);
        match signer {
            Some(k) => {
                let mut sigs = SignatureFile::default();
                sigs.sign(MANIFEST_CONTEXT, &body, k);
                self.files.insert(format!("{path}.sig"), sigs.to_toml().into_bytes());
            }
            None => {
                self.files.remove(&format!("{path}.sig"));
            }
        }
        self.files.insert(path, body);
    }
}

impl Source for Server {
    fn get(&self, path: &str, limit: u64) -> io::Result<Option<Vec<u8>>> {
        let file = self.files.get(path).cloned();
        if file.as_ref().is_some_and(|f| f.len() as u64 > limit) {
            return Err(io::Error::other("too large"));
        }
        Ok(file)
    }
    fn repository_url(&self, path: &str) -> String {
        format!("https://updates.example/{path}")
    }
}

#[derive(Default)]
struct Device {
    installed: RefCell<BTreeMap<String, String>>,
    /// What `update` changes the installed set to.
    after_update: BTreeMap<String, String>,
    fail_update: bool,
    synced: bool,
    now: u64,
    keys: RefCell<Vec<String>>,
    log: RefCell<Vec<String>>,
}

impl Device {
    fn new() -> Self {
        let installed: BTreeMap<String, String> = [("demo".to_owned(), "1.0_1".to_owned()), ("base".to_owned(), "1_1".to_owned())].into();
        let mut after = installed.clone();
        after.insert("demo".into(), "1.1_1".into());
        Device { installed: RefCell::new(installed), after_update: after, now: 1500, ..Device::default() }
    }
    fn log(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

impl Machine for Device {
    fn arch(&self) -> io::Result<String> {
        Ok("x86_64".into())
    }
    fn clock_synced(&self) -> bool {
        self.synced
    }
    fn now(&self) -> u64 {
        self.now
    }
    fn has_key(&self, fingerprint: &str) -> bool {
        self.keys.borrow().iter().any(|k| k == fingerprint)
    }
    fn install_key(&self, key: &XbpsKey) -> io::Result<()> {
        self.log.borrow_mut().push(format!("key {}", key.fingerprint));
        self.keys.borrow_mut().push(key.fingerprint.clone());
        Ok(())
    }
    fn place_repodata(&self, url: &str, arch: &str, bytes: &[u8]) -> io::Result<()> {
        assert_eq!(bytes, REPODATA);
        self.log.borrow_mut().push(format!("index {url} {arch}"));
        Ok(())
    }
    fn installed(&self) -> io::Result<BTreeMap<String, String>> {
        Ok(self.installed.borrow().clone())
    }
    fn update(&self, urls: &[String]) -> Result<(), String> {
        self.log.borrow_mut().push(format!("update {}", urls.join(" ")));
        if self.fail_update {
            return Err("ERROR: demo-1.1_1: hash mismatch".into());
        }
        *self.installed.borrow_mut() = self.after_update.clone();
        Ok(())
    }
    fn downgrade(&self, urls: &[String], packages: &[String]) -> Result<(), String> {
        self.log.borrow_mut().push(format!("downgrade {} from {}", packages.join(" "), urls.join(" ")));
        let mut installed = self.installed.borrow_mut();
        for name in packages {
            installed.insert(name.clone(), "1.1_1".into());
        }
        Ok(())
    }
    fn restart_services(&self, packages: &[String]) -> Vec<String> {
        self.log.borrow_mut().push(format!("restart {}", packages.join(" ")));
        packages.to_vec()
    }
}

#[test]
fn a_newer_signed_snapshot_is_applied_and_only_then_recorded() {
    let mut server = Server::default();
    server.publish(&manifest(4), Some(&key(1)));
    let device = Device::new();
    let mut st = state();
    let outcome = apply(&server, &device, &mut st).unwrap();
    assert_eq!(
        outcome,
        Outcome::Applied {
            serial: 4,
            snapshot: "s4".into(),
            changed: vec!["demo: 1.0_1 -> 1.1_1".into()],
            restarted: vec!["demo".into()],
            needs_restart: false,
            withdrew: None,
        }
    );
    assert_eq!(
        device.log(),
        [
            format!("key {FINGERPRINT}"),
            "index https://updates.example/snapshots/s4/current x86_64".to_owned(),
            "index https://updates.example/snapshots/s4/nodo/x86_64 x86_64".to_owned(),
            "update https://updates.example/snapshots/s4/current https://updates.example/snapshots/s4/nodo/x86_64".to_owned(),
            "restart demo".to_owned(),
        ]
    );
    assert_eq!(st.serial, 4);
    assert_eq!(st.snapshot.as_deref(), Some("s4"));
    // The same manifest again changes nothing.
    let again = Device::new();
    assert_eq!(apply(&server, &again, &mut st).unwrap(), Outcome::UpToDate { serial: 4 });
    assert!(again.log().is_empty());
}

fn refused(server: &Server, device: &Device) -> (UpdateError, TrustState) {
    let mut st = state();
    let before = st.clone();
    let e = apply(server, device, &mut st).unwrap_err();
    assert_eq!(st, before, "a refused snapshot changes no state");
    (e, st)
}

#[test]
fn an_unsigned_manifest_changes_nothing() {
    let mut server = Server::default();
    server.publish(&manifest(4), None);
    let device = Device::new();
    let (e, _) = refused(&server, &device);
    assert!(matches!(e, UpdateError::Unsigned), "{e}");
    assert!(device.log().is_empty());
    // An empty or garbled signature file is the same as none.
    server.files.insert(format!("{}.sig", manifest_path("stable")), b"\n".to_vec());
    assert!(matches!(refused(&server, &device).0, UpdateError::Unsigned));
}

#[test]
fn a_tampered_or_foreign_manifest_changes_nothing() {
    let mut server = Server::default();
    server.publish(&manifest(4), Some(&key(1)));
    let path = manifest_path("stable");
    let body = String::from_utf8(server.files[&path].clone()).unwrap().replace("serial = 4", "serial = 40");
    server.files.insert(path.clone(), body.into_bytes());
    let device = Device::new();
    let (e, _) = refused(&server, &device);
    assert!(matches!(e, UpdateError::Refused(TrustError::Manifest(ManifestError::NotEnoughSignatures { .. }))), "{e}");
    server.publish(&manifest(4), Some(&key(9)));
    let (e, _) = refused(&server, &device);
    assert!(matches!(e, UpdateError::Refused(TrustError::Manifest(ManifestError::UnknownKey { .. }))), "{e}");
    assert!(device.log().is_empty());
}

#[test]
fn an_older_snapshot_is_refused_after_a_newer_one() {
    let mut server = Server::default();
    server.publish(&manifest(5), Some(&key(1)));
    let mut st = state();
    apply(&server, &Device::new(), &mut st).unwrap();
    server.publish(&manifest(4), Some(&key(1)));
    let device = Device::new();
    let e = apply(&server, &device, &mut st).unwrap_err();
    assert!(matches!(e, UpdateError::Refused(TrustError::Downgrade { current: 5, offered: 4 })), "{e}");
    assert!(device.log().is_empty());
    assert_eq!(st.serial, 5);
    // Below the image's floor too, with no state at all.
    server.publish(&manifest(3), Some(&key(1)));
    assert!(matches!(refused(&server, &device).0, UpdateError::Refused(TrustError::Downgrade { .. })));
}

#[test]
fn a_tampered_index_stops_before_xbps_runs() {
    let mut server = Server::default();
    server.publish(&manifest(4), Some(&key(1)));
    let mut index = REPODATA.to_vec();
    index[100] ^= 1;
    server.files.insert("snapshots/s4/nodo/x86_64/x86_64-repodata".into(), index);
    let device = Device::new();
    let (e, _) = refused(&server, &device);
    assert!(matches!(&e, UpdateError::BadRepository { name, .. } if name == "nodo"), "{e}");
    assert!(device.log().is_empty(), "{:?}", device.log());
    server.files.remove("snapshots/s4/nodo/x86_64/x86_64-repodata");
    assert!(matches!(refused(&server, &device).0, UpdateError::BadRepository { .. }));
}

#[test]
fn a_failed_install_can_be_retried() {
    let mut server = Server::default();
    server.publish(&manifest(4), Some(&key(1)));
    let device = Device { fail_update: true, ..Device::new() };
    let (e, _) = refused(&server, &device);
    assert!(matches!(&e, UpdateError::Xbps(text) if text.contains("hash mismatch")), "{e}");
    let mut st = state();
    assert!(matches!(apply(&server, &Device::new(), &mut st).unwrap(), Outcome::Applied { .. }));
}

#[test]
fn pinned_versions_must_be_what_xbps_installed() {
    let mut server = Server::default();
    let mut m = manifest(4);
    m.packages[0].version = "1.2_1".into();
    server.publish(&m, Some(&key(1)));
    let device = Device::new();
    let (e, _) = refused(&server, &device);
    assert!(matches!(&e, UpdateError::WrongVersion { package, .. } if package == "demo"), "{e}");
    // A pinned package that is not installed is not installed by an update.
    m.packages[0].name = "absent".into();
    server.publish(&m, Some(&key(1)));
    assert!(matches!(apply(&server, &Device::new(), &mut state()).unwrap(), Outcome::Applied { .. }));
}

#[test]
fn a_snapshot_that_needs_a_restart_restarts_no_services() {
    let mut server = Server::default();
    let mut m = manifest(4);
    m.needs_restart = true;
    server.publish(&m, Some(&key(1)));
    let device = Device::new();
    match apply(&server, &device, &mut state()).unwrap() {
        Outcome::Applied { restarted, needs_restart, .. } => assert!(restarted.is_empty() && needs_restart),
        other => panic!("{other:?}"),
    }
    assert!(!device.log().iter().any(|l| l.starts_with("restart")));
}

#[test]
fn expiry_counts_only_with_a_synced_clock() {
    let mut server = Server::default();
    server.publish(&manifest(4), Some(&key(1)));
    let late = Device { synced: true, now: 5000, ..Device::new() };
    assert!(matches!(refused(&server, &late).0, UpdateError::Refused(TrustError::Expired { .. })));
    let unsure = Device { synced: false, now: 5000, ..Device::new() };
    assert!(matches!(apply(&server, &unsure, &mut state()).unwrap(), Outcome::Applied { .. }));
}

#[test]
fn a_key_rotation_published_on_the_channel_is_followed() {
    let mut server = Server::default();
    let statement = KeyStatement { format: 1, sequence: 1, keys: vec![PublicKey::from(&key(2))], threshold: 1 };
    let body = statement.to_toml().into_bytes();
    let mut sigs = SignatureFile::default();
    sigs.sign(KEY_STATEMENT_CONTEXT, &body, &key(1));
    server.files.insert("channels/stable/keys.toml".into(), body);
    server.files.insert("channels/stable/keys.toml.sig".into(), sigs.to_toml().into_bytes());
    server.publish(&manifest(4), Some(&key(2)));
    let mut st = state();
    assert!(matches!(apply(&server, &Device::new(), &mut st).unwrap(), Outcome::Applied { .. }));
    assert!(st.keys.contains(&PublicKey::from(&key(2))) && !st.keys.contains(&PublicKey::from(&key(1))));
    // Applied once, the same statement is not an error the next time.
    assert_eq!(apply(&server, &Device::new(), &mut st).unwrap(), Outcome::UpToDate { serial: 4 });
    // A statement signed by an outsider stops everything.
    let mut forged = SignatureFile::default();
    forged.sign(KEY_STATEMENT_CONTEXT, &server.files["channels/stable/keys.toml"], &key(7));
    server.files.insert("channels/stable/keys.toml.sig".into(), forged.to_toml().into_bytes());
    assert!(matches!(refused(&server, &Device::new()).0, UpdateError::Refused(_)));
}

#[test]
fn xbps_names_its_repository_copies_like_this() {
    // As seen in /var/db/xbps on the VM, for the mirror the image was built from.
    assert_eq!(
        xbps_repo_dir("https://mirror.aarnet.edu.au/pub/voidlinux/current"),
        "https___mirror_aarnet_edu_au_pub_voidlinux_current"
    );
    assert_eq!(xbps_repo_dir("http://updates.example.test:8080/a/b"), "http___updates_example_test_8080_a_b");
}

#[test]
fn key_files_look_like_the_ones_xbps_writes() {
    let key = XbpsKey { fingerprint: FINGERPRINT.into(), pem: b"PEM".to_vec(), bits: 4096, signed_by: "Void <Linux> & co".into() };
    let plist = xbps_key_plist(&key);
    assert!(plist.contains("\t<key>public-key</key>\n\t<data>UEVN</data>\n"), "{plist}");
    assert!(plist.contains("\t<integer>4096</integer>\n"));
    assert!(plist.contains("<string>Void &lt;Linux&gt; &amp; co</string>"));
    assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
}

/// A device on snapshot s5, whose demo 1.2_1 the publisher pulled. The fixture index offers 1.1_1.
fn on_pulled_snapshot() -> (Device, TrustState) {
    let mut st = state();
    st.serial = 5;
    st.snapshot = Some("s5".into());
    st.manifest_sha256 = Some("00".repeat(32));
    let installed: BTreeMap<String, String> = [("demo".to_owned(), "1.2_1".to_owned()), ("base".to_owned(), "1_1".to_owned())].into();
    let device = Device { installed: RefCell::new(installed.clone()), after_update: installed, now: 1500, ..Device::default() };
    (device, st)
}

#[test]
fn a_snapshot_that_would_move_a_package_back_is_refused_before_xbps_runs() {
    let mut server = Server::default();
    server.publish(&manifest(6), Some(&key(1)));
    let (device, mut st) = on_pulled_snapshot();
    let before = st.clone();
    let e = apply(&server, &device, &mut st).unwrap_err();
    assert!(
        matches!(&e, UpdateError::Downgrade { package, installed, offered } if package == "demo" && installed == "1.2_1" && offered == "1.1_1"),
        "{e}"
    );
    assert_eq!(st, before);
    assert!(device.log().is_empty(), "nothing was handed to xbps: {:?}", device.log());
    // Withdrawing some other snapshot does not cover this device.
    let mut m = manifest(6);
    m.withdraws = vec!["s4".into()];
    server.publish(&m, Some(&key(1)));
    assert!(matches!(apply(&server, &device, &mut st), Err(UpdateError::Downgrade { .. })));
    // Nor does a device that lost its record, which cannot say which snapshot it is on.
    m.withdraws = vec!["s5".into()];
    server.publish(&m, Some(&key(1)));
    let mut lost = state();
    assert!(matches!(apply(&server, &device, &mut lost), Err(UpdateError::Downgrade { .. })));
    assert!(device.log().is_empty());
}

#[test]
fn a_withdrawal_moves_the_pulled_packages_back_and_records_the_new_serial() {
    let mut server = Server::default();
    let mut m = manifest(6);
    m.withdraws = vec!["s5".into()];
    server.publish(&m, Some(&key(1)));
    let (device, mut st) = on_pulled_snapshot();
    let outcome = apply(&server, &device, &mut st).unwrap();
    assert_eq!(
        outcome,
        Outcome::Applied {
            serial: 6,
            snapshot: "s6".into(),
            changed: vec!["demo: 1.2_1 -> 1.1_1".into()],
            restarted: vec!["demo".into()],
            needs_restart: false,
            withdrew: Some("s5".into()),
        }
    );
    let log = device.log();
    let update = log.iter().position(|l| l.starts_with("update ")).unwrap();
    let back = log.iter().position(|l| l.starts_with("downgrade ")).unwrap();
    assert!(update < back, "{log:?}");
    assert_eq!(
        log[back],
        "downgrade demo from https://updates.example/snapshots/s6/current https://updates.example/snapshots/s6/nodo/x86_64"
    );
    assert_eq!((st.serial, st.snapshot.as_deref()), (6, Some("s6")));
    // The serial rules are untouched: the withdrawal itself cannot be replayed, nor anything older.
    assert_eq!(apply(&server, &Device::new(), &mut st).unwrap(), Outcome::UpToDate { serial: 6 });
    let mut old = manifest(5);
    old.withdraws = vec!["s6".into()];
    server.publish(&old, Some(&key(1)));
    assert!(matches!(apply(&server, &Device::new(), &mut st), Err(UpdateError::Refused(TrustError::Downgrade { .. }))));
}

#[test]
fn a_withdrawal_below_the_image_floor_is_refused_like_any_old_manifest() {
    let mut server = Server::default();
    let mut m = manifest(3);
    m.withdraws = vec!["s5".into()];
    server.publish(&m, Some(&key(1)));
    let (device, _) = on_pulled_snapshot();
    let mut st = state();
    st.snapshot = Some("s5".into());
    assert!(matches!(apply(&server, &device, &mut st), Err(UpdateError::Refused(TrustError::Downgrade { .. }))));
    assert!(device.log().is_empty());
}

#[test]
fn applying_only_the_serial_the_user_saw() {
    let mut server = Server::default();
    server.publish(&manifest(4), Some(&key(1)));
    let device = Device::new();
    let mut st = state();
    let e = apply_expecting(&server, &device, &mut st, Some(5)).unwrap_err();
    assert!(matches!(e, UpdateError::UnexpectedSerial { expected: 5, offered: 4 }), "{e}");
    assert!(device.log().is_empty());
    assert!(matches!(apply_expecting(&server, &device, &mut st, Some(4)).unwrap(), Outcome::Applied { serial: 4, .. }));
    // Once applied, the same serial is simply up to date; another one is not what the user saw.
    assert_eq!(apply_expecting(&server, &Device::new(), &mut st, Some(4)).unwrap(), Outcome::UpToDate { serial: 4 });
    assert!(matches!(apply_expecting(&server, &Device::new(), &mut st, Some(7)), Err(UpdateError::UnexpectedSerial { .. })));
}
