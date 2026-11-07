//! The package manifest: what a package declares about itself.
//!
//! Every generated artifact (sandbox, firewall, proxy config, TUI forms) derives from this,
//! so packages never write system configuration themselves.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The only manifest API version this build understands.
pub const API_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid manifest: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("manifest uses API version {found}, this device supports {API_VERSION}")]
    UnsupportedApi { found: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub api_version: u32,
    pub id: String,
    pub version: String,
    /// Executable path, always inside the package's own directory.
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
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest = toml::from_str(text)?;
        if manifest.api_version != API_VERSION {
            return Err(ManifestError::UnsupportedApi { found: manifest.api_version });
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
}
