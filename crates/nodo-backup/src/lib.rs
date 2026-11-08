//! Encrypted backups of what a device cannot rebuild from its feeds: package settings, secrets,
//! the device key, the contents of encrypted storage and Tor onion keys. Chain data is never
//! included; it is large and can always be synced again.
//!
//! A lost device passphrase makes encrypted storage unreadable, so the backup holds the device key
//! itself, protected by the backup passphrase. Restoring then needs only the backup passphrase,
//! and the user sets a new device passphrase. That makes the backup passphrase as valuable as the
//! device passphrase, which is why it gets the same Argon2id cost and a minimum length.
//!
//! Restores are verified completely before anything is written, and write into a fresh staging
//! directory, so a tampered, truncated or wrongly unlocked backup leaves nothing behind.

pub mod crypto;
mod target;

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

pub use crypto::{KdfParams, MIN_PASSPHRASE_LEN};
pub use target::DirTarget;

use crypto::{OpenReader, SealWriter};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BackupError {
    #[error("this is not a monokulo-nodo backup")]
    NotABackup,
    /// Authenticated encryption cannot tell a wrong passphrase from damage to the first chunk.
    #[error("the passphrase did not open this backup, or its start is damaged")]
    WrongPassphrase,
    #[error("the backup was altered or truncated, so nothing was restored")]
    Tampered,
    #[error("the backup is damaged: {0}")]
    Corrupt(String),
    #[error("a backup passphrase needs at least {MIN_PASSPHRASE_LEN} characters")]
    WeakPassphrase,
    #[error("refused: {0}")]
    Refused(String),
    #[error("{0}")]
    Io(String),
}

impl From<io::Error> for BackupError {
    fn from(e: io::Error) -> Self {
        BackupError::Io(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Category {
    Settings,
    Secrets,
    DeviceKey,
    Storage,
    Onion,
}

impl Category {
    pub fn dir(self) -> &'static str {
        match self {
            Category::Settings => "settings",
            Category::Secrets => "secrets",
            Category::DeviceKey => "device-key",
            Category::Storage => "storage",
            Category::Onion => "onion",
        }
    }

    fn from_dir(dir: &str) -> Option<Self> {
        [Category::Settings, Category::Secrets, Category::DeviceKey, Category::Storage, Category::Onion]
            .into_iter()
            .find(|c| c.dir() == dir)
    }
}

/// A file or directory tree to back up, stored under `<category>/<name>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub category: Category,
    pub name: String,
    pub path: PathBuf,
}

/// Paths that hold chain data on a monokulo-nodo device.
pub const CHAIN_PATHS: &[&str] =
    &["/media/monero", "/var/lib/cuprated", "/var/lib/monerod", "/var/lib/apps/cuprated", "/var/lib/apps/monerod"];

pub const DEVICE_KEY_PATH: &str = "device-key/device.key";

pub struct Plan {
    pub sources: Vec<Source>,
    pub device_key: Option<Zeroizing<Vec<u8>>>,
    /// Never read, even when inside a source.
    pub exclude: Vec<PathBuf>,
}

impl Plan {
    /// The standard layout. `apps` are installed package ids, `encrypted` those with unlocked
    /// encrypted storage, and `device_key` the unwrapped key from the helper.
    pub fn standard(apps: &[&str], encrypted: &[&str], device_key: Option<Zeroizing<Vec<u8>>>) -> Plan {
        let mut sources = vec![
            Source { category: Category::Settings, name: "nodo".into(), path: "/var/lib/nodo/settings".into() },
            Source { category: Category::Settings, name: "exposure".into(), path: "/var/lib/nodo/exposure.toml".into() },
            Source { category: Category::Secrets, name: "nodo".into(), path: "/var/lib/nodo/secrets".into() },
            Source { category: Category::Onion, name: "tor".into(), path: "/var/lib/tor".into() },
        ];
        for id in apps {
            sources.push(Source { category: Category::Settings, name: format!("app-{id}"), path: format!("/etc/apps/{id}").into() });
        }
        for id in encrypted {
            sources.push(Source { category: Category::Storage, name: (*id).into(), path: format!("/run/nodo/volumes/{id}").into() });
        }
        Plan { sources, device_key, exclude: CHAIN_PATHS.iter().map(PathBuf::from).collect() }
    }

    fn check(&self) -> Result<(), BackupError> {
        for s in &self.sources {
            if !component_ok(&s.name) {
                return Err(BackupError::Refused(format!("bad source name {:?}", s.name)));
            }
            if s.category == Category::DeviceKey {
                return Err(BackupError::Refused("the device key is not a file source".into()));
            }
            // A source that contains chain data is fine: the walk skips the excluded paths.
            for ex in &self.exclude {
                if s.path.starts_with(ex) {
                    return Err(BackupError::Refused(format!("{} holds chain data", s.path.display())));
                }
            }
        }
        Ok(())
    }
}

fn component_ok(c: &str) -> bool {
    !c.is_empty() && c != "." && c != ".." && c.len() <= 255 && !c.contains(['/', '\0'])
}

/// Validates a path from inside a backup. It is authenticated, but a backup made on another
/// device may be from someone else, so it is still treated as untrusted input.
pub fn archive_path_ok(path: &str) -> Option<Category> {
    if path.len() > 4096 {
        return None;
    }
    let mut parts = path.split('/');
    let category = Category::from_dir(parts.next()?)?;
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() || !rest.iter().all(|c| component_ok(c)) {
        return None;
    }
    if (category == Category::DeviceKey) != (path == DEVICE_KEY_PATH) {
        return None;
    }
    Some(category)
}

const TAG_FILE: u8 = 1;
const TAG_DIR: u8 = 2;
const TAG_KEY: u8 = 3;
const TAG_END: u8 = 0xff;
/// Permission bits only. setuid, setgid and sticky never survive a backup.
const MODE_MASK: u32 = 0o777;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Kind {
    File,
    Dir,
    DeviceKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryInfo {
    pub path: String,
    pub kind: Kind,
    pub mode: u32,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub entries: Vec<EntryInfo>,
    pub missing: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
}

impl Summary {
    pub fn has_device_key(&self) -> bool {
        self.entries.iter().any(|e| e.kind == Kind::DeviceKey)
    }
}

struct Writer<W: Write> {
    seal: SealWriter<W>,
    summary: Summary,
}

impl<W: Write> Writer<W> {
    fn header(&mut self, tag: u8, path: &str, mode: u32, size: u64) -> Result<(), BackupError> {
        let len = u16::try_from(path.len()).map_err(|_| BackupError::Refused(format!("path too long: {path}")))?;
        let mut h = Vec::with_capacity(15 + path.len());
        h.push(tag);
        h.extend_from_slice(&len.to_be_bytes());
        h.extend_from_slice(path.as_bytes());
        h.extend_from_slice(&(mode & MODE_MASK).to_be_bytes());
        h.extend_from_slice(&size.to_be_bytes());
        self.seal.write_all(&h)?;
        let kind = match tag {
            TAG_FILE => Kind::File,
            TAG_DIR => Kind::Dir,
            _ => Kind::DeviceKey,
        };
        self.summary.entries.push(EntryInfo { path: path.to_owned(), kind, mode: mode & MODE_MASK, size });
        Ok(())
    }

    fn file(&mut self, path: &str, host: &Path, mode: u32, size: u64) -> Result<(), BackupError> {
        let mut f = File::open(host)?;
        self.header(TAG_FILE, path, mode, size)?;
        let mut left = size;
        let mut buf = Zeroizing::new(vec![0u8; 64 * 1024]);
        while left > 0 {
            let want = left.min(buf.len() as u64) as usize;
            let n = f.read(&mut buf[..want])?;
            if n == 0 {
                return Err(BackupError::Io(format!("{} shrank while being backed up", host.display())));
            }
            self.seal.write_all(&buf[..n])?;
            left -= n as u64;
        }
        Ok(())
    }

    fn walk(&mut self, prefix: &str, host: &Path, exclude: &[PathBuf]) -> Result<(), BackupError> {
        if exclude.iter().any(|ex| host.starts_with(ex)) {
            self.summary.skipped.push(host.to_owned());
            return Ok(());
        }
        let meta = fs::symlink_metadata(host)?;
        let mode = meta.permissions().mode();
        if meta.is_file() {
            return self.file(prefix, host, mode, meta.len());
        }
        if !meta.is_dir() {
            // Symlinks, sockets and devices: a link could point at chain data or another
            // package's files, so it is never followed.
            self.summary.skipped.push(host.to_owned());
            return Ok(());
        }
        self.header(TAG_DIR, prefix, mode, 0)?;
        let mut names: Vec<_> = fs::read_dir(host)?.map(|e| e.map(|e| e.file_name())).collect::<Result<_, _>>()?;
        names.sort();
        for name in names {
            let Some(text) = name.to_str().filter(|t| component_ok(t)) else {
                self.summary.skipped.push(host.join(&name));
                continue;
            };
            self.walk(&format!("{prefix}/{text}"), &host.join(&name), exclude)?;
        }
        Ok(())
    }
}

/// Writes an encrypted backup of `plan` to `out`.
pub fn backup<W: Write>(plan: &Plan, passphrase: &[u8], params: KdfParams, out: W, now: u64) -> Result<Summary, BackupError> {
    plan.check()?;
    let mut w = Writer { seal: SealWriter::new(out, passphrase, params, now)?, summary: Summary::default() };
    if let Some(key) = &plan.device_key {
        w.header(TAG_KEY, DEVICE_KEY_PATH, 0o600, key.len() as u64)?;
        w.seal.write_all(key)?;
    }
    for source in &plan.sources {
        if fs::symlink_metadata(&source.path).is_err() {
            w.summary.missing.push(source.path.clone());
            continue;
        }
        let prefix = format!("{}/{}", source.category.dir(), source.name);
        w.walk(&prefix, &source.path, &plan.exclude)?;
    }
    w.seal.write_all(&[TAG_END])?;
    w.seal.write_all(&(w.summary.entries.len() as u64).to_be_bytes())?;
    w.seal.finish()?;
    Ok(w.summary)
}

/// Receives a verified backup's contents, in order.
pub trait RestoreTarget {
    fn dir(&mut self, path: &str, mode: u32) -> Result<(), BackupError>;
    fn file_begin(&mut self, path: &str, mode: u32, size: u64) -> Result<(), BackupError>;
    fn file_data(&mut self, data: &[u8]) -> Result<(), BackupError>;
    fn file_end(&mut self) -> Result<(), BackupError>;
    fn device_key(&mut self, key: Zeroizing<Vec<u8>>) -> Result<(), BackupError>;
    /// Removes everything written so far.
    fn abort(&mut self);
}

fn u16_at(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn read_entries<R: Read>(r: &mut OpenReader<R>, mut target: Option<&mut dyn RestoreTarget>) -> Result<Summary, BackupError> {
    let mut summary = Summary::default();
    let max_key = 1024;
    loop {
        let mut tag = [0u8; 1];
        r.read_exact(&mut tag)?;
        if tag[0] == TAG_END {
            let mut count = [0u8; 8];
            r.read_exact(&mut count)?;
            if u64::from_be_bytes(count) != summary.entries.len() as u64 || !r.at_end()? {
                return Err(BackupError::Corrupt("entry count does not match".into()));
            }
            return Ok(summary);
        }
        let mut len = [0u8; 2];
        r.read_exact(&mut len)?;
        let mut path = vec![0u8; u16_at(&len) as usize];
        r.read_exact(&mut path)?;
        let path = String::from_utf8(path).map_err(|_| BackupError::Corrupt("path is not UTF-8".into()))?;
        let mut fixed = [0u8; 12];
        r.read_exact(&mut fixed)?;
        let mode = u32::from_be_bytes([fixed[0], fixed[1], fixed[2], fixed[3]]) & MODE_MASK;
        let size = u64::from_be_bytes(fixed[4..].try_into().expect("8 bytes"));
        let category = archive_path_ok(&path).ok_or_else(|| BackupError::Corrupt(format!("unsafe path {path:?}")))?;
        let kind = match tag[0] {
            TAG_FILE if category != Category::DeviceKey => Kind::File,
            TAG_DIR if category != Category::DeviceKey && size == 0 => Kind::Dir,
            TAG_KEY if category == Category::DeviceKey && size <= max_key => Kind::DeviceKey,
            _ => return Err(BackupError::Corrupt(format!("bad entry for {path:?}"))),
        };
        if kind == Kind::DeviceKey && summary.has_device_key() {
            return Err(BackupError::Corrupt("two device keys".into()));
        }
        match kind {
            Kind::Dir => {
                if let Some(t) = target.as_deref_mut() {
                    t.dir(&path, mode)?;
                }
            }
            Kind::DeviceKey => {
                let mut key = Zeroizing::new(vec![0u8; size as usize]);
                r.read_exact(&mut key)?;
                if let Some(t) = target.as_deref_mut() {
                    t.device_key(key)?;
                }
            }
            Kind::File => {
                if let Some(t) = target.as_deref_mut() {
                    t.file_begin(&path, mode, size)?;
                }
                let mut left = size;
                let mut buf = Zeroizing::new(vec![0u8; 64 * 1024]);
                while left > 0 {
                    let n = left.min(buf.len() as u64) as usize;
                    r.read_exact(&mut buf[..n])?;
                    if let Some(t) = target.as_deref_mut() {
                        t.file_data(&buf[..n])?;
                    }
                    left -= n as u64;
                }
                if let Some(t) = target.as_deref_mut() {
                    t.file_end()?;
                }
            }
        }
        summary.entries.push(EntryInfo { path, kind, mode, size });
    }
}

/// Decrypts and checks a whole backup without writing anything.
pub fn verify<R: Read>(input: R, passphrase: &[u8]) -> Result<Summary, BackupError> {
    let mut r = OpenReader::new(input, passphrase)?;
    read_entries(&mut r, None)
}

/// Verifies the whole backup, then restores it into `target`. Any failure in either pass leaves
/// the target empty.
pub fn restore<R: Read + Seek>(mut input: R, passphrase: &[u8], target: &mut dyn RestoreTarget) -> Result<Summary, BackupError> {
    let result = (|| {
        let start = input.stream_position()?;
        verify(&mut input, passphrase)?;
        input.seek(SeekFrom::Start(start))?;
        let mut r = OpenReader::new(&mut input, passphrase)?;
        read_entries(&mut r, Some(&mut *target))
    })();
    if result.is_err() {
        target.abort();
    }
    result
}

#[cfg(test)]
mod tests;
