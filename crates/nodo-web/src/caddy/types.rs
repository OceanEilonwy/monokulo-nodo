//! The subset of Caddy's JSON config this crate emits. Field names follow
//! <https://caddyserver.com/docs/json/>. Every module used here ships with standard Caddy, except
//! `rate_limit`, which comes from `github.com/mholt/caddy-ratelimit` in the `nodo-caddy` build.

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaddyConfig {
    pub admin: Admin,
    pub apps: Apps,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Admin {
    pub listen: String,
    pub config: AdminConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdminConfig {
    pub persist: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Apps {
    pub http: HttpApp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsApp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pki: Option<PkiApp>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HttpApp {
    /// The ports Caddy itself binds for plain HTTP and HTTPS. nftables redirects 80 and 443 here.
    pub http_port: u16,
    pub https_port: u16,
    pub servers: BTreeMap<String, Server>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Server {
    pub listen: Vec<String>,
    pub routes: Vec<Route>,
    pub errors: ErrorRoutes,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tls_connection_policies: Vec<TlsConnectionPolicy>,
    pub automatic_https: AutomaticHttps,
    pub protocols: Vec<String>,
    pub strict_sni_host: bool,
}

/// A handshake whose SNI matches no policy is refused, so one listener never presents another
/// listener's certificate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TlsConnectionPolicy {
    #[serde(rename = "match")]
    pub matcher: SniMatch,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SniMatch {
    pub sni: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ErrorRoutes {
    pub routes: Vec<Route>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AutomaticHttps {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disable: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disable_redirects: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Route {
    #[serde(rename = "match", skip_serializing_if = "Vec::is_empty")]
    pub matchers: Vec<Matcher>,
    pub handle: Vec<Handler>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
}

/// All fields in one matcher must match.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Matcher {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub host: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_regexp: Option<PathRegexp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_ip: Option<RemoteIp>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PathRegexp {
    pub name: String,
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RemoteIp {
    pub ranges: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "handler", rename_all = "snake_case")]
pub enum Handler {
    Subroute {
        routes: Vec<Route>,
    },
    ReverseProxy {
        upstreams: Vec<Upstream>,
    },
    StaticResponse {
        #[serde(skip_serializing_if = "Option::is_none")]
        status_code: Option<String>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        abort: bool,
    },
    RateLimit {
        rate_limits: BTreeMap<String, RateLimitZone>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RateLimitZone {
    pub key: String,
    pub window: String,
    pub max_events: u32,
    /// Groups an IPv6 client's whole /64, since one home connection can use any address in it.
    pub ipv6_prefix: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Upstream {
    pub dial: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TlsApp {
    pub automation: TlsAutomation,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TlsAutomation {
    pub policies: Vec<AutomationPolicy>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AutomationPolicy {
    pub subjects: Vec<String>,
    pub issuers: Vec<Issuer>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "module", rename_all = "snake_case")]
pub enum Issuer {
    Acme {
        #[serde(skip_serializing_if = "Option::is_none")]
        email: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        challenges: Option<AcmeChallenges>,
    },
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AcmeChallenges {
    pub dns: DnsChallengeConfig,
}

/// `provider` holds credentials. Keep it out of logs and give the config file to Caddy's user only.
#[derive(Clone, PartialEq, Serialize)]
pub struct DnsChallengeConfig {
    pub provider: serde_json::Value,
}

impl std::fmt::Debug for DnsChallengeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DnsChallengeConfig").field("provider", &self.provider["name"]).finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PkiApp {
    pub certificate_authorities: BTreeMap<String, CertificateAuthority>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CertificateAuthority {
    pub install_trust: bool,
}
