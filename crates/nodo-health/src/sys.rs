//! Everything the health daemon needs from the system, behind one trait.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub total: u64,
    /// What an unprivileged service can still write. Root has the ext4 reserve on top.
    pub available: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub is_file: bool,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvAction {
    /// Stops the service and waits. runit runs `control/t`, so bwrap services shut down cleanly.
    Down,
    Up,
    /// Makes svlogd reread its `config`.
    HupLog,
}

pub trait System {
    fn space(&self, path: &Path) -> Option<Space>;
    fn is_mountpoint(&self, path: &Path) -> bool;
    fn read(&self, path: &Path) -> io::Result<Option<String>>;
    /// Atomic replace, mode 0644.
    fn write(&self, path: &Path, contents: &str) -> io::Result<()>;
    /// Writes into an existing file in place, for kernel interfaces such as cgroupfs, where a
    /// temporary file and rename are impossible.
    fn write_in_place(&self, path: &Path, contents: &str) -> io::Result<()>;
    /// Removes a file or symlink. Never follows a symlink.
    fn remove(&self, path: &Path) -> io::Result<()>;
    fn mkdir(&self, path: &Path) -> io::Result<()>;
    /// Entries without following symlinks, so a service cannot redirect a size count or a delete.
    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>>;
    fn sv(&self, action: SvAction, service: &str) -> Result<(), String>;
    fn sv_status(&self, service: &str) -> Option<String>;
    fn command(&self, program: &str, args: &[&str]) -> Option<String>;
    fn uid_of(&self, user: &str) -> Option<u32>;
    fn pids_of_uid(&self, uid: u32) -> Vec<u32>;
    fn now(&self) -> u64;
}

pub struct RealSystem {
    pub sv_dir: PathBuf,
}

impl Default for RealSystem {
    fn default() -> Self {
        RealSystem { sv_dir: "/etc/sv".into() }
    }
}

impl System for RealSystem {
    fn space(&self, path: &Path) -> Option<Space> {
        let st = rustix::fs::statvfs(path).ok()?;
        Some(Space { total: st.f_blocks * st.f_frsize, available: st.f_bavail * st.f_frsize })
    }

    fn is_mountpoint(&self, path: &Path) -> bool {
        let (Ok(here), Ok(parent)) = (fs::metadata(path), fs::metadata(path.join(".."))) else { return false };
        here.dev() != parent.dev() || here.ino() == parent.ino()
    }

    fn read(&self, path: &Path) -> io::Result<Option<String>> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        let dir = path.parent().ok_or_else(|| io::Error::other("no parent directory"))?;
        let name = path.file_name().ok_or_else(|| io::Error::other("no file name"))?;
        let tmp = dir.join(format!(".{}.nodo-health", name.to_string_lossy()));
        let _ = fs::remove_file(&tmp);
        // create_new refuses a symlink planted at the temporary name.
        let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    }

    fn write_in_place(&self, path: &Path, contents: &str) -> io::Result<()> {
        let mut file = fs::OpenOptions::new().write(true).open(path)?;
        file.write_all(contents.as_bytes())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    fn mkdir(&self, path: &Path) -> io::Result<()> {
        match fs::create_dir(path) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            other => other,
        }
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            out.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                size: meta.len(),
                is_file: meta.file_type().is_file(),
                is_dir: meta.file_type().is_dir(),
            });
        }
        Ok(out)
    }

    fn sv(&self, action: SvAction, service: &str) -> Result<(), String> {
        let dir = self.sv_dir.join(service);
        let args: Vec<String> = match action {
            SvAction::Down => vec!["-w".into(), "180".into(), "down".into(), dir.display().to_string()],
            SvAction::Up => vec!["up".into(), dir.display().to_string()],
            SvAction::HupLog => vec!["hup".into(), dir.join("log").display().to_string()],
        };
        let out = Command::new("sv").args(&args).output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        }
    }

    fn sv_status(&self, service: &str) -> Option<String> {
        let out = Command::new("sv").arg("status").arg(self.sv_dir.join(service)).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn command(&self, program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn uid_of(&self, user: &str) -> Option<u32> {
        let passwd = fs::read_to_string("/etc/passwd").ok()?;
        passwd.lines().find_map(|line| {
            let mut fields = line.split(':');
            (fields.next()? == user).then(|| fields.nth(1)?.parse().ok()).flatten()
        })
    }

    fn pids_of_uid(&self, uid: u32) -> Vec<u32> {
        let Ok(dir) = fs::read_dir("/proc") else { return Vec::new() };
        dir.filter_map(|e| {
            let e = e.ok()?;
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            (e.metadata().ok()?.uid() == uid).then_some(pid)
        })
        .collect()
    }

    fn now(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }
}

#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    /// An in-memory filesystem tree plus recorded service actions.
    #[derive(Default)]
    pub struct FakeSystem {
        pub files: RefCell<BTreeMap<PathBuf, String>>,
        pub sizes: RefCell<BTreeMap<PathBuf, u64>>,
        pub space: RefCell<BTreeMap<PathBuf, Space>>,
        pub mounts: RefCell<BTreeSet<PathBuf>>,
        pub status: RefCell<BTreeMap<String, String>>,
        pub calls: RefCell<Vec<String>>,
        pub outputs: RefCell<BTreeMap<String, String>>,
        pub users: BTreeMap<String, u32>,
        pub pids: BTreeMap<u32, Vec<u32>>,
        pub clock: std::cell::Cell<u64>,
    }

    impl FakeSystem {
        pub fn set_space(&self, path: &str, total: u64, available: u64) {
            self.space.borrow_mut().insert(path.into(), Space { total, available });
            self.mounts.borrow_mut().insert(path.into());
        }

        pub fn put(&self, path: &str, contents: &str) {
            self.files.borrow_mut().insert(path.into(), contents.into());
        }

        pub fn put_sized(&self, path: &str, size: u64) {
            self.put(path, "");
            self.sizes.borrow_mut().insert(path.into(), size);
        }

        pub fn exists(&self, path: &str) -> bool {
            self.files.borrow().contains_key(Path::new(path))
        }

        pub fn get(&self, path: &str) -> Option<String> {
            self.files.borrow().get(Path::new(path)).cloned()
        }

        pub fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl System for FakeSystem {
        fn space(&self, path: &Path) -> Option<Space> {
            self.space.borrow().get(path).copied()
        }
        fn is_mountpoint(&self, path: &Path) -> bool {
            self.mounts.borrow().contains(path)
        }
        fn read(&self, path: &Path) -> io::Result<Option<String>> {
            Ok(self.files.borrow().get(path).cloned())
        }
        fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
            self.calls.borrow_mut().push(format!("write {}", path.display()));
            self.files.borrow_mut().insert(path.into(), contents.into());
            Ok(())
        }
        fn write_in_place(&self, path: &Path, contents: &str) -> io::Result<()> {
            self.write(path, contents)
        }
        fn remove(&self, path: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push(format!("remove {}", path.display()));
            self.files.borrow_mut().remove(path);
            Ok(())
        }
        fn mkdir(&self, path: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push(format!("mkdir {}", path.display()));
            Ok(())
        }
        fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
            let files = self.files.borrow();
            let mut names = BTreeMap::new();
            for path in files.keys() {
                let Ok(rest) = path.strip_prefix(dir) else { continue };
                let mut parts = rest.components();
                let Some(first) = parts.next() else { continue };
                let name = first.as_os_str().to_string_lossy().into_owned();
                let is_dir = parts.next().is_some();
                let size = if is_dir { 0 } else { self.sizes.borrow().get(path).copied().unwrap_or(0) };
                names.entry(name.clone()).or_insert(Entry { name, size, is_file: !is_dir, is_dir });
            }
            if names.is_empty() {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            Ok(names.into_values().collect())
        }
        fn sv(&self, action: SvAction, service: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("sv {action:?} {service}"));
            let state = match action {
                SvAction::Down => format!("down: {service}: 1s, normally up"),
                SvAction::Up => format!("run: {service}: (pid 1) 1s"),
                SvAction::HupLog => return Ok(()),
            };
            self.status.borrow_mut().insert(service.into(), state);
            Ok(())
        }
        fn sv_status(&self, service: &str) -> Option<String> {
            self.status.borrow().get(service).cloned()
        }
        fn command(&self, program: &str, _args: &[&str]) -> Option<String> {
            self.outputs.borrow().get(program).cloned()
        }
        fn uid_of(&self, user: &str) -> Option<u32> {
            self.users.get(user).copied()
        }
        fn pids_of_uid(&self, uid: u32) -> Vec<u32> {
            self.pids.get(&uid).cloned().unwrap_or_default()
        }
        fn now(&self) -> u64 {
            self.clock.get()
        }
    }
}
