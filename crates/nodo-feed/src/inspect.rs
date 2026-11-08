//! The rules an untrusted package must pass before it may reach xbps.
//!
//! Inspection reads the archive from a reader, keeps only small metadata files in memory, and never
//! touches the disk. It keeps going after a violation, so the user and the package author see every
//! problem at once. Only damage that makes the rest of the stream unreadable stops it early.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read};

use nodo_manifest::{is_valid_id, Manifest};
use thiserror::Error;

use crate::plist::{self, Plist};
use crate::tar::{Header, Kind, TarError, TarReader};

/// Where a package's files must live, relative to the root.
const APPS_DIR: &str = "opt/nodo/apps";
/// Directories a package may name on the way to its own, as long as it leaves them unchanged.
const ANCESTORS: [&str; 4] = ["", "opt", "opt/nodo", "opt/nodo/apps"];
/// The manifest's location, relative to the package's directory.
pub const MANIFEST_FILE: &str = "manifest.toml";

const MAX_SHOWN_LEN: usize = 256;

/// Packages a feed may never replace. Compared after names are known to be valid package ids.
pub const BASE_PACKAGES: &[&str] = &[
    "base-container",
    "base-files",
    "base-minimal",
    "base-system",
    "base-voidstrap",
    "bash",
    "bubblewrap",
    "busybox",
    "ca-certificates",
    "caddy",
    "chrony",
    "coreutils",
    "cuprated",
    "cuprated-service",
    "dash",
    "dbus",
    "dhcpcd",
    "diffutils",
    "dracut",
    "e2fsprogs",
    "eudev",
    "findutils",
    "gawk",
    "glibc",
    "gnupg",
    "grep",
    "iproute2",
    "iptables",
    "iputils",
    "kmod",
    "libarchive",
    "monero",
    "monerod",
    "musl",
    "nftables",
    "nodo",
    "opendoas",
    "openssh",
    "openssl",
    "pam",
    "runit",
    "sed",
    "shadow",
    "sudo",
    "tar",
    "tor",
    "util-linux",
    "void-repo",
    "xbps",
    "zstd",
];

/// Prefixes for families of system packages, such as `linux6.6` and `linux-firmware`.
pub const BASE_PACKAGE_PREFIXES: &[&str] =
    &["base-", "glibc-", "linux", "monokulo-", "nodo-", "pam-", "runit-", "void-repo-", "xbps-"];

/// Properties that make xbps act outside the package's own files, even with a clean payload.
///
/// `alternatives` can repoint `/usr/bin/sh`, `replaces` can remove a system package, and `provides`
/// and `shlib-provides` can stand in for one when dependencies are resolved. `reverts` makes a
/// lower version count as an upgrade. The script and message keys hold what INSTALL and REMOVE do.
const FORBIDDEN_PROPERTIES: &[&str] = &[
    "alternatives",
    "install-msg",
    "install-script",
    "provides",
    "remove-msg",
    "remove-script",
    "replaces",
    "reverts",
    "shlib-provides",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InspectLimits {
    /// The decompression bomb limit, covering every byte of the decompressed stream.
    pub max_decompressed_bytes: u64,
    pub max_entries: u64,
    /// Total size of the metadata files held in memory.
    pub max_metadata_bytes: u64,
    /// zstd allocates a window of up to 2^n bytes on the strength of the frame header alone.
    pub window_log_max: u32,
}

impl Default for InspectLimits {
    fn default() -> Self {
        InspectLimits {
            max_decompressed_bytes: 1024 * 1024 * 1024,
            max_entries: 100_000,
            max_metadata_bytes: 16 * 1024 * 1024,
            window_log_max: 27,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub limits: InspectLimits,
    /// Further names to refuse, in addition to the built in lists.
    pub denied_packages: Vec<String>,
}

/// One broken rule. Text taken from the archive is already made safe to display.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Violation {
    #[error("{path}: packages may not have install scripts")]
    InstallScript { path: String },
    #[error("{path}: packages may not have remove scripts")]
    RemoveScript { path: String },
    #[error("{path}: setuid files are not allowed")]
    Setuid { path: String },
    #[error("{path}: setgid files are not allowed")]
    Setgid { path: String },
    #[error("{path}: the sticky bit is not allowed")]
    Sticky { path: String },
    #[error("{path}: world writable files are not allowed")]
    WorldWritable { path: String },
    #[error("{path}: must be owned by root, not {uid}:{gid}")]
    UnexpectedOwner { path: String, uid: u64, gid: u64 },
    #[error("{path}: device nodes are not allowed")]
    DeviceNode { path: String },
    #[error("{path}: named pipes are not allowed")]
    Fifo { path: String },
    #[error("{path}: unsupported entry type {kind:#04x}")]
    UnknownEntryType { path: String, kind: u8 },
    #[error("{path}: absolute paths are not allowed")]
    AbsolutePath { path: String },
    #[error("{path}: '..' is not allowed in paths")]
    ParentTraversal { path: String },
    #[error("{path}: the path is not in canonical form")]
    NonCanonicalPath { path: String },
    #[error("{path}: the path contains unsafe characters")]
    UnsafePathCharacters { path: String },
    #[error("{path}: outside the package's directory /{APPS_DIR}/<id>")]
    OutsideAppDir { path: String },
    #[error("{path}: must be a plain root owned 0755 directory, because it is shared with the system")]
    AncestorChanged { path: String },
    #[error("{path}: symlink target {target} leaves the package's directory")]
    SymlinkEscape { path: String, target: String },
    #[error("{path}: hard link target {target} is not an earlier file in this package")]
    HardlinkEscape { path: String, target: String },
    #[error("{path}: passes through a symlink in the same package")]
    PathThroughSymlink { path: String },
    #[error("{path}: appears more than once")]
    DuplicatePath { path: String },
    #[error("{path}: extended attribute or unsupported pax key {key}")]
    UnsupportedPaxKey { path: String, key: String },
    #[error("{path}: must be a regular file")]
    MetadataNotRegular { path: String },
    #[error("{file} is missing")]
    MetadataMissing { file: String },
    #[error("{file} is invalid: {detail}")]
    MetadataInvalid { file: String, detail: String },
    #[error("package metadata is larger than {limit} bytes")]
    MetadataTooLarge { limit: u64 },
    #[error("'{name}' is a base system package and cannot come from a feed")]
    BasePackageName { name: String },
    #[error("'{name}' is not a valid package id")]
    InvalidPackageName { name: String },
    #[error("package '{package}' does not match manifest id '{manifest}'")]
    PackageNameMismatch { package: String, manifest: String },
    #[error("package version '{package}' does not match manifest version '{manifest}'")]
    VersionMismatch { package: String, manifest: String },
    #[error("the package has no {MANIFEST_FILE} in its directory")]
    ManifestMissing,
    #[error("the manifest is invalid: {detail}")]
    ManifestInvalid { detail: String },
    #[error("package property '{key}' is not allowed")]
    ForbiddenProperty { key: String },
    #[error("files.plist: {path}: {detail}")]
    FilesListMismatch { path: String, detail: String },
    #[error("decompresses to more than {limit} bytes")]
    DecompressedTooLarge { limit: u64 },
    #[error("has more than {limit} entries")]
    TooManyEntries { limit: u64 },
    #[error("the archive ends unexpectedly")]
    Truncated,
    #[error("the archive is malformed: {detail}")]
    Malformed { detail: String },
    #[error("the archive uses an unsupported feature: {detail}")]
    UnsupportedFeature { detail: String },
    #[error("the file is not valid zstd data")]
    NotZstd,
}

/// What `props.plist` says about the package, made safe to display. The caller checks that the
/// architecture suits the device, and shows the run dependencies to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    pub name: String,
    /// `version_revision`, the form version comparisons use.
    pub version: String,
    pub architecture: String,
    pub run_depends: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inspection {
    pub package: Option<PackageInfo>,
    pub manifest: Option<Manifest>,
    pub entries: u64,
    pub decompressed_bytes: u64,
    pub violations: Vec<Violation>,
}

impl Inspection {
    /// Only a clean package may proceed to xbps.
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Makes archive text safe to show: a hostile name must not drive the terminal or fill the screen.
fn shown(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().take(MAX_SHOWN_LEN) {
        if c.is_control() {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    if text.chars().count() > MAX_SHOWN_LEN {
        out.push_str("...");
    }
    out
}

#[derive(Debug)]
struct BombLimit;

impl fmt::Display for BombLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("decompression limit reached")
    }
}

impl std::error::Error for BombLimit {}

/// Stops the decompressor at the limit, whatever the archive headers claim about sizes.
struct Limited<R> {
    inner: R,
    used: u64,
    limit: u64,
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let got = self.inner.read(buffer)?;
        self.used += got as u64;
        if self.used > self.limit {
            return Err(io::Error::other(BombLimit));
        }
        Ok(got)
    }
}

/// An entry reduced to the fields later rules need.
struct Entry {
    path: String,
    kind: Kind,
    link: String,
    mode: u32,
    uid: u64,
    gid: u64,
}

fn normalize(header: &Header, violations: &mut Vec<Violation>) -> Option<String> {
    let raw = header.path.as_str();
    let path = shown(raw);
    if !header.valid_utf8 || raw.chars().any(|c| c.is_control() || c == '\\') {
        violations.push(Violation::UnsafePathCharacters { path: path.clone() });
    }
    if raw.starts_with('/') {
        violations.push(Violation::AbsolutePath { path });
        return None;
    }
    let relative = raw.strip_prefix("./").unwrap_or(raw);
    if relative == "." || relative.is_empty() {
        if raw.is_empty() {
            violations.push(Violation::NonCanonicalPath { path });
            return None;
        }
        return Some(String::new());
    }
    // Only directories have a trailing slash. libarchive treats a file named that way as one.
    let trimmed = match relative.strip_suffix('/') {
        Some(rest) if header.kind == Kind::Directory => rest,
        _ => relative,
    };
    let mut ok = true;
    for segment in trimmed.split('/') {
        if segment == ".." {
            violations.push(Violation::ParentTraversal { path: path.clone() });
            ok = false;
        } else if segment.is_empty() || segment == "." {
            violations.push(Violation::NonCanonicalPath { path: path.clone() });
            ok = false;
        }
    }
    ok.then(|| trimmed.to_string())
}

struct AppDir {
    /// `opt/nodo/apps/<id>`, or `None` when no usable id is known and only the shared parent can
    /// be checked. That case always has a metadata violation of its own.
    prefix: Option<String>,
}

impl AppDir {
    fn contains(&self, path: &str) -> bool {
        match &self.prefix {
            Some(prefix) => path == prefix || path.strip_prefix(prefix.as_str()).is_some_and(|r| r.starts_with('/')),
            None => path.strip_prefix(APPS_DIR).is_some_and(|r| r.starts_with('/') && r.len() > 1),
        }
    }

    fn is_root(&self, path: &str) -> bool {
        match &self.prefix {
            Some(prefix) => path == prefix,
            None => path.strip_prefix(APPS_DIR).is_some_and(|r| r.len() > 1 && !r[1..].contains('/')),
        }
    }

    fn root_depth(&self) -> usize {
        APPS_DIR.split('/').count() + 1
    }
}

fn check_entry_attributes(entry: &Entry, violations: &mut Vec<Violation>) {
    let path = || shown(&entry.path);
    if entry.mode & 0o4000 != 0 {
        violations.push(Violation::Setuid { path: path() });
    }
    if entry.mode & 0o2000 != 0 {
        violations.push(Violation::Setgid { path: path() });
    }
    if entry.mode & 0o1000 != 0 {
        violations.push(Violation::Sticky { path: path() });
    }
    // Writable by a different service would let one app modify another. Symlink modes mean nothing.
    if entry.mode & 0o002 != 0 && entry.kind != Kind::Symlink {
        violations.push(Violation::WorldWritable { path: path() });
    }
    if entry.uid != 0 || entry.gid != 0 {
        violations.push(Violation::UnexpectedOwner { path: path(), uid: entry.uid, gid: entry.gid });
    }
    match entry.kind {
        Kind::CharDevice | Kind::BlockDevice => violations.push(Violation::DeviceNode { path: path() }),
        Kind::Fifo => violations.push(Violation::Fifo { path: path() }),
        Kind::Unknown(kind) => violations.push(Violation::UnknownEntryType { path: path(), kind }),
        Kind::Regular | Kind::Hardlink | Kind::Symlink | Kind::Directory => {}
    }
}

/// Whether a symlink's target stays inside the package.
///
/// `..` is only accepted at the start of a relative target. Combined with the rule that no entry
/// passes through a symlink, this means the physical location of the link is exactly its path, so
/// the leading `..` steps can be counted. Whatever the remaining components pass through, each of
/// them is either a real directory or another link checked by this same rule.
fn symlink_stays_inside(entry: &Entry, app: &AppDir) -> bool {
    let target = entry.link.as_str();
    if target.is_empty() || target.chars().any(|c| c.is_control() || c == '\\') {
        return false;
    }
    let segments_ok = |rest: &str| rest.split('/').all(|s| !s.is_empty() && s != "." && s != "..");
    if let Some(absolute) = target.strip_prefix('/') {
        return segments_ok(absolute) && app.contains(absolute);
    }
    let parent_depth = entry.path.split('/').count() - 1;
    let mut rest = target;
    let mut ups = 0;
    while let Some(after) = rest.strip_prefix("../") {
        ups += 1;
        rest = after;
    }
    if rest == ".." {
        ups += 1;
        rest = "";
    }
    let Some(depth_after) = parent_depth.checked_sub(ups) else {
        return false;
    };
    (rest.is_empty() || segments_ok(rest)) && depth_after >= app.root_depth()
}

fn proper_ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.char_indices().filter(|(_, c)| *c == '/').map(|(at, _)| &path[..at])
}

fn check_tree(entries: &[Entry], app: &AppDir, violations: &mut Vec<Violation>) {
    let symlinks: BTreeSet<&str> =
        entries.iter().filter(|e| e.kind == Kind::Symlink).map(|e| e.path.as_str()).collect();
    let through_symlink = |path: &str| proper_ancestors(path).any(|a| symlinks.contains(a));

    let mut earlier_files: BTreeSet<&str> = BTreeSet::new();
    for entry in entries {
        let path = entry.path.as_str();
        if path == "props.plist" || path == "files.plist" {
            if entry.kind != Kind::Regular {
                violations.push(Violation::MetadataNotRegular { path: path.to_string() });
            }
        } else if path == "INSTALL" {
            violations.push(Violation::InstallScript { path: path.to_string() });
        } else if path == "REMOVE" {
            violations.push(Violation::RemoveScript { path: path.to_string() });
        } else if ANCESTORS.contains(&path) || app.is_root(path) {
            // Extracting these with other attributes would change a directory the system shares.
            let plain = entry.kind == Kind::Directory && entry.mode & 0o7777 == 0o755;
            if !plain {
                violations.push(Violation::AncestorChanged { path: shown(path) });
            }
        } else if !app.contains(path) {
            violations.push(Violation::OutsideAppDir { path: shown(path) });
        }

        if through_symlink(path) {
            violations.push(Violation::PathThroughSymlink { path: shown(path) });
        }

        match entry.kind {
            Kind::Symlink if !symlink_stays_inside(entry, app) => {
                violations.push(Violation::SymlinkEscape { path: shown(path), target: shown(&entry.link) });
            }
            Kind::Hardlink => {
                let target = entry.link.strip_prefix("./").unwrap_or(&entry.link);
                let inside = app.contains(target) && earlier_files.contains(target) && !through_symlink(target);
                if !inside {
                    violations.push(Violation::HardlinkEscape { path: shown(path), target: shown(&entry.link) });
                }
            }
            Kind::Regular => {
                earlier_files.insert(path);
            }
            _ => {}
        }
    }
}

fn is_base_package(name: &str, policy: &Policy) -> bool {
    BASE_PACKAGES.contains(&name)
        || BASE_PACKAGE_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        || policy.denied_packages.iter().any(|denied| denied == name)
}

fn string_list(value: Option<&Plist>) -> Option<Vec<String>> {
    match value {
        None => Some(Vec::new()),
        Some(value) => value.as_array()?.iter().map(|item| item.as_str().map(str::to_string)).collect(),
    }
}

/// The result of reading `props.plist`. The name is returned even when other fields are unusable,
/// because the directory rules can still use it.
fn check_props(bytes: &[u8], policy: &Policy, out: &mut Inspection) -> Option<String> {
    let invalid = |detail: &str| Violation::MetadataInvalid { file: "props.plist".to_string(), detail: detail.to_string() };
    let parsed = match plist::parse(bytes) {
        Ok(parsed) => parsed,
        Err(detail) => {
            out.violations.push(invalid(&detail));
            return None;
        }
    };
    let Some(props) = parsed.as_dict() else {
        out.violations.push(invalid("the root is not a dict"));
        return None;
    };

    for key in props.keys() {
        if FORBIDDEN_PROPERTIES.contains(&key.as_str()) {
            out.violations.push(Violation::ForbiddenProperty { key: shown(key) });
        }
    }

    let get = |key: &str| props.get(key).and_then(Plist::as_str);
    let Some(name) = get("pkgname") else {
        out.violations.push(invalid("pkgname is missing"));
        return None;
    };

    if !is_valid_id(name) {
        out.violations.push(Violation::InvalidPackageName { name: shown(name) });
    }
    if is_base_package(name, policy) {
        out.violations.push(Violation::BasePackageName { name: shown(name) });
    }

    let pkgver = get("pkgver");
    let expected_prefix = format!("{name}-");
    let version = pkgver.and_then(|p| p.strip_prefix(expected_prefix.as_str())).filter(|version| {
        // xbps reads the revision from the part after the last underscore.
        version.rsplit_once('_').is_some_and(|(_, revision)| {
            !revision.is_empty() && revision.bytes().all(|b| b.is_ascii_digit())
        })
    });
    // Both fields exist, and something reading the other one must not see a different version.
    if let (Some(version), Some(declared)) = (version, get("version")) {
        if version != declared {
            out.violations.push(invalid("version does not match pkgver"));
        }
    }
    let run_depends = string_list(props.get("run_depends"));
    if run_depends.is_none() {
        out.violations.push(invalid("run_depends is not a list of strings"));
    }
    match (version, get("architecture")) {
        (Some(version), Some(architecture)) => {
            out.package = Some(PackageInfo {
                name: shown(name),
                version: shown(version),
                architecture: shown(architecture),
                run_depends: run_depends.unwrap_or_default().iter().map(|d| shown(d)).collect(),
            });
        }
        (None, _) => out.violations.push(invalid("pkgver is missing or is not pkgname-version_revision")),
        (_, None) => out.violations.push(invalid("architecture is missing")),
    }

    if is_valid_id(name) {
        let prefix = format!("/{APPS_DIR}/{name}/");
        let conf_files = string_list(props.get("conf_files")).unwrap_or_else(|| vec![String::new()]);
        for file in conf_files {
            if !file.starts_with(&prefix) || file.split('/').any(|s| s == "..") {
                out.violations.push(Violation::FilesListMismatch {
                    path: shown(&file),
                    detail: "conf_files entry is outside the package's directory".to_string(),
                });
            }
        }
    }
    Some(name.to_string())
}

fn check_manifest(name: &str, bytes: Option<&[u8]>, out: &mut Inspection) {
    let Some(bytes) = bytes else {
        out.violations.push(Violation::ManifestMissing);
        return;
    };
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            out.violations.push(Violation::ManifestInvalid { detail: "is not valid UTF-8".to_string() });
            return;
        }
    };
    let manifest = match Manifest::parse(text) {
        Ok(manifest) => manifest,
        Err(error) => {
            out.violations.push(Violation::ManifestInvalid { detail: shown(&error.to_string()) });
            return;
        }
    };
    if manifest.id != name {
        out.violations.push(Violation::PackageNameMismatch { package: shown(name), manifest: shown(&manifest.id) });
    }
    if let Some(package) = &out.package {
        let without_revision = package.version.rsplit_once('_').map_or(package.version.as_str(), |(version, _)| version);
        if without_revision != manifest.version {
            out.violations.push(Violation::VersionMismatch {
                package: shown(&package.version),
                manifest: shown(&manifest.version),
            });
        }
    }
    out.manifest = Some(manifest);
}

/// `files.plist` is what xbps deletes when the package is removed. It is checked against the
/// payload in both directions, because a listed `/usr/bin/sh` would remove that file from the
/// system even though the archive never contained it.
fn check_files_list(bytes: &[u8], entries: &[Entry], app: &AppDir, violations: &mut Vec<Violation>) {
    let invalid = |detail: &str| Violation::MetadataInvalid { file: "files.plist".to_string(), detail: detail.to_string() };
    let parsed = match plist::parse(bytes) {
        Ok(parsed) => parsed,
        Err(detail) => {
            violations.push(invalid(&detail));
            return;
        }
    };
    let Some(lists) = parsed.as_dict() else {
        violations.push(invalid("the root is not a dict"));
        return;
    };

    let by_path: BTreeMap<&str, &Entry> = entries.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut listed: BTreeSet<String> = BTreeSet::new();
    for (list_name, list) in lists {
        let is_kind: fn(Kind) -> bool = match list_name.as_str() {
            "files" | "conf_files" => |kind| matches!(kind, Kind::Regular | Kind::Hardlink),
            "links" => |kind| kind == Kind::Symlink,
            "dirs" => |kind| kind == Kind::Directory,
            _ => {
                violations.push(invalid("has an unknown list"));
                continue;
            }
        };
        let Some(items) = list.as_array() else {
            violations.push(invalid("a list is not an array"));
            continue;
        };
        for item in items {
            let Some(file) = item.as_dict().and_then(|d| d.get("file")).and_then(Plist::as_str) else {
                violations.push(invalid("an entry has no file"));
                continue;
            };
            let mismatch = |detail: &str| Violation::FilesListMismatch { path: shown(file), detail: detail.to_string() };
            let Some(path) = file.strip_prefix('/') else {
                violations.push(mismatch("is not an absolute path"));
                continue;
            };
            if !app.contains(path) && !ANCESTORS.contains(&path) {
                violations.push(mismatch("is outside the package's directory"));
                continue;
            }
            match by_path.get(path) {
                // xbps-create lists directories here but does not store them in the archive.
                None if list_name == "dirs" => {}
                None => violations.push(mismatch("is not in the archive")),
                Some(entry) if !is_kind(entry.kind) => violations.push(mismatch("has a different type in the archive")),
                Some(_) => {
                    listed.insert(path.to_string());
                }
            }
        }
    }

    // Empty directories left behind on removal are harmless, so only files and links must be listed.
    for entry in entries {
        let must_be_listed = matches!(entry.kind, Kind::Regular | Kind::Hardlink | Kind::Symlink)
            && app.contains(&entry.path)
            && !listed.contains(&entry.path);
        if must_be_listed {
            violations.push(Violation::FilesListMismatch {
                path: shown(&entry.path),
                detail: "is in the archive but not listed".to_string(),
            });
        }
    }
}

/// Reads the archive, recording every entry and holding on to the metadata files.
struct Collected {
    entries: Vec<Entry>,
    files: BTreeMap<String, Vec<u8>>,
    decompressed_bytes: u64,
}

fn is_metadata_file(path: &str) -> bool {
    if path == "props.plist" || path == "files.plist" {
        return true;
    }
    // The owning id is unknown until props.plist has been read, which may come later.
    path.strip_prefix(APPS_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .and_then(|rest| rest.strip_suffix(MANIFEST_FILE))
        .and_then(|rest| rest.strip_suffix('/'))
        .is_some_and(|id| !id.contains('/'))
}

fn collect<R: Read>(reader: R, policy: &Policy, violations: &mut Vec<Violation>) -> Collected {
    let limits = policy.limits;
    let mut collected = Collected { entries: Vec::new(), files: BTreeMap::new(), decompressed_bytes: 0 };

    let decoder = zstd::stream::read::Decoder::new(reader).and_then(|mut decoder| {
        decoder.window_log_max(limits.window_log_max)?;
        Ok(decoder)
    });
    let Ok(decoder) = decoder else {
        violations.push(Violation::NotZstd);
        return collected;
    };
    let mut limited = Limited { inner: decoder, used: 0, limit: limits.max_decompressed_bytes };
    let result = read_entries(&mut limited, policy, &mut collected, violations);
    collected.decompressed_bytes = limited.used;

    if let Err(error) = result {
        violations.push(match error {
            TarError::Truncated => Violation::Truncated,
            TarError::Malformed(detail) => Violation::Malformed { detail },
            TarError::Unsupported(detail) => Violation::UnsupportedFeature { detail },
            TarError::Io(error) if error.get_ref().is_some_and(|e| e.is::<BombLimit>()) => {
                Violation::DecompressedTooLarge { limit: limits.max_decompressed_bytes }
            }
            TarError::Io(_) => Violation::NotZstd,
        });
    }
    collected
}

fn read_entries<R: Read>(
    reader: R,
    policy: &Policy,
    collected: &mut Collected,
    violations: &mut Vec<Violation>,
) -> Result<(), TarError> {
    let limits = policy.limits;
    let mut tar = TarReader::new(reader);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut declared: u64 = 0;
    let mut retained: u64 = 0;
    let mut count: u64 = 0;

    while let Some(header) = tar.next_header()? {
        count += 1;
        if count > limits.max_entries {
            violations.push(Violation::TooManyEntries { limit: limits.max_entries });
            return Ok(());
        }
        // Refusing on the claimed size saves decompressing everything to find out.
        declared = declared.saturating_add(header.size);
        if declared > limits.max_decompressed_bytes {
            violations.push(Violation::DecompressedTooLarge { limit: limits.max_decompressed_bytes });
            return Ok(());
        }

        for key in &header.unknown_pax_keys {
            violations.push(Violation::UnsupportedPaxKey { path: shown(&header.path), key: shown(key) });
        }

        let normalized = normalize(&header, violations);
        let attributes = Entry {
            path: normalized.clone().unwrap_or_else(|| header.path.clone()),
            kind: header.kind,
            link: header.link.clone(),
            mode: header.mode,
            uid: header.uid,
            gid: header.gid,
        };
        check_entry_attributes(&attributes, violations);

        let Some(path) = normalized else {
            continue;
        };
        if !seen.insert(path.clone()) {
            violations.push(Violation::DuplicatePath { path: shown(&path) });
            continue;
        }
        if is_metadata_file(&path) && header.kind == Kind::Regular {
            retained = retained.saturating_add(header.size);
            if retained > limits.max_metadata_bytes {
                violations.push(Violation::MetadataTooLarge { limit: limits.max_metadata_bytes });
            } else {
                collected.files.insert(path.clone(), tar.read_body()?);
            }
        }
        collected.entries.push(attributes);
    }
    Ok(())
}

/// Inspects an `.xbps` file from any reader. Pure: nothing is written and nothing is executed.
pub fn inspect_package<R: Read>(archive: R, policy: &Policy) -> Inspection {
    let mut out = Inspection::default();
    let collected = collect(archive, policy, &mut out.violations);
    out.entries = collected.entries.len() as u64;
    out.decompressed_bytes = collected.decompressed_bytes;

    let name = match collected.files.get("props.plist") {
        Some(bytes) => check_props(bytes, policy, &mut out),
        None => {
            out.violations.push(Violation::MetadataMissing { file: "props.plist".to_string() });
            None
        }
    };

    let id = name.as_deref().filter(|name| is_valid_id(name));
    let app = AppDir { prefix: id.map(|id| format!("{APPS_DIR}/{id}")) };
    check_tree(&collected.entries, &app, &mut out.violations);

    match collected.files.get("files.plist") {
        Some(bytes) => check_files_list(bytes, &collected.entries, &app, &mut out.violations),
        None => out.violations.push(Violation::MetadataMissing { file: "files.plist".to_string() }),
    }

    if let Some(name) = &name {
        let manifest_path = format!("{APPS_DIR}/{name}/{MANIFEST_FILE}");
        check_manifest(name, collected.files.get(&manifest_path).map(Vec::as_slice), &mut out);
    }
    out
}

#[cfg(test)]
#[path = "inspect_tests.rs"]
mod tests;
