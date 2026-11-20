use super::*;
use nodo_api::{AcmeSettingsInput, CertProblemKind, CertState, ChallengeChoice, HostKind, LocalNameVerdict};
use crate::exposure::WebSite;
use nodo_api::Exposure;
use nodo_manifest::WebService;
use nodo_web::dns::{Change, DnsError, RecordType, UpsertOutcome};
use std::sync::{Arc, Mutex};

type StoredOwner = (u32, String, String);
type Records = Arc<Mutex<BTreeMap<String, Vec<IpAddr>>>>;

/// A DNS provider that keeps records in memory, by relative name.
struct FakeProvider(Records);

impl DnsProvider for FakeProvider {
    fn min_ttl(&self) -> u32 {
        60
    }
    fn read(&self, name: &RecordName, _: RecordType) -> Result<Vec<IpAddr>, DnsError> {
        Ok(self.0.lock().unwrap().get(name.as_str()).cloned().unwrap_or_default())
    }
    fn upsert(&self, name: &RecordName, ip: IpAddr, _: u32) -> Result<UpsertOutcome, DnsError> {
        self.0.lock().unwrap().insert(name.as_str().to_owned(), vec![ip]);
        Ok(UpsertOutcome { change: Change::Created, warnings: Vec::new() })
    }
    fn delete(&self, name: &RecordName, _: RecordType) -> Result<(), DnsError> {
        self.0.lock().unwrap().remove(name.as_str());
        Ok(())
    }
}

#[derive(Default)]
struct Fake {
    files: Mutex<BTreeMap<PathBuf, (Vec<u8>, StoredOwner)>>,
    services: Mutex<BTreeMap<String, bool>>,
    calls: Mutex<Vec<String>>,
    caddy_running: bool,
    caddy_refuses: bool,
    nft_refuses: bool,
    retire_output: String,
    records: Records,
    /// What `list_domains` answers per provider kind; a missing kind cannot list.
    listings: Mutex<BTreeMap<String, Result<Vec<String>, String>>>,
    /// The account's domains a kind's key may not change yet, reported beside its listing.
    blocked: Mutex<BTreeMap<String, Vec<String>>>,
    /// What this device's resolver answers, by name. Missing names fail to resolve.
    resolved: Mutex<BTreeMap<String, Vec<IpAddr>>>,
    removed_dirs: Mutex<Vec<String>>,
    now: u64,
    tor_installed: bool,
    /// A device whose nodo-tools has no web GUI service yet.
    web_gui_missing: bool,
}

impl Fake {
    fn get(&self, path: &str) -> Option<String> {
        self.files.lock().unwrap().get(Path::new(path)).map(|(b, _)| String::from_utf8(b.clone()).unwrap())
    }
    fn owner(&self, path: &str) -> StoredOwner {
        self.files.lock().unwrap()[Path::new(path)].1.clone()
    }
    fn put(&self, path: &str, text: &str) {
        self.files.lock().unwrap().insert(path.into(), (text.as_bytes().to_vec(), (0o644, "root".into(), "root".into())));
    }
    fn up(&self, name: &str) -> bool {
        self.services.lock().unwrap().get(name).copied().unwrap_or(false)
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl WebIo for Fake {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(path).map(|(b, _)| b.clone())
    }
    fn write(&self, path: &Path, bytes: &[u8], (mode, user, group): Owner) -> Result<(), String> {
        self.files.lock().unwrap().insert(path.into(), (bytes.to_vec(), (mode, user.into(), group.into())));
        Ok(())
    }
    fn remove(&self, path: &Path) -> Result<(), String> {
        self.files.lock().unwrap().remove(path);
        Ok(())
    }
    fn ensure_dir(&self, _: &Path, _: Owner) -> Result<(), String> {
        Ok(())
    }
    fn nft_reload(&self, _: &Path) -> Result<(), String> {
        self.calls.lock().unwrap().push("nft".into());
        if self.nft_refuses && self.get("/nft/web.nft").is_some_and(|t| t.contains("dnat")) {
            return Err("syntax error".into());
        }
        Ok(())
    }
    fn caddy_load(&self, _: &Path, _: &[u8]) -> Result<bool, String> {
        self.calls.lock().unwrap().push("caddy-load".into());
        if self.caddy_refuses {
            return Err("listen tcp: address in use".into());
        }
        Ok(self.caddy_running)
    }
    fn service_installed(&self, name: &str) -> bool {
        match name {
            "tor" => self.tor_installed,
            "nodo-web-gui" => !self.web_gui_missing,
            _ => true,
        }
    }
    fn service_running(&self, name: &str) -> bool {
        self.up(name)
    }
    fn service(&self, name: &str, up: bool, restart: bool) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("{} {name}", if !up { "down" } else if restart { "restart" } else { "up" }));
        self.services.lock().unwrap().insert(name.into(), up);
        Ok(())
    }
    fn retire_stale(&self, _: &Path) -> Result<String, String> {
        self.calls.lock().unwrap().push("retire".into());
        Ok(self.retire_output.clone())
    }
    fn http(&self) -> SharedHttp {
        unreachable!("no test reaches the network")
    }
    fn sleep(&self, _: Duration) {}
    fn random(&self, out: &mut [u8]) -> Result<(), String> {
        out.fill(9);
        Ok(())
    }
    fn list_dir(&self, _: &Path) -> Vec<String> {
        Vec::new()
    }
    fn remove_dir_all(&self, path: &Path) -> Result<(), String> {
        let prefix = path.to_string_lossy().into_owned();
        self.files.lock().unwrap().retain(|p, _| !p.starts_with(&prefix));
        self.removed_dirs.lock().unwrap().push(prefix);
        Ok(())
    }
    fn resolve(&self, host: &str) -> Option<Vec<IpAddr>> {
        self.resolved.lock().unwrap().get(host).cloned()
    }
    fn now(&self) -> u64 {
        self.now
    }
    fn caddy_get(&self, _: &Path, _: &str) -> Option<Vec<u8>> {
        None
    }
    fn detect_public_ip(&self) -> Option<IpAddr> {
        None
    }
    fn dns_provider(&self, _: ProviderConfig, _: &str) -> Result<Box<dyn DnsProvider>, String> {
        Ok(Box::new(FakeProvider(self.records.clone())))
    }
    fn list_domains(&self, config: ProviderConfig) -> Result<Option<nodo_web::dns::DomainListing>, String> {
        self.calls.lock().unwrap().push(format!("list {}", config.kind()));
        let usable = self.listings.lock().unwrap().get(config.kind()).cloned().transpose()?;
        let blocked = self.blocked.lock().unwrap().get(config.kind()).cloned().unwrap_or_default();
        Ok(usable.map(|usable| nodo_web::dns::DomainListing {
            usable,
            blocked: blocked
                .into_iter()
                .map(|name| nodo_web::dns::BlockedDomain { name, reason: "API Access is off for this domain".to_owned() })
                .collect(),
        }))
    }
}

fn paths() -> WebPaths {
    WebPaths {
        caddy_config: "/web/caddy.json".into(),
        caddy_admin: "/run/admin.sock".into(),
        nft_fragment: "/nft/web.nft".into(),
        nft_main: "/nft/main.conf".into(),
        ddns_config: "/secrets/nodo-ddns/config.toml".into(),
        ddns_state: "/ddns/state.json".into(),
        dns_credentials: "/nodo/web-dns.toml".into(),
        mdns_names: "/localnet/mdns-names".into(),
        localnet_state: "/run/localnet/state".into(),
        torrc: "/web/torrc".into(),
        tor_sv_conf: "/sv/tor/conf".into(),
        tor_dir: "/tor".into(),
        acme_settings: "/nodo/web-acme.toml".into(),
        acme_test: "/etc/acme-test.toml".into(),
        caddy_storage: "/caddy".into(),
        tls_events: "/caddy-state/nodo-tls.log".into(),
        local_records: "/web/local-records.json".into(),
        trust_dir: "/web/trust".into(),
        local_applied: "/web/local-address".into(),
    }
}

fn web(fake: Fake) -> NodoWeb<Fake> {
    let web = NodoWeb::new(paths(), fake);
    web.io.put("/run/localnet/state", "status=ready\naddress=192.168.1.60\n");
    web
}

fn key() -> std::sync::Arc<SecretsKey> {
    std::sync::Arc::new(SecretsKey::derive(&[7u8; crate::keys::DEVICE_KEY_LEN]))
}

fn input(local: bool, public: bool) -> WebInput {
    WebInput {
        secrets: Some(key()),
        sites: vec![WebSite {
            package: "shop".into(),
            service: WebService {
                name: "web".into(),
                upstream: "127.0.0.1:3001".into(),
                suggested_subdomain: Some("shop".into()),
                multi_address: true,
                upstream_protocol: nodo_manifest::UpstreamProtocol::H1,
                paths: Default::default(),
            },
            exposure: Exposure { local, onion: false, public },
            public_host: Some("shop.example.com".into()),
            public_aliases: Vec::new(),
            locked: false,
        }],
        node_onion: None,
        web_gui: false,
    }
}

fn store_cloudflare(web: &NodoWeb<Fake>) {
    let token = Secret::new("cfut_0123456789abcdefghijABCDEFGHIJ0123456789");
    web.set_provider("example.com", "cloudflare", &[("api_token".into(), token)], &input(false, false)).unwrap();
}

#[test]
fn local_and_public_write_every_file_and_start_the_services() {
    let web = web(Fake::default());
    web.apply(&input(true, true)).unwrap();

    let json = web.io.get("/web/caddy.json").unwrap();
    assert!(json.contains("192.168.1.60:10444") && json.contains("shop.example.com"));
    assert_eq!(web.io.owner("/web/caddy.json"), (0o640, "root".into(), "_caddy".into()));
    let nft = web.io.get("/nft/web.nft").unwrap();
    assert!(nft.contains("dnat ip to 192.168.1.60:10444") && nft.contains("redirect to :10443"));
    assert_eq!(web.io.get("/localnet/mdns-names").unwrap(), "nodo.local\nshop.local\n");
    assert_eq!(web.io.get("/web/local-address").unwrap(), "192.168.1.60\n");
    for service in ["nodo-localnet", "nodo-caddy", "nodo-mdns"] {
        assert!(web.io.up(service), "{service}");
    }
    assert!(!web.io.up("nodo-ddns"), "no provider, so no dynamic DNS");
}

#[test]
fn a_sites_path_rules_reach_the_written_proxy_config() {
    let web = web(Fake::default());
    web.apply(&input(true, true)).unwrap();
    assert!(!web.io.get("/web/caddy.json").unwrap().contains("path_regexp"), "no rules, no 404 route");

    let mut ruled = input(true, true);
    let rule: nodo_manifest::PathRule = toml::from_str("path = \"/private\"\nblocked_on = [\"public\"]\n").unwrap();
    ruled.sites[0].service.paths.insert("private".into(), rule);
    web.apply(&ruled).unwrap();
    let json = web.io.get("/web/caddy.json").unwrap();
    assert!(json.contains("\"/private;*\"") && json.contains("(?i)^(?:/private)"), "{json}");
}

#[test]
fn dynamic_dns_runs_only_while_a_site_is_public() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.apply(&input(true, true)).unwrap();
    assert!(web.io.up("nodo-ddns"));
    let config = web.io.get("/secrets/nodo-ddns/config.toml").unwrap();
    assert!(config.contains("records = [\"shop\"]") && config.contains("provider = \"cloudflare\""));
    assert_eq!(web.io.owner("/secrets/nodo-ddns/config.toml"), (0o600, "_nododdns".into(), "_nododdns".into()));

    web.apply(&input(true, false)).unwrap();
    assert!(!web.io.up("nodo-ddns"));
    assert!(web.io.get("/secrets/nodo-ddns/config.toml").is_none(), "no public site, no config with credentials");
}

#[test]
fn while_locked_dynamic_dns_waits_and_an_unlock_gives_it_its_config() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.apply(&input(false, false)).unwrap();
    let mut locked = input(false, true);
    locked.secrets = None;
    web.apply(&locked).unwrap();
    assert!(web.io.up("nodo-ddns"), "enabled, and waiting for its config");
    assert!(web.io.get("/secrets/nodo-ddns/config.toml").is_none());
    web.unlocked(&input(false, true)).unwrap();
    let config = web.io.get("/secrets/nodo-ddns/config.toml").unwrap();
    assert!(config.contains("api_token = \"cfut_0123456789"), "opened for the service");
}

#[test]
fn credentials_are_sealed_root_only_and_validated() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    assert_eq!(web.io.owner("/nodo/web-dns.toml"), (0o600, "root".into(), "root".into()));
    let stored = web.io.get("/nodo/web-dns.toml").unwrap();
    assert!(!stored.contains("cfut_") && stored.contains("api_token = \"v1."), "{stored}");
    let mut locked = input(false, false);
    locked.secrets = None;
    let token = vec![("api_token".to_owned(), Secret::new("cfut_0123456789abcdefghijABCDEFGHIJ0123456789"))];
    assert!(matches!(web.set_provider("example.com", "cloudflare", &token, &locked), Err(ApiError::Locked(_))));
    assert!(matches!(web.test_provider(&locked), Err(ApiError::Locked(_))));
    let none = input(false, false);
    let token = || vec![("api_token".to_owned(), Secret::new("x"))];
    for (domain, provider, fields) in [
        ("example.com", "namecheap", token()),
        ("example.com", "nope", token()),
        ("bad domain", "cloudflare", token()),
        ("example.com", "cloudflare", vec![("password".to_owned(), Secret::new("x"))]),
        ("example.com", "porkbun", vec![("api_key".to_owned(), Secret::new("x"))]),
    ] {
        assert!(web.set_provider(domain, provider, &fields, &none).is_err(), "{provider} {domain}");
    }
    let providers = web.providers().unwrap();
    let cloudflare = providers.iter().find(|p| p.id == "cloudflare").unwrap();
    assert!(cloudflare.configured && cloudflare.dynamic_dns && cloudflare.dns01 == Dns01Support::Bundled);
    let godaddy = providers.iter().find(|p| p.id == "godaddy").unwrap();
    assert!(matches!(godaddy.dns01, Dns01Support::BundledSeparateCredentials(_)));
    let ionos = providers.iter().find(|p| p.id == "ionos").unwrap();
    assert!(!ionos.dynamic_dns && ionos.unavailable_reason.is_some());
}

#[test]
fn without_a_local_address_the_reason_is_reported_and_nothing_falls_back() {
    let web = web(Fake::default());
    web.io.put("/run/localnet/state", "status=wireless\n");
    web.apply(&input(true, true)).unwrap();
    let json = web.io.get("/web/caddy.json").unwrap();
    assert!(!json.contains("shop.local") && !json.contains("10444"));
    assert!(!web.io.up("nodo-mdns"));

    let status = web.status(&input(true, true)).unwrap();
    assert!(status.local_address.is_none());
    assert!(status.local_unavailable.unwrap().contains("Wi-Fi"));
    assert!(status.local_remedy.unwrap().contains("static local address"));
    assert!(status.sites[0].local_waiting && status.sites[0].local_url.is_none());
    assert_eq!(status.sites[0].public_url.as_deref(), Some("https://shop.example.com"));
    assert!(!status.sites[0].rate_limit_unenforced);
}

#[test]
fn a_refused_caddy_config_puts_the_old_files_back() {
    let web = web(Fake { caddy_running: true, ..Fake::default() });
    web.apply(&input(false, true)).unwrap();
    let before = (web.io.get("/web/caddy.json"), web.io.get("/nft/web.nft"));
    let web = NodoWeb::new(paths(), Fake { caddy_refuses: true, ..Fake::default() });
    web.io.put("/run/localnet/state", "status=ready\naddress=192.168.1.60\n");
    web.io.put("/web/caddy.json", before.0.as_deref().unwrap());
    web.io.put("/nft/web.nft", before.1.as_deref().unwrap());
    assert!(web.apply(&input(true, true)).unwrap_err().contains("Caddy refused"));
    assert_eq!((web.io.get("/web/caddy.json"), web.io.get("/nft/web.nft")), before);
    assert_eq!(web.io.calls().iter().filter(|c| *c == "nft").count(), 2, "the old rules are loaded again");
}

#[test]
fn refused_firewall_rules_are_rolled_back_before_caddy_is_touched() {
    let web = web(Fake { nft_refuses: true, ..Fake::default() });
    assert!(web.apply(&input(true, false)).unwrap_err().contains("firewall"));
    assert_eq!(web.io.get("/web/caddy.json"), None);
    assert!(!web.io.calls().contains(&"caddy-load".to_owned()));
}

#[test]
fn going_fully_offline_stops_caddy() {
    let web = web(Fake::default());
    web.apply(&input(false, true)).unwrap();
    assert!(web.io.up("nodo-caddy"));
    web.apply(&input(false, false)).unwrap();
    assert!(!web.io.up("nodo-caddy"));
    assert!(!web.io.get("/nft/web.nft").unwrap().contains("accept"));
}

#[test]
fn retiring_stale_records_pauses_the_running_service() {
    let web = web(Fake { retire_output: "{\"removed\":[\"old/A\"]}\n".into(), ..Fake::default() });
    assert!(matches!(web.retire_stale(), Err(ApiError::Locked(_))), "no config, nothing to retire with");
    store_cloudflare(&web);
    web.apply(&input(false, true)).unwrap();
    assert_eq!(web.retire_stale().unwrap(), vec!["old/A"]);
    let calls = web.io.calls();
    let at = calls.iter().position(|c| c == "retire").unwrap();
    assert_eq!(calls[at - 1], "down nodo-ddns");
    assert_eq!(calls[at + 1], "up nodo-ddns");
}

#[test]
fn status_reports_a_domain_whose_updates_keep_failing() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.io.put(
        "/ddns/example.com.json",
        r#"{"published":{},"failures":3,"next_attempt_at":900,"failing_since":500,
            "last_errors":["shop: the provider rejected this request (HTTP 400, RECORD_CONFLICT)"],"observed_v4":"93.184.216.34"}"#,
    );
    let status = web.status(&input(false, true)).unwrap();
    assert_eq!(
        status.dns_problems,
        vec![nodo_api::DnsProblem {
            domain: "example.com".into(),
            errors: vec!["shop: the provider rejected this request (HTTP 400, RECORD_CONFLICT)".into()],
            failures: 3,
            since: Some(500),
            retry_at: Some(900),
        }]
    );
}

#[test]
fn status_reports_failures_from_a_ddns_that_keeps_no_errors() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.io.put("/ddns/example.com.json", r#"{"published":{},"failures":1,"next_attempt_at":60}"#);
    let status = web.status(&input(false, true)).unwrap();
    assert!(status.dns_problems[0].errors[0].contains("log"));
}

#[test]
fn status_reports_no_problem_while_updates_succeed() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.io.put("/ddns/example.com.json", r#"{"published":{},"failures":0,"observed_v4":"93.184.216.34"}"#);
    assert!(web.status(&input(false, true)).unwrap().dns_problems.is_empty());
}

#[test]
fn status_reports_stale_records_and_the_last_change() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.io.put(
        "/ddns/example.com.json",
        r#"{"published":{"shop/A":{"name":"shop","record_type":"A","ip":"93.184.216.34","changed_at":5},
            "old/A":{"name":"old","record_type":"A","ip":"93.184.216.34","changed_at":9}},"observed_v4":"93.184.216.34"}"#,
    );
    let status = web.status(&input(false, true)).unwrap();
    assert_eq!(status.stale_records, vec!["old/A"]);
    assert_eq!(status.dns_changed_at, Some(9));
    assert_eq!(status.public_ip.as_deref(), Some("93.184.216.34"));
    assert_eq!(status.domain.as_deref(), Some("example.com"));
    assert_eq!(status.provider.as_deref(), Some("cloudflare"));
}

#[test]
fn onion_addresses_come_from_tor_once_it_has_made_them() {
    let web = web(Fake::default());
    let mut sites = input(false, false);
    sites.sites[0].exposure.onion = true;
    web.apply(&sites).unwrap();
    assert!(!web.io.get("/web/caddy.json").unwrap().contains(".onion"), "tor is not installed here");
    let onion = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion";
    web.io.put("/tor/shop.web/hostname", &format!("{onion}\n"));
    web.apply(&sites).unwrap();
    assert!(web.io.get("/web/caddy.json").unwrap().contains(onion));
    assert_eq!(web.status(&sites).unwrap().sites[0].onion_url, Some(format!("http://{onion}")));
}

fn caddy_json(web: &NodoWeb<Fake>) -> serde_json::Value {
    serde_json::from_str(&web.io.get("/web/caddy.json").unwrap()).unwrap()
}

fn issuer(web: &NodoWeb<Fake>) -> serde_json::Value {
    let json = caddy_json(web);
    json["apps"]["tls"]["automation"]["policies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["subjects"][0] == "shop.example.com")
        .unwrap()["issuers"][0]
        .clone()
}

fn acme(email: Option<&str>, staging: bool, local_domain: Option<&str>) -> AcmeSettingsInput {
    AcmeSettingsInput { email: email.map(str::to_owned), staging, local_domain: local_domain.map(str::to_owned) }
}

#[test]
fn without_dns01_certificates_use_the_port_challenges_and_say_why() {
    let web = web(Fake::default());
    web.apply(&input(false, true)).unwrap();
    assert!(issuer(&web).get("challenges").is_none());
    assert_eq!(issuer(&web)["ca"], caddy::LETS_ENCRYPT);
    let info = web.acme_settings(&input(false, true)).unwrap();
    assert!(matches!(&info.challenge, ChallengeChoice::PortsNeeded { reason } if reason.contains("No DNS provider")));
    assert!(!info.test_server && !info.staging);
}

#[test]
fn a_provider_with_a_certificate_module_switches_to_dns01_and_keeps_it_while_locked() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.apply(&input(false, true)).unwrap();
    let issuer_now = issuer(&web);
    assert_eq!(issuer_now["challenges"]["dns"]["provider"]["name"], "cloudflare");
    assert_eq!(web.io.owner("/web/caddy.json"), (0o640, "root".into(), "_caddy".into()));
    let info = web.acme_settings(&input(false, true)).unwrap();
    assert_eq!(info.challenge, ChallengeChoice::Dns01 { provider: "Cloudflare".into() });

    // After a reboot the credentials are sealed, but an exposure change keeps DNS-01.
    let mut locked = input(true, true);
    locked.secrets = None;
    web.apply(&locked).unwrap();
    assert_eq!(issuer(&web)["challenges"], issuer_now["challenges"]);
    assert!(matches!(web.acme_settings(&locked).unwrap().challenge, ChallengeChoice::Dns01 { .. }));
}

#[test]
fn godaddy_needs_its_own_key_and_secret_for_dns01_and_they_are_sealed() {
    let web = web(Fake::default());
    let token = vec![("personal_access_token".to_owned(), Secret::new("gd-pat-token"))];
    web.set_provider("example.com", "godaddy", &token, &input(false, false)).unwrap();
    web.apply(&input(false, true)).unwrap();
    let info = web.acme_settings(&input(false, true)).unwrap();
    assert_eq!(info.dns01_fields, vec!["api_key", "api_secret"]);
    assert!(!info.dns01_stored);
    assert!(matches!(&info.challenge, ChallengeChoice::PortsNeeded { reason } if reason.contains("API key and secret")));
    assert!(issuer(&web).get("challenges").is_none());

    let missing = vec![("api_key".to_owned(), Secret::new("gdkey"))];
    assert!(web.set_dns01(&missing, &input(false, true)).is_err());
    let wrong = vec![("password".to_owned(), Secret::new("x")), ("api_secret".to_owned(), Secret::new("y"))];
    assert!(web.set_dns01(&wrong, &input(false, true)).is_err());
    let mut locked = input(false, true);
    locked.secrets = None;
    let fields = vec![("api_key".to_owned(), Secret::new("gdkey")), ("api_secret".to_owned(), Secret::new("gdsecret"))];
    assert!(matches!(web.set_dns01(&fields, &locked), Err(ApiError::Locked(_))));
    web.set_dns01(&fields, &input(false, true)).unwrap();

    let stored = web.io.get("/nodo/web-dns.toml").unwrap();
    assert!(!stored.contains("gdkey") && !stored.contains("gdsecret") && stored.contains("dns01"), "{stored}");
    assert_eq!(issuer(&web)["challenges"]["dns"]["provider"]["api_token"], "gdkey:gdsecret");
    assert!(web.acme_settings(&input(false, true)).unwrap().dns01_stored);
    // Changing only the domain keeps them; the dynamic DNS token is not reused for DNS-01.
    web.set_provider("example.org", "godaddy", &token, &input(false, false)).unwrap();
    assert!(web.acme_settings(&input(false, true)).unwrap().dns01_stored);
    // Cloudflare has no separate certificate credentials.
    store_cloudflare(&web);
    assert!(web.set_dns01(&fields, &input(false, true)).is_err());
}

#[test]
fn email_staging_and_a_test_server_reach_the_issuer() {
    let web = web(Fake::default());
    web.apply(&input(false, true)).unwrap();
    assert!(web.set_acme_settings(&acme(Some("not an email"), false, None), &input(false, true)).is_err());
    web.set_acme_settings(&acme(Some(" me@example.com "), true, None), &input(false, true)).unwrap();
    assert_eq!(web.io.owner("/nodo/web-acme.toml"), (0o600, "root".into(), "root".into()));
    assert_eq!(issuer(&web)["email"], "me@example.com");
    assert_eq!(issuer(&web)["ca"], caddy::LETS_ENCRYPT_STAGING);
    let info = web.acme_settings(&input(false, true)).unwrap();
    assert!(info.staging && info.email.as_deref() == Some("me@example.com"));

    // Back to production: Caddy would keep serving the untrusted staging certificate otherwise.
    let staging_dir = "/caddy/certificates/acme-staging-v02.api.letsencrypt.org-directory/shop.example.com";
    web.io.put(&format!("{staging_dir}/shop.example.com.crt"), "x");
    web.set_acme_settings(&acme(Some("me@example.com"), false, None), &input(false, true)).unwrap();
    assert_eq!(issuer(&web)["ca"], caddy::LETS_ENCRYPT);
    assert!(web.io.removed_dirs.lock().unwrap().iter().any(|d| d == staging_dir));

    web.io.put("/etc/acme-test.toml", "directory = \"https://203.0.113.1/dir\"\ndns_propagation_check = false\nrenew_interval_secs = 10\n");
    web.apply(&input(false, true)).unwrap();
    assert_eq!(issuer(&web)["ca"], "https://203.0.113.1/dir");
    assert_eq!(caddy_json(&web)["apps"]["tls"]["automation"]["renew_interval"], "10s");
    assert!(web.acme_settings(&input(false, true)).unwrap().test_server);
}

#[test]
fn local_names_under_the_domain_need_dns01_and_a_name_under_the_zone() {
    let web = web(Fake::default());
    let local = acme(None, false, Some("home.example.com"));
    assert!(web.set_acme_settings(&local, &input(true, false)).unwrap_err().to_string().contains("connect your domain"));
    let token = vec![("api_token".to_owned(), Secret::new("hostinger-token"))];
    web.set_provider("example.com", "hostinger", &token, &input(false, false)).unwrap();
    assert!(web.set_acme_settings(&local, &input(true, false)).unwrap_err().to_string().contains("certificate module"));
    store_cloudflare(&web);
    let outside = acme(None, false, Some("home.example.org"));
    assert!(web.set_acme_settings(&outside, &input(true, false)).unwrap_err().to_string().contains("under example.com"));
    assert!(web.set_acme_settings(&acme(None, false, Some("home.local")), &input(true, false)).is_err());
    assert!(web.io.get("/nodo/web-acme.toml").is_none(), "nothing refused was stored");
}

#[test]
fn local_names_are_served_on_the_local_listener_and_published_with_the_local_address() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.set_acme_settings(&acme(None, false, Some("home.example.com")), &input(true, false)).unwrap();
    let json = web.io.get("/web/caddy.json").unwrap();
    assert!(json.contains("shop.home.example.com") && !json.contains("shop.local"));
    assert!(web.io.get("/localnet/mdns-names").is_none(), "no .local name to announce");
    let status = web.status(&input(true, false)).unwrap();
    assert_eq!(status.sites[0].local_url.as_deref(), Some("https://shop.home.example.com"));
    assert_eq!(web.io.records.lock().unwrap().get("shop.home"), Some(&vec!["192.168.1.60".parse().unwrap()]));

    // The router hides private answers: the check names rebinding protection.
    let checks = web.check_local_names(&input(true, false)).unwrap();
    assert_eq!(checks[0].verdict, LocalNameVerdict::Blocked);
    assert!(checks[0].explanation.contains("rebinding"));
    web.io.resolved.lock().unwrap().insert("shop.home.example.com".into(), vec!["192.168.1.60".parse().unwrap()]);
    assert_eq!(web.check_local_names(&input(true, false)).unwrap()[0].verdict, LocalNameVerdict::Resolves);
    web.io.resolved.lock().unwrap().insert("shop.home.example.com".into(), vec!["203.0.113.9".parse().unwrap()]);
    assert_eq!(web.check_local_names(&input(true, false)).unwrap()[0].verdict, LocalNameVerdict::WrongAddress);
    web.io.records.lock().unwrap().clear();
    assert_eq!(web.check_local_names(&input(true, false)).unwrap()[0].verdict, LocalNameVerdict::NotPublished);

    // Back to .local: the record that revealed the LAN address is removed.
    web.io.records.lock().unwrap().insert("shop.home".into(), vec!["192.168.1.60".parse().unwrap()]);
    web.set_acme_settings(&acme(None, false, None), &input(true, false)).unwrap();
    assert!(web.io.records.lock().unwrap().is_empty());
    assert!(web.io.get("/web/caddy.json").unwrap().contains("shop.local"));
}

#[test]
fn local_names_wait_while_locked_without_any_dns01_config_to_keep() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.set_acme_settings(&acme(None, false, Some("home.example.com")), &input(true, false)).unwrap();
    web.io.remove(Path::new("/web/caddy.json")).unwrap();
    let mut locked = input(true, false);
    locked.secrets = None;
    web.apply(&locked).unwrap();
    let status = web.status(&locked).unwrap();
    assert!(status.sites[0].local_waiting);
    assert!(status.local_unavailable.unwrap().contains("DNS provider"));
}

const LE_DIR: &str = "/caddy/certificates/acme-v02.api.letsencrypt.org-directory";

fn cert_web(now: u64) -> NodoWeb<Fake> {
    let web = web(Fake { now, ..Fake::default() });
    web.apply(&input(false, true)).unwrap();
    web
}

#[test]
fn a_stored_certificate_from_the_configured_issuer_reports_valid() {
    let web = cert_web(1_795_000_000);
    let report = web.certificates(&input(false, true)).unwrap();
    assert_eq!(report.hosts[0].state, CertState::Waiting);
    web.io.put(
        "/caddy-state/nodo-tls.log",
        r#"{"level":"info","ts":1794999000.1,"logger":"tls.obtain","msg":"obtaining certificate","identifier":"shop.example.com"}"#,
    );
    assert_eq!(web.certificates(&input(false, true)).unwrap().hosts[0].state, CertState::Obtaining);
    web.io.put(
        &format!("{LE_DIR}/shop.example.com/shop.example.com.crt"),
        include_str!("../../../nodo-web/tests/fixtures/cert.pem"),
    );
    let report = web.certificates(&input(false, true)).unwrap();
    let host = &report.hosts[0];
    assert_eq!((host.state, host.kind), (CertState::Valid, HostKind::Public));
    assert_eq!(host.issuer.as_deref(), Some("Test Org (Test Issuer R1)"));
    assert_eq!(host.not_after, Some(1_801_267_200));
    assert_eq!(host.last_renewal, Some(1_793_491_200));
    // A certificate from another issuer, such as staging, does not count for production.
    let web = cert_web(1_795_000_000);
    web.io.put(
        "/caddy/certificates/acme-staging-v02.api.letsencrypt.org-directory/shop.example.com/shop.example.com.crt",
        include_str!("../../../nodo-web/tests/fixtures/cert.pem"),
    );
    assert_ne!(web.certificates(&input(false, true)).unwrap().hosts[0].state, CertState::Valid);
}

#[test]
fn a_failed_challenge_with_dns_elsewhere_says_dns_is_not_pointing_here() {
    let web = cert_web(2000);
    store_cloudflare(&web);
    // Each owned domain has its own state file; the single state.json is from before that.
    web.io.put("/ddns/state.json", r#"{"published":{},"observed_v4":"203.0.113.77"}"#);
    web.io.put("/ddns/example.com.json", r#"{"published":{},"observed_v4":"203.0.113.2"}"#);
    web.io.put(
        "/caddy-state/nodo-tls.log",
        &[
            r#"{"ts":1000.0,"logger":"tls.obtain","msg":"obtaining certificate","identifier":"shop.example.com"}"#,
            r#"{"ts":1001.0,"logger":"http.acme_client","msg":"challenge failed","identifier":"shop.example.com","challenge_type":"http-01","problem":{"type":"urn:ietf:params:acme:error:connection","detail":"Connection refused"}}"#,
            r#"{"ts":1002.0,"logger":"tls.obtain","msg":"could not get certificate from issuer","identifier":"shop.example.com","error":"HTTP 400 urn:ietf:params:acme:error:connection - Connection refused"}"#,
        ]
        .join("\n"),
    );
    let host = web.certificates(&input(false, true)).unwrap().hosts.remove(0);
    assert_eq!(host.state, CertState::Failed);
    assert!(matches!(host.last_error.as_ref().unwrap().kind, CertProblemKind::ChallengeFailed { .. }));
    web.io.resolved.lock().unwrap().insert("shop.example.com".into(), vec!["203.0.113.99".parse().unwrap()]);
    let host = web.certificates(&input(false, true)).unwrap().hosts.remove(0);
    let error = host.last_error.unwrap();
    assert_eq!(error.kind, CertProblemKind::DnsNotPointingHere);
    assert!(error.summary.contains("203.0.113.99") && error.summary.contains("203.0.113.2"), "{}", error.summary);
    assert_eq!(host.challenge.as_deref(), Some("http-01"));
}

/// Caddy's local CA root, as it stores it. Any certificate does for these tests.
const ROOT_PEM: &str = include_str!("../../../nodo-web/tests/fixtures/cert.pem");
const ROOT_FP: &str = "31:E5:AB:8B:5C:45:A6:50:47:ED:0F:37:0B:F0:4F:FA:05:5F:22:C0:74:78:85:65:90:49:4A:68:E2:DA:71:C9";
const ROOT_PATH: &str = "/caddy/pki/authorities/local/root.crt";

fn move_local(web: &NodoWeb<Fake>, address: &str) {
    web.io.put("/run/localnet/state", &format!("status=ready\naddress={address}\n"));
}

fn mdns_restarts(web: &NodoWeb<Fake>) -> usize {
    web.io.calls().iter().filter(|c| *c == "restart nodo-mdns").count()
}

#[test]
fn a_new_local_address_moves_the_listener_the_firewall_and_mdns() {
    let web = web(Fake::default());
    web.apply(&input(true, true)).unwrap();
    assert!(!web.refresh_local(&input(true, true)).unwrap(), "nothing changed");
    let restarts = mdns_restarts(&web);

    move_local(&web, "192.168.1.61");
    assert!(web.refresh_local(&input(true, true)).unwrap());
    let json = web.io.get("/web/caddy.json").unwrap();
    assert!(json.contains("192.168.1.61:10444") && !json.contains("192.168.1.60"));
    let nft = web.io.get("/nft/web.nft").unwrap();
    assert!(nft.contains("dnat ip to 192.168.1.61:10444") && !nft.contains("192.168.1.60"));
    assert!(nft.contains("redirect to :10443"), "public sites stay as they were");
    assert_eq!(mdns_restarts(&web), restarts + 1, "the responder reads its address when it starts");
    assert_eq!(web.io.get("/web/local-address").unwrap(), "192.168.1.61\n");
    assert_eq!(web.status(&input(true, true)).unwrap().local_address.as_deref(), Some("192.168.1.61"));
    assert!(!web.refresh_local(&input(true, true)).unwrap(), "applied once");
    assert_eq!(mdns_restarts(&web), restarts + 1);
}

#[test]
fn losing_the_local_address_takes_local_sites_down_until_it_returns() {
    let web = web(Fake::default());
    web.apply(&input(true, false)).unwrap();
    web.io.put("/run/localnet/state", "status=no-lease\ninterface=nodo-local\n");
    assert!(web.refresh_local(&input(true, false)).unwrap());
    assert!(!web.io.get("/web/caddy.json").unwrap().contains("10444"));
    assert!(!web.io.get("/nft/web.nft").unwrap().contains("dnat"));
    assert!(!web.io.up("nodo-mdns"));
    assert_eq!(web.io.get("/web/local-address").unwrap(), "");
    move_local(&web, "192.168.1.62");
    assert!(web.refresh_local(&input(true, false)).unwrap());
    assert!(web.io.get("/web/caddy.json").unwrap().contains("192.168.1.62:10444"));
    assert!(web.io.up("nodo-mdns"));
}

fn local_names_web() -> NodoWeb<Fake> {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.set_acme_settings(&acme(None, false, Some("home.example.com")), &input(true, false)).unwrap();
    assert_eq!(web.io.records.lock().unwrap().get("shop.home"), Some(&vec!["192.168.1.60".parse().unwrap()]));
    web
}

#[test]
fn records_of_local_names_follow_the_address_while_unlocked() {
    let web = local_names_web();
    move_local(&web, "192.168.1.61");
    assert!(web.refresh_local(&input(true, false)).unwrap());
    assert_eq!(web.io.records.lock().unwrap().get("shop.home"), Some(&vec!["192.168.1.61".parse().unwrap()]));
    assert!(web.io.get("/web/caddy.json").unwrap().contains("192.168.1.61:10444"));
    let status = web.local_site_status(&input(true, false)).unwrap();
    assert_eq!(status.address.as_deref(), Some("192.168.1.61"));
    assert_eq!(status.records_note, None);
    assert_eq!(status.trust_url, None, "no .local site, so no local CA to trust");
}

#[test]
fn losing_the_address_for_a_moment_keeps_the_records() {
    let web = local_names_web();
    web.io.put("/run/localnet/state", "status=waiting\n");
    assert!(web.refresh_local(&input(true, false)).unwrap());
    assert_eq!(web.io.records.lock().unwrap().get("shop.home"), Some(&vec!["192.168.1.60".parse().unwrap()]));
    move_local(&web, "192.168.1.61");
    assert!(web.refresh_local(&input(true, false)).unwrap());
    assert_eq!(web.io.records.lock().unwrap().get("shop.home"), Some(&vec!["192.168.1.61".parse().unwrap()]));
    // Names no longer served are removed once there is an address to work with.
    web.set_acme_settings(&acme(None, false, None), &input(true, false)).unwrap();
    assert!(web.io.records.lock().unwrap().is_empty());
}

#[test]
fn while_locked_a_missing_address_keeps_the_dns01_config_for_when_it_returns() {
    let web = local_names_web();
    let dns01 = |web: &NodoWeb<Fake>| caddy::DnsChallenge::kept_from(&caddy_json(web)).is_some();
    assert!(dns01(&web));
    let mut locked = input(true, false);
    locked.secrets = None;
    let before = web.io.get("/web/caddy.json");
    // At boot nodo-localnet reports waiting before its address is ready.
    web.io.put("/run/localnet/state", "status=waiting\n");
    assert!(web.refresh_local(&locked).unwrap());
    assert_eq!(web.io.get("/web/caddy.json"), before, "Caddy keeps the credentials it cannot get back");
    assert!(!web.io.up("nodo-mdns"));
    move_local(&web, "192.168.1.61");
    assert!(web.refresh_local(&locked).unwrap());
    assert!(dns01(&web), "still DNS-01, so the local name is served");
    assert!(web.io.get("/web/caddy.json").unwrap().contains("192.168.1.61:10444"));
    assert!(!web.status(&locked).unwrap().sites[0].local_waiting);
}

#[test]
fn while_locked_records_wait_for_the_unlock_and_the_status_says_why() {
    let web = local_names_web();
    let mut locked = input(true, false);
    locked.secrets = None;
    move_local(&web, "192.168.1.61");
    assert!(web.refresh_local(&locked).unwrap());
    assert!(web.io.get("/web/caddy.json").unwrap().contains("192.168.1.61:10444"), "the listener moves at once");
    assert_eq!(
        web.io.records.lock().unwrap().get("shop.home"),
        Some(&vec!["192.168.1.60".parse().unwrap()]),
        "the credentials are sealed"
    );
    let note = web.local_site_status(&locked).unwrap().records_note.unwrap();
    assert!(note.contains("192.168.1.61") && note.contains("192.168.1.60") && note.contains("unlocked"), "{note}");
    assert!(note.contains("shop.home.example.com"), "{note}");

    web.unlocked(&input(true, false)).unwrap();
    assert_eq!(web.io.records.lock().unwrap().get("shop.home"), Some(&vec!["192.168.1.61".parse().unwrap()]));
    assert_eq!(web.local_site_status(&input(true, false)).unwrap().records_note, None);
}

#[test]
fn the_trust_page_serves_caddys_root_with_its_fingerprint() {
    let web = web(Fake::default());
    web.io.put(ROOT_PATH, ROOT_PEM);
    web.apply(&input(true, false)).unwrap();
    let root = nodo_web::trust::RootCert::from_pem(ROOT_PEM).unwrap();
    let der = web.io.files.lock().unwrap()[Path::new("/web/trust/nodo-local-ca.crt")].0.clone();
    assert_eq!(der, root.der);
    assert_eq!(web.io.get("/web/trust/nodo-local-ca.pem").unwrap(), root.pem);
    let page = web.io.get("/web/trust/index.html").unwrap();
    assert!(page.contains(&ROOT_FP[..47]) && page.contains(&ROOT_FP[48..]));
    for file in ["index.html", "nodo-local-ca.pem", "nodo-local-ca.crt"] {
        assert_eq!(web.io.owner(&format!("/web/trust/{file}")), (0o640, "root".into(), "_caddy".into()));
    }
    let status = web.local_site_status(&input(true, false)).unwrap();
    assert_eq!(status.trust_url.as_deref(), Some("http://nodo.local"));
    assert_eq!(status.ca_fingerprint.as_deref(), Some(ROOT_FP));
    assert_eq!(web.certificates(&input(true, false)).unwrap().local_ca_fingerprint.as_deref(), Some(ROOT_FP));
    assert!(web.io.get("/web/caddy.json").unwrap().contains("\"root\": \"/web/trust\""));

    web.apply(&input(false, true)).unwrap();
    assert!(web.io.get("/web/trust/index.html").is_none() && web.io.get("/web/trust/nodo-local-ca.crt").is_none());
    assert!(web.local_site_status(&input(false, true)).unwrap().trust_url.is_none());
}

#[test]
fn before_caddy_has_a_root_the_page_says_so_and_the_check_fills_it_in() {
    let web = web(Fake::default());
    web.apply(&input(true, false)).unwrap();
    assert!(web.io.get("/web/trust/index.html").unwrap().contains("not ready"));
    assert!(web.io.get("/web/trust/nodo-local-ca.crt").is_none());
    assert!(!web.refresh_local(&input(true, false)).unwrap(), "still no root");
    web.io.put(ROOT_PATH, ROOT_PEM);
    assert!(web.refresh_local(&input(true, false)).unwrap());
    assert!(web.io.get("/web/trust/index.html").unwrap().contains(&ROOT_FP[..47]));
    assert!(web.io.get("/web/trust/nodo-local-ca.pem").is_some());
    assert!(!web.refresh_local(&input(true, false)).unwrap());
}

#[test]
fn local_dot_local_hosts_report_the_device_ca() {
    let web = cert_web(1_795_000_000);
    web.apply(&input(true, false)).unwrap();
    let report = web.certificates(&input(true, false)).unwrap();
    let host = &report.hosts[0];
    assert_eq!((host.kind, host.state), (HostKind::LocalInternal, CertState::Obtaining));
    assert_eq!(host.host, "shop.local");
}

const NODE_ONION: &str = "bcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwxa.onion";

fn tor_web() -> NodoWeb<Fake> {
    let web = web(Fake { tor_installed: true, ..Fake::default() });
    web.io.services.lock().unwrap().insert("tor".into(), true);
    web
}

fn with_node(mut input: WebInput) -> WebInput {
    input.node_onion = Some(crate::onion::NodeOnionTarget { provider: "cuprated".into(), port: 18081 });
    input
}

#[test]
fn the_node_onion_forwards_only_to_the_restricted_rpc() {
    let web = tor_web();
    let node = with_node(input(false, false));
    web.io.put("/tor/node-rpc/hostname", &format!("{NODE_ONION}\n"));
    web.apply(&node).unwrap();
    let torrc = web.io.get("/web/torrc").unwrap();
    assert!(torrc.contains("HiddenServiceDir /tor/node-rpc\n"), "{torrc}");
    // cuprated's only RPC is the restricted one, on loopback.
    assert!(torrc.contains("HiddenServicePort 18081 127.0.0.1:18081\n"), "{torrc}");
    let ports: Vec<&str> = torrc.lines().filter(|l| l.starts_with("HiddenServicePort")).collect();
    assert_eq!(ports, ["HiddenServicePort 18081 127.0.0.1:18081"], "only that port, only on loopback: {torrc}");
    assert!(!web.io.get("/web/caddy.json").unwrap().contains(NODE_ONION), "Caddy has no part in it");
    let found = web.onion_addresses(&node).unwrap();
    assert!(found.tor_installed && found.tor_running);
    assert_eq!(found.node.as_deref(), Some(NODE_ONION));
}

#[test]
fn turning_every_onion_off_stops_tor_publishing() {
    let web = tor_web();
    let mut node = with_node(input(false, false));
    web.apply(&node).unwrap();
    assert!(web.io.get("/web/torrc").unwrap().contains("HiddenServiceDir"));
    node.node_onion = None;
    web.apply(&node).unwrap();
    let torrc = web.io.get("/web/torrc").unwrap();
    assert!(!torrc.contains("HiddenService"), "Tor kept publishing a service that was turned off: {torrc}");
    assert!(web.io.calls.lock().unwrap().iter().any(|c| c == "down tor"));
    assert_eq!(web.onion_addresses(&node).unwrap().node, None);
}

#[test]
fn a_site_onion_leaves_the_node_out_unless_asked() {
    let web = tor_web();
    let mut sites = input(false, false);
    sites.sites[0].exposure.onion = true;
    web.apply(&sites).unwrap();
    let torrc = web.io.get("/web/torrc").unwrap();
    assert!(torrc.contains("HiddenServiceDir /tor/shop.web"), "{torrc}");
    assert!(!torrc.contains("node-rpc"), "{torrc}");
}

fn two_sites(public: bool) -> WebInput {
    let mut input = input(false, public);
    let mut blog = input.sites[0].clone();
    blog.package = "blog".into();
    blog.service.suggested_subdomain = Some("blog".into());
    blog.service.upstream = "127.0.0.1:3002".into();
    blog.public_host = Some("example.org".into());
    input.sites.push(blog);
    input
}

fn cloudflare_token() -> Vec<(String, Secret)> {
    vec![("api_token".into(), Secret::new("cfut_0123456789abcdefghijABCDEFGHIJ0123456789"))]
}

fn gandi_token() -> Vec<(String, Secret)> {
    vec![("personal_access_token".into(), Secret::new("pat_0123456789abcdef"))]
}

fn lists(web: &NodoWeb<Fake>, kind: &str, domains: &[&str]) {
    web.io.listings.lock().unwrap().insert(kind.into(), Ok(domains.iter().map(|d| (*d).to_owned()).collect()));
}

#[test]
fn an_old_single_provider_store_becomes_the_first_provider_with_its_state() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    // Write the file in the form devices had before several providers, from the same sealed values.
    let (store, _) = web.provider_store();
    let old = &store.providers[0];
    let legacy = toml::to_string(&Credentials { domain: "example.com".into(), dns: old.dns.clone(), dns01: old.dns01.clone() }).unwrap();
    web.io.put("/nodo/web-dns.toml", &legacy);
    web.io.put("/ddns/state.json", r#"{"published":{"shop/A":{"name":"shop","record_type":"A","ip":"93.184.216.34","changed_at":5}}}"#);
    assert!(web.provider_store().1, "the old form is recognised");

    web.apply(&input(false, true)).unwrap();
    let stored = web.io.get("/nodo/web-dns.toml").unwrap();
    assert!(stored.contains("version = 2") && stored.contains("[[provider]]"), "{stored}");
    assert!(!stored.contains("cfut_"), "credentials stay sealed: {stored}");
    assert!(web.io.get("/ddns/state.json").is_none());
    assert!(web.io.get("/ddns/example.com.json").unwrap().contains("shop/A"));
    let providers = web.list_domain_providers(&input(false, true));
    assert_eq!(providers.len(), 1);
    assert_eq!((providers[0].id.as_str(), providers[0].domains[0].name.as_str()), ("cloudflare", "example.com"));
    assert_eq!(providers[0].domains[0].used_by.len(), 1);
    // The sealed credentials still open, so dynamic DNS gets them.
    assert!(web.io.get("/secrets/nodo-ddns/config.toml").unwrap().contains("cfut_0123456789"));
}

#[test]
fn adding_providers_discovers_their_domains_and_each_domain_uses_its_own() {
    let web = web(Fake::default());
    lists(&web, "cloudflare", &["example.com"]);
    lists(&web, "gandi", &["example.org", "example.com"]);
    let cf = web.add_domain_provider("cloudflare", &cloudflare_token(), None, &two_sites(true)).unwrap();
    assert_eq!(cf.domains.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["example.com"]);
    assert!(cf.lists_domains && cf.checked_at.is_some());
    let gandi = web.add_domain_provider("gandi", &gandi_token(), None, &two_sites(true)).unwrap();
    // example.com stays with Cloudflare, connected first.
    let held = gandi.domains.iter().find(|d| d.name == "example.com").unwrap();
    assert_eq!(held.held_by.as_deref(), Some("Cloudflare"));
    assert!(held.used_by.is_empty());

    web.apply(&two_sites(true)).unwrap();
    let stored = web.io.get("/nodo/web-dns.toml").unwrap();
    assert!(!stored.contains("cfut_") && !stored.contains("pat_0123"), "{stored}");

    // nodo-ddns gets one zone per domain, each with its provider and its own state file.
    let ddns: toml::Value = toml::from_str(&web.io.get("/secrets/nodo-ddns/config.toml").unwrap()).unwrap();
    let zones = ddns["zones"].as_array().unwrap();
    let zone = |domain: &str| zones.iter().find(|z| z["domain"].as_str() == Some(domain)).unwrap().clone();
    assert_eq!(zone("example.com")["dns"]["provider"].as_str(), Some("cloudflare"));
    assert_eq!(zone("example.com")["records"].as_array().unwrap()[0].as_str(), Some("shop"));
    assert_eq!(zone("example.com")["state_file"].as_str(), Some("/ddns/example.com.json"));
    assert_eq!(zone("example.org")["dns"]["provider"].as_str(), Some("gandi"));
    assert_eq!(zone("example.org")["records"].as_array().unwrap()[0].as_str(), Some("@"));

    // Certificates are answered at each domain's provider.
    let json = caddy_json(&web);
    let policies = json["apps"]["tls"]["automation"]["policies"].as_array().unwrap().clone();
    let provider_of = |host: &str| {
        policies.iter().find(|p| p["subjects"].as_array().unwrap().iter().any(|s| s == host)).unwrap()["issuers"][0]
            ["challenges"]["dns"]["provider"]["name"]
            .clone()
    };
    assert_eq!(provider_of("shop.example.com"), "cloudflare");
    assert_eq!(provider_of("example.org"), "gandi");

    let listed = web.list_domain_providers(&two_sites(true));
    assert_eq!(listed.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["cloudflare", "gandi"]);
    let org = listed[1].domains.iter().find(|d| d.name == "example.org").unwrap();
    assert_eq!(org.used_by[0].package, "blog");
}

#[test]
fn a_provider_that_cannot_list_needs_a_domain_proved_with_a_test_record() {
    let web = web(Fake::default());
    let token = vec![("personal_access_token".to_owned(), Secret::new("gd-pat-token"))];
    let error = web.add_domain_provider("godaddy", &token, None, &input(false, false)).unwrap_err();
    assert!(error.to_string().contains("type the domain"), "{error}");
    let added = web.add_domain_provider("godaddy", &token, Some("Example.com"), &input(false, false)).unwrap();
    assert!(!added.lists_domains);
    assert_eq!(added.domains[0].name, "example.com");
    // The test record was written and removed again.
    assert!(web.io.records.lock().unwrap().is_empty());
    let more = web.add_provider_domain("godaddy", "example.net", &input(false, false)).unwrap();
    assert_eq!(more.domains.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["example.com", "example.net"]);
}

#[test]
fn a_listing_must_include_the_typed_domain_and_bad_input_is_refused() {
    let web = web(Fake::default());
    lists(&web, "cloudflare", &["example.org"]);
    let error = web.add_domain_provider("cloudflare", &cloudflare_token(), Some("example.com"), &input(false, false)).unwrap_err();
    assert!(error.to_string().contains("does not control example.com"), "{error}");
    lists(&web, "cloudflare", &[]);
    assert!(web.add_domain_provider("cloudflare", &cloudflare_token(), None, &input(false, false)).is_err());
    web.io.listings.lock().unwrap().insert("cloudflare".into(), Err("the provider rejected these credentials".into()));
    assert!(web.add_domain_provider("cloudflare", &cloudflare_token(), None, &input(false, false)).is_err());
    assert!(web.add_domain_provider("namecheap", &cloudflare_token(), None, &input(false, false)).is_err());
    assert!(web.add_domain_provider("cloudflare", &[], None, &input(false, false)).is_err());
    let mut locked = input(false, false);
    locked.secrets = None;
    assert!(matches!(web.add_domain_provider("cloudflare", &cloudflare_token(), None, &locked), Err(ApiError::Locked(_))));
    assert!(web.list_domain_providers(&input(false, false)).is_empty(), "nothing was stored");
}

#[test]
fn a_provider_in_use_cannot_be_removed() {
    let web = web(Fake::default());
    lists(&web, "cloudflare", &["example.com"]);
    lists(&web, "gandi", &["example.org"]);
    web.add_domain_provider("cloudflare", &cloudflare_token(), None, &input(false, true)).unwrap();
    web.add_domain_provider("gandi", &gandi_token(), None, &input(false, true)).unwrap();
    let error = web.remove_domain_provider("cloudflare", &input(false, true)).unwrap_err();
    assert!(error.to_string().contains("shop.example.com"), "{error}");
    web.remove_domain_provider("gandi", &input(false, true)).unwrap();
    assert!(matches!(web.remove_domain_provider("gandi", &input(false, true)), Err(ApiError::NotFound(_))));
    // Off the internet, it can go.
    web.remove_domain_provider("cloudflare", &input(false, false)).unwrap();
    assert!(web.list_domain_providers(&input(false, false)).is_empty());
}

#[test]
fn refreshing_keeps_a_domain_a_site_uses_and_records_problems() {
    let web = web(Fake::default());
    lists(&web, "cloudflare", &["example.com", "example.net"]);
    web.add_domain_provider("cloudflare", &cloudflare_token(), None, &input(false, true)).unwrap();
    lists(&web, "cloudflare", &["example.net", "example.io"]);
    let refreshed = web.refresh_domain_provider("cloudflare", &input(false, true)).unwrap();
    let names: Vec<_> = refreshed.domains.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["example.com", "example.io", "example.net"]);
    assert!(refreshed.problem.as_deref().is_some_and(|p| p.contains("example.com")));
    web.io.listings.lock().unwrap().insert("cloudflare".into(), Err("the provider is rate limiting requests".into()));
    let failed = web.refresh_domain_provider("cloudflare", &input(false, true)).unwrap();
    assert_eq!(failed.problem.as_deref(), Some("the provider is rate limiting requests"));
    assert_eq!(failed.domains.len(), 3, "a failed listing keeps the domains");
}

#[test]
fn the_older_request_still_sets_the_first_provider_without_touching_others() {
    let web = web(Fake::default());
    lists(&web, "gandi", &["example.org"]);
    web.add_domain_provider("gandi", &gandi_token(), None, &input(false, false)).unwrap();
    store_cloudflare(&web);
    let listed = web.list_domain_providers(&input(false, false));
    assert_eq!(listed.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["cloudflare", "gandi"]);
    let status = web.status(&input(false, true)).unwrap();
    assert_eq!(status.domain.as_deref(), Some("example.com"));
}

/// The shop at www.example.com with the bare domain redirecting to it and a name under the second
/// provider's domain serving it, as the owner's own sites are set up.
fn shop_with_aliases(public: bool) -> WebInput {
    let mut input = input(false, public);
    input.sites[0].public_host = Some("www.example.com".into());
    input.sites[0].public_aliases = vec![
        nodo_api::PublicAlias { host: "example.com".into(), mode: nodo_api::AliasMode::Redirect },
        nodo_api::PublicAlias { host: "pay.example.org".into(), mode: nodo_api::AliasMode::Serve },
    ];
    input
}

#[test]
fn every_alias_gets_its_record_and_its_providers_challenge() {
    let web = web(Fake::default());
    lists(&web, "cloudflare", &["example.com"]);
    lists(&web, "gandi", &["example.org"]);
    web.add_domain_provider("cloudflare", &cloudflare_token(), None, &shop_with_aliases(true)).unwrap();
    web.add_domain_provider("gandi", &gandi_token(), None, &shop_with_aliases(true)).unwrap();
    assert_eq!(web.owned_domains(), ["example.com", "example.org"]);
    web.apply(&shop_with_aliases(true)).unwrap();

    let ddns: toml::Value = toml::from_str(&web.io.get("/secrets/nodo-ddns/config.toml").unwrap()).unwrap();
    let zones = ddns["zones"].as_array().unwrap();
    let records = |domain: &str| -> Vec<String> {
        let zone = zones.iter().find(|z| z["domain"].as_str() == Some(domain)).unwrap();
        zone["records"].as_array().unwrap().iter().map(|r| r.as_str().unwrap().to_owned()).collect()
    };
    // The bare domain is the zone's own record.
    assert_eq!(records("example.com"), ["@", "www"]);
    assert_eq!(records("example.org"), ["pay"]);

    let json = caddy_json(&web);
    let policies = json["apps"]["tls"]["automation"]["policies"].as_array().unwrap().clone();
    let provider_of = |host: &str| {
        policies.iter().find(|p| p["subjects"].as_array().unwrap().iter().any(|s| s == host)).unwrap()["issuers"][0]
            ["challenges"]["dns"]["provider"]["name"]
            .clone()
    };
    assert_eq!(provider_of("www.example.com"), "cloudflare");
    assert_eq!(provider_of("example.com"), "cloudflare");
    assert_eq!(provider_of("pay.example.org"), "gandi");

    // The site's address in the status stays its primary.
    let status = web.status(&shop_with_aliases(true)).unwrap();
    assert_eq!(status.sites[0].public_url.as_deref(), Some("https://www.example.com"));
}

#[test]
fn a_provider_whose_domain_only_an_alias_uses_is_in_use() {
    let web = web(Fake::default());
    lists(&web, "cloudflare", &["example.com"]);
    lists(&web, "gandi", &["example.org"]);
    web.add_domain_provider("cloudflare", &cloudflare_token(), None, &shop_with_aliases(true)).unwrap();
    web.add_domain_provider("gandi", &gandi_token(), None, &shop_with_aliases(true)).unwrap();
    let listed = web.list_domain_providers(&shop_with_aliases(true));
    let org = listed[1].domains.iter().find(|d| d.name == "example.org").unwrap();
    assert_eq!(org.used_by.len(), 1, "the serving alias uses example.org");
    let error = web.remove_domain_provider("gandi", &shop_with_aliases(true)).unwrap_err();
    assert!(error.to_string().contains("pay.example.org"), "{error}");
    // A refresh that no longer lists example.org keeps it, since the alias still needs it.
    lists(&web, "gandi", &[]);
    let refreshed = web.refresh_domain_provider("gandi", &shop_with_aliases(true)).unwrap();
    assert_eq!(refreshed.domains.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["example.org"]);
    assert!(refreshed.problem.is_some());
    // Off the internet, it can go.
    web.remove_domain_provider("gandi", &shop_with_aliases(false)).unwrap();
}

fn web_gui_input(sites_local: bool) -> WebInput {
    let mut input = input(sites_local, false);
    input.web_gui = true;
    input
}

#[test]
fn the_web_gui_alone_brings_up_the_local_listener_the_trust_page_and_its_service() {
    let web = web(Fake::default());
    web.io.put(ROOT_PATH, ROOT_PEM);
    web.apply(&web_gui_input(false)).unwrap();

    let json = web.io.get("/web/caddy.json").unwrap();
    assert!(json.contains("192.168.1.60:10444") && json.contains("unix//run/nodo-web-gui/http.sock"), "{json}");
    assert!(json.contains("\"nodo.local\"") && json.contains("192.168.1.60:10081"));
    assert!(!json.contains("shop.local"), "no site is on the local network");
    let nft = web.io.get("/nft/web.nft").unwrap();
    assert!(nft.contains("dnat ip to 192.168.1.60:10444") && nft.contains("dnat ip to 192.168.1.60:10081"), "{nft}");
    assert!(!nft.contains("nodo-web-gui"));
    assert_eq!(web.io.get("/localnet/mdns-names").unwrap(), "nodo.local\n");
    assert_eq!(web.io.get("/web/local-address").unwrap(), "192.168.1.60\n");
    for service in ["nodo-localnet", "nodo-caddy", "nodo-mdns", "nodo-web-gui"] {
        assert!(web.io.up(service), "{service}");
    }
    // The service starts last, after the firewall and the route are in place.
    let calls = web.io.calls();
    let position = |call: &str| calls.iter().position(|c| c == call).unwrap_or_else(|| panic!("{call} in {calls:?}"));
    assert!(position("nft") < position("up nodo-web-gui") && position("caddy-load") < position("up nodo-web-gui"));
    assert_eq!(calls.last().map(String::as_str), Some("up nodo-web-gui"));

    // The trust page is there for the web GUI's root, and links to it.
    let page = web.io.get("/web/trust/index.html").unwrap();
    assert!(page.contains("href=\"https://nodo.local\""), "{page}");
    assert_eq!(web.web_gui_url(&web_gui_input(false)).as_deref(), Some("https://nodo.local"));
    let status = web.local_site_status(&web_gui_input(false)).unwrap();
    assert_eq!((status.address.as_deref(), status.trust_url.as_deref()), (Some("192.168.1.60"), Some("http://nodo.local")));
}

#[test]
fn turning_the_web_gui_off_stops_its_service_and_leaves_nothing_of_it() {
    // What a device that never had the web GUI on looks like, with one local site.
    let never = web(Fake::default());
    never.io.put(ROOT_PATH, ROOT_PEM);
    never.apply(&input(true, false)).unwrap();
    assert!(!never.io.up("nodo-web-gui"));
    assert!(!never.io.calls().contains(&"up nodo-web-gui".to_owned()));

    let web = web(Fake::default());
    web.io.put(ROOT_PATH, ROOT_PEM);
    web.apply(&web_gui_input(true)).unwrap();
    assert!(web.io.up("nodo-web-gui"));
    assert!(web.io.get("/web/caddy.json").unwrap().contains("unix//run/nodo-web-gui/http.sock"));
    assert!(web.io.get("/web/trust/index.html").unwrap().contains("https://nodo.local"));
    web.io.calls.lock().unwrap().clear();

    web.apply(&input(true, false)).unwrap();
    assert!(!web.io.up("nodo-web-gui"));
    // Stopped first, before anything that could fail.
    assert_eq!(web.io.calls().first().map(String::as_str), Some("down nodo-web-gui"));
    for path in ["/web/caddy.json", "/nft/web.nft", "/localnet/mdns-names", "/web/trust/index.html"] {
        assert_eq!(web.io.get(path), never.io.get(path), "{path} is as if the web GUI had never been on");
    }
    assert!(!web.io.get("/web/caddy.json").unwrap().contains("nodo-web-gui"));
    assert_eq!(web.web_gui_url(&input(true, false)), None);

    // With no local site either, the local listener, the trust page and the address all go.
    web.apply(&input(false, false)).unwrap();
    assert!(!web.io.get("/web/caddy.json").unwrap().contains("nodo.local"));
    assert!(web.io.get("/web/trust/index.html").is_none());
    for service in ["nodo-localnet", "nodo-caddy", "nodo-mdns", "nodo-web-gui"] {
        assert!(!web.io.up(service), "{service}");
    }
}

#[test]
fn the_web_gui_waits_for_the_local_address_with_its_service_running() {
    let web = web(Fake::default());
    web.io.put("/run/localnet/state", "status=unavailable\nreason=no-lease\n");
    let waiting = web_gui_input(false);
    web.apply(&waiting).unwrap();
    assert!(web.io.up("nodo-localnet"), "asked for the address");
    assert!(web.io.up("nodo-web-gui"), "ready for when the address comes");
    assert!(!web.io.get("/web/caddy.json").unwrap().contains("nodo-web-gui"), "served nowhere until then");
    assert_eq!(web.web_gui_url(&waiting), None);
    let status = web.status(&waiting).unwrap();
    assert!(status.local_unavailable.is_some() && status.local_remedy.is_some(), "{status:?}");

    // The address arrives: the periodic check applies it, and the web GUI is served.
    web.io.put("/run/localnet/state", "status=ready\naddress=192.168.1.60\n");
    assert_eq!(web.local_address_changed(&waiting), Ok(true));
    assert!(web.io.get("/web/caddy.json").unwrap().contains("unix//run/nodo-web-gui/http.sock"));
    assert_eq!(web.io.get("/localnet/mdns-names").unwrap(), "nodo.local\n");
    assert_eq!(web.web_gui_url(&waiting).as_deref(), Some("https://nodo.local"));
}

#[test]
fn a_device_without_the_web_gui_service_cannot_turn_it_on_and_changes_nothing() {
    let fake = Fake { web_gui_missing: true, ..Fake::default() };
    let web = web(fake);
    let err = web.apply(&web_gui_input(false)).unwrap_err();
    assert!(err.contains("not installed"), "{err}");
    assert!(web.io.get("/web/caddy.json").is_none(), "nothing was written");
    assert!(web.io.calls().is_empty(), "{:?}", web.io.calls());
    // Off, such a device applies as it always did.
    web.apply(&input(true, false)).unwrap();
    assert!(!web.io.calls().iter().any(|c| c.contains("nodo-web-gui")));
}

#[test]
fn the_helper_start_sync_only_touches_the_web_guis_service() {
    let web = web(Fake::default());
    web.sync_web_gui_service(&web_gui_input(true)).unwrap();
    assert_eq!(web.io.calls(), ["up nodo-web-gui"]);
    web.sync_web_gui_service(&input(true, false)).unwrap();
    assert_eq!(web.io.calls(), ["up nodo-web-gui", "down nodo-web-gui"]);
    assert!(web.io.get("/web/caddy.json").is_none());
}

/// Lets a test keep looking at the web side after the helper has taken it.
struct Shared(Arc<NodoWeb<Fake>>);

impl ExposureApplier for Shared {
    fn apply(&self, input: &WebInput) -> Result<(), String> {
        ExposureApplier::apply(self.0.as_ref(), input)
    }
    fn web_gui_url(&self, input: &WebInput) -> Option<String> {
        self.0.web_gui_url(input)
    }
    fn sync_web_gui_service(&self, input: &WebInput) -> Result<(), String> {
        self.0.sync_web_gui_service(input)
    }
}

#[test]
fn the_helpers_switch_drives_the_service_the_route_and_the_gate_together() {
    use crate::fake::{FakeSystem, TestPolicy, TEST_PATHS};
    use nodo_api::{Request, Response, WebGuiStatus};
    let web = Arc::new(web(Fake::default()));
    let h = crate::Helper::new(
        FakeSystem::default(),
        crate::AllowedUids::new([1000]),
        Box::new(TestPolicy),
        Box::new(Shared(web.clone())),
    )
    .with_paths(TEST_PATHS())
    .with_kdf(crate::keys::KdfParams { memory_kib: 64, iterations: 1, lanes: 1 })
    .with_radios(crate::radio_tests::no_radios());
    let pass = || Secret::new("correct horse battery");
    h.init_device(&pass()).unwrap();
    assert!(!web.io.up("nodo-web-gui"), "off on a new device");

    let on = h.handle(1000, Request::SetWebGui(true)).unwrap();
    let served = WebGuiStatus { enabled: true, url: Some("https://nodo.local".into()), waiting: false, open: false };
    assert_eq!(on, Response::WebGui(served.clone()));
    assert!(web.io.up("nodo-web-gui") && web.io.up("nodo-localnet") && web.io.up("nodo-caddy"));
    assert!(web.io.get("/web/caddy.json").unwrap().contains("unix//run/nodo-web-gui/http.sock"));
    assert!(matches!(h.handle_web(4243, Request::WebGuiLogin { passphrase: pass() }), Ok(Response::WebGui(s)) if s.open));
    assert!(h.handle_web(4243, Request::ListServices).is_ok());

    // Without the address it is on and waiting, and every socket hears that.
    web.io.put("/run/localnet/state", "status=unavailable\nreason=no-lease\n");
    let waiting = WebGuiStatus { url: None, waiting: true, ..served };
    assert_eq!(h.handle(1000, Request::WebGuiStatus), Ok(Response::WebGui(waiting.clone())));
    assert_eq!(h.handle_screen(4242, Request::WebGuiStatus), Ok(Response::WebGui(waiting)));
    web.io.put("/run/localnet/state", "status=ready\naddress=192.168.1.60\n");

    let off = h.handle(1000, Request::SetWebGui(false)).unwrap();
    assert_eq!(off, Response::WebGui(WebGuiStatus { enabled: false, url: None, waiting: false, open: false }));
    for service in ["nodo-web-gui", "nodo-localnet", "nodo-caddy", "nodo-mdns"] {
        assert!(!web.io.up(service), "{service}");
    }
    let json = web.io.get("/web/caddy.json").unwrap();
    assert!(!json.contains("nodo-web-gui") && !json.contains("nodo.local"), "{json}");
    assert!(!web.io.get("/nft/web.nft").unwrap().contains("dnat"));
    assert!(h.handle_web(4243, Request::ListServices).unwrap_err().is_web_login_required());
}

fn porkbun_keys() -> Vec<(String, Secret)> {
    vec![
        ("api_key".into(), Secret::new("pk1_3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789c2d1")),
        ("secret_key".into(), Secret::new("sk1_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")),
    ]
}

/// The owner's report: several domains at Porkbun, one usable, and no way to see which key the
/// Nodo holds or why the other domains were missing.
#[test]
fn a_connection_says_which_key_it_holds_and_why_a_domain_cannot_be_used() {
    let web = web(Fake::default());
    lists(&web, "porkbun", &["example.com"]);
    web.io.blocked.lock().unwrap().insert("porkbun".into(), vec!["example.net".into(), "Example.ORG".into()]);
    let added = web.add_domain_provider("porkbun", &porkbun_keys(), None, &input(false, false)).unwrap();
    assert_eq!(added.domains.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["example.com"]);
    assert_eq!(added.blocked.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["example.net", "example.org"]);
    assert!(added.blocked[0].reason.contains("API Access"), "{}", added.blocked[0].reason);
    let key = added.key.clone().expect("the key is identified");
    assert_eq!((key.field.as_str(), key.hint.as_str()), ("api_key", "pk1_3f9a…c2d1"));

    // The stored file holds the hint and nothing more of either key in the clear.
    let stored = String::from_utf8(web.io.read(&paths().dns_credentials).unwrap()).unwrap();
    assert!(stored.contains("pk1_3f9a…c2d1"), "{stored}");
    assert!(!stored.contains("0123456789abcdef"), "{stored}");

    // The owner switches API Access on for one more domain and looks again.
    lists(&web, "porkbun", &["example.com", "example.net"]);
    web.io.blocked.lock().unwrap().insert("porkbun".into(), vec!["example.org".into()]);
    let refreshed = web.refresh_domain_provider("porkbun", &input(false, false)).unwrap();
    assert_eq!(refreshed.domains.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["example.com", "example.net"]);
    assert_eq!(refreshed.blocked.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["example.org"]);
    assert_eq!(web.list_domain_providers(&input(false, false))[0], refreshed);
}

#[test]
fn a_key_that_may_change_no_domain_yet_names_the_domains_and_the_switch() {
    let web = web(Fake::default());
    lists(&web, "porkbun", &[]);
    web.io.blocked.lock().unwrap().insert("porkbun".into(), vec!["example.net".into()]);
    let error = web.add_domain_provider("porkbun", &porkbun_keys(), None, &input(false, false)).unwrap_err().to_string();
    assert!(error.contains("example.net") && error.contains("API Access"), "{error}");
}

#[test]
fn a_connection_made_before_hints_were_kept_gets_one_when_looked_up_again() {
    let web = web(Fake::default());
    lists(&web, "porkbun", &["example.com"]);
    web.add_domain_provider("porkbun", &porkbun_keys(), None, &input(false, false)).unwrap();
    let (mut store, _) = web.provider_store();
    store.providers[0].key_hint = None;
    web.save_store(&store).unwrap();
    assert_eq!(web.list_domain_providers(&input(false, false))[0].key, None);
    let refreshed = web.refresh_domain_provider("porkbun", &input(false, false)).unwrap();
    assert_eq!(refreshed.key.map(|k| k.hint).as_deref(), Some("pk1_3f9a…c2d1"));
}

#[test]
fn looking_again_applies_the_web_setup_only_when_the_domains_changed() {
    let web = web(Fake::default());
    lists(&web, "porkbun", &["example.com"]);
    web.add_domain_provider("porkbun", &porkbun_keys(), None, &input(false, false)).unwrap();
    let loads = |web: &NodoWeb<Fake>| web.io.calls.lock().unwrap().iter().filter(|c| *c == "caddy-load").count();
    let before = loads(&web);
    web.refresh_domain_provider("porkbun", &input(false, false)).unwrap();
    assert_eq!(loads(&web), before, "nothing changed, so nothing is applied");
    lists(&web, "porkbun", &["example.com", "example.net"]);
    web.refresh_domain_provider("porkbun", &input(false, false)).unwrap();
    assert!(loads(&web) > before, "a new domain is applied");
}

#[test]
fn key_hints_show_little_of_a_long_key_and_nothing_of_a_short_one() {
    use super::providers::key_hint;
    assert_eq!(key_hint("pk1_3f9a0123456789abcdef0123456789abcdefc2d1", true), "pk1_3f9a…c2d1");
    // A token that is the whole secret never shows how it starts.
    assert_eq!(key_hint("cfut_0123456789abcdefghijABCDEFGHIJ0123456789", false), "…6789");
    assert_eq!(key_hint("  pat_0123456789abcdef ", false), "…cdef");
    assert_eq!(key_hint("shortidentifier12", true), "…er12");
    assert_eq!(key_hint("short", true), "…");
    assert_eq!(key_hint("", false), "…");
}

#[test]
fn a_single_token_provider_is_identified_by_the_end_of_its_token_only() {
    let web = web(Fake::default());
    lists(&web, "cloudflare", &["example.com"]);
    let added = web.add_domain_provider("cloudflare", &cloudflare_token(), None, &input(false, false)).unwrap();
    let key = added.key.unwrap();
    assert_eq!((key.field.as_str(), key.hint.as_str()), ("api_token", "…6789"));
}

/// The node's RPC site, as the helper reads it from the manifest the base image ships, with the
/// owner's saved choices: every exposure on, a host, and another name redirecting to it.
fn shipped_node_site(public_port_file: bool) -> WebInput {
    use crate::fake::{helper, FakeSystem};
    let sys = FakeSystem::default();
    sys.add_system_manifest(include_str!("../../../../packages/nodo-tools/files/cuprated/manifest.toml"));
    if public_port_file {
        sys.put("/s/public/cuprated/restricted-rpc-port", b"18081");
    }
    sys.put("/s/exposure.toml", b"[cuprated.rpc]\nlocal = true\nonion = true\npublic = true\n");
    sys.put(
        "/s/public-hosts.toml",
        b"[cuprated.rpc]\nhost = \"node.example.com\"\naliases = [{ host = \"monero.example.com\", mode = \"redirect\" }]\n",
    );
    let mut input = helper(sys).current_web_input().unwrap();
    input.secrets = Some(key());
    input
}

/// Every upstream the Caddy config dials.
fn dials(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                match (k.as_str(), v.as_str()) {
                    ("dial", Some(dial)) => out.push(dial.to_owned()),
                    _ => dials(v, out),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| dials(v, out)),
        _ => {}
    }
}

/// The route of the server that answers `host`.
fn route_for<'a>(json: &'a serde_json::Value, host: &str) -> &'a serde_json::Value {
    json["apps"]["http"]["servers"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|server| server["routes"].as_array().unwrap())
        .find(|route| route["match"].as_array().is_some_and(|m| m.iter().any(|m| m["host"].as_array().is_some_and(|h| h.iter().any(|h| h == host)))))
        .unwrap_or_else(|| panic!("no route for {host}: {json:#}"))
}

#[test]
fn the_shipped_node_rpc_site_proxies_only_to_the_restricted_rpc() {
    let input = shipped_node_site(true);
    assert_eq!(input.sites.len(), 1);
    let node = &input.sites[0];
    assert_eq!((node.package.as_str(), node.service.name.as_str()), ("cuprated", "rpc"));
    assert_eq!(node.exposure, Exposure { local: true, onion: true, public: true });

    let web = tor_web();
    web.io.put("/tor/cuprated.rpc/hostname", &format!("{NODE_ONION}\n"));
    store_cloudflare(&web);
    web.apply(&input).unwrap();
    let json = caddy_json(&web);

    // Whatever address it is reached at, it is the restricted RPC on loopback and nothing else.
    let mut all = Vec::new();
    dials(&json, &mut all);
    all.sort();
    all.dedup();
    assert_eq!(all, ["127.0.0.1:18081"], "{json:#}");

    for host in ["node.example.com", "node.local", NODE_ONION] {
        let route = route_for(&json, host).to_string();
        assert!(route.contains("\"reverse_proxy\"") && route.contains("127.0.0.1:18081"), "{host}: {route}");
        // HTTP/1.1, like any app's site: Monero RPC clients POST to /json_rpc and the other
        // endpoints, and every path and method goes through unchanged, with no path matcher.
        assert!(!route.contains("transport") && !route.contains("\"path\""), "{host}: {route}");
    }
    // Rate limiting is on for public sites, the same as for every app's site.
    let public = route_for(&json, "node.example.com").to_string();
    assert!(public.contains("rate_limit") && public.contains("cuprated.rpc"), "{public}");
    let alias = route_for(&json, "monero.example.com").to_string();
    assert!(alias.contains("node.example.com") && !alias.contains("reverse_proxy"), "a redirect: {alias}");

    // The firewall opens Caddy's own ports, never the RPC port itself.
    let nft = web.io.get("/nft/web.nft").unwrap();
    assert!(!nft.contains("18081"), "{nft}");
    let ddns = web.io.get("/secrets/nodo-ddns/config.toml").unwrap();
    assert!(ddns.contains("\"node\"") && ddns.contains("\"monero\""), "{ddns}");
    assert!(web.io.get("/localnet/mdns-names").unwrap().contains("node.local\n"));
    let torrc = web.io.get("/web/torrc").unwrap();
    assert!(torrc.contains("HiddenServiceDir /tor/cuprated.rpc"), "{torrc}");
}

#[test]
fn a_node_that_does_not_export_its_restricted_rpc_port_serves_no_site() {
    let input = shipped_node_site(false);
    assert_eq!(input.sites[0].exposure, Exposure::default(), "saved choices are not served");
    let web = web(Fake::default());
    web.apply(&input).unwrap();
    let json = web.io.get("/web/caddy.json").unwrap_or_default();
    assert!(!json.contains("18081"), "{json}");
}
