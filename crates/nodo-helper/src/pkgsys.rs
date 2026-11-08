//! What feeds, installs and updates do to the machine, behind one trait.
//!
//! The real implementation hands feeds and package files to `nodo-install`, which verifies and
//! inspects them and runs xbps privately. Tests use a scripted fake, so the helper's rules can be
//! checked without signed packages or root.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use nodo_feed::{parse_repodata, KeyFingerprint, PinStore, Policy};
use nodo_install::{InstallError, Installer, Outcome, Permissions, Request, Reviewed};

/// A feed's index as far as this device cares: who signs it and what it offers for this
/// architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedIndex {
    pub fingerprint: KeyFingerprint,
    pub packages: Vec<IndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndexEntry {
    pub name: String,
    /// `version_revision`.
    pub version: String,
    pub size: u64,
}

pub trait PackageBackend: Send + Sync {
    fn index(&self, address: &str) -> Result<FeedIndex, String>;
    /// Downloads, verifies and inspects without installing. Never changes `pins`.
    fn review(
        &self,
        pins: &mut PinStore,
        address: &str,
        package: &str,
        approved: Option<&Permissions>,
    ) -> Result<Reviewed, InstallError>;
    /// Installs exactly `pkgver`, keeping a copy of the verified file in `keep`.
    fn install(
        &self,
        pins: &mut PinStore,
        address: &str,
        package: &str,
        approved: &Permissions,
        pkgver: &str,
        keep: &Path,
    ) -> Result<(Outcome, Option<PathBuf>), InstallError>;
    fn reinstall(&self, kept: &Path) -> Result<String, InstallError>;
    fn remove(&self, name: &str) -> Result<(), InstallError>;
    fn now(&self) -> u64;
    /// Device-local minutes since midnight, for the update window.
    fn local_minute(&self) -> u16;
    /// Copies a data directory for rollback, without following symlinks. `skip` names an entry
    /// at the top level to leave out, which is where an in-place copy itself lives.
    fn copy_tree(&self, from: &Path, to: &Path, skip: Option<&str>) -> Result<(), String>;
    /// Replaces the contents of `to` with those of `from`, keeping `to` itself and `skip`.
    fn restore_tree(&self, from: &Path, to: &Path, skip: Option<&str>) -> Result<(), String>;
    fn remove_tree(&self, path: &Path) -> Result<(), String>;
    /// Bytes under `path`, without following symlinks.
    fn tree_size(&self, path: &Path) -> u64;
    /// Bytes an unprivileged writer can still use on the filesystem holding `path`.
    fn free_bytes(&self, path: &Path) -> u64;
    /// Whether a kept package file is still there.
    fn kept_exists(&self, path: &Path) -> bool;
    fn remove_kept(&self, path: &Path);
}

/// Lets the generic installer run on a trait object.
struct Dyn<'a>(&'a dyn nodo_install::System);

impl nodo_install::System for Dyn<'_> {
    fn device_arch(&self) -> std::io::Result<String> {
        self.0.device_arch()
    }
    fn fetch(&self, feed: &str, file: &str, limit: u64) -> std::io::Result<Vec<u8>> {
        self.0.fetch(feed, file, limit)
    }
    fn private_dir(&self) -> std::io::Result<PathBuf> {
        self.0.private_dir()
    }
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        self.0.create_dir(path)
    }
    fn write_new(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        self.0.write_new(path, bytes)
    }
    fn read(&self, path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
        self.0.read(path, limit)
    }
    fn run(
        &self,
        program: &str,
        args: &[OsString],
        env: &[(&str, &str)],
    ) -> std::io::Result<nodo_install::Output> {
        self.0.run(program, args, env)
    }
    fn remove_dir_all(&self, path: &Path) -> std::io::Result<()> {
        self.0.remove_dir_all(path)
    }
    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        self.0.remove_file(path)
    }
}

const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;

pub struct RealPackages {
    /// The installer's private work directory, created on first use.
    pub work: PathBuf,
}

impl Default for RealPackages {
    fn default() -> Self {
        RealPackages {
            work: "/var/lib/nodo/install-work".into(),
        }
    }
}

fn io_text(e: std::io::Error) -> String {
    e.to_string()
}

impl RealPackages {
    fn sys(&self) -> Result<nodo_install::RealSystem, InstallError> {
        let io = |source| InstallError::Io {
            action: "prepare the installer's work directory".to_owned(),
            source,
        };
        match fs::symlink_metadata(&self.work) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.work)
                .map_err(io)?,
            Err(e) => return Err(io(e)),
            Ok(_) => {}
        }
        nodo_install::RealSystem::new(&self.work).map_err(io)
    }

    fn installer<T>(
        &self,
        pins: &mut PinStore,
        f: impl FnOnce(&mut Installer<'_, Dyn<'_>>) -> Result<T, InstallError>,
    ) -> Result<T, InstallError> {
        let sys = self.sys()?;
        let dynamic = Dyn(&sys);
        let mut installer = Installer {
            sys: &dynamic,
            pins,
            policy: Policy::default(),
        };
        f(&mut installer)
    }
}

/// Runs a program with an argument vector, never a shell.
fn run(program: &str, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn entries(dir: &Path, skip: Option<&str>) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(io_text)?;
        if skip.is_some_and(|s| entry.file_name() == s) {
            continue;
        }
        out.push(entry.path());
    }
    Ok(out)
}

impl PackageBackend for RealPackages {
    fn index(&self, address: &str) -> Result<FeedIndex, String> {
        use nodo_install::System as _;
        let sys = self.sys().map_err(|e| e.to_string())?;
        let arch = sys.device_arch().map_err(io_text)?;
        let bytes = sys
            .fetch(address, &format!("{arch}-repodata"), MAX_INDEX_BYTES)
            .map_err(|e| format!("could not read the feed's index: {e}"))?;
        let repo = parse_repodata(&bytes).map_err(|e| e.to_string())?;
        let key = repo.signing_key().map_err(|e| e.to_string())?;
        let packages = repo
            .packages
            .values()
            .filter(|e| e.architecture == arch || e.architecture == "noarch")
            .map(|e| IndexEntry {
                name: e.name.clone(),
                version: e.version.clone(),
                size: e.size,
            })
            .collect();
        Ok(FeedIndex {
            fingerprint: key.fingerprint.clone(),
            packages,
        })
    }

    fn review(
        &self,
        pins: &mut PinStore,
        address: &str,
        package: &str,
        approved: Option<&Permissions>,
    ) -> Result<Reviewed, InstallError> {
        self.installer(pins, |i| {
            i.review(&Request {
                feed: address,
                package,
                approved,
            })
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
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(keep)
            .map_err(|source| InstallError::Io {
                action: "create the directory for kept packages".to_owned(),
                source,
            })?;
        self.installer(pins, |i| {
            let request = Request {
                feed: address,
                package,
                approved: Some(approved),
            };
            i.install_checked(&request, Some(pkgver), Some(keep))
        })
    }

    fn reinstall(&self, kept: &Path) -> Result<String, InstallError> {
        let mut pins = PinStore::new();
        self.installer(&mut pins, |i| i.reinstall_kept(kept))
    }

    fn remove(&self, name: &str) -> Result<(), InstallError> {
        let mut pins = PinStore::new();
        self.installer(&mut pins, |i| i.remove(name))
    }

    fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// `date` applies the device's time zone, which the helper cannot do itself without libc.
    fn local_minute(&self) -> u16 {
        let text = run("date", &["+%H %M".as_ref()]).unwrap_or_default();
        let mut parts = text.split_whitespace().map(|p| p.parse::<u16>().ok());
        match (parts.next().flatten(), parts.next().flatten()) {
            (Some(h), Some(m)) if h < 24 && m < 60 => h * 60 + m,
            _ => ((self.now() % 86_400) / 60) as u16,
        }
    }

    fn copy_tree(&self, from: &Path, to: &Path, skip: Option<&str>) -> Result<(), String> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(to)
            .map_err(io_text)?;
        for entry in entries(from, skip)? {
            // -a keeps owners, modes and symlinks as links. -T makes the target exact.
            let name = entry.file_name().ok_or("bad entry")?;
            run(
                "cp",
                &[
                    "-a".as_ref(),
                    "-T".as_ref(),
                    entry.as_os_str(),
                    to.join(name).as_os_str(),
                ],
            )?;
        }
        Ok(())
    }

    fn restore_tree(&self, from: &Path, to: &Path, skip: Option<&str>) -> Result<(), String> {
        for entry in entries(to, skip)? {
            let meta = fs::symlink_metadata(&entry).map_err(io_text)?;
            if meta.is_dir() {
                fs::remove_dir_all(&entry).map_err(io_text)?;
            } else {
                fs::remove_file(&entry).map_err(io_text)?;
            }
        }
        for entry in entries(from, None)? {
            let name = entry.file_name().ok_or("bad entry")?;
            run(
                "cp",
                &[
                    "-a".as_ref(),
                    "-T".as_ref(),
                    entry.as_os_str(),
                    to.join(name).as_os_str(),
                ],
            )?;
        }
        Ok(())
    }

    fn remove_tree(&self, path: &Path) -> Result<(), String> {
        match fs::remove_dir_all(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }

    fn free_bytes(&self, path: &Path) -> u64 {
        rustix::fs::statvfs(path)
            .map(|s| s.f_bavail.saturating_mul(s.f_frsize))
            .unwrap_or(0)
    }

    fn kept_exists(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
    }

    fn remove_kept(&self, path: &Path) {
        let _ = fs::remove_file(path);
    }

    fn tree_size(&self, path: &Path) -> u64 {
        fn walk(path: &Path, dev: u64) -> u64 {
            let Ok(meta) = fs::symlink_metadata(path) else {
                return 0;
            };
            if !meta.is_dir() {
                return meta.len();
            }
            // Another filesystem mounted inside is not this package's data.
            if meta.dev() != dev {
                return 0;
            }
            fs::read_dir(path)
                .map(|list| list.flatten().map(|e| walk(&e.path(), dev)).sum())
                .unwrap_or(0)
        }
        match fs::symlink_metadata(path) {
            Ok(meta) => walk(path, meta.dev()),
            Err(_) => 0,
        }
    }
}
