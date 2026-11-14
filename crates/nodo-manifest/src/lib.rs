//! The package manifest: what a package declares about itself.
//!
//! Every generated artifact (sandbox, firewall, proxy config, TUI forms) derives from this,
//! so packages never write system configuration themselves.

mod validate;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use validate::{
    is_valid_config_name, is_valid_id, is_valid_name, is_valid_package_path, is_valid_version, ManifestIssue, MAX_ID_LEN,
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
    /// A TOML template, filled with this package's resolved settings, that becomes the
    /// package's own configuration file. Without it, settings are written as a flat table.
    #[serde(default)]
    pub config: Option<ConfigFile>,
    #[serde(default)]
    pub storage: Storage,
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub web: Vec<WebService>,
    /// Hardware the package asks to use. Each is a permission the owner approves.
    #[serde(default, skip_serializing_if = "Devices::is_empty")]
    pub devices: Devices,
    /// How long a clean stop may take before the supervisor kills the service.
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout_secs: u32,
    /// Local URL or command used to decide whether the service is healthy.
    #[serde(default)]
    pub health: Option<HealthCheck>,
    /// Panels the package offers for the device's screen (docs/SCREEN.md).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panels: Vec<Panel>,
}

/// A panel for the device's screen: where its data comes from and the Lua file that turns the data
/// into the panel's values. The screen fetches and draws; the package only computes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    /// Unique within the package.
    pub id: String,
    /// Shown in the panel's border.
    pub title: String,
    /// Path of the Lua file, relative to the package directory.
    pub script: String,
    pub refresh_secs: u32,
    /// `[columns, rows]` on the screen's grid.
    pub min_size: [u8; 2],
    pub max_size: [u8; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_size: Option<[u8; 2]>,
    pub source: PanelSource,
}

impl Panel {
    /// The size a panel takes when the screen first places it.
    pub fn initial_size(&self) -> [u8; 2] {
        self.default_size.unwrap_or(self.min_size)
    }
}

/// Where a panel's data comes from. Always the package's own service on loopback, except
/// `Device`, which only the base system may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum PanelSource {
    /// A GET whose response is JSON.
    Http { url: String },
    /// A JSON-RPC 2.0 call without parameters; the panel gets its `result`.
    Jsonrpc { url: String, method: String },
    /// The device's own temperature, storage and memory, read by the screen.
    Device {},
}

/// The screen's grid, in columns and rows.
pub const PANEL_GRID: [u8; 2] = [8, 4];
pub const MAX_PANELS: usize = 8;
pub const MIN_PANEL_REFRESH_SECS: u32 = 2;
pub const MAX_PANEL_REFRESH_SECS: u32 = 3600;
pub const MAX_PANEL_TITLE_LEN: usize = 24;

fn default_stop_timeout() -> u32 {
    120
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Path of the template, relative to the package directory.
    pub template: String,
    /// File name written in the package's config directory, such as `Cuprated.toml`.
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    /// Encrypted storage opens with the single device passphrase after a reboot.
    #[serde(default)]
    pub encrypted: bool,
    /// The encrypted volume's size in MiB, [`DEFAULT_VOLUME_MIB`] when not given. An update that
    /// asks for more grows the volume in place. A volume never shrinks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_mib: Option<u32>,
    /// The version of the package's own data layout, 1 when not given. A new version that raises
    /// it converts the data itself on its first start. The helper records the layout the data is
    /// in, keeps a copy of the data for a rollback, and never starts a version on data in a newer
    /// layout than it declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_layout: Option<u32>,
}

/// The encrypted volume size of a package that does not ask for one.
pub const DEFAULT_VOLUME_MIB: u32 = 256;
/// Bounds for `storage.size_mib`: room for a filesystem, and at most 1 TiB.
pub const MIN_VOLUME_MIB: u32 = 32;
pub const MAX_VOLUME_MIB: u32 = 1024 * 1024;

impl Storage {
    /// The volume size this version asks for, in bytes.
    pub fn volume_bytes(&self) -> u64 {
        u64::from(self.size_mib.unwrap_or(DEFAULT_VOLUME_MIB)) * 1024 * 1024
    }

    pub fn layout(&self) -> u32 {
        self.data_layout.unwrap_or(1)
    }
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

/// Devices a sandbox may reach. Nothing by default: a service's `/dev` holds only the basic
/// pseudo-devices.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Devices {
    /// The GPU's render nodes (`/dev/dri/renderD*`), for compute through Vulkan. Never the card
    /// nodes, which drive displays.
    #[serde(default)]
    pub gpu: bool,
}

impl Devices {
    pub fn is_empty(&self) -> bool {
        !self.gpu
    }
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
    /// How the proxy speaks to the upstream. Most apps speak HTTP/1.1; an app that speaks only
    /// HTTP/2 with prior knowledge (no TLS, no upgrade) declares `h2c`.
    #[serde(default, skip_serializing_if = "UpstreamProtocol::is_default")]
    pub upstream_protocol: UpstreamProtocol,
}

/// The HTTP version the proxy uses towards a web upstream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpstreamProtocol {
    /// HTTP/1.1, the default.
    #[default]
    H1,
    /// HTTP/2 over cleartext with prior knowledge. The proxy also passes every chunk on as soon
    /// as it arrives, in both directions, since apps that need HTTP/2 often stream.
    H2c,
}

impl UpstreamProtocol {
    fn is_default(&self) -> bool {
        *self == UpstreamProtocol::H1
    }
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

    const PANEL_BASE: &str = "api_version = 1\nid = \"shop\"\nversion = \"1.0\"\nexec = [\"bin/shop\"]\n\
        health = { http = { url = \"http://127.0.0.1:8081/status\" } }\n";

    fn panel(body: &str) -> Result<Manifest, ManifestError> {
        Manifest::parse(&format!("{PANEL_BASE}[[panels]]\n{body}"))
    }

    const GOOD_PANEL: &str = "id = \"sales\"\ntitle = \"Sales today\"\nscript = \"panels/sales.lua\"\n\
        refresh_secs = 10\nmin_size = [2, 1]\nmax_size = [4, 2]\n";

    #[test]
    fn panels_parse_with_a_source_of_the_packages_own() {
        let m = panel(&format!("{GOOD_PANEL}source = {{ http = {{ url = \"http://127.0.0.1:8081/stats\" }} }}\n")).unwrap();
        let p = &m.panels[0];
        assert_eq!(p.initial_size(), [2, 1]);
        assert_eq!(p.source, PanelSource::Http { url: "http://127.0.0.1:8081/stats".into() });
        let m = panel(&format!(
            "{GOOD_PANEL}default_size = [4, 2]\n[panels.source.jsonrpc]\nurl = \"http://localhost:8081/json_rpc\"\nmethod = \"get_info\"\n"
        ))
        .unwrap();
        assert_eq!(m.panels[0].initial_size(), [4, 2]);
        // It survives serializing, as the helper stores manifests.
        assert_eq!(Manifest::parse(&toml::to_string(&m).unwrap()).unwrap(), m);
        assert!(Manifest::parse(PANEL_BASE).unwrap().panels.is_empty(), "no panels unless declared");
    }

    #[test]
    fn a_panel_reads_only_its_own_package_on_loopback() {
        for url in [
            "http://127.0.0.1:18081/json_rpc", // another package's port
            "http://10.0.0.5:8081/stats",       // not loopback
            "https://127.0.0.1:8081/stats",     // not plain http
            "http://127.0.0.1/stats",           // no explicit port
            "http://127.0.0.1:80/stats",        // below the package range
            "http://127.0.0.1:8081/../x",       // not a clean path
        ] {
            let body = format!("{GOOD_PANEL}source = {{ http = {{ url = \"{url}\" }} }}\n");
            assert!(panel(&body).is_err(), "{url}");
        }
        // The base system's device source is refused from a feed manifest.
        assert!(panel(&format!("{GOOD_PANEL}source = {{ device = {{}} }}\n")).is_err());
        let system = Manifest::parse_system(&format!("{PANEL_BASE}[[panels]]\n{GOOD_PANEL}source = {{ device = {{}} }}\n"));
        assert_eq!(system.unwrap().panels[0].source, PanelSource::Device {});
    }

    #[test]
    fn panel_fields_have_bounds() {
        let source = "source = { http = { url = \"http://127.0.0.1:8081/stats\" } }\n";
        let bad = [
            GOOD_PANEL.replace("id = \"sales\"", "id = \"Sales!\""),
            GOOD_PANEL.replace("Sales today", "\\u001b[2Jclear"),
            GOOD_PANEL.replace("Sales today", "a title that is far too long to fit"),
            GOOD_PANEL.replace("panels/sales.lua", "../sales.lua"),
            GOOD_PANEL.replace("panels/sales.lua", "panels/sales.sh"),
            GOOD_PANEL.replace("refresh_secs = 10", "refresh_secs = 1"),
            GOOD_PANEL.replace("max_size = [4, 2]", "max_size = [9, 2]"),
            GOOD_PANEL.replace("min_size = [2, 1]", "min_size = [0, 1]"),
            GOOD_PANEL.replace("max_size = [4, 2]", "max_size = [1, 1]"),
            format!("{GOOD_PANEL}default_size = [8, 4]\n"),
        ];
        for body in bad {
            assert!(panel(&format!("{body}{source}")).is_err(), "{body}");
        }
        let method = |m: &str| {
            panel(&format!("{GOOD_PANEL}[panels.source.jsonrpc]\nurl = \"http://127.0.0.1:8081/rpc\"\nmethod = \"{m}\"\n"))
        };
        assert!(method("get_info").is_ok());
        assert!(method("get info; rm").is_err());
        assert!(method("").is_err());
        let twice = format!("{GOOD_PANEL}{source}[[panels]]\n{GOOD_PANEL}{source}");
        assert!(panel(&twice).is_err(), "duplicate ids");
        let many: String = (0..9)
            .map(|i| format!("[[panels]]\n{}{source}", GOOD_PANEL.replace("\"sales\"", &format!("\"p{i}\""))))
            .collect();
        assert!(Manifest::parse(&format!("{PANEL_BASE}{many}")).is_err(), "more than 8 panels");
    }

    #[test]
    fn parses_minimal_manifest_with_safe_defaults() {
        let m = Manifest::parse(
            "api_version = 1\nid = \"shop\"\nversion = \"1.0\"\nexec = [\"bin/shop\"]\n",
        )
        .unwrap();
        assert_eq!(m.network.outbound, Outbound::None);
        assert!(!m.storage.encrypted);
        assert_eq!(m.stop_timeout_secs, 120);
        assert!(!m.devices.gpu, "no device unless asked for");
    }

    #[test]
    fn the_gpu_is_asked_for_in_devices() {
        let parse = |extra: &str| {
            Manifest::parse(&format!("api_version = 1\nid = \"shop\"\nversion = \"1.0\"\nexec = [\"bin/shop\"]\n{extra}"))
        };
        assert!(parse("[devices]\ngpu = true\n").unwrap().devices.gpu);
        assert!(!parse("[devices]\ngpu = false\n").unwrap().devices.gpu);
        assert!(!parse("[devices]\n").unwrap().devices.gpu);
        // Only known devices, and never a device path of the package's choosing.
        for bad in ["[devices]\ncard = true\n", "[devices]\ngpu = \"/dev/dri/card0\"\n", "[devices]\npaths = [\"/dev/mem\"]\n"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let m = parse("[devices]\ngpu = true\n").unwrap();
        let text = toml::to_string(&m).unwrap();
        assert_eq!(Manifest::parse(&text).unwrap(), m);
    }

    #[test]
    fn storage_size_and_layout_have_defaults_and_bounds() {
        let parse = |storage: &str| {
            Manifest::parse(&format!(
                "api_version = 1\nid = \"shop\"\nversion = \"1.0\"\nexec = [\"bin/shop\"]\n[storage]\n{storage}"
            ))
        };
        let m = parse("encrypted = true\n").unwrap();
        assert_eq!(m.storage.volume_bytes(), 256 * 1024 * 1024);
        assert_eq!(m.storage.layout(), 1);
        let m = parse("encrypted = true\nsize_mib = 512\ndata_layout = 2\n").unwrap();
        assert_eq!(m.storage.volume_bytes(), 512 * 1024 * 1024);
        assert_eq!(m.storage.layout(), 2);
        for bad in ["size_mib = 512\n", "encrypted = true\nsize_mib = 8\n", "encrypted = true\ndata_layout = 0\n"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
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
            upstream_protocol: UpstreamProtocol::H1,
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
    fn the_projects_apps_use_the_mk_prefix_and_no_feed_app_may_be_monokulo() {
        let mut m = base();
        for id in ["mk-pay", "mk-bell", "mk-push-relay", "mk-lws"] {
            m.id = id.into();
            assert!(m.validate().is_empty(), "{id}");
        }
        m.id = "monokulo".into();
        assert_eq!(fields(&m), ["id"], "reserved, so no feed app can pass itself off as the project");
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
    fn config_template_must_stay_inside_the_package() {
        let ok = Manifest::parse(
            "api_version = 1\nid = \"shop\"\nversion = \"1\"\nexec = [\"bin/shop\"]\nsettings = \"s.lua\"\n[config]\ntemplate = \"conf/shop.toml\"\nname = \"shop.toml\"\n",
        )
        .unwrap();
        assert_eq!(ok.config.unwrap().name, "shop.toml");
        for (template, name) in [
            ("../x.toml", "shop.toml"),
            ("/etc/x.toml", "shop.toml"),
            ("x.toml", "../shop.toml"),
            ("x.toml", "a/b.toml"),
            ("x.toml", ".settings.toml"),
            ("x.toml", ""),
        ] {
            let mut m = base();
            m.settings = Some("s.lua".into());
            m.config = Some(ConfigFile { template: template.into(), name: name.into() });
            assert!(!m.validate().is_empty(), "{template} {name}");
        }
        let mut m = base();
        m.config = Some(ConfigFile { template: "x.toml".into(), name: "x.toml".into() });
        assert_eq!(fields(&m), ["config"]);
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
    fn web_upstreams_speak_http1_unless_they_ask_for_h2c() {
        let parse = |extra: &str| {
            Manifest::parse(&format!(
                "api_version = 1\nid = \"shop\"\nversion = \"1.0\"\nexec = [\"bin/shop\"]\n\
                 [[web]]\nname = \"api\"\nupstream = \"127.0.0.1:8080\"\n{extra}"
            ))
        };
        assert_eq!(parse("").unwrap().web[0].upstream_protocol, UpstreamProtocol::H1);
        assert_eq!(parse("upstream_protocol = \"h1\"\n").unwrap().web[0].upstream_protocol, UpstreamProtocol::H1);
        let h2c = parse("upstream_protocol = \"h2c\"\n").unwrap();
        assert_eq!(h2c.web[0].upstream_protocol, UpstreamProtocol::H2c);
        for bad in ["\"h2\"", "\"h3\"", "\"H2C\"", "true"] {
            assert!(parse(&format!("upstream_protocol = {bad}\n")).is_err(), "{bad}");
        }
        // Stored manifests round-trip, and an h1 site serializes exactly as before this field.
        assert_eq!(Manifest::parse(&toml::to_string(&h2c).unwrap()).unwrap(), h2c);
        assert!(!toml::to_string(&parse("").unwrap()).unwrap().contains("upstream_protocol"));
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
