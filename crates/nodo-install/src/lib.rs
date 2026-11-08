//! Installs a package from a user-added feed.
//!
//! The steps, each of which must pass before the next begins:
//!
//! 1. Read the feed's index and require its signing key to match the key pinned for the feed.
//! 2. Find the package, check it suits this device's architecture, and ask the pins whether its
//!    version is acceptable (no key change, no downgrade without approval).
//! 3. Download it once into memory. Its size and SHA-256 must match the index, and its `.sig2`
//!    must verify with the pinned key.
//! 4. Inspect those same bytes with [`nodo_feed::inspect_package`] and compare its metadata with
//!    the index.
//! 5. Summarise what it may do. The first install, and any update that asks for more, stops here
//!    until the user approves exactly that summary.
//! 6. Write the bytes to a new file in a private directory, index that directory with
//!    `xbps-rindex`, and require the new index to carry the hash computed in step 3. xbps checks
//!    the file against that index when it installs, so what xbps installs is what was inspected.
//! 7. Run `xbps-install` with a private configuration directory and only that repository. The
//!    repository is local and unsigned, so xbps has no key to import and the system keys and
//!    repository configuration are never touched.
//! 8. Confirm the installed version, and only then raise the feed's version floor.

mod permissions;
mod system;

use std::cmp::Ordering;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use nodo_feed::{
    compare_versions, inspect_package, parse_repodata, BlockReason, Decision, PinStore, Policy, RepoEntry, RepoError,
    SignatureError, Violation,
};
use thiserror::Error;

pub use permissions::{Permission, Permissions};
pub use system::{Output, RealSystem, System};

/// An index lists every package in a feed. This is generous for that and still bounded.
const MAX_REPODATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;
/// A `.sig2` is one RSA signature, at most 16384 bits.
const MAX_SIGNATURE_BYTES: u64 = 2048;
const MAX_ERROR_TEXT: usize = 2000;

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("could not {action}: {source}")]
    Io { action: String, source: std::io::Error },
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error(transparent)]
    Signature(#[from] SignatureError),
    #[error("the feed has no package named '{0}'")]
    NotInFeed(String),
    #[error("'{package}' is built for {found}, and this device is {device}")]
    WrongArchitecture { package: String, found: String, device: String },
    #[error("the package does not match the feed's index: {0}")]
    IndexMismatch(String),
    #[error("the package breaks {} rule(s): {}", .0.len(), join(.0))]
    Rejected(Vec<Violation>),
    #[error("{command} failed: {detail}")]
    Command { command: String, detail: String },
    #[error("after installing, xbps reports {found:?} instead of {expected}")]
    NotInstalled { expected: String, found: Option<String> },
    #[error("the version floor could not be raised: {0}")]
    Pins(#[from] nodo_feed::ApprovalError),
}

fn join(violations: &[Violation]) -> String {
    violations.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

fn io(action: &str) -> impl FnOnce(std::io::Error) -> InstallError + '_ {
    move |source| InstallError::Io { action: action.to_string(), source }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The pins refuse this version or key. Some reasons carry an approval the user can give.
    Blocked(Vec<BlockReason>),
    /// Nothing was installed. The user must approve `permissions` first; `added` is what goes
    /// beyond the previous approval, or everything on a first install.
    NeedsApproval { pkgver: String, permissions: Permissions, added: Vec<Permission> },
    Installed { pkgver: String, permissions: Permissions },
}

pub struct Request<'a> {
    /// The feed's URL or directory, exactly as pinned.
    pub feed: &'a str,
    pub package: &'a str,
    /// What the user approved for this package before, if anything.
    pub approved: Option<&'a Permissions>,
}

pub struct Installer<'a, S: System> {
    pub sys: &'a S,
    pub pins: &'a mut PinStore,
    pub policy: Policy,
}

/// Removes the staging directory on every path out of the install.
struct Stage<'a, S: System> {
    sys: &'a S,
    path: PathBuf,
}

impl<S: System> Drop for Stage<'_, S> {
    fn drop(&mut self) {
        let _ = self.sys.remove_dir_all(&self.path);
    }
}

fn shown(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().filter(|c| !c.is_control() || *c == '\n').take(MAX_ERROR_TEXT).collect()
}

impl<S: System> Installer<'_, S> {
    fn run(&self, program: &str, args: &[OsString], env: &[(&str, &str)]) -> Result<Output, InstallError> {
        let out = self.sys.run(program, args, env).map_err(io(program))?;
        if !out.success {
            return Err(InstallError::Command { command: program.to_string(), detail: shown(&out.stderr).trim().to_string() });
        }
        Ok(out)
    }

    /// The installed `name-version_revision`, if the package is installed.
    fn installed_pkgver(&self, name: &str) -> Result<Option<String>, InstallError> {
        let out = self.sys.run("xbps-query", &["-p".into(), "pkgver".into(), name.into()], &[]).map_err(io("xbps-query"))?;
        let text = shown(&out.stdout).trim().to_string();
        Ok((out.success && !text.is_empty()).then_some(text))
    }

    pub fn install(&mut self, request: &Request) -> Result<Outcome, InstallError> {
        let arch = self.sys.device_arch().map_err(io("read the device architecture"))?;
        let index = self
            .sys
            .fetch(request.feed, &format!("{arch}-repodata"), MAX_REPODATA_BYTES)
            .map_err(io("read the feed's index"))?;
        let repo = parse_repodata(&index)?;
        let key = repo.signing_key()?;
        let entry = repo.packages.get(request.package).ok_or_else(|| InstallError::NotInFeed(request.package.to_string()))?;
        if entry.architecture != arch && entry.architecture != "noarch" {
            return Err(InstallError::WrongArchitecture {
                package: entry.name.clone(),
                found: entry.architecture.clone(),
                device: arch,
            });
        }
        if let Decision::Blocked(reasons) = self.pins.check_package(request.feed, &key.fingerprint, &entry.name, &entry.version) {
            return Ok(Outcome::Blocked(reasons));
        }

        let file_name = entry.file_name();
        let limit = entry.size.min(MAX_PACKAGE_BYTES);
        let bytes = self.sys.fetch(request.feed, &file_name, limit).map_err(io("download the package"))?;
        entry.check_bytes(&bytes)?;
        let signature = self
            .sys
            .fetch(request.feed, &format!("{file_name}.sig2"), MAX_SIGNATURE_BYTES)
            .map_err(io("download the package signature"))?;
        key.verify(&bytes, &signature)?;

        let inspection = inspect_package(bytes.as_slice(), &self.policy);
        if !inspection.is_clean() {
            return Err(InstallError::Rejected(inspection.violations));
        }
        let (Some(info), Some(manifest)) = (&inspection.package, &inspection.manifest) else {
            return Err(InstallError::IndexMismatch("the package has no metadata".to_string()));
        };
        check_against_index(entry, &info.name, &info.version, &info.architecture)?;
        let pkgver = format!("{}-{}", entry.name, entry.version);

        let permissions = Permissions::of(manifest, &info.run_depends);
        let added = match request.approved {
            Some(approved) => permissions.added_since(approved),
            None => permissions.granted.iter().cloned().collect(),
        };
        if request.approved.is_none() || !added.is_empty() {
            return Ok(Outcome::NeedsApproval { pkgver, permissions, added });
        }

        self.install_bytes(&arch, entry, &file_name, &bytes)?;
        let found = self.installed_pkgver(&entry.name)?;
        if found.as_deref() != Some(pkgver.as_str()) {
            return Err(InstallError::NotInstalled { expected: pkgver, found });
        }
        self.pins.record_installed(request.feed, &entry.name, &entry.version)?;
        Ok(Outcome::Installed { pkgver, permissions })
    }

    fn install_bytes(&self, arch: &str, entry: &RepoEntry, file_name: &str, bytes: &[u8]) -> Result<(), InstallError> {
        let stage = Stage { sys: self.sys, path: self.sys.private_dir().map_err(io("create a staging directory"))? };
        let repo_dir = stage.path.join("repo");
        let conf_dir = stage.path.join("xbps.d");
        self.sys.create_dir(&repo_dir).map_err(io("create the staging repository"))?;
        // Empty on purpose: no system repository, key or cache setting applies to this install.
        self.sys.create_dir(&conf_dir).map_err(io("create the private xbps configuration"))?;
        let staged = repo_dir.join(file_name);
        self.sys.write_new(&staged, bytes).map_err(io("stage the package"))?;

        self.run("xbps-rindex", &["-a".into(), staged.clone().into()], &[("XBPS_TARGET_ARCH", arch)])?;
        let local_index = self
            .sys
            .read(&repo_dir.join(format!("{arch}-repodata")), MAX_REPODATA_BYTES)
            .map_err(io("read the regenerated index"))?;
        let local = parse_repodata(&local_index)?;
        let regenerated = local
            .packages
            .get(&entry.name)
            .ok_or_else(|| InstallError::IndexMismatch("xbps-rindex did not index the package".to_string()))?;
        if regenerated.sha256 != entry.sha256 || regenerated.version != entry.version {
            return Err(InstallError::IndexMismatch("the staged file is not the file that was inspected".to_string()));
        }

        let mut args: Vec<OsString> = vec![
            "-C".into(),
            conf_dir.into(),
            "--ignore-conf-repos".into(),
            "--repository".into(),
            repo_dir.into(),
            "--yes".into(),
        ];
        match self.installed_pkgver(&entry.name)? {
            None => {}
            Some(installed) => {
                let current = installed.strip_prefix(&format!("{}-", entry.name)).unwrap_or(&installed);
                // `-u` only moves forward. Reinstalling the same version, or an approved
                // downgrade, needs `-f`.
                match compare_versions(&entry.version, current) {
                    Some(Ordering::Greater) => args.push("--update".into()),
                    _ => args.push("--force".into()),
                }
            }
        }
        args.push(entry.name.clone().into());
        self.run("xbps-install", &args, &[])?;
        Ok(())
    }
}

fn check_against_index(entry: &RepoEntry, name: &str, version: &str, arch: &str) -> Result<(), InstallError> {
    let mismatch = |what: &str, inside: &str, index: &str| {
        InstallError::IndexMismatch(format!("its {what} is {inside}, the index says {index}"))
    };
    if name != entry.name {
        return Err(mismatch("name", name, &entry.name));
    }
    if version != entry.version {
        return Err(mismatch("version", version, &entry.version));
    }
    if arch != entry.architecture {
        return Err(mismatch("architecture", arch, &entry.architecture));
    }
    Ok(())
}

/// Reads a package file and inspects it, for authors checking a build before they publish it.
pub fn inspect_file(path: &Path, policy: &Policy) -> std::io::Result<(nodo_feed::Inspection, Option<Permissions>)> {
    let file = std::fs::File::open(path)?;
    let inspection = inspect_package(std::io::BufReader::new(file), policy);
    let permissions = match (&inspection.package, &inspection.manifest) {
        (Some(info), Some(manifest)) => Some(Permissions::of(manifest, &info.run_depends)),
        _ => None,
    };
    Ok((inspection, permissions))
}

#[cfg(test)]
mod tests;
