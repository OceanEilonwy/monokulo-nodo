use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;

use nodo_feed::{Approval, KeyFingerprint};

use super::*;

const FEED: &str = "https://feed.example/apps";
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../nodo-feed/tests/fixtures/signed-feed");

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).unwrap()
}

/// The fingerprint `xbps-query -vL` printed for the fixture key.
fn pinned_key() -> KeyFingerprint {
    KeyFingerprint::new("d7:3e:73:fc:ea:e5:23:56:da:c9:28:cd:ba:2d:0d:87").unwrap()
}

/// Program, arguments and environment of one command.
type Run = (String, Vec<String>, Vec<(String, String)>);

#[derive(Default)]
struct Fake {
    arch: String,
    feed: BTreeMap<String, Vec<u8>>,
    files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    dirs: RefCell<Vec<PathBuf>>,
    removed: RefCell<Vec<PathBuf>>,
    runs: RefCell<Vec<Run>>,
    /// What `xbps-rindex` writes. `None` writes the real fixture index.
    rindex_output: Option<Vec<u8>>,
    installed: RefCell<Option<String>>,
    /// What `xbps-install` leaves installed, or `None` to make it fail.
    installs_as: Option<String>,
}

impl Fake {
    fn new() -> Self {
        let mut feed = BTreeMap::new();
        for name in ["x86_64-repodata", "demo-1.1_1.x86_64.xbps", "demo-1.1_1.x86_64.xbps.sig2"] {
            feed.insert(name.to_string(), fixture(name));
        }
        Fake { arch: "x86_64".into(), feed, installs_as: Some("demo-1.1_1".into()), ..Fake::default() }
    }

    fn ran(&self, program: &str) -> Vec<Vec<String>> {
        self.runs.borrow().iter().filter(|(p, _, _)| p == program).map(|(_, args, _)| args.clone()).collect()
    }
}

impl System for Fake {
    fn device_arch(&self) -> io::Result<String> {
        Ok(self.arch.clone())
    }

    fn fetch(&self, feed: &str, file: &str, limit: u64) -> io::Result<Vec<u8>> {
        assert_eq!(feed, FEED);
        let bytes = self.feed.get(file).cloned().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::other("too large"));
        }
        Ok(bytes)
    }

    fn private_dir(&self) -> io::Result<PathBuf> {
        let path = PathBuf::from(format!("/work/stage-{}", self.dirs.borrow().len()));
        self.dirs.borrow_mut().push(path.clone());
        Ok(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.dirs.borrow_mut().push(path.to_path_buf());
        Ok(())
    }

    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut files = self.files.borrow_mut();
        if files.contains_key(path) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        files.insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn read(&self, path: &Path, _limit: u64) -> io::Result<Vec<u8>> {
        self.files.borrow().get(path).cloned().ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    fn run(&self, program: &str, args: &[OsString], env: &[(&str, &str)]) -> io::Result<Output> {
        let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let env = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        self.runs.borrow_mut().push((program.to_string(), args.clone(), env));
        let ok = |stdout: &str| Ok(Output { success: true, stdout: stdout.as_bytes().to_vec(), stderr: Vec::new() });
        match program {
            "xbps-rindex" => {
                let staged = Path::new(&args[1]);
                let index = staged.parent().unwrap().join(format!("{}-repodata", self.arch));
                let bytes = self.rindex_output.clone().unwrap_or_else(|| fixture("x86_64-repodata"));
                self.files.borrow_mut().insert(index, bytes);
                ok("")
            }
            "xbps-query" => match self.installed.borrow().as_deref() {
                Some(pkgver) => ok(pkgver),
                None => Ok(Output { success: false, stdout: Vec::new(), stderr: Vec::new() }),
            },
            "xbps-install" => match &self.installs_as {
                Some(pkgver) => {
                    *self.installed.borrow_mut() = Some(pkgver.clone());
                    ok("")
                }
                None => Ok(Output { success: false, stdout: Vec::new(), stderr: b"ERROR: unresolvable shlib\n".to_vec() }),
            },
            "xbps-remove" => {
                *self.installed.borrow_mut() = None;
                ok("")
            }
            other => panic!("unexpected program {other}"),
        }
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        self.removed.borrow_mut().push(path.to_path_buf());
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.files.borrow_mut().remove(path);
        Ok(())
    }
}

fn pins() -> PinStore {
    let mut pins = PinStore::new();
    pins.add_feed(FEED, &pinned_key());
    pins
}

fn install(sys: &Fake, pins: &mut PinStore, approved: Option<&Permissions>) -> Result<Outcome, InstallError> {
    let mut installer = Installer { sys, pins, policy: Policy::default() };
    installer.install(&Request { feed: FEED, package: "demo", approved })
}

/// Runs the first, unapproved attempt and returns what it asked the user to approve.
fn first_summary(sys: &Fake, pins: &mut PinStore) -> Permissions {
    match install(sys, pins, None).unwrap() {
        Outcome::NeedsApproval { pkgver, permissions, added } => {
            assert_eq!(pkgver, "demo-1.1_1");
            assert_eq!(added, permissions.granted.iter().cloned().collect::<Vec<_>>());
            permissions
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_first_install_stops_for_approval_and_changes_nothing() {
    let sys = Fake::new();
    let mut pins = pins();
    let summary = first_summary(&sys, &mut pins);
    assert_eq!(summary.summary(), ["keeps its data unencrypted"]);
    assert!(sys.runs.borrow().is_empty(), "{:?}", sys.runs.borrow());
    assert!(sys.files.borrow().is_empty());
    assert!(pins.feeds[FEED].highest.is_empty());
}

#[test]
fn an_approved_install_runs_xbps_privately_and_then_raises_the_floor() {
    let sys = Fake::new();
    let mut pins = pins();
    let summary = first_summary(&sys, &mut pins);
    let outcome = install(&sys, &mut pins, Some(&summary)).unwrap();
    assert_eq!(outcome, Outcome::Installed { pkgver: "demo-1.1_1".into(), permissions: summary });

    let staged = PathBuf::from("/work/stage-0/repo/demo-1.1_1.x86_64.xbps");
    assert_eq!(sys.files.borrow()[&staged], fixture("demo-1.1_1.x86_64.xbps"), "the inspected bytes are staged");
    let rindex = &sys.runs.borrow()[0];
    assert_eq!(rindex.0, "xbps-rindex");
    assert_eq!(rindex.1, ["-a", staged.to_str().unwrap()]);
    assert_eq!(rindex.2, [("XBPS_TARGET_ARCH".to_string(), "x86_64".to_string())]);
    assert_eq!(
        sys.ran("xbps-install"),
        [[
            "-C",
            "/work/stage-0/xbps.d",
            "--ignore-conf-repos",
            "--repository",
            "/work/stage-0/repo",
            "--yes",
            "demo"
        ]]
    );
    assert_eq!(pins.feeds[FEED].highest["demo"], "1.1_1");
    assert_eq!(*sys.removed.borrow(), [PathBuf::from("/work/stage-0")]);
}

#[test]
fn updates_and_reinstalls_pick_the_right_xbps_mode() {
    for (installed, mode) in [("demo-1.0_1", "--update"), ("demo-1.1_1", "--force")] {
        let sys = Fake::new();
        *sys.installed.borrow_mut() = Some(installed.into());
        let mut pins = pins();
        let summary = first_summary(&sys, &mut pins);
        install(&sys, &mut pins, Some(&summary)).unwrap();
        let args = &sys.ran("xbps-install")[0];
        assert_eq!(&args[args.len() - 2..], [mode, "demo"], "installed {installed}");
    }
}

#[test]
fn an_update_asking_for_more_needs_approval_again() {
    let sys = Fake::new();
    let mut pins = pins();
    // The previous version kept its data encrypted.
    let approved = Permissions::default();
    match install(&sys, &mut pins, Some(&approved)).unwrap() {
        Outcome::NeedsApproval { added, .. } => assert_eq!(added, [Permission::UnencryptedStorage]),
        other => panic!("{other:?}"),
    }
    assert!(sys.ran("xbps-install").is_empty());
}

#[test]
fn a_changed_key_or_unknown_feed_is_blocked() {
    let sys = Fake::new();
    let mut pins = PinStore::new();
    pins.add_feed(FEED, &KeyFingerprint::new(&"ab".repeat(16)).unwrap());
    let Outcome::Blocked(reasons) = install(&sys, &mut pins, None).unwrap() else { panic!() };
    assert!(matches!(&reasons[..], [BlockReason::KeyChanged { offered, .. }] if *offered == pinned_key()));

    let Outcome::Blocked(reasons) = install(&sys, &mut PinStore::new(), None).unwrap() else { panic!() };
    assert!(matches!(&reasons[..], [BlockReason::UnknownFeed { .. }]));
    assert!(sys.runs.borrow().is_empty());
}

#[test]
fn a_downgrade_is_blocked_until_approved() {
    let sys = Fake::new();
    let mut pins = pins();
    pins.record_installed(FEED, "demo", "1.2_1").unwrap();
    let Outcome::Blocked(reasons) = install(&sys, &mut pins, None).unwrap() else { panic!() };
    let approval = reasons[0].approval().unwrap();
    assert!(matches!(&approval, Approval::Downgrade { to, .. } if to == "1.1_1"));
    pins.apply_approval(&approval).unwrap();
    assert!(matches!(install(&sys, &mut pins, None).unwrap(), Outcome::NeedsApproval { .. }));
}

#[test]
fn tampered_bytes_or_signatures_are_refused_before_anything_runs() {
    let mut sys = Fake::new();
    let package = sys.feed.get_mut("demo-1.1_1.x86_64.xbps").unwrap();
    let last = package.len() - 1;
    package[last] ^= 1;
    assert!(matches!(install(&sys, &mut pins(), None), Err(InstallError::Signature(SignatureError::HashMismatch))));

    let mut sys = Fake::new();
    sys.feed.get_mut("demo-1.1_1.x86_64.xbps.sig2").unwrap()[0] ^= 1;
    assert!(matches!(install(&sys, &mut pins(), None), Err(InstallError::Signature(SignatureError::Mismatch))));

    let mut sys = Fake::new();
    sys.feed.insert("demo-1.1_1.x86_64.xbps.sig2".into(), fixture("demo-1.0_1.x86_64.xbps.sig2"));
    assert!(matches!(install(&sys, &mut pins(), None), Err(InstallError::Signature(SignatureError::Mismatch))));
    assert!(sys.runs.borrow().is_empty());
}

#[test]
fn an_unsigned_feed_is_refused() {
    let mut sys = Fake::new();
    let unsigned = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/unsigned-x86_64-repodata")).unwrap();
    sys.feed.insert("x86_64-repodata".into(), unsigned);
    assert!(matches!(install(&sys, &mut pins(), None), Err(InstallError::Repo(RepoError::Unsigned))));
}

#[test]
fn another_architecture_is_refused() {
    let mut sys = Fake::new();
    sys.arch = "aarch64".into();
    sys.feed.insert("aarch64-repodata".into(), fixture("x86_64-repodata"));
    let error = install(&sys, &mut pins(), None).unwrap_err();
    assert!(matches!(&error, InstallError::WrongArchitecture { found, device, .. } if found == "x86_64" && device == "aarch64"));
}

#[test]
fn a_missing_package_is_reported() {
    let sys = Fake::new();
    let mut pins = pins();
    let mut installer = Installer { sys: &sys, pins: &mut pins, policy: Policy::default() };
    let error = installer.install(&Request { feed: FEED, package: "nope", approved: None }).unwrap_err();
    assert!(matches!(error, InstallError::NotInFeed(name) if name == "nope"));
}

#[test]
fn a_regenerated_index_must_carry_the_inspected_hash() {
    let mut sys = Fake::new();
    // An index of the same repository made after the file was swapped for another version.
    sys.rindex_output = Some(std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/older-x86_64-repodata")).unwrap());
    let mut pins = pins();
    let summary = first_summary(&sys, &mut pins);
    let error = install(&sys, &mut pins, Some(&summary)).unwrap_err();
    assert!(matches!(error, InstallError::IndexMismatch(_)), "{error}");
    assert!(sys.ran("xbps-install").is_empty());
    assert!(pins.feeds[FEED].highest.is_empty());
    assert_eq!(sys.removed.borrow().len(), 1, "the stage is cleaned up");
}

#[test]
fn a_failed_xbps_install_leaves_the_floor_alone() {
    let mut sys = Fake::new();
    sys.installs_as = None;
    let mut pins = pins();
    let summary = first_summary(&sys, &mut pins);
    let error = install(&sys, &mut pins, Some(&summary)).unwrap_err();
    assert!(error.to_string().contains("unresolvable shlib"), "{error}");
    assert!(pins.feeds[FEED].highest.is_empty());
    assert_eq!(sys.removed.borrow().len(), 1);
}

#[test]
fn the_installed_version_is_confirmed() {
    let mut sys = Fake::new();
    sys.installs_as = Some("demo-1.0_1".into());
    let mut pins = pins();
    let summary = first_summary(&sys, &mut pins);
    let error = install(&sys, &mut pins, Some(&summary)).unwrap_err();
    assert!(matches!(error, InstallError::NotInstalled { found: Some(found), .. } if found == "demo-1.0_1"));
    assert!(pins.feeds[FEED].highest.is_empty());
}

#[test]
fn a_review_never_installs_even_when_already_approved() {
    let sys = Fake::new();
    let mut pins = pins();
    let summary = first_summary(&sys, &mut pins);
    let mut installer = Installer { sys: &sys, pins: &mut pins, policy: Policy::default() };
    let reviewed = installer.review(&Request { feed: FEED, package: "demo", approved: Some(&summary) }).unwrap();
    let Reviewed::Ready { pkgver, added, manifest, .. } = reviewed else { panic!("{reviewed:?}") };
    assert_eq!(pkgver, "demo-1.1_1");
    assert!(added.is_empty());
    assert_eq!(manifest.id, "demo");
    assert!(sys.runs.borrow().is_empty());
}

#[test]
fn a_version_other_than_the_reviewed_one_is_refused_before_xbps_runs() {
    let sys = Fake::new();
    let mut pins = pins();
    let summary = first_summary(&sys, &mut pins);
    let mut installer = Installer { sys: &sys, pins: &mut pins, policy: Policy::default() };
    let request = Request { feed: FEED, package: "demo", approved: Some(&summary) };
    let error = installer.install_checked(&request, Some("demo-1.0_1"), None).unwrap_err();
    assert!(matches!(error, InstallError::VersionChanged { .. }), "{error}");
    assert!(sys.runs.borrow().is_empty());
}

#[test]
fn a_kept_copy_is_the_verified_file_and_reinstalls_with_force() {
    let sys = Fake::new();
    let mut pins = pins();
    let summary = first_summary(&sys, &mut pins);
    let mut installer = Installer { sys: &sys, pins: &mut pins, policy: Policy::default() };
    let request = Request { feed: FEED, package: "demo", approved: Some(&summary) };
    let keep = Path::new("/keep");
    let (outcome, kept) = installer.install_checked(&request, Some("demo-1.1_1"), Some(keep)).unwrap();
    assert!(matches!(outcome, Outcome::Installed { .. }));
    let kept = kept.unwrap();
    assert_eq!(kept, Path::new("/keep/demo-1.1_1.x86_64.xbps"));
    assert_eq!(sys.files.borrow()[&kept], fixture("demo-1.1_1.x86_64.xbps"));

    // A later, broken version is installed, then the kept one comes back.
    *sys.installed.borrow_mut() = Some("demo-1.2_1".into());
    assert_eq!(installer.reinstall_kept(&kept).unwrap(), "demo-1.1_1");
    let args = sys.ran("xbps-install").pop().unwrap();
    assert_eq!(&args[args.len() - 2..], ["--force", "demo"]);
    assert_eq!(installer.pins.feeds[FEED].highest["demo"], "1.1_1");
}

#[test]
fn a_damaged_kept_copy_is_not_reinstalled() {
    let sys = Fake::new();
    let mut bad = fixture("demo-1.1_1.x86_64.xbps");
    bad.truncate(bad.len() / 2);
    sys.files.borrow_mut().insert("/keep/demo.xbps".into(), bad);
    let mut pins = pins();
    let installer = Installer { sys: &sys, pins: &mut pins, policy: Policy::default() };
    assert!(installer.reinstall_kept(Path::new("/keep/demo.xbps")).is_err());
    assert!(sys.ran("xbps-install").is_empty());
}

#[test]
fn remove_uses_the_private_configuration_and_skips_what_is_not_installed() {
    let sys = Fake::new();
    let mut pins = pins();
    let installer = Installer { sys: &sys, pins: &mut pins, policy: Policy::default() };
    installer.remove("demo").unwrap();
    assert!(sys.ran("xbps-remove").is_empty());
    *sys.installed.borrow_mut() = Some("demo-1.1_1".into());
    installer.remove("demo").unwrap();
    let args = sys.ran("xbps-remove").pop().unwrap();
    assert_eq!(args[0], "-C");
    assert_eq!(&args[2..], ["--yes", "demo"]);
}
