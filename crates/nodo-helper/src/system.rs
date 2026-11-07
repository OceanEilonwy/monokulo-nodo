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

    fn probe(&self, probe: &Probe) -> bool;
    fn sleep(&self, duration: Duration);
    fn random(&self, out: &mut [u8]) -> SysResult<()>;
}
