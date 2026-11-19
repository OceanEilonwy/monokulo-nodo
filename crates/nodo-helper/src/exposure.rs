use std::collections::BTreeMap;
use std::path::Path;

use nodo_api::{
    AcmeSettingsInfo, AcmeSettingsInput, ApiError, CertificateReport, DnsProviderDetail, DomainProvider, Exposure,
    LocalNameCheck, LocalSiteStatus, ProviderInfo, PublicAlias, PublicConfirmation, ReachabilityCheck,
    ReachabilityDetail, ReachabilityReport, Request, Response, Secret, SiteId, SiteInfo, WebStatus,
};
use nodo_manifest::WebService;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::catalog::Package;
use crate::secrets::SecretsKey;
use std::sync::Arc;
use crate::helper::{lock, Helper};
use crate::system::{System, WriteOptions};

/// Every site's chosen exposure, keyed by package then site.
pub type ExposureState = BTreeMap<String, BTreeMap<String, Exposure>>;

/// The host name the user brought for each site, keyed like [`ExposureState`].
pub type PublicHosts = BTreeMap<String, BTreeMap<String, HostEntry>>;

/// One site's entry in `public-hosts.toml`. A site without other names keeps the plain string the
/// file always held, so the file stays readable by an older helper until an alias is added. With
/// aliases it is a table:
///
/// ```toml
/// [monokulo-pay]
/// web = { host = "www.monokulo.com", aliases = [{ host = "monokulo.com", mode = "redirect" }] }
/// ```
///
/// An older helper cannot read that table and takes the whole file for corrupt, so after a
/// downgrade it refuses to list or change sites until the aliases are removed again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HostEntry {
    Host(String),
    WithAliases {
        host: String,
        #[serde(default)]
        aliases: Vec<PublicAlias>,
    },
}

impl HostEntry {
    pub fn new(host: String, aliases: Vec<PublicAlias>) -> Self {
        if aliases.is_empty() {
            HostEntry::Host(host)
        } else {
            HostEntry::WithAliases { host, aliases }
        }
    }

    pub fn host(&self) -> &str {
        match self {
            HostEntry::Host(host) | HostEntry::WithAliases { host, .. } => host,
        }
    }

    pub fn aliases(&self) -> &[PublicAlias] {
        match self {
            HostEntry::Host(_) => &[],
            HostEntry::WithAliases { aliases, .. } => aliases,
        }
    }
}

/// One site as the web proxy needs it: what its package declares, plus the user's choices.
#[derive(Debug, Clone, PartialEq)]
pub struct WebSite {
    pub package: String,
    pub service: WebService,
    pub exposure: Exposure,
    pub public_host: Option<String>,
    /// The other names the site answers at while it is public.
    pub public_aliases: Vec<PublicAlias>,
    pub locked: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WebInput {
    pub sites: Vec<WebSite>,
    /// Present while the device is unlocked. DNS credentials are sealed under it.
    pub secrets: Option<Arc<SecretsKey>>,
    /// The node's restricted RPC on an onion address, while that is wanted and possible.
    pub node_onion: Option<crate::onion::NodeOnionTarget>,
    /// The owner turned the web GUI on. See `crate::webgui`.
    pub web_gui: bool,
}

fn not_connected() -> ApiError {
    ApiError::NotImplemented("web hosting is not connected".to_owned())
}

/// Puts exposure state into effect and answers the Web screens. `crate::web::NodoWeb` implements
/// it with Caddy, nftables, Tor and dynamic DNS.
pub trait ExposureApplier: Send + Sync {
    fn apply(&self, input: &WebInput) -> Result<(), String>;

    /// Called after an unlock, when sealed credentials can be opened again.
    fn unlocked(&self, _input: &WebInput) -> Result<(), String> {
        Ok(())
    }

    fn status(&self, _input: &WebInput) -> Result<WebStatus, ApiError> {
        Err(not_connected())
    }
    fn providers(&self) -> Result<Vec<DnsProviderDetail>, ApiError> {
        Err(not_connected())
    }
    fn set_provider(
        &self,
        _domain: &str,
        _provider: &str,
        _fields: &[(String, Secret)],
        _input: &WebInput,
    ) -> Result<(), ApiError> {
        Err(not_connected())
    }
    fn test_provider(&self, _input: &WebInput) -> Result<String, ApiError> {
        Err(not_connected())
    }
    fn domain_providers(&self, _input: &WebInput) -> Result<Vec<DomainProvider>, ApiError> {
        Err(not_connected())
    }
    fn add_domain_provider(
        &self,
        _kind: &str,
        _fields: &[(String, Secret)],
        _domain: Option<&str>,
        _input: &WebInput,
    ) -> Result<DomainProvider, ApiError> {
        Err(not_connected())
    }
    fn remove_domain_provider(&self, _id: &str, _input: &WebInput) -> Result<(), ApiError> {
        Err(not_connected())
    }
    fn refresh_domain_provider(&self, _id: &str, _input: &WebInput) -> Result<DomainProvider, ApiError> {
        Err(not_connected())
    }
    fn add_provider_domain(&self, _id: &str, _domain: &str, _input: &WebInput) -> Result<DomainProvider, ApiError> {
        Err(not_connected())
    }
    fn retire_stale(&self) -> Result<Vec<String>, ApiError> {
        Err(not_connected())
    }
    fn reachability(&self, _input: &WebInput, _site: &SiteId) -> Result<ReachabilityDetail, ApiError> {
        Err(not_connected())
    }
    fn acme_settings(&self, _input: &WebInput) -> Result<AcmeSettingsInfo, ApiError> {
        Err(not_connected())
    }
    fn set_acme_settings(&self, _settings: &AcmeSettingsInput, _input: &WebInput) -> Result<(), ApiError> {
        Err(not_connected())
    }
    fn set_dns01(&self, _fields: &[(String, Secret)], _input: &WebInput) -> Result<(), ApiError> {
        Err(not_connected())
    }
    fn certificates(&self, _input: &WebInput) -> Result<CertificateReport, ApiError> {
        Err(not_connected())
    }
    fn check_local_names(&self, _input: &WebInput) -> Result<Vec<LocalNameCheck>, ApiError> {
        Err(not_connected())
    }
    /// Applies the local address again if it changed. Returns whether anything changed.
    fn local_address_changed(&self, _input: &WebInput) -> Result<bool, String> {
        Ok(false)
    }
    fn local_site_status(&self, _input: &WebInput) -> Result<LocalSiteStatus, ApiError> {
        Err(not_connected())
    }
    /// The device's main LAN address, the one the owner reaches it on, when it can be told.
    fn device_address(&self) -> Option<String> {
        None
    }
    /// The domains of the connected providers. A site's other names must be under one, since only
    /// those can get a DNS record and a DNS-01 certificate. Without a proxy there are none.
    fn owned_domains(&self) -> Vec<String> {
        Vec::new()
    }
    /// The onion addresses Tor has made. Without a proxy there are none.
    fn onion_addresses(&self, _input: &WebInput) -> Result<crate::onion::OnionAddresses, ApiError> {
        Ok(crate::onion::OnionAddresses::default())
    }
    /// Where the web GUI is being served, while it is on and the local address exists. Without a
    /// proxy it is served nowhere.
    fn web_gui_url(&self, _input: &WebInput) -> Option<String> {
        None
    }
    /// Starts or stops the web GUI's service to match the saved choice, and nothing else. For the
    /// helper's start; a change of the choice goes through [`Self::apply`].
    fn sync_web_gui_service(&self, _input: &WebInput) -> Result<(), String> {
        Ok(())
    }
}

/// Persists state and changes nothing else. Used by tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCaddyYet;

impl ExposureApplier for NoCaddyYet {
    fn apply(&self, _input: &WebInput) -> Result<(), String> {
        Ok(())
    }
}

const STATE_FILE: WriteOptions = WriteOptions {
    mode: 0o644,
    group: None,
};

/// The rule lives in nodo-api, so the TUI's test backend checks names exactly as the helper does.
pub(crate) use nodo_api::valid_public_host as valid_host;

fn find_site<'a>(package: &'a Package, site: &str) -> Result<&'a WebService, ApiError> {
    package
        .manifest
        .web
        .iter()
        .find(|web| web.name == site)
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "{} has no site {}",
                package.id(),
                site.escape_debug()
            ))
        })
}

impl<S: System> Helper<S> {
    pub(crate) fn load_toml<T: DeserializeOwned + Default>(
        &self,
        path: &Path,
    ) -> Result<(T, Option<Vec<u8>>), ApiError> {
        let previous = self.sys.read_file(path)?;
        let value = match &previous {
            Some(bytes) => std::str::from_utf8(bytes)
                .ok()
                .and_then(|text| toml::from_str(text).ok())
                .ok_or_else(|| ApiError::Failed(format!("{} is corrupt", path.display())))?,
            None => T::default(),
        };
        Ok((value, previous))
    }

    fn save_toml<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), ApiError> {
        let text = toml::to_string(value).map_err(|e| ApiError::Failed(e.to_string()))?;
        if let Some(dir) = path.parent() {
            self.sys.ensure_dir(dir, 0o755, None)?;
        }
        self.sys.atomic_write(path, text.as_bytes(), STATE_FILE)?;
        Ok(())
    }

    fn saved_entry(&self, site: &SiteId) -> Result<Option<HostEntry>, ApiError> {
        let (hosts, _): (PublicHosts, _) = self.load_toml(&self.paths.public_hosts)?;
        Ok(hosts.get(&site.package).and_then(|sites| sites.get(&site.site)).cloned())
    }

    /// Every site of every package, with its saved exposure and host.
    pub(crate) fn web_input(&self, state: &ExposureState) -> Result<WebInput, ApiError> {
        let (packages, _) = self.packages()?;
        let (hosts, _): (PublicHosts, _) = self.load_toml(&self.paths.public_hosts)?;
        let mut sites = Vec::new();
        for package in &packages {
            let locked = self.is_locked(package).unwrap_or(false);
            for web in &package.manifest.web {
                let get = |id: &str| (package.id().to_owned(), id.to_owned());
                let (pkg, name) = get(&web.name);
                let entry = hosts.get(&pkg).and_then(|s| s.get(&name));
                sites.push(WebSite {
                    exposure: state.get(&pkg).and_then(|s| s.get(&name)).copied().unwrap_or_default(),
                    public_host: entry.map(|e| e.host().to_owned()),
                    public_aliases: entry.map(|e| e.aliases().to_vec()).unwrap_or_default(),
                    package: pkg,
                    service: web.clone(),
                    locked,
                });
            }
        }
        Ok(WebInput {
            sites,
            secrets: self.secrets_key(),
            node_onion: self.node_onion_target(),
            // A choice that cannot be read counts as off.
            web_gui: self.web_gui_wanted().unwrap_or(false),
        })
    }

    /// After an unlock, gives the web side its sealed credentials again. The caller holds the
    /// service lock.
    pub(crate) fn web_unlocked(&self) -> Result<(), String> {
        let input = self.current_web_input().map_err(|e| e.to_string())?;
        self.exposure.unlocked(&input)
    }

    /// Applies a changed local address to the local sites. nodo-localnet asks whenever its state
    /// changes, and the helper itself checks once a minute in case a message was lost.
    pub fn check_local_address(&self) -> Result<bool, ApiError> {
        let _guard = lock(&self.service_lock);
        let input = self.current_web_input()?;
        self.exposure.local_address_changed(&input).map_err(ApiError::Failed)
    }

    /// Puts the saved exposure and public hosts into effect, as a restore leaves them. The
    /// saved state stays as it is either way: it is what the owner chose on the backed-up device.
    pub(crate) fn apply_saved_exposure(&self) -> Result<(), String> {
        let _guard = lock(&self.service_lock);
        let input = self.current_web_input().map_err(|e| e.to_string())?;
        self.exposure.apply(&input)
    }

    pub(crate) fn current_web_input(&self) -> Result<WebInput, ApiError> {
        let (state, _): (ExposureState, _) = self.load_toml(&self.paths.exposure_state)?;
        self.web_input(&state)
    }

    /// The web requests other than exposure changes, all answered by the applier.
    pub(crate) fn web_request(&self, request: Request) -> Result<Response, ApiError> {
        match request {
            Request::WebStatus => Ok(Response::WebStatus(self.exposure.status(&self.current_web_input()?)?)),
            Request::ListDnsProviders => Ok(Response::DnsProviders(self.exposure.providers()?)),
            Request::ListProviders => Ok(Response::Providers(
                self.exposure
                    .providers()?
                    .into_iter()
                    .map(|p| ProviderInfo { name: p.name, dynamic_dns: p.dynamic_dns, configured: p.configured })
                    .collect(),
            )),
            Request::SetDnsProvider { domain, provider, fields } => {
                let _guard = lock(&self.service_lock);
                let input = self.current_web_input()?;
                self.exposure.set_provider(&domain, &provider, &fields, &input).map(|()| Response::Ok)
            }
            Request::TestDnsProvider => {
                Ok(Response::ProviderTest(self.exposure.test_provider(&self.current_web_input()?)?))
            }
            Request::ListDomainProviders => {
                Ok(Response::DomainProviders(self.exposure.domain_providers(&self.current_web_input()?)?))
            }
            Request::AddDomainProvider { kind, fields, domain } => {
                let _guard = lock(&self.service_lock);
                let input = self.current_web_input()?;
                Ok(Response::DomainProvider(self.exposure.add_domain_provider(&kind, &fields, domain.as_deref(), &input)?))
            }
            Request::RemoveDomainProvider { id } => {
                let _guard = lock(&self.service_lock);
                let input = self.current_web_input()?;
                self.exposure.remove_domain_provider(&id, &input).map(|()| Response::Ok)
            }
            Request::RefreshDomainProvider { id } => {
                let _guard = lock(&self.service_lock);
                let input = self.current_web_input()?;
                Ok(Response::DomainProvider(self.exposure.refresh_domain_provider(&id, &input)?))
            }
            Request::AddProviderDomain { id, domain } => {
                let _guard = lock(&self.service_lock);
                let input = self.current_web_input()?;
                Ok(Response::DomainProvider(self.exposure.add_provider_domain(&id, &domain, &input)?))
            }
            Request::RetireStaleRecords => {
                let _guard = lock(&self.service_lock);
                Ok(Response::RetiredRecords(self.exposure.retire_stale()?))
            }
            Request::CheckReachabilityDetail { site } => {
                let input = self.current_web_input()?;
                let detail = self.exposure.reachability(&input, &site)?;
                self.remember_reachability(&detail);
                Ok(Response::ReachabilityDetail(detail))
            }
            Request::AcmeSettings => {
                Ok(Response::AcmeSettings(self.exposure.acme_settings(&self.current_web_input()?)?))
            }
            Request::SetAcmeSettings(settings) => {
                let _guard = lock(&self.service_lock);
                let input = self.current_web_input()?;
                self.exposure.set_acme_settings(&settings, &input).map(|()| Response::Ok)
            }
            Request::SetDns01Credentials { fields } => {
                let _guard = lock(&self.service_lock);
                let input = self.current_web_input()?;
                self.exposure.set_dns01(&fields, &input).map(|()| Response::Ok)
            }
            Request::CertificateStatus => {
                Ok(Response::Certificates(self.exposure.certificates(&self.current_web_input()?)?))
            }
            Request::CheckLocalNames => {
                Ok(Response::LocalNames(self.exposure.check_local_names(&self.current_web_input()?)?))
            }
            Request::LocalSiteStatus => {
                Ok(Response::LocalSiteStatus(self.exposure.local_site_status(&self.current_web_input()?)?))
            }
            Request::CheckReachability { site } => {
                let input = self.current_web_input()?;
                let detail = self.exposure.reachability(&input, &site)?;
                self.remember_reachability(&detail);
                Ok(Response::Reachability(match (detail.verdict.as_str(), &detail.router_wan) {
                    ("blocked_by_cgnat", Some(wan)) => ReachabilityReport::Cgnat {
                        wan_address: wan.clone(),
                        internet_address: detail.public_ip.clone().unwrap_or_default(),
                    },
                    // This older form has only pass and fail, so anything not passed fails.
                    _ => ReachabilityReport::Checked {
                        checks: detail
                            .checks
                            .iter()
                            .map(|c| ReachabilityCheck {
                                passed: c.status == nodo_api::CheckStatus::Passed,
                                text: c.id.clone(),
                                note: c.detail.clone(),
                            })
                            .collect(),
                    },
                }))
            }
            _ => Err(ApiError::Failed("not a web request".to_owned())),
        }
    }

    pub(crate) fn set_exposure(
        &self,
        site: &SiteId,
        exposure: Exposure,
        confirmation: Option<&PublicConfirmation>,
    ) -> Result<Response, ApiError> {
        let package = self.package(&site.package)?;
        let web = find_site(&package, &site.site)?;
        let enabled = [exposure.local, exposure.onion, exposure.public];
        // An app tied to one base URL breaks when reached through a second address.
        if !web.multi_address && enabled.iter().filter(|on| **on).count() > 1 {
            return Err(ApiError::Failed(format!(
                "{} works from a single address, so only one exposure can be on",
                site.site
            )));
        }

        let _guard = lock(&self.service_lock);
        let path = &self.paths.exposure_state;
        let (mut state, previous): (ExposureState, _) = self.load_toml(path)?;
        let was_public = state
            .get(&site.package)
            .and_then(|sites| sites.get(&site.site))
            .is_some_and(|old| old.public);
        if exposure.public && !was_public {
            let Some(entry) = self.saved_entry(site)? else {
                return Err(ApiError::NeedsConfirmation(
                    "choose the public host name before going public".to_owned(),
                ));
            };
            let host = entry.host();
            if confirmation.map(|c| c.typed_host.as_str()) != Some(host) {
                return Err(ApiError::NeedsConfirmation(format!(
                    "type {host} exactly to confirm going public"
                )));
            }
            // A name saved while its provider was connected may have lost it since, and without
            // the provider it would get neither its record nor its certificate.
            let owned = self.exposure.owned_domains();
            if let Some(stale) = entry
                .aliases()
                .iter()
                .find(|alias| !owned.iter().any(|domain| nodo_api::host_under(&alias.host, domain)))
            {
                return Err(ApiError::Failed(format!(
                    "{} is not under a domain of a connected provider any more. Remove it, or connect its provider, before going public",
                    stale.host
                )));
            }
        }
        state
            .entry(site.package.clone())
            .or_default()
            .insert(site.site.clone(), exposure);
        self.save_toml(path, &state)?;

        let applied = self
            .web_input(&state)
            .map_err(|e| e.to_string())
            .and_then(|input| self.exposure.apply(&input));
        if let Err(reason) = applied {
            // Saved state must not claim an exposure that never took effect.
            let restored = match &previous {
                Some(bytes) => self.sys.atomic_write(path, bytes, STATE_FILE),
                None => self.sys.remove_file(path),
            };
            return Err(ApiError::Failed(match restored {
                Ok(()) => format!("could not apply exposure: {reason}"),
                Err(e) => format!(
                    "could not apply exposure: {reason}. The old state could not be restored: {e}"
                ),
            }));
        }
        Ok(Response::Ok)
    }

    /// A public site keeps its host, because the user confirmed going public for that name. Its
    /// other names can change while it is public, and the change is applied at once. Removing a
    /// name, or changing whether one redirects, publishes nothing new. A name the site does not
    /// answer at yet does: it gets a record pointing at the home and a certificate that public
    /// logs keep, and the going-public confirmation never listed it. So each such name must be in
    /// `confirm_added`, the names the user confirmed adding. `aliases` of `None` keeps the saved
    /// ones.
    pub(crate) fn set_public_host(
        &self,
        site: &SiteId,
        host: &str,
        aliases: Option<&[PublicAlias]>,
        confirm_added: &[String],
    ) -> Result<Response, ApiError> {
        let package = self.package(&site.package)?;
        find_site(&package, &site.site)?;
        if !valid_host(host) {
            return Err(ApiError::Failed(
                "the host must be a full lowercase domain name, such as shop.example.com"
                    .to_owned(),
            ));
        }
        let _guard = lock(&self.service_lock);
        let (state, _): (ExposureState, _) = self.load_toml(&self.paths.exposure_state)?;
        let public = state
            .get(&site.package)
            .and_then(|sites| sites.get(&site.site))
            .is_some_and(|e| e.public);
        let (mut hosts, previous): (PublicHosts, _) = self.load_toml(&self.paths.public_hosts)?;
        let current = hosts.get(&site.package).and_then(|s| s.get(&site.site));
        if public && current.map(HostEntry::host) != Some(host) {
            return Err(ApiError::Failed(
                "take the site off its public address before changing the host".to_owned(),
            ));
        }
        let saved = current.map(|e| e.aliases().to_vec()).unwrap_or_default();
        let aliases = aliases.map_or_else(|| saved.clone(), <[PublicAlias]>::to_vec);
        if public {
            let unconfirmed: Vec<&str> = aliases
                .iter()
                .map(|alias| alias.host.as_str())
                .filter(|name| !saved.iter().any(|old| old.host == *name))
                .filter(|name| !confirm_added.iter().any(|confirmed| confirmed == name))
                .collect();
            if !unconfirmed.is_empty() {
                return Err(ApiError::NeedsConfirmation(format!(
                    "the site is public, so {} would go on the internet at once: confirm adding it",
                    unconfirmed.join(", ")
                )));
            }
        }
        // Every name another site answers at, so two sites never claim one name.
        let taken: Vec<(String, String)> = hosts
            .iter()
            .flat_map(|(package, sites)| sites.iter().map(move |(name, entry)| (package, name, entry)))
            .filter(|(package, name, _)| **package != site.package || **name != site.site)
            .flat_map(|(package, name, entry)| {
                let owner = format!("{package}/{name}");
                std::iter::once(entry.host().to_owned())
                    .chain(entry.aliases().iter().map(|a| a.host.clone()))
                    .map(move |h| (h, owner.clone()))
            })
            .collect();
        nodo_api::check_aliases(host, &aliases, &self.exposure.owned_domains(), &taken)
            .map_err(ApiError::Failed)?;
        let changed = aliases != saved;
        hosts
            .entry(site.package.clone())
            .or_default()
            .insert(site.site.clone(), HostEntry::new(host.to_owned(), aliases));
        self.save_toml(&self.paths.public_hosts, &hosts)?;
        if public && changed {
            // A removed name must stop answering now, and an added one start, not at the next
            // change.
            let applied = self
                .web_input(&state)
                .map_err(|e| e.to_string())
                .and_then(|input| self.exposure.apply(&input));
            if let Err(reason) = applied {
                let restored = match &previous {
                    Some(bytes) => self.sys.atomic_write(&self.paths.public_hosts, bytes, STATE_FILE),
                    None => self.sys.remove_file(&self.paths.public_hosts),
                };
                return Err(ApiError::Failed(match restored {
                    Ok(()) => format!("could not apply the change: {reason}"),
                    Err(e) => format!("could not apply the change: {reason}. The old names could not be restored: {e}"),
                }));
            }
        }
        Ok(Response::Ok)
    }

    pub(crate) fn list_sites(&self) -> Result<Response, ApiError> {
        let (packages, _) = self.packages()?;
        let (state, _): (ExposureState, _) = self.load_toml(&self.paths.exposure_state)?;
        let (hosts, _): (PublicHosts, _) = self.load_toml(&self.paths.public_hosts)?;
        let mut sites = Vec::new();
        for package in &packages {
            let locked = self.is_locked(package).unwrap_or(false);
            for web in &package.manifest.web {
                let id = SiteId {
                    package: package.id().to_owned(),
                    site: web.name.clone(),
                };
                sites.push(SiteInfo {
                    exposure: state
                        .get(&id.package)
                        .and_then(|s| s.get(&id.site))
                        .copied()
                        .unwrap_or_default(),
                    public_host: hosts
                        .get(&id.package)
                        .and_then(|s| s.get(&id.site))
                        .map(|e| e.host().to_owned()),
                    public_aliases: hosts
                        .get(&id.package)
                        .and_then(|s| s.get(&id.site))
                        .map(|e| e.aliases().to_vec())
                        .unwrap_or_default(),
                    multi_address: web.multi_address,
                    locked,
                    id,
                });
            }
        }
        Ok(Response::Sites(sites))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{helper, FakeSystem, TestPolicy, SHOP};
    use nodo_api::AliasMode;

    const STATE: &str = "/s/exposure.toml";
    const HOST: &str = "shop.example.com";
    const BOTH: Exposure = Exposure {
        local: true,
        onion: false,
        public: true,
    };
    const LOCAL: Exposure = Exposure {
        local: true,
        onion: false,
        public: false,
    };
    const PUBLIC: Exposure = Exposure {
        local: false,
        onion: false,
        public: true,
    };

    fn site(name: &str) -> SiteId {
        SiteId {
            package: "shop".into(),
            site: name.into(),
        }
    }

    fn typed(host: &str) -> PublicConfirmation {
        PublicConfirmation {
            typed_host: host.into(),
        }
    }

    fn setup() -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let h = helper(sys);
        h.set_public_host(&site("web"), HOST, None, &[]).unwrap();
        h.set_public_host(&site("single"), "one.example.com", None, &[])
            .unwrap();
        h
    }

    fn saved(h: &Helper<FakeSystem>) -> ExposureState {
        toml::from_str(std::str::from_utf8(&h.sys.get(STATE).unwrap()).unwrap()).unwrap()
    }

    fn go(h: &Helper<FakeSystem>, name: &str, exposure: Exposure) -> Result<Response, ApiError> {
        let host = if name == "web" {
            HOST
        } else {
            "one.example.com"
        };
        h.set_exposure(&site(name), exposure, Some(&typed(host)))
    }

    #[test]
    fn persists_and_survives_later_changes() {
        let h = setup();
        go(&h, "web", BOTH).unwrap();
        assert_eq!(saved(&h)["shop"]["web"], BOTH);
        go(&h, "single", LOCAL).unwrap();
        let state = saved(&h);
        assert_eq!(state["shop"]["web"], BOTH);
        assert_eq!(state["shop"]["single"], LOCAL);
    }

    #[test]
    fn unknown_package_or_site_is_not_found() {
        let h = setup();
        for (package, name) in [("ghost", "web"), ("shop", "ghost"), ("shop", "../../x")] {
            let id = SiteId {
                package: package.into(),
                site: name.into(),
            };
            assert!(matches!(
                h.set_exposure(&id, LOCAL, None),
                Err(ApiError::NotFound(_))
            ));
        }
        assert_eq!(h.sys.get(STATE), None);
    }

    #[test]
    fn single_address_site_allows_only_one_exposure_whatever_the_client_sends() {
        let h = setup();
        for exposure in [
            BOTH,
            Exposure {
                local: true,
                onion: true,
                public: false,
            },
        ] {
            assert!(matches!(
                go(&h, "single", exposure),
                Err(ApiError::Failed(_))
            ));
        }
        assert_eq!(h.sys.get(STATE), None);
        go(&h, "single", PUBLIC).unwrap();
    }

    #[test]
    fn going_public_needs_the_typed_host() {
        let h = setup();
        for confirmation in [
            None,
            Some(typed("")),
            Some(typed("SHOP.example.com")),
            Some(typed("evil.example.com")),
        ] {
            assert!(matches!(
                h.set_exposure(&site("web"), BOTH, confirmation.as_ref()),
                Err(ApiError::NeedsConfirmation(_))
            ));
        }
        assert_eq!(h.sys.get(STATE), None);
        assert_eq!(
            h.set_exposure(&site("web"), BOTH, Some(&typed(HOST))),
            Ok(Response::Ok)
        );
    }

    #[test]
    fn local_and_onion_and_going_offline_need_no_confirmation() {
        let h = setup();
        h.set_exposure(&site("web"), LOCAL, None).unwrap();
        go(&h, "web", BOTH).unwrap();
        // Staying public while changing the rest, then turning public off.
        h.set_exposure(&site("web"), PUBLIC, None).unwrap();
        h.set_exposure(&site("web"), Exposure::default(), None)
            .unwrap();
        assert_eq!(saved(&h)["shop"]["web"], Exposure::default());
    }

    #[test]
    fn going_public_without_a_host_is_refused() {
        let sys = FakeSystem::default();
        sys.add_manifest(SHOP);
        let h = helper(sys);
        assert!(matches!(
            h.set_exposure(&site("web"), PUBLIC, Some(&typed(HOST))),
            Err(ApiError::NeedsConfirmation(_))
        ));
    }

    #[test]
    fn host_names_are_validated_and_frozen_while_public() {
        let h = setup();
        for bad in [
            "",
            "localhost",
            "Shop.example.com",
            "a..b",
            "-a.com",
            "a.com.",
            "1.2.3.4",
            "a b.com",
            "a_b.com",
        ] {
            assert!(h.set_public_host(&site("web"), bad, None, &[]).is_err(), "{bad}");
        }
        go(&h, "web", PUBLIC).unwrap();
        assert!(h
            .set_public_host(&site("web"), "other.example.com", None, &[])
            .is_err());
        assert!(h.set_public_host(&site("web"), HOST, None, &[]).is_ok());
    }

    /// A proxy that owns example.com and example.org and counts how often it was applied.
    #[derive(Clone, Default)]
    struct Domains(Arc<std::sync::Mutex<usize>>);
    impl ExposureApplier for Domains {
        fn apply(&self, _: &WebInput) -> Result<(), String> {
            *self.0.lock().unwrap() += 1;
            Ok(())
        }
        fn owned_domains(&self) -> Vec<String> {
            vec!["example.com".to_owned(), "example.org".to_owned()]
        }
    }

    fn with_domains(proxy: &Domains) -> Helper<FakeSystem> {
        let h = setup();
        Helper::new(h.sys, h.allowed, Box::new(TestPolicy), Box::new(proxy.clone())).with_paths(h.paths)
    }

    fn alias(host: &str, mode: AliasMode) -> PublicAlias {
        PublicAlias { host: host.to_owned(), mode }
    }

    fn listed(h: &Helper<FakeSystem>, name: &str) -> SiteInfo {
        let Ok(Response::Sites(sites)) = h.list_sites() else { panic!() };
        sites.into_iter().find(|s| s.id.site == name).unwrap()
    }

    const HOSTS: &str = "/s/public-hosts.toml";

    #[test]
    fn aliases_are_saved_as_a_table_and_a_site_without_them_keeps_the_old_string() {
        let proxy = Domains::default();
        let h = with_domains(&proxy);
        let aliases = [alias("example.com", AliasMode::Redirect), alias("pay.example.org", AliasMode::Serve)];
        h.set_public_host(&site("web"), "www.example.com", Some(&aliases), &[]).unwrap();
        let text = String::from_utf8(h.sys.get(HOSTS).unwrap()).unwrap();
        // The other site keeps the plain string an older helper reads.
        assert!(text.contains("single = \"one.example.com\""), "{text}");
        let saved: PublicHosts = toml::from_str(&text).unwrap();
        assert_eq!(saved["shop"]["web"], HostEntry::WithAliases { host: "www.example.com".into(), aliases: aliases.to_vec() });
        assert_eq!(listed(&h, "web").public_aliases, aliases);
        assert_eq!(listed(&h, "single").public_aliases, []);
        let input = h.current_web_input().unwrap();
        let web = input.sites.iter().find(|s| s.service.name == "web").unwrap();
        assert_eq!(web.public_aliases, aliases, "the proxy gets them");
        // No aliases given keeps them; an empty list removes them and the old form comes back.
        h.set_public_host(&site("web"), "www.example.com", None, &[]).unwrap();
        assert_eq!(listed(&h, "web").public_aliases, aliases);
        h.set_public_host(&site("web"), "www.example.com", Some(&[]), &[]).unwrap();
        let text = String::from_utf8(h.sys.get(HOSTS).unwrap()).unwrap();
        assert!(text.contains("web = \"www.example.com\""), "{text}");
        assert_eq!(*proxy.0.lock().unwrap(), 0, "nothing public, so nothing to apply");
    }

    #[test]
    fn an_old_file_reads_unchanged() {
        let h = setup();
        h.sys.put(HOSTS, b"[shop]\nweb = \"shop.example.com\"\n");
        let web = listed(&h, "web");
        assert_eq!(web.public_host.as_deref(), Some("shop.example.com"));
        assert!(web.public_aliases.is_empty());
    }

    #[test]
    fn aliases_outside_the_connected_domains_duplicates_and_other_sites_names_are_refused() {
        let proxy = Domains::default();
        let h = with_domains(&proxy);
        let refused = |aliases: &[PublicAlias]| match h.set_public_host(&site("web"), "www.example.com", Some(aliases), &[]) {
            Err(ApiError::Failed(reason)) => reason,
            other => panic!("{other:?}"),
        };
        assert!(refused(&[alias("example.net", AliasMode::Redirect)]).contains("not under a domain of a connected provider"));
        assert!(refused(&[alias("a.example.com", AliasMode::Serve), alias("a.example.com", AliasMode::Redirect)]).contains("listed twice"));
        assert!(refused(&[alias("www.example.com", AliasMode::Serve)]).contains("main address"));
        assert_eq!(refused(&[alias("one.example.com", AliasMode::Redirect)]), "one.example.com is already an address of shop/single");
        assert!(refused(&[alias("Example.com", AliasMode::Redirect)]).contains("lowercase"));
        assert_eq!(listed(&h, "web").public_host.as_deref(), Some(HOST), "a refusal changes nothing");
        // Without a proxy no domain is connected, so no alias can be checked into place.
        let bare = setup();
        assert!(bare.set_public_host(&site("web"), HOST, Some(&[alias("example.com", AliasMode::Redirect)]), &[]).is_err());
        // And another site cannot take a name an alias already has.
        h.set_public_host(&site("web"), "www.example.com", Some(&[alias("example.com", AliasMode::Redirect)]), &[]).unwrap();
        assert!(h.set_public_host(&site("single"), "example.com", None, &[]).is_err());
    }

    #[test]
    fn while_public_aliases_can_be_removed_and_applied_at_once() {
        let proxy = Domains::default();
        let h = with_domains(&proxy);
        let apex = alias("example.com", AliasMode::Redirect);
        let pay = alias("pay.example.org", AliasMode::Serve);
        h.set_public_host(&site("web"), HOST, Some(&[apex.clone(), pay.clone()]), &[]).unwrap();
        go(&h, "web", PUBLIC).unwrap();
        let applied = *proxy.0.lock().unwrap();
        h.set_public_host(&site("web"), HOST, Some(std::slice::from_ref(&pay)), &[]).unwrap();
        assert_eq!(listed(&h, "web").public_aliases, std::slice::from_ref(&pay));
        assert_eq!(*proxy.0.lock().unwrap(), applied + 1, "a removed name stops answering at once");
        // Saving the same names again applies nothing.
        h.set_public_host(&site("web"), HOST, Some(std::slice::from_ref(&pay)), &[]).unwrap();
        assert_eq!(*proxy.0.lock().unwrap(), applied + 1);
    }

    #[test]
    fn while_public_a_new_name_goes_on_only_when_it_is_confirmed() {
        let proxy = Domains::default();
        let h = with_domains(&proxy);
        let apex = alias("example.com", AliasMode::Redirect);
        let b = alias("b.example.com", AliasMode::Serve);
        h.set_public_host(&site("web"), HOST, Some(std::slice::from_ref(&apex)), &[]).unwrap();
        go(&h, "web", PUBLIC).unwrap();
        let applied = *proxy.0.lock().unwrap();
        let both = [apex.clone(), b.clone()];
        let confirm = |name: &str| vec![name.to_owned()];

        let unconfirmed = h.set_public_host(&site("web"), HOST, Some(&both), &[]);
        assert!(
            matches!(&unconfirmed, Err(ApiError::NeedsConfirmation(reason)) if reason.contains("so b.example.com would go")),
            "only the new name needs confirming: {unconfirmed:?}"
        );
        // Confirming some other name is not confirming this one.
        let wrong = h.set_public_host(&site("web"), HOST, Some(&both), &confirm("example.com"));
        assert!(matches!(wrong, Err(ApiError::NeedsConfirmation(_))), "{wrong:?}");
        assert_eq!(listed(&h, "web").public_aliases, std::slice::from_ref(&apex), "a refusal leaves the saved names");
        assert_eq!(*proxy.0.lock().unwrap(), applied, "and applies nothing");

        h.set_public_host(&site("web"), HOST, Some(&both), &confirm("b.example.com")).unwrap();
        assert_eq!(listed(&h, "web").public_aliases, both);
        assert_eq!(*proxy.0.lock().unwrap(), applied + 1, "the confirmed name answers at once");

        // Whether a name redirects can change without a confirmation: it is public already.
        let served = [alias("example.com", AliasMode::Serve), b.clone()];
        h.set_public_host(&site("web"), HOST, Some(&served), &[]).unwrap();
        assert_eq!(listed(&h, "web").public_aliases, served);
        assert_eq!(*proxy.0.lock().unwrap(), applied + 2);

        // A confirmed name is still checked like any other.
        let stray = [served[0].clone(), served[1].clone(), alias("elsewhere.invalid", AliasMode::Serve)];
        let refused = h.set_public_host(&site("web"), HOST, Some(&stray), &confirm("elsewhere.invalid"));
        assert!(
            matches!(&refused, Err(ApiError::Failed(reason)) if reason.contains("not under a domain of a connected provider")),
            "{refused:?}"
        );
        assert_eq!(listed(&h, "web").public_aliases, served);
    }

    #[test]
    fn going_public_with_a_name_whose_provider_has_gone_is_refused() {
        let proxy = Domains::default();
        let h = with_domains(&proxy);
        // Saved while example.net was connected, by hand here.
        h.sys.put(HOSTS, b"[shop.web]\nhost = \"shop.example.com\"\naliases = [{ host = \"example.net\", mode = \"redirect\" }]\n");
        let refused = go(&h, "web", PUBLIC);
        assert!(matches!(&refused, Err(ApiError::Failed(reason)) if reason.contains("example.net is not under a domain")), "{refused:?}");
        assert_eq!(h.sys.get(STATE), None);
        h.set_public_host(&site("web"), HOST, Some(&[alias("example.com", AliasMode::Redirect)]), &[]).unwrap();
        go(&h, "web", PUBLIC).unwrap();
    }

    #[test]
    fn a_removal_the_proxy_refuses_puts_the_names_back() {
        let h = setup();
        let h = with_failing_caddy(h);
        // Saved by hand: going public through the failing proxy is not possible.
        h.sys.put(HOSTS, b"[shop.web]\nhost = \"shop.example.com\"\naliases = [{ host = \"example.com\", mode = \"redirect\" }]\n");
        h.sys.put(STATE, b"[shop.web]\nlocal = false\nonion = false\npublic = true\n");
        let before = h.sys.get(HOSTS).unwrap();
        assert!(h.set_public_host(&site("web"), HOST, Some(&[]), &[]).is_err());
        assert_eq!(h.sys.get(HOSTS).unwrap(), before);
    }

    #[test]
    fn sites_list_every_web_entry_with_its_state() {
        let h = setup();
        go(&h, "web", BOTH).unwrap();
        let Ok(Response::Sites(sites)) = h.list_sites() else {
            panic!()
        };
        assert_eq!(sites.len(), 2);
        let web = sites.iter().find(|s| s.id.site == "web").unwrap();
        assert_eq!(web.exposure, BOTH);
        assert_eq!(web.public_host.as_deref(), Some(HOST));
        assert!(web.multi_address);
        let single = sites.iter().find(|s| s.id.site == "single").unwrap();
        assert_eq!(single.exposure, Exposure::default());
    }

    #[test]
    fn write_failure_is_reported() {
        let h = setup();
        h.sys.fail("write /s/exposure");
        assert!(matches!(go(&h, "web", BOTH), Err(ApiError::Failed(_))));
    }

    #[test]
    fn corrupt_state_is_not_overwritten() {
        let h = setup();
        h.sys.put(STATE, b"[[[");
        assert!(matches!(go(&h, "web", BOTH), Err(ApiError::Failed(_))));
        assert_eq!(h.sys.get(STATE).unwrap(), b"[[[");
    }

    struct FailingCaddy;
    impl ExposureApplier for FailingCaddy {
        fn apply(&self, _: &WebInput) -> Result<(), String> {
            Err("caddy refused".to_owned())
        }
    }

    fn with_failing_caddy(h: Helper<FakeSystem>) -> Helper<FakeSystem> {
        Helper::new(
            h.sys,
            h.allowed,
            Box::new(TestPolicy),
            Box::new(FailingCaddy),
        )
        .with_paths(h.paths)
    }

    #[test]
    fn failed_apply_restores_the_previous_state() {
        let h = setup();
        h.sys.put(
            STATE,
            b"[shop.web]\nlocal = true\nonion = false\npublic = false\n",
        );
        let h = with_failing_caddy(h);
        assert!(matches!(go(&h, "web", BOTH), Err(ApiError::Failed(_))));
        assert_eq!(saved(&h)["shop"]["web"], LOCAL);
    }

    #[test]
    fn failed_first_apply_leaves_no_state() {
        let h = with_failing_caddy(setup());
        assert!(go(&h, "web", BOTH).is_err());
        assert_eq!(h.sys.get(STATE), None);
    }
}
