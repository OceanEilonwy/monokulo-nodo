use super::*;

const ONION: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion";
const ONION_B: &str = "bbcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion";
const LAN_CLIENT: &str = "192.168.1.20";
const INTERNET_CLIENT: &str = "198.51.100.7";

fn service(name: &str, upstream: &str) -> WebService {
    WebService {
        name: name.to_owned(),
        upstream: upstream.to_owned(),
        suggested_subdomain: None,
        multi_address: true,
        upstream_protocol: UpstreamProtocol::H1,
    }
}

fn site(package: &str, name: &str, port: u16, exposure: Exposure) -> Site {
    Site {
        package: package.to_owned(),
        service: service(name, &format!("127.0.0.1:{port}")),
        exposure,
        state: ServiceState::Running,
        local_host: None,
        public_subdomain: None,
        public_host: None,
        public_aliases: Vec::new(),
        onion_host: None,
        rate_limit: false,
    }
}

fn all_exposures() -> Exposure {
    Exposure { local: true, onion: true, public: true }
}

const LOCAL_IP: std::net::Ipv4Addr = std::net::Ipv4Addr::new(192, 168, 1, 60);

fn settings() -> DomainSettings {
    DomainSettings {
        domain: Some("example.com".to_owned()),
        local_address: LocalAddress::Ready { address: LOCAL_IP },
        ..DomainSettings::default()
    }
}

/// What a request ends in. A small model of Caddy's routing, so isolation is asserted as behaviour.
#[derive(Debug, PartialEq)]
enum Outcome {
    Proxy(String),
    /// A file from this directory.
    File(String),
    Status(String),
    Dropped,
    NoRoute,
}

fn matches(matcher: &Matcher, host: &str, path: &str, client: &str) -> bool {
    let host_ok = matcher.host.is_empty() || matcher.host.iter().any(|h| h == host);
    let path_ok = matcher.path.is_empty()
        || matcher.path.iter().any(|pattern| match pattern.strip_suffix('*') {
            Some(prefix) => path.to_lowercase().starts_with(&prefix.to_lowercase()),
            None => path.eq_ignore_ascii_case(pattern),
        });
    let ip_ok = matcher.remote_ip.as_ref().is_none_or(|remote| {
        // These tests only use the two client addresses above.
        client == LAN_CLIENT && remote.ranges.iter().any(|r| r == "192.168.0.0/16")
    });
    host_ok && path_ok && ip_ok
}

fn walk(routes: &[Route], host: &str, path: &str, client: &str) -> Outcome {
    for route in routes {
        if !route.matchers.is_empty() && !route.matchers.iter().any(|m| matches(m, host, path, client)) {
            continue;
        }
        for handle in &route.handle {
            match handle {
                Handler::RateLimit { .. } | Handler::Headers { .. } => {}
                Handler::FileServer { root, .. } => return Outcome::File(root.clone()),
                Handler::Subroute { routes } => return walk(routes, host, path, client),
                Handler::ReverseProxy { upstreams, .. } => return Outcome::Proxy(upstreams[0].dial.clone()),
                Handler::StaticResponse { abort: true, .. } => return Outcome::Dropped,
                Handler::StaticResponse { status_code, .. } => return Outcome::Status(status_code.clone().unwrap()),
            }
        }
    }
    Outcome::NoRoute
}

fn request(plan: &WebPlan, server: &str, host: &str, path: &str, client: &str) -> Outcome {
    walk(&plan.caddy.apps.http.servers[server].routes, host, path, client)
}

fn onion_server_for<'a>(plan: &'a WebPlan, host: &str) -> &'a str {
    let target = &plan.addresses.iter().find(|a| a.host == host).unwrap().listen;
    plan.caddy.apps.http.servers.iter().find(|(_, s)| s.listen.contains(target)).unwrap().0
}

fn json(plan: &WebPlan) -> serde_json::Value {
    serde_json::to_value(&plan.caddy).unwrap()
}

#[test]
fn an_h2c_upstream_gets_http2_prior_knowledge_and_immediate_flushing() {
    let public = Exposure { public: true, ..Exposure::default() };
    let mut relay = site("relay", "relay", 9473, public);
    relay.service.upstream_protocol = UpstreamProtocol::H2c;
    let shop = site("shop", "shop", 8080, public);
    let value = json(&generate(&[relay.clone(), shop], &settings()).unwrap());
    let routes = value["apps"]["http"]["servers"]["public"]["routes"].as_array().unwrap();
    let proxy_for = |host: &str| {
        let route = routes.iter().find(|r| r["match"][0]["host"][0] == host).unwrap();
        let inner = route["handle"][0]["routes"].as_array().unwrap();
        inner.last().unwrap()["handle"][0].clone()
    };

    let h2c = proxy_for("relay.example.com");
    assert_eq!(h2c["handler"], "reverse_proxy");
    assert_eq!(h2c["upstreams"][0]["dial"], "127.0.0.1:9473");
    assert_eq!(h2c["transport"], serde_json::json!({"protocol": "http", "versions": ["h2c"]}));
    assert_eq!(h2c["flush_interval"], -1);

    // An HTTP/1.1 site is exactly as before: no transport, Caddy's default buffering.
    let h1 = proxy_for("shop.example.com");
    assert_eq!(h1["handler"], "reverse_proxy");
    assert!(h1.get("transport").is_none() && h1.get("flush_interval").is_none(), "{h1}");

    // A stopped h2c site answers like any other stopped site.
    relay.state = ServiceState::Stopped;
    let plan = generate(&[relay], &settings()).unwrap();
    assert!(!serde_json::to_string(&plan.caddy).unwrap().contains("h2c"));
}

#[test]
fn caddy_json_has_the_documented_shape() {
    let mut shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    shop.state = ServiceState::Running;
    let value = json(&generate(&[shop], &settings()).unwrap());
    let server = &value["apps"]["http"]["servers"]["public"];

    assert_eq!(server["listen"], serde_json::json!([":10443"]));
    assert_eq!(value["apps"]["http"]["http_port"], 10080);
    assert_eq!(value["apps"]["http"]["https_port"], 10443);
    assert_eq!(server["tls_connection_policies"][0]["match"]["sni"], serde_json::json!(["shop.example.com"]));
    let route = &server["routes"][0];
    assert_eq!(route["match"][0]["host"][0], "shop.example.com");
    assert_eq!(route["terminal"], true);
    assert_eq!(route["handle"][0]["handler"], "subroute");
    let inner = route["handle"][0]["routes"].as_array().unwrap();
    assert_eq!(inner.len(), 1, "the whole site goes to the upstream: {inner:?}");
    assert!(inner[0].get("match").is_none());
    assert_eq!(inner[0]["handle"][0]["handler"], "reverse_proxy");
    assert_eq!(inner[0]["handle"][0]["upstreams"][0]["dial"], "127.0.0.1:8080");
    assert_eq!(server["routes"][1]["handle"][0]["abort"], true);
    assert_eq!(value["apps"]["tls"]["automation"]["policies"][0]["issuers"][0]["module"], "acme");
    assert_eq!(value["admin"]["listen"], "unix//run/nodo-web/caddy-admin.sock");
    assert_eq!(value["admin"]["config"]["persist"], false);
}

#[test]
fn generation_is_deterministic_and_independent_of_input_order() {
    let a = site("a", "a", 8001, all_exposures());
    let b = site("b", "b", 8002, all_exposures());
    let forward = generate(&[a.clone(), b.clone()], &settings()).unwrap();
    let reverse = generate(&[b, a], &settings()).unwrap();
    assert_eq!(forward, reverse);
    assert_eq!(json(&forward).to_string(), json(&reverse).to_string());
}

#[test]
fn one_site_can_be_local_onion_and_public_at_once() {
    let mut shop = site("shop", "shop", 8080, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    let plan = generate(&[shop], &settings()).unwrap();

    let kinds: Vec<_> = plan.addresses.iter().map(|a| (a.kind, a.url.as_str())).collect();
    assert_eq!(
        kinds,
        vec![
            (AddressKind::Public, "https://shop.example.com"),
            (AddressKind::Local, "https://shop.local"),
            (AddressKind::Onion, &*format!("http://{ONION}")),
        ]
    );
    assert_eq!(request(&plan, "public", "shop.example.com", "/", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
    assert_eq!(request(&plan, "local", "shop.local", "/", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
    let onion = onion_server_for(&plan, ONION);
    assert_eq!(request(&plan, onion, ONION, "/", "127.0.0.1"), Outcome::Proxy("127.0.0.1:8080".into()));
}

#[test]
fn every_path_is_served_at_every_address() {
    // Sites are served whole wherever the owner puts them; an app guards its own admin pages, as
    // mk-pay does behind its sign-in. Paths that once could be hidden are no different.
    let mut shop = site("shop", "shop", 8080, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    let plan = generate(&[shop], &settings()).unwrap();
    let onion = onion_server_for(&plan, ONION);
    let served = Outcome::Proxy("127.0.0.1:8080".into());
    for path in ["/", "/admin", "/ADMIN", "/admin/users", "/admin;x=1", "/api/v1/admin/x", "/status/engine"] {
        assert_eq!(request(&plan, "public", "shop.example.com", path, INTERNET_CLIENT), served, "{path}");
        assert_eq!(request(&plan, onion, ONION, path, "127.0.0.1"), served, "{path}");
        assert_eq!(request(&plan, "local", "shop.local", path, LAN_CLIENT), served, "{path}");
    }
}

#[test]
fn a_stopped_site_answers_503_on_every_path() {
    let mut shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    shop.state = ServiceState::Locked;
    let plan = generate(&[shop], &settings()).unwrap();
    for path in ["/", "/admin"] {
        assert_eq!(request(&plan, "public", "shop.example.com", path, INTERNET_CLIENT), Outcome::Status("503".into()));
    }
}

#[test]
fn admin_paths_in_an_old_manifest_change_nothing_in_the_proxy_config() {
    // Manifests from before admin paths were removed still parse (nodo-manifest drops the key).
    // Publishing a site from one gives exactly the config of the same manifest without it.
    let text = "api_version = 1\nid = \"shop\"\nversion = \"1.0\"\nexec = [\"bin/shop\"]\n\
                [[web]]\nname = \"web\"\nupstream = \"127.0.0.1:8080\"\nmulti_address = true\n";
    let plan = |text: &str| {
        let manifest = nodo_manifest::Manifest::parse(text).unwrap();
        let mut published = site("shop", "web", 8080, all_exposures());
        published.service = manifest.web[0].clone();
        published.onion_host = Some(ONION.to_owned());
        generate(&[published], &settings()).unwrap()
    };
    let plain = plan(text);
    for paths in ["[\"/admin\", \"/admin/*\"]", "[\"/\"]", "[\"/*\"]"] {
        let old = plan(&format!("{text}admin_paths = {paths}\n"));
        assert_eq!(old, plain, "{paths}");
        assert_eq!(request(&old, "public", "web.example.com", "/admin", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
    }
}

#[test]
fn every_packaged_site_can_be_published() {
    // The feed check validates manifests with nodo-manifest; this proves the proxy accepts what that
    // allows, for each site of each package in the repository, made public with an onion address.
    let packages = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages");
    let mut checked = 0;
    for entry in std::fs::read_dir(&packages).unwrap() {
        let file = entry.unwrap().path().join("files/manifest.toml");
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let manifest = nodo_manifest::Manifest::parse(&text.replace("@VERSION@", "1.0"))
            .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for (index, web) in manifest.web.iter().enumerate() {
            let mut published = site(&manifest.id, &web.name, 9000 + index as u16, all_exposures());
            published.service = web.clone();
            published.onion_host = Some(ONION.to_owned());
            if !web.multi_address {
                published.exposure = Exposure { public: true, ..Exposure::default() };
                published.onion_host = None;
            }
            generate(&[published], &settings())
                .unwrap_or_else(|e| panic!("{} site {} cannot be published: {e}", manifest.id, web.name));
            checked += 1;
        }
    }
    assert!(checked >= 5, "found only {checked} packaged sites under {}", packages.display());
}

#[test]
fn a_host_only_ever_reaches_its_own_upstream() {
    let mut a = site("a", "a", 8001, all_exposures());
    a.onion_host = Some(ONION.to_owned());
    let mut b = site("b", "b", 8002, all_exposures());
    b.onion_host = Some(ONION_B.to_owned());
    let plan = generate(&[a, b], &settings()).unwrap();

    assert_eq!(request(&plan, "public", "a.example.com", "/", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8001".into()));
    assert_eq!(request(&plan, "public", "b.example.com", "/", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8002".into()));
    assert_eq!(request(&plan, "local", "a.local", "/", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8001".into()));
    assert_eq!(request(&plan, "local", "b.local", "/", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8002".into()));
    assert_eq!(request(&plan, onion_server_for(&plan, ONION), ONION, "/", "127.0.0.1"), Outcome::Proxy("127.0.0.1:8001".into()));
    assert_eq!(
        request(&plan, onion_server_for(&plan, ONION_B), ONION_B, "/", "127.0.0.1"),
        Outcome::Proxy("127.0.0.1:8002".into())
    );
}

#[test]
fn internet_clients_cannot_reach_local_or_onion_hosts_through_the_public_listener() {
    let mut shop = site("shop", "shop", 8080, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    let plan = generate(&[shop], &settings()).unwrap();

    // A masquerading router makes internet clients look local, so the LAN address must not help either.
    for client in [INTERNET_CLIENT, LAN_CLIENT] {
        assert_eq!(request(&plan, "public", "shop.local", "/", client), Outcome::Dropped);
        assert_eq!(request(&plan, "public", "shop.local", "/other", client), Outcome::Dropped);
        assert_eq!(request(&plan, "public", ONION, "/", client), Outcome::Dropped);
        assert_eq!(request(&plan, "public", "unknown.example.com", "/", client), Outcome::Dropped);
        assert_eq!(request(&plan, "public", "shop.example.com", "/", client), Outcome::Proxy("127.0.0.1:8080".into()));
    }
    let public = &plan.caddy.apps.http.servers["public"];
    assert_eq!(public.tls_connection_policies[0].matcher.sni, vec!["shop.example.com"]);
}

#[test]
fn the_local_listener_serves_only_local_hosts_to_lan_clients() {
    let mut shop = site("shop", "shop", 8080, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    let plan = generate(&[shop], &settings()).unwrap();

    assert_eq!(request(&plan, "local", "shop.local", "/", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
    assert_eq!(request(&plan, "local", "shop.local", "/", INTERNET_CLIENT), Outcome::Dropped);
    assert_eq!(request(&plan, "local", "shop.example.com", "/", LAN_CLIENT), Outcome::Dropped);
    assert_eq!(request(&plan, "local", ONION, "/", LAN_CLIENT), Outcome::Dropped);
    let local = &plan.caddy.apps.http.servers["local"];
    assert_eq!(local.listen, vec!["192.168.1.60:10444"], "bound to the local address only");
    assert_eq!(local.tls_connection_policies[0].matcher.sni, vec!["shop.local"]);
}

#[test]
fn onion_listeners_cannot_serve_other_hosts() {
    let mut shop = site("shop", "shop", 8080, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    let plan = generate(&[shop], &settings()).unwrap();
    let onion = onion_server_for(&plan, ONION);

    assert_eq!(request(&plan, onion, "shop.example.com", "/", "127.0.0.1"), Outcome::Dropped);
    assert_eq!(request(&plan, onion, "shop.local", "/", "127.0.0.1"), Outcome::Dropped);
}

#[test]
fn onion_servers_are_plain_http_on_loopback_with_their_own_listener() {
    let mut a = site("a", "a", 8001, Exposure { onion: true, ..Exposure::default() });
    a.onion_host = Some(ONION.to_owned());
    let mut b = site("b", "b", 8002, Exposure { onion: true, ..Exposure::default() });
    b.onion_host = Some(ONION_B.to_owned());
    let plan = generate(&[a, b], &settings()).unwrap();

    let servers = &plan.caddy.apps.http.servers;
    assert!(!servers.contains_key("public") && !servers.contains_key("local"));
    assert_eq!(servers["onion-0"].listen, vec!["127.0.0.1:19000"]);
    assert_eq!(servers["onion-1"].listen, vec!["127.0.0.1:19001"]);
    for server in servers.values() {
        assert!(server.automatic_https.disable);
    }
    assert!(plan.caddy.apps.tls.is_none());
}

#[test]
fn tls_issuers_match_each_exposure() {
    let plan = generate(
        &[site("shop", "shop", 8080, Exposure { local: true, public: true, ..Exposure::default() })],
        &DomainSettings { acme_email: Some("me@example.com".to_owned()), ..settings() },
    )
    .unwrap();
    let policies = &plan.caddy.apps.tls.as_ref().unwrap().automation.policies;
    assert_eq!(policies[0].subjects, vec!["shop.local"]);
    assert_eq!(policies[0].issuers, vec![Issuer::Internal]);
    assert_eq!(policies[1].subjects, vec!["shop.example.com"]);
    assert_eq!(policies[1].issuers, vec![Issuer::Acme { ca: LETS_ENCRYPT.to_owned(), email: Some("me@example.com".to_owned()), challenges: None }]);
    assert!(!plan.caddy.apps.pki.as_ref().unwrap().certificate_authorities["local"].install_trust);
    for name in ["public", "local"] {
        assert!(!plan.caddy.apps.http.servers[name].automatic_https.disable);
        assert!(!plan.caddy.apps.http.servers[name].protocols.iter().any(|p| p == "h3"));
    }
}

#[test]
fn ports_80_and_443_open_only_with_a_public_site() {
    let none = generate(&[], &settings()).unwrap();
    assert_eq!(none.firewall, FirewallPlan::default());
    assert!(none.caddy.apps.http.servers.is_empty());

    let local = generate(&[site("a", "a", 8001, Exposure { local: true, ..Exposure::default() })], &settings()).unwrap();
    assert!(local.firewall.public_tcp.is_empty() && local.firewall.redirects.is_empty());
    let fw = local.firewall.local.as_ref().unwrap();
    assert_eq!((fw.address, fw.caddy_port), (LOCAL_IP, 10444));
    assert!(fw.lan_ranges.iter().all(|r| !r.contains(':')));
    assert_eq!(local.local.mdns_names, vec!["a.local", "nodo.local"]);
    assert!(local.caddy.apps.http.servers["local"].automatic_https.disable_redirects);

    let onion = generate(&[site("a", "a", 8001, Exposure { onion: true, ..Exposure::default() })], &settings()).unwrap();
    assert_eq!(onion.firewall, FirewallPlan::default());

    let public = generate(
        &[
            site("a", "a", 8001, Exposure { local: true, ..Exposure::default() }),
            site("b", "b", 8002, Exposure { public: true, ..Exposure::default() }),
        ],
        &settings(),
    )
    .unwrap();
    assert_eq!(public.firewall.public_tcp, vec![80, 443]);
    assert_eq!(public.firewall.redirects, vec![(80, 10080), (443, 10443)]);
    assert!(public.firewall.local.is_some());
    assert!(!public.caddy.apps.http.servers["public"].automatic_https.disable_redirects);
    assert!(public.caddy.apps.http.servers["local"].automatic_https.disable_redirects);
}

#[test]
fn nft_rules_keep_the_local_address_apart_from_the_redirects() {
    let plan = generate(
        &[site("a", "a", 8001, Exposure { local: true, public: true, ..Exposure::default() })],
        &settings(),
    )
    .unwrap();
    let rules = render_nft(&plan.firewall);
    let lan = "{ 127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16 }";
    let local_dnat = format!("ip daddr 192.168.1.60 ip saddr {lan} tcp dport 443 dnat ip to 192.168.1.60:10444");
    let leave = "ip daddr 192.168.1.60 return";
    let redirect = "tcp dport 443 redirect to :10443";
    let position = |needle: &str| rules.find(needle).unwrap_or_else(|| panic!("missing: {needle}\n{rules}"));
    assert!(position(&local_dnat) < position(leave) && position(leave) < position(redirect),
        "the local address must leave the chain before the public redirects");
    assert!(rules.contains("tcp dport 80 redirect to :10080"));
    assert!(rules.contains("tcp dport 10443 ct status dnat accept"));
    assert!(rules.contains(&format!("ip daddr 192.168.1.60 ip saddr {lan} tcp dport 10444 ct status dnat accept")));
    assert!(rules.contains(&format!("ip daddr 224.0.0.251 ip saddr {lan} udp dport 5353 accept")));
    assert!(!rules.contains("dport 443 accept"), "the public port itself is never opened");

    let local_only = render_nft(
        &generate(&[site("a", "a", 8001, Exposure { local: true, ..Exposure::default() })], &settings()).unwrap().firewall,
    );
    assert!(!local_only.contains("redirect to"));
    assert!(local_only.contains("dnat ip to 192.168.1.60:10444"));
    assert_eq!(render_nft(&FirewallPlan::default()).matches("accept").count(), 0);
}

#[test]
fn the_trust_page_is_plain_http_on_the_local_address_for_lan_clients_only() {
    let mut shop = site("shop", "shop", 8080, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    let plan = generate(&[shop], &settings()).unwrap();
    let trust = &plan.caddy.apps.http.servers["local-trust"];
    assert_eq!(trust.listen, vec!["192.168.1.60:10081"], "bound to the local address only");
    assert!(trust.automatic_https.disable && trust.tls_connection_policies.is_empty());
    assert_eq!(plan.local.trust_url.as_deref(), Some("http://nodo.local"));
    assert!(plan.local.mdns_names.contains(&"nodo.local".to_owned()));

    let dir = "/var/lib/nodo-web/trust".to_owned();
    for host in ["nodo.local", "192.168.1.60"] {
        for path in ["/", "/nodo-local-ca.crt", "/nodo-local-ca.pem"] {
            assert_eq!(request(&plan, "local-trust", host, path, LAN_CLIENT), Outcome::File(dir.clone()), "{host}{path}");
            assert_eq!(request(&plan, "local-trust", host, path, INTERNET_CLIENT), Outcome::Dropped, "{host}{path}");
        }
        assert_eq!(request(&plan, "local-trust", host, "/index.html/../x", LAN_CLIENT), Outcome::Status("404".into()));
    }
    assert_eq!(request(&plan, "local-trust", "shop.local", "/", LAN_CLIENT), Outcome::Dropped);
    assert_eq!(request(&plan, "local-trust", "shop.example.com", "/", LAN_CLIENT), Outcome::Dropped);
    // No other listener knows the page.
    for client in [LAN_CLIENT, INTERNET_CLIENT] {
        assert_eq!(request(&plan, "public", "nodo.local", "/", client), Outcome::Dropped);
        assert_eq!(request(&plan, "local", "nodo.local", "/", client), Outcome::Dropped);
        assert_eq!(request(&plan, onion_server_for(&plan, ONION), "nodo.local", "/", client), Outcome::Dropped);
    }

    let value = json(&plan);
    let routes = &value["apps"]["http"]["servers"]["local-trust"]["routes"][0]["handle"][0]["routes"];
    assert_eq!(routes[1]["match"][0]["path"][0], "/nodo-local-ca.crt");
    assert_eq!(routes[1]["handle"][0]["handler"], "headers");
    assert_eq!(routes[1]["handle"][0]["response"]["set"]["Content-Type"][0], "application/x-x509-ca-cert");
    assert_eq!(routes[2]["handle"][0]["response"]["set"]["Content-Type"][0], "application/x-pem-file");
    assert_eq!(routes[0]["handle"][1]["handler"], "file_server");
    assert_eq!(routes[0]["handle"][1]["index_names"][0], "index.html");

    let rules = render_nft(&plan.firewall);
    let lan = "{ 127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16 }";
    let dnat = format!("ip daddr 192.168.1.60 ip saddr {lan} tcp dport 80 dnat ip to 192.168.1.60:10081");
    let position = |needle: &str| rules.find(needle).unwrap_or_else(|| panic!("missing: {needle}\n{rules}"));
    assert!(position(&dnat) < position("ip daddr 192.168.1.60 return"));
    assert!(position("ip daddr 192.168.1.60 return") < position("tcp dport 80 redirect to :10080"));
    assert!(rules.contains(&format!("ip daddr 192.168.1.60 ip saddr {lan} tcp dport 10081 ct status dnat accept")));
}

#[test]
fn the_trust_page_exists_only_while_a_dot_local_site_uses_the_internal_ca() {
    let local = Exposure { local: true, ..Exposure::default() };
    let under_domain = DomainSettings {
        local_domain: Some("home.example.com".to_owned()),
        dns_challenge: Some(DnsChallenge::Cloudflare { api_token: Secret::new("cfut_0123456789abcdefghijABCDEFGHIJ0123456789") }),
        ..settings()
    };
    let no_address = DomainSettings {
        local_address: LocalAddress::Unavailable { reason: LocalUnavailable::NoLease },
        ..settings()
    };
    let public = Exposure { public: true, ..Exposure::default() };
    for (sites, settings) in [
        (vec![site("a", "a", 8001, local)], under_domain),
        (vec![site("a", "a", 8001, local)], no_address),
        (vec![site("a", "a", 8001, public)], settings()),
    ] {
        let plan = generate(&sites, &settings).unwrap();
        assert!(!plan.caddy.apps.http.servers.contains_key("local-trust"));
        assert!(plan.local.trust_url.is_none());
        assert!(!plan.local.mdns_names.contains(&"nodo.local".to_owned()));
        assert!(plan.firewall.local.as_ref().is_none_or(|l| l.trust_port.is_none()));
        assert!(!render_nft(&plan.firewall).contains("10081"));
    }
    let mut nodo = site("nodo", "nodo", 8001, local);
    nodo.service.suggested_subdomain = Some("nodo".to_owned());
    let plan = generate(&[nodo], &settings()).unwrap();
    assert_eq!(plan.local.mdns_names, vec!["nodo.local"], "a site named nodo shares the name, on port 443");
    assert_eq!(request(&plan, "local", "nodo.local", "/", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8001".into()));
}

#[test]
fn without_a_local_address_local_sites_wait_and_nothing_falls_back() {
    let no_address = DomainSettings {
        local_address: LocalAddress::Unavailable { reason: LocalUnavailable::WirelessParent },
        ..settings()
    };
    let mut shop = site("shop", "shop", 8080, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    let plan = generate(&[shop], &no_address).unwrap();

    assert!(!plan.caddy.apps.http.servers.contains_key("local"));
    assert!(plan.addresses.iter().all(|a| a.kind != AddressKind::Local));
    assert_eq!(plan.local.waiting, vec![SiteRef { package: "shop".into(), site: "shop".into() }]);
    assert_eq!(local_problem(&plan), Some(&LocalUnavailable::WirelessParent));
    assert!(plan.local.remedy.unwrap().contains("static local address"));
    let value = serde_json::to_value(&plan.local).unwrap();
    assert_eq!(value["address"]["reason"]["kind"], "wireless_parent");
    assert!(plan.firewall.local.is_none());
    assert!(plan.local.mdns_names.is_empty());
    // The public and onion addresses keep working, and never pick up the local host.
    for client in [LAN_CLIENT, INTERNET_CLIENT] {
        assert_eq!(request(&plan, "public", "shop.local", "/", client), Outcome::Dropped);
    }
    assert_eq!(request(&plan, "public", "shop.example.com", "/", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
    let text = json(&plan).to_string();
    assert!(!text.contains("shop.local") && !text.contains("10444"));

    let quiet = generate(&[site("a", "a", 8001, Exposure { public: true, ..Exposure::default() })], &no_address).unwrap();
    assert_eq!(local_problem(&quiet), None, "no local site, nothing to report");
}

#[test]
fn an_unsuitable_local_address_makes_local_sites_wait_instead_of_failing() {
    for bad in [[8, 8, 8, 8], [100, 64, 0, 9], [169, 254, 122, 240]] {
        let address = std::net::Ipv4Addr::from(bad);
        let settings = DomainSettings { local_address: LocalAddress::Ready { address }, ..settings() };
        let plan = generate(&[site("a", "a", 8001, Exposure { local: true, public: true, ..Exposure::default() })], &settings)
            .unwrap();
        assert_eq!(local_problem(&plan), Some(&LocalUnavailable::UnsuitableAddress(address)), "{bad:?}");
        assert!(!plan.caddy.apps.http.servers.contains_key("local"));
        assert!(plan.caddy.apps.http.servers.contains_key("public"), "public keeps working");
    }
}

#[test]
fn upstreams_cannot_use_the_proxy_ports() {
    for port in [80, 443, 10080, 10443, 10444] {
        let error = generate(&[site("a", "a", port, Exposure { local: true, ..Exposure::default() })], &settings());
        assert!(matches!(error, Err(WebConfigError::ReservedPort { .. })), "{port}");
    }
}

#[test]
fn stopped_or_locked_sites_show_the_unavailable_page() {
    for state in [ServiceState::Stopped, ServiceState::Locked, ServiceState::Failed, ServiceState::Waiting] {
        let mut shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
        shop.state = state;
        let plan = generate(&[shop], &settings()).unwrap();
        assert_eq!(request(&plan, "public", "shop.example.com", "/", INTERNET_CLIENT), Outcome::Status("503".into()));
        let text = json(&plan).to_string();
        assert!(text.contains("This site is unavailable"));
        assert!(!text.contains("reverse_proxy"), "no proxy route for a site that is down");
        assert!(!text.to_lowercase().contains("locked"), "device state must not be revealed");
    }
}

#[test]
fn proxy_errors_fall_back_to_the_unavailable_page() {
    let plan =
        generate(&[site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() })], &settings()).unwrap();
    let errors = serde_json::to_value(&plan.caddy.apps.http.servers["public"].errors).unwrap();
    assert_eq!(errors["routes"][0]["handle"][0]["status_code"], "{http.error.status_code}");
    assert!(errors["routes"][0]["handle"][0]["body"].as_str().unwrap().contains("unavailable"));
}

#[test]
fn tor_lines_are_generated_even_before_the_address_is_known() {
    let plan = generate(&[site("shop", "web", 8080, Exposure { onion: true, ..Exposure::default() })], &settings()).unwrap();
    assert_eq!(
        plan.tor_lines,
        vec![
            "HiddenServiceDir /var/lib/tor/nodo/shop.web",
            "HiddenServiceVersion 3",
            "HiddenServicePort 80 127.0.0.1:19000",
        ]
    );
    assert_eq!(plan.onion_pending, vec![SiteRef { package: "shop".into(), site: "web".into() }]);
    // Nothing listens yet, so Tor gets connection refused rather than reaching the wrong service.
    assert!(plan.caddy.apps.http.servers.is_empty());
    assert!(plan.addresses.is_empty());
}

#[test]
fn onion_ports_avoid_upstream_ports() {
    let plan = generate(&[site("shop", "shop", 19000, Exposure { onion: true, ..Exposure::default() })], &settings()).unwrap();
    assert_eq!(plan.tor_services[0].target, "127.0.0.1:19001");
}

#[test]
fn unix_upstreams_become_caddy_dial_addresses() {
    let mut shop = site("shop", "web", 1, Exposure { local: true, ..Exposure::default() });
    shop.service.upstream = "unix:/run/apps/shop/web.sock".to_owned();
    let plan = generate(&[shop], &settings()).unwrap();
    assert_eq!(request(&plan, "local", "web.local", "/", LAN_CLIENT), Outcome::Proxy("unix//run/apps/shop/web.sock".into()));
}

#[test]
fn rate_limit_runs_only_on_public_routes_that_asked() {
    let mut shop = site("shop", "shop", 8080, Exposure { local: true, public: true, ..Exposure::default() });
    shop.rate_limit = true;

    let without_module = generate(std::slice::from_ref(&shop), &DomainSettings { rate_limit: None, ..settings() }).unwrap();
    assert_eq!(without_module.rate_limit_unenforced, vec![SiteRef { package: "shop".into(), site: "shop".into() }]);
    assert!(!json(&without_module).to_string().contains("rate_limit"));

    let limited = generate(std::slice::from_ref(&shop), &settings()).unwrap();
    assert!(limited.rate_limit_unenforced.is_empty());
    let value = json(&limited);
    let public = value["apps"]["http"]["servers"]["public"]["routes"][0]["handle"][0]["routes"].as_array().unwrap();
    let local = value["apps"]["http"]["servers"]["local"]["routes"][0]["handle"][0]["routes"].as_array().unwrap();
    assert_eq!(public[0]["handle"][0]["handler"], "rate_limit", "counted before anything else");
    let zone = &public[0]["handle"][0]["rate_limits"]["shop.shop"];
    assert_eq!(zone["key"], "{http.request.remote.host}");
    assert_eq!(zone["window"], "60s");
    assert_eq!(zone["max_events"], 600);
    assert_eq!(public[1]["handle"][0]["handler"], "reverse_proxy");
    assert!(local.iter().all(|r| r["handle"][0]["handler"] != "rate_limit"));

    shop.rate_limit = false;
    assert!(!json(&generate(&[shop], &settings()).unwrap()).to_string().contains("rate_limit"));
}

const HOSTILE: &[&str] = &[
    "evil\n",
    "a\nHiddenServicePort 80 127.0.0.1:22",
    "a b",
    "{http.request.host}",
    "{env.SECRET}",
    "a\"}],\"x\":[{\"",
    "*",
    "*.example.com",
    "../../etc",
    "EXAMPLE",
    "a/b",
    "a\\b",
    "a;b",
    "é",
    "",
];

fn assert_rejected(site: Site, settings: &DomainSettings, what: &str) {
    let result = generate(&[site], settings);
    assert!(result.is_err(), "{what} should be rejected");
}

#[test]
fn hostile_names_are_rejected_everywhere() {
    let onion = Exposure { onion: true, ..Exposure::default() };
    let public = Exposure { public: true, ..Exposure::default() };
    let local = Exposure { local: true, ..Exposure::default() };

    for hostile in HOSTILE {
        let hostile = (*hostile).to_owned();

        let mut s = site("shop", "shop", 80, onion);
        s.package.clone_from(&hostile);
        assert_rejected(s, &settings(), &format!("package {hostile:?}"));

        let mut s = site("shop", "shop", 8080, onion);
        s.service.name.clone_from(&hostile);
        assert_rejected(s, &settings(), &format!("service {hostile:?}"));

        let mut s = site("shop", "shop", 8080, public);
        s.service.suggested_subdomain = Some(hostile.clone());
        assert_rejected(s, &settings(), &format!("suggested {hostile:?}"));

        let mut s = site("shop", "shop", 8080, public);
        s.public_subdomain = Some(hostile.clone());
        assert_rejected(s, &settings(), &format!("subdomain {hostile:?}"));

        let mut s = site("shop", "shop", 8080, local);
        s.local_host = Some(hostile.clone());
        assert_rejected(s, &settings(), &format!("local host {hostile:?}"));

        let mut s = site("shop", "shop", 8080, onion);
        s.onion_host = Some(hostile.clone());
        assert_rejected(s, &settings(), &format!("onion host {hostile:?}"));

        let s = site("shop", "shop", 8080, public);
        let bad = DomainSettings { domain: Some(hostile.clone()), ..settings() };
        assert_rejected(s, &bad, &format!("domain {hostile:?}"));

        let s = site("shop", "shop", 8080, public);
        let bad = DomainSettings { acme_email: Some(hostile.clone()), ..settings() };
        assert_rejected(s, &bad, &format!("email {hostile:?}"));
    }
}

#[test]
fn a_public_subdomain_cannot_escape_into_a_reserved_name() {
    let mut s = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    s.public_subdomain = Some("a.b".to_owned());
    let plan = generate(std::slice::from_ref(&s), &settings()).unwrap();
    assert_eq!(plan.addresses[0].host, "a.b.example.com");

    s.public_subdomain = Some("@".to_owned());
    let plan = generate(&[s], &settings()).unwrap();
    assert_eq!(plan.addresses[0].host, "example.com");

    let local_as_domain = DomainSettings { domain: Some("shop.local".to_owned()), ..settings() };
    assert_rejected(site("a", "a", 1, Exposure { public: true, ..Exposure::default() }), &local_as_domain, "local domain");
}

#[test]
fn local_hosts_must_use_a_local_suffix() {
    let mut s = site("shop", "shop", 8080, Exposure { local: true, ..Exposure::default() });
    s.local_host = Some("shop.example.com".to_owned());
    assert_rejected(s, &settings(), "public name as a local host");
}

#[test]
fn hostile_upstreams_are_rejected() {
    for upstream in [
        "",
        "127.0.0.1",
        "127.0.0.1:0",
        "127.0.0.1:99999",
        "127.0.0.1:080",
        "127.0.0.1:+80",
        "127.0.0.1:80 {",
        "127.0.0.1:{env.PORT}",
        "{http.request.header.x}:80",
        "10.0.0.5:80",
        "localhost:80",
        "example.com:443",
        "unix:/etc/shadow",
        "unix:/run/apps/other/shop.sock",
        "unix:/run/apps/shop/../other/shop.sock",
        "unix:/run/apps/shop/shop.sock\n",
    ] {
        let mut s = site("shop", "shop", 1, Exposure { local: true, ..Exposure::default() });
        s.service.upstream = upstream.to_owned();
        assert!(
            matches!(generate(&[s], &settings()), Err(WebConfigError::InvalidUpstream { .. })),
            "{upstream:?}"
        );
    }
}

#[test]
fn hostile_settings_are_rejected() {
    let s = || site("shop", "shop", 8080, Exposure { onion: true, ..Exposure::default() });
    for bad in ["/var/lib/tor x", "relative", "/a\nHiddenServicePort 1 1.1.1.1:1", "/a/../b", "/{x}"] {
        assert_rejected(s(), &DomainSettings { tor_dir: bad.to_owned(), ..settings() }, bad);
    }
    for bad in ["localhost:2019", ":2019", "unix//a b", "unix/relative"] {
        assert_rejected(s(), &DomainSettings { admin_listen: bad.to_owned(), ..settings() }, bad);
    }
    for bad in ["10.0.0.0/33", "not-an-ip", "{x}", "::1/129"] {
        assert_rejected(s(), &DomainSettings { lan_ranges: vec![bad.to_owned()], ..settings() }, bad);
    }
}

#[test]
fn error_messages_do_not_carry_raw_control_characters() {
    let mut s = site("shop", "shop", 8080, Exposure { local: true, ..Exposure::default() });
    s.package = "evil\n\x1b[31m".to_owned();
    let message = generate(&[s], &settings()).unwrap_err().to_string();
    assert!(!message.chars().any(char::is_control), "{message:?}");
}

#[test]
fn single_address_apps_cannot_use_several_exposures() {
    let mut s = site("shop", "shop", 8080, Exposure { local: true, public: true, ..Exposure::default() });
    s.service.multi_address = false;
    assert!(matches!(generate(std::slice::from_ref(&s), &settings()), Err(WebConfigError::SingleAddressOnly { .. })));

    s.exposure = Exposure { public: true, ..Exposure::default() };
    assert!(generate(&[s], &settings()).is_ok());
}

#[test]
fn public_without_a_domain_is_an_error() {
    let s = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    assert!(matches!(generate(&[s], &DomainSettings::default()), Err(WebConfigError::NoDomain { .. })));
}

#[test]
fn duplicates_are_rejected() {
    let exposure = Exposure { public: true, ..Exposure::default() };
    let a = site("a", "shop", 8001, exposure);
    assert!(matches!(generate(&[a.clone(), a.clone()], &settings()), Err(WebConfigError::DuplicateSite { .. })));

    let b = site("b", "shop", 8002, exposure);
    assert!(matches!(generate(&[a.clone(), b], &settings()), Err(WebConfigError::DuplicateHost { .. })));

    let c = site("c", "other", 8001, exposure);
    assert!(matches!(generate(&[a, c], &settings()), Err(WebConfigError::SharedUpstream { .. })));
}

#[test]
fn sites_with_no_exposure_produce_nothing() {
    let plan = generate(&[site("shop", "shop", 8080, Exposure::default())], &settings()).unwrap();
    assert!(plan.caddy.apps.http.servers.is_empty());
    assert!(plan.tor_lines.is_empty());
    assert_eq!(plan.firewall, FirewallPlan::default());
}

fn every_dns_challenge() -> Vec<(&'static str, DnsChallenge)> {
    let s = |v: &str| Secret::new(v);
    vec![
        ("porkbun", DnsChallenge::Porkbun { api_key: s("pk1_test"), secret_key: s("sk1_test") }),
        ("cloudflare", DnsChallenge::Cloudflare { api_token: s("cfut_0123456789abcdefghijABCDEFGHIJ0123456789") }),
        ("godaddy", DnsChallenge::GoDaddy { api_key: s("gdkey"), api_secret: s("gdsecret") }),
        ("gandi", DnsChallenge::Gandi { personal_access_token: s("gandi-pat") }),
        ("namedotcom", DnsChallenge::NameCom { username: "user".to_owned(), api_token: s("nc-token") }),
        ("alidns", DnsChallenge::Alibaba { access_key_id: s("LTAItest"), access_key_secret: s("alisecret") }),
    ]
}

fn dns_challenge_plan(challenge: DnsChallenge) -> WebPlan {
    let shop = site("shop", "shop", 3001, Exposure { public: true, ..Exposure::default() });
    // The VM only validates these files, so they carry no log file for Caddy to open.
    let acme = AcmeConfig { events_log: None, ..AcmeConfig::default() };
    generate(&[shop], &DomainSettings { dns_challenge: Some(challenge), acme, ..settings() }).unwrap()
}

#[test]
fn dns_challenges_use_each_module_field_names() {
    for (name, challenge) in every_dns_challenge() {
        let value = json(&dns_challenge_plan(challenge.clone()));
        let issuer = &value["apps"]["tls"]["automation"]["policies"][0]["issuers"][0];
        let provider = &issuer["challenges"]["dns"]["provider"];
        assert_eq!(issuer["module"], "acme");
        assert_eq!(provider["name"], name);
        let shown = format!("{challenge:?} {:?}", dns_challenge_plan(challenge.clone()));
        for secret in challenge.values().into_iter().filter(|v| *v != "user") {
            assert!(!shown.contains(secret), "{name}: Debug shows a credential");
        }
    }
    let godaddy = json(&dns_challenge_plan(every_dns_challenge().remove(2).1));
    let provider = &godaddy["apps"]["tls"]["automation"]["policies"][0]["issuers"][0]["challenges"]["dns"]["provider"];
    assert_eq!(provider["api_token"], "gdkey:gdsecret");
}

#[test]
fn dns_challenge_credentials_cannot_carry_placeholders() {
    // The Cloudflare module logs a token it rejects, so a malformed one must never reach Caddy.
    for bad in ["{env.HOME}", "a b", "", "x\n", "cf-test-token", "cfut_short"] {
        let challenge = DnsChallenge::Cloudflare { api_token: Secret::new(bad) };
        let shop = site("shop", "shop", 3001, Exposure { public: true, ..Exposure::default() });
        let error = generate(&[shop], &DomainSettings { dns_challenge: Some(challenge), ..settings() }).unwrap_err();
        assert!(matches!(error, WebConfigError::InvalidSetting { field: "dns_challenge", .. }), "{bad:?}");
        if !bad.is_empty() {
            assert!(!error.to_string().contains(bad), "the error must not quote the credential");
        }
    }
}

/// The config the VM tier loads into real Caddy (`tests/web-routing.sh`). Public hosts use the internal
/// CA there, because the VM is offline and cannot reach an ACME server. Run with `UPDATE_FIXTURES=1` to
/// rewrite the files after a deliberate change.
#[test]
fn vm_fixture_matches_the_generator() {
    let mut shop = site("shop", "shop", 3001, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    // The bare domain redirects to the shop and a name under a second domain serves it, so the VM
    // checks both kinds of alias against real Caddy.
    shop.public_aliases = vec![alias("example.com", AliasMode::Redirect), alias("shop.example.org", AliasMode::Serve)];
    let mut pay = site("pay", "pay", 3002, Exposure { local: true, public: true, ..Exposure::default() });
    pay.state = ServiceState::Stopped;
    let mut vault = site("vault", "vault", 3003, Exposure { public: true, ..Exposure::default() });
    vault.state = ServiceState::Locked;
    let mut blog = site("blog", "blog", 3004, Exposure { public: true, ..Exposure::default() });
    blog.rate_limit = true;
    // The VM test gives the device 192.168.77.2 as its main address and 192.168.77.3 as the local one.
    let settings = DomainSettings {
        rate_limit: Some(RateLimit { max_events: 5, window_secs: 60 }),
        local_address: LocalAddress::Ready { address: std::net::Ipv4Addr::new(192, 168, 77, 3) },
        ..settings()
    };

    let mut plan = generate(&[shop, pay, vault, blog], &settings).unwrap();
    for policy in &mut plan.caddy.apps.tls.as_mut().unwrap().automation.policies {
        policy.issuers = vec![Issuer::Internal];
    }
    plan.caddy.logging = None;
    let caddy = serde_json::to_string_pretty(&plan.caddy).unwrap() + "\n";
    let nft = render_nft(&plan.firewall);
    let names = plan.local.mdns_names.join("\n") + "\n";

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vm");
    if std::env::var_os("UPDATE_FIXTURES").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("caddy.json"), &caddy).unwrap();
        std::fs::write(dir.join("web.nft"), &nft).unwrap();
        std::fs::write(dir.join("mdns-names"), &names).unwrap();
    }
    assert_eq!(std::fs::read_to_string(dir.join("caddy.json")).unwrap(), caddy, "run with UPDATE_FIXTURES=1");
    assert_eq!(std::fs::read_to_string(dir.join("web.nft")).unwrap(), nft, "run with UPDATE_FIXTURES=1");
    assert_eq!(std::fs::read_to_string(dir.join("mdns-names")).unwrap(), names, "run with UPDATE_FIXTURES=1");

    // Each DNS provider module must accept the generated fields. The VM runs `caddy validate` on these.
    for (name, challenge) in every_dns_challenge() {
        let text = serde_json::to_string_pretty(&dns_challenge_plan(challenge).caddy).unwrap() + "\n";
        let path = dir.join(format!("dns01-{name}.json"));
        if std::env::var_os("UPDATE_FIXTURES").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "run with UPDATE_FIXTURES=1");
    }
}

#[test]
fn a_chosen_public_host_needs_no_domain_and_is_validated() {
    let mut shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    shop.public_host = Some("shop.other.org".to_owned());
    let plan = generate(std::slice::from_ref(&shop), &DomainSettings { domain: None, ..settings() }).unwrap();
    assert_eq!(request(&plan, "public", "shop.other.org", "/", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
    shop.public_host = Some("shop.local".to_owned());
    assert!(generate(&[shop], &settings()).is_err());
}

#[test]
fn sites_that_are_off_never_block_the_others() {
    let on = site("a", "a", 8001, Exposure { local: true, ..Exposure::default() });
    let mut copy = site("b", "b", 8001, Exposure::default());
    copy.service.upstream = "127.0.0.1:8001".to_owned();
    assert!(generate(&[on.clone(), copy.clone()], &settings()).is_ok());
    copy.exposure.local = true;
    assert!(matches!(generate(&[on, copy], &settings()), Err(WebConfigError::SharedUpstream { .. })));
}

fn cloudflare() -> DnsChallenge {
    DnsChallenge::Cloudflare { api_token: Secret::new("cfut_0123456789abcdefghijABCDEFGHIJ0123456789") }
}

#[test]
fn acme_issuer_names_its_directory_and_dns01_disables_the_port_challenges() {
    let shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    let plan = generate(std::slice::from_ref(&shop), &settings()).unwrap();
    let value = json(&plan);
    let issuer = &value["apps"]["tls"]["automation"]["policies"][0]["issuers"][0];
    assert_eq!(issuer["ca"], LETS_ENCRYPT);
    // Without DNS-01 Caddy uses HTTP-01 and TLS-ALPN-01, which need the forwarded ports.
    assert!(issuer.get("challenges").is_none());
    assert_eq!(value["logging"]["logs"]["nodo_tls"]["writer"]["filename"], TLS_EVENTS_LOG);
    assert_eq!(value["logging"]["logs"]["nodo_tls"]["level"], "INFO");
    assert!(value["apps"]["tls"]["automation"].get("renew_interval").is_none());

    let staging = DomainSettings {
        dns_challenge: Some(cloudflare()),
        acme: AcmeConfig { directory: LETS_ENCRYPT_STAGING.to_owned(), ..AcmeConfig::default() },
        ..settings()
    };
    let value = json(&generate(std::slice::from_ref(&shop), &staging).unwrap());
    let issuer = &value["apps"]["tls"]["automation"]["policies"][0]["issuers"][0];
    assert_eq!(issuer["ca"], LETS_ENCRYPT_STAGING);
    assert_eq!(issuer["challenges"]["dns"]["provider"]["name"], "cloudflare");
    // Production keeps Caddy's propagation check.
    assert!(issuer["challenges"]["dns"].get("propagation_timeout").is_none());

    let test = DomainSettings {
        dns_challenge: Some(cloudflare()),
        acme: AcmeConfig {
            directory: "https://203.0.113.1/dir".to_owned(),
            dns_propagation_check: false,
            renew_interval_secs: Some(10),
            events_log: Some(TLS_EVENTS_LOG.to_owned()),
        },
        ..settings()
    };
    let value = json(&generate(&[shop], &test).unwrap());
    let dns = &value["apps"]["tls"]["automation"]["policies"][0]["issuers"][0]["challenges"]["dns"];
    assert_eq!(dns["propagation_timeout"], -1);
    assert_eq!(value["apps"]["tls"]["automation"]["renew_interval"], "10s");
}

#[test]
fn no_acme_site_means_no_certificate_log() {
    let local = site("a", "a", 8001, Exposure { local: true, ..Exposure::default() });
    assert!(json(&generate(&[local], &settings()).unwrap()).get("logging").is_none());
}

#[test]
fn hostile_directories_and_log_paths_are_refused() {
    let shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    for directory in ["http://acme.test/dir", "https://", "https://a/{env.HOME}", "https://a b/dir", "ftp://x"] {
        let bad = DomainSettings {
            acme: AcmeConfig { directory: directory.to_owned(), ..AcmeConfig::default() },
            ..settings()
        };
        assert!(
            matches!(
                generate(std::slice::from_ref(&shop), &bad),
                Err(WebConfigError::InvalidSetting { field: "acme_directory", .. })
            ),
            "{directory}"
        );
    }
    let bad = DomainSettings {
        acme: AcmeConfig { events_log: Some("/var/lib/{x}".to_owned()), ..AcmeConfig::default() },
        ..settings()
    };
    assert!(generate(&[shop], &bad).is_err());
}

#[test]
fn a_kept_provider_block_is_reused_while_locked_and_stays_out_of_debug() {
    let shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    let with_dns = DomainSettings { dns_challenge: Some(cloudflare()), ..settings() };
    let old = json(&generate(std::slice::from_ref(&shop), &with_dns).unwrap());
    let kept = DnsChallenge::kept_from(&old).unwrap();
    assert!(!format!("{kept:?}").contains("cfut_"));
    assert_eq!(kept.module(), "cloudflare");
    let again = generate(std::slice::from_ref(&shop), &DomainSettings { dns_challenge: Some(kept), ..settings() }).unwrap();
    assert_eq!(json(&again), old);
    assert_eq!(DnsChallenge::kept_from(&json(&generate(&[], &settings()).unwrap())), None);
    // A damaged block with a placeholder is refused like typed credentials are.
    let hostile =
        DnsChallenge::Kept(KeptProvider(serde_json::json!({"name": "cloudflare", "api_token": "{env.HOME}"})));
    assert!(generate(&[shop], &DomainSettings { dns_challenge: Some(hostile), ..settings() }).is_err());
}

fn local_domain_settings(dns: Option<DnsChallenge>) -> DomainSettings {
    DomainSettings { local_domain: Some("home.example.com".to_owned()), dns_challenge: dns, ..settings() }
}

#[test]
fn local_names_under_the_owners_domain_get_acme_on_the_local_listener_only() {
    let shop = site("shop", "shop", 8080, Exposure { local: true, public: true, ..Exposure::default() });
    let plan = generate(&[shop], &local_domain_settings(Some(cloudflare()))).unwrap();
    let local_host = "shop.home.example.com";
    assert_eq!(plan.local.dns_names, vec![local_host]);
    assert!(plan.local.mdns_names.is_empty());
    assert!(plan.local.waiting.is_empty());
    // Served on the local address, for LAN clients only, and never on the public listener.
    assert_eq!(request(&plan, "local", local_host, "/", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
    assert_eq!(request(&plan, "local", local_host, "/", INTERNET_CLIENT), Outcome::Dropped);
    assert_eq!(request(&plan, "public", local_host, "/", INTERNET_CLIENT), Outcome::Dropped);
    let servers = &plan.caddy.apps.http.servers;
    assert_eq!(servers["local"].listen, vec!["192.168.1.60:10444"]);
    assert!(servers["local"].automatic_https.disable_redirects);
    assert!(!servers["public"].tls_connection_policies[0].matcher.sni.iter().any(|h| h == local_host));
    assert_eq!(servers["local"].tls_connection_policies[0].matcher.sni, vec![local_host]);
    // Its certificate comes from ACME through DNS-01, so no internal CA is needed at all.
    let policies = &plan.caddy.apps.tls.as_ref().unwrap().automation.policies;
    let local_policy = policies.iter().find(|p| p.subjects == [local_host]).unwrap();
    assert!(matches!(&local_policy.issuers[0], Issuer::Acme { challenges: Some(_), .. }));
    assert!(plan.caddy.apps.pki.is_none());
    assert_eq!(plan.firewall.local.as_ref().unwrap().address, LOCAL_IP);
}

#[test]
fn local_names_under_the_domain_wait_without_dns01_and_never_fall_back() {
    let shop = site("shop", "shop", 8080, Exposure { local: true, ..Exposure::default() });
    let plan = generate(&[shop], &local_domain_settings(None)).unwrap();
    assert_eq!(plan.local.waiting.len(), 1);
    assert_eq!(plan.local.names_problem, Some(LOCAL_NAMES_NEED_DNS01));
    assert_eq!(plan.local.remedy, Some(LOCAL_NAMES_NEED_DNS01));
    assert!(plan.caddy.apps.http.servers.is_empty());
    assert!(plan.local.dns_names.is_empty() && plan.local.mdns_names.is_empty());
    assert!(plan.firewall.local.is_none());
}

#[test]
fn local_domain_is_validated_and_cannot_share_a_public_name() {
    let shop = site("shop", "shop", 8080, Exposure { local: true, ..Exposure::default() });
    for bad in ["home.local", "x", "home..example.com", "{a}.com"] {
        let settings = DomainSettings { local_domain: Some(bad.to_owned()), ..local_domain_settings(Some(cloudflare())) };
        assert!(generate(std::slice::from_ref(&shop), &settings).is_err(), "{bad}");
    }
    // One DNS name never points at both the home IP and the LAN address.
    let mut both = site("shop", "shop", 8080, Exposure { local: true, public: true, ..Exposure::default() });
    both.public_host = Some("shop.home.example.com".to_owned());
    assert!(matches!(
        generate(&[both], &local_domain_settings(Some(cloudflare()))),
        Err(WebConfigError::DuplicateHost { .. })
    ));
}

#[test]
fn dot_local_stays_the_default_with_the_internal_ca() {
    let shop = site("shop", "shop", 8080, Exposure { local: true, ..Exposure::default() });
    let plan = generate(&[shop], &DomainSettings { dns_challenge: Some(cloudflare()), ..settings() }).unwrap();
    assert_eq!(plan.local.mdns_names, vec!["nodo.local", "shop.local"]);
    assert!(plan.local.dns_names.is_empty());
    let policies = &plan.caddy.apps.tls.as_ref().unwrap().automation.policies;
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0].issuers, vec![Issuer::Internal]);
}

#[test]
fn each_domain_gets_the_challenge_of_its_own_provider() {
    let public = Exposure { public: true, ..Exposure::default() };
    let mut shop = site("shop", "shop", 8001, public);
    shop.public_host = Some("shop.example.com".to_owned());
    let mut blog = site("blog", "blog", 8002, public);
    blog.public_host = Some("example.org".to_owned());
    let mut other = site("other", "other", 8003, public);
    other.public_host = Some("other.example.net".to_owned());
    let cloudflare = DnsChallenge::Cloudflare { api_token: Secret::new("cfut_0123456789abcdefghijABCDEFGHIJ0123456789") };
    let gandi = DnsChallenge::Gandi { personal_access_token: Secret::new("pat_0123456789abcdef") };
    let settings = DomainSettings {
        zone_challenges: vec![
            ZoneChallenge { zone: "example.com".to_owned(), challenge: cloudflare },
            ZoneChallenge { zone: "example.org".to_owned(), challenge: gandi },
        ],
        ..settings()
    };
    let plan = generate(&[shop, blog, other], &settings).unwrap();
    let value = json(&plan);
    let policies = value["apps"]["tls"]["automation"]["policies"].as_array().unwrap();
    let provider_of = |host: &str| {
        let policy = policies.iter().find(|p| p["subjects"].as_array().unwrap().iter().any(|s| s == host)).unwrap();
        policy["issuers"][0]["challenges"]["dns"]["provider"]["name"].as_str().map(str::to_owned)
    };
    assert_eq!(provider_of("shop.example.com").as_deref(), Some("cloudflare"));
    assert_eq!(provider_of("example.org").as_deref(), Some("gandi"));
    // A name under no connected domain falls back to the HTTP challenges.
    assert_eq!(provider_of("other.example.net"), None);
}

#[test]
fn the_longest_zone_wins_and_names_are_matched_whole() {
    let a = DnsChallenge::Cloudflare { api_token: Secret::new("cfut_0123456789abcdefghijABCDEFGHIJ0123456789") };
    let b = DnsChallenge::Gandi { personal_access_token: Secret::new("pat_0123456789abcdef") };
    let settings = DomainSettings {
        zone_challenges: vec![
            ZoneChallenge { zone: "example.com".to_owned(), challenge: a.clone() },
            ZoneChallenge { zone: "shop.example.com".to_owned(), challenge: b.clone() },
        ],
        ..settings()
    };
    assert_eq!(settings.challenge_for("pay.shop.example.com"), Some(&b));
    assert_eq!(settings.challenge_for("www.example.com"), Some(&a));
    assert_eq!(settings.challenge_for("notexample.com"), None);
}

#[test]
fn locked_configs_keep_each_subjects_provider() {
    let public = Exposure { public: true, ..Exposure::default() };
    let mut shop = site("shop", "shop", 8001, public);
    shop.public_host = Some("shop.example.com".to_owned());
    let challenge = DnsChallenge::Cloudflare { api_token: Secret::new("cfut_0123456789abcdefghijABCDEFGHIJ0123456789") };
    let settings = DomainSettings {
        zone_challenges: vec![ZoneChallenge { zone: "example.com".to_owned(), challenge }],
        ..settings()
    };
    let value = json(&generate(&[shop], &settings).unwrap());
    let kept = DnsChallenge::kept_by_subject(&value);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].0, "shop.example.com");
    assert_eq!(kept[0].1.module(), "cloudflare");
}

#[test]
fn a_zone_challenge_with_a_bad_name_is_refused() {
    let challenge = DnsChallenge::Cloudflare { api_token: Secret::new("cfut_0123456789abcdefghijABCDEFGHIJ0123456789") };
    let settings = DomainSettings {
        zone_challenges: vec![ZoneChallenge { zone: "{env.X}.com".to_owned(), challenge }],
        ..settings()
    };
    assert!(generate(&[], &settings).is_err());
}

fn alias(host: &str, mode: AliasMode) -> PublicAlias {
    PublicAlias { host: host.to_owned(), mode }
}

/// `www.example.com` with the bare domain redirecting to it, as most sites with a `www` name want.
fn www_with_apex_redirect() -> Site {
    let mut www = site("www", "web", 8001, Exposure { public: true, ..Exposure::default() });
    www.public_host = Some("www.example.com".to_owned());
    www.public_aliases = vec![alias("example.com", AliasMode::Redirect)];
    www
}

#[test]
fn an_apex_redirect_alias_answers_308_to_the_primary_keeping_path_and_query() {
    let plan = generate(&[www_with_apex_redirect()], &settings()).unwrap();
    assert_eq!(request(&plan, "public", "www.example.com", "/", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8001".into()));
    assert_eq!(request(&plan, "public", "example.com", "/shop", INTERNET_CLIENT), Outcome::Status("308".into()));
    let value = json(&plan);
    let routes = value["apps"]["http"]["servers"]["public"]["routes"].as_array().unwrap();
    let redirect = routes.iter().find(|r| r["match"][0]["host"] == serde_json::json!(["example.com"])).unwrap();
    let handler = &redirect["handle"][0]["routes"][0]["handle"][0];
    assert_eq!(handler["handler"], "static_response");
    assert_eq!(handler["status_code"], "308");
    // The request URI is the path with its query, so both survive the redirect.
    assert_eq!(handler["headers"]["Location"], serde_json::json!(["https://www.example.com{http.request.uri}"]));
    // The redirect never reaches the upstream, so it needs no rate limit.
    assert_eq!(redirect["handle"][0]["routes"].as_array().unwrap().len(), 1);
}

#[test]
fn every_alias_gets_a_certificate_an_sni_entry_an_http_redirect_and_an_address() {
    let mut www = www_with_apex_redirect();
    www.public_aliases.push(alias("www.example.org", AliasMode::Serve));
    let plan = generate(&[www], &settings()).unwrap();
    let server = &plan.caddy.apps.http.servers["public"];
    let sni = &server.tls_connection_policies[0].matcher.sni;
    for host in ["www.example.com", "example.com", "www.example.org"] {
        assert!(sni.iter().any(|h| h == host), "{host} in {sni:?}");
        let subjects: Vec<&String> = plan.caddy.apps.tls.as_ref().unwrap().automation.policies.iter().flat_map(|p| &p.subjects).collect();
        assert!(subjects.contains(&&host.to_owned()), "{host} in {subjects:?}");
        // Caddy's automatic HTTP to HTTPS redirect covers the names in host matchers.
        assert!(server.routes.iter().any(|r| r.matchers.iter().any(|m| m.host.iter().any(|h| h == host))), "{host}");
        assert!(plan.addresses.iter().any(|a| a.host == host && a.kind == AddressKind::Public), "{host}");
    }
    assert!(!server.automatic_https.disable && !server.automatic_https.disable_redirects);
    // The primary stays the site's first public address, which the screens show as its URL.
    assert_eq!(plan.addresses.iter().find(|a| a.kind == AddressKind::Public).unwrap().host, "www.example.com");
}

#[test]
fn a_serving_alias_under_a_second_domain_gets_that_domains_challenge_and_the_same_protections() {
    let mut pay = site("pay", "web", 8002, Exposure { public: true, ..Exposure::default() });
    pay.public_host = Some("www.example.com".to_owned());
    pay.public_aliases = vec![alias("pay.example.org", AliasMode::Serve)];
    pay.rate_limit = true;
    let settings = DomainSettings {
        zone_challenges: vec![
            ZoneChallenge { zone: "example.com".to_owned(), challenge: cloudflare() },
            ZoneChallenge { zone: "example.org".to_owned(), challenge: DnsChallenge::Gandi { personal_access_token: Secret::new("pat_0123456789abcdef") } },
        ],
        rate_limit: Some(RateLimit::default()),
        ..settings()
    };
    let plan = generate(&[pay], &settings).unwrap();
    assert_eq!(request(&plan, "public", "pay.example.org", "/", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8002".into()));
    assert_eq!(request(&plan, "public", "pay.example.org", "/admin", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8002".into()));
    let value = json(&plan);
    let policies = value["apps"]["tls"]["automation"]["policies"].as_array().unwrap();
    let provider_of = |host: &str| {
        let policy = policies.iter().find(|p| p["subjects"].as_array().unwrap().iter().any(|s| s == host)).unwrap();
        policy["issuers"][0]["challenges"]["dns"]["provider"]["name"].as_str().map(str::to_owned)
    };
    assert_eq!(provider_of("www.example.com").as_deref(), Some("cloudflare"));
    assert_eq!(provider_of("pay.example.org").as_deref(), Some("gandi"));
}

#[test]
fn aliases_are_unique_across_every_site_and_name() {
    let www = www_with_apex_redirect();
    // Another site's primary.
    let mut apex = site("apex", "web", 8003, Exposure { public: true, ..Exposure::default() });
    apex.public_host = Some("example.com".to_owned());
    let clash = generate(&[www.clone(), apex], &settings());
    assert_eq!(clash.unwrap_err(), WebConfigError::DuplicateHost { host: "example.com".to_owned() });
    // Another site's alias, whatever its mode.
    let mut other = site("other", "web", 8004, Exposure { public: true, ..Exposure::default() });
    other.public_host = Some("other.example.com".to_owned());
    other.public_aliases = vec![alias("example.com", AliasMode::Serve)];
    assert!(matches!(generate(&[www.clone(), other], &settings()), Err(WebConfigError::DuplicateHost { .. })));
    // The site's own primary, or the same alias twice.
    let mut own = www.clone();
    own.public_aliases.push(alias("www.example.com", AliasMode::Serve));
    assert!(matches!(generate(&[own], &settings()), Err(WebConfigError::DuplicateHost { .. })));
    let mut twice = www.clone();
    twice.public_aliases.push(alias("example.com", AliasMode::Serve));
    assert!(matches!(generate(&[twice], &settings()), Err(WebConfigError::DuplicateHost { .. })));
}

#[test]
fn hostile_alias_names_are_refused_and_aliases_wait_while_the_site_is_not_public() {
    for bad in ["example.com/x", "{env.HOME}.com", "a b.com", "shop.local", "x"] {
        let mut www = www_with_apex_redirect();
        www.public_aliases = vec![alias(bad, AliasMode::Redirect)];
        assert!(matches!(generate(&[www], &settings()), Err(WebConfigError::InvalidName { field: "public_alias", .. })), "{bad}");
    }
    let mut local = www_with_apex_redirect();
    local.exposure = Exposure { local: true, ..Exposure::default() };
    let plan = generate(&[local], &settings()).unwrap();
    assert!(!plan.caddy.apps.http.servers.contains_key("public"));
    assert!(!plan.addresses.iter().any(|a| a.host == "example.com"));
}

fn web_gui_settings() -> DomainSettings {
    DomainSettings { web_gui: true, ..settings() }
}

#[test]
fn the_web_gui_turns_on_everything_it_needs_without_any_local_site() {
    let plan = generate(&[], &web_gui_settings()).unwrap();
    assert_eq!(nodo_api::WEBGUI_URL, format!("https://{WEB_GUI_HOST}"));
    assert_eq!(plan.local.web_gui_url.as_deref(), Some(nodo_api::WEBGUI_URL));
    assert!(!plan.local.web_gui_waiting);

    // The local HTTPS server, on the local address only, knowing only the web GUI's name.
    let servers = &plan.caddy.apps.http.servers;
    assert_eq!(servers.keys().collect::<Vec<_>>(), ["local", "local-trust"]);
    let local = &servers["local"];
    assert_eq!(local.listen, vec!["192.168.1.60:10444"]);
    assert_eq!(local.tls_connection_policies[0].matcher.sni, ["nodo.local"]);
    assert!(local.strict_sni_host && local.automatic_https.disable_redirects);
    assert_eq!(nodo_api::WEBGUI_HTTP_SOCKET, "/run/nodo-web-gui/http.sock");
    for path in ["/", "/login", "/admin", "/system/keys"] {
        let proxied = Outcome::Proxy("unix//run/nodo-web-gui/http.sock".into());
        assert_eq!(request(&plan, "local", "nodo.local", path, LAN_CLIENT), proxied, "{path}");
        assert_eq!(request(&plan, "local", "nodo.local", path, INTERNET_CLIENT), Outcome::Dropped, "{path}");
    }
    // By name only: the bare address has no route, no certificate and no place in the TLS policy.
    assert_eq!(request(&plan, "local", "192.168.1.60", "/", LAN_CLIENT), Outcome::Dropped);
    assert_eq!(request(&plan, "local", "shop.local", "/", LAN_CLIENT), Outcome::Dropped);

    // The route as Caddy reads it.
    let value = json(&plan);
    let route = &value["apps"]["http"]["servers"]["local"]["routes"][0];
    assert_eq!(route["match"][0]["host"], serde_json::json!(["nodo.local"]));
    assert_eq!(route["match"][0]["remote_ip"]["ranges"], serde_json::json!(DEFAULT_LAN_RANGES));
    assert_eq!(route["terminal"], true);
    let proxy = &route["handle"][0]["routes"][0]["handle"][0];
    assert_eq!(proxy["handler"], "reverse_proxy");
    assert_eq!(proxy["upstreams"], serde_json::json!([{ "dial": "unix//run/nodo-web-gui/http.sock" }]));
    assert!(proxy.get("transport").is_none());

    // The internal CA for that one name, and its PKI app.
    let tls = plan.caddy.apps.tls.as_ref().unwrap();
    assert_eq!(tls.automation.policies.len(), 1);
    assert_eq!(tls.automation.policies[0].subjects, ["nodo.local"]);
    assert_eq!(tls.automation.policies[0].issuers, vec![Issuer::Internal]);
    assert!(plan.caddy.apps.pki.is_some());
    assert!(!serde_json::to_string(&plan.caddy.apps.tls).unwrap().contains("192.168.1.60"));
    assert!(plan.caddy.logging.is_none(), "no ACME, so no certificate log");

    // The trust page, the mDNS name, and the local firewall block. The trust page, in plain HTTP,
    // still answers by the bare address as it always did.
    assert_eq!(servers["local-trust"].listen, vec!["192.168.1.60:10081"]);
    assert!(matches!(request(&plan, "local-trust", "192.168.1.60", "/", LAN_CLIENT), Outcome::File(_)));
    assert_eq!(plan.local.trust_url.as_deref(), Some("http://nodo.local"));
    assert_eq!(plan.local.mdns_names, ["nodo.local"]);
    let firewall = plan.firewall.local.as_ref().unwrap();
    assert_eq!((firewall.address, firewall.caddy_port, firewall.trust_port), (LOCAL_IP, 10444, Some(10081)));
    let rules = render_nft(&plan.firewall);
    assert!(rules.contains("tcp dport 443 dnat ip to 192.168.1.60:10444"), "{rules}");
    assert!(rules.contains("tcp dport 80 dnat ip to 192.168.1.60:10081"), "{rules}");
    assert!(rules.contains("tcp dport 10444 ct status dnat accept"), "{rules}");
    assert!(rules.contains("udp dport 5353 accept"), "{rules}");
    assert!(!rules.contains("nodo-web-gui"), "the web GUI's socket needs no firewall rule");
    assert!(plan.firewall.public_tcp.is_empty() && plan.firewall.redirects.is_empty());
    assert!(plan.addresses.is_empty(), "the web GUI is not a package's site");
}

#[test]
fn the_web_gui_off_leaves_no_route_and_changes_nothing() {
    let mut shop = site("shop", "shop", 8080, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    let sites = [shop, site("pay", "pay", 8081, Exposure { local: true, ..Exposure::default() })];
    let off = generate(&sites, &settings()).unwrap();
    assert!(!settings().web_gui, "off unless the owner turns it on");
    assert_eq!(off.local.web_gui_url, None);
    assert!(!off.local.web_gui_waiting);
    let text = serde_json::to_string_pretty(&off.caddy).unwrap();
    assert!(!text.contains("nodo-web-gui"), "{text}");
    for client in [LAN_CLIENT, INTERNET_CLIENT] {
        for server in ["local", "public"] {
            for host in ["nodo.local", "192.168.1.60"] {
                assert_eq!(request(&off, server, host, "/", client), Outcome::Dropped, "{server} {host}");
            }
        }
    }
    assert!(!off.caddy.apps.http.servers["local"].tls_connection_policies[0].matcher.sni.contains(&"nodo.local".to_owned()));

    // With no site and the web GUI off there is nothing at all.
    let empty = generate(&[], &settings()).unwrap();
    assert!(empty.caddy.apps.http.servers.is_empty());
    assert!(empty.caddy.apps.tls.is_none() && empty.caddy.apps.pki.is_none());
    assert_eq!(empty.firewall, FirewallPlan::default());
    assert!(empty.local.mdns_names.is_empty() && empty.local.trust_url.is_none());

    // Turning it on only adds: the sites' own routes, addresses and firewall stay as they were.
    let on = generate(&sites, &web_gui_settings()).unwrap();
    assert_eq!(on.addresses, off.addresses);
    assert_eq!(on.firewall, off.firewall);
    assert_eq!(render_nft(&on.firewall), render_nft(&off.firewall));
    assert_eq!(on.tor_lines, off.tor_lines);
    assert_eq!(on.caddy.apps.http.servers["public"], off.caddy.apps.http.servers["public"]);
    assert_eq!(on.caddy.apps.http.servers["local-trust"], off.caddy.apps.http.servers["local-trust"]);
    let (with, without) = (&on.caddy.apps.http.servers["local"].routes, &off.caddy.apps.http.servers["local"].routes);
    assert_eq!(with[1..], without[..], "the web GUI's route goes first and the rest is unchanged");
    assert_eq!(request(&on, "local", "nodo.local", "/", LAN_CLIENT), Outcome::Proxy("unix//run/nodo-web-gui/http.sock".into()));
    assert_eq!(request(&on, "local", "shop.local", "/", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
    // Never on the public or an onion listener, whatever the Host header says.
    for client in [LAN_CLIENT, INTERNET_CLIENT] {
        assert_eq!(request(&on, "public", "nodo.local", "/", client), Outcome::Dropped);
        assert_eq!(request(&on, "public", "192.168.1.60", "/", client), Outcome::Dropped);
        assert_eq!(request(&on, onion_server_for(&on, ONION), "nodo.local", "/", client), Outcome::Dropped);
    }
    assert_eq!(on.local.mdns_names, ["nodo.local", "pay.local", "shop.local"]);
}

#[test]
fn the_web_gui_waits_for_the_local_address_like_a_local_site() {
    for reason in [LocalUnavailable::NoLease, LocalUnavailable::UnsuitableAddress(std::net::Ipv4Addr::new(8, 8, 8, 8))] {
        let waiting = DomainSettings { local_address: LocalAddress::Unavailable { reason }, ..web_gui_settings() };
        let plan = generate(&[], &waiting).unwrap();
        assert!(plan.local.web_gui_waiting);
        assert_eq!(plan.local.web_gui_url, None);
        assert!(plan.caddy.apps.http.servers.is_empty(), "never served on the main address or another port");
        assert!(plan.caddy.apps.tls.is_none() && plan.caddy.apps.pki.is_none());
        assert_eq!(plan.firewall, FirewallPlan::default());
        assert!(plan.local.mdns_names.is_empty() && plan.local.trust_url.is_none());
        assert!(plan.local.remedy.is_some(), "the owner is told what to do about the address");
        assert!(local_problem(&plan).is_some());
    }
    // A public address handed out as the local one counts as none.
    let public_ip = DomainSettings {
        local_address: LocalAddress::Ready { address: std::net::Ipv4Addr::new(8, 8, 8, 8) },
        ..web_gui_settings()
    };
    assert!(generate(&[], &public_ip).unwrap().local.web_gui_waiting);
    // Without the web GUI a missing address is nobody's problem.
    let quiet = DomainSettings { local_address: LocalAddress::Unavailable { reason: LocalUnavailable::NoLease }, ..settings() };
    let plan = generate(&[], &quiet).unwrap();
    assert!(!plan.local.web_gui_waiting && plan.local.remedy.is_none() && local_problem(&plan).is_none());
}

#[test]
fn no_site_can_take_the_web_guis_name_or_socket() {
    // A site's socket upstream must be its own under /run/apps, so none can name the web GUI's.
    for settings in [settings(), web_gui_settings()] {
        let mut squatter = site("evil", "evil", 8001, Exposure { local: true, ..Exposure::default() });
        squatter.service.upstream = format!("unix:{}", nodo_api::WEBGUI_HTTP_SOCKET);
        assert!(matches!(generate(&[squatter], &settings), Err(WebConfigError::InvalidUpstream { .. })));
        // The loopback port it once used is an ordinary port again.
        let old_port = site("fine", "fine", 10082, Exposure { local: true, ..Exposure::default() });
        assert!(generate(&[old_port], &settings).is_ok());
    }
    let mut named = site("nodo", "nodo", 8001, Exposure { local: true, ..Exposure::default() });
    assert_eq!(
        generate(std::slice::from_ref(&named), &web_gui_settings()),
        Err(WebConfigError::DuplicateHost { host: "nodo.local".into() })
    );
    // The same site is fine while the web GUI is off, as it always was.
    assert!(generate(std::slice::from_ref(&named), &settings()).is_ok());
    // And with the web GUI on while that site is not on the local network.
    named.exposure = Exposure { public: true, ..Exposure::default() };
    assert!(generate(&[named], &web_gui_settings()).is_ok());
}

#[test]
fn the_web_gui_keeps_the_internal_ca_beside_local_names_under_the_owners_domain() {
    let dns = DnsChallenge::Cloudflare { api_token: Secret::new("cfut_0123456789abcdefghijABCDEFGHIJ0123456789") };
    let settings = DomainSettings { web_gui: true, ..local_domain_settings(Some(dns)) };
    let plan = generate(&[site("shop", "shop", 8080, Exposure { local: true, ..Exposure::default() })], &settings).unwrap();
    let policies = &plan.caddy.apps.tls.as_ref().unwrap().automation.policies;
    assert_eq!(policies[0].subjects, ["nodo.local"]);
    assert_eq!(policies[0].issuers, vec![Issuer::Internal]);
    assert!(policies[1..].iter().all(|p| !p.subjects.iter().any(|s| s == "nodo.local")));
    assert!(matches!(policies[1].issuers[0], Issuer::Acme { .. }));
    // The trust page is there for the web GUI's root, though the site itself needs none.
    assert!(plan.caddy.apps.http.servers.contains_key("local-trust"));
    assert_eq!(plan.local.mdns_names, ["nodo.local"]);
}
