//! The node's onion service, added to the plan beside the sites' ones, and the onion addresses
//! for the Onion screen.

use nodo_web::caddy::{AddressKind, TorHiddenService, WebPlan};

use super::{hostname_ok, NodoWeb, WebIo, TOR};
use crate::exposure::WebInput;
use crate::onion::{OnionAddresses, NODE_ONION_DIR};

impl<I: WebIo> NodoWeb<I> {
    /// Adds the node's service, which forwards the onion port to the restricted RPC on loopback.
    pub(super) fn add_node_onion(&self, plan: &mut WebPlan, input: &WebInput) {
        let Some(target) = &input.node_onion else { return };
        let service = TorHiddenService {
            package: target.provider.clone(),
            site: NODE_ONION_DIR.to_owned(),
            dir: self.paths.tor_dir.join(NODE_ONION_DIR).to_string_lossy().into_owned(),
            virtual_port: target.port,
            target: format!("127.0.0.1:{}", target.port),
        };
        plan.tor_lines.extend(service.lines());
        plan.tor_services.push(service);
    }

    pub(super) fn node_onion_host(&self) -> Option<String> {
        let path = self.paths.tor_dir.join(NODE_ONION_DIR).join("hostname");
        let host = String::from_utf8(self.io.read(&path)?).ok()?.trim().to_owned();
        hostname_ok(&host).then_some(host)
    }

    /// The torrc without any service, so Tor publishes nothing once every onion address is off.
    pub(super) fn withdraw_tor(&self) -> Result<(), String> {
        let Some(old) = self.io.read(&self.paths.torrc) else { return Ok(()) };
        if !String::from_utf8_lossy(&old).contains("HiddenServiceDir") {
            return Ok(());
        }
        self.io.write(&self.paths.torrc, super::TORRC_HEADER.as_bytes(), super::PUBLIC_FILE)?;
        if self.io.service_installed(TOR) {
            self.io.service(TOR, false, false)?;
        }
        Ok(())
    }

    pub(super) fn onion_addresses(&self, input: &WebInput) -> Result<OnionAddresses, nodo_api::ApiError> {
        let plan = self.plan(input).map_err(nodo_api::ApiError::Failed)?;
        Ok(OnionAddresses {
            tor_installed: self.io.service_installed(TOR),
            tor_running: self.io.service_running(TOR),
            node: input.node_onion.as_ref().and_then(|_| self.node_onion_host()),
            sites: plan
                .addresses
                .iter()
                .filter(|a| a.kind == AddressKind::Onion)
                .map(|a| ((a.package.clone(), a.site.clone()), a.url.clone()))
                .collect(),
        })
    }
}
