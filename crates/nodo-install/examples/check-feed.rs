//! Checks a published feed the way a device does before it offers anything from it.
//!
//!     cargo run -p nodo-install --example check-feed -- <feed> <arch> <fingerprint>
//!
//! `<feed>` is a directory or an `https://` address, exactly as a device would be given it. The
//! index must be signed by the key with `<fingerprint>` (the form `KeyFingerprint` accepts,
//! colons optional). The key is then pinned as a device pins a feed it ships with, and every
//! package in the index goes through the installer's own review: download, size and SHA-256
//! against the index, the `.sig2` against the pinned key, and the package gate. Nothing is
//! installed or run.
//!
//! Prints `fingerprint <hex>` and then one tab separated line per package:
//! `package <name> <version> <bytes> <permissions>`. Exits non-zero on the first failure.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use nodo_feed::{parse_repodata, Decision, KeyFingerprint, PinStore, Policy};
use nodo_install::http::Fetcher;
use nodo_install::{Installer, Output, Request, Reviewed, System};

const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;

/// A device of the given architecture that can only read feeds.
struct ReadOnly {
    arch: String,
    http: Fetcher,
}

fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "check-feed only reads")
}

impl System for ReadOnly {
    fn device_arch(&self) -> io::Result<String> {
        Ok(self.arch.clone())
    }

    fn fetch(&self, feed: &str, file: &str, limit: u64) -> io::Result<Vec<u8>> {
        if feed.starts_with("https://") || feed.starts_with("http://") {
            return self.http.get(
                &format!("{}/{file}", feed.trim_end_matches('/')),
                file,
                limit,
            );
        }
        self.read(&Path::new(feed).join(file), limit)
    }

    fn private_dir(&self) -> io::Result<PathBuf> {
        Err(refused())
    }

    fn create_dir(&self, _: &Path) -> io::Result<()> {
        Err(refused())
    }

    fn write_new(&self, _: &Path, _: &[u8]) -> io::Result<()> {
        Err(refused())
    }

    fn read(&self, path: &Path, limit: u64) -> io::Result<Vec<u8>> {
        let bytes = std::fs::read(path)?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("larger than {limit} bytes"),
            ));
        }
        Ok(bytes)
    }

    fn run(&self, _: &str, _: &[OsString], _: &[(&str, &str)]) -> io::Result<Output> {
        Err(refused())
    }

    fn remove_dir_all(&self, _: &Path) -> io::Result<()> {
        Err(refused())
    }

    fn remove_file(&self, _: &Path) -> io::Result<()> {
        Err(refused())
    }
}

fn check(feed: &str, arch: &str, expected: &str) -> Result<(), String> {
    let expected = KeyFingerprint::new(expected).map_err(|e| format!("{expected}: {e}"))?;
    let sys = ReadOnly {
        arch: arch.to_owned(),
        http: Fetcher::new(),
    };
    let index = sys
        .fetch(feed, &format!("{arch}-repodata"), MAX_INDEX_BYTES)
        .map_err(|e| format!("could not read {arch}-repodata: {e}"))?;
    let repo = parse_repodata(&index).map_err(|e| e.to_string())?;
    let key = repo.signing_key().map_err(|e| e.to_string())?;
    println!(
        "fingerprint\t{}\t{}\t{} bits",
        key.fingerprint, key.signed_by, key.bits
    );
    if key.fingerprint != expected {
        return Err(format!(
            "the index is signed by {}, not {expected}",
            key.fingerprint
        ));
    }
    let mut pins = PinStore::new();
    if pins.add_feed(feed, &expected) != Decision::Pinned {
        return Err("could not pin the key".to_owned());
    }
    let names: Vec<_> = repo
        .packages
        .values()
        .filter(|e| e.architecture == arch || e.architecture == "noarch")
        .map(|e| (e.name.clone(), e.version.clone(), e.size))
        .collect();
    if names.is_empty() {
        return Err(format!("the index lists no {arch} packages"));
    }
    for (name, version, size) in names {
        let mut installer = Installer {
            sys: &sys,
            pins: &mut pins,
            policy: Policy::default(),
        };
        let request = Request {
            feed,
            package: &name,
            approved: None,
        };
        match installer
            .review(&request)
            .map_err(|e| format!("{name}: {e}"))?
        {
            Reviewed::Blocked(reasons) => return Err(format!("{name}: blocked: {reasons:?}")),
            Reviewed::Ready {
                pkgver,
                permissions,
                ..
            } => {
                if pkgver != format!("{name}-{version}") {
                    return Err(format!(
                        "{name}: reviewed {pkgver}, the index lists {version}"
                    ));
                }
                let granted: Vec<String> = permissions
                    .granted
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                println!("package\t{name}\t{version}\t{size}\t{}", granted.join("; "));
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [feed, arch, fingerprint] = args.as_slice() else {
        eprintln!("usage: check-feed <feed directory or address> <arch> <fingerprint>");
        return ExitCode::from(2);
    };
    match check(feed, arch, fingerprint) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("check-feed: {e}");
            ExitCode::FAILURE
        }
    }
}
