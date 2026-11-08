//! The dynamic DNS service. Usage: `nodo-ddns --config <path> [--once | --list-stale | --retire-stale]`.
//!
//! Records this service published for names that are no longer configured are only reported, by every
//! pass and by `--list-stale`. Deleting them is the separate, explicit `--retire-stale`, so a mistaken
//! or empty config can never remove DNS records by itself.
//!
//! The config holds provider credentials, so it must not be readable by group or others:
//!
//! ```toml
//! domain = "example.com"
//! state_file = "/var/lib/nodo-ddns/state.json"
//! records = ["shop"]
//! ttl = 600
//! ipv6 = false
//! interval_secs = 300
//!
//! [dns]
//! provider = "cloudflare"
//! api_token = "..."
//! ```
//!
//! Log lines hold outcomes and scrubbed errors, but not the home IP.

use nodo_web::daemon::{Daemon, DaemonConfig, FileStateStore};
use nodo_web::detect::default_sources;
use nodo_web::dns::config::ProviderConfig;
use nodo_web::dns::RecordName;
use nodo_web::http::SharedHttp;
use nodo_web::net::UreqClient;
use serde::Deserialize;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MIN_INTERVAL_SECS: u64 = 60;

fn default_ttl() -> u32 {
    600
}

fn default_interval() -> u64 {
    300
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    domain: String,
    state_file: PathBuf,
    #[serde(default)]
    records: Vec<String>,
    #[serde(default = "default_ttl")]
    ttl: u32,
    #[serde(default)]
    ipv6: bool,
    #[serde(default = "default_interval")]
    interval_secs: u64,
    dns: ProviderConfig,
}

enum Mode {
    Forever,
    Once,
    ListStale,
    RetireStale,
}

fn parse_args() -> Result<(PathBuf, Mode), String> {
    let mut config = None;
    let mut mode = Mode::Forever;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = args.next().map(PathBuf::from),
            "--once" => mode = Mode::Once,
            "--list-stale" => mode = Mode::ListStale,
            "--retire-stale" => mode = Mode::RetireStale,
            _ => return Err(format!("unknown argument: {}", arg.escape_default())),
        }
    }
    let config = config.ok_or("usage: nodo-ddns --config <path> [--once | --list-stale | --retire-stale]")?;
    Ok((config, mode))
}

fn load_config(path: &Path) -> Result<Config, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("cannot read the config: {e}"))?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err("the config holds credentials, so it must not be readable by group or others".to_owned());
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read the config: {e}"))?;
    // A TOML error can quote the offending line, which may be a credential, so only the position is kept.
    toml::from_str(&text).map_err(|e: toml::de::Error| match e.span() {
        Some(span) => format!("the config is not valid, near byte {}", span.start),
        None => "the config is not valid".to_owned(),
    })
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

fn run() -> Result<(), String> {
    let (config_path, mode) = parse_args()?;
    let config = load_config(&config_path)?;

    let records = config
        .records
        .iter()
        .map(|name| RecordName::new(name))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let http: SharedHttp = Arc::new(UreqClient::new());
    let provider = config.dns.build(Arc::clone(&http), &config.domain).map_err(|e| e.to_string())?;
    let store = FileStateStore { path: config.state_file };
    let daemon_config = DaemonConfig { records, ttl: config.ttl, ipv6: config.ipv6, sources: default_sources() };

    let (mut daemon, state_error) = Daemon::new(daemon_config, provider.as_ref(), http.as_ref(), &store, None);
    if let Some(error) = state_error {
        eprintln!("nodo-ddns: starting from empty state: {error}");
    }

    if matches!(mode, Mode::ListStale) {
        println!("{}", serde_json::json!({ "stale": daemon.stale() }));
        return Ok(());
    }
    if matches!(mode, Mode::RetireStale) {
        let removed = daemon.retire_stale().map_err(|e| e.to_string())?;
        println!("{}", serde_json::json!({ "removed": removed }));
        return Ok(());
    }

    let interval = Duration::from_secs(config.interval_secs.max(MIN_INTERVAL_SECS));
    loop {
        let report = daemon.tick(now_secs());
        eprintln!(
            "{}",
            serde_json::json!({
                "outcome": report.outcome,
                "changed": report.changes.iter().map(|c| &c.name).collect::<Vec<_>>(),
                "errors": report.errors,
                "warnings": report.warnings,
                "stale": report.stale,
            })
        );
        if matches!(mode, Mode::Once) {
            return Ok(());
        }
        std::thread::sleep(interval);
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("nodo-ddns: {message}");
            ExitCode::from(2)
        }
    }
}
