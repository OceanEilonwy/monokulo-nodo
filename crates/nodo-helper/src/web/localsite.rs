//! Keeping local sites on the local address when that address changes, and the local CA trust page.
//!
//! nodo-localnet tells the helper whenever it reports a new state, and the helper also checks once
//! a minute in case that message was lost. Either way [`NodoWeb::refresh_local`] compares the
//! address local sites are served on with the one recorded in `local_applied`, and on a difference
//! applies the Caddy listeners, the firewall, the mDNS answers and the DNS records of local names
//! again. The records need the sealed provider credentials, so while the device is locked they wait
//! for the unlock, and [`NodoWeb::local_site_status`] says so in plain words.

use super::*;

use nodo_api::LocalSiteStatus;
use nodo_web::trust::{self, RootCert};

const TRUST_DIR_OWNER: Owner = (0o750, "root", "_caddy");
const TRUST_FILE_OWNER: Owner = (0o640, "root", "_caddy");

/// The address local sites are served on in this plan, if any are.
pub(super) fn served_address(plan: &WebPlan) -> Option<Ipv4Addr> {
    plan.firewall.local.as_ref().map(|local| local.address)
}

impl<I: WebIo> NodoWeb<I> {
    /// The address the running Caddy config, firewall, mDNS responder and records were made for.
    pub(super) fn applied_address(&self) -> Option<Ipv4Addr> {
        let bytes = self.io.read(&self.paths.local_applied)?;
        std::str::from_utf8(&bytes).ok()?.trim().parse().ok()
    }

    pub(super) fn record_applied(&self, plan: &WebPlan) -> Result<(), String> {
        let text = served_address(plan).map(|a| format!("{a}\n")).unwrap_or_default();
        if self.io.read(&self.paths.local_applied).as_deref() == Some(text.as_bytes()) {
            return Ok(());
        }
        self.io.write(&self.paths.local_applied, text.as_bytes(), PUBLIC_FILE)
    }

    /// The local CA root Caddy uses: from its admin API, or from its storage when it is not running.
    pub(super) fn local_ca_root(&self) -> Option<RootCert> {
        let from_api = self
            .io
            .caddy_get(&self.paths.caddy_admin, "/pki/ca/local")
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|v| v["root_certificate"].as_str().and_then(RootCert::from_pem));
        from_api.or_else(|| {
            let path = self.paths.caddy_storage.join("pki/authorities/local/root.crt");
            RootCert::from_pem(&String::from_utf8(self.io.read(&path)?).ok()?)
        })
    }

    /// Whether the trust files differ from what the plan and Caddy's root call for.
    fn trust_stale(&self, plan: &WebPlan) -> bool {
        let wanted = plan.local.trust_url.as_ref().map(|_| trust::files(self.local_ca_root().as_ref()));
        match wanted {
            Some(files) => files.iter().any(|(name, bytes)| self.io.read(&self.paths.trust_dir.join(name)).as_ref() != Some(bytes)),
            None => self.io.read(&self.paths.trust_dir.join(trust::INDEX_FILE)).is_some(),
        }
    }

    /// Writes the trust page and the root for Caddy to serve, or removes them when no `.local`
    /// site uses the local CA. Caddy creates its CA when it first loads a config that needs one,
    /// so a just started Caddy gets a moment to do that.
    pub(super) fn apply_trust(&self, plan: &WebPlan, wait: bool) -> Result<(), String> {
        let names = [trust::INDEX_FILE, trust::PEM_FILE, trust::DER_FILE];
        if plan.local.trust_url.is_none() {
            for name in names {
                self.io.remove(&self.paths.trust_dir.join(name))?;
            }
            return Ok(());
        }
        if wait {
            self.wait_for(|| self.local_ca_root().is_some());
        }
        let root = self.local_ca_root();
        self.io.ensure_dir(&self.paths.trust_dir, TRUST_DIR_OWNER)?;
        let files = trust::files(root.as_ref());
        // The certificate files go first, so the page never offers a file that is not there yet.
        for (name, bytes) in files.iter().rev() {
            let path = self.paths.trust_dir.join(name);
            if self.io.read(&path).as_ref() != Some(bytes) {
                self.io.write(&path, bytes, TRUST_FILE_OWNER)?;
            }
        }
        if root.is_none() {
            for name in [trust::PEM_FILE, trust::DER_FILE] {
                self.io.remove(&self.paths.trust_dir.join(name))?;
            }
        }
        Ok(())
    }

    /// Applies a changed local address. Returns whether anything had to change.
    pub(super) fn refresh_local(&self, input: &WebInput) -> Result<bool, String> {
        let plan = self.plan(input)?;
        let served = served_address(&plan);
        let applied = self.applied_address();
        let moved = served != applied;
        let trust_stale = self.trust_stale(&plan);
        if !moved && !trust_stale {
            return Ok(false);
        }
        if moved {
            eprintln!(
                "nodo-helper: local address {} -> {}, applying local sites again",
                applied.map_or_else(|| "none".to_owned(), |a| a.to_string()),
                served.map_or_else(|| "none".to_owned(), |a| a.to_string())
            );
            self.apply_files(&plan)?;
            self.apply_mdns(&plan, true)?;
            self.apply_local_names(&plan, input.secrets.as_deref());
            self.record_applied(&plan)?;
        }
        self.apply_trust(&plan, false)?;
        Ok(true)
    }

    /// Why the records of local names under the owner's domain do not hold the served address.
    fn records_note(&self, plan: &WebPlan, key: Option<&SecretsKey>) -> Option<String> {
        if plan.local.dns_names.is_empty() {
            return None;
        }
        let served = served_address(plan)?.to_string();
        let record = self.local_records();
        let names = plan.local.dns_names.join(", ");
        let current = record.address.as_deref() == Some(served.as_str())
            && plan.local.dns_names.iter().all(|n| record.names.contains(n));
        match (current, key, record.error) {
            (true, _, None) => None,
            (true, _, Some(error)) => Some(format!("Some DNS records could not be updated: {error}")),
            (false, None, _) => Some(match record.address {
                Some(old) => format!(
                    "The local address is now {served}, but the DNS records for {names} still point at {old}. \
They are updated when the device is unlocked, because the DNS provider credentials stay sealed until then."
                ),
                None => format!(
                    "The DNS records for {names} are written when the device is unlocked, because the DNS provider \
credentials stay sealed until then."
                ),
            }),
            (false, Some(_), Some(error)) => Some(format!("The DNS records for {names} could not be updated: {error}")),
            (false, Some(_), None) => Some(format!("The DNS records for {names} are being updated to {served}.")),
        }
    }

    pub(super) fn local_site_status(&self, input: &WebInput) -> Result<LocalSiteStatus, ApiError> {
        let plan = self.plan(input).map_err(ApiError::Failed)?;
        let fingerprint = plan.local.trust_url.as_ref().and_then(|_| self.local_ca_root()).map(|r| r.fingerprint());
        Ok(LocalSiteStatus {
            address: served_address(&plan).map(|a| a.to_string()),
            trust_url: plan.local.trust_url.clone(),
            ca_fingerprint: fingerprint,
            records_note: self.records_note(&plan, input.secrets.as_deref()),
        })
    }
}
