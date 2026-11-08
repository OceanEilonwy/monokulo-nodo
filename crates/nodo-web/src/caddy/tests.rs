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
        admin_paths: vec!["/admin".to_owned()],
        multi_address: true,
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
    let regexp_ok = matcher.path_regexp.as_ref().is_none_or(|re| {
        let inner = re.pattern.strip_prefix("(?i)^(?:").and_then(|p| p.strip_suffix(")(?:[^a-z0-9_~-]|$)")).unwrap();
        let lower = path.to_lowercase();
        inner.split('|').any(|alt| {
            lower.strip_prefix(&alt.replace("\\.", ".").to_lowercase()).is_some_and(|rest| {
                rest.chars().next().is_none_or(|c| !(c.is_ascii_alphanumeric() || "_~-".contains(c)))
            })
        })
    });
    host_ok && path_ok && regexp_ok && ip_ok
}

fn walk(routes: &[Route], host: &str, path: &str, client: &str) -> Outcome {
    for route in routes {
        if !route.matchers.is_empty() && !route.matchers.iter().any(|m| matches(m, host, path, client)) {
            continue;
        }
        for handle in &route.handle {
            match handle {
                Handler::RateLimit { .. } => {}
                Handler::Subroute { routes } => return walk(routes, host, path, client),
                Handler::ReverseProxy { upstreams } => return Outcome::Proxy(upstreams[0].dial.clone()),
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
    let inner = &route["handle"][0]["routes"];
    assert_eq!(inner[0]["match"][0]["path"][0], "/admin");
    assert_eq!(inner[0]["match"][1]["path_regexp"]["pattern"], "(?i)^(?:/admin)(?:[^a-z0-9_~-]|$)");
    assert_eq!(inner[0]["handle"][0]["handler"], "static_response");
    assert_eq!(inner[0]["handle"][0]["status_code"], "404");
    assert_eq!(inner[1]["handle"][0]["handler"], "reverse_proxy");
    assert_eq!(inner[1]["handle"][0]["upstreams"][0]["dial"], "127.0.0.1:8080");
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
fn admin_paths_are_blocked_on_public_and_onion_only() {
    let mut shop = site("shop", "shop", 8080, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    let plan = generate(&[shop], &settings()).unwrap();
    let onion = onion_server_for(&plan, ONION);
    let blocked = Outcome::Status("404".into());

    for path in ["/admin", "/ADMIN", "/admin/", "/admin/users", "/admin;x=1", "/admin.", "/admin ", "/admin\0", "/Admin.json"] {
        assert_eq!(request(&plan, "public", "shop.example.com", path, INTERNET_CLIENT), blocked, "{path}");
        assert_eq!(request(&plan, onion, ONION, path, "127.0.0.1"), blocked, "{path}");
        assert_eq!(
            request(&plan, "local", "shop.local", path, LAN_CLIENT),
            Outcome::Proxy("127.0.0.1:8080".into()),
            "{path}"
        );
    }
    // A LAN client using the public name gets the public rules.
    assert_eq!(request(&plan, "public", "shop.example.com", "/admin", LAN_CLIENT), blocked);
    // Neighbouring names stay served.
    for path in ["/administrator", "/admin-panel", "/admin_x", "/admins"] {
        assert_eq!(
            request(&plan, "public", "shop.example.com", path, INTERNET_CLIENT),
            Outcome::Proxy("127.0.0.1:8080".into()),
            "{path}"
        );
    }
}

#[test]
fn admin_paths_stay_blocked_while_the_upstream_is_down() {
    let mut shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    shop.state = ServiceState::Locked;
    let plan = generate(&[shop], &settings()).unwrap();
    assert_eq!(request(&plan, "public", "shop.example.com", "/admin", INTERNET_CLIENT), Outcome::Status("404".into()));
}

#[test]
fn root_admin_path_blocks_the_whole_host() {
    let mut shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    shop.service.admin_paths = vec!["/".to_owned()];
    let plan = generate(&[shop], &settings()).unwrap();
    assert_eq!(request(&plan, "public", "shop.example.com", "/any", INTERNET_CLIENT), Outcome::Status("404".into()));
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
        assert_eq!(request(&plan, "public", "shop.local", "/admin", client), Outcome::Dropped);
        assert_eq!(request(&plan, "public", ONION, "/", client), Outcome::Dropped);
        assert_eq!(request(&plan, "public", "unknown.example.com", "/", client), Outcome::Dropped);
        assert_eq!(request(&plan, "public", "shop.example.com", "/admin", client), Outcome::Status("404".into()));
    }
    let public = &plan.caddy.apps.http.servers["public"];
    assert_eq!(public.tls_connection_policies[0].matcher.sni, vec!["shop.example.com"]);
}

#[test]
fn the_local_listener_serves_only_local_hosts_to_lan_clients() {
    let mut shop = site("shop", "shop", 8080, all_exposures());
    shop.onion_host = Some(ONION.to_owned());
    let plan = generate(&[shop], &settings()).unwrap();

    assert_eq!(request(&plan, "local", "shop.local", "/admin", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
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
    assert_eq!(local.local.mdns_names, vec!["a.local"]);
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
    assert_eq!(public[0]["handle"][0]["handler"], "static_response", "admin paths are refused before counting");
    assert_eq!(public[1]["handle"][0]["handler"], "rate_limit");
    let zone = &public[1]["handle"][0]["rate_limits"]["shop.shop"];
    assert_eq!(zone["key"], "{http.request.remote.host}");
    assert_eq!(zone["window"], "60s");
    assert_eq!(zone["max_events"], 600);
    assert_eq!(public[2]["handle"][0]["handler"], "reverse_proxy");
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
fn hostile_admin_paths_are_rejected() {
    for path in ["admin", "/admin*", "/{x}", "/a b", "/a\n", "//admin", "/a/../b", "/%61dmin", ""] {
        let mut s = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
        s.service.admin_paths = vec![path.to_owned()];
        assert!(matches!(generate(&[s], &settings()), Err(WebConfigError::InvalidAdminPath { .. })), "{path:?}");
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
fn admin_regexp_escapes_dots_and_joins_paths() {
    let paths = vec!["/wp-admin/".to_owned(), "/v1.0/admin".to_owned(), "/".to_owned()];
    assert_eq!(admin_path_regexp(&paths).unwrap(), "(?i)^(?:/wp-admin|/v1\\.0/admin)(?:[^a-z0-9_~-]|$)");
    assert_eq!(admin_path_regexp(&[]), None);
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
    assert_eq!(request(&plan, "local", local_host, "/admin", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
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
    assert_eq!(plan.local.mdns_names, vec!["shop.local"]);
    assert!(plan.local.dns_names.is_empty());
    let policies = &plan.caddy.apps.tls.as_ref().unwrap().automation.policies;
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0].issuers, vec![Issuer::Internal]);
}
