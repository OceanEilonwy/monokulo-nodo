//! Generates the Caddy config, firewall rules and Tor hidden-service lines from the enabled sites.
//!
//! [`generate`] is pure: the same input always gives the same output, and nothing is written.
//!
//! # Listener layout
//!
//! Each kind of address gets its own listener, and a listener only knows the hosts of its kind:
//!
//! - **Public** hosts are served on the router-facing ports 80 and 443. Caddy runs unprivileged, so it
//!   binds [`CADDY_HTTP_PORT`] and [`CADDY_HTTPS_PORT`], and nftables redirects 80 and 443 to them.
//!   Admin paths are always blocked here.
//! - **Local** hosts are served only on the device's second LAN address (see [`crate::local`]), at
//!   port 443, so their URLs have no port: `https://shop.local`. nftables sends that address's 443 to
//!   Caddy's [`CADDY_LOCAL_PORT`], which Caddy binds on that address alone, and only for LAN sources.
//! - Every **onion** address has its own plain-HTTP server on its own loopback port, reachable only
//!   through Tor. A shared socket would let anyone probing the public IP send `Host: <addr>.onion` and
//!   link the onion site to the home IP.
//!
//! Local hosts used to share the main address with public hosts and relied on a `remote_ip` check. A
//! router that masquerades forwarded connections makes every internet client look like the router's
//! LAN address, and that check then let the internet reach local hosts and their admin paths. Now what
//! a request can reach depends on the address it was sent to. The router forwards only to the main
//! address, so a forwarded connection can never be addressed to a local host, whatever its source. The
//! `remote_ip` check stays on local hosts as a second layer. Without a second address, local sites are
//! unavailable, listed in [`WebPlan::local`], and never served on the main address or another port.
//!
//! Each HTTPS listener also has TLS connection policies limited to its own host names, so a handshake
//! for `shop.local` on the public port fails instead of presenting the local certificate.

pub mod types;

use crate::local::{LocalAddress, LocalUnavailable};
use crate::names::{self, quoted};
use crate::secret::Secret;
use nodo_api::{Exposure, ServiceState};
use nodo_manifest::WebService;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use thiserror::Error;
use types::*;

/// The ports the router forwards. The firewall redirects them to Caddy's own ports.
pub const PUBLIC_HTTPS_PORT: u16 = 443;
pub const PUBLIC_HTTP_PORT: u16 = 80;
/// Unprivileged ports Caddy binds for public traffic. Only redirected connections are accepted on them.
pub const CADDY_HTTPS_PORT: u16 = 10443;
pub const CADDY_HTTP_PORT: u16 = 10080;
/// Caddy binds this on the local address only. nftables sends the local address's port 443 here.
pub const CADDY_LOCAL_PORT: u16 = 10444;
const ONION_VIRTUAL_PORT: u16 = 80;
const INTERNAL_CA: &str = "local";
const APEX: &str = "@";

pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
/// Caddy writes certificate progress and errors here, inside its own state directory, for the helper.
pub const TLS_EVENTS_LOG: &str = "/var/lib/caddy/nodo-tls.log";
/// The loggers whose lines say how obtaining and renewing went. `events` logs at debug level only.
const TLS_LOGGERS: [&str; 4] = ["events", "tls.obtain", "tls.renew", "tls.issuance.acme"];

const UNAVAILABLE_PAGE: &str = "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>Unavailable</title>\
<body><h1>This site is unavailable</h1><p>Please try again later.</p></body></html>";

/// Loopback, RFC 1918, link-local and ULA. Excludes 100.64.0.0/10, which is shared with other ISP customers.
const DEFAULT_LAN_RANGES: &[&str] = &[
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "::1/128",
    "fc00::/7",
    "fe80::/10",
];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WebConfigError {
    #[error("{field} is not a valid name: \"{value}\"")]
    InvalidName { field: &'static str, value: String },
    #[error("site {site}: upstream is not an allowed address: \"{value}\"")]
    InvalidUpstream { site: String, value: String },
    #[error("site {site}: admin path is not allowed: \"{value}\"")]
    InvalidAdminPath { site: String, value: String },
    #[error("site {site} is public, but no domain is connected")]
    NoDomain { site: String },
    #[error("site {site} is tied to one address and cannot use several exposures")]
    SingleAddressOnly { site: String },
    #[error("site {package}/{site} is listed twice")]
    DuplicateSite { package: String, site: String },
    #[error("two sites use the host {host}")]
    DuplicateHost { host: String },
    #[error("sites from {first} and {second} claim the same upstream {upstream}")]
    SharedUpstream { first: String, second: String, upstream: String },
    #[error("setting {field} is not valid: \"{value}\"")]
    InvalidSetting { field: &'static str, value: String },
    #[error("site {site}: upstream port {port} is reserved for the web proxy")]
    ReservedPort { site: String, port: u16 },
    #[error("no free loopback port for onion sites")]
    OutOfOnionPorts,
}

/// Why local sites named under the owner's domain wait. DNS-01 is the only challenge that works
/// for them, because the local address is never reachable from the internet.
pub const LOCAL_NAMES_NEED_DNS01: &str = "Local names under your domain need certificates through your DNS provider. \
Connect a provider that supports it, unlock the device, or switch local names back to .local.";

/// One site as the helper knows it: what the package declared, plus what the user chose.
#[derive(Debug, Clone, PartialEq)]
pub struct Site {
    pub package: String,
    pub service: WebService,
    pub exposure: Exposure,
    pub state: ServiceState,
    /// Overrides the default `<subdomain>.local`.
    pub local_host: Option<String>,
    /// Overrides the subdomain under the connected domain. `@` means the domain itself.
    pub public_subdomain: Option<String>,
    /// The full host the user chose, which wins over the subdomain and needs no connected domain.
    pub public_host: Option<String>,
    /// Known only after Tor has created the service. See [`WebPlan::onion_pending`].
    pub onion_host: Option<String>,
    /// The user's choice on the Confirm screen. See [`DomainSettings::rate_limit`].
    pub rate_limit: bool,
}

/// A per-client request budget for public routes, enforced by the `rate_limit` handler that the
/// `nodo-caddy` build includes. Clients are told apart by source address, so behind a router that
/// masquerades forwarded traffic every visitor shares one budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub max_events: u32,
    pub window_secs: u32,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self { max_events: 600, window_secs: 60 }
    }
}

/// Credentials for ACME DNS-01 challenges, so public certificates need no inbound port 80. Each
/// variant matches the fields of its caddy-dns module in the `nodo-caddy` build. GoDaddy's module
/// uses the older API key and secret, not the Personal Access Token the dynamic DNS client uses.
#[derive(Debug, Clone, PartialEq)]
pub enum DnsChallenge {
    Porkbun { api_key: Secret, secret_key: Secret },
    Cloudflare { api_token: Secret },
    GoDaddy { api_key: Secret, api_secret: Secret },
    Gandi { personal_access_token: Secret },
    NameCom { username: String, api_token: Secret },
    Alibaba { access_key_id: Secret, access_key_secret: Secret },
    /// The provider block of the config Caddy already runs. While the device is locked the sealed
    /// credentials cannot be opened, and dropping DNS-01 then would break renewals for an owner who
    /// forwards no ports.
    Kept(KeptProvider),
}

/// A provider block copied from a config this crate generated earlier. It holds credentials.
#[derive(Clone, PartialEq)]
pub struct KeptProvider(pub serde_json::Value);

impl std::fmt::Debug for KeptProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("KeptProvider").field(&self.0["name"]).finish()
    }
}

/// Which ACME server issues public certificates, and how Caddy talks to it.
#[derive(Debug, Clone, PartialEq)]
pub struct AcmeConfig {
    /// Let's Encrypt, its staging server, or a test server.
    pub directory: String,
    /// Caddy normally waits until a DNS-01 record is visible at the zone's own name servers before
    /// asking for validation. A test DNS server that cannot answer SOA queries needs this off.
    pub dns_propagation_check: bool,
    pub renew_interval_secs: Option<u32>,
    /// Where Caddy logs certificate progress and errors for the helper to read.
    pub events_log: Option<String>,
}

impl Default for AcmeConfig {
    fn default() -> Self {
        Self {
            directory: LETS_ENCRYPT.to_owned(),
            dns_propagation_check: true,
            renew_interval_secs: None,
            events_log: Some(TLS_EVENTS_LOG.to_owned()),
        }
    }
}

impl DnsChallenge {
    /// The provider block of an ACME issuer in a config generated earlier, if it used DNS-01.
    pub fn kept_from(config: &serde_json::Value) -> Option<DnsChallenge> {
        config["apps"]["tls"]["automation"]["policies"]
            .as_array()?
            .iter()
            .flat_map(|policy| policy["issuers"].as_array().into_iter().flatten())
            .find_map(|issuer| issuer["challenges"]["dns"]["provider"].as_object().cloned())
            .map(|provider| DnsChallenge::Kept(KeptProvider(serde_json::Value::Object(provider))))
    }

    /// Whether these credentials can go into a config: no placeholders, and in a form the module
    /// accepts without logging them.
    pub fn usable(&self) -> bool {
        let safe = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_graphic() && b != b'{' && b != b'}');
        self.values().into_iter().all(safe) && self.module_accepts()
    }

    /// The caddy-dns module name, for the screens.
    pub fn module(&self) -> String {
        self.provider_json()["name"].as_str().unwrap_or_default().to_owned()
    }

    fn values(&self) -> Vec<&str> {
        match self {
            Self::Porkbun { api_key, secret_key } => vec![api_key.expose(), secret_key.expose()],
            Self::Cloudflare { api_token } => vec![api_token.expose()],
            Self::GoDaddy { api_key, api_secret } => vec![api_key.expose(), api_secret.expose()],
            Self::Gandi { personal_access_token } => vec![personal_access_token.expose()],
            Self::NameCom { username, api_token } => vec![username, api_token.expose()],
            Self::Alibaba { access_key_id, access_key_secret } => vec![access_key_id.expose(), access_key_secret.expose()],
            Self::Kept(KeptProvider(value)) => match value.as_object() {
                Some(fields) if fields.values().all(serde_json::Value::is_string) => {
                    fields.values().filter_map(serde_json::Value::as_str).collect()
                }
                // Only flat string fields are ever generated, so anything else fails validation.
                _ => vec![""],
            },
        }
    }

    /// The Cloudflare module rejects a token outside its documented formats and puts the token in the
    /// error, which Caddy logs. Checking first keeps a mistyped token out of the log.
    fn module_accepts(&self) -> bool {
        let token_chars = |s: &str| s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        match self {
            Self::Cloudflare { api_token } => {
                let token = api_token.expose();
                let scoped = ["cfut_", "cfat_"]
                    .iter()
                    .find_map(|prefix| token.strip_prefix(prefix))
                    .is_some_and(|rest| (32..=256).contains(&rest.len()) && token_chars(rest));
                scoped || ((35..=50).contains(&token.len()) && token_chars(token))
            }
            _ => true,
        }
    }

    fn provider_json(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Self::Porkbun { api_key, secret_key } => {
                json!({"name": "porkbun", "api_key": api_key.expose(), "api_secret_key": secret_key.expose()})
            }
            Self::Cloudflare { api_token } => json!({"name": "cloudflare", "api_token": api_token.expose()}),
            Self::GoDaddy { api_key, api_secret } => {
                json!({"name": "godaddy", "api_token": format!("{}:{}", api_key.expose(), api_secret.expose())})
            }
            Self::Gandi { personal_access_token } => {
                json!({"name": "gandi", "bearer_token": personal_access_token.expose()})
            }
            Self::NameCom { username, api_token } => json!({
                "name": "namedotcom", "user": username, "api_token": api_token.expose(), "server": "https://api.name.com"
            }),
            Self::Alibaba { access_key_id, access_key_secret } => json!({
                "name": "alidns", "access_key_id": access_key_id.expose(), "access_key_secret": access_key_secret.expose()
            }),
            Self::Kept(KeptProvider(value)) => value.clone(),
        }
    }
}

/// Device-wide settings.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainSettings {
    /// The user's own domain, such as `example.com`.
    pub domain: Option<String>,
    pub acme_email: Option<String>,
    /// Client ranges allowed to reach local hosts.
    pub lan_ranges: Vec<String>,
    /// The second LAN address that local hosts are served on, from `nodo-localnet`.
    pub local_address: LocalAddress,
    /// Onion listeners are allocated upward from here on loopback.
    pub onion_port_base: u16,
    /// Parent of each hidden-service directory. Tor needs this to exist with strict permissions.
    pub tor_dir: String,
    pub admin_listen: String,
    /// `None` for a Caddy build without the rate limit module. Sites that asked for rate limiting are
    /// then listed in [`WebPlan::rate_limit_unenforced`], so the screens do not claim protection.
    pub rate_limit: Option<RateLimit>,
    /// `None` uses the HTTP and TLS-ALPN challenges, which need ports 80 and 443 forwarded.
    pub dns_challenge: Option<DnsChallenge>,
    pub acme: AcmeConfig,
    /// Names local sites `<label>.<local_domain>` with a public certificate through DNS-01, instead
    /// of `<label>.local` with the internal CA. The names still resolve only to the local address.
    pub local_domain: Option<String>,
}

impl Default for DomainSettings {
    fn default() -> Self {
        Self {
            domain: None,
            acme_email: None,
            lan_ranges: DEFAULT_LAN_RANGES.iter().map(|r| (*r).to_owned()).collect(),
            local_address: LocalAddress::default(),
            onion_port_base: 19000,
            tor_dir: "/var/lib/tor/nodo".to_owned(),
            // The default TCP admin port would be reachable from every process on loopback.
            admin_listen: "unix//run/nodo-web/caddy-admin.sock".to_owned(),
            rate_limit: Some(RateLimit::default()),
            dns_challenge: None,
            acme: AcmeConfig::default(),
            local_domain: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressKind {
    Local,
    Onion,
    Public,
}

/// One live address, for the Sites and Exposure screens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SiteAddress {
    pub package: String,
    pub site: String,
    pub kind: AddressKind,
    pub host: String,
    pub url: String,
    pub listen: String,
}

/// What the firewall must do for Caddy. Onion sites need nothing, since Tor connects out and reaches
/// its listeners over loopback. [`render_nft`] turns this into rules.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FirewallPlan {
    /// Router-facing ports, open to the internet. Empty unless at least one site is public.
    pub public_tcp: Vec<u16>,
    /// Where each public port is redirected, as `(public, caddy)`. Never applied to the local address.
    pub redirects: Vec<(u16, u16)>,
    pub local: Option<LocalFirewall>,
}

/// Port 443 and mDNS on the local address, for LAN sources only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalFirewall {
    pub address: Ipv4Addr,
    pub caddy_port: u16,
    /// IPv4 ranges only, since the local address is IPv4.
    pub lan_ranges: Vec<String>,
}

/// Whether local sites are being served, and which are waiting for the local address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalStatus {
    pub address: LocalAddress,
    /// Sites with local exposure that have no local address yet.
    pub waiting: Vec<SiteRef>,
    /// The `.local` names the mDNS responder should answer with the local address.
    pub mdns_names: Vec<String>,
    /// What the user can do when sites are waiting. Every reason offers a static address or a fix.
    pub remedy: Option<&'static str>,
    /// Local names under the owner's domain. Each needs a DNS record pointing at the local address.
    pub dns_names: Vec<String>,
    /// Why local sites named under the owner's domain wait, when the address itself is fine.
    pub names_problem: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TorHiddenService {
    pub package: String,
    pub site: String,
    pub dir: String,
    pub virtual_port: u16,
    pub target: String,
}

impl TorHiddenService {
    fn lines(&self) -> [String; 3] {
        [
            format!("HiddenServiceDir {}", self.dir),
            "HiddenServiceVersion 3".to_owned(),
            format!("HiddenServicePort {} {}", self.virtual_port, self.target),
        ]
    }
}

/// A site whose onion service is configured but whose address Tor has not reported yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SiteRef {
    pub package: String,
    pub site: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WebPlan {
    pub caddy: CaddyConfig,
    pub firewall: FirewallPlan,
    pub tor_services: Vec<TorHiddenService>,
    pub tor_lines: Vec<String>,
    pub addresses: Vec<SiteAddress>,
    /// Tor must start with `tor_lines` first. Generate again once its `hostname` files exist.
    pub onion_pending: Vec<SiteRef>,
    pub rate_limit_unenforced: Vec<SiteRef>,
    pub local: LocalStatus,
}

struct Resolved<'a> {
    site: &'a Site,
    dial: String,
    admin_patterns: Vec<String>,
    /// Also matches an admin path followed by any character that cannot continue a path segment name.
    admin_regexp: Option<String>,
    local: Option<String>,
    public: Option<String>,
    onion_enabled: bool,
    onion: Option<String>,
}

pub fn generate(sites: &[Site], settings: &DomainSettings) -> Result<WebPlan, WebConfigError> {
    validate_settings(settings)?;

    // A site with every exposure off produces nothing, so it cannot clash with anything either.
    let mut resolved = sites
        .iter()
        .filter(|site| site.exposure.local || site.exposure.onion || site.exposure.public)
        .map(|site| resolve(site, settings))
        .collect::<Result<Vec<_>, _>>()?;
    resolved.sort_by(|a, b| (&a.site.package, &a.site.service.name).cmp(&(&b.site.package, &b.site.service.name)));
    check_unique(&resolved)?;

    let mut public_routes = Vec::new();
    let mut local_routes = Vec::new();
    let mut public_subjects = Vec::new();
    let mut local_subjects = Vec::new();
    let mut servers = BTreeMap::new();
    let mut plan_addresses = Vec::new();
    let mut tor_services = Vec::new();
    let mut onion_pending = Vec::new();
    let mut rate_limit_unenforced = Vec::new();

    let mut reserved_ports: BTreeSet<u16> = BTreeSet::from(LISTENER_PORTS);
    reserved_ports.extend(resolved.iter().filter_map(|r| tcp_port(&r.dial)));
    let mut next_onion_port = u32::from(settings.onion_port_base);

    let public_listen = format!(":{CADDY_HTTPS_PORT}");
    // An address that is not private and inside the LAN ranges is treated as no address, so local
    // sites wait instead of every exposure failing.
    let local_address = match settings.local_address {
        LocalAddress::Ready { address }
            if !address.is_private()
                || !settings.lan_ranges.iter().any(|range| range_contains(range, IpAddr::V4(address))) =>
        {
            LocalAddress::Unavailable { reason: LocalUnavailable::UnsuitableAddress(address) }
        }
        ref other => other.clone(),
    };
    let local_ip = match &local_address {
        LocalAddress::Ready { address } => Some(*address),
        LocalAddress::Unavailable { .. } => None,
    };
    let local_listen = local_ip.map(|ip| format!("{ip}:{CADDY_LOCAL_PORT}")).unwrap_or_default();
    let mut local_waiting = Vec::new();
    let mut local_dns_names = Vec::new();
    let mut names_problem = None;

    for item in &resolved {
        let site = item.site;
        let address = |kind, host: &str, url: String, listen: &str| SiteAddress {
            package: site.package.clone(),
            site: site.service.name.clone(),
            kind,
            host: host.to_owned(),
            url,
            listen: listen.to_owned(),
        };
        let site_ref = || SiteRef { package: site.package.clone(), site: site.service.name.clone() };

        if let Some(host) = &item.public {
            let limit = if site.rate_limit { settings.rate_limit } else { None };
            if site.rate_limit && limit.is_none() {
                rate_limit_unenforced.push(site_ref());
            }
            public_routes.push(host_route(
                Matcher { host: vec![host.clone()], ..Matcher::default() },
                site_handlers(item, true, limit),
            ));
            public_subjects.push(host.clone());
            plan_addresses.push(address(AddressKind::Public, host, format!("https://{host}"), &public_listen));
        }

        // A name under the owner's domain can only get its certificate through DNS-01.
        let needs_dns01 = item.local.as_ref().is_some_and(|host| !names::is_local_name(host));
        let names_blocked = needs_dns01 && settings.dns_challenge.is_none();
        if item.local.is_some() && (local_ip.is_none() || names_blocked) {
            local_waiting.push(site_ref());
            if local_ip.is_some() {
                names_problem = Some(LOCAL_NAMES_NEED_DNS01);
            }
        }
        if let Some(host) = item.local.as_ref().filter(|_| local_ip.is_some() && !names_blocked) {
            if needs_dns01 {
                local_dns_names.push(host.clone());
            }
            local_routes.push(host_route(
                Matcher {
                    host: vec![host.clone()],
                    remote_ip: Some(RemoteIp { ranges: settings.lan_ranges.clone() }),
                    ..Matcher::default()
                },
                site_handlers(item, false, None),
            ));
            local_subjects.push(host.clone());
            plan_addresses.push(address(
                AddressKind::Local,
                host,
                format!("https://{host}"),
                &local_listen,
            ));
        }

        if item.onion_enabled {
            let port = allocate_port(&mut next_onion_port, &mut reserved_ports)?;
            let listen = format!("127.0.0.1:{port}");
            tor_services.push(TorHiddenService {
                package: site.package.clone(),
                site: site.service.name.clone(),
                // The dot cannot appear in either validated part, so distinct sites never collide.
                dir: format!("{}/{}.{}", settings.tor_dir, site.package, site.service.name),
                virtual_port: ONION_VIRTUAL_PORT,
                target: listen.clone(),
            });
            match &item.onion {
                Some(host) => {
                    servers.insert(
                        format!("onion-{}", tor_services.len() - 1),
                        onion_server(listen.clone(), host, site_handlers(item, true, None)),
                    );
                    plan_addresses.push(address(AddressKind::Onion, host, format!("http://{host}"), &listen));
                }
                None => onion_pending.push(site_ref()),
            }
        }
    }

    let any_public = !public_routes.is_empty();
    let any_local = !local_routes.is_empty();
    if any_public {
        // Caddy builds its HTTP to HTTPS redirect, and answers ACME HTTP challenges, on `http_port`.
        servers.insert("public".to_owned(), https_server(public_listen, public_routes, &public_subjects, true));
    }
    if any_local {
        // Redirects would be served on the public HTTP port, which would announce local host names.
        servers.insert("local".to_owned(), https_server(local_listen, local_routes, &local_subjects, false));
    }

    let mut policies = Vec::new();
    let internal_subjects: Vec<String> =
        local_subjects.iter().filter(|host| names::is_local_name(host)).cloned().collect();
    if !internal_subjects.is_empty() {
        policies.push(AutomationPolicy { subjects: internal_subjects.clone(), issuers: vec![Issuer::Internal] });
    }
    let acme_issuer = || Issuer::Acme {
        ca: settings.acme.directory.clone(),
        email: settings.acme_email.clone(),
        // Caddy uses only DNS-01 when it is configured, so no inbound port is needed then.
        challenges: settings.dns_challenge.as_ref().map(|challenge| AcmeChallenges {
            dns: DnsChallengeConfig {
                provider: challenge.provider_json(),
                propagation_timeout: (!settings.acme.dns_propagation_check).then_some(-1),
                propagation_delay: (!settings.acme.dns_propagation_check).then(|| "2s".to_owned()),
            },
        }),
    };
    if !local_dns_names.is_empty() {
        policies.push(AutomationPolicy { subjects: local_dns_names.clone(), issuers: vec![acme_issuer()] });
    }
    let any_acme = !public_subjects.is_empty() || !local_dns_names.is_empty();
    if !public_subjects.is_empty() {
        policies.push(AutomationPolicy { subjects: public_subjects, issuers: vec![acme_issuer()] });
    }

    let firewall = FirewallPlan {
        public_tcp: if any_public { vec![PUBLIC_HTTP_PORT, PUBLIC_HTTPS_PORT] } else { Vec::new() },
        redirects: if any_public {
            vec![(PUBLIC_HTTP_PORT, CADDY_HTTP_PORT), (PUBLIC_HTTPS_PORT, CADDY_HTTPS_PORT)]
        } else {
            Vec::new()
        },
        local: local_ip.filter(|_| any_local).map(|address| LocalFirewall {
            address,
            caddy_port: CADDY_LOCAL_PORT,
            lan_ranges: settings.lan_ranges.iter().filter(|r| !r.contains(':')).cloned().collect(),
        }),
    };
    let mut mdns_names: Vec<String> =
        local_subjects.iter().filter(|host| names::has_mdns_suffix(host)).cloned().collect();
    mdns_names.sort();

    let tor_lines = tor_services.iter().flat_map(TorHiddenService::lines).collect();

    Ok(WebPlan {
        caddy: CaddyConfig {
            admin: Admin { listen: settings.admin_listen.clone(), config: AdminConfig { persist: false } },
            logging: settings.acme.events_log.as_ref().filter(|_| any_acme).map(|path| Logging {
                logs: BTreeMap::from([(
                    "nodo_tls".to_owned(),
                    LogSink {
                        writer: LogWriter {
                            output: "file".to_owned(),
                            filename: path.clone(),
                            roll_size_mb: 1,
                            roll_keep: 2,
                        },
                        encoder: LogEncoder { format: "json".to_owned() },
                        level: "DEBUG".to_owned(),
                        include: TLS_LOGGERS.iter().map(|l| (*l).to_owned()).collect(),
                    },
                )]),
            }),
            apps: Apps {
                http: HttpApp { http_port: CADDY_HTTP_PORT, https_port: CADDY_HTTPS_PORT, servers },
                tls: (!policies.is_empty()).then(|| TlsApp {
                    automation: TlsAutomation {
                        policies,
                        renew_interval: settings.acme.renew_interval_secs.map(|secs| format!("{secs}s")),
                    },
                }),
                // Stops Caddy trying to install its root in the system trust store, which needs a prompt.
                pki: (!internal_subjects.is_empty()).then(|| PkiApp {
                    certificate_authorities: BTreeMap::from([(
                        INTERNAL_CA.to_owned(),
                        CertificateAuthority { install_trust: false },
                    )]),
                }),
            },
        },
        firewall,
        tor_services,
        tor_lines,
        addresses: plan_addresses,
        onion_pending,
        rate_limit_unenforced,
        local: LocalStatus {
            remedy: match &local_address {
                LocalAddress::Unavailable { reason } if !local_waiting.is_empty() => Some(reason.remedy()),
                _ => names_problem,
            },
            address: local_address,
            waiting: local_waiting,
            mdns_names,
            dns_names: local_dns_names,
            names_problem,
        },
    })
}

const LISTENER_PORTS: [u16; 5] =
    [PUBLIC_HTTP_PORT, PUBLIC_HTTPS_PORT, CADDY_HTTP_PORT, CADDY_HTTPS_PORT, CADDY_LOCAL_PORT];
const MDNS_GROUP: &str = "224.0.0.251";
const MDNS_PORT: u16 = 5353;

/// Renders the firewall plan as an nftables include for `/etc/nftables.d/`. It extends the base
/// `inet monokulo` table, whose input policy is drop.
///
/// Traffic to the local address leaves the NAT chain before the public redirects, so it can only
/// reach the local listener, and only from LAN sources. Traffic to any other address can only reach
/// the public listeners. Caddy's own ports accept only connections that really were translated, so
/// none of them can be reached directly.
pub fn render_nft(plan: &FirewallPlan) -> String {
    let mut out = String::from("# Generated by nodo-web. Changes are overwritten.\ntable inet monokulo {\n");
    let lan = plan.local.as_ref().map(|l| format!("{{ {} }}", l.lan_ranges.join(", ")));
    if !plan.redirects.is_empty() || plan.local.is_some() {
        out.push_str("\tchain web_prerouting {\n\t\ttype nat hook prerouting priority dstnat; policy accept;\n");
        if let (Some(local), Some(lan)) = (&plan.local, &lan) {
            let ip = local.address;
            out.push_str(&format!(
                "\t\tip daddr {ip} ip saddr {lan} tcp dport {PUBLIC_HTTPS_PORT} dnat ip to {ip}:{}\n",
                local.caddy_port
            ));
            out.push_str(&format!("\t\tip daddr {ip} return\n"));
        }
        for (from, to) in &plan.redirects {
            out.push_str(&format!("\t\ttcp dport {from} redirect to :{to}\n"));
        }
        out.push_str("\t}\n\n");
    }
    out.push_str("\tchain input {\n");
    for (_, to) in &plan.redirects {
        out.push_str(&format!("\t\ttcp dport {to} ct status dnat accept\n"));
    }
    if let (Some(local), Some(lan)) = (&plan.local, &lan) {
        let ip = local.address;
        out.push_str(&format!(
            "\t\tip daddr {ip} ip saddr {lan} tcp dport {} ct status dnat accept\n",
            local.caddy_port
        ));
        out.push_str(&format!("\t\tip daddr {MDNS_GROUP} ip saddr {lan} udp dport {MDNS_PORT} accept\n"));
    }
    out.push_str("\t}\n}\n");
    out
}

fn validate_settings(settings: &DomainSettings) -> Result<(), WebConfigError> {
    let invalid = |field, value: &str| WebConfigError::InvalidSetting { field, value: quoted(value) };

    if let Some(domain) = &settings.domain {
        if !names::is_public_name(domain) {
            return Err(WebConfigError::InvalidName { field: "domain", value: quoted(domain) });
        }
    }
    if let Some(local_domain) = &settings.local_domain {
        if !names::is_public_name(local_domain) {
            return Err(WebConfigError::InvalidName { field: "local_domain", value: quoted(local_domain) });
        }
    }
    // Caddy expands placeholders in most string fields, so the URL gets the same strict treatment.
    let directory = &settings.acme.directory;
    let url_ok = directory.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty()
            && rest.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/' | b':' | b'~'))
    });
    if !url_ok {
        return Err(invalid("acme_directory", directory));
    }
    if let Some(path) = &settings.acme.events_log {
        if !is_safe_path(path) {
            return Err(invalid("events_log", path));
        }
    }
    if let Some(email) = &settings.acme_email {
        if !is_email(email) {
            return Err(invalid("acme_email", email));
        }
    }
    for range in &settings.lan_ranges {
        if !is_ip_range(range) {
            return Err(invalid("lan_ranges", range));
        }
    }

    if !is_safe_path(&settings.tor_dir) {
        return Err(invalid("tor_dir", &settings.tor_dir));
    }
    // Some modules expand `{...}` placeholders in their fields, which could pull in environment values.
    if let Some(challenge) = &settings.dns_challenge {
        if !challenge.usable() {
            return Err(WebConfigError::InvalidSetting { field: "dns_challenge", value: "(credential not shown)".to_owned() });
        }
    }
    let admin_ok = settings.admin_listen.strip_prefix("unix/").is_some_and(is_safe_path);
    if !admin_ok {
        return Err(invalid("admin_listen", &settings.admin_listen));
    }
    Ok(())
}

fn resolve<'a>(site: &'a Site, settings: &DomainSettings) -> Result<Resolved<'a>, WebConfigError> {
    let service = &site.service;
    if !names::is_label(&site.package) {
        return Err(WebConfigError::InvalidName { field: "package", value: quoted(&site.package) });
    }
    if !names::is_label(&service.name) {
        return Err(WebConfigError::InvalidName { field: "site", value: quoted(&service.name) });
    }
    let site_label = format!("{}/{}", site.package, service.name);

    let dial = upstream_dial(site).ok_or_else(|| WebConfigError::InvalidUpstream {
        site: site_label.clone(),
        value: quoted(&service.upstream),
    })?;

    let admin_patterns = service
        .admin_paths
        .iter()
        .map(|path| {
            admin_path_patterns(path).ok_or_else(|| WebConfigError::InvalidAdminPath {
                site: site_label.clone(),
                value: quoted(path),
            })
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    let admin_regexp = admin_path_regexp(&service.admin_paths);

    let enabled = [site.exposure.local, site.exposure.onion, site.exposure.public];
    if !service.multi_address && enabled.iter().filter(|on| **on).count() > 1 {
        return Err(WebConfigError::SingleAddressOnly { site: site_label });
    }

    let label = service.suggested_subdomain.as_deref().unwrap_or(&service.name);
    if !names::is_label(label) {
        return Err(WebConfigError::InvalidName { field: "suggested_subdomain", value: quoted(label) });
    }

    let local = site
        .exposure
        .local
        .then(|| {
            site.local_host.clone().unwrap_or_else(|| match &settings.local_domain {
                Some(local_domain) => format!("{label}.{local_domain}"),
                None => format!("{label}.local"),
            })
        })
        .map(|host| {
            let under_local_domain = settings.local_domain.as_ref().is_some_and(|domain| {
                host.strip_suffix(domain.as_str()).is_some_and(|rest| rest.ends_with('.'))
                    && names::is_public_name(&host)
            });
            if names::is_local_name(&host) || under_local_domain {
                Ok(host)
            } else {
                Err(WebConfigError::InvalidName { field: "local_host", value: quoted(&host) })
            }
        })
        .transpose()?;

    let public = if let Some(host) = site.public_host.as_ref().filter(|_| site.exposure.public) {
        if !names::is_public_name(host) {
            return Err(WebConfigError::InvalidName { field: "public_host", value: quoted(host) });
        }
        Some(host.clone())
    } else if site.exposure.public {
        let domain = settings.domain.as_deref().ok_or(WebConfigError::NoDomain { site: site_label })?;
        let sub = site.public_subdomain.as_deref().unwrap_or(label);
        let host = if sub == APEX { domain.to_owned() } else { format!("{sub}.{domain}") };
        if !names::is_public_name(&host) {
            return Err(WebConfigError::InvalidName { field: "public_subdomain", value: quoted(sub) });
        }
        Some(host)
    } else {
        None
    };

    let onion = if site.exposure.onion {
        site.onion_host
            .as_ref()
            .map(|host| {
                if names::is_onion_name(host) {
                    Ok(host.clone())
                } else {
                    Err(WebConfigError::InvalidName { field: "onion_host", value: quoted(host) })
                }
            })
            .transpose()?
    } else {
        None
    };

    Ok(Resolved { site, dial, admin_patterns, admin_regexp, local, public, onion_enabled: site.exposure.onion, onion })
}

fn check_unique(resolved: &[Resolved<'_>]) -> Result<(), WebConfigError> {
    let mut sites = BTreeSet::new();
    let mut hosts = BTreeSet::new();
    let mut upstreams: HashMap<&str, &str> = HashMap::new();
    for item in resolved {
        let site = item.site;
        if !sites.insert((&site.package, &site.service.name)) {
            return Err(WebConfigError::DuplicateSite {
                package: site.package.clone(),
                site: site.service.name.clone(),
            });
        }
        for host in [&item.local, &item.public, &item.onion].into_iter().flatten() {
            if !hosts.insert(host) {
                return Err(WebConfigError::DuplicateHost { host: host.clone() });
            }
        }
        if let Some(port) = tcp_port(&item.dial).filter(|port| LISTENER_PORTS.contains(port)) {
            return Err(WebConfigError::ReservedPort {
                site: format!("{}/{}", site.package, site.service.name),
                port,
            });
        }
        // Stops a package from pointing its public site at another package's private port.
        if let Some(first) = upstreams.insert(&item.dial, &site.package) {
            if first != site.package {
                return Err(WebConfigError::SharedUpstream {
                    first: first.to_owned(),
                    second: site.package.clone(),
                    upstream: item.dial.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Returns the Caddy dial address, allowing only the two forms the manifest documents. A unix socket
/// must be the site's own path, so a package cannot name another package's socket.
fn upstream_dial(site: &Site) -> Option<String> {
    let upstream = &site.service.upstream;
    if let Some(path) = upstream.strip_prefix("unix:") {
        let expected = format!("/run/apps/{}/{}.sock", site.package, site.service.name);
        return (path == expected).then(|| format!("unix/{path}"));
    }
    let port = upstream.strip_prefix("127.0.0.1:")?;
    let parsed: u16 = port.parse().ok()?;
    // Comparing the round trip rejects forms like `+80` and `080` that parse but are not canonical.
    (parsed != 0 && parsed.to_string() == port).then(|| upstream.clone())
}

fn tcp_port(dial: &str) -> Option<u16> {
    dial.strip_prefix("127.0.0.1:")?.parse().ok()
}

/// Caddy's path matcher is exact unless it has a wildcard, and `/admin/*` does not match `/admin`, so
/// both forms are needed. The `;` form covers servers that treat `/admin;x=1` as `/admin`.
fn admin_path_patterns(path: &str) -> Option<Vec<String>> {
    let allowed = path.starts_with('/')
        && !path.contains("//")
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~'))
        && path.split('/').all(|segment| segment != "." && segment != "..");
    if !allowed {
        return None;
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Some(vec!["/*".to_owned()]);
    }
    Some(vec![trimmed.to_owned(), format!("{trimmed}/*"), format!("{trimmed};*")])
}

/// An address Let's Encrypt accepts for its account, in the conservative form the config allows.
pub fn is_email(value: &str) -> bool {
    let Some((user, domain)) = value.split_once('@') else {
        return false;
    };
    !user.is_empty()
        && user.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
        && names::is_public_name(domain)
}

fn range_contains(range: &str, ip: IpAddr) -> bool {
    let (base, prefix) = range.split_once('/').unwrap_or((range, ""));
    match (base.parse::<IpAddr>(), ip) {
        (Ok(IpAddr::V4(base)), IpAddr::V4(ip)) => {
            let bits: u32 = prefix.parse().unwrap_or(32).min(32);
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
            u32::from(base) & mask == u32::from(ip) & mask
        }
        _ => false,
    }
}

/// For the screens: why local sites cannot be served right now, if they cannot.
pub fn local_problem(plan: &WebPlan) -> Option<&LocalUnavailable> {
    match &plan.local.address {
        LocalAddress::Unavailable { reason } if !plan.local.waiting.is_empty() => Some(reason),
        _ => None,
    }
}

fn is_ip_range(value: &str) -> bool {
    let (ip, prefix) = value.split_once('/').unwrap_or((value, ""));
    let Ok(ip) = ip.parse::<std::net::IpAddr>() else {
        return false;
    };
    let max = if ip.is_ipv4() { 32 } else { 128 };
    prefix.is_empty() && !value.contains('/') || prefix.parse::<u8>().is_ok_and(|p| p <= max)
}

/// An absolute path with no whitespace, placeholders or shell-like characters.
fn is_safe_path(value: &str) -> bool {
    value.starts_with('/')
        && !value.contains("..")
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.'))
}

fn allocate_port(next: &mut u32, reserved: &mut BTreeSet<u16>) -> Result<u16, WebConfigError> {
    while let Ok(candidate) = u16::try_from(*next) {
        *next += 1;
        if reserved.insert(candidate) {
            return Ok(candidate);
        }
    }
    Err(WebConfigError::OutOfOnionPorts)
}

fn host_route(matcher: Matcher, routes: Vec<Route>) -> Route {
    Route { matchers: vec![matcher], handle: vec![Handler::Subroute { routes }], terminal: true }
}

fn https_server(listen: String, mut routes: Vec<Route>, hosts: &[String], redirects: bool) -> Server {
    routes.push(drop_connection_route());
    Server {
        listen: vec![listen],
        routes,
        errors: error_routes(),
        tls_connection_policies: vec![TlsConnectionPolicy { matcher: SniMatch { sni: hosts.to_vec() } }],
        automatic_https: AutomaticHttps { disable: false, disable_redirects: !redirects },
        // HTTP/3 would need UDP 443 open as well, and the Web flow only forwards TCP.
        protocols: vec!["h1".to_owned(), "h2".to_owned()],
        // A request must not present one host in SNI and reach another through its Host header.
        strict_sni_host: true,
    }
}

/// Upstreams differ in what they treat as the same path. Some drop a trailing dot or space, and some
/// stop at a NUL, so `/admin.`, `/admin%20` and `/admin%00` can reach `/admin`. This blocks the admin
/// path followed by anything except a letter, digit, `-`, `_` or `~`, at the cost of also refusing
/// names like `/admin.json` publicly. Call only with paths [`admin_path_patterns`] accepted.
fn admin_path_regexp(paths: &[String]) -> Option<String> {
    let alternatives: Vec<String> = paths
        .iter()
        .map(|path| path.trim_end_matches('/'))
        .filter(|path| !path.is_empty())
        .map(|path| path.replace('.', "\\."))
        .collect();
    (!alternatives.is_empty()).then(|| format!("(?i)^(?:{})(?:[^a-z0-9_~-]|$)", alternatives.join("|")))
}

/// The routes inside one host. `hide_admin` is set for public and onion addresses.
fn site_handlers(item: &Resolved<'_>, hide_admin: bool, limit: Option<RateLimit>) -> Vec<Route> {
    let mut routes = Vec::new();
    // Must stay first: it answers the same way whether the upstream is running or not.
    if hide_admin && !item.admin_patterns.is_empty() {
        routes.push(Route {
            matchers: std::iter::once(Matcher { path: item.admin_patterns.clone(), ..Matcher::default() })
                .chain(item.admin_regexp.iter().map(|pattern| Matcher {
                    path_regexp: Some(PathRegexp { name: "admin".to_owned(), pattern: pattern.clone() }),
                    ..Matcher::default()
                }))
                .collect(),
            handle: vec![Handler::StaticResponse {
                status_code: Some("404".to_owned()),
                headers: BTreeMap::new(),
                body: None,
                abort: false,
            }],
            terminal: true,
        });
    }
    if let Some(limit) = limit {
        let zone = RateLimitZone {
            key: "{http.request.remote.host}".to_owned(),
            window: format!("{}s", limit.window_secs),
            max_events: limit.max_events,
            ipv6_prefix: 64,
        };
        // Zones are shared by name across the whole Caddy process, so each site needs its own name.
        let name = format!("{}.{}", item.site.package, item.site.service.name);
        routes.push(Route {
            handle: vec![Handler::RateLimit { rate_limits: BTreeMap::from([(name, zone)]) }],
            ..Route::default()
        });
    }
    let last = match item.site.state {
        ServiceState::Running => Handler::ReverseProxy { upstreams: vec![Upstream { dial: item.dial.clone() }] },
        ServiceState::Stopped | ServiceState::Failed | ServiceState::Locked | ServiceState::Waiting => {
            unavailable_response("503")
        }
    };
    routes.push(Route { handle: vec![last], terminal: true, ..Route::default() });
    routes
}

/// One generic page for every state. Saying "device locked" to the internet would reveal device state,
/// and a per-site page would help link a site's addresses together.
fn unavailable_response(status: &str) -> Handler {
    Handler::StaticResponse {
        status_code: Some(status.to_owned()),
        headers: BTreeMap::from([
            ("Content-Type".to_owned(), vec!["text/html; charset=utf-8".to_owned()]),
            ("Cache-Control".to_owned(), vec!["no-store".to_owned()]),
            ("Retry-After".to_owned(), vec!["60".to_owned()]),
        ]),
        body: Some(UNAVAILABLE_PAGE.to_owned()),
        abort: false,
    }
}

/// Covers an upstream that dies after this config was generated, when the proxy itself returns an error.
fn error_routes() -> ErrorRoutes {
    ErrorRoutes {
        routes: vec![Route {
            handle: vec![unavailable_response("{http.error.status_code}")],
            terminal: true,
            ..Route::default()
        }],
    }
}

/// An unknown host gets nothing, not even a page that confirms this device runs a web server.
fn drop_connection_route() -> Route {
    Route {
        handle: vec![Handler::StaticResponse { status_code: None, headers: BTreeMap::new(), body: None, abort: true }],
        terminal: true,
        ..Route::default()
    }
}

fn onion_server(listen: String, host: &str, routes: Vec<Route>) -> Server {
    Server {
        listen: vec![listen],
        routes: vec![
            host_route(Matcher { host: vec![host.to_owned()], ..Matcher::default() }, routes),
            drop_connection_route(),
        ],
        errors: error_routes(),
        tls_connection_policies: Vec::new(),
        // Tor already encrypts and authenticates the connection. Without this, Caddy would try to get a
        // certificate because the listener is on neither of its default HTTP ports.
        automatic_https: AutomaticHttps { disable: true, disable_redirects: false },
        protocols: vec!["h1".to_owned()],
        strict_sni_host: false,
    }
}

#[cfg(test)]
mod tests;
