//! Everything the installer does to the machine, behind one trait so the logic can be tested.

use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// What a finished command produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub trait System {
    /// The xbps architecture of this device, such as `aarch64`.
    fn device_arch(&self) -> io::Result<String>;
    /// Reads one file of a feed, refusing anything longer than `limit` bytes.
    fn fetch(&self, feed: &str, file: &str, limit: u64) -> io::Result<Vec<u8>>;
    /// A new, empty directory that only this process's user can enter.
    fn private_dir(&self) -> io::Result<PathBuf>;
    fn create_dir(&self, path: &Path) -> io::Result<()>;
    /// Writes a file that must not exist yet, and makes it durable before returning.
    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    fn read(&self, path: &Path, limit: u64) -> io::Result<Vec<u8>>;
    /// Runs a program directly, never through a shell.
    fn run(&self, program: &str, args: &[OsString], env: &[(&str, &str)]) -> io::Result<Output>;
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
}

fn read_limited(reader: impl Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("larger than {limit} bytes")));
    }
    Ok(bytes)
}

/// The real machine.
pub struct RealSystem {
    /// Root of the installer's work directories. It must belong to this user and be closed to
    /// everyone else, because the package file sits there between inspection and installation.
    work: PathBuf,
    counter: AtomicU64,
}

impl RealSystem {
    pub fn new(work: impl Into<PathBuf>) -> io::Result<Self> {
        let work = work.into();
        let meta = fs::symlink_metadata(&work)?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if !meta.is_dir() || meta.uid() != euid || meta.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} must be a directory owned by this user with mode 0700", work.display()),
            ));
        }
        Ok(RealSystem { work, counter: AtomicU64::new(0) })
    }

    fn fetch_url(&self, url: &str, limit: u64) -> io::Result<Vec<u8>> {
        let dir = self.private_dir()?;
        let target = dir.join("download");
        let result = self
            .run("xbps-fetch", &["-o".into(), target.clone().into(), url.into()], &[])
            .and_then(|out| {
                if out.success {
                    self.read(&target, limit)
                } else {
                    Err(io::Error::other(format!("xbps-fetch failed: {}", String::from_utf8_lossy(&out.stderr).trim())))
                }
            });
        let _ = self.remove_dir_all(&dir);
        result
    }
}

impl System for RealSystem {
    fn device_arch(&self) -> io::Result<String> {
        let out = self.run("xbps-uhelper", &["arch".into()], &[])?;
        let arch = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.success || arch.is_empty() {
            return Err(io::Error::other("xbps-uhelper arch failed"));
        }
        Ok(arch)
    }

    fn fetch(&self, feed: &str, file: &str, limit: u64) -> io::Result<Vec<u8>> {
        if feed.starts_with("https://") || feed.starts_with("http://") {
            return self.fetch_url(&format!("{}/{file}", feed.trim_end_matches('/')), limit);
        }
        self.read(&Path::new(feed).join(file), limit)
    }

    fn private_dir(&self) -> io::Result<PathBuf> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        loop {
            let n = self.counter.fetch_add(1, Ordering::Relaxed);
            let path = self.work.join(format!("stage-{}-{nanos}-{n}", std::process::id()));
            // create_dir fails if anything already has the name, so the directory is always new.
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        DirBuilder::new().mode(0o700).create(path)
    }

    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    }

    fn read(&self, path: &Path, limit: u64) -> io::Result<Vec<u8>> {
        read_limited(fs::File::open(path)?, limit)
    }

    fn run(&self, program: &str, args: &[OsString], env: &[(&str, &str)]) -> io::Result<Output> {
        let mut command = Command::new(program);
        command.args(args).stdin(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        let out = command.output()?;
        Ok(Output { success: out.status.success(), stdout: out.stdout, stderr: out.stderr })
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_work_directory_must_be_private() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(RealSystem::new(dir.path()).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let sys = RealSystem::new(dir.path()).unwrap();
        let a = sys.private_dir().unwrap();
        let b = sys.private_dir().unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::metadata(&a).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn files_are_written_once_and_reads_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let sys = RealSystem::new(dir.path()).unwrap();
        let path = dir.path().join("f");
        sys.write_new(&path, b"hello").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(sys.write_new(&path, b"again").is_err(), "an existing file is never reused");
        assert_eq!(sys.read(&path, 5).unwrap(), b"hello");
        assert!(sys.read(&path, 4).is_err());
        assert_eq!(sys.fetch(dir.path().to_str().unwrap(), "f", 5).unwrap(), b"hello");
    }
}
