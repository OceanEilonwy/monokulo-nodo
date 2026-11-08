//! The device's system update client. Runs as root.
//!
//! nodo-sysupdate check [options]   says whether the channel has a newer snapshot, changing nothing
//! nodo-sysupdate apply [options]   moves the system to it
//!
//! Options:
//!   --server URL   the update server, https:// or http://. Default: `server` in
//!                  /etc/nodo/system-update.toml
//!   --pins FILE    trusted keys, channel and floor. Default: /usr/share/nodo/snapshot-pins.toml
//!   --state DIR    where the last applied snapshot is recorded. Default: /var/lib/nodo/update
//!   --now SECONDS  treat the clock as synchronised at this time, for checking expiry. Without
//!                  it, expiry is checked only when nodo-healthd reports a synchronised clock.
//!
//! Plain http:// is allowed for the same reason as for feeds: the manifest is signed, the indexes
//! it names are pinned by hash, and every package is checked by xbps against them. Someone on
//! the network can still withhold updates until the manifest expires.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use nodo_install::http::Fetcher;
use nodo_update::client::{self, xbps_key_plist, xbps_repo_dir, Machine, Outcome, Source, XbpsKey};
use nodo_update::store::{load_toml, save_toml, DirStore};
use nodo_update::trust::{Accepted, Pins};
use nodo_update::TrustState;

const CONFIG: &str = "/etc/nodo/system-update.toml";
const PINS: &str = "/usr/share/nodo/snapshot-pins.toml";
const STATE_DIR: &str = "/var/lib/nodo/update";
const STATE_FILE: &str = "system-trust.toml";
const XBPS_DB: &str = "/var/db/xbps";
const HEALTH: &str = "/run/nodo/health.json";
const MAX_OUTPUT: usize = 4000;

struct Http {
    base: String,
    fetcher: Fetcher,
}

impl Source for Http {
    fn get(&self, path: &str, limit: u64) -> io::Result<Option<Vec<u8>>> {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.fetcher.get_optional(&format!("{}/{path}", self.base), name, limit)
    }
    fn repository_url(&self, path: &str) -> String {
        format!("{}/{path}", self.base)
    }
}

struct Device {
    now: Option<u64>,
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).chars().filter(|c| !c.is_control() || *c == '\n').collect::<String>();
    if out.status.success() {
        return Ok(text(&out.stdout));
    }
    let mut detail = format!("{}{}", text(&out.stdout), text(&out.stderr)).trim().to_owned();
    if detail.len() > MAX_OUTPUT {
        detail = detail[detail.len() - MAX_OUTPUT..].to_owned();
    }
    Err(format!("{program} failed: {detail}"))
}

/// Writes a file whole or not at all.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent directory"))?;
    fs::DirBuilder::new().recursive(true).mode(0o755).create(dir)?;
    let tmp = dir.join(format!(".{}.nodo-tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

/// `name-version_revision` split at the last dash, as xbps names packages.
fn split_pkgver(pkgver: &str) -> Option<(String, String)> {
    let (name, version) = pkgver.rsplit_once('-')?;
    version.contains('_').then(|| (name.to_owned(), version.to_owned()))
}

impl Machine for Device {
    fn arch(&self) -> io::Result<String> {
        let arch = run("xbps-uhelper", &["arch"]).map_err(io::Error::other)?.trim().to_owned();
        if arch.is_empty() {
            return Err(io::Error::other("xbps-uhelper arch printed nothing"));
        }
        Ok(arch)
    }

    fn clock_synced(&self) -> bool {
        if self.now.is_some() {
            return true;
        }
        fs::read(HEALTH)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|report| report.get("time")?.get("synced")?.as_bool())
            .unwrap_or(false)
    }

    fn now(&self) -> u64 {
        self.now.unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
    }

    fn has_key(&self, fingerprint: &str) -> bool {
        Path::new(XBPS_DB).join("keys").join(format!("{fingerprint}.plist")).is_file()
    }

    fn install_key(&self, key: &XbpsKey) -> io::Result<()> {
        let path = Path::new(XBPS_DB).join("keys").join(format!("{}.plist", key.fingerprint));
        write_atomic(&path, xbps_key_plist(key).as_bytes(), 0o644)
    }

    fn place_repodata(&self, url: &str, arch: &str, bytes: &[u8]) -> io::Result<()> {
        let path = Path::new(XBPS_DB).join(xbps_repo_dir(url)).join(format!("{arch}-repodata"));
        write_atomic(&path, bytes, 0o644)
    }

    fn installed(&self) -> io::Result<BTreeMap<String, String>> {
        let text = run("xbps-query", &["-l"]).map_err(io::Error::other)?;
        Ok(text.lines().filter_map(|line| line.split_whitespace().nth(1)).filter_map(split_pkgver).collect())
    }

    fn update(&self, urls: &[String]) -> Result<(), String> {
        // No -S: the indexes xbps uses are the ones verified above. --ignore-conf-repos keeps
        // every other repository out, while /etc/xbps.d still applies its noextract and
        // ignorepkg settings.
        let mut args = vec!["--ignore-conf-repos"];
        for url in urls {
            args.push("--repository");
            args.push(url);
        }
        args.push("--yes");
        args.push("--update");
        run("xbps-install", &args).map(drop)
    }

    fn restart_services(&self, packages: &[String]) -> Vec<String> {
        let mut restarted = Vec::new();
        for package in packages {
            let Ok(files) = run("xbps-query", &["-f", package]) else { continue };
            for line in files.lines() {
                let path = line.split(" -> ").next().unwrap_or(line).trim();
                let Some(name) = path.strip_prefix("/etc/sv/").and_then(|rest| rest.strip_suffix("/run")) else { continue };
                if name.contains('/') || !Path::new("/var/service").join(name).exists() {
                    continue;
                }
                let running = run("sv", &["status", name]).is_ok_and(|s| s.starts_with("run:"));
                if running && run("sv", &["-w", "60", "restart", name]).is_ok() {
                    restarted.push(name.to_owned());
                }
            }
        }
        restarted
    }
}

struct Options {
    command: String,
    server: Option<String>,
    pins: PathBuf,
    state: PathBuf,
    now: Option<u64>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let (command, mut rest) = args.split_first().ok_or("usage: nodo-sysupdate check|apply [--server URL] [--pins FILE] [--state DIR] [--now SECONDS]")?;
    if command != "check" && command != "apply" {
        return Err(format!("unknown command {command}"));
    }
    let mut options = Options { command: command.clone(), server: None, pins: PINS.into(), state: STATE_DIR.into(), now: None };
    while let [flag, value, tail @ ..] = rest {
        match flag.as_str() {
            "--server" => options.server = Some(value.clone()),
            "--pins" => options.pins = value.into(),
            "--state" => options.state = value.into(),
            "--now" => options.now = Some(value.parse().map_err(|_| "--now takes seconds since 1970")?),
            other => return Err(format!("unknown option {other}")),
        }
        rest = tail;
    }
    if !rest.is_empty() {
        return Err(format!("{} needs a value", rest[0]));
    }
    Ok(options)
}

fn server_address(options: &Options) -> Result<String, String> {
    let address = match &options.server {
        Some(server) => server.clone(),
        None => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Config {
                server: String,
            }
            let text = fs::read_to_string(CONFIG).map_err(|e| format!("no update server is set: {CONFIG}: {e}"))?;
            toml::from_str::<Config>(&text).map_err(|e| format!("{CONFIG}: {}", e.message()))?.server
        }
    };
    let address = address.trim_end_matches('/').to_owned();
    let usable = (address.starts_with("https://") || address.starts_with("http://"))
        && !address.chars().any(|c| c.is_control() || c.is_whitespace());
    if !usable {
        return Err(format!("the update server must be an https:// or http:// address, not {address:?}"));
    }
    Ok(address)
}

fn trust_state(options: &Options, store: &DirStore) -> Result<TrustState, String> {
    let text = fs::read_to_string(&options.pins)
        .map_err(|e| format!("no update signing keys are set on this device: {}: {e}", options.pins.display()))?;
    let pins: Pins = toml::from_str(&text).map_err(|e| format!("{}: {}", options.pins.display(), e.message()))?;
    let mut state = match load_toml::<TrustState>(store, STATE_FILE).map_err(|e| format!("{STATE_FILE}: {e}"))? {
        Some(state) => state,
        None => TrustState::from_pins(&pins),
    };
    state.raise_floor(&pins);
    Ok(state)
}

fn main_inner(args: &[String]) -> Result<(), String> {
    let options = parse(args)?;
    let base = server_address(&options)?;
    let store = DirStore::new(&options.state);
    let mut state = trust_state(&options, &store)?;
    let source = Http { base, fetcher: Fetcher::new().named("update") };
    let device = Device { now: options.now };
    if options.command == "check" {
        let (accepted, _, _) = client::check(&source, &device, &state).map_err(|e| e.to_string())?;
        match accepted {
            Accepted::Unchanged => println!("up to date: snapshot {} (serial {})", state.snapshot.as_deref().unwrap_or("?"), state.serial),
            Accepted::New(m) => {
                let restart = if m.needs_restart { ", needs a restart" } else { "" };
                println!("snapshot {} (serial {}) is available{restart}", m.snapshot, m.serial);
            }
        }
        return Ok(());
    }
    let outcome = client::apply(&source, &device, &mut state).map_err(|e| e.to_string())?;
    match outcome {
        Outcome::UpToDate { serial } => println!("up to date: serial {serial}"),
        Outcome::Applied { serial, snapshot, changed, restarted, needs_restart } => {
            save_toml(&store, STATE_FILE, &state).map_err(|e| format!("could not record the update: {e}"))?;
            println!("applied snapshot {snapshot} (serial {serial})");
            for line in &changed {
                println!("  updated {line}");
            }
            for name in &restarted {
                println!("  restarted {name}");
            }
            if needs_restart {
                println!("restart the device to finish this update");
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match main_inner(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nodo-sysupdate: {e}");
            ExitCode::FAILURE
        }
    }
}
