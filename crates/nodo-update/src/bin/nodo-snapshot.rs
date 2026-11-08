//! Publisher tool for snapshot manifests. Runs on the publisher's machine, never on a device.
//!
//! nodo-snapshot keygen <secret-key-file>
//! nodo-snapshot pubkey <secret-key-file>
//! nodo-snapshot manifest <channel> <serial> <snapshot> <valid-days> <needs-restart:yes|no> \
//!     <arch>[/<repo-name>]=<repo-path>=<repodata-file>... [pkg:<name>=<version>...]
//!     (the repository name is `void` unless given, and `nodo` for this project's packages)
//! nodo-snapshot sign manifest|keys <document> <secret-key-file>
//! nodo-snapshot verify <manifest> <pins.toml> <arch>

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use nodo_update::manifest::{PackagePin, RepoEntry, KEY_STATEMENT_CONTEXT, MANIFEST_CONTEXT};
use nodo_update::trust::Pins;
use nodo_update::{PublicKey, SignatureFile, SnapshotManifest, TrustState};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

fn read_key(path: &str) -> Result<SigningKey, String> {
    let text = Zeroizing::new(fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?);
    let mut seed = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(text.trim(), seed.as_mut()).map_err(|_| format!("{path}: not a secret key"))?;
    Ok(SigningKey::from_bytes(&seed))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn keygen(path: &str) -> Result<(), String> {
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(seed.as_mut()).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))?;
    let text = Zeroizing::new(hex::encode(seed.as_ref()));
    writeln!(file, "{}", text.as_str()).map_err(|e| e.to_string())?;
    println!("{}", PublicKey::from(&SigningKey::from_bytes(&seed)));
    Ok(())
}

fn manifest(args: &[String]) -> Result<(), String> {
    let [channel, serial, snapshot, days, restart, rest @ ..] = args else {
        return Err("manifest needs channel, serial, snapshot, valid-days, needs-restart and repos".into());
    };
    let serial: u64 = serial.parse().map_err(|_| "serial must be a number")?;
    let days: u64 = days.parse().map_err(|_| "valid-days must be a number")?;
    let needs_restart = match restart.as_str() {
        "yes" => true,
        "no" => false,
        _ => return Err("needs-restart is yes or no".into()),
    };
    let mut repo = Vec::new();
    let mut packages = Vec::new();
    for arg in rest {
        if let Some(pin) = arg.strip_prefix("pkg:") {
            let (name, version) = pin.split_once('=').ok_or("pkg:<name>=<version>")?;
            packages.push(PackagePin { name: name.into(), version: version.into() });
            continue;
        }
        let mut parts = arg.splitn(3, '=');
        let (Some(arch), Some(path), Some(file)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("{arg}: expected <arch>[/<repo-name>]=<repo-path>=<repodata-file>"));
        };
        let index = fs::read(file).map_err(|e| format!("{file}: {e}"))?;
        let (arch, name) = arch.split_once('/').unwrap_or((arch, nodo_update::manifest::VOID_REPO));
        repo.push(RepoEntry {
            arch: arch.into(),
            name: name.into(),
            path: path.into(),
            repodata_sha256: hex::encode(Sha256::digest(&index)),
        });
    }
    let published = now();
    let m = SnapshotManifest {
        format: 1,
        channel: channel.clone(),
        serial,
        snapshot: snapshot.clone(),
        published,
        expires: published + days * 86_400,
        needs_restart,
        repo,
        packages,
    };
    m.validate().map_err(|e| e.to_string())?;
    print!("{}", m.to_toml());
    Ok(())
}

fn sign(kind: &str, doc: &str, key: &str) -> Result<(), String> {
    let context = match kind {
        "manifest" => MANIFEST_CONTEXT,
        "keys" => KEY_STATEMENT_CONTEXT,
        _ => return Err("sign manifest|keys".into()),
    };
    let body = fs::read(doc).map_err(|e| format!("{doc}: {e}"))?;
    match kind {
        "manifest" => drop(SnapshotManifest::parse(&body).map_err(|e| e.to_string())?),
        _ => drop(nodo_update::KeyStatement::parse(&body).map_err(|e| e.to_string())?),
    }
    let sig_path = format!("{doc}.sig");
    let mut sigs = match fs::read(&sig_path) {
        Ok(bytes) => SignatureFile::parse(&bytes).map_err(|e| e.to_string())?,
        Err(_) => SignatureFile::default(),
    };
    sigs.sign(context, &body, &read_key(key)?);
    fs::write(&sig_path, sigs.to_toml()).map_err(|e| format!("{sig_path}: {e}"))
}

fn verify(doc: &str, pins: &str, arch: &str) -> Result<(), String> {
    let body = fs::read(doc).map_err(|e| format!("{doc}: {e}"))?;
    let sig = fs::read(format!("{doc}.sig")).map_err(|e| format!("{doc}.sig: {e}"))?;
    let pins: Pins = toml::from_str(&fs::read_to_string(pins).map_err(|e| format!("{pins}: {e}"))?)
        .map_err(|e| e.to_string())?;
    let state = TrustState::from_pins(&pins);
    state.check(&body, &sig, arch, Some(now())).map_err(|e| e.to_string())?;
    println!("ok");
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen", path] => keygen(path),
        ["pubkey", path] => read_key(path).map(|k| println!("{}", PublicKey::from(&k))),
        ["manifest", ..] => manifest(&args[1..]),
        ["sign", kind, doc, key] => sign(kind, doc, key),
        ["verify", doc, pins, arch] => verify(doc, pins, arch),
        _ => Err("usage: nodo-snapshot keygen|pubkey|manifest|sign|verify ...".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nodo-snapshot: {e}");
            ExitCode::FAILURE
        }
    }
}
