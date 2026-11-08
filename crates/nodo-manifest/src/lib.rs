//! The package manifest: what a package declares about itself.
//!
//! Every generated artifact (sandbox, firewall, proxy config, TUI forms) derives from this,
//! so packages never write system configuration themselves.

mod validate;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use validate::{
    is_valid_id, is_valid_name, is_valid_package_path, is_valid_version, ManifestIssue, MAX_ID_LEN,
    MIN_PACKAGE_PORT, RESERVED_IDS,
};

/// The only manifest API version this build understands.
pub const API_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid manifest: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("manifest uses API version {found}, this device supports {API_VERSION}")]
    UnsupportedApi { found: u32 },
    #[error("invalid manifest: {}", join_issues(.0))]
    Invalid(Vec<ManifestIssue>),
}

fn join_issues(issues: &[ManifestIssue]) -> String {
    issues.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub api_version: u32,
    pub id: String,
    pub version: String,
    /// Executable path, always inside the package's own directory, followed by its arguments.
    /// Arguments may contain `$(package.setting)` references.
    pub exec: Vec<String>,
    /// Roles or capabilities required, such as `daemon-rpc`.
    #[serde(default)]
    pub needs: Vec<String>,
    /// Roles or capabilities offered, such as `monero-daemon`.
    #[serde(default)]
    pub provides: Vec<String>,
    /// Path of the Lua settings file, relative to the package directory.
    #[serde(default)]
    pub settings: Option<String>,
    #[serde(default)]
    pub storage: Storage,
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub web: Vec<WebService>,
    /// How long a clean stop may take before the supervisor kills the service.
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout_secs: u32,
    /// Local URL or command used to decide whether the service is healthy.
    #[serde(default)]
    pub health: Option<HealthCheck>,
}

fn default_stop_timeout() -> u32 {
    120
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    /// Encrypted storage opens with the single device passphrase after a reboot.
    #[serde(default)]
    pub encrypted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    #[serde(default)]
    pub inbound: Vec<Inbound>,
    /// Outbound internet access. Local dependencies are granted through references.
    #[serde(default)]
    pub outbound: Outbound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inbound {
    pub port: u16,
    pub protocol: Protocol,
    pub scope: Scope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Local,
    Lan,
    Internet,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outbound {
    #[default]
    None,
    Internet,
}

/// A site the package serves. The user, never the package, chooses its exposure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebService {
    pub name: String,
    /// `unix:/run/apps/<id>/<name>.sock` or `127.0.0.1:<port>`.
    pub upstream: String,
    #[serde(default)]
    pub suggested_subdomain: Option<String>,
    /// Paths that must never be reachable from the public address.
    #[serde(default)]
    pub admin_paths: Vec<String>,
    /// False when the app is tied to a single base URL.
    #[serde(default)]
    pub multi_address: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum HealthCheck {
    Http { url: String },
    Tcp { port: u16 },
}

impl Manifest {
    /// Parses a manifest shipped in the signed base system, such as cuprated's, which legitimately
    /// uses system paths and reserved ids. Never use this for anything from a feed.
    pub fn parse_system(text: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest = toml::from_str(text)?;
        if manifest.api_version != API_VERSION {
            return Err(ManifestError::UnsupportedApi { found: manifest.api_version });
        }
        Ok(manifest)
    }

    /// Parses and validates a manifest. A manifest returned from here has passed every check.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest = toml::from_str(text)?;
        if manifest.api_version != API_VERSION {
            return Err(ManifestError::UnsupportedApi { found: manifest.api_version });
        }
        let issues = manifest.validate();
        if !issues.is_empty() {
            return Err(ManifestError::Invalid(issues));
        }
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_manifest_with_safe_defaults() {
        let m = Manifest::parse(
            "api_version = 1\nid = \"shop\"\nversion = \"1.0\"\nexec = [\"bin/shop\"]\n",
        )
        .unwrap();
        assert_eq!(m.network.outbound, Outbound::None);
        assert!(!m.storage.encrypted);
        assert_eq!(m.stop_timeout_secs, 120);
    }

    #[test]
    fn rejects_unknown_api_version() {
        let e = Manifest::parse("api_version = 2\nid = \"a\"\nversion = \"1\"\nexec = [\"a\"]\n");
        assert!(matches!(e, Err(ManifestError::UnsupportedApi { found: 2 })));
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(Manifest::parse(
            "api_version = 1\nid = \"a\"\nversion = \"1\"\nexec = [\"a\"]\nsetuid = true\n"
        )
        .is_err());
    }

    fn base() -> Manifest {
        Manifest::parse("api_version = 1\nid = \"shop\"\nversion = \"1.0\"\nexec = [\"bin/shop\"]\n").unwrap()
    }

    fn fields(manifest: &Manifest) -> Vec<String> {
        manifest.validate().into_iter().map(|issue| issue.field).collect()
    }

    fn web(name: &str, upstream: &str) -> WebService {
        WebService {
            name: name.into(),
            upstream: upstream.into(),
            suggested_subdomain: None,
            admin_paths: vec![],
            multi_address: false,
        }
    }

    #[test]
    fn accepts_a_full_manifest() {
        let m = Manifest::parse(
            r#"
            api_version = 1
            id = "btcpay-lite"
            version = "1.2.3+rev~1"
            exec = ["bin/shop", "--rpc=$(monero-daemon.rpc-url)", "--config", "/run/apps/btcpay-lite/shop.toml"]
            needs = ["monero-daemon"]
            provides = ["pos"]
            settings = "settings/main.lua"
            stop_timeout_secs = 30
            health = { http = { url = "http://127.0.0.1:8080/health" } }
            [[network.inbound]]
            port = 18080
            protocol = "tcp"
            scope = "internet"
            [[network.inbound]]
            port = 18080
            protocol = "udp"
            scope = "lan"
            [[web]]
            name = "store"
            upstream = "unix:/run/apps/btcpay-lite/store.sock"
            suggested_subdomain = "pay"
            admin_paths = ["/admin", "/admin/*", "/.well-known/x", "/*"]
            [[web]]
            name = "api"
            upstream = "127.0.0.1:8080"
            "#,
        )
        .unwrap();
        assert!(m.validate().is_empty());
    }

    #[test]
    fn rejects_bad_ids() {
        let long = "a".repeat(MAX_ID_LEN + 1);
        for id in [
            "",
            "Shop",
            "1shop",
            "-shop",
            "shop-",
            "sh--op",
            "sh_op",
            "sh.op",
            "../shop",
            "sh op",
            "shop\n",
            "shöp",
            "root",
            "nobody",
            "monero-daemon",
            "nodo",
            long.as_str(),
        ] {
            let mut m = base();
            m.id = id.into();
            assert_eq!(fields(&m), ["id"], "id {id:?} should be rejected");
        }
    }

    #[test]
    fn rejects_exec_escaping_the_package() {
        for path in [
            "",
            "/bin/sh",
            "../bin/sh",
            "bin/../../sh",
            "./bin/shop",
            "bin//shop",
            "bin/shop/",
            "..",
            ".",
            "bin\\shop",
            "bin/sh op",
            "-rf",
            "bin/-x",
            "bin/shop\0",
            "bin/$(a.b)",
            "bin/ünicode",
        ] {
            let mut m = base();
            m.exec = vec![path.into()];
            assert_eq!(fields(&m), ["exec[0]"], "exec {path:?} should be rejected");
        }
        let mut m = base();
        m.exec = vec![];
        assert_eq!(fields(&m), ["exec"]);
    }

    #[test]
    fn the_payment_gateway_app_may_use_its_own_name() {
        let mut m = base();
        m.id = "monokulo".into();
        assert!(m.validate().is_empty());
    }

    #[test]
    fn rejects_exec_arguments_naming_outside_paths() {
        for arg in [
            "../secret",
            "--config=../../etc/passwd",
            "/etc/shadow",
            "--config=/etc/shadow",
            "x:/etc/shadow",
            "/opt/nodo/apps/other/x",
            "/opt/nodo/apps/shop/../other/x",
            "bad\narg",
            "nul\0arg",
        ] {
            let mut m = base();
            m.exec.push(arg.into());
            assert_eq!(fields(&m), ["exec[1]"], "argument {arg:?} should be rejected");
        }
        let mut m = base();
        m.exec.push("--config=/opt/nodo/apps/shop/shop.toml".into());
        m.exec.push("$(monero-daemon.rpc-url)".into());
        m.exec.push("--peer=http://127.0.0.1:18081/rpc".into());
        assert!(m.validate().is_empty());
    }

    #[test]
    fn rejects_settings_paths_escaping_the_package() {
        for path in ["/etc/settings.lua", "../settings.lua", "a/../../settings.lua", "settings.txt", ""] {
            let mut m = base();
            m.settings = Some(path.into());
            assert_eq!(fields(&m), ["settings"], "settings {path:?} should be rejected");
        }
    }

    #[test]
    fn rejects_bad_versions() {
        for version in ["", "1.0_1", "1-0", "abc", ".1", "1 0", "1;rm", "1\n"] {
            let mut m = base();
            m.version = version.into();
            assert_eq!(fields(&m), ["version"], "version {version:?} should be rejected");
        }
    }

    #[test]
    fn rejects_privileged_and_duplicate_ports() {
        let inbound = |port| Inbound { port, protocol: Protocol::Tcp, scope: Scope::Lan };
        for port in [0, 22, 80, 443, 1023] {
            let mut m = base();
            m.network.inbound = vec![inbound(port)];
            assert_eq!(fields(&m), ["network.inbound[0].port"], "port {port} should be rejected");
        }
        let mut m = base();
        m.network.inbound = vec![inbound(18080), inbound(18080)];
        assert_eq!(fields(&m), ["network.inbound[1].port"]);
    }

    #[test]
    fn upstream_must_be_this_packages_socket_or_loopback() {
        for upstream in [
            "unix:/run/apps/other/store.sock",
            "unix:/run/apps/shop/other.sock",
            "unix:/run/apps/shop/../other/store.sock",
            "unix:/var/run/docker.sock",
            "/run/apps/shop/store.sock",
            "0.0.0.0:8080",
            "10.0.0.1:8080",
            "localhost:8080",
            "127.0.0.1:",
            "127.0.0.1:80",
            "127.0.0.1:0",
            "127.0.0.1:+8080",
            "127.0.0.1:08080",
            "127.0.0.1:65536",
            "127.0.0.1:8080/x",
            "127.0.0.1:8080\nreverse_proxy evil",
            "http://127.0.0.1:8080",
        ] {
            let mut m = base();
            m.web = vec![web("store", upstream)];
            assert_eq!(fields(&m), ["web[0].upstream"], "upstream {upstream:?} should be rejected");
        }
        let mut m = base();
        m.web = vec![web("store", "unix:/run/apps/shop/store.sock"), web("api", "127.0.0.1:8080")];
        assert!(m.validate().is_empty());
    }

    #[test]
    fn rejects_bad_web_names_subdomains_and_admin_paths() {
        for name in ["", "Store", "a b", "../x", "a.b", "a\n", "-a", "a-"] {
            let mut m = base();
            m.web = vec![web(name, "127.0.0.1:8080")];
            let found = fields(&m);
            assert!(found.contains(&"web[0].name".to_string()), "web name {name:?} should be rejected");
        }
        let mut m = base();
        m.web = vec![web("a", "127.0.0.1:8080"), web("a", "127.0.0.1:8081")];
        assert_eq!(fields(&m), ["web[1].name"]);

        let mut m = base();
        m.web = vec![web("a", "127.0.0.1:8080")];
        m.web[0].suggested_subdomain = Some("evil.example.com".into());
        assert_eq!(fields(&m), ["web[0].suggested_subdomain"]);

        for path in ["admin", "/a/../b", "/a b", "/a\n}", "/a?b", "/a#b", "/%2e%2e", "/a*", "/*/a", "//a", ""] {
            let mut m = base();
            m.web = vec![web("a", "127.0.0.1:8080")];
            m.web[0].admin_paths = vec![path.into()];
            assert_eq!(fields(&m), ["web[0].admin_paths[0]"], "admin path {path:?} should be rejected");
        }
    }

    #[test]
    fn health_checks_must_stay_local() {
        for url in [
            "https://127.0.0.1:8080/",
            "http://example.com:8080/",
            "http://127.0.0.1.example.com:8080/",
            "http://127.0.0.1@example.com:8080/",
            "http://127.0.0.1/",
            "http://127.0.0.1:80/",
            "http://127.0.0.1:8080/a b",
            "http://127.0.0.1:8080/?x=1",
        ] {
            let mut m = base();
            m.health = Some(HealthCheck::Http { url: url.into() });
            assert_eq!(fields(&m), ["health.http.url"], "health url {url:?} should be rejected");
        }
        let mut m = base();
        m.health = Some(HealthCheck::Http { url: "http://[::1]:8080/healthz".into() });
        assert!(m.validate().is_empty());
        m.health = Some(HealthCheck::Tcp { port: 0 });
        assert_eq!(fields(&m), ["health.tcp.port"]);
    }

    #[test]
    fn rejects_bad_roles_and_stop_timeouts() {
        let mut m = base();
        m.needs = vec!["Bad Role".into(), "ok".into(), "ok".into()];
        m.provides = vec!["a.b".into()];
        m.stop_timeout_secs = 0;
        assert_eq!(fields(&m), ["needs[0]", "needs[2]", "provides[0]", "stop_timeout_secs"]);
    }

    #[test]
    fn parse_reports_every_problem_at_once() {
        let e = Manifest::parse(
            "api_version = 1\nid = \"../x\"\nversion = \"1\"\nexec = [\"/bin/sh\"]\nsettings = \"/etc/x.lua\"\n",
        )
        .unwrap_err();
        let ManifestError::Invalid(issues) = &e else { panic!("{e}") };
        let fields: Vec<_> = issues.iter().map(|issue| issue.field.as_str()).collect();
        assert_eq!(fields, ["id", "exec[0]", "settings"]);
        assert!(e.to_string().contains("exec[0]"));
    }
}
