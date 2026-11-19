//! Connected DNS providers. The owner can connect several accounts; each holds the domains its key
//! controls, discovered through the provider's API where it can list them and added by name (proved
//! with a temporary record) where it cannot. A site's public address is under one of those domains,
//! and that domain's provider publishes its record and answers its certificate challenge.
//!
//! The store is the root-only file that used to hold one domain and one provider. Its credentials
//! are sealed per field exactly as before, so the old file converts without opening anything: a
//! device updated with a provider already connected keeps it, as the first provider, with its domain.
//!
//! A domain belongs to the first provider that holds it. Another provider that lists the same domain
//! (a registrar and a DNS host often both do) shows it as held elsewhere, so records and
//! certificates for one domain always go through one account.

use std::collections::BTreeMap;
use std::path::PathBuf;

use nodo_api::{ApiError, BlockedDomain, Dns01Support, DomainProvider, ProviderDomain, ProviderKey, Secret, SiteId};
use nodo_web::caddy::{DnsChallenge, ZoneChallenge};
use nodo_web::dns::catalog::{self, Dns01, ProviderStatus};
use nodo_web::dns::config::ProviderConfig;
use nodo_web::dns::{DnsProvider, DomainListing, Zone};
use serde::{Deserialize, Serialize};

use super::{field_names, secrets, Credentials, NodoWeb, WebIo, ROOT_ONLY, SEAL_PACKAGE};
use crate::exposure::{ExposureApplier, WebInput};
use crate::secrets::SecretsKey;

const STORE_VERSION: u32 = 2;

/// One connected provider as stored. `dns` is nodo-ddns's provider table: `provider` in the clear,
/// every other value sealed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct StoredProvider {
    pub id: String,
    pub dns: BTreeMap<String, String>,
    /// DNS-01 credentials for a provider whose certificate module needs its own, sealed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dns01: BTreeMap<String, String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// The first credential, masked (see [`key_hint`]), in the clear so the owner can be shown which
    /// key this is while the device is locked. Never enough of the credential to use it, and never
    /// the start of a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_hint: Option<String>,
    /// The account's domains the key could not change at the last listing, with the reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<StoredBlocked>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct StoredBlocked {
    pub name: String,
    pub reason: String,
}

/// Whether a provider's first credential only names the key (Porkbun's `pk1_…` API key, an Alibaba
/// AccessKey ID, a Name.com username) and is useless without the second one. A provider with a
/// single token has no such half: its first credential is the whole secret.
fn first_field_is_an_identifier(kind: &str) -> bool {
    matches!(kind, "porkbun" | "namecom" | "alibaba")
}

/// Enough of a credential to recognise it beside the keys in the provider's account, and no more.
/// An identifier shows its first eight and last four characters. A secret shows its last four
/// only, as account pages do, so no prefix of a token is ever kept in the clear.
pub(super) fn key_hint(value: &str, identifier: bool) -> String {
    let chars: Vec<char> = value.trim().chars().collect();
    let part = |range: std::ops::Range<usize>| chars[range].iter().collect::<String>();
    match chars.len() {
        n if identifier && n >= 24 => format!("{}…{}", part(0..8), part(n - 4..n)),
        n if n >= 16 => format!("…{}", part(n - 4..n)),
        _ => "…".to_owned(),
    }
}

impl StoredProvider {
    pub fn kind(&self) -> &str {
        self.dns.get("provider").map_or("", String::as_str)
    }

    fn credentials(&self, domain: &str) -> Credentials {
        Credentials { domain: domain.to_owned(), dns: self.dns.clone(), dns01: self.dns01.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct ProviderStore {
    pub version: u32,
    #[serde(default, rename = "provider")]
    pub providers: Vec<StoredProvider>,
}

impl Default for ProviderStore {
    fn default() -> Self {
        Self { version: STORE_VERSION, providers: Vec::new() }
    }
}

/// The store as the file held it before several providers were supported.
#[derive(Deserialize)]
struct LegacyStore {
    domain: String,
    dns: BTreeMap<String, String>,
    #[serde(default)]
    dns01: BTreeMap<String, String>,
}

fn under(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.'))
}

/// Every public name a site answers at: its host, then its other names. A provider whose domain
/// holds any of them publishes that record and answers that certificate challenge.
fn site_hosts(site: &crate::exposure::WebSite) -> impl Iterator<Item = &str> {
    site.public_host.as_deref().into_iter().chain(site.public_aliases.iter().map(|a| a.host.as_str()))
}

impl ProviderStore {
    /// The provider records and certificates for `domain` go through: the first one that holds it.
    pub fn owner(&self, domain: &str) -> Option<&StoredProvider> {
        self.providers.iter().find(|p| p.domains.iter().any(|d| d == domain))
    }

    pub fn owns(&self, provider: &StoredProvider, domain: &str) -> bool {
        self.owner(domain).is_some_and(|owner| owner.id == provider.id)
    }

    /// The domain a host is under, the longest when domains nest, with the provider that owns it.
    pub fn zone_of(&self, host: &str) -> Option<(&StoredProvider, &str)> {
        let host = host.to_ascii_lowercase();
        self.providers
            .iter()
            .flat_map(|p| p.domains.iter().map(move |d| (p, d.as_str())))
            .filter(|(p, d)| under(&host, d) && self.owns(p, d))
            .max_by_key(|(_, d)| d.len())
    }

    /// Every owned domain with its provider, in the store's order.
    pub fn owned(&self) -> Vec<(&StoredProvider, &str)> {
        self.providers
            .iter()
            .flat_map(|p| p.domains.iter().map(move |d| (p, d.as_str())))
            .filter(|(p, d)| self.owns(p, d))
            .collect()
    }

    pub fn next_id(&self, kind: &str) -> String {
        (1..)
            .map(|n| if n == 1 { kind.to_owned() } else { format!("{kind}-{n}") })
            .find(|id| !self.providers.iter().any(|p| &p.id == id))
            .unwrap_or_else(|| kind.to_owned())
    }

    fn get(&self, id: &str) -> Result<usize, ApiError> {
        self.providers
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| ApiError::NotFound(format!("no connected provider is called {}", id.escape_debug())))
    }
}

fn dns01_support(kind: &str) -> Dns01Support {
    match catalog::lookup(kind).map(|p| p.dns01) {
        Some(Dns01::Bundled) => Dns01Support::Bundled,
        Some(Dns01::BundledSeparateCredentials(note)) => Dns01Support::BundledSeparateCredentials(note.to_owned()),
        _ => Dns01Support::Unavailable,
    }
}

fn display_name(kind: &str) -> String {
    catalog::lookup(kind).map_or_else(|| kind.to_owned(), |p| p.name.to_owned())
}

/// Names from a provider are untrusted: only valid public domains are kept, lowercased and sorted.
fn valid_domains(names: Vec<String>) -> Vec<String> {
    let mut domains: Vec<String> = names
        .into_iter()
        .map(|d| d.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|d| Zone::new(d).is_ok())
        .collect();
    domains.sort();
    domains.dedup();
    domains
}

/// A listing as stored: the usable domains, and the blocked ones that are valid names and not usable.
fn valid_listing(listing: DomainListing) -> (Vec<String>, Vec<StoredBlocked>) {
    let usable = valid_domains(listing.usable);
    let mut blocked: Vec<StoredBlocked> = listing
        .blocked
        .into_iter()
        .filter_map(|b| {
            let name = valid_domains(vec![b.name]).pop()?;
            (!usable.contains(&name)).then_some(StoredBlocked { name, reason: b.reason })
        })
        .collect();
    blocked.sort_by(|a, b| a.name.cmp(&b.name));
    blocked.dedup_by(|a, b| a.name == b.name);
    (usable, blocked)
}

/// "a.com, b.org: reason" for blocked domains that share a reason, which they do per provider.
fn blocked_text(blocked: &[StoredBlocked]) -> String {
    let names: Vec<&str> = blocked.iter().map(|b| b.name.as_str()).collect();
    format!("{}: {}", names.join(", "), blocked.first().map_or("", |b| b.reason.as_str()))
}

fn failed(error: impl std::fmt::Display) -> ApiError {
    ApiError::Failed(error.to_string())
}

impl<I: WebIo> NodoWeb<I> {
    /// The providers, read from either file form. The flag says the file still has the old form.
    pub(super) fn provider_store(&self) -> (ProviderStore, bool) {
        let Some(text) = self.io.read(&self.paths.dns_credentials).and_then(|b| String::from_utf8(b).ok()) else {
            return (ProviderStore::default(), false);
        };
        if let Ok(store) = toml::from_str::<ProviderStore>(&text) {
            return (store, false);
        }
        match toml::from_str::<LegacyStore>(&text) {
            Ok(old) => {
                let kind = old.dns.get("provider").cloned().unwrap_or_default();
                let provider = StoredProvider {
                    id: kind,
                    dns: old.dns,
                    dns01: old.dns01,
                    domains: vec![old.domain],
                    checked_at: None,
                    problem: None,
                    key_hint: None,
                    blocked: Vec::new(),
                };
                (ProviderStore { version: STORE_VERSION, providers: vec![provider] }, true)
            }
            Err(_) => (ProviderStore::default(), false),
        }
    }

    pub(super) fn save_store(&self, store: &ProviderStore) -> Result<(), String> {
        let store = ProviderStore { version: STORE_VERSION, providers: store.providers.clone() };
        let text = toml::to_string(&store).map_err(|e| e.to_string())?;
        if let Some(dir) = self.paths.dns_credentials.parent() {
            self.io.ensure_dir(dir, (0o700, "root", "root"))?;
        }
        self.io.write(&self.paths.dns_credentials, text.as_bytes(), ROOT_ONLY)
    }

    /// Rewrites a store still in the old form, and moves nodo-ddns's state to its domain's file so
    /// the records it published are still known (and so still reported if they go stale).
    pub(super) fn migrate_providers(&self) -> Result<(), String> {
        let (store, legacy) = self.provider_store();
        if !legacy {
            return Ok(());
        }
        if let Some(domain) = store.providers.first().and_then(|p| p.domains.first()) {
            let target = self.zone_state(domain);
            if let Some(state) = self.io.read(&self.paths.ddns_state) {
                if self.io.read(&target).is_none() {
                    self.io.write(&target, &state, (0o600, "_nododdns", "_nododdns"))?;
                }
                self.io.remove(&self.paths.ddns_state)?;
            }
        }
        self.save_store(&store)
    }

    /// nodo-ddns's state file for one domain.
    pub(super) fn zone_state(&self, domain: &str) -> PathBuf {
        self.paths.ddns_state.with_file_name(format!("{domain}.json"))
    }

    /// The provider table with every sealed value opened.
    pub(super) fn open_provider(&self, provider: &StoredProvider, key: &SecretsKey) -> Result<BTreeMap<String, String>, String> {
        self.opened(&provider.credentials(""), key)
    }

    fn provider_config(&self, provider: &StoredProvider, key: &SecretsKey) -> Result<ProviderConfig, ApiError> {
        let dns = self.open_provider(provider, key).map_err(failed)?;
        let text = toml::to_string(&dns).map_err(failed)?;
        toml::from_str(&text).map_err(|_| failed("the stored provider settings are not valid"))
    }

    /// A provider client for one domain of a connected provider.
    pub(super) fn provider_for(
        &self,
        provider: &StoredProvider,
        domain: &str,
        key: Option<&SecretsKey>,
    ) -> Result<Box<dyn DnsProvider>, ApiError> {
        let key = key.ok_or_else(|| ApiError::Locked("unlock the device to use the DNS provider".to_owned()))?;
        let config = self.provider_config(provider, key)?;
        self.io.dns_provider(config, domain).map_err(ApiError::Failed)
    }

    /// Checks the fields against the provider's form and seals them. Returns the sealed table and the
    /// config built from the clear values, for listing or testing before anything is stored.
    pub(super) fn seal_fields(
        &self,
        kind: &str,
        fields: &[(String, Secret)],
        key: &SecretsKey,
    ) -> Result<(BTreeMap<String, String>, ProviderConfig), ApiError> {
        let info = catalog::lookup(kind)
            .filter(|p| p.status == ProviderStatus::Implemented)
            .ok_or_else(|| failed(format!("{} is not a supported provider", kind.escape_debug())))?;
        let expected = field_names(info.id);
        let mut dns = BTreeMap::from([("provider".to_owned(), info.id.to_owned())]);
        for (name, value) in fields {
            if !expected.contains(name) {
                return Err(failed(format!("{} is not a field of {}", name.escape_debug(), info.name)));
            }
            dns.insert(name.clone(), value.expose().trim().to_owned());
        }
        // Parsing it the way nodo-ddns will catches missing fields before anything is stored.
        let text = toml::to_string(&dns).map_err(failed)?;
        let config = toml::from_str::<ProviderConfig>(&text)
            .map_err(|_| failed(format!("{} needs: {}", info.name, expected.join(", "))))?;
        let mut sealed = BTreeMap::new();
        for (name, value) in &dns {
            let stored = if name == "provider" {
                value.clone()
            } else {
                let mut nonce = [0u8; 24];
                self.io.random(&mut nonce).map_err(ApiError::Failed)?;
                secrets::seal(key, SEAL_PACKAGE, name, value, &nonce).map_err(ApiError::Failed)?
            };
            sealed.insert(name.clone(), stored);
        }
        Ok((sealed, config))
    }

    fn describe(&self, store: &ProviderStore, provider: &StoredProvider, input: &WebInput) -> DomainProvider {
        let kind = provider.kind().to_owned();
        let domains = provider
            .domains
            .iter()
            .map(|domain| {
                let owner = store.owner(domain).filter(|o| o.id != provider.id);
                let used_by = input
                    .sites
                    .iter()
                    .filter(|s| s.exposure.public && owner.is_none())
                    .filter(|s| {
                        site_hosts(s).any(|host| {
                            store.zone_of(host).is_some_and(|(p, d)| p.id == provider.id && d == domain)
                        })
                    })
                    .map(|s| SiteId { package: s.package.clone(), site: s.service.name.clone() })
                    .collect();
                ProviderDomain { name: domain.clone(), used_by, held_by: owner.map(|o| display_name(o.kind())) }
            })
            .collect();
        DomainProvider {
            id: provider.id.clone(),
            name: display_name(&kind),
            lists_domains: catalog::lookup(&kind).is_some_and(|p| p.lists_domains),
            dns01: dns01_support(&kind),
            kind,
            domains,
            checked_at: provider.checked_at,
            problem: provider.problem.clone(),
            key: provider.key_hint.clone().and_then(|hint| {
                Some(ProviderKey { field: field_names(provider.kind()).into_iter().next()?, hint })
            }),
            blocked: provider
                .blocked
                .iter()
                .map(|b| BlockedDomain { name: b.name.clone(), reason: b.reason.clone() })
                .collect(),
        }
    }

    /// The hint for a provider's first credential, from its opened table.
    fn hint_of(kind: &str, opened: &BTreeMap<String, String>) -> Option<String> {
        let identifier = first_field_is_an_identifier(kind);
        field_names(kind).first().and_then(|field| opened.get(field)).map(|value| key_hint(value, identifier))
    }

    pub(super) fn list_domain_providers(&self, input: &WebInput) -> Vec<DomainProvider> {
        let (store, _) = self.provider_store();
        store.providers.iter().map(|p| self.describe(&store, p, input)).collect()
    }

    /// Lists the key's domains, or for a provider that cannot list, proves the key on `domain`.
    /// Beside them come the account's domains the key may not change yet, when the provider tells.
    fn discover(&self, config: ProviderConfig, domain: Option<&str>) -> Result<(Vec<String>, Vec<StoredBlocked>), ApiError> {
        let name = display_name(config.kind());
        let wanted = domain
            .map(|d| Zone::new(&d.trim().to_ascii_lowercase()).map(|z| z.as_str().to_owned()))
            .transpose()
            .map_err(failed)?;
        let listing = self.io.list_domains(config.clone()).map_err(ApiError::Failed)?.map(valid_listing);
        match (listing, wanted) {
            (Some((listed, blocked)), Some(domain)) if !listed.contains(&domain) => {
                // The account has it but the key may not change it: say what to switch on.
                if let Some(found) = blocked.iter().find(|b| b.name == domain) {
                    return Err(failed(format!("{name} accepted the key, but cannot change {domain} yet. {}", found.reason)));
                }
                Err(failed(format!(
                    "{name} accepted the key, but it does not control {domain}. It controls: {}",
                    if listed.is_empty() { "no domains".to_owned() } else { listed.join(", ") }
                )))
            }
            (Some((listed, blocked)), _) if listed.is_empty() && !blocked.is_empty() => Err(failed(format!(
                "{name} accepted the key, but it may not change any domain yet. {}",
                blocked_text(&blocked)
            ))),
            (Some((listed, _)), _) if listed.is_empty() => {
                Err(failed(format!("{name} accepted the key, but it controls no domains the API may change")))
            }
            (Some(listing), _) => Ok(listing),
            (None, None) => Err(failed(format!("{name} cannot list your domains, so type the domain to use"))),
            (None, Some(domain)) => {
                let provider = self.io.dns_provider(config, &domain).map_err(ApiError::Failed)?;
                provider.test().map_err(failed)?;
                Ok((vec![domain], Vec::new()))
            }
        }
    }

    pub(super) fn add_domain_provider(
        &self,
        kind: &str,
        fields: &[(String, Secret)],
        domain: Option<&str>,
        input: &WebInput,
    ) -> Result<DomainProvider, ApiError> {
        let key = input
            .secrets
            .as_deref()
            .ok_or_else(|| ApiError::Locked("unlock the device to store DNS credentials".to_owned()))?;
        let (sealed, config) = self.seal_fields(kind, fields, key)?;
        let (domains, blocked) = self.discover(config, domain)?;
        let (mut store, _) = self.provider_store();
        let first = field_names(kind).into_iter().next();
        let identifier = first_field_is_an_identifier(kind);
        let key_hint = fields
            .iter()
            .find(|(name, _)| Some(name) == first.as_ref())
            .map(|(_, value)| key_hint(value.expose(), identifier));
        let provider = StoredProvider {
            id: store.next_id(kind),
            dns: sealed,
            dns01: BTreeMap::new(),
            domains,
            checked_at: Some(self.io.now()),
            problem: None,
            key_hint,
            blocked,
        };
        store.providers.push(provider.clone());
        self.save_store(&store).map_err(ApiError::Failed)?;
        // A new domain can change which challenge certificates use, so everything is applied again.
        ExposureApplier::apply(self, input).map_err(ApiError::Failed)?;
        Ok(self.describe(&store, &provider, input))
    }

    pub(super) fn remove_domain_provider(&self, id: &str, input: &WebInput) -> Result<(), ApiError> {
        let (mut store, _) = self.provider_store();
        let index = store.get(id)?;
        let provider = &store.providers[index];
        let in_use: Vec<String> = input
            .sites
            .iter()
            .filter(|s| s.exposure.public)
            .flat_map(site_hosts)
            .filter(|host| store.zone_of(host).is_some_and(|(p, _)| p.id == provider.id))
            .map(str::to_owned)
            .collect();
        if !in_use.is_empty() {
            return Err(failed(format!(
                "{} serves {}. Take those sites off the internet, or move them to another domain, first",
                display_name(provider.kind()),
                in_use.join(", ")
            )));
        }
        store.providers.remove(index);
        self.save_store(&store).map_err(ApiError::Failed)?;
        ExposureApplier::apply(self, input).map_err(ApiError::Failed)
    }

    pub(super) fn refresh_domain_provider(&self, id: &str, input: &WebInput) -> Result<DomainProvider, ApiError> {
        let key = input
            .secrets
            .as_deref()
            .ok_or_else(|| ApiError::Locked("unlock the device to use the DNS provider".to_owned()))?;
        let (mut store, _) = self.provider_store();
        let index = store.get(id)?;
        let config = self.provider_config(&store.providers[index], key)?;
        let name = display_name(config.kind());
        // A connection made before hints were kept gets its hint now, while the key can be opened.
        if store.providers[index].key_hint.is_none() {
            let opened = self.open_provider(&store.providers[index], key).map_err(failed)?;
            store.providers[index].key_hint = Self::hint_of(config.kind(), &opened);
        }
        let before = store.providers[index].domains.clone();
        match self.io.list_domains(config).map(|l| l.map(valid_listing)) {
            Ok(Some((listed, blocked))) => {
                let provider = &mut store.providers[index];
                provider.blocked = blocked;
                // A domain a site still uses stays, so its record and certificate keep working, but
                // the problem says the key no longer lists it.
                let kept: Vec<String> = provider.domains.iter().filter(|d| !listed.contains(d)).cloned().collect();
                let still_used: Vec<String> = kept
                    .into_iter()
                    .filter(|d| {
                        input.sites.iter().any(|s| s.exposure.public && site_hosts(s).any(|host| under(host, d)))
                    })
                    .collect();
                provider.problem = (!still_used.is_empty()).then(|| {
                    format!("{name} no longer lists {}, which a site still uses", still_used.join(", "))
                });
                let mut domains = listed;
                domains.extend(still_used);
                domains.sort();
                domains.dedup();
                provider.domains = domains;
                provider.checked_at = Some(self.io.now());
            }
            Ok(None) => {
                store.providers[index].checked_at = Some(self.io.now());
            }
            Err(e) => store.providers[index].problem = Some(e),
        }
        self.save_store(&store).map_err(ApiError::Failed)?;
        // Only a changed set of domains can change what is served or which challenge is used, and
        // the providers are looked up again every time their screen opens.
        if store.providers[index].domains != before {
            ExposureApplier::apply(self, input).map_err(ApiError::Failed)?;
        }
        Ok(self.describe(&store, &store.providers[index], input))
    }

    pub(super) fn add_provider_domain(&self, id: &str, domain: &str, input: &WebInput) -> Result<DomainProvider, ApiError> {
        let key = input
            .secrets
            .as_deref()
            .ok_or_else(|| ApiError::Locked("unlock the device to use the DNS provider".to_owned()))?;
        let (mut store, _) = self.provider_store();
        let index = store.get(id)?;
        let domain = Zone::new(&domain.trim().to_ascii_lowercase()).map_err(failed)?.as_str().to_owned();
        if store.providers[index].domains.contains(&domain) {
            return Ok(self.describe(&store, &store.providers[index], input));
        }
        let config = self.provider_config(&store.providers[index], key)?;
        let (added, _) = self.discover(config, Some(&domain))?;
        let provider = &mut store.providers[index];
        provider.domains.extend(added.into_iter().filter(|d| d == &domain));
        provider.domains.sort();
        provider.domains.dedup();
        provider.checked_at = Some(self.io.now());
        provider.problem = None;
        self.save_store(&store).map_err(ApiError::Failed)?;
        ExposureApplier::apply(self, input).map_err(ApiError::Failed)?;
        Ok(self.describe(&store, &store.providers[index], input))
    }

    /// The DNS-01 challenge for each owned domain whose provider has a certificate module. While the
    /// device is locked the sealed credentials cannot be opened, so each domain keeps the provider
    /// block Caddy already runs for it, which the owner set up while unlocked.
    pub(super) fn zone_challenges(&self, key: Option<&SecretsKey>) -> Vec<ZoneChallenge> {
        let (store, _) = self.provider_store();
        let kept = if key.is_none() {
            self.io
                .read(&self.paths.caddy_config)
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .map(|old| DnsChallenge::kept_by_subject(&old))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        store
            .owned()
            .into_iter()
            .filter_map(|(provider, domain)| {
                match catalog::lookup(provider.kind()).map(|p| p.dns01) {
                    None | Some(Dns01::Unavailable) => return None,
                    Some(Dns01::BundledSeparateCredentials(_)) if provider.dns01.is_empty() => return None,
                    Some(_) => {}
                }
                let challenge = match key {
                    Some(key) => self.open_challenge(provider.kind(), &provider.credentials(domain), key)?,
                    None => kept.iter().find(|(subject, _)| under(subject, domain)).map(|(_, c)| c.clone())?,
                };
                challenge.usable().then(|| ZoneChallenge { zone: domain.to_owned(), challenge })
            })
            .collect()
    }

    /// Stores changed credentials of the first provider, which the single-provider requests address.
    pub(super) fn save_primary(&self, credentials: &Credentials) -> Result<(), String> {
        let (mut store, _) = self.provider_store();
        if let Some(provider) = store.providers.iter_mut().find(|p| p.domains.contains(&credentials.domain)) {
            provider.dns = credentials.dns.clone();
            provider.dns01 = credentials.dns01.clone();
        } else {
            let kind = credentials.dns.get("provider").cloned().unwrap_or_default();
            store.providers.push(StoredProvider {
                id: store.next_id(&kind),
                dns: credentials.dns.clone(),
                dns01: credentials.dns01.clone(),
                domains: vec![credentials.domain.clone()],
                checked_at: None,
                problem: None,
                key_hint: None,
                blocked: Vec::new(),
            });
        }
        self.save_store(&store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, domains: &[&str]) -> StoredProvider {
        StoredProvider {
            id: id.to_owned(),
            dns: BTreeMap::from([("provider".to_owned(), id.split('-').next().unwrap().to_owned())]),
            domains: domains.iter().map(|d| (*d).to_owned()).collect(),
            ..StoredProvider::default()
        }
    }

    #[test]
    fn a_domain_belongs_to_the_first_provider_that_holds_it() {
        let store = ProviderStore {
            version: 2,
            providers: vec![provider("porkbun", &["example.com", "example.org"]), provider("cloudflare", &["example.com", "shop.example.com"])],
        };
        assert_eq!(store.owner("example.com").unwrap().id, "porkbun");
        assert!(!store.owns(&store.providers[1], "example.com"));
        assert_eq!(store.zone_of("pay.example.com").map(|(p, d)| (p.id.as_str(), d)), Some(("porkbun", "example.com")));
        // The longest domain wins when domains nest.
        assert_eq!(store.zone_of("a.shop.example.com").map(|(p, d)| (p.id.as_str(), d)), Some(("cloudflare", "shop.example.com")));
        assert_eq!(store.zone_of("notexample.com"), None);
        assert_eq!(store.owned().len(), 3);
    }

    #[test]
    fn ids_stay_unique_per_kind() {
        let store = ProviderStore { version: 2, providers: vec![provider("cloudflare", &[]), provider("cloudflare-2", &[])] };
        assert_eq!(store.next_id("cloudflare"), "cloudflare-3");
        assert_eq!(store.next_id("gandi"), "gandi");
    }
}
