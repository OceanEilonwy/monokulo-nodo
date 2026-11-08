//! Everything the helper does to the machine.

use std::path::Path;
use std::time::Duration;

use nodo_api::ApiError;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct SysError(pub String);

impl From<std::io::Error> for SysError {
    fn from(e: std::io::Error) -> Self {
        SysError(e.to_string())
    }
}

impl From<SysError> for ApiError {
    fn from(e: SysError) -> Self {
        ApiError::Failed(e.0)
    }
}

pub type SysResult<T> = Result<T, SysError>;

/// `sv` commands. `force-*` honours the manifest's stop timeout, then kills, which is what
/// `stop_timeout_secs` promises and keeps a stuck service from blocking a restart forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvAction {
    Start,
    ForceStop,
    ForceRestart,
}

impl SvAction {
    pub fn as_arg(self) -> &'static str {
        match self {
            SvAction::Start => "start",
            SvAction::ForceStop => "force-stop",
            SvAction::ForceRestart => "force-restart",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOptions<'a> {
    pub mode: u32,
    /// Group that may read the file. `None` leaves it as root's.
    pub group: Option<&'a str>,
}

/// A health check that can only ever reach loopback, because the target comes from an untrusted
/// manifest and the helper runs as root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Tcp { port: u16 },
    Http { port: u16, path: String },
}

/// Key parameters are bytes that go to the child's stdin. They never reach an argument vector,
/// where any local user could read them from `/proc`.
pub trait System: Send + Sync {
    fn sv(&self, action: SvAction, id: &str, wait_secs: u32) -> SysResult<()>;
    /// The text `sv status` printed. A nonzero exit is not an error, because it is how `sv`
    /// reports a service that is not supervised.
    fn sv_status(&self, id: &str) -> SysResult<String>;

    fn exists(&self, path: &Path) -> bool;
    /// `None` when the file does not exist.
    fn read_file(&self, path: &Path) -> SysResult<Option<Vec<u8>>>;
    /// File names in a directory, or nothing when it does not exist.
    fn list_dir(&self, path: &Path) -> SysResult<Vec<String>>;
    /// Replaces a file so that readers and a power cut see the old or the new bytes, never a mix.
    fn atomic_write(&self, path: &Path, bytes: &[u8], options: WriteOptions) -> SysResult<()>;
    fn remove_file(&self, path: &Path) -> SysResult<()>;
    fn ensure_dir(&self, path: &Path, mode: u32, owner: Option<&str>) -> SysResult<()>;
    fn set_owner(&self, path: &Path, user: &str, mode: u32) -> SysResult<()>;

    fn nft_check(&self, path: &Path) -> SysResult<()>;
    fn nft_load(&self, path: &Path) -> SysResult<()>;

    /// Creates a new file of `size` bytes, failing if it exists.
    fn allocate_file(&self, path: &Path, size: u64) -> SysResult<()>;
    fn luks_format(&self, file: &Path, key: &[u8]) -> SysResult<()>;
    fn luks_open(&self, file: &Path, name: &str, key: &[u8]) -> SysResult<()>;
    fn luks_close(&self, name: &str) -> SysResult<()>;
    fn mapper_active(&self, name: &str) -> bool;
    fn make_filesystem(&self, device: &Path) -> SysResult<()>;
    fn mount(&self, device: &Path, target: &Path) -> SysResult<()>;
    fn is_mounted(&self, path: &Path) -> bool;

    /// Creates `link` pointing at `target`. Fails if anything already exists at `link`.
    fn symlink(&self, target: &Path, link: &Path) -> SysResult<()>;
    /// Where a symlink points, or `None` when `path` is missing or not a symlink.
    fn read_link(&self, path: &Path) -> Option<std::path::PathBuf>;
    fn user_exists(&self, name: &str) -> bool;
    /// A locked system account with its own group and no login shell.
    fn create_user(&self, name: &str, home: &Path) -> SysResult<()>;
    /// Total and free bytes of the filesystem holding `path`.
    fn fs_usage(&self, path: &Path) -> SysResult<(u64, u64)>;
    /// The filesystem type on a block device, if one is recognised.
    fn fs_type(&self, device: &Path) -> Option<String>;
    /// Checks a filesystem, repairing only when asked. Returns whether it was clean, and the
    /// tool's output.
    fn fs_check(&self, device: &Path, fs_type: &str, repair: bool) -> SysResult<(bool, String)>;
    /// Mounts what `/etc/fstab` lists for this mount point.
    fn mount_fstab(&self, mount_point: &Path) -> SysResult<()>;
    /// Runs package settings code as an unprivileged user and returns its answer. Lua never runs
    /// inside the helper itself.
    fn settings_worker(&self, input: &[u8]) -> SysResult<Vec<u8>>;

    fn probe(&self, probe: &Probe) -> bool;
    fn sleep(&self, duration: Duration);
    fn random(&self, out: &mut [u8]) -> SysResult<()>;

    /// Renames within one filesystem, replacing nothing: fails if `to` exists.
    fn rename(&self, from: &Path, to: &Path) -> SysResult<()>;
    /// The body of a successful `GET` to a loopback port, at most `limit` bytes. Only ever
    /// loopback, because the port can come from an untrusted package.
    fn http_get_local(&self, port: u16, path: &str, limit: usize) -> SysResult<Vec<u8>>;
}
