//! A feed's repository index and its signing key, checked against the key the user pinned.
//!
//! xbps does not sign the index itself. Each package carries its own signature (`.sig2`: RSA,
//! PKCS#1 v1.5, over the SHA-256 of the whole file), and the index only names the key that made
//! them. So the index is used to find a package and its expected hash, and the package bytes are
//! what the signature proves. A forged index can at most point at another genuinely signed package,
//! which the pinned key, the name and version checks and the version floor then judge.

use std::collections::BTreeMap;
use std::io::Read;

use base64::Engine as _;
use md5::{Digest as _, Md5};
use rsa::pkcs8::DecodePublicKey;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use sha2::Sha256;
use thiserror::Error;

use crate::pins::KeyFingerprint;
use crate::plist::{self, Plist};
use crate::tar::{Kind, TarError, TarReader};

/// The index lists every package of a feed, so it is allowed to be much larger than one package's
/// metadata, but not unbounded.
const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 16;
const WINDOW_LOG_MAX: u32 = 27;
/// Keys below this size are not accepted however they were pinned.
const MIN_KEY_BITS: usize = 2048;
const MAX_KEY_BITS: usize = 16384;
const MAX_PEM_LEN: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RepoError {
    #[error("the repository index is not valid zstd data")]
    NotZstd,
    #[error("the repository index is malformed: {0}")]
    Malformed(String),
    #[error("the repository index is larger than {MAX_INDEX_BYTES} bytes")]
    TooLarge,
    #[error("{file} is invalid: {detail}")]
    Invalid { file: String, detail: String },
    #[error("the repository is not signed")]
    Unsigned,
    #[error("the repository key is unusable: {0}")]
    BadKey(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SignatureError {
    #[error("the signature does not match the package and the feed's key")]
    Mismatch,
    #[error("the package's SHA-256 does not match the repository index")]
    HashMismatch,
    #[error("the package is {found} bytes, the repository index says {expected}")]
    SizeMismatch { expected: u64, found: u64 },
}

/// The key a repository says it is signed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoKey {
    pem: Vec<u8>,
    key: RsaPublicKey,
    pub fingerprint: KeyFingerprint,
    pub signed_by: String,
    pub bits: usize,
}

impl RepoKey {
    /// Reads a PEM `PUBLIC KEY`, the form xbps stores in `index-meta.plist`.
    pub fn from_pem(pem: &[u8], signed_by: &str) -> Result<Self, RepoError> {
        if pem.len() > MAX_PEM_LEN {
            return Err(RepoError::BadKey("the key is too long".to_string()));
        }
        let text = std::str::from_utf8(pem).map_err(|_| RepoError::BadKey("the key is not text".to_string()))?;
        let key = RsaPublicKey::from_public_key_pem(text.trim())
            .map_err(|_| RepoError::BadKey("the key is not an RSA public key".to_string()))?;
        let bits = key.size() * 8;
        if !(MIN_KEY_BITS..=MAX_KEY_BITS).contains(&bits) {
            return Err(RepoError::BadKey(format!("{bits} bit keys are not accepted")));
        }
        let fingerprint = fingerprint(&key);
        Ok(RepoKey { pem: pem.to_vec(), key, fingerprint, signed_by: shown(signed_by), bits })
    }

    pub fn pem(&self) -> &[u8] {
        &self.pem
    }

    /// Checks a `.sig2` signature over the exact bytes given.
    pub fn verify(&self, file: &[u8], signature: &[u8]) -> Result<(), SignatureError> {
        let digest = Sha256::digest(file);
        self.key.verify(Pkcs1v15Sign::new::<Sha256>(), &digest, signature).map_err(|_| SignatureError::Mismatch)
    }
}

/// The fingerprint xbps shows for a key: MD5 over the key in SSH wire form, the same value
/// `ssh-keygen -l -E md5` prints. Matching it lets the user compare what nodo shows with what the
/// feed's publisher announces.
fn fingerprint(key: &RsaPublicKey) -> KeyFingerprint {
    fn ssh_mpint(out: &mut Vec<u8>, bytes: &[u8]) {
        let pad = bytes.first().is_some_and(|b| b & 0x80 != 0);
        let len = bytes.len() + usize::from(pad);
        out.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes());
        if pad {
            out.push(0);
        }
        out.extend_from_slice(bytes);
    }
    let mut blob = Vec::new();
    blob.extend_from_slice(&7u32.to_be_bytes());
    blob.extend_from_slice(b"ssh-rsa");
    ssh_mpint(&mut blob, &key.e().to_bytes_be());
    ssh_mpint(&mut blob, &key.n().to_bytes_be());
    let hex: String = Md5::digest(&blob).iter().map(|b| format!("{b:02x}")).collect();
    KeyFingerprint::new(&hex).expect("an MD5 digest is 32 hex digits")
}

/// One package as the repository index describes it, made safe to display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoEntry {
    pub name: String,
    /// `version_revision`.
    pub version: String,
    pub architecture: String,
    pub sha256: [u8; 32],
    pub size: u64,
}

impl RepoEntry {
    /// The file name xbps gives the package in a repository.
    pub fn file_name(&self) -> String {
        format!("{}-{}.{}.xbps", self.name, self.version, self.architecture)
    }

    /// Checks downloaded bytes against what the index promised, before anything else reads them.
    pub fn check_bytes(&self, file: &[u8]) -> Result<(), SignatureError> {
        let found = file.len() as u64;
        if found != self.size {
            return Err(SignatureError::SizeMismatch { expected: self.size, found });
        }
        if Sha256::digest(file).as_slice() != self.sha256 {
            return Err(SignatureError::HashMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repodata {
    pub key: Option<RepoKey>,
    pub packages: BTreeMap<String, RepoEntry>,
}

impl Repodata {
    /// The signing key, which must exist for anything from a feed.
    pub fn signing_key(&self) -> Result<&RepoKey, RepoError> {
        self.key.as_ref().ok_or(RepoError::Unsigned)
    }
}

fn shown(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(256).collect()
}

fn invalid(file: &str, detail: impl Into<String>) -> RepoError {
    RepoError::Invalid { file: file.to_string(), detail: detail.into() }
}

fn decode_hex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, pair) in text.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(pair).ok()?;
        out[index] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

fn read_meta(bytes: &[u8]) -> Result<Option<RepoKey>, RepoError> {
    const FILE: &str = "index-meta.plist";
    // An unsigned repository has an empty meta file.
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let parsed = plist::parse(bytes).map_err(|detail| invalid(FILE, detail))?;
    let meta = parsed.as_dict().ok_or_else(|| invalid(FILE, "the root is not a dict"))?;
    let Some(Plist::Data(encoded)) = meta.get("public-key") else {
        return Ok(None);
    };
    if meta.get("signature-type").and_then(Plist::as_str) != Some("rsa") {
        return Err(invalid(FILE, "only RSA signatures are supported"));
    }
    let compact: String = encoded.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let pem = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map_err(|_| invalid(FILE, "public-key is not base64"))?;
    let signed_by = meta.get("signature-by").and_then(Plist::as_str).unwrap_or("");
    RepoKey::from_pem(&pem, signed_by).map(Some)
}

fn read_index(bytes: &[u8]) -> Result<BTreeMap<String, RepoEntry>, RepoError> {
    const FILE: &str = "index.plist";
    let parsed = plist::parse(bytes).map_err(|detail| invalid(FILE, detail))?;
    let index = parsed.as_dict().ok_or_else(|| invalid(FILE, "the root is not a dict"))?;
    let mut packages = BTreeMap::new();
    for (name, props) in index {
        let field = |key: &str| props.as_dict().and_then(|d| d.get(key));
        let text = |key: &str| field(key).and_then(Plist::as_str);
        let what = |detail: &str| invalid(FILE, format!("{}: {detail}", shown(name)));
        let pkgver = text("pkgver").ok_or_else(|| what("pkgver is missing"))?;
        let version = pkgver
            .strip_prefix(name.as_str())
            .and_then(|rest| rest.strip_prefix('-'))
            .filter(|version| version.rsplit_once('_').is_some_and(|(_, r)| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit())))
            .ok_or_else(|| what("pkgver is not pkgname-version_revision"))?;
        let architecture = text("architecture").ok_or_else(|| what("architecture is missing"))?;
        let sha256 = text("filename-sha256").and_then(decode_hex32).ok_or_else(|| what("filename-sha256 is invalid"))?;
        let size = match field("filename-size") {
            Some(Plist::Integer(digits)) => digits.parse().map_err(|_| what("filename-size is out of range"))?,
            _ => return Err(what("filename-size is missing")),
        };
        packages.insert(
            name.clone(),
            RepoEntry {
                name: shown(name),
                version: shown(version),
                architecture: shown(architecture),
                sha256,
                size,
            },
        );
    }
    Ok(packages)
}

fn tar_error(error: TarError, read: u64) -> RepoError {
    match error {
        TarError::Truncated if read > MAX_INDEX_BYTES => RepoError::TooLarge,
        TarError::Truncated => RepoError::Malformed("the archive ends unexpectedly".to_string()),
        TarError::Malformed(detail) | TarError::Unsupported(detail) => RepoError::Malformed(detail),
        // The decompressor reports damaged data as an I/O error.
        TarError::Io(_) => RepoError::NotZstd,
    }
}

/// Reads an `<arch>-repodata` file: a zstd compressed tar holding `index.plist` and
/// `index-meta.plist`. Pure: nothing is written and nothing is executed.
pub fn parse_repodata(bytes: &[u8]) -> Result<Repodata, RepoError> {
    let mut decoder = zstd::stream::read::Decoder::new(bytes).map_err(|_| RepoError::NotZstd)?;
    decoder.window_log_max(WINDOW_LOG_MAX).map_err(|_| RepoError::NotZstd)?;
    let mut tar = TarReader::new(decoder.take(MAX_INDEX_BYTES + 1));
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut total: u64 = 0;
    let mut entries = 0;
    loop {
        let header = match tar.next_header() {
            Ok(Some(header)) => header,
            Ok(None) => break,
            Err(error) => return Err(tar_error(error, total)),
        };
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(RepoError::Malformed("too many entries".to_string()));
        }
        total = total.saturating_add(header.size);
        if total > MAX_INDEX_BYTES {
            return Err(RepoError::TooLarge);
        }
        if header.kind != Kind::Regular {
            return Err(RepoError::Malformed("an entry is not a regular file".to_string()));
        }
        let body = tar.read_body().map_err(|error| tar_error(error, total))?;
        if files.insert(header.path.clone(), body).is_some() {
            return Err(RepoError::Malformed("an entry appears twice".to_string()));
        }
    }
    let index = files.get("index.plist").ok_or_else(|| invalid("index.plist", "is missing"))?;
    let packages = read_index(index)?;
    let key = match files.get("index-meta.plist") {
        Some(meta) => read_meta(meta)?,
        None => None,
    };
    Ok(Repodata { key, packages })
}

#[cfg(test)]
#[path = "repo_tests.rs"]
mod tests;
