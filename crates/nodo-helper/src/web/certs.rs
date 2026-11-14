//! Certificates: the owner's ACME choices, the challenge the helper picks, what Caddy has issued,
//! and the DNS records behind local names under the owner's domain.
//!
//! Local names such as `shop.home.example.com` point at the device's local address through an A
//! record at the owner's DNS provider, written by the helper. A record is the only way every
//! device on the LAN finds the name without configuring anything, and the provider is already
//! connected for DNS-01. It publishes the private address, and some routers drop such answers, so
//! [`NodoWeb::local_name_checks`] tests what this network's resolver actually returns.

use super::*;

use nodo_api::{
    CertProblem, CertProblemKind, CertState, ChallengeChoice, HostCertificate, HostKind, LocalNameVerdict,
};
use nodo_web::acme::{self, DnsView, Phase, ProblemKind};
use nodo_web::caddy::{AcmeConfig, DnsChallenge, LETS_ENCRYPT, LETS_ENCRYPT_STAGING};
use nodo_web::cert::{self, CertInfo};
use nodo_web::dns::RecordType;
use nodo_web::names;
use nodo_web::secret::Secret as WebSecret;

/// The owner's choices, as stored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct AcmeStored {
    pub email: Option<String>,
    #[serde(default)]
    pub staging: bool,
    pub local_domain: Option<String>,
}

/// A test ACME server. Present only on test devices, and shown on the screens when it is.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcmeTest {
    directory: String,
    #[serde(default = "yes")]
    dns_propagation_check: bool,
    renew_interval_secs: Option<u32>,
}

fn yes() -> bool {
    true
}

/// What the helper last published for local names.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct LocalRecords {
    pub domain: String,
    pub names: Vec<String>,
    pub address: Option<String>,
    pub error: Option<String>,
}

const LOCAL_RECORD_TTL: u32 = 300;

/// Fields a provider's certificate module needs beyond the dynamic DNS ones.
fn dns01_fields(provider: &str) -> &'static [&'static str] {
    match provider {
        "godaddy" => &["api_key", "api_secret"],
        _ => &[],
    }
}

fn ports(reason: impl Into<String>) -> ChallengeChoice {
    ChallengeChoice::PortsNeeded { reason: reason.into() }
}

impl<I: WebIo> NodoWeb<I> {
    pub(super) fn acme_stored(&self) -> AcmeStored {
        self.io
            .read(&self.paths.acme_settings)
            .and_then(|bytes| toml::from_str(std::str::from_utf8(&bytes).ok()?).ok())
            .unwrap_or_default()
    }

    fn acme_test(&self) -> Option<AcmeTest> {
        let bytes = self.io.read(&self.paths.acme_test)?;
        toml::from_str(std::str::from_utf8(&bytes).ok()?).ok()
    }

    pub(super) fn acme_config(&self, stored: &AcmeStored) -> AcmeConfig {
        let test = self.acme_test();
        let directory = match &test {
            Some(test) => test.directory.clone(),
            None if stored.staging => LETS_ENCRYPT_STAGING.to_owned(),
            None => LETS_ENCRYPT.to_owned(),
        };
        AcmeConfig {
            directory,
            dns_propagation_check: test.as_ref().is_none_or(|t| t.dns_propagation_check),
            renew_interval_secs: test.as_ref().and_then(|t| t.renew_interval_secs),
            events_log: Some(self.paths.tls_events.to_string_lossy().into_owned()),
        }
    }

    /// DNS-01 when the provider has a certificate module and its credentials are stored, else the
    /// challenges that need forwarded ports, with the reason.
    pub(super) fn dns_challenge(&self, key: Option<&SecretsKey>) -> (Option<DnsChallenge>, ChallengeChoice) {
        let Some(credentials) = self.credentials() else {
            return (None, ports("No DNS provider is connected."));
        };
        let id = credentials.dns.get("provider").cloned().unwrap_or_default();
        let info = catalog::lookup(&id);
        let name = info.map_or_else(|| id.clone(), |p| p.name.to_owned());
        match info.map(|p| p.dns01) {
            None | Some(Dns01::Unavailable) => return (None, ports(format!("{name} has no certificate module."))),
            Some(Dns01::BundledSeparateCredentials(_)) if credentials.dns01.is_empty() => {
                return (None, ports(format!("{name} needs its own API key and secret for certificates.")));
            }
            Some(_) => {}
        }
        let challenge = match key {
            Some(key) => self.open_challenge(&id, &credentials, key),
            // Locked: keep what Caddy runs, which the owner already set up while unlocked.
            None => self
                .io
                .read(&self.paths.caddy_config)
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .and_then(|old| DnsChallenge::kept_from(&old)),
        };
        match challenge {
            Some(challenge) if challenge.usable() => (Some(challenge), ChallengeChoice::Dns01 { provider: name }),
            Some(_) => (None, ports(format!("The stored {name} credentials cannot be used for certificates."))),
            None if key.is_none() => (None, ports("The device is locked, so the DNS provider credentials are sealed.")),
            None => (None, ports(format!("The stored {name} credentials could not be opened."))),
        }
    }

    pub(super) fn open_challenge(&self, id: &str, credentials: &Credentials, key: &SecretsKey) -> Option<DnsChallenge> {
        let dns = self.opened(credentials, key).ok()?;
        let dns01: BTreeMap<String, String> = credentials
            .dns01
            .iter()
            .map(|(name, value)| secrets::open(key, SEAL_DNS01, name, value).map(|v| (name.clone(), v.to_string())))
            .collect::<Result<_, _>>()
            .ok()?;
        let get = |map: &BTreeMap<String, String>, field: &str| map.get(field).map(|v| WebSecret::new(v.clone()));
        Some(match id {
            "porkbun" => DnsChallenge::Porkbun { api_key: get(&dns, "api_key")?, secret_key: get(&dns, "secret_key")? },
            "cloudflare" => DnsChallenge::Cloudflare { api_token: get(&dns, "api_token")? },
            "godaddy" => DnsChallenge::GoDaddy { api_key: get(&dns01, "api_key")?, api_secret: get(&dns01, "api_secret")? },
            "gandi" => DnsChallenge::Gandi { personal_access_token: get(&dns, "personal_access_token")? },
            "namecom" => DnsChallenge::NameCom { username: dns.get("username")?.clone(), api_token: get(&dns, "api_token")? },
            "alibaba" => DnsChallenge::Alibaba {
                access_key_id: get(&dns, "access_key_id")?,
                access_key_secret: get(&dns, "access_key_secret")?,
            },
            _ => return None,
        })
    }

    pub(super) fn acme_info(&self, key: Option<&SecretsKey>) -> AcmeSettingsInfo {
        let stored = self.acme_stored();
        let config = self.acme_config(&stored);
        let credentials = self.credentials();
        let provider = credentials.as_ref().and_then(|c| c.dns.get("provider").cloned()).unwrap_or_default();
        AcmeSettingsInfo {
            email: stored.email,
            staging: stored.staging,
            test_server: self.acme_test().is_some(),
            directory: config.directory,
            challenge: self.dns_challenge(key).1,
            dns01_fields: dns01_fields(&provider).iter().map(|f| (*f).to_owned()).collect(),
            dns01_stored: credentials.is_some_and(|c| !c.dns01.is_empty()),
            local_domain: stored.local_domain,
            local_address: match self.local_address() {
                LocalAddress::Ready { address } => Some(address.to_string()),
                LocalAddress::Unavailable { .. } => None,
            },
        }
    }

    pub(super) fn store_acme_settings(&self, settings: &AcmeSettingsInput, input: &WebInput) -> Result<(), ApiError> {
        let email = settings.email.as_deref().map(str::trim).filter(|e| !e.is_empty()).map(str::to_owned);
        if email.as_deref().is_some_and(|e| !caddy::is_email(e)) {
            return Err(ApiError::Failed("that email address is not valid".to_owned()));
        }
        let local_domain = settings
            .local_domain
            .as_deref()
            .map(|d| d.trim().trim_end_matches('.').to_ascii_lowercase())
            .filter(|d| !d.is_empty());
        if let Some(local_domain) = &local_domain {
            if !names::is_public_name(local_domain) {
                return Err(ApiError::Failed(
                    "the local domain must be a full domain name, such as home.example.com".to_owned(),
                ));
            }
            let credentials = self
                .credentials()
                .ok_or_else(|| ApiError::Failed("connect your domain and DNS provider first".to_owned()))?;
            let zone = &credentials.domain;
            if local_domain != zone && !local_domain.ends_with(&format!(".{zone}")) {
                return Err(ApiError::Failed(format!(
                    "local names must be under {zone}, where your DNS provider can publish them"
                )));
            }
            if let (_, ChallengeChoice::PortsNeeded { reason }) = self.dns_challenge(input.secrets.as_deref()) {
                return Err(ApiError::Failed(format!(
                    "local names under your domain need certificates through your DNS provider. {reason}"
                )));
            }
        }
        let old = self.acme_stored();
        let new = AcmeStored { email, staging: settings.staging, local_domain };
        let text = toml::to_string(&new).map_err(|e| ApiError::Failed(e.to_string()))?;
        let previous = self.io.read(&self.paths.acme_settings);
        if let Some(dir) = self.paths.acme_settings.parent() {
            self.io.ensure_dir(dir, (0o700, "root", "root")).map_err(ApiError::Failed)?;
        }
        self.io.write(&self.paths.acme_settings, text.as_bytes(), ROOT_ONLY).map_err(ApiError::Failed)?;
        if let Err(reason) = ExposureApplier::apply(self, input) {
            let _ = match previous {
                Some(bytes) => self.io.write(&self.paths.acme_settings, &bytes, ROOT_ONLY),
                None => self.io.remove(&self.paths.acme_settings),
            };
            let _ = ExposureApplier::apply(self, input);
            return Err(ApiError::Failed(format!("could not apply the certificate settings: {reason}")));
        }
        // Caddy keeps serving a stored certificate from any issuer until it is due for renewal, so
        // untrusted staging certificates are removed once production is chosen again.
        if old.staging && !new.staging && self.acme_test().is_none() {
            let staging = self.paths.caddy_storage.join("certificates").join(acme::issuer_key(LETS_ENCRYPT_STAGING));
            if let Ok(plan) = self.plan(input) {
                for address in plan.addresses.iter().filter(|a| a.kind != AddressKind::Onion) {
                    let _ = self.io.remove_dir_all(&staging.join(&address.host));
                }
                let _ = self.apply_files(&plan);
            }
        }
        Ok(())
    }

    pub(super) fn store_dns01(&self, fields: &[(String, Secret)], input: &WebInput) -> Result<(), ApiError> {
        let key = input
            .secrets
            .as_deref()
            .ok_or_else(|| ApiError::Locked("unlock the device to store DNS credentials".to_owned()))?;
        let mut credentials =
            self.credentials().ok_or_else(|| ApiError::Failed("connect a DNS provider first".to_owned()))?;
        let id = credentials.dns.get("provider").cloned().unwrap_or_default();
        let name = catalog::lookup(&id).map_or_else(|| id.clone(), |p| p.name.to_owned());
        let expected = dns01_fields(&id);
        if expected.is_empty() {
            return Err(ApiError::Failed(format!("{name} uses its dynamic DNS credentials for certificates as well")));
        }
        let mut sealed = BTreeMap::new();
        if !fields.is_empty() {
            for (field, value) in fields {
                if !expected.contains(&field.as_str()) {
                    return Err(ApiError::Failed(format!("{} is not a certificate field of {name}", field.escape_debug())));
                }
                if value.expose().trim().is_empty() {
                    return Err(ApiError::Failed(format!("{field} is empty")));
                }
                let mut nonce = [0u8; 24];
                self.io.random(&mut nonce).map_err(ApiError::Failed)?;
                let value = secrets::seal(key, SEAL_DNS01, field, value.expose().trim(), &nonce).map_err(ApiError::Failed)?;
                sealed.insert(field.clone(), value);
            }
            if let Some(missing) = expected.iter().find(|f| !sealed.contains_key(**f)) {
                return Err(ApiError::Failed(format!("{name} needs {missing} for certificates")));
            }
        }
        credentials.dns01 = sealed;
        self.save_primary(&credentials).map_err(ApiError::Failed)?;
        ExposureApplier::apply(self, input).map_err(ApiError::Failed)
    }

    fn stored_cert(&self, issuer: &str, host: &str) -> Option<CertInfo> {
        let path = self.paths.caddy_storage.join("certificates").join(issuer).join(host).join(format!("{host}.crt"));
        cert::parse_pem(&String::from_utf8(self.io.read(&path)?).ok()?)
    }

    fn public_ip(&self) -> Option<IpAddr> {
        let state: DdnsState = serde_json::from_slice(&self.io.read(&self.paths.ddns_state)?).ok()?;
        state.observed_v4?.parse().ok()
    }

    pub(super) fn certificate_report(&self, plan: &WebPlan, input: &WebInput) -> CertificateReport {
        let events = self
            .io
            .read(&self.paths.tls_events)
            .map(|bytes| acme::parse_log(&String::from_utf8_lossy(&bytes)))
            .unwrap_or_default();
        let stored = self.acme_stored();
        let issuer = acme::issuer_key(&self.acme_config(&stored).directory);
        let now = self.io.now();
        let mut hosts = Vec::new();
        for address in plan.addresses.iter().filter(|a| a.kind != AddressKind::Onion) {
            let kind = match address.kind {
                AddressKind::Public => HostKind::Public,
                _ if names::is_local_name(&address.host) => HostKind::LocalInternal,
                _ => HostKind::LocalDomain,
            };
            let site = SiteId { package: address.package.clone(), site: address.site.clone() };
            if kind == HostKind::LocalInternal {
                // The local CA issues at once and without any network, so there is no progress.
                let cert = self.stored_cert("local", &address.host);
                let valid = cert.as_ref().is_some_and(|c| c.covers(&address.host) && c.not_after > now);
                hosts.push(HostCertificate {
                    site,
                    host: address.host.clone(),
                    kind,
                    state: if valid { CertState::Valid } else { CertState::Obtaining },
                    issuer: Some("this device's local CA".to_owned()),
                    not_after: cert.as_ref().map(|c| c.not_after),
                    last_renewal: cert.as_ref().map(|c| c.not_before),
                    challenge: None,
                    last_error: None,
                });
                continue;
            }
            let cert = self.stored_cert(&issuer, &address.host);
            let mut status = acme::host_status(&address.host, &events, cert.clone(), &DnsView::default(), now);
            // Only a failed port challenge needs the slower look at where public DNS points.
            let port_failure = status.last_error.as_ref().is_some_and(|e| match &e.kind {
                ProblemKind::ChallengeFailed { challenges } => challenges.iter().any(|c| c != "dns-01"),
                ProblemKind::Other => true,
                _ => false,
            });
            if kind == HostKind::Public && port_failure {
                // The dynamic DNS service knows the public address. Without it, the IP echo services
                // are asked, as the reachability check does, and only because a certificate failed.
                let expected = self.public_ip().or_else(|| self.io.detect_public_ip());
                let dns = DnsView { resolved: self.io.resolve(&address.host), expected };
                status = acme::host_status(&address.host, &events, cert, &dns, now);
            }
            hosts.push(HostCertificate {
                site,
                host: address.host.clone(),
                kind,
                state: match status.phase {
                    Phase::Waiting => CertState::Waiting,
                    Phase::Obtaining => CertState::Obtaining,
                    Phase::Valid => CertState::Valid,
                    Phase::Failed => CertState::Failed,
                    Phase::Expired => CertState::Expired,
                },
                issuer: status.cert.as_ref().map(CertInfo::issuer),
                not_after: status.cert.as_ref().map(|c| c.not_after),
                last_renewal: status.last_renewal,
                challenge: status.challenge,
                last_error: status.last_error.map(|e| CertProblem {
                    at: e.at,
                    kind: match e.kind {
                        ProblemKind::ChallengeFailed { challenges } => CertProblemKind::ChallengeFailed { challenges },
                        ProblemKind::RateLimited => CertProblemKind::RateLimited,
                        ProblemKind::DnsNotPointingHere => CertProblemKind::DnsNotPointingHere,
                        ProblemKind::CaaBlocked => CertProblemKind::CaaBlocked,
                        ProblemKind::DnsProvider => CertProblemKind::DnsProvider,
                        ProblemKind::Other => CertProblemKind::Other,
                    },
                    summary: e.summary,
                    detail: e.detail,
                }),
            });
        }
        let any_internal = hosts.iter().any(|h| h.kind == HostKind::LocalInternal);
        CertificateReport {
            hosts,
            challenge: self.dns_challenge(input.secrets.as_deref()).1,
            staging: stored.staging && self.acme_test().is_none(),
            test_server: self.acme_test().is_some(),
            local_ca_fingerprint: any_internal.then(|| self.local_ca_fingerprint()).flatten(),
        }
    }

    fn local_ca_fingerprint(&self) -> Option<String> {
        self.local_ca_root().map(|root| root.fingerprint())
    }

    pub(super) fn local_records(&self) -> LocalRecords {
        self.io
            .read(&self.paths.local_records)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn local_record_name(zone: &str, host: &str) -> Option<RecordName> {
        if host == zone {
            Some(RecordName::apex())
        } else {
            RecordName::new(host.strip_suffix(&format!(".{zone}"))?).ok()
        }
    }

    /// Points each local name under the owner's domain at the local address, and removes the
    /// records of names no longer served. Failures are kept for the screens rather than failing
    /// the exposure change, since the sites themselves are already applied.
    pub(super) fn apply_local_names(&self, plan: &WebPlan, key: Option<&SecretsKey>) {
        let mut record = self.local_records();
        let current = &plan.local.dns_names;
        if current.is_empty() && record.names.is_empty() {
            return;
        }
        // Locked: the provider credentials are sealed, so the records stay as they are.
        let Some(key) = key else {
            return;
        };
        // Without an address the records stay as they are, rather than being removed and written
        // again when a lease or a service restart briefly takes the address away. Names no longer
        // wanted are removed once there is an address again.
        let LocalAddress::Ready { address } = plan.local.address else {
            return;
        };
        let mut errors = Vec::new();
        let mut kept = Vec::new();
        match self.local_provider(Some(key)) {
            Ok((zone, provider)) => {
                let zone = zone.as_str();
                for old in record.names.iter().filter(|n| !current.contains(n)) {
                    let removed = Self::local_record_name(zone, old)
                        .map(|name| provider.delete(&name, RecordType::A).map_err(|e| e.to_string()));
                    if let Some(Err(e)) = removed {
                        errors.push(format!("could not remove {old}: {e}"));
                        kept.push(old.clone());
                    }
                }
                for host in current {
                    match Self::local_record_name(zone, host) {
                        Some(name) => {
                            if let Err(e) = provider.upsert(&name, IpAddr::V4(address), LOCAL_RECORD_TTL) {
                                errors.push(format!("could not publish {host}: {e}"));
                            }
                        }
                        None => errors.push(format!("{host} is not under {zone}")),
                    }
                }
                record.domain = zone.to_owned();
            }
            Err(e) => errors.push(e.to_string()),
        }
        let mut names = current.clone();
        names.extend(kept);
        record = LocalRecords {
            names,
            address: Some(address.to_string()),
            error: (!errors.is_empty()).then(|| errors.join("; ")),
            ..record
        };
        if let Some(e) = &record.error {
            eprintln!("nodo-helper: local names: {e}");
        }
        if let Ok(text) = serde_json::to_vec_pretty(&record) {
            let _ = self.io.write(&self.paths.local_records, &text, ROOT_ONLY);
        }
    }

    /// The domain and provider local names are published under: the provider that holds the
    /// chosen local domain, or the first provider.
    fn local_provider(&self, key: Option<&SecretsKey>) -> Result<(String, Box<dyn DnsProvider>), ApiError> {
        let (store, _) = self.provider_store();
        if let Some(local_domain) = self.acme_stored().local_domain {
            if let Some((provider, domain)) = store.zone_of(&local_domain) {
                return Ok((domain.to_owned(), self.provider_for(provider, domain, key)?));
            }
        }
        let credentials = self.credentials().ok_or_else(|| ApiError::Failed("no DNS provider is connected".to_owned()))?;
        Ok((credentials.domain, self.provider(key)?))
    }

    pub(super) fn local_name_checks(&self, plan: &WebPlan, key: Option<&SecretsKey>) -> Result<Vec<LocalNameCheck>, ApiError> {
        if plan.local.dns_names.is_empty() {
            return Ok(Vec::new());
        }
        let (zone, provider) = self.local_provider(key)?;
        let zone = zone.as_str();
        let local_domain = self.acme_stored().local_domain.unwrap_or_else(|| zone.to_owned());
        let address = match plan.local.address {
            LocalAddress::Ready { address } => IpAddr::V4(address),
            LocalAddress::Unavailable { .. } => return Err(ApiError::Failed("there is no local address yet".to_owned())),
        };
        let list = |ips: &[IpAddr]| ips.iter().map(ToString::to_string).collect::<Vec<_>>();
        let mut checks = Vec::new();
        for host in &plan.local.dns_names {
            let published = Self::local_record_name(zone, host)
                .and_then(|name| provider.read(&name, RecordType::A).ok())
                .unwrap_or_default();
            let resolved = self.io.resolve(host).unwrap_or_default();
            let (verdict, explanation) = if !published.contains(&address) {
                (
                    LocalNameVerdict::NotPublished,
                    format!(
                        "Your DNS provider does not have {host} pointing at {address}. It is written when local sites are applied, so check the provider credentials."
                    ),
                )
            } else if resolved.contains(&address) {
                (LocalNameVerdict::Resolves, format!("{host} resolves to {address} on this network."))
            } else if resolved.is_empty() {
                (
                    LocalNameVerdict::Blocked,
                    format!(
                        "Your DNS provider has {host} pointing at {address}, but this network's DNS returns nothing for it. \
Routers with DNS rebinding protection drop private addresses from public answers. Allow {local_domain} as a rebinding \
exception in the router, or use a DNS server without that filter. Right after a change it can also take a few minutes."
                    ),
                )
            } else {
                (
                    LocalNameVerdict::WrongAddress,
                    format!(
                        "This network's DNS answers {} for {host}, not {address}. Another record or a cached answer is in the way.",
                        list(&resolved).join(", ")
                    ),
                )
            };
            checks.push(LocalNameCheck {
                host: host.clone(),
                address: address.to_string(),
                published: list(&published),
                resolves_to: list(&resolved),
                verdict,
                explanation,
            });
        }
        Ok(checks)
    }
}
