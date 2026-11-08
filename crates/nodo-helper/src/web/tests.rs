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
    /// What this device's resolver answers, by name. Missing names fail to resolve.
    resolved: Mutex<BTreeMap<String, Vec<IpAddr>>>,
    removed_dirs: Mutex<Vec<String>>,
    now: u64,
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
        name != "tor"
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
                admin_paths: vec!["/admin".into()],
                multi_address: true,
            },
            exposure: Exposure { local, onion: false, public },
            public_host: Some("shop.example.com".into()),
            locked: false,
        }],
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
fn status_reports_stale_records_and_the_last_change() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    web.io.put(
        "/ddns/state.json",
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
    assert!(!stored.contains("gdkey") && !stored.contains("gdsecret") && stored.contains("[dns01]"), "{stored}");
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
    web.io.put("/ddns/state.json", r#"{"published":{},"observed_v4":"203.0.113.2"}"#);
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
