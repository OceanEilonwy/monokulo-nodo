use super::*;
use crate::inspect::{inspect_package, Policy};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/signed-feed");

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).unwrap()
}

fn repodata() -> Repodata {
    parse_repodata(&fixture("x86_64-repodata")).unwrap()
}

/// What `xbps-query -vL` printed for the fixture repository, as nodo spells fingerprints.
fn xbps_fingerprint() -> KeyFingerprint {
    let listing = String::from_utf8(fixture("query-L.txt")).unwrap();
    let line = listing.lines().nth(2).unwrap();
    KeyFingerprint::new(line.split_whitespace().nth(1).unwrap()).unwrap()
}

#[test]
fn reads_a_real_signed_index() {
    let repo = repodata();
    let key = repo.signing_key().unwrap();
    assert_eq!(key.signed_by, "nodo test");
    assert_eq!(key.bits, 2048);
    let demo = &repo.packages["demo"];
    assert_eq!(demo.version, "1.1_1", "xbps-rindex keeps the newest version");
    assert_eq!(demo.architecture, "x86_64");
    assert_eq!(demo.file_name(), "demo-1.1_1.x86_64.xbps");
    assert!(!repo.packages.contains_key("other"), "xbps-rindex left out the other architecture");
}

#[test]
fn fingerprints_match_what_xbps_shows() {
    assert_eq!(repodata().signing_key().unwrap().fingerprint, xbps_fingerprint());
    let pem = fixture("test-signing-key.pub");
    assert_eq!(RepoKey::from_pem(&pem, "x").unwrap().fingerprint, xbps_fingerprint());
}

#[test]
fn real_signatures_verify_and_tampering_is_caught() {
    let repo = repodata();
    let key = repo.signing_key().unwrap();
    let file = fixture("demo-1.1_1.x86_64.xbps");
    let signature = fixture("demo-1.1_1.x86_64.xbps.sig2");
    key.verify(&file, &signature).unwrap();
    repo.packages["demo"].check_bytes(&file).unwrap();

    let mut flipped = file.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert_eq!(key.verify(&flipped, &signature), Err(SignatureError::Mismatch));
    assert_eq!(repo.packages["demo"].check_bytes(&flipped), Err(SignatureError::HashMismatch));

    // A genuine signature for another package does not carry over.
    let older = fixture("demo-1.0_1.x86_64.xbps");
    assert_eq!(key.verify(&older, &signature), Err(SignatureError::Mismatch));
    let mut bad_sig = signature.clone();
    bad_sig[10] ^= 1;
    assert_eq!(key.verify(&file, &bad_sig), Err(SignatureError::Mismatch));
    assert_eq!(key.verify(&file, &[]), Err(SignatureError::Mismatch));
}

#[test]
fn sizes_are_checked_before_hashes() {
    let repo = repodata();
    let file = fixture("demo-1.1_1.x86_64.xbps");
    let short = &file[..file.len() - 1];
    assert_eq!(
        repo.packages["demo"].check_bytes(short),
        Err(SignatureError::SizeMismatch { expected: file.len() as u64, found: file.len() as u64 - 1 })
    );
}

#[test]
fn real_xbps_create_output_passes_the_gate() {
    for name in ["demo-1.0_1.x86_64.xbps", "demo-1.1_1.x86_64.xbps", "other-2.0_1.aarch64.xbps"] {
        let inspection = inspect_package(fixture(name).as_slice(), &Policy::default());
        assert!(inspection.is_clean(), "{name}: {:?}", inspection.violations);
    }
}

#[test]
fn an_unsigned_index_has_no_key() {
    let repo = Repodata { key: None, packages: BTreeMap::new() };
    assert_eq!(repo.signing_key(), Err(RepoError::Unsigned));
    assert_eq!(read_meta(b"").unwrap(), None);
}

#[test]
fn small_or_foreign_keys_are_refused() {
    let pem = fixture("test-signing-key.pub");
    assert!(RepoKey::from_pem(b"not a key", "x").is_err());
    assert!(RepoKey::from_pem(&pem[..pem.len() / 2], "x").is_err());
    assert!(RepoKey::from_pem(&vec![b'A'; MAX_PEM_LEN + 1], "x").is_err());
    // A 1024 bit key, generated once with openssl for this test.
    let small = b"-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQC7VJTUt9Us8cKjMzEfYyjiWA4R
4/M2bS1GB4t7NXp98C3SC6dVMvDuictGeurT8jNbvJZHtCSuYEvuNMoSfm76oqFv
AvDRxg+4ntgcNsqYmqCz1NaXqnoh4tqfsgyCnhU8ArPv5omV2/YfEbbZDHHQubw4
nzo0iRNDRqDD9/J5lQIDAQAB
-----END PUBLIC KEY-----";
    assert!(matches!(RepoKey::from_pem(small, "x"), Err(RepoError::BadKey(text)) if text.contains("1024")));
}

#[test]
fn damaged_indexes_are_refused() {
    assert_eq!(parse_repodata(b"plain text"), Err(RepoError::NotZstd));
    let mut data = fixture("x86_64-repodata");
    data.truncate(data.len() / 2);
    assert!(parse_repodata(&data).is_err());
}

#[test]
fn hex_hashes_are_strict() {
    assert!(decode_hex32(&"ab".repeat(32)).is_some());
    for bad in ["", "ab", &"zz".repeat(32), &"ab".repeat(33), &format!("+{}", "a".repeat(63))] {
        assert!(decode_hex32(bad).is_none(), "{bad:?}");
    }
}
