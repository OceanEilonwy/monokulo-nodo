//! The device side of system updates: find the channel's newest signed snapshot, check it, and
//! move the system to it with xbps.
//!
//! The update server is a directory of static files, laid out by `scripts/publish-snapshot.sh`:
//!
//! ```text
//! channels/<channel>/manifest.toml      the newest signed manifest of the channel
//! channels/<channel>/manifest.toml.sig
//! channels/<channel>/keys.toml(.sig)    an optional key rotation statement
//! snapshots/<name>/...                  each repository the manifest names, by its path
//! ```
//!
//! Steps, each of which must pass before the next begins:
//!
//! 1. Apply a key rotation statement if the channel has one, signed by enough current keys.
//! 2. Download the manifest and its signatures, and check them with [`TrustState::check`]:
//!    trusted keys, this channel and architecture, not expired when the clock is known, and a
//!    serial higher than the last one applied. An older, unsigned, tampered or foreign
//!    manifest stops here.
//! 3. Download every repository index the manifest names for this architecture and require
//!    the exact SHA-256 it pins. The indexes name the keys their packages are signed with, so
//!    those keys are authenticated by the manifest's signature too.
//! 4. Hand the verified indexes to xbps as its cached copy of each repository, install any of
//!    their signing keys it does not have yet, and run `xbps-install -u` against only those
//!    repositories, without syncing. xbps downloads each package and checks it against the
//!    verified index and its `.sig2` before anything is unpacked.
//!    A snapshot whose `nodo` repository offers an older version of an installed package than
//!    the device has is refused before xbps runs, unless its manifest withdraws the snapshot
//!    this device is recorded on. Then exactly those packages are moved back with
//!    `xbps-install -f`, from the same verified indexes. The serial still has to be higher than
//!    the record and the image's floor, so a withdrawal is a newer statement by the publisher,
//!    never a replay. Void's packages are never moved back: `-u` leaves a newer one as it is,
//!    as it always did, because an older libc or library under newer programs is not a state
//!    anyone tested.
//! 5. Require every pinned package that is installed to be at the pinned version.
//! 6. Restart the running services of the packages that changed, unless the snapshot needs a
//!    restart of the whole device, and only then record the snapshot as applied, so a failure
//!    anywhere above can be retried and a refused snapshot changes nothing.

use std::collections::BTreeMap;
use std::io;

use nodo_feed::{compare_versions, parse_repodata};
use thiserror::Error;

use crate::manifest::{
    verify_repodata, KeyStatement, ManifestError, PackagePin, SnapshotManifest, MAX_DOCUMENT_LEN, NODO_REPO,
};
use crate::trust::{Accepted, TrustError, TrustState};

/// The largest repository index accepted, the same bound as for feeds.
pub const MAX_REPODATA_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("could not {action}: {source}")]
    Io { action: String, source: io::Error },
    #[error("the snapshot manifest is not signed")]
    Unsigned,
    #[error("the snapshot was refused: {0}")]
    Refused(#[from] TrustError),
    #[error("the {name} repository for {arch} is refused: {detail}")]
    BadRepository { name: String, arch: String, detail: String },
    #[error("after updating, {package} is {found}, but the snapshot says {expected}")]
    WrongVersion { package: String, expected: String, found: String },
    #[error("xbps failed: {0}")]
    Xbps(String),
    /// The snapshot would move a package back, and does not withdraw the one this device is on.
    #[error(
        "{package} is {installed}, but the snapshot says {offered}, and it does not withdraw the snapshot this device is on"
    )]
    Downgrade { package: String, installed: String, offered: String },
    /// The channel moved on since the user saw what an update would do.
    #[error("the channel now offers serial {offered}, not {expected}. Check again.")]
    UnexpectedSerial { expected: u64, offered: u64 },
}

fn io(action: &str) -> impl FnOnce(io::Error) -> UpdateError + '_ {
    move |source| UpdateError::Io { action: action.to_owned(), source }
}

/// The update server, read by path relative to its base.
pub trait Source {
    /// `Ok(None)` when the server has no such file.
    fn get(&self, path: &str, limit: u64) -> io::Result<Option<Vec<u8>>>;
    /// The address xbps uses for a repository path.
    fn repository_url(&self, path: &str) -> String;
}

/// A repository signing key as xbps keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XbpsKey {
    /// Colon-separated, as xbps names the key file.
    pub fingerprint: String,
    pub pem: Vec<u8>,
    pub bits: usize,
    pub signed_by: String,
}

/// What the client does to the device.
pub trait Machine {
    fn arch(&self) -> io::Result<String>;
    /// Whether the clock is known to be right, so expiry can be checked.
    fn clock_synced(&self) -> bool;
    fn now(&self) -> u64;
    fn has_key(&self, fingerprint: &str) -> bool;
    fn install_key(&self, key: &XbpsKey) -> io::Result<()>;
    /// Writes a verified index where xbps keeps its copy of the repository at `url`.
    fn place_repodata(&self, url: &str, arch: &str, bytes: &[u8]) -> io::Result<()>;
    /// Installed packages, name to `version_revision`.
    fn installed(&self) -> io::Result<BTreeMap<String, String>>;
    /// Updates every installed package from exactly these repositories, without syncing them.
    fn update(&self, urls: &[String]) -> Result<(), String>;
    /// Installs these packages again from exactly these repositories, even where that moves
    /// them back to an older version (`xbps-install -f`). Only for a withdrawn snapshot.
    fn downgrade(&self, urls: &[String], packages: &[String]) -> Result<(), String>;
    /// Restarts the enabled, running services that these packages ship. Returns their names.
    fn restart_services(&self, packages: &[String]) -> Vec<String>;
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    UpToDate { serial: u64 },
    Applied {
        serial: u64,
        snapshot: String,
        /// `name: old -> new`.
        changed: Vec<String>,
        restarted: Vec<String>,
        needs_restart: bool,
        /// The withdrawn snapshot this device left, when it moved packages back.
        withdrew: Option<String>,
    },
}

/// What `nodo-sysupdate check --json` prints, for the helper.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckReport {
    /// The last applied snapshot's serial, or the image's floor.
    pub current_serial: u64,
    pub current_snapshot: Option<String>,
    /// The channel's newest snapshot, when it is newer than the current one.
    pub available: Option<AvailableSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AvailableSnapshot {
    pub serial: u64,
    pub snapshot: String,
    pub needs_restart: bool,
    /// It withdraws the snapshot this device is on, so packages may move back.
    pub withdraws_current: bool,
}

/// What `nodo-sysupdate apply --json` prints, for the helper.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApplyReport {
    pub applied: bool,
    pub serial: u64,
    pub snapshot: Option<String>,
    pub changed: Vec<String>,
    pub restarted: Vec<String>,
    /// Running services that changed but were left running, as asked (`--keep-running`).
    pub deferred: Vec<String>,
    pub needs_restart: bool,
    pub withdrew: Option<String>,
}

/// Either report, or why nothing happened, as one line of JSON.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Report {
    Check(CheckReport),
    Apply(ApplyReport),
    Error(String),
}

/// One verified repository, ready for xbps.
struct Verified {
    name: String,
    url: String,
    bytes: Vec<u8>,
    key: XbpsKey,
    /// Package name to `version_revision`, as the verified index offers it.
    offers: BTreeMap<String, String>,
}

fn colon_hex(hex: &str) -> String {
    hex.as_bytes().chunks(2).map(|p| String::from_utf8_lossy(p).into_owned()).collect::<Vec<_>>().join(":")
}

pub fn manifest_path(channel: &str) -> String {
    format!("channels/{channel}/manifest.toml")
}

/// Fetches and applies a key rotation statement, if the channel publishes one. One that was
/// already applied is not an error.
fn rotate_keys(source: &dyn Source, state: &mut TrustState) -> Result<bool, UpdateError> {
    let path = format!("channels/{}/keys.toml", state.channel);
    let limit = MAX_DOCUMENT_LEN as u64;
    let Some(body) = source.get(&path, limit).map_err(io("download the key statement"))? else {
        return Ok(false);
    };
    // One already applied is signed by keys this device may no longer trust, so it is skipped
    // by its sequence before any signature is checked. Nothing else of it is used.
    if KeyStatement::parse(&body).is_ok_and(|s| s.sequence <= state.key_sequence) {
        return Ok(false);
    }
    let sig = source
        .get(&format!("{path}.sig"), limit)
        .map_err(io("download the key statement's signatures"))?
        .ok_or(UpdateError::Unsigned)?;
    match state.apply_key_statement(&body, &sig) {
        Ok(()) => Ok(true),
        Err(TrustError::StaleKeyStatement { .. }) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Downloads the channel's manifest and checks it. Nothing is changed.
pub fn check(
    source: &dyn Source,
    machine: &dyn Machine,
    state: &TrustState,
) -> Result<(Accepted, Vec<u8>, Vec<u8>), UpdateError> {
    let arch = machine.arch().map_err(io("read the device architecture"))?;
    let path = manifest_path(&state.channel);
    let limit = MAX_DOCUMENT_LEN as u64;
    let body = source
        .get(&path, limit)
        .map_err(io("download the snapshot manifest"))?
        .ok_or_else(|| UpdateError::Io {
            action: "download the snapshot manifest".into(),
            source: io::Error::new(io::ErrorKind::NotFound, format!("the server has no {path}")),
        })?;
    let sig = source
        .get(&format!("{path}.sig"), limit)
        .map_err(io("download the snapshot manifest's signatures"))?
        .ok_or(UpdateError::Unsigned)?;
    let now = machine.clock_synced().then(|| machine.now());
    let accepted = match state.check(&body, &sig, &arch, now) {
        Err(TrustError::Manifest(ManifestError::BadSignatureFile(_))) => return Err(UpdateError::Unsigned),
        other => other?,
    };
    Ok((accepted, body, sig))
}

fn verify_repositories(
    source: &dyn Source,
    manifest: &SnapshotManifest,
    arch: &str,
) -> Result<Vec<Verified>, UpdateError> {
    let mut verified = Vec::new();
    for repo in manifest.repos_for(arch) {
        let bad = |detail: String| UpdateError::BadRepository { name: repo.name.clone(), arch: arch.to_owned(), detail };
        let file = format!("{}/{arch}-repodata", repo.path);
        let bytes = source
            .get(&file, MAX_REPODATA_BYTES)
            .map_err(io("download a repository index"))?
            .ok_or_else(|| bad("the server does not have its index".into()))?;
        verify_repodata(repo, &bytes).map_err(|e| match e {
            ManifestError::Invalid(text) => bad(text),
            other => bad(other.to_string()),
        })?;
        let index = parse_repodata(&bytes).map_err(|e| bad(e.to_string()))?;
        let key = index.signing_key().map_err(|e| bad(e.to_string()))?;
        let offers = index.packages.values().map(|p| (p.name.clone(), p.version.clone())).collect();
        verified.push(Verified {
            name: repo.name.clone(),
            offers,
            // Packages come from the mirror when the manifest names one; the index above did not.
            url: repo.mirror.clone().unwrap_or_else(|| source.repository_url(&repo.path)),
            key: XbpsKey {
                fingerprint: colon_hex(&key.fingerprint.to_string()),
                pem: key.pem().to_vec(),
                bits: key.bits,
                signed_by: key.signed_by.clone(),
            },
            bytes,
        });
    }
    Ok(verified)
}

fn check_pins(pins: &[PackagePin], installed: &BTreeMap<String, String>) -> Result<(), UpdateError> {
    for pin in pins {
        if let Some(found) = installed.get(&pin.name) {
            if *found != pin.version {
                return Err(UpdateError::WrongVersion {
                    package: pin.name.clone(),
                    expected: pin.version.clone(),
                    found: found.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Installed packages the snapshot's own repository offers at an older version: name,
/// installed, offered.
fn going_back(repos: &[Verified], installed: &BTreeMap<String, String>) -> Vec<(String, String, String)> {
    let mut offers = BTreeMap::new();
    for repo in repos.iter().filter(|r| r.name == NODO_REPO) {
        offers.extend(repo.offers.iter());
    }
    installed
        .iter()
        .filter_map(|(name, have)| {
            let offered = offers.get(name)?;
            (compare_versions(offered, have) == Some(std::cmp::Ordering::Less))
                .then(|| (name.clone(), have.clone(), (*offered).clone()))
        })
        .collect()
}

/// Moves the system to the channel's newest snapshot. `state` is only changed, and should only
/// be saved, when this returns `Applied`.
pub fn apply(source: &dyn Source, machine: &dyn Machine, state: &mut TrustState) -> Result<Outcome, UpdateError> {
    apply_expecting(source, machine, state, None)
}

/// Like [`apply`], and when `expected` is given, only that serial: what the user saw and chose.
/// A channel that moved on in the meantime changes nothing and says so.
pub fn apply_expecting(
    source: &dyn Source,
    machine: &dyn Machine,
    state: &mut TrustState,
    expected: Option<u64>,
) -> Result<Outcome, UpdateError> {
    let mut working = state.clone();
    let rotated = rotate_keys(source, &mut working)?;
    let (accepted, body, sig) = check(source, machine, &working)?;
    let manifest = match accepted {
        Accepted::Unchanged => {
            if let Some(expected) = expected.filter(|e| *e != working.serial) {
                return Err(UpdateError::UnexpectedSerial { expected, offered: working.serial });
            }
            if rotated {
                *state = working;
            }
            return Ok(Outcome::UpToDate { serial: state.serial });
        }
        Accepted::New(manifest) => manifest,
    };
    if let Some(expected) = expected.filter(|e| *e != manifest.serial) {
        return Err(UpdateError::UnexpectedSerial { expected, offered: manifest.serial });
    }
    let arch = machine.arch().map_err(io("read the device architecture"))?;
    let repos = verify_repositories(source, &manifest, &arch)?;
    let installed = machine.installed().map_err(io("list installed packages"))?;
    let back = going_back(&repos, &installed);
    let withdrawn = manifest.withdraws_snapshot(working.snapshot.as_deref());
    if let (Some((package, have, offered)), false) = (back.first(), withdrawn) {
        return Err(UpdateError::Downgrade {
            package: package.clone(),
            installed: have.clone(),
            offered: offered.clone(),
        });
    }
    for repo in &repos {
        if !machine.has_key(&repo.key.fingerprint) {
            machine.install_key(&repo.key).map_err(io("install a repository key"))?;
        }
        machine.place_repodata(&repo.url, &arch, &repo.bytes).map_err(io("hand the verified index to xbps"))?;
    }
    let before = installed;
    let urls: Vec<String> = repos.iter().map(|r| r.url.clone()).collect();
    machine.update(&urls).map_err(UpdateError::Xbps)?;
    if !back.is_empty() {
        let names: Vec<String> = back.iter().map(|(name, _, _)| name.clone()).collect();
        machine.downgrade(&urls, &names).map_err(UpdateError::Xbps)?;
    }
    let after = machine.installed().map_err(io("list installed packages"))?;
    check_pins(&manifest.packages, &after)?;
    let mut changed = Vec::new();
    let mut changed_names = Vec::new();
    for (name, version) in &after {
        match before.get(name) {
            Some(old) if old == version => {}
            Some(old) => {
                changed.push(format!("{name}: {old} -> {version}"));
                changed_names.push(name.clone());
            }
            None => {
                changed.push(format!("{name}: new, {version}"));
                changed_names.push(name.clone());
            }
        }
    }
    let restarted = if manifest.needs_restart { Vec::new() } else { machine.restart_services(&changed_names) };
    let left = working.snapshot.clone().filter(|_| !back.is_empty());
    // The manifest was checked against `working`, so it is accepted there.
    working.accept(&body, &sig, &arch, machine.clock_synced().then(|| machine.now()))?;
    *state = working;
    Ok(Outcome::Applied {
        serial: manifest.serial,
        snapshot: manifest.snapshot.clone(),
        changed,
        restarted,
        needs_restart: manifest.needs_restart,
        withdrew: left,
    })
}

/// The directory name xbps gives its copy of a remote repository: the URL with `.`, `:` and
/// `/` replaced by `_`, as `xbps_get_remote_repo_string` does.
pub fn xbps_repo_dir(url: &str) -> String {
    url.chars().map(|c| if matches!(c, '.' | ':' | '/') { '_' } else { c }).collect()
}

/// The key xbps would record for a repository signed with this RSA public key (PEM), for seeding
/// `/var/db/xbps/keys` from a pinned key instead of accepting one the first time it is seen.
pub fn xbps_key_from_pem(pem: &[u8], signed_by: &str) -> Result<XbpsKey, String> {
    let key = nodo_feed::RepoKey::from_pem(pem, signed_by).map_err(|e| e.to_string())?;
    Ok(XbpsKey {
        fingerprint: colon_hex(&key.fingerprint.to_string()),
        pem: key.pem().to_vec(),
        bits: key.bits,
        signed_by: key.signed_by.clone(),
    })
}

/// A key file as xbps writes it into `/var/db/xbps/keys`.
pub fn xbps_key_plist(key: &XbpsKey) -> String {
    use base64::Engine as _;
    let escaped: String = key
        .signed_by
        .chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            other => other.to_string(),
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple Computer//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>public-key</key>\n\
         \t<data>{}</data>\n\
         \t<key>public-key-size</key>\n\
         \t<integer>{}</integer>\n\
         \t<key>signature-by</key>\n\
         \t<string>{escaped}</string>\n\
         </dict>\n\
         </plist>\n",
        base64::engine::general_purpose::STANDARD.encode(&key.pem),
        key.bits,
    )
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
