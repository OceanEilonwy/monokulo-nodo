//! Tor exposure for the node and the sites, for the TUI's Onion screen.
//!
//! Sites use the saved exposure like every other address (`SetExposure`). The node is not a site:
//! its onion service forwards straight to the node's restricted RPC on loopback, never to a full
//! RPC, which can stop the node and change its settings. cuprated serves only restricted RPC, on
//! 127.0.0.1:18081, and exports that port as its public `restricted-rpc-port` setting. The onion
//! forwards only to a port exported under that name, so a node that does not say which port is
//! restricted is never put on Tor.

use std::path::PathBuf;

use nodo_api::{ApiError, NodeOnion, OnionStatus, Response, SiteOnion};
use serde::{Deserialize, Serialize};

use crate::catalog::Package;
use crate::helper::{lock, Helper};
use crate::system::{System, WriteOptions};

/// cuprated's restricted RPC port, shown for a node that exports none.
pub const RESTRICTED_RPC_PORT: u16 = 18081;
/// Where the node's onion service lives under the helper's Tor directory. Site services are
/// named `<package>.<site>`, always with a dot, so this name can never clash with one.
pub const NODE_ONION_DIR: &str = "node-rpc";

/// What the web side needs to serve the node on an onion address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeOnionTarget {
    pub provider: String,
    /// The restricted RPC port on loopback. Also the onion address's port, as wallets expect.
    pub port: u16,
}

/// The addresses Tor made, as the web side knows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OnionAddresses {
    pub tor_installed: bool,
    pub tor_running: bool,
    /// `host.onion` of the node's service.
    pub node: Option<String>,
    /// `http://host.onion` by `(package, site)`.
    pub sites: Vec<((String, String), String)>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct NodeOnionFile {
    enabled: bool,
}

const STATE_FILE: WriteOptions = WriteOptions {
    mode: 0o644,
    group: None,
};

impl<S: System> Helper<S> {
    pub(crate) fn node_onion_path(&self) -> PathBuf {
        self.paths.exposure_state.with_file_name("node-onion.toml")
    }

    fn node_onion_wanted(&self) -> Result<bool, ApiError> {
        let Some(bytes) = self.sys.read_file(&self.node_onion_path())? else {
            return Ok(false);
        };
        let file: NodeOnionFile = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| toml::from_str(t).ok())
            .ok_or_else(|| ApiError::Failed("the node onion setting is corrupt".to_owned()))?;
        Ok(file.enabled)
    }

    /// The node's restricted RPC port on loopback, from its public `restricted-rpc-port`
    /// setting, which cuprated always exports. `None` for a node that does not, which therefore
    /// never goes on Tor: the only port it exports might be a full RPC.
    fn restricted_rpc_port(&self, provider: &Package) -> Option<u16> {
        provider.manifest.settings.as_ref()?;
        let (packages, _) = self.packages().ok()?;
        self.with_settings(&packages, |ctx| self.settings.public_value(ctx, provider, "restricted-rpc-port"))
            .ok()
            .flatten()
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p > 0)
    }

    /// The node's onion target while it is wanted.
    pub(crate) fn node_onion_target(&self) -> Option<NodeOnionTarget> {
        if !self.node_onion_wanted().unwrap_or(false) {
            return None;
        }
        let provider = self.node_provider().ok()?;
        let port = self.restricted_rpc_port(&provider)?;
        Some(NodeOnionTarget { provider: provider.id().to_owned(), port })
    }

    pub(crate) fn onion_status(&self) -> Result<Response, ApiError> {
        let input = self.current_web_input()?;
        let found = self.exposure.onion_addresses(&input)?;
        let node = match self.node_provider() {
            Ok(provider) => {
                let port = self.restricted_rpc_port(&provider);
                let restricted_rpc = port.is_some();
                let port = port.unwrap_or(RESTRICTED_RPC_PORT);
                let enabled = self.node_onion_wanted()?;
                Some(NodeOnion {
                    provider: provider.id().to_owned(),
                    enabled,
                    restricted_rpc,
                    port,
                    address: found
                        .node
                        .clone()
                        .filter(|_| enabled && restricted_rpc)
                        .map(|host| format!("{host}:{port}")),
                })
            }
            Err(_) => None,
        };
        let (packages, _) = self.packages()?;
        let mut sites = Vec::new();
        for site in &input.sites {
            let multi_address = packages
                .iter()
                .find(|p| p.id() == site.package)
                .and_then(|p| p.manifest.web.iter().find(|w| w.name == site.service.name))
                .is_some_and(|w| w.multi_address);
            let key = (site.package.clone(), site.service.name.clone());
            sites.push(SiteOnion {
                id: nodo_api::SiteId { package: key.0.clone(), site: key.1.clone() },
                exposure: site.exposure,
                multi_address,
                locked: site.locked,
                url: found
                    .sites
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, url)| url.clone())
                    .filter(|_| site.exposure.onion),
            });
        }
        Ok(Response::Onion(OnionStatus {
            tor_installed: found.tor_installed,
            tor_running: found.tor_running,
            node,
            sites,
        }))
    }

    /// Turning it on needs a node that exports its restricted RPC port, which cuprated always
    /// does. Turning it off always works.
    pub(crate) fn set_node_onion(&self, enabled: bool) -> Result<Response, ApiError> {
        if enabled {
            let provider = self.node_provider()?;
            if self.restricted_rpc_port(&provider).is_none() {
                return Err(ApiError::Failed(format!(
                    "{} does not say which port serves its restricted RPC, and the onion address \
                     only ever serves the restricted RPC",
                    provider.id()
                )));
            }
        }
        let _guard = lock(&self.service_lock);
        let path = self.node_onion_path();
        let previous = self.sys.read_file(&path)?;
        let text = toml::to_string(&NodeOnionFile { enabled }).map_err(|e| ApiError::Failed(e.to_string()))?;
        if let Some(dir) = path.parent() {
            self.sys.ensure_dir(dir, 0o755, None)?;
        }
        self.sys.atomic_write(&path, text.as_bytes(), STATE_FILE)?;
        let applied = self
            .current_web_input()
            .map_err(|e| e.to_string())
            .and_then(|input| self.exposure.apply(&input));
        if let Err(reason) = applied {
            // The saved choice must not claim an onion service that never took effect.
            let _ = match &previous {
                Some(bytes) => self.sys.atomic_write(&path, bytes, STATE_FILE),
                None => self.sys.remove_file(&path),
            };
            return Err(ApiError::Failed(format!("could not apply the node's onion service: {reason}")));
        }
        Ok(Response::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exposure::{ExposureApplier, WebInput};
    use crate::fake::{FakeSystem, TestPolicy, SHOP, TEST_PATHS};
    use crate::helper::AllowedUids;
    use nodo_api::{Exposure, Request, SiteId};
    use std::sync::{Arc, Mutex};

    const NODE: &str = r#"api_version = 1
id = "node"
version = "1"
exec = ["bin/node"]
provides = ["monero-daemon"]
settings = "settings.lua"

[[network.inbound]]
port = 18081
protocol = "tcp"
scope = "local"
"#;

    /// Records what it was asked to apply and reports the addresses Tor would have made.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<Option<NodeOnionTarget>>>>);

    impl ExposureApplier for Recorder {
        fn apply(&self, input: &WebInput) -> Result<(), String> {
            self.0.lock().unwrap().push(input.node_onion.clone());
            Ok(())
        }
        fn onion_addresses(&self, input: &WebInput) -> Result<OnionAddresses, ApiError> {
            Ok(OnionAddresses {
                tor_installed: true,
                tor_running: true,
                node: input.node_onion.as_ref().map(|_| "nodeaddr.onion".to_owned()),
                sites: vec![(("shop".into(), "web".into()), "http://shopaddr.onion".into())],
            })
        }
    }

    fn setup(exports_restricted_port: bool) -> (Helper<FakeSystem>, Recorder) {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        sys.add_manifest(NODE);
        if exports_restricted_port {
            sys.put("/s/public/node/restricted-rpc-port", b"18081");
        }
        let recorder = Recorder::default();
        let h = Helper::new(sys, AllowedUids::new([1000]), Box::new(TestPolicy), Box::new(recorder.clone()))
            .with_paths(TEST_PATHS());
        (h, recorder)
    }

    fn status(h: &Helper<FakeSystem>) -> OnionStatus {
        match h.handle(1000, Request::OnionStatus) {
            Ok(Response::Onion(status)) => status,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_node_that_exports_no_restricted_port_never_goes_on_tor() {
        let (h, recorder) = setup(false);
        let err = h.handle(1000, Request::SetNodeOnion { enabled: true }).unwrap_err();
        assert!(err.to_string().contains("restricted RPC"), "{err}");
        assert!(recorder.0.lock().unwrap().is_empty(), "nothing was applied");
        let node = status(&h).node.unwrap();
        assert!(!node.enabled && !node.restricted_rpc);
        assert_eq!(node.address, None);
        // Turning it off never needs anything.
        h.handle(1000, Request::SetNodeOnion { enabled: false }).unwrap();
    }

    #[test]
    fn the_node_onion_targets_only_the_restricted_port() {
        let (h, recorder) = setup(true);
        h.handle(1000, Request::SetNodeOnion { enabled: true }).unwrap();
        let applied = recorder.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(applied, Some(NodeOnionTarget { provider: "node".into(), port: RESTRICTED_RPC_PORT }));
        let node = status(&h).node.unwrap();
        assert!(node.enabled && node.restricted_rpc);
        assert_eq!(node.address.as_deref(), Some("nodeaddr.onion:18081"));

        h.handle(1000, Request::SetNodeOnion { enabled: false }).unwrap();
        assert_eq!(recorder.0.lock().unwrap().last().cloned().unwrap(), None);
        assert_eq!(status(&h).node.unwrap().address, None);
    }

    #[test]
    fn a_node_that_stops_exporting_its_restricted_port_leaves_tor() {
        let (h, _) = setup(true);
        h.handle(1000, Request::SetNodeOnion { enabled: true }).unwrap();
        assert!(h.node_onion_target().is_some());
        h.sys.remove("/s/public/node/restricted-rpc-port");
        assert_eq!(h.node_onion_target(), None, "the full RPC is never put on Tor");
    }

    #[test]
    fn site_onion_urls_show_only_while_onion_is_on() {
        let (h, _) = setup(false);
        let site = SiteId { package: "shop".into(), site: "web".into() };
        assert!(status(&h).sites.iter().all(|s| s.url.is_none()));
        let exposure = Exposure { local: false, onion: true, public: false };
        h.handle(1000, Request::SetExposure { site: site.clone(), exposure, confirmation: None }).unwrap();
        let sites = status(&h).sites;
        let web = sites.iter().find(|s| s.id == site).unwrap();
        assert!(web.exposure.onion && web.multi_address);
        assert_eq!(web.url.as_deref(), Some("http://shopaddr.onion"));
        let single = sites.iter().find(|s| s.id.site == "single").unwrap();
        assert!(!single.multi_address && single.url.is_none());
    }

    #[test]
    fn a_failed_apply_keeps_the_old_choice() {
        struct Failing;
        impl ExposureApplier for Failing {
            fn apply(&self, _: &WebInput) -> Result<(), String> {
                Err("tor refused".into())
            }
        }
        let sys = FakeSystem::default();
        sys.add_manifest(NODE);
        sys.put("/s/public/node/restricted-rpc-port", b"18081");
        let h = Helper::new(sys, AllowedUids::new([1000]), Box::new(TestPolicy), Box::new(Failing))
            .with_paths(TEST_PATHS());
        assert!(h.handle(1000, Request::SetNodeOnion { enabled: true }).is_err());
        assert_eq!(h.node_onion_target(), None);
    }

    #[test]
    fn other_users_are_denied() {
        let (h, _) = setup(true);
        assert_eq!(h.handle(1001, Request::SetNodeOnion { enabled: true }), Err(ApiError::Denied));
        assert_eq!(h.handle(1001, Request::OnionStatus), Err(ApiError::Denied));
    }
}
