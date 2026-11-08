use std::fs;
use std::io::Cursor;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use super::*;
use crate::crypto::{SealWriter, CHUNK, MAGIC};

const PASS: &[u8] = b"correct horse battery staple";

fn fast() -> KdfParams {
    KdfParams { memory_kib: 8 * 1024, iterations: 1, lanes: 1 }
}

fn write(path: &Path, data: &[u8], mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// A device-like tree: settings, a secret, onion keys, encrypted storage, chain data, and a
/// symlink pointing outside.
fn tree(root: &Path) -> Plan {
    let big: Vec<u8> = (0..(3 * CHUNK + 123)).map(|i| (i * 7 % 251) as u8).collect();
    write(&root.join("settings/cuprated.toml"), b"peers = 32\n", 0o644);
    write(&root.join("settings/nested/deep/value"), b"x", 0o640);
    write(&root.join("secrets/moneropay.rpc"), b"s3cret", 0o600);
    write(&root.join("tor/hs/hs_ed25519_secret_key"), &[9u8; 96], 0o600);
    write(&root.join("tor/hs/hostname"), b"abc.onion\n", 0o600);
    write(&root.join("volumes/notes/db.sqlite"), &big, 0o600);
    write(&root.join("volumes/notes/chain/data.mdb"), b"CHAIN", 0o600);
    write(&root.join("settings/setuid-tool"), b"#!", 0o4755);
    symlink("/etc/shadow", root.join("settings/shadow-link")).unwrap();
    let src = |category, name: &str, rel: &str| Source { category, name: name.into(), path: root.join(rel) };
    Plan {
        sources: vec![
            src(Category::Settings, "nodo", "settings"),
            src(Category::Secrets, "nodo", "secrets"),
            src(Category::Onion, "tor", "tor"),
            src(Category::Storage, "notes", "volumes/notes"),
            src(Category::Settings, "absent", "nope"),
        ],
        device_key: Some(Zeroizing::new(vec![0x42; 32])),
        exclude: vec![root.join("volumes/notes/chain")],
    }
}

fn make() -> (tempfile::TempDir, Vec<u8>, Summary) {
    let dir = tempfile::tempdir().unwrap();
    let plan = tree(&dir.path().join("src"));
    let mut out = Vec::new();
    let summary = backup(&plan, PASS, fast(), &mut out, 1_700_000_000).unwrap();
    (dir, out, summary)
}

fn restore_into(dir: &Path, bytes: &[u8], pass: &[u8]) -> (Result<Summary, BackupError>, DirTarget) {
    let staging = dir.join("staging");
    let _ = fs::remove_dir_all(&staging);
    let mut target = DirTarget::create(&staging).unwrap();
    let result = restore(Cursor::new(bytes), pass, &mut target);
    (result, target)
}

#[test]
fn round_trip_restores_everything_but_chain_data_and_links() {
    let (dir, bytes, summary) = make();
    assert!(summary.has_device_key());
    assert_eq!(summary.missing.len(), 1);
    assert!(summary.skipped.iter().any(|p| p.ends_with("shadow-link")));
    assert!(summary.skipped.iter().any(|p| p.ends_with("chain")));
    assert!(!bytes.windows(6).any(|w| w == b"s3cret"), "secrets never appear in plaintext");

    let (result, target) = restore_into(dir.path(), &bytes, PASS);
    let restored = result.unwrap();
    assert_eq!(restored.entries, summary.entries);
    let out = target.root();
    let src = dir.path().join("src");
    for (archive, host) in [
        ("settings/nodo/cuprated.toml", "settings/cuprated.toml"),
        ("settings/nodo/nested/deep/value", "settings/nested/deep/value"),
        ("secrets/nodo/moneropay.rpc", "secrets/moneropay.rpc"),
        ("onion/tor/hs/hs_ed25519_secret_key", "tor/hs/hs_ed25519_secret_key"),
        ("storage/notes/db.sqlite", "volumes/notes/db.sqlite"),
    ] {
        assert_eq!(fs::read(out.join(archive)).unwrap(), fs::read(src.join(host)).unwrap(), "{archive}");
    }
    let mode = |p: &str| fs::metadata(out.join(p)).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode("secrets/nodo/moneropay.rpc"), 0o600);
    assert_eq!(mode("settings/nodo/setuid-tool") & 0o7000, 0, "setuid is stripped");
    assert!(!out.join("storage/notes/chain").exists());
    assert!(!out.join("settings/nodo/shadow-link").exists());
    assert_eq!(target.device_key.as_deref().map(|k| k.as_slice()), Some(&[0x42u8; 32][..]));
}

#[test]
fn wrong_passphrase_is_refused_and_writes_nothing() {
    let (dir, bytes, _) = make();
    assert!(verify(Cursor::new(&bytes), PASS).is_ok(), "control");
    let (result, target) = restore_into(dir.path(), &bytes, b"correct horse battery stapler");
    assert_eq!(result.unwrap_err(), BackupError::WrongPassphrase);
    assert!(!target.root().exists(), "staging is removed");
    assert!(target.device_key.is_none());
}

#[test]
fn any_flipped_byte_is_refused_and_writes_nothing() {
    let (dir, bytes, _) = make();
    let header_end = 12 + u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let chunk = CHUNK + 16;
    let offsets = [
        0,
        9,
        header_end - 3,
        header_end,
        header_end + 100,
        header_end + chunk - 1,
        header_end + chunk + 5,
        header_end + 2 * chunk + 77,
        bytes.len() - 17,
        bytes.len() - 1,
    ];
    for off in offsets {
        let mut bad = bytes.clone();
        bad[off] ^= 0x01;
        let (result, target) = restore_into(dir.path(), &bad, PASS);
        assert!(result.is_err(), "offset {off} was accepted");
        assert!(!target.root().exists(), "offset {off} left files behind");
    }
}

#[test]
fn truncated_extended_or_reordered_backups_are_refused() {
    let (dir, bytes, _) = make();
    let header_end = 12 + u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let chunk = CHUNK + 16;
    let full_chunks = (bytes.len() - header_end) / chunk;
    assert!(full_chunks >= 3, "the test needs several chunks");
    let mut cases = vec![
        bytes[..bytes.len() - 1].to_vec(),
        bytes[..header_end + chunk].to_vec(),
        bytes[..header_end + 2 * chunk].to_vec(),
        bytes[..header_end].to_vec(),
        [bytes.clone(), vec![0u8; 16]].concat(),
    ];
    let mut swapped = bytes.clone();
    let (a, b) = (header_end, header_end + chunk);
    let first = bytes[a..a + chunk].to_vec();
    swapped.copy_within(b..b + chunk, a);
    swapped[b..b + chunk].copy_from_slice(&first);
    cases.push(swapped);
    for (i, case) in cases.iter().enumerate() {
        let (result, target) = restore_into(dir.path(), case, PASS);
        assert!(result.is_err(), "case {i} was accepted");
        assert!(!target.root().exists(), "case {i} left files behind");
    }
}

#[test]
fn a_header_with_cheaper_key_derivation_is_refused() {
    let (_dir, bytes, _) = make();
    let text = String::from_utf8_lossy(&bytes[12..]).into_owned();
    assert!(text.contains("iterations = 1"));
    let mut forged = bytes.clone();
    let at = 12 + text.find("memory_kib = 8192").unwrap();
    forged[at..at + 17].copy_from_slice(b"memory_kib = 8193");
    assert!(verify(Cursor::new(&forged), PASS).is_err(), "the header is authenticated");
    let mut weak = bytes.clone();
    weak[at..at + 17].copy_from_slice(b"memory_kib = 0008");
    assert!(verify(Cursor::new(&weak), PASS).is_err());
}

#[test]
fn not_a_backup() {
    assert_eq!(verify(Cursor::new(b"hello world, not a backup"), PASS).unwrap_err(), BackupError::NotABackup);
}

#[test]
fn weak_passphrases_and_chain_sources_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let plan = tree(&dir.path().join("src"));
    assert_eq!(backup(&plan, b"short", fast(), Vec::new(), 0).unwrap_err(), BackupError::WeakPassphrase);
    let chain = Plan::standard(&[], &[], None);
    assert!(chain.check().is_ok(), "control: the standard plan is allowed");
    let mut chain = Plan::standard(&[], &[], None);
    chain.sources.push(Source { category: Category::Storage, name: "node".into(), path: "/media/monero/cuprated".into() });
    assert!(matches!(chain.check(), Err(BackupError::Refused(_))));
}

/// Builds a correctly encrypted backup with arbitrary entries, as someone else's device or a
/// buggy writer might, to show the reader validates what the crypto lets through.
fn crafted(entries: &[(u8, &str, u32, &[u8])], count: u64) -> Vec<u8> {
    let mut seal = SealWriter::new(Vec::new(), PASS, fast(), 0).unwrap();
    for (tag, path, mode, data) in entries {
        let mut h = vec![*tag];
        h.extend_from_slice(&(path.len() as u16).to_be_bytes());
        h.extend_from_slice(path.as_bytes());
        h.extend_from_slice(&mode.to_be_bytes());
        h.extend_from_slice(&(data.len() as u64).to_be_bytes());
        seal.write_all(&h).unwrap();
        seal.write_all(data).unwrap();
    }
    seal.write_all(&[0xff]).unwrap();
    seal.write_all(&count.to_be_bytes()).unwrap();
    seal.finish().unwrap()
}

#[test]
fn hostile_paths_inside_a_valid_backup_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let ok = crafted(&[(1, "settings/x/file", 0o644, b"hi")], 1);
    assert!(verify(Cursor::new(&ok), PASS).is_ok(), "control: a crafted but sane backup is accepted");
    for path in [
        "../etc/passwd",
        "settings/../../etc/passwd",
        "/etc/passwd",
        "settings",
        "settings//x",
        "settings/./x",
        "unknown/x",
        "device-key/other",
    ] {
        let bad = crafted(&[(1, path, 0o644, b"hi")], 1);
        let (result, target) = restore_into(dir.path(), &bad, PASS);
        assert!(matches!(result, Err(BackupError::Corrupt(_))), "{path}: {result:?}");
        assert!(!target.root().exists());
    }
    assert!(verify(Cursor::new(&crafted(&[(1, "settings/x/f", 0o644, b"a")], 2)), PASS).is_err(), "count mismatch");
    let dup = crafted(&[(1, "settings/x/f", 0o644, b"a"), (1, "settings/x/f", 0o644, b"b")], 2);
    let (result, target) = restore_into(dir.path(), &dup, PASS);
    assert!(result.is_err(), "a duplicate path cannot overwrite an earlier file");
    assert!(!target.root().exists());
    assert!(verify(Cursor::new(&crafted(&[(3, "device-key/device.key", 0o600, &[1; 2000])], 1)), PASS).is_err());
}

#[test]
fn starts_with_the_magic() {
    let (_dir, bytes, _) = make();
    assert_eq!(&bytes[..8], MAGIC);
}
