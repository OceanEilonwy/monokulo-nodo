//! A command line for the installer, for package authors and for testing on a device.
//!
//! The appliance itself reaches the installer through the root helper. This front end keeps its
//! state, the feed pins and the approved permissions, in one TOML file.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use nodo_feed::{KeyFingerprint, PinStore, Policy};
use nodo_install::{inspect_file, Installer, Outcome, Permissions, RealSystem, Request};
use serde::{Deserialize, Serialize};

const USAGE: &str = "usage:
  nodo-install inspect PACKAGE.xbps
  nodo-install add-feed STATE FEED FINGERPRINT
  nodo-install install STATE WORKDIR FEED PACKAGE [--approve]";

#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    pins: PinStore,
    /// Approved permissions by feed, then package.
    #[serde(default)]
    approved: BTreeMap<String, BTreeMap<String, Permissions>>,
}

fn load(path: &Path) -> Result<State, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn save(path: &Path, state: &State) -> Result<(), String> {
    let text = toml::to_string(state).map_err(|e| e.to_string())?;
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, path)).map_err(|e| format!("{}: {e}", path.display()))
}

fn inspect(path: &str) -> Result<ExitCode, String> {
    let (inspection, permissions) = inspect_file(Path::new(path), &Policy::default()).map_err(|e| format!("{path}: {e}"))?;
    if let Some(info) = &inspection.package {
        println!("{}-{} ({})", info.name, info.version, info.architecture);
        for dependency in &info.run_depends {
            println!("  depends on {dependency}");
        }
    }
    println!("{} entries, {} bytes uncompressed", inspection.entries, inspection.decompressed_bytes);
    for line in permissions.iter().flat_map(Permissions::summary) {
        println!("  permission: {line}");
    }
    for violation in &inspection.violations {
        println!("  violation: {violation}");
    }
    Ok(if inspection.is_clean() {
        println!("clean");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn add_feed(state_path: &str, feed: &str, fingerprint: &str) -> Result<ExitCode, String> {
    let state_path = Path::new(state_path);
    let mut state = load(state_path)?;
    let key = KeyFingerprint::new(fingerprint).map_err(|e| e.to_string())?;
    println!("{:?}", state.pins.add_feed(feed, &key));
    save(state_path, &state)?;
    Ok(ExitCode::SUCCESS)
}

fn install(state_path: &str, work: &str, feed: &str, package: &str, approve: bool) -> Result<ExitCode, String> {
    let state_path = Path::new(state_path);
    let mut state = load(state_path)?;
    let sys = RealSystem::new(work).map_err(|e| e.to_string())?;
    loop {
        let approved = state.approved.get(feed).and_then(|by_package| by_package.get(package)).cloned();
        let outcome = {
            let mut installer = Installer { sys: &sys, pins: &mut state.pins, policy: Policy::default() };
            installer.install(&Request { feed, package, approved: approved.as_ref() }).map_err(|e| e.to_string())?
        };
        match outcome {
            Outcome::Blocked(reasons) => {
                for reason in reasons {
                    println!("blocked: {reason:?}");
                }
                return Ok(ExitCode::FAILURE);
            }
            Outcome::NeedsApproval { pkgver, permissions, added } => {
                println!("{pkgver} asks for:");
                for line in permissions.summary() {
                    println!("  {line}");
                }
                for permission in &added {
                    println!("  new: {permission}");
                }
                if !approve {
                    println!("not installed: run again with --approve to accept exactly this");
                    return Ok(ExitCode::FAILURE);
                }
                state.approved.entry(feed.to_string()).or_default().insert(package.to_string(), permissions);
                save(state_path, &state)?;
            }
            Outcome::Installed { pkgver, .. } => {
                save(state_path, &state)?;
                println!("installed {pkgver}");
                return Ok(ExitCode::SUCCESS);
            }
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        ["inspect", path] => inspect(path),
        ["add-feed", state, feed, fingerprint] => add_feed(state, feed, fingerprint),
        ["install", state, work, feed, package] => install(state, work, feed, package, false),
        ["install", state, work, feed, package, "--approve"] => install(state, work, feed, package, true),
        _ => Err(USAGE.to_string()),
    };
    result.unwrap_or_else(|message| {
        eprintln!("nodo-install: {message}");
        ExitCode::from(2)
    })
}
