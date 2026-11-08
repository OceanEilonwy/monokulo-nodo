use super::*;
use crate::exposure::WebSite;
use nodo_api::Exposure;
use nodo_manifest::WebService;
use std::sync::Mutex;

type StoredOwner = (u32, String, String);

#[derive(Default)]
struct Fake {
    files: Mutex<BTreeMap<PathBuf, (Vec<u8>, StoredOwner)>>,
    services: Mutex<BTreeMap<String, bool>>,
    calls: Mutex<Vec<String>>,
    caddy_running: bool,
    caddy_refuses: bool,
    nft_refuses: bool,
    retire_output: String,
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
}

fn paths() -> WebPaths {
    WebPaths {
        caddy_config: "/web/caddy.json".into(),
        caddy_admin: "/run/admin.sock".into(),
        nft_fragment: "/nft/web.nft".into(),
        nft_main: "/nft/main.conf".into(),
        ddns_config: "/ddns/config.toml".into(),
        ddns_state: "/ddns/state.json".into(),
        dns_credentials: "/nodo/web-dns.toml".into(),
        mdns_names: "/localnet/mdns-names".into(),
        localnet_state: "/run/localnet/state".into(),
        torrc: "/web/torrc".into(),
        tor_sv_conf: "/sv/tor/conf".into(),
        tor_dir: "/tor".into(),
    }
}

fn web(fake: Fake) -> NodoWeb<Fake> {
    let web = NodoWeb::new(paths(), fake);
    web.io.put("/run/localnet/state", "status=ready\naddress=192.168.1.60\n");
    web
}

fn input(local: bool, public: bool) -> WebInput {
    WebInput {
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
    assert_eq!(web.io.get("/localnet/mdns-names").unwrap(), "shop.local\n");
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
    let config = web.io.get("/ddns/config.toml").unwrap();
    assert!(config.contains("records = [\"shop\"]") && config.contains("provider = \"cloudflare\""));
    assert_eq!(web.io.owner("/ddns/config.toml"), (0o600, "_nododdns".into(), "_nododdns".into()));

    web.apply(&input(true, false)).unwrap();
    assert!(!web.io.up("nodo-ddns"));
}

#[test]
fn credentials_are_root_only_and_validated() {
    let web = web(Fake::default());
    store_cloudflare(&web);
    assert_eq!(web.io.owner("/nodo/web-dns.toml"), (0o600, "root".into(), "root".into()));
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
    assert_eq!(web.retire_stale().unwrap(), Vec::<String>::new(), "no config, nothing to retire");
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
