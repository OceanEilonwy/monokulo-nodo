//! The signed snapshot manifest and key rotation statements.
//!
//! Signatures cover the exact bytes of a document, prefixed with a context string, so there is no
//! canonical form to get wrong and a signature for one kind of document can never be presented as
//! another. Documents are only parsed after their signature has been checked by the caller, and
//! parsing is strict: unknown fields are refused rather than silently ignored.

use std::collections::BTreeSet;
use std::fmt;

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MANIFEST_CONTEXT: &[u8] = b"monokulo-nodo snapshot manifest v1\0";
pub const KEY_STATEMENT_CONTEXT: &[u8] = b"monokulo-nodo key statement v1\0";
pub const FORMAT: u32 = 1;
/// Real documents are a few kilobytes. The limit keeps a hostile mirror from costing memory.
pub const MAX_DOCUMENT_LEN: usize = 256 * 1024;
const MAX_SIGNATURES: usize = 16;
const MAX_KEYS: usize = 16;
const MAX_NAME_LEN: usize = 64;
const MAX_PATH_LEN: usize = 200;
/// How many earlier snapshots one manifest may withdraw.
const MAX_WITHDRAWN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ManifestError {
    #[error("document is larger than {MAX_DOCUMENT_LEN} bytes")]
    TooLarge,
    #[error("document is not valid: {0}")]
    Invalid(String),
    #[error("signature file is not valid: {0}")]
    BadSignatureFile(String),
    #[error("not signed by enough trusted keys ({valid} of {needed})")]
    NotEnoughSignatures { valid: usize, needed: usize },
    /// Signed only by keys this device does not trust, which is what a changed signing key or a
    /// forged manifest looks like. Either way it needs the user, never an automatic decision.
    #[error("signed by an unknown key")]
    UnknownKey { offered: Vec<PublicKey> },
}

fn invalid(text: impl Into<String>) -> ManifestError {
    ManifestError::Invalid(text.into())
}

/// An Ed25519 public key, written as 64 lowercase hex digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, ManifestError> {
        // Rejects encodings that are not points on the curve.
        VerifyingKey::from_bytes(&bytes).map_err(|_| invalid("not an Ed25519 public key"))?;
        Ok(PublicKey(bytes))
    }

    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let bytes = decode_hex::<32>(text).ok_or_else(|| invalid("a public key is 64 hex digits"))?;
        PublicKey::from_bytes(bytes)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn verifying(&self) -> VerifyingKey {
        VerifyingKey::from_bytes(&self.0).expect("checked on construction")
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({self})")
    }
}

impl TryFrom<String> for PublicKey {
    type Error = ManifestError;

    fn try_from(text: String) -> Result<Self, ManifestError> {
        PublicKey::parse(&text)
    }
}

impl From<PublicKey> for String {
    fn from(key: PublicKey) -> String {
        key.to_string()
    }
}

impl From<&SigningKey> for PublicKey {
    fn from(key: &SigningKey) -> Self {
        PublicKey(key.verifying_key().to_bytes())
    }
}

/// Only lowercase, so one key or hash has exactly one spelling.
fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut out = [0u8; N];
    hex::decode_to_slice(text, &mut out).ok()?;
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    pub key: PublicKey,
    /// 128 lowercase hex digits.
    pub sig: String,
}

/// The detached signatures for one document, stored next to it as `<name>.sig`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureFile {
    #[serde(default)]
    pub signature: Vec<Signature>,
}

impl SignatureFile {
    pub fn parse(text: &[u8]) -> Result<Self, ManifestError> {
        if text.len() > MAX_DOCUMENT_LEN {
            return Err(ManifestError::TooLarge);
        }
        let text = std::str::from_utf8(text).map_err(|_| ManifestError::BadSignatureFile("not UTF-8".into()))?;
        let file: SignatureFile =
            toml::from_str(text).map_err(|e| ManifestError::BadSignatureFile(e.message().to_owned()))?;
        if file.signature.is_empty() || file.signature.len() > MAX_SIGNATURES {
            return Err(ManifestError::BadSignatureFile("expected 1 to 16 signatures".into()));
        }
        Ok(file)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("signature files always serialize")
    }

    /// Adds a signature over `context || body`.
    pub fn sign(&mut self, context: &[u8], body: &[u8], key: &SigningKey) {
        let sig = key.sign(&signed_message(context, body));
        self.signature.push(Signature { key: PublicKey::from(key), sig: hex::encode(sig.to_bytes()) });
    }

    /// Returns the distinct trusted keys with a valid signature, failing unless there are at least
    /// `threshold` of them. Signatures by untrusted keys are never checked, since anyone can make one.
    pub fn verify(
        &self,
        context: &[u8],
        body: &[u8],
        trusted: &BTreeSet<PublicKey>,
        threshold: usize,
    ) -> Result<BTreeSet<PublicKey>, ManifestError> {
        if body.len() > MAX_DOCUMENT_LEN {
            return Err(ManifestError::TooLarge);
        }
        let message = signed_message(context, body);
        let mut valid = BTreeSet::new();
        let mut untrusted = Vec::new();
        for entry in &self.signature {
            if !trusted.contains(&entry.key) {
                untrusted.push(entry.key);
                continue;
            }
            let Some(bytes) = decode_hex::<64>(&entry.sig) else { continue };
            let sig = ed25519_dalek::Signature::from_bytes(&bytes);
            // Strict verification refuses malleable and small-order encodings.
            if entry.key.verifying().verify_strict(&message, &sig).is_ok() {
                valid.insert(entry.key);
            }
        }
        if valid.len() >= threshold.max(1) {
            return Ok(valid);
        }
        if valid.is_empty() && !untrusted.is_empty() && self.signature.iter().all(|s| !trusted.contains(&s.key)) {
            untrusted.sort();
            untrusted.dedup();
            return Err(ManifestError::UnknownKey { offered: untrusted });
        }
        Err(ManifestError::NotEnoughSignatures { valid: valid.len(), needed: threshold.max(1) })
    }
}

fn signed_message(context: &[u8], body: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(context.len() + body.len());
    message.extend_from_slice(context);
    message.extend_from_slice(body);
    message
}

/// One repository of one architecture inside a snapshot. An architecture has Void's copy,
/// named `void`, and usually this project's own packages, named `nodo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoEntry {
    pub arch: String,
    /// Which repository of this architecture. Manifests from before names default to `void`.
    #[serde(default = "void_repo")]
    pub name: String,
    /// Relative to the update server's base URL, for example `snapshots/2026.12/aarch64`.
    pub path: String,
    /// SHA-256 of `<arch>-repodata` exactly as tested. xbps verifies packages against it.
    pub repodata_sha256: String,
}

pub const VOID_REPO: &str = "void";
pub const NODO_REPO: &str = "nodo";

fn void_repo() -> String {
    VOID_REPO.to_owned()
}

/// A package version shown to the user, for example the base system's version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagePin {
    pub name: String,
    pub version: String,
}

/// Which snapshot devices on a channel may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifest {
    pub format: u32,
    pub channel: String,
    /// Increases with every manifest the publisher signs. Devices never accept a lower or equal
    /// one, which stops an old, vulnerable snapshot from being replayed to them.
    pub serial: u64,
    pub snapshot: String,
    /// Unix seconds.
    pub published: u64,
    /// Unix seconds. After this a device with a synchronised clock stops trusting the manifest, so
    /// a mirror cannot hide newer snapshots by serving a stale one forever.
    pub expires: u64,
    /// Moving to this snapshot replaces the kernel, libc or init, so it needs a system restart and
    /// is never installed automatically.
    pub needs_restart: bool,
    pub repo: Vec<RepoEntry>,
    #[serde(default)]
    pub packages: Vec<PackagePin>,
    /// Snapshots the publisher pulled, by name. A device recorded on one of them may move
    /// installed packages back to the versions this snapshot offers; any other device refuses a
    /// snapshot that would move a package back. Left out of manifests that withdraw nothing, so
    /// their bytes are what they always were.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub withdraws: Vec<String>,
}

fn name_ok(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_NAME_LEN
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !text.starts_with(['.', '-'])
}

/// A relative path that cannot leave the server's base directory or carry URL syntax.
fn repo_path_ok(path: &str) -> bool {
    path.len() <= MAX_PATH_LEN && !path.is_empty() && path.split('/').all(name_ok)
}

impl SnapshotManifest {
    pub fn parse(body: &[u8]) -> Result<Self, ManifestError> {
        if body.len() > MAX_DOCUMENT_LEN {
            return Err(ManifestError::TooLarge);
        }
        let text = std::str::from_utf8(body).map_err(|_| invalid("not UTF-8"))?;
        let manifest: SnapshotManifest = toml::from_str(text).map_err(|e| invalid(e.message()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.format != FORMAT {
            return Err(invalid(format!("unsupported format {}", self.format)));
        }
        if !name_ok(&self.channel) || !name_ok(&self.snapshot) {
            return Err(invalid("channel and snapshot names must be short and plain"));
        }
        if self.serial == 0 {
            return Err(invalid("serial must be positive"));
        }
        if self.expires <= self.published {
            return Err(invalid("expires must be after published"));
        }
        if self.repo.is_empty() {
            return Err(invalid("no repositories"));
        }
        let mut repos = BTreeSet::new();
        for repo in &self.repo {
            if !name_ok(&repo.arch) || !name_ok(&repo.name) || !repos.insert((repo.arch.as_str(), repo.name.as_str())) {
                return Err(invalid("each repository must have a plain architecture and name, listed once"));
            }
            if !repo_path_ok(&repo.path) {
                return Err(invalid(format!("unsafe repository path {:?}", repo.path)));
            }
            if decode_hex::<32>(&repo.repodata_sha256).is_none() {
                return Err(invalid("repodata_sha256 must be 64 lowercase hex digits"));
            }
        }
        for package in &self.packages {
            if !name_ok(&package.name) || nodo_feed::compare_versions(&package.version, &package.version).is_none() {
                return Err(invalid(format!("bad package pin {:?}", package.name)));
            }
        }
        let withdrawn: BTreeSet<&str> = self.withdraws.iter().map(String::as_str).collect();
        if self.withdraws.len() > MAX_WITHDRAWN
            || withdrawn.len() != self.withdraws.len()
            || !self.withdraws.iter().all(|name| name_ok(name))
            || withdrawn.contains(self.snapshot.as_str())
        {
            return Err(invalid("withdraws lists other snapshots by plain name, each once"));
        }
        Ok(())
    }

    /// Whether this manifest withdraws the snapshot a device is on.
    pub fn withdraws_snapshot(&self, current: Option<&str>) -> bool {
        current.is_some_and(|name| self.withdraws.iter().any(|w| w == name))
    }

    /// Every repository of one architecture, Void's first.
    pub fn repos_for(&self, arch: &str) -> Vec<&RepoEntry> {
        let mut repos: Vec<&RepoEntry> = self.repo.iter().filter(|r| r.arch == arch).collect();
        repos.sort_by_key(|r| r.name != VOID_REPO);
        repos
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("manifests always serialize")
    }
}

/// Checks a downloaded repository index against the manifest, so a mirror cannot substitute a
/// newer, untested Void index that is still validly signed by Void.
pub fn verify_repodata(entry: &RepoEntry, repodata: &[u8]) -> Result<(), ManifestError> {
    let digest = hex::encode(Sha256::digest(repodata));
    if digest == entry.repodata_sha256 {
        Ok(())
    } else {
        Err(invalid(format!("repodata for {} does not match the signed manifest", entry.arch)))
    }
}

/// Replaces the set of trusted keys. Signed by enough of the keys it replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyStatement {
    pub format: u32,
    /// Increases with each statement, so an old statement cannot restore a retired key.
    pub sequence: u64,
    pub keys: Vec<PublicKey>,
    pub threshold: u32,
}

impl KeyStatement {
    pub fn parse(body: &[u8]) -> Result<Self, ManifestError> {
        if body.len() > MAX_DOCUMENT_LEN {
            return Err(ManifestError::TooLarge);
        }
        let text = std::str::from_utf8(body).map_err(|_| invalid("not UTF-8"))?;
        let statement: KeyStatement = toml::from_str(text).map_err(|e| invalid(e.message()))?;
        statement.validate()?;
        Ok(statement)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.format != FORMAT {
            return Err(invalid(format!("unsupported format {}", self.format)));
        }
        let distinct: BTreeSet<_> = self.keys.iter().collect();
        if self.keys.is_empty() || self.keys.len() > MAX_KEYS || distinct.len() != self.keys.len() {
            return Err(invalid("a key statement lists 1 to 16 distinct keys"));
        }
        if self.threshold == 0 || self.threshold as usize > self.keys.len() {
            return Err(invalid("threshold must be between 1 and the number of keys"));
        }
        if self.sequence == 0 {
            return Err(invalid("sequence must be positive"));
        }
        Ok(())
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("key statements always serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn sample() -> SnapshotManifest {
        SnapshotManifest {
            format: 1,
            channel: "stable".into(),
            serial: 7,
            snapshot: "2026.12".into(),
            published: 1000,
            expires: 2000,
            needs_restart: false,
            repo: vec![RepoEntry {
                arch: "aarch64".into(),
                name: VOID_REPO.into(),
                path: "snapshots/2026.12/aarch64".into(),
                repodata_sha256: hex::encode(Sha256::digest(b"index")),
            }],
            packages: vec![PackagePin { name: "nodo-base".into(), version: "0.1.0_2".into() }],
            withdraws: vec![],
        }
    }

    fn trusted(keys: &[&SigningKey]) -> BTreeSet<PublicKey> {
        keys.iter().map(|k| PublicKey::from(*k)).collect()
    }

    #[test]
    fn round_trips_and_verifies() {
        let body = sample().to_toml();
        let mut sigs = SignatureFile::default();
        sigs.sign(MANIFEST_CONTEXT, body.as_bytes(), &key(1));
        let sigs = SignatureFile::parse(sigs.to_toml().as_bytes()).unwrap();
        let valid = sigs.verify(MANIFEST_CONTEXT, body.as_bytes(), &trusted(&[&key(1)]), 1).unwrap();
        assert_eq!(valid.len(), 1);
        assert_eq!(SnapshotManifest::parse(body.as_bytes()).unwrap(), sample());
    }

    #[test]
    fn any_change_to_the_body_breaks_the_signature() {
        let body = sample().to_toml();
        let mut sigs = SignatureFile::default();
        sigs.sign(MANIFEST_CONTEXT, body.as_bytes(), &key(1));
        let tampered = body.replace("serial = 7", "serial = 8");
        assert_ne!(tampered, body, "the control edit must actually change the body");
        let err = sigs.verify(MANIFEST_CONTEXT, tampered.as_bytes(), &trusted(&[&key(1)]), 1).unwrap_err();
        assert_eq!(err, ManifestError::NotEnoughSignatures { valid: 0, needed: 1 });
    }

    #[test]
    fn a_key_statement_signature_is_not_a_manifest_signature() {
        let body = sample().to_toml();
        let mut sigs = SignatureFile::default();
        sigs.sign(KEY_STATEMENT_CONTEXT, body.as_bytes(), &key(1));
        assert!(sigs.verify(MANIFEST_CONTEXT, body.as_bytes(), &trusted(&[&key(1)]), 1).is_err());
    }

    #[test]
    fn unknown_signer_is_reported_as_a_key_change() {
        let body = sample().to_toml();
        let mut sigs = SignatureFile::default();
        sigs.sign(MANIFEST_CONTEXT, body.as_bytes(), &key(2));
        match sigs.verify(MANIFEST_CONTEXT, body.as_bytes(), &trusted(&[&key(1)]), 1) {
            Err(ManifestError::UnknownKey { offered }) => assert_eq!(offered, vec![PublicKey::from(&key(2))]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn threshold_counts_distinct_keys() {
        let body = sample().to_toml();
        let mut sigs = SignatureFile::default();
        sigs.sign(MANIFEST_CONTEXT, body.as_bytes(), &key(1));
        sigs.sign(MANIFEST_CONTEXT, body.as_bytes(), &key(1));
        let set = trusted(&[&key(1), &key(2)]);
        assert!(sigs.verify(MANIFEST_CONTEXT, body.as_bytes(), &set, 2).is_err(), "one key twice is not two");
        sigs.sign(MANIFEST_CONTEXT, body.as_bytes(), &key(2));
        assert_eq!(sigs.verify(MANIFEST_CONTEXT, body.as_bytes(), &set, 2).unwrap().len(), 2);
    }

    #[test]
    fn refuses_unsafe_or_malformed_manifests() {
        type Change = Box<dyn Fn(&mut SnapshotManifest)>;
        let cases: Vec<Change> = vec![
            Box::new(|m| m.repo[0].path = "../../etc".into()),
            Box::new(|m| m.repo[0].path = "/abs/path".into()),
            Box::new(|m| m.repo[0].path = "http://evil/x".into()),
            Box::new(|m| m.repo[0].path = "a//b".into()),
            Box::new(|m| m.repo[0].repodata_sha256 = "ABC".into()),
            Box::new(|m| m.repo[0].repodata_sha256 = m.repo[0].repodata_sha256.to_uppercase()),
            Box::new(|m| m.repo.push(m.repo[0].clone())),
            Box::new(|m| m.repo.clear()),
            Box::new(|m| m.format = 2),
            Box::new(|m| m.serial = 0),
            Box::new(|m| m.expires = m.published),
            Box::new(|m| m.channel = "st able".into()),
            Box::new(|m| m.packages[0].version = "1.0-1".into()),
            Box::new(|m| m.withdraws = vec!["../x".into()]),
            Box::new(|m| m.withdraws = vec![m.snapshot.clone()]),
            Box::new(|m| m.withdraws = vec!["2026.11".into(), "2026.11".into()]),
        ];
        assert!(sample().validate().is_ok(), "control: the unmodified sample is valid");
        for (i, case) in cases.iter().enumerate() {
            let mut m = sample();
            case(&mut m);
            assert!(SnapshotManifest::parse(m.to_toml().as_bytes()).is_err(), "case {i}");
        }
    }

    #[test]
    fn an_architecture_can_have_a_void_and_a_nodo_repository() {
        let mut m = sample();
        let mut nodo = m.repo[0].clone();
        nodo.name = NODO_REPO.into();
        nodo.path = "snapshots/2026.12/nodo/aarch64".into();
        m.repo.insert(0, nodo);
        let parsed = SnapshotManifest::parse(m.to_toml().as_bytes()).unwrap();
        let names: Vec<&str> = parsed.repos_for("aarch64").iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["void", "nodo"]);
        assert!(parsed.repos_for("x86_64").is_empty());
        m.repo[0].name = "no thing".into();
        assert!(SnapshotManifest::parse(m.to_toml().as_bytes()).is_err());
    }

    #[test]
    fn a_manifest_without_repository_names_means_void() {
        let body = sample().to_toml().replace("name = \"void\"\n", "");
        assert!(!body.contains("name = \"void\""), "{body}");
        assert_eq!(SnapshotManifest::parse(body.as_bytes()).unwrap().repo[0].name, "void");
    }

    #[test]
    fn withdrawals_are_signed_fields_that_older_manifests_leave_out() {
        assert!(!sample().to_toml().contains("withdraws"), "a manifest withdrawing nothing keeps its old form");
        let mut m = sample();
        m.withdraws = vec!["2026.13".into()];
        let parsed = SnapshotManifest::parse(m.to_toml().as_bytes()).unwrap();
        assert_eq!(parsed.withdraws, ["2026.13"]);
        assert!(parsed.withdraws_snapshot(Some("2026.13")));
        assert!(!parsed.withdraws_snapshot(Some("2026.11")));
        assert!(!parsed.withdraws_snapshot(None), "a device that does not know its snapshot is never told it was withdrawn");
    }

    #[test]
    fn refuses_unknown_fields() {
        let body = format!("{}\nallow_downgrade = true\n", sample().to_toml());
        assert!(SnapshotManifest::parse(body.as_bytes()).is_err());
    }

    #[test]
    fn repodata_must_match_exactly() {
        let m = sample();
        assert!(verify_repodata(&m.repo[0], b"index").is_ok());
        assert!(verify_repodata(&m.repo[0], b"index2").is_err());
    }

    #[test]
    fn public_keys_have_one_spelling() {
        let k = PublicKey::from(&key(1)).to_string();
        assert!(PublicKey::parse(&k).is_ok());
        assert!(PublicKey::parse(&k.to_uppercase()).is_err());
        assert!(PublicKey::parse(&k[..62]).is_err());
    }

    #[test]
    fn key_statements_are_validated() {
        let good = KeyStatement { format: 1, sequence: 1, keys: vec![PublicKey::from(&key(1))], threshold: 1 };
        assert!(good.validate().is_ok());
        for bad in [
            KeyStatement { threshold: 0, ..good.clone() },
            KeyStatement { threshold: 2, ..good.clone() },
            KeyStatement { keys: vec![], ..good.clone() },
            KeyStatement { keys: vec![good.keys[0], good.keys[0]], threshold: 1, ..good.clone() },
            KeyStatement { sequence: 0, ..good.clone() },
        ] {
            assert!(KeyStatement::parse(bad.to_toml().as_bytes()).is_err(), "{bad:?}");
        }
    }
}
