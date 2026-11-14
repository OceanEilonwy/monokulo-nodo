//! The dynamic DNS service. Usage: `nodo-ddns --config <path> [--once | --list-stale | --retire-stale]`.
//!
//! Records this service published for names that are no longer configured are only reported, by every
//! pass and by `--list-stale`. Deleting them is the separate, explicit `--retire-stale`, so a mistaken
//! or empty config can never remove DNS records by itself.
//!
//! The config holds provider credentials, so it must not be readable by group or others. Each zone is
//! one domain at one provider, with its own state file; the owner can have several providers:
//!
//! ```toml
//! ttl = 600
//! ipv6 = false
//! interval_secs = 300
//!
//! [[zones]]
//! domain = "example.com"
//! state_file = "/var/lib/nodo-ddns/example.com.json"
//! records = ["shop"]
//!
//! [zones.dns]
//! provider = "cloudflare"
//! api_token = "..."
//! ```
//!
//! The older single-zone form, with `domain`, `state_file`, `records` and `[dns]` at the top level, is
//! still read, so a config written before an update keeps working until the helper rewrites it.
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
struct ZoneConfig {
    domain: String,
    state_file: PathBuf,
    #[serde(default)]
    records: Vec<String>,
    dns: ProviderConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    zones: Vec<ZoneConfig>,
    // The single-zone form, read for configs written before several providers were supported.
    domain: Option<String>,
    state_file: Option<PathBuf>,
    #[serde(default)]
    records: Vec<String>,
    dns: Option<ProviderConfig>,
    #[serde(default = "default_ttl")]
    ttl: u32,
    #[serde(default)]
    ipv6: bool,
    #[serde(default = "default_interval")]
    interval_secs: u64,
}

impl Config {
    /// Every zone, with the single-zone form as one more. A zone named twice is refused, because two
    /// daemons would then fight over the same records.
    fn into_zones(self) -> Result<(Vec<ZoneConfig>, u32, bool, u64), String> {
        let mut zones = self.zones;
        match (self.domain, self.state_file, self.dns) {
            (Some(domain), Some(state_file), Some(dns)) => {
                zones.push(ZoneConfig { domain, state_file, records: self.records, dns });
            }
            (None, None, None) => {}
            _ => return Err("the config's single zone needs domain, state_file and [dns] together".to_owned()),
        }
        let mut seen = std::collections::BTreeSet::new();
        for zone in &zones {
            if !seen.insert(zone.domain.to_ascii_lowercase()) || zones.iter().filter(|z| z.state_file == zone.state_file).count() > 1 {
                return Err("a zone or state file appears twice in the config".to_owned());
            }
        }
        Ok((zones, self.ttl, self.ipv6, self.interval_secs))
    }
}

/// A key in the output: bare for a single zone, as before, and `domain: key` with several.
fn label(single: bool, domain: &str, key: String) -> String {
    if single {
        key
    } else {
        format!("{domain}: {key}")
    }
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

struct Zone {
    domain: String,
    provider: Box<dyn nodo_web::dns::DnsProvider>,
    store: FileStateStore,
    records: Vec<RecordName>,
}

fn run() -> Result<(), String> {
    let (config_path, mode) = parse_args()?;
    let (zone_configs, ttl, ipv6, interval_secs) = load_config(&config_path)?.into_zones()?;
    let http: SharedHttp = Arc::new(UreqClient::new());

    let mut zones = Vec::new();
    for zone in zone_configs {
        let records = zone
            .records
            .iter()
            .map(|name| RecordName::new(name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let provider = zone.dns.build(Arc::clone(&http), &zone.domain).map_err(|e| e.to_string())?;
        zones.push(Zone { domain: zone.domain, provider, store: FileStateStore { path: zone.state_file }, records });
    }
    let single = zones.len() == 1;

    let mut daemons = Vec::new();
    for zone in &zones {
        let daemon_config = DaemonConfig { records: zone.records.clone(), ttl, ipv6, sources: default_sources() };
        let (daemon, state_error) = Daemon::new(daemon_config, zone.provider.as_ref(), http.as_ref(), &zone.store, None);
        if let Some(error) = state_error {
            eprintln!("nodo-ddns: {}: starting from empty state: {error}", zone.domain);
        }
        daemons.push((zone.domain.as_str(), daemon));
    }

    if matches!(mode, Mode::ListStale) {
        let stale: Vec<String> = daemons
            .iter()
            .flat_map(|(domain, daemon)| daemon.stale().into_iter().map(|key| label(single, domain, key)))
            .collect();
        println!("{}", serde_json::json!({ "stale": stale }));
        return Ok(());
    }
    if matches!(mode, Mode::RetireStale) {
        let mut removed = Vec::new();
        for (domain, daemon) in &mut daemons {
            let keys = daemon.retire_stale().map_err(|e| format!("{domain}: {e}"))?;
            removed.extend(keys.into_iter().map(|key| label(single, domain, key)));
        }
        println!("{}", serde_json::json!({ "removed": removed }));
        return Ok(());
    }

    let interval = Duration::from_secs(interval_secs.max(MIN_INTERVAL_SECS));
    loop {
        for (domain, daemon) in &mut daemons {
            let report = daemon.tick(now_secs());
            eprintln!(
                "{}",
                serde_json::json!({
                    "domain": domain,
                    "outcome": report.outcome,
                    "changed": report.changes.iter().map(|c| &c.name).collect::<Vec<_>>(),
                    "errors": report.errors,
                    "warnings": report.warnings,
                    "stale": report.stale,
                })
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_config_forms() {
        let legacy = "domain = \"example.com\"\nstate_file = \"/s.json\"\nrecords = [\"shop\"]\n[dns]\nprovider = \"cloudflare\"\napi_token = \"t\"\n";
        let (zones, ttl, _, _) = toml::from_str::<Config>(legacy).unwrap().into_zones().unwrap();
        assert_eq!((zones.len(), zones[0].domain.as_str(), ttl), (1, "example.com", 600));

        let several = "ttl = 300\n[[zones]]\ndomain = \"a.com\"\nstate_file = \"/a.json\"\n[zones.dns]\nprovider = \"cloudflare\"\napi_token = \"t\"\n[[zones]]\ndomain = \"b.org\"\nstate_file = \"/b.json\"\nrecords = [\"@\"]\n[zones.dns]\nprovider = \"gandi\"\npersonal_access_token = \"g\"\n";
        let (zones, ttl, _, _) = toml::from_str::<Config>(several).unwrap().into_zones().unwrap();
        assert_eq!(zones.iter().map(|z| z.domain.as_str()).collect::<Vec<_>>(), ["a.com", "b.org"]);
        assert_eq!(ttl, 300);
    }

    #[test]
    fn a_repeated_zone_or_half_a_legacy_zone_is_refused() {
        let twice = "[[zones]]\ndomain = \"a.com\"\nstate_file = \"/a.json\"\n[zones.dns]\nprovider = \"cloudflare\"\napi_token = \"t\"\n[[zones]]\ndomain = \"A.com\"\nstate_file = \"/b.json\"\n[zones.dns]\nprovider = \"cloudflare\"\napi_token = \"t\"\n";
        assert!(toml::from_str::<Config>(twice).unwrap().into_zones().is_err());
        let half = "domain = \"a.com\"\n";
        assert!(toml::from_str::<Config>(half).unwrap().into_zones().is_err());
    }

    #[test]
    fn keys_are_labelled_only_with_several_zones() {
        assert_eq!(label(true, "a.com", "shop/A".to_owned()), "shop/A");
        assert_eq!(label(false, "a.com", "shop/A".to_owned()), "a.com: shop/A");
    }
}
