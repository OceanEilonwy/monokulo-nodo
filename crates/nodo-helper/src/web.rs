//! Web hosting: turns the saved exposure into Caddy, nftables, Tor, mDNS and dynamic DNS state.
//!
//! Generation is `nodo_web`'s and is pure. This module reads the inputs, writes the results with the
//! right owners, reloads each service, and puts the old files back when a reload fails. Side effects
//! go through [`WebIo`], so the logic runs in tests without root.
//!
//! DNS provider credentials are sealed under the device key (see `crate::secrets`) in a root-only
//! file, so they cannot be read while the device is locked. nodo-ddns gets them in a config under
//! `/run/nodo/secrets`, which is memory only, so after a reboot it waits until the device is unlocked.
//!
//! Certificates come from Let's Encrypt through Caddy. The helper picks the challenge: DNS-01 when
//! the provider has a certificate module and its credentials are stored, otherwise HTTP-01 and
//! TLS-ALPN-01, which need ports 80 and 443 forwarded. The DNS-01 credentials are opened from the
//! sealed store into the Caddy config, which only root and Caddy can read. See `certs` for the
//! certificate status and the local names under the owner's domain.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use nodo_api::{
    AcmeSettingsInfo, AcmeSettingsInput, ApiError, CertificateReport, CheckStatus, Dns01Support,
    DnsProviderDetail, LocalNameCheck, ReachabilityDetail, ReachabilityItem, Secret, ServiceState,
    SiteId, WebSiteStatus, WebStatus,
};
use nodo_web::caddy::{self, AddressKind, DomainSettings, Site, WebPlan};
use nodo_web::dns::catalog::{self, Dns01, ProviderStatus};
use nodo_web::dns::config::ProviderConfig;
use nodo_web::dns::{DnsProvider, RecordName, Zone};
use nodo_web::http::SharedHttp;
use nodo_web::local::{self, LocalAddress};
use nodo_web::reachability::{self, CheckId, LanInfo, NoExternalVantage, ReachabilityInputs, Verdict};
use nodo_web::router::{self, GatewayQuery};
use serde::{Deserialize, Serialize};

use crate::exposure::{ExposureApplier, WebInput};
use crate::secrets::{self, SecretsKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebPaths {
    pub caddy_config: PathBuf,
    pub caddy_admin: PathBuf,
    pub nft_fragment: PathBuf,
    pub nft_main: PathBuf,
    pub ddns_config: PathBuf,
    pub ddns_state: PathBuf,
    /// The connected providers, their domains and sealed credentials. Root only. See `providers`.
    pub dns_credentials: PathBuf,
    pub mdns_names: PathBuf,
    pub localnet_state: PathBuf,
    pub torrc: PathBuf,
    pub tor_sv_conf: PathBuf,
    pub tor_dir: PathBuf,
    /// The owner's certificate choices. Root only.
    pub acme_settings: PathBuf,
    /// Points the helper at a test ACME server instead of Let's Encrypt. Only tests create it.
    pub acme_test: PathBuf,
    /// Caddy's certificate storage: certificates, ACME accounts and the local CA.
    pub caddy_storage: PathBuf,
    pub tls_events: PathBuf,
    /// The local names this helper published at the DNS provider, so stale ones can be removed.
    pub local_records: PathBuf,
    /// The local CA trust page and root files, which Caddy serves on the local address.
    pub trust_dir: PathBuf,
    /// The local address that Caddy, the firewall, mDNS and the records were last applied for.
    pub local_applied: PathBuf,
}

impl Default for WebPaths {
    fn default() -> Self {
        WebPaths {
            caddy_config: "/var/lib/nodo-web/caddy.json".into(),
            caddy_admin: "/run/nodo-web/caddy-admin.sock".into(),
            nft_fragment: "/etc/nftables.d/nodo-web.nft".into(),
            nft_main: "/etc/nftables.conf".into(),
            ddns_config: "/run/nodo/secrets/nodo-ddns/config.toml".into(),
            ddns_state: "/var/lib/nodo-ddns/state.json".into(),
            // Inside the secrets directory, so backups carry it.
            dns_credentials: "/var/lib/nodo/secrets/web-dns.toml".into(),
            mdns_names: "/var/lib/nodo-localnet/mdns-names".into(),
            localnet_state: local::STATE_FILE.into(),
            torrc: "/var/lib/nodo-web/torrc".into(),
            tor_sv_conf: "/etc/sv/tor/conf".into(),
            tor_dir: "/var/lib/tor/nodo".into(),
            // Beside the DNS credentials, so backups carry it.
            acme_settings: "/var/lib/nodo/secrets/web-acme.toml".into(),
            acme_test: "/etc/nodo/acme-test.toml".into(),
            caddy_storage: "/var/lib/caddy/data/caddy".into(),
            tls_events: caddy::TLS_EVENTS_LOG.into(),
            local_records: "/var/lib/nodo-web/local-records.json".into(),
            trust_dir: nodo_web::trust::TRUST_DIR.into(),
            local_applied: "/var/lib/nodo-web/local-address".into(),
        }
    }
}

/// File modes and owners, as `(mode, user, group)`.
pub type Owner<'a> = (u32, &'a str, &'a str);

/// The side effects of applying web state.
pub trait WebIo: Send + Sync {
    fn read(&self, path: &Path) -> Option<Vec<u8>>;
    fn write(&self, path: &Path, bytes: &[u8], owner: Owner) -> Result<(), String>;
    fn remove(&self, path: &Path) -> Result<(), String>;
    fn ensure_dir(&self, path: &Path, owner: Owner) -> Result<(), String>;
    /// Checks the whole ruleset, then loads it.
    fn nft_reload(&self, main: &Path) -> Result<(), String>;
    /// Loads a config into a running Caddy. `Ok(false)` when Caddy is not running.
    fn caddy_load(&self, socket: &Path, json: &[u8]) -> Result<bool, String>;
    fn service_installed(&self, name: &str) -> bool;
    fn service_running(&self, name: &str) -> bool;
    /// Enables or disables a runit service. Enabling an already running one restarts it when `restart`.
    fn service(&self, name: &str, up: bool, restart: bool) -> Result<(), String>;
    /// Runs `nodo-ddns --retire-stale` as its service user, returning its standard output.
    fn retire_stale(&self, config: &Path) -> Result<String, String>;
    fn http(&self) -> SharedHttp;
    fn sleep(&self, duration: Duration);
    fn random(&self, out: &mut [u8]) -> Result<(), String>;
    /// File names in a directory, empty when it cannot be read.
    fn list_dir(&self, path: &Path) -> Vec<String>;
    fn remove_dir_all(&self, path: &Path) -> Result<(), String>;
    /// Looks a name up through this device's resolver. `None` when the lookup failed.
    fn resolve(&self, host: &str) -> Option<Vec<IpAddr>>;
    /// Unix seconds.
    fn now(&self) -> u64;
    /// A GET on Caddy's admin API. `None` when Caddy is not running or answers with an error.
    fn caddy_get(&self, socket: &Path, path: &str) -> Option<Vec<u8>>;
    fn dns_provider(&self, config: ProviderConfig, domain: &str) -> Result<Box<dyn DnsProvider>, String>;
    /// The domains the credentials control, or `None` when the provider cannot list them.
    fn list_domains(&self, config: ProviderConfig) -> Result<Option<Vec<String>>, String>;
    /// The home's public IPv4 address, as the IP echo services see it.
    fn detect_public_ip(&self) -> Option<IpAddr>;
}

/// The stored domain and credentials, in the same shape as nodo-ddns's `[dns]` table. Every value
/// other than `provider` is sealed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Credentials {
    domain: String,
    dns: BTreeMap<String, String>,
    /// DNS-01 credentials for a provider whose certificate module needs its own, sealed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    dns01: BTreeMap<String, String>,
}

/// nodo-ddns's config file: one zone per owned domain that has public records.
#[derive(Serialize)]
struct DdnsConfig {
    ttl: u32,
    ipv6: bool,
    interval_secs: u64,
    zones: Vec<DdnsZone>,
}

#[derive(Serialize)]
struct DdnsZone {
    domain: String,
    state_file: String,
    records: Vec<String>,
    dns: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct DdnsState {
    #[serde(default)]
    published: BTreeMap<String, serde_json::Value>,
    observed_v4: Option<String>,
}

const CADDY: &str = "nodo-caddy";
const DDNS: &str = "nodo-ddns";
const MDNS: &str = "nodo-mdns";
const LOCALNET: &str = "nodo-localnet";
const TOR: &str = "tor";
const ROOT_ONLY: Owner = (0o600, "root", "root");
const TORRC_HEADER: &str =
    "# Generated by nodo-helper. Changes are overwritten.\nUser tor\nDataDirectory /var/lib/tor\nSocksPort 0\n";
const PUBLIC_FILE: Owner = (0o644, "root", "root");
/// Sealed values are bound to this name and the field, so they open nowhere else.
const SEAL_PACKAGE: &str = "nodo-ddns";
const SEAL_DNS01: &str = "nodo-caddy";
const SECRETS_DIR: Owner = (0o711, "root", "root");
/// How long to wait for a new local address or a new onion address before generating without it.
const SETTLE_SECS: u64 = 20;

pub struct NodoWeb<I: WebIo> {
    pub paths: WebPaths,
    pub io: I,
}

fn field_names(id: &str) -> Vec<String> {
    let names: &[&str] = match id {
        "porkbun" => &["api_key", "secret_key"],
        "cloudflare" => &["api_token"],
        "godaddy" | "gandi" => &["personal_access_token"],
        "namecom" => &["username", "api_token"],
        "alibaba" => &["access_key_id", "access_key_secret"],
        "hostinger" => &["api_token"],
        _ => &[],
    };
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn hostname_ok(host: &str) -> bool {
    nodo_web::names::is_onion_name(host)
}

impl<I: WebIo> NodoWeb<I> {
    pub fn new(paths: WebPaths, io: I) -> Self {
        NodoWeb { paths, io }
    }

    /// The first connected provider with its first domain. The single-provider views (the
    /// certificate settings, the local names and the older requests) address this one.
    fn credentials(&self) -> Option<Credentials> {
        let (store, _) = self.provider_store();
        let provider = store.providers.iter().find(|p| !p.domains.is_empty())?;
        Some(Credentials { domain: provider.domains[0].clone(), dns: provider.dns.clone(), dns01: provider.dns01.clone() })
    }

    /// The provider table with every sealed value opened.
    fn opened(&self, credentials: &Credentials, key: &SecretsKey) -> Result<BTreeMap<String, String>, String> {
        credentials
            .dns
            .iter()
            .map(|(name, value)| {
                if name == "provider" {
                    Ok((name.clone(), value.clone()))
                } else {
                    secrets::open(key, SEAL_PACKAGE, name, value).map(|v| (name.clone(), v.to_string()))
                }
            })
            .collect()
    }

    fn local_address(&self) -> LocalAddress {
        self.io
            .read(&self.paths.localnet_state)
            .and_then(|b| String::from_utf8(b).ok())
            .map(|text| local::parse_state(&text))
            .unwrap_or_default()
    }

    fn onion_host(&self, package: &str, site: &str) -> Option<String> {
        let path = self.paths.tor_dir.join(format!("{package}.{site}")).join("hostname");
        let text = String::from_utf8(self.io.read(&path)?).ok()?;
        let host = text.trim().to_owned();
        hostname_ok(&host).then_some(host)
    }

    fn settings(&self, key: Option<&SecretsKey>) -> DomainSettings {
        let stored = self.acme_stored();
        DomainSettings {
            domain: self.credentials().map(|c| c.domain),
            local_address: self.local_address(),
            tor_dir: self.paths.tor_dir.to_string_lossy().into_owned(),
            acme_email: stored.email.clone(),
            zone_challenges: self.zone_challenges(key),
            acme: self.acme_config(&stored),
            local_domain: stored.local_domain,
            trust_dir: self.paths.trust_dir.to_string_lossy().into_owned(),
            ..DomainSettings::default()
        }
    }

    fn plan(&self, input: &WebInput) -> Result<WebPlan, String> {
        let sites: Vec<Site> = input
            .sites
            .iter()
            .map(|s| Site {
                package: s.package.clone(),
                service: s.service.clone(),
                exposure: s.exposure,
                // A stopped upstream already gets the unavailable page through Caddy's error routes.
                state: if s.locked { ServiceState::Locked } else { ServiceState::Running },
                local_host: None,
                public_subdomain: None,
                public_host: s.public_host.clone(),
                onion_host: self.onion_host(&s.package, &s.service.name),
                // Rate limiting is always on for public sites.
                rate_limit: true,
            })
            .collect();
        let mut plan = caddy::generate(&sites, &self.settings(input.secrets.as_deref())).map_err(|e| e.to_string())?;
        self.add_node_onion(&mut plan, input);
        Ok(plan)
    }

    /// The DNS record names for public hosts in `domain`: the hosts whose longest connected domain
    /// is this one.
    fn ddns_records(&self, plan: &WebPlan, domain: &str, store: &providers::ProviderStore) -> Vec<String> {
        let mut records: Vec<String> = plan
            .addresses
            .iter()
            .filter(|a| a.kind == AddressKind::Public)
            .filter(|a| store.zone_of(&a.host).is_some_and(|(_, d)| d == domain))
            .filter_map(|a| {
                if a.host == domain {
                    Some("@".to_owned())
                } else {
                    a.host.strip_suffix(&format!(".{domain}")).map(str::to_owned)
                }
            })
            .filter(|name| RecordName::new(name).is_ok())
            .collect();
        records.sort();
        records.dedup();
        records
    }

    fn wait_for(&self, ready: impl Fn() -> bool) {
        for _ in 0..SETTLE_SECS {
            if ready() {
                return;
            }
            self.io.sleep(Duration::from_secs(1));
        }
    }

    fn apply_tor(&self, plan: &WebPlan) -> Result<(), String> {
        if plan.tor_services.is_empty() {
            // Without this Tor would go on publishing services that were turned off.
            return self.withdraw_tor();
        }
        if !self.io.service_installed(TOR) {
            return Ok(());
        }
        // Tor creates each service directory itself, inside a parent only it may enter.
        self.io.ensure_dir(&self.paths.tor_dir, (0o700, "tor", "tor"))?;
        let mut torrc = String::from(TORRC_HEADER);
        for line in &plan.tor_lines {
            torrc.push_str(line);
            torrc.push('\n');
        }
        let changed = self.io.read(&self.paths.torrc).as_deref() != Some(torrc.as_bytes());
        self.io.write(&self.paths.torrc, torrc.as_bytes(), PUBLIC_FILE)?;
        let conf = format!("OPTS=\"--quiet -f {}\"\n", self.paths.torrc.display());
        self.io.write(&self.paths.tor_sv_conf, conf.as_bytes(), PUBLIC_FILE)?;
        self.io.service(TOR, true, changed)
    }

    fn apply_files(&self, plan: &WebPlan) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(&plan.caddy).map_err(|e| e.to_string())?;
        let nft = caddy::render_nft(&plan.firewall);
        let old_json = self.io.read(&self.paths.caddy_config);
        let old_nft = self.io.read(&self.paths.nft_fragment);
        let restore = |io: &I| {
            let put = |path: &Path, old: &Option<Vec<u8>>, owner| match old {
                Some(bytes) => io.write(path, bytes, owner),
                None => io.remove(path),
            };
            let _ = put(&self.paths.caddy_config, &old_json, (0o640, "root", "_caddy"));
            let _ = put(&self.paths.nft_fragment, &old_nft, PUBLIC_FILE);
            let _ = io.nft_reload(&self.paths.nft_main);
        };

        if let Some(dir) = self.paths.caddy_config.parent() {
            self.io.ensure_dir(dir, (0o750, "root", "_caddy"))?;
        }
        // Provider credentials can appear in the config for DNS-01, so only Caddy may read it.
        self.io.write(&self.paths.caddy_config, &json, (0o640, "root", "_caddy"))?;
        self.io.write(&self.paths.nft_fragment, nft.as_bytes(), PUBLIC_FILE)?;
        if let Err(e) = self.io.nft_reload(&self.paths.nft_main) {
            restore(&self.io);
            return Err(format!("the firewall rejected the rules: {e}"));
        }
        let serving = !plan.caddy.apps.http.servers.is_empty();
        match self.io.caddy_load(&self.paths.caddy_admin, &json) {
            Ok(true) => {}
            Ok(false) if serving => self.io.service(CADDY, true, false)?,
            Ok(false) => {}
            Err(e) => {
                restore(&self.io);
                return Err(format!("Caddy refused the config: {e}"));
            }
        }
        if !serving && self.io.service_running(CADDY) {
            self.io.service(CADDY, false, false)?;
        }
        Ok(())
    }

    /// The responder reads the address when it starts, so a new address restarts it, like new names.
    fn apply_mdns(&self, plan: &WebPlan, moved: bool) -> Result<(), String> {
        let ready = matches!(plan.local.address, LocalAddress::Ready { .. });
        if ready && !plan.local.mdns_names.is_empty() {
            let names = plan.local.mdns_names.join("\n") + "\n";
            let changed = self.io.read(&self.paths.mdns_names).as_deref() != Some(names.as_bytes());
            self.io.write(&self.paths.mdns_names, names.as_bytes(), PUBLIC_FILE)?;
            self.io.service(MDNS, true, changed || moved)
        } else if self.io.service_installed(MDNS) {
            self.io.service(MDNS, false, false)
        } else {
            Ok(())
        }
    }

    fn apply_ddns(&self, plan: &WebPlan, key: Option<&SecretsKey>) -> Result<(), String> {
        let (store, _) = self.provider_store();
        let wanted: Vec<(&providers::StoredProvider, &str, Vec<String>)> = store
            .owned()
            .into_iter()
            .map(|(provider, domain)| (provider, domain, self.ddns_records(plan, domain, &store)))
            .filter(|(_, _, records)| !records.is_empty())
            .collect();
        if wanted.is_empty() {
            // No public site, so the home IP has no business being looked up or published.
            if self.io.service_installed(DDNS) {
                self.io.service(DDNS, false, false)?;
            }
            self.io.remove(&self.paths.ddns_config)?;
            return Ok(());
        }
        let Some(key) = key else {
            // Locked: the credentials cannot be opened, so nodo-ddns waits for its config.
            return self.io.service(DDNS, true, false);
        };
        let mut zones = Vec::new();
        for (provider, domain, records) in wanted {
            zones.push(DdnsZone {
                domain: domain.to_owned(),
                state_file: self.zone_state(domain).to_string_lossy().into_owned(),
                records,
                dns: self.open_provider(provider, key)?,
            });
        }
        let config = DdnsConfig { ttl: 600, ipv6: false, interval_secs: 300, zones };
        if let Some(dir) = self.paths.ddns_config.parent() {
            if let Some(parent) = dir.parent() {
                self.io.ensure_dir(parent, SECRETS_DIR)?;
            }
            self.io.ensure_dir(dir, (0o700, "_nododdns", "_nododdns"))?;
        }
        let text = toml::to_string(&config).map_err(|e| e.to_string())?;
        let changed = self.io.read(&self.paths.ddns_config).as_deref() != Some(text.as_bytes());
        // nodo-ddns refuses a config that group or others can read.
        self.io.write(&self.paths.ddns_config, text.as_bytes(), (0o600, "_nododdns", "_nododdns"))?;
        self.io.service(DDNS, true, changed)
    }

    fn status_from(&self, plan: &WebPlan, input: &WebInput) -> WebStatus {
        let sites = input
            .sites
            .iter()
            .map(|s| {
                let url = |kind| {
                    plan.addresses
                        .iter()
                        .find(|a| a.package == s.package && a.site == s.service.name && a.kind == kind)
                        .map(|a| a.url.clone())
                };
                let is = |list: &[caddy::SiteRef]| {
                    list.iter().any(|r| r.package == s.package && r.site == s.service.name)
                };
                WebSiteStatus {
                    id: SiteId { package: s.package.clone(), site: s.service.name.clone() },
                    local_url: url(AddressKind::Local),
                    onion_url: url(AddressKind::Onion),
                    public_url: url(AddressKind::Public),
                    local_waiting: is(&plan.local.waiting),
                    rate_limit_unenforced: is(&plan.rate_limit_unenforced),
                }
            })
            .collect();
        let credentials = self.credentials();
        let (store, _) = self.provider_store();
        // Every owned domain has its own state file. With several, stale keys name their domain, as
        // nodo-ddns's own output does.
        let owned = store.owned();
        let single = owned.len() == 1;
        let mut stale_records = Vec::new();
        let mut changed = None;
        let mut observed_v4 = None;
        for (_, domain) in &owned {
            let state: DdnsState = self
                .io
                .read(&self.zone_state(domain))
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let records = self.ddns_records(plan, domain, &store);
            for (key, record) in &state.published {
                let current = record.get("name").and_then(|n| n.as_str()).is_some_and(|n| records.iter().any(|r| r == n));
                if !current {
                    stale_records.push(if single { key.clone() } else { format!("{domain}: {key}") });
                }
            }
            changed = changed.max(state.published.values().filter_map(|r| r.get("changed_at")?.as_u64()).max());
            observed_v4 = observed_v4.or(state.observed_v4);
        }
        let (local_address, local_unavailable) = match &plan.local.address {
            LocalAddress::Ready { address } => (Some(address.to_string()), plan.local.names_problem.map(str::to_owned)),
            LocalAddress::Unavailable { reason } => {
                (None, caddy::local_problem(plan).map(|_| reason.summary()))
            }
        };
        WebStatus {
            sites,
            local_address,
            local_unavailable,
            local_remedy: plan.local.remedy.map(str::to_owned),
            domain: credentials.as_ref().map(|c| c.domain.clone()),
            provider: credentials.and_then(|c| c.dns.get("provider").cloned()),
            ddns_running: self.io.service_running(DDNS),
            public_ip: observed_v4,
            dns_changed_at: changed,
            stale_records,
        }
    }

    fn provider(&self, key: Option<&SecretsKey>) -> Result<Box<dyn DnsProvider>, ApiError> {
        let credentials =
            self.credentials().ok_or_else(|| ApiError::Failed("no DNS provider is set up".to_owned()))?;
        let key = key.ok_or_else(|| ApiError::Locked("unlock the device to use the DNS provider".to_owned()))?;
        let dns = self.opened(&credentials, key).map_err(ApiError::Failed)?;
        let text = toml::to_string(&dns).map_err(|e| ApiError::Failed(e.to_string()))?;
        let config: ProviderConfig =
            toml::from_str(&text).map_err(|_| ApiError::Failed("the stored provider settings are not valid".to_owned()))?;
        self.io.dns_provider(config, &credentials.domain).map_err(ApiError::Failed)
    }
}

fn check_id(id: CheckId) -> &'static str {
    match id {
        CheckId::DomainAndCredentials => "domain_and_credentials",
        CheckId::PublicIp => "public_ip",
        CheckId::RouterAddress => "router_address",
        CheckId::TemporaryRecord => "temporary_record",
        CheckId::Port80 => "port_80",
        CheckId::Port443 => "port_443",
    }
}

/// The address this device uses towards the gateway. Connecting a UDP socket sends nothing.
fn device_address(gateway: Ipv4Addr) -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((gateway, 9)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

impl<I: WebIo> ExposureApplier for NodoWeb<I> {
    fn apply(&self, input: &WebInput) -> Result<(), String> {
        self.migrate_providers()?;
        let wants_local = input.sites.iter().any(|s| s.exposure.local);
        if self.io.service_installed(LOCALNET) {
            self.io.service(LOCALNET, wants_local, false)?;
            if wants_local {
                self.wait_for(|| matches!(self.local_address(), LocalAddress::Ready { .. }));
            }
        }
        let mut plan = self.plan(input)?;
        self.apply_tor(&plan)?;
        if !plan.onion_pending.is_empty() && self.io.service_running(TOR) {
            let pending = plan.onion_pending.clone();
            self.wait_for(|| pending.iter().all(|r| self.onion_host(&r.package, &r.site).is_some()));
            plan = self.plan(input)?;
        }
        if input.node_onion.is_some() && self.io.service_running(TOR) {
            self.wait_for(|| self.node_onion_host().is_some());
        }
        let moved = localsite::served_address(&plan) != self.applied_address();
        self.apply_files(&plan)?;
        self.apply_mdns(&plan, moved)?;
        self.apply_trust(&plan, true)?;
        self.apply_local_names(&plan, input.secrets.as_deref());
        self.record_applied(&plan)?;
        self.apply_ddns(&plan, input.secrets.as_deref())
    }

    fn unlocked(&self, input: &WebInput) -> Result<(), String> {
        self.migrate_providers()?;
        let plan = self.plan(input)?;
        // The config written while locked may lack DNS-01, which only now can be opened.
        let json = serde_json::to_vec_pretty(&plan.caddy).map_err(|e| e.to_string())?;
        if self.io.read(&self.paths.caddy_config).as_deref() != Some(json.as_slice()) {
            self.apply_files(&plan)?;
        }
        // The address may have moved while locked and before the periodic check noticed.
        if localsite::served_address(&plan) != self.applied_address() {
            self.apply_mdns(&plan, true)?;
            self.apply_trust(&plan, false)?;
            self.record_applied(&plan)?;
        }
        // Records that waited for the credentials since the address changed are written now.
        self.apply_local_names(&plan, input.secrets.as_deref());
        self.apply_ddns(&plan, input.secrets.as_deref())
    }

    fn local_address_changed(&self, input: &WebInput) -> Result<bool, String> {
        self.refresh_local(input)
    }

    fn local_site_status(&self, input: &WebInput) -> Result<nodo_api::LocalSiteStatus, ApiError> {
        NodoWeb::local_site_status(self, input)
    }

    fn onion_addresses(&self, input: &WebInput) -> Result<crate::onion::OnionAddresses, ApiError> {
        NodoWeb::onion_addresses(self, input)
    }

    fn status(&self, input: &WebInput) -> Result<WebStatus, ApiError> {
        let plan = self.plan(input).map_err(ApiError::Failed)?;
        Ok(self.status_from(&plan, input))
    }

    fn providers(&self) -> Result<Vec<DnsProviderDetail>, ApiError> {
        let configured = self.credentials().and_then(|c| c.dns.get("provider").cloned());
        Ok(catalog::CATALOG
            .iter()
            .map(|p| DnsProviderDetail {
                id: p.id.to_owned(),
                name: p.name.to_owned(),
                dynamic_dns: p.status == ProviderStatus::Implemented,
                unavailable_reason: match p.status {
                    ProviderStatus::Implemented => None,
                    ProviderStatus::NotImplemented(reason) => Some(reason.to_owned()),
                    ProviderStatus::NoApi => Some("This provider has no API.".to_owned()),
                },
                dns01: match p.dns01 {
                    Dns01::Bundled => Dns01Support::Bundled,
                    Dns01::BundledSeparateCredentials(note) => Dns01Support::BundledSeparateCredentials(note.to_owned()),
                    Dns01::Unavailable => Dns01Support::Unavailable,
                },
                caveat: p.caveat.map(str::to_owned),
                fields: field_names(p.id),
                configured: configured.as_deref() == Some(p.id),
                lists_domains: p.lists_domains,
            })
            .collect())
    }

    fn set_provider(
        &self,
        domain: &str,
        provider: &str,
        fields: &[(String, Secret)],
        input: &WebInput,
    ) -> Result<(), ApiError> {
        let key = input
            .secrets
            .as_deref()
            .ok_or_else(|| ApiError::Locked("unlock the device to store DNS credentials".to_owned()))?;
        let domain = Zone::new(&domain.trim().to_ascii_lowercase()).map_err(|e| ApiError::Failed(e.to_string()))?.as_str().to_owned();
        let (sealed, _) = self.seal_fields(provider, fields, key)?;
        // The older single-provider request. It makes this the first provider, which the
        // single-provider views address: the connected provider of this kind gets the new
        // credentials and the domain first, or a new provider is connected in front. Others stay.
        let (mut store, _) = self.provider_store();
        let mut chosen = match store.providers.iter().position(|p| p.kind() == provider) {
            Some(index) => {
                let mut existing = store.providers.remove(index);
                existing.dns = sealed;
                existing.domains.retain(|d| d != &domain);
                existing.domains.insert(0, domain);
                existing
            }
            None => providers::StoredProvider {
                id: store.next_id(provider),
                dns: sealed,
                domains: vec![domain],
                ..providers::StoredProvider::default()
            },
        };
        chosen.problem = None;
        store.providers.insert(0, chosen);
        self.save_store(&store).map_err(ApiError::Failed)?;
        // The challenge may change with the provider, so the proxy is applied in full.
        ExposureApplier::apply(self, input).map_err(ApiError::Failed)
    }

    fn domain_providers(&self, input: &WebInput) -> Result<Vec<nodo_api::DomainProvider>, ApiError> {
        Ok(self.list_domain_providers(input))
    }

    fn add_domain_provider(
        &self,
        kind: &str,
        fields: &[(String, Secret)],
        domain: Option<&str>,
        input: &WebInput,
    ) -> Result<nodo_api::DomainProvider, ApiError> {
        NodoWeb::add_domain_provider(self, kind, fields, domain, input)
    }

    fn remove_domain_provider(&self, id: &str, input: &WebInput) -> Result<(), ApiError> {
        NodoWeb::remove_domain_provider(self, id, input)
    }

    fn refresh_domain_provider(&self, id: &str, input: &WebInput) -> Result<nodo_api::DomainProvider, ApiError> {
        NodoWeb::refresh_domain_provider(self, id, input)
    }

    fn add_provider_domain(&self, id: &str, domain: &str, input: &WebInput) -> Result<nodo_api::DomainProvider, ApiError> {
        NodoWeb::add_provider_domain(self, id, domain, input)
    }

    fn test_provider(&self, input: &WebInput) -> Result<String, ApiError> {
        let report = self.provider(input.secrets.as_deref())?.test().map_err(|e| ApiError::Failed(e.to_string()))?;
        let mut text = format!("Test passed: wrote and removed {}", report.record);
        for warning in report.warnings {
            text.push_str(". ");
            text.push_str(&warning);
        }
        Ok(text)
    }

    fn retire_stale(&self) -> Result<Vec<String>, ApiError> {
        if self.io.read(&self.paths.ddns_config).is_none() {
            return Err(ApiError::Locked(
                "dynamic DNS has no config, because no site is public or the device is locked".to_owned(),
            ));
        }
        // The service and this run would both write the state file.
        let was_running = self.io.service_running(DDNS);
        if was_running {
            self.io.service(DDNS, false, false).map_err(ApiError::Failed)?;
        }
        let result = self.io.retire_stale(&self.paths.ddns_config);
        if was_running {
            self.io.service(DDNS, true, false).map_err(ApiError::Failed)?;
        }
        let output = result.map_err(ApiError::Failed)?;
        #[derive(Deserialize)]
        struct Removed {
            removed: Vec<String>,
        }
        serde_json::from_str::<Removed>(output.trim())
            .map(|r| r.removed)
            .map_err(|_| ApiError::Failed("nodo-ddns gave an unexpected answer".to_owned()))
    }

    fn reachability(&self, input: &WebInput, site: &SiteId) -> Result<ReachabilityDetail, ApiError> {
        if !input.sites.iter().any(|s| s.package == site.package && s.service.name == site.site) {
            return Err(ApiError::NotFound(format!("{}/{}", site.package, site.site)));
        }
        // The site's own domain decides the provider; a site with no address yet uses the first.
        let (store, _) = self.provider_store();
        let host = input
            .sites
            .iter()
            .find(|s| s.package == site.package && s.service.name == site.site)
            .and_then(|s| s.public_host.clone());
        let chosen = match host.as_deref().and_then(|h| store.zone_of(h)) {
            Some((provider, domain)) => self.provider_for(provider, domain, input.secrets.as_deref()),
            None => self.provider(input.secrets.as_deref()),
        };
        let provider = match chosen {
            Ok(provider) => provider,
            Err(e) => {
                return Ok(ReachabilityDetail {
                    checks: vec![ReachabilityItem {
                        id: "domain_and_credentials".to_owned(),
                        status: CheckStatus::Failed,
                        detail: e.to_string(),
                    }],
                    verdict: "failed".to_owned(),
                    public_ip: None,
                    router_wan: None,
                    device_ip: None,
                    gateway: None,
                    manual_check: Vec::new(),
                    warnings: Vec::new(),
                })
            }
        };
        let http = self.io.http();
        let gateway = router::system_gateway();
        let lan = gateway.and_then(|g| Some(LanInfo { device_ip: device_address(g)?, gateway: IpAddr::V4(g) }));
        let sources = nodo_web::detect::default_sources();
        let report = match gateway {
            Some(gateway) => {
                let router = GatewayQuery { gateway, http: http.as_ref() };
                reachability::run_check(&ReachabilityInputs {
                    provider: provider.as_ref(),
                    http: http.as_ref(),
                    sources: &sources,
                    router: &router,
                    ports: &NoExternalVantage,
                    lan: lan.as_ref(),
                })
            }
            None => reachability::run_check(&ReachabilityInputs {
                provider: provider.as_ref(),
                http: http.as_ref(),
                sources: &sources,
                router: &reachability::UnknownRouter,
                ports: &NoExternalVantage,
                lan: None,
            }),
        };
        let router_wan = match &report.connection {
            reachability::ConnectionAssessment::PublicAddress { router_wan, .. }
            | reachability::ConnectionAssessment::Cgnat { router_wan, .. }
            | reachability::ConnectionAssessment::PrivateWan { router_wan, .. }
            | reachability::ConnectionAssessment::UpstreamNat { router_wan, .. } => Some(router_wan.to_string()),
            reachability::ConnectionAssessment::Unknown { .. } => None,
        };
        Ok(ReachabilityDetail {
            checks: report
                .checks
                .iter()
                .map(|c| ReachabilityItem {
                    id: check_id(c.id).to_owned(),
                    status: match c.status {
                        reachability::CheckStatus::Passed => CheckStatus::Passed,
                        reachability::CheckStatus::Failed => CheckStatus::Failed,
                        reachability::CheckStatus::Unknown => CheckStatus::Unknown,
                        reachability::CheckStatus::Skipped => CheckStatus::Skipped,
                    },
                    detail: c.detail.clone(),
                })
                .collect(),
            verdict: match report.verdict {
                Verdict::Ready => "ready",
                Verdict::PortsUnverified => "ports_unverified",
                Verdict::NeedsPortForward { .. } => "needs_port_forward",
                Verdict::BlockedByCgnat { .. } => "blocked_by_cgnat",
                Verdict::Failed => "failed",
            }
            .to_owned(),
            public_ip: report.public_ip.map(|ip| ip.to_string()),
            router_wan,
            device_ip: lan.as_ref().map(|l| l.device_ip.to_string()),
            gateway: lan.as_ref().map(|l| l.gateway.to_string()),
            manual_check: report.manual_check.map(|m| m.steps).unwrap_or_default(),
            warnings: report.warnings,
        })
    }

    fn acme_settings(&self, input: &WebInput) -> Result<AcmeSettingsInfo, ApiError> {
        Ok(self.acme_info(input.secrets.as_deref()))
    }

    fn set_acme_settings(&self, settings: &AcmeSettingsInput, input: &WebInput) -> Result<(), ApiError> {
        self.store_acme_settings(settings, input)
    }

    fn set_dns01(&self, fields: &[(String, Secret)], input: &WebInput) -> Result<(), ApiError> {
        self.store_dns01(fields, input)
    }

    fn certificates(&self, input: &WebInput) -> Result<CertificateReport, ApiError> {
        let plan = self.plan(input).map_err(ApiError::Failed)?;
        Ok(self.certificate_report(&plan, input))
    }

    fn check_local_names(&self, input: &WebInput) -> Result<Vec<LocalNameCheck>, ApiError> {
        let plan = self.plan(input).map_err(ApiError::Failed)?;
        self.local_name_checks(&plan, input.secrets.as_deref())
    }
}

/// The real side effects, as root.
#[derive(Debug, Clone)]
pub struct RealWebIo {
    pub sv_dir: PathBuf,
    pub service_dir: PathBuf,
}

impl Default for RealWebIo {
    fn default() -> Self {
        RealWebIo { sv_dir: "/etc/sv".into(), service_dir: "/var/service".into() }
    }
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program).args(args).output().map_err(|e| format!("{program}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(format!("{program} failed: {}", err.lines().take(3).collect::<Vec<_>>().join(" ")))
    }
}

fn lookup_id(name: &str, group: bool) -> Result<u32, String> {
    let file = if group { "/etc/group" } else { "/etc/passwd" };
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    text.lines()
        .find_map(|line| {
            let mut parts = line.split(':');
            (parts.next()? == name).then(|| parts.nth(1)?.parse().ok())?
        })
        .ok_or_else(|| format!("no such {}: {name}", if group { "group" } else { "user" }))
}

fn set_owner(path: &Path, owner: Owner) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let (mode, user, group) = owner;
    std::os::unix::fs::chown(path, Some(lookup_id(user, false)?), Some(lookup_id(group, true)?))
        .map_err(|e| e.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())
}

impl WebIo for RealWebIo {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    fn write(&self, path: &Path, bytes: &[u8], owner: Owner) -> Result<(), String> {
        use std::os::unix::fs::OpenOptionsExt;
        let temp = path.with_extension("nodo-tmp");
        // Created private, then given its owner and mode before it replaces the old file.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| format!("{}: {e}", temp.display()))?;
        file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| e.to_string())?;
        set_owner(&temp, owner)?;
        std::fs::rename(&temp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn remove(&self, path: &Path) -> Result<(), String> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }

    fn ensure_dir(&self, path: &Path, owner: Owner) -> Result<(), String> {
        std::fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
        set_owner(path, owner)
    }

    fn nft_reload(&self, main: &Path) -> Result<(), String> {
        let main = main.to_string_lossy();
        run("nft", &["-c", "-f", &main])?;
        run("nft", &["-f", &main]).map(|_| ())
    }

    fn caddy_load(&self, socket: &Path, json: &[u8]) -> Result<bool, String> {
        let Ok(mut stream) = UnixStream::connect(socket) else {
            return Ok(false);
        };
        stream.set_read_timeout(Some(Duration::from_secs(60))).map_err(|e| e.to_string())?;
        // Caddy's admin API accepts only an empty Host over a unix socket, as RFC 9112 asks for a
        // request without an internet host.
        let head = format!(
            "POST /load HTTP/1.1\r\nHost: \r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            json.len()
        );
        stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(json)).map_err(|e| e.to_string())?;
        let mut reply = Vec::new();
        stream.take(64 * 1024).read_to_end(&mut reply).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&reply);
        if text.starts_with("HTTP/1.1 200") {
            Ok(true)
        } else {
            let body = text.split("\r\n\r\n").nth(1).unwrap_or_default();
            Err(body.chars().filter(|c| !c.is_control()).take(300).collect())
        }
    }

    fn service_installed(&self, name: &str) -> bool {
        self.sv_dir.join(name).join("run").exists()
    }

    fn service_running(&self, name: &str) -> bool {
        let path = self.service_dir.join(name);
        path.exists() && run("sv", &["status", &path.to_string_lossy()]).is_ok_and(|s| s.starts_with("run:"))
    }

    fn service(&self, name: &str, up: bool, restart: bool) -> Result<(), String> {
        let dir = self.sv_dir.join(name);
        let link = self.service_dir.join(name);
        // A down file keeps a disabled service down across reboots. The link stays, because runsv
        // can linger after its link goes and then ignore the next start.
        let down = dir.join("down");
        if up {
            self.remove(&down)?;
            let new_link = !link.exists();
            if new_link {
                std::os::unix::fs::symlink(&dir, &link).map_err(|e| e.to_string())?;
            }
            let path = link.to_string_lossy();
            // runsvdir starts a newly linked service itself, with the current config. Restarting
            // it as well would stop that first start part way and leave it briefly down.
            let action = if restart && !new_link { "restart" } else { "up" };
            let mut last = String::new();
            // runsvdir notices a new link within five seconds.
            for _ in 0..10 {
                match run("sv", &["-w", "30", action, &path]) {
                    Ok(_) => return Ok(()),
                    Err(e) => last = e,
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(last)
        } else {
            if !link.exists() {
                return Ok(());
            }
            std::fs::write(&down, b"").map_err(|e| e.to_string())?;
            run("sv", &["-w", "30", "down", &link.to_string_lossy()]).map(|_| ())
        }
    }

    fn retire_stale(&self, config: &Path) -> Result<String, String> {
        run(
            "chpst",
            &["-u", "_nododdns:_nododdns", "nodo-ddns", "--config", &config.to_string_lossy(), "--retire-stale"],
        )
    }

    fn http(&self) -> SharedHttp {
        std::sync::Arc::new(nodo_web::net::UreqClient::new())
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn random(&self, out: &mut [u8]) -> Result<(), String> {
        getrandom::fill(out).map_err(|e| format!("no secure randomness: {e}"))
    }

    fn list_dir(&self, path: &Path) -> Vec<String> {
        std::fs::read_dir(path)
            .map(|entries| entries.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect())
            .unwrap_or_default()
    }

    fn remove_dir_all(&self, path: &Path) -> Result<(), String> {
        match std::fs::remove_dir_all(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
            _ => Ok(()),
        }
    }

    fn resolve(&self, host: &str) -> Option<Vec<IpAddr>> {
        use std::net::ToSocketAddrs;
        match (host, 0).to_socket_addrs() {
            Ok(addresses) => {
                let mut ips: Vec<IpAddr> = addresses.map(|a| a.ip()).collect();
                ips.sort();
                ips.dedup();
                Some(ips)
            }
            Err(_) => None,
        }
    }

    fn now(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }

    fn caddy_get(&self, socket: &Path, path: &str) -> Option<Vec<u8>> {
        let mut stream = UnixStream::connect(socket).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
        let head = format!("GET {path} HTTP/1.1\r\nHost: \r\nConnection: close\r\n\r\n");
        stream.write_all(head.as_bytes()).ok()?;
        let mut reply = Vec::new();
        stream.take(256 * 1024).read_to_end(&mut reply).ok()?;
        let split = reply.windows(4).position(|w| w == b"\r\n\r\n")?;
        reply.starts_with(b"HTTP/1.1 200").then(|| reply[split + 4..].to_vec())
    }

    fn dns_provider(&self, config: ProviderConfig, domain: &str) -> Result<Box<dyn DnsProvider>, String> {
        config.build(self.http(), domain).map_err(|e| e.to_string())
    }

    fn list_domains(&self, config: ProviderConfig) -> Result<Option<Vec<String>>, String> {
        config.list_domains(self.http()).map_err(|e| e.to_string())
    }

    fn detect_public_ip(&self) -> Option<IpAddr> {
        let http = self.http();
        nodo_web::detect::detect(http.as_ref(), &nodo_web::detect::default_sources(), nodo_web::detect::Family::V4)
            .agreed_ip()
    }
}

mod certs;
mod localsite;
mod onion;
mod providers;

#[cfg(test)]
mod tests;
