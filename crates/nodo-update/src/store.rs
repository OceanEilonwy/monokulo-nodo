//! Small named documents, persisted atomically.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::Serialize;

pub trait StateStore {
    fn load(&self, name: &str) -> io::Result<Option<Vec<u8>>>;
    fn save(&self, name: &str, bytes: &[u8]) -> io::Result<()>;
}

fn bad_data(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

pub fn load_toml<T: DeserializeOwned>(store: &dyn StateStore, name: &str) -> io::Result<Option<T>> {
    let Some(bytes) = store.load(name)? else { return Ok(None) };
    let text = std::str::from_utf8(&bytes).map_err(bad_data)?;
    toml::from_str(text).map(Some).map_err(bad_data)
}

pub fn save_toml<T: Serialize>(store: &dyn StateStore, name: &str, value: &T) -> io::Result<()> {
    let text = toml::to_string(value).map_err(bad_data)?;
    store.save(name, text.as_bytes())
}

/// Files in one directory. A crash leaves either the old or the new document, never a mix.
pub struct DirStore {
    dir: PathBuf,
}

impl DirStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        DirStore { dir: dir.into() }
    }

    fn path(&self, name: &str) -> io::Result<PathBuf> {
        let plain = !name.is_empty()
            && !name.starts_with('.')
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !plain {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid state name"));
        }
        Ok(self.dir.join(name))
    }
}

impl StateStore for DirStore {
    fn load(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.path(name)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn save(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self.path(name)?;
        fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(".{name}.tmp"));
        let mut file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &path)?;
        File::open(&self.dir)?.sync_all()
    }
}

/// For tests and for callers that keep state elsewhere.
#[derive(Default)]
pub struct MemStore {
    pub docs: RefCell<BTreeMap<String, Vec<u8>>>,
}

impl StateStore for MemStore {
    fn load(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.docs.borrow().get(name).cloned())
    }

    fn save(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.docs.borrow_mut().insert(name.to_owned(), bytes.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_store_round_trips_and_refuses_odd_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = DirStore::new(dir.path().join("state"));
        assert_eq!(store.load("trust.toml").unwrap(), None);
        store.save("trust.toml", b"a = 1\n").unwrap();
        store.save("trust.toml", b"a = 2\n").unwrap();
        assert_eq!(store.load("trust.toml").unwrap().unwrap(), b"a = 2\n");
        for bad in ["../x", "a/b", ".hidden", ""] {
            assert!(store.save(bad, b"x").is_err(), "{bad}");
        }
        let leftovers: Vec<_> = fs::read_dir(dir.path().join("state")).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temporary files remain");
    }
}
