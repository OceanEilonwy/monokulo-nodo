//! The node's own view of its progress, from the RPC of whichever package provides
//! `monero-daemon`. The TUI is not allowed to reach that port, so the helper asks for it.

use nodo_api::{ApiError, NodeStatus, Response};
use nodo_manifest::{HealthCheck, Protocol};

use crate::catalog::Package;
use crate::helper::Helper;
use crate::system::System;

const ROLE: &str = "monero-daemon";
/// `get_info` is about a kilobyte. Anything far larger is not a node answering.
const MAX_INFO: usize = 64 * 1024;

fn failed(message: impl Into<String>) -> ApiError {
    ApiError::Failed(message.into())
}

/// The parts of `get_info` Home shows. Unknown fields are ignored, since monerod and cuprated
/// both send many more.
#[derive(serde::Deserialize)]
struct Info {
    #[serde(default)]
    status: Option<String>,
    height: u64,
    #[serde(default)]
    target_height: u64,
    #[serde(default)]
    outgoing_connections_count: u32,
    #[serde(default)]
    incoming_connections_count: u32,
    #[serde(default)]
    synchronized: bool,
}

pub(crate) fn parse_info(provider: &str, body: &[u8]) -> Result<NodeStatus, ApiError> {
    let info: Info = serde_json::from_slice(body)
        .map_err(|_| failed(format!("{provider} answered get_info with something else")))?;
    if info.status.as_deref().is_some_and(|s| s != "OK") {
        return Err(failed(format!("{provider} reports it is not ready")));
    }
    Ok(NodeStatus {
        provider: provider.to_owned(),
        height: info.height,
        target_height: info.target_height,
        outgoing_peers: info.outgoing_connections_count,
        incoming_peers: info.incoming_connections_count,
        synchronized: info.synchronized,
    })
}

impl<S: System> Helper<S> {
    /// The provider the user chose, or the only one installed.
    pub(crate) fn node_provider(&self) -> Result<Package, ApiError> {
        let (packages, _) = self.packages()?;
        let bindings = self.load_bindings()?;
        let providers: Vec<&Package> = packages
            .iter()
            .filter(|p| p.manifest.provides.iter().any(|r| r == ROLE))
            .collect();
        let chosen = match bindings.roles.get(ROLE) {
            Some(bound) => providers.iter().find(|p| p.id() == bound),
            None if providers.len() == 1 => providers.first(),
            None if providers.is_empty() => {
                return Err(ApiError::NotFound("no node is installed".to_owned()))
            }
            None => return Err(failed("several nodes are installed. Choose one")),
        };
        chosen
            .map(|p| (*p).clone())
            .ok_or_else(|| failed("the chosen node is not installed"))
    }

    /// Its public `rpc-port` setting, else the port its health check uses, else the
    /// loopback-only port it declares.
    fn rpc_port(&self, package: &Package) -> Result<u16, ApiError> {
        if package.manifest.settings.is_some() {
            let (packages, _) = self.packages()?;
            let value = self.with_settings(&packages, |ctx| {
                self.settings.public_value(ctx, package, "rpc-port")
            });
            if let Ok(Some(port)) = value {
                if let Ok(port) = port.parse::<u16>() {
                    return Ok(port);
                }
            }
        }
        if let Some(HealthCheck::Tcp { port }) = &package.manifest.health {
            return Ok(*port);
        }
        package
            .manifest
            .network
            .inbound
            .iter()
            .find(|i| i.protocol == Protocol::Tcp && i.scope == nodo_manifest::Scope::Local)
            .map(|i| i.port)
            .ok_or_else(|| failed(format!("{} declares no RPC port", package.id())))
    }

    pub(crate) fn node_status(&self) -> Result<Response, ApiError> {
        let provider = self.node_provider()?;
        if self.is_locked(&provider)? {
            return Err(ApiError::Locked(format!(
                "{} waits for the device passphrase",
                provider.id()
            )));
        }
        let port = self.rpc_port(&provider)?;
        let body = self
            .sys
            .http_get_local(port, "/get_info", MAX_INFO)
            .map_err(|e| failed(format!("{} RPC: {}", provider.id(), e.0)))?;
        parse_info(provider.id(), &body).map(Response::NodeStatus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem};
    use nodo_api::Request;

    const NODE: &str = "api_version = 1\nid = \"node\"\nversion = \"1\"\nexec = [\"bin/node\"]\nprovides = [\"monero-daemon\"]\n\n[health.tcp]\nport = 18081\n";
    const OTHER: &str = "api_version = 1\nid = \"other\"\nversion = \"1\"\nexec = [\"bin/node\"]\nprovides = [\"monero-daemon\"]\n\n[health.tcp]\nport = 28081\n";
    /// What cuprated 0.1.0 answered in the dev VM, shortened.
    const INFO: &str = r#"{"status":"OK","height":1,"target_height":0,"incoming_connections_count":0,"outgoing_connections_count":0,"synchronized":true,"version":"0.1.0-preview-release","mainnet":true}"#;

    fn status(h: &Helper<FakeSystem>) -> Result<Response, ApiError> {
        h.handle(1000, Request::NodeStatus)
    }

    #[test]
    fn reads_height_and_peers_from_the_node_rpc() {
        let sys = FakeSystem::default();
        sys.add_manifest(NODE);
        sys.put("http 18081/get_info", INFO.as_bytes());
        let h = helper(sys);
        assert_eq!(
            status(&h),
            Ok(Response::NodeStatus(NodeStatus {
                provider: "node".into(),
                height: 1,
                target_height: 0,
                outgoing_peers: 0,
                incoming_peers: 0,
                synchronized: true,
            }))
        );
    }

    #[test]
    fn follows_the_role_binding_when_several_nodes_are_installed() {
        let sys = FakeSystem::default();
        sys.add_manifest(NODE);
        sys.add_manifest(OTHER);
        sys.put(
            "http 28081/get_info",
            br#"{"status":"OK","height":3000000,"target_height":3100000,"outgoing_connections_count":12,"incoming_connections_count":3,"synchronized":false}"#,
        );
        let h = helper(sys);
        assert!(matches!(status(&h), Err(ApiError::Failed(m)) if m.contains("Choose")));
        h.sys
            .put("/s/bindings.toml", b"[roles]\nmonero-daemon = \"other\"\n");
        let Ok(Response::NodeStatus(node)) = status(&h) else {
            panic!()
        };
        assert_eq!(node.provider, "other");
        assert_eq!((node.height, node.target_height), (3_000_000, 3_100_000));
        assert_eq!((node.outgoing_peers, node.incoming_peers), (12, 3));
    }

    #[test]
    fn a_node_that_is_down_or_answers_nonsense_is_an_error() {
        let sys = FakeSystem::default();
        sys.add_manifest(NODE);
        let h = helper(sys);
        assert!(matches!(status(&h), Err(ApiError::Failed(m)) if m.contains("refused")));
        h.sys.put("http 18081/get_info", b"<html>");
        assert!(status(&h).is_err());
        h.sys
            .put("http 18081/get_info", br#"{"status":"BUSY","height":5}"#);
        assert!(status(&h).is_err());
    }

    #[test]
    fn no_node_is_not_found() {
        let h = helper(FakeSystem::default());
        assert!(matches!(status(&h), Err(ApiError::NotFound(_))));
    }
}
