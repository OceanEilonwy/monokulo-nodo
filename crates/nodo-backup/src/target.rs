//! Restores into a new staging directory. The helper then moves each category into place while
//! the affected services are stopped, which is outside this crate.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

use zeroize::Zeroizing;

use crate::{BackupError, RestoreTarget};

pub struct DirTarget {
    root: PathBuf,
    open: Option<(File, u64, u64)>,
    pub device_key: Option<Zeroizing<Vec<u8>>>,
}

impl DirTarget {
    /// `root` must not exist yet, so a restore never mixes with older files or follows a link
    /// someone planted there.
    pub fn create(root: impl Into<PathBuf>) -> Result<Self, BackupError> {
        let root = root.into();
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        Ok(DirTarget { root, open: None, device_key: None })
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    fn path(&self, archive: &str) -> PathBuf {
        // Paths were validated by the reader: relative, plain components, no "..".
        self.root.join(archive)
    }

    fn parent_dirs(&self, archive: &str) -> Result<(), BackupError> {
        if let Some(parent) = self.path(archive).parent() {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
        }
        Ok(())
    }
}

impl RestoreTarget for DirTarget {
    fn dir(&mut self, path: &str, mode: u32) -> Result<(), BackupError> {
        self.parent_dirs(path)?;
        let p = self.path(path);
        match fs::DirBuilder::new().mode(mode | 0o700).create(&p) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && fs::symlink_metadata(&p)?.is_dir() => Ok(()),
            other => Ok(other?),
        }
    }

    fn file_begin(&mut self, path: &str, mode: u32, size: u64) -> Result<(), BackupError> {
        self.parent_dirs(path)?;
        // create_new: two entries with one path, or a planted link, fail instead of overwriting.
        let file = OpenOptions::new().write(true).create_new(true).mode(mode | 0o600).open(self.path(path))?;
        self.open = Some((file, size, 0));
        Ok(())
    }

    fn file_data(&mut self, data: &[u8]) -> Result<(), BackupError> {
        let (file, size, written) = self.open.as_mut().ok_or_else(|| BackupError::Io("no open file".into()))?;
        *written += data.len() as u64;
        if *written > *size {
            return Err(BackupError::Corrupt("file longer than declared".into()));
        }
        file.write_all(data)?;
        Ok(())
    }

    fn file_end(&mut self) -> Result<(), BackupError> {
        let (file, size, written) = self.open.take().ok_or_else(|| BackupError::Io("no open file".into()))?;
        if written != size {
            return Err(BackupError::Corrupt("file shorter than declared".into()));
        }
        file.sync_all()?;
        Ok(())
    }

    fn device_key(&mut self, key: Zeroizing<Vec<u8>>) -> Result<(), BackupError> {
        self.device_key = Some(key);
        Ok(())
    }

    fn abort(&mut self) {
        self.open = None;
        self.device_key = None;
        let _ = fs::remove_dir_all(&self.root);
    }
}
