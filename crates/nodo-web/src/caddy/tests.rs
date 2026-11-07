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
        onion_host: None,
        rate_limit: false,
    }
}

fn all_exposures() -> Exposure {
    Exposure { local: true, onion: true, public: true }
}

fn settings() -> DomainSettings {
    DomainSettings { domain: Some("example.com".to_owned()), ..DomainSettings::default() }
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
    host_ok && path_ok && ip_ok
}

fn walk(routes: &[Route], host: &str, path: &str, client: &str) -> Outcome {
    for route in routes {
        if !route.matchers.is_empty() && !route.matchers.iter().any(|m| matches(m, host, path, client)) {
            continue;
        }
        for handle in &route.handle {
            match handle {
                Handle::Hook(_) => {}
                Handle::Known(Handler::Subroute { routes }) => return walk(routes, host, path, client),
                Handle::Known(Handler::ReverseProxy { upstreams }) => {
                    return Outcome::Proxy(upstreams[0].dial.clone())
                }
                Handle::Known(Handler::StaticResponse { abort: true, .. }) => return Outcome::Dropped,
                Handle::Known(Handler::StaticResponse { status_code, .. }) => {
                    return Outcome::Status(status_code.clone().unwrap())
                }
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
    let server = &value["apps"]["http"]["servers"]["web"];

    assert_eq!(server["listen"], serde_json::json!([":443"]));
    let route = &server["routes"][0];
    assert_eq!(route["match"][0]["host"][0], "shop.example.com");
    assert_eq!(route["terminal"], true);
    assert_eq!(route["handle"][0]["handler"], "subroute");
    let inner = &route["handle"][0]["routes"];
    assert_eq!(inner[0]["match"][0]["path"][0], "/admin");
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
    assert_eq!(request(&plan, "web", "shop.example.com", "/", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
    assert_eq!(request(&plan, "web", "shop.local", "/", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8080".into()));
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

    for path in ["/admin", "/ADMIN", "/admin/", "/admin/users", "/admin;x=1"] {
        assert_eq!(request(&plan, "web", "shop.example.com", path, INTERNET_CLIENT), blocked, "{path}");
        assert_eq!(request(&plan, onion, ONION, path, "127.0.0.1"), blocked, "{path}");
        assert_eq!(
            request(&plan, "web", "shop.local", path, LAN_CLIENT),
            Outcome::Proxy("127.0.0.1:8080".into()),
            "{path}"
        );
    }
    // A LAN client using the public name gets the public rules.
    assert_eq!(request(&plan, "web", "shop.example.com", "/admin", LAN_CLIENT), blocked);
    // Neighbouring paths stay served.
    assert_eq!(
        request(&plan, "web", "shop.example.com", "/administrator", INTERNET_CLIENT),
        Outcome::Proxy("127.0.0.1:8080".into())
    );
}

#[test]
fn admin_paths_stay_blocked_while_the_upstream_is_down() {
    let mut shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    shop.state = ServiceState::Locked;
    let plan = generate(&[shop], &settings()).unwrap();
    assert_eq!(request(&plan, "web", "shop.example.com", "/admin", INTERNET_CLIENT), Outcome::Status("404".into()));
}

#[test]
fn root_admin_path_blocks_the_whole_host() {
    let mut shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
    shop.service.admin_paths = vec!["/".to_owned()];
    let plan = generate(&[shop], &settings()).unwrap();
    assert_eq!(request(&plan, "web", "shop.example.com", "/any", INTERNET_CLIENT), Outcome::Status("404".into()));
}

#[test]
fn a_host_only_ever_reaches_its_own_upstream() {
    let mut a = site("a", "a", 8001, all_exposures());
    a.onion_host = Some(ONION.to_owned());
    let mut b = site("b", "b", 8002, all_exposures());
    b.onion_host = Some(ONION_B.to_owned());
    let plan = generate(&[a, b], &settings()).unwrap();

    assert_eq!(request(&plan, "web", "a.example.com", "/", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8001".into()));
    assert_eq!(request(&plan, "web", "b.example.com", "/", INTERNET_CLIENT), Outcome::Proxy("127.0.0.1:8002".into()));
    assert_eq!(request(&plan, "web", "a.local", "/", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8001".into()));
    assert_eq!(request(&plan, "web", "b.local", "/", LAN_CLIENT), Outcome::Proxy("127.0.0.1:8002".into()));
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

    assert_eq!(request(&plan, "web", "shop.local", "/", INTERNET_CLIENT), Outcome::Dropped);
    assert_eq!(request(&plan, "web", "shop.local", "/admin", INTERNET_CLIENT), Outcome::Dropped);
    assert_eq!(request(&plan, "web", ONION, "/", INTERNET_CLIENT), Outcome::Dropped);
    assert_eq!(request(&plan, "web", "unknown.example.com", "/", INTERNET_CLIENT), Outcome::Dropped);
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
    assert!(!servers.contains_key("web"));
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
    assert_eq!(policies[1].issuers, vec![Issuer::Acme { email: Some("me@example.com".to_owned()) }]);
    assert!(!plan.caddy.apps.pki.as_ref().unwrap().certificate_authorities["local"].install_trust);
    assert!(!plan.caddy.apps.http.servers["web"].automatic_https.disable);
    assert!(!plan.caddy.apps.http.servers["web"].protocols.iter().any(|p| p == "h3"));
}

#[test]
fn ports_80_and_443_open_only_with_a_public_site() {
    let none = generate(&[], &settings()).unwrap();
    assert_eq!(none.firewall, FirewallPlan::default());
    assert!(none.caddy.apps.http.servers.is_empty());

    let local = generate(&[site("a", "a", 8001, Exposure { local: true, ..Exposure::default() })], &settings()).unwrap();
    assert_eq!(local.firewall, FirewallPlan { public_tcp: vec![], lan_tcp: vec![443] });
    assert!(local.caddy.apps.http.servers["web"].automatic_https.disable_redirects);

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
    assert_eq!(public.firewall, FirewallPlan { public_tcp: vec![80, 443], lan_tcp: vec![443] });
    assert!(!public.caddy.apps.http.servers["web"].automatic_https.disable_redirects);
}

#[test]
fn stopped_or_locked_sites_show_the_unavailable_page() {
    for state in [ServiceState::Stopped, ServiceState::Locked, ServiceState::Failed, ServiceState::Waiting] {
        let mut shop = site("shop", "shop", 8080, Exposure { public: true, ..Exposure::default() });
        shop.state = state;
        let plan = generate(&[shop], &settings()).unwrap();
        assert_eq!(request(&plan, "web", "shop.example.com", "/", INTERNET_CLIENT), Outcome::Status("503".into()));
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
    let errors = serde_json::to_value(&plan.caddy.apps.http.servers["web"].errors).unwrap();
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
    assert_eq!(request(&plan, "web", "web.local", "/", LAN_CLIENT), Outcome::Proxy("unix//run/apps/shop/web.sock".into()));
}

#[test]
fn rate_limit_hook_runs_only_on_public_routes_that_asked() {
    let mut shop = site("shop", "shop", 8080, Exposure { local: true, public: true, ..Exposure::default() });
    shop.rate_limit = true;

    let unhooked = generate(std::slice::from_ref(&shop), &settings()).unwrap();
    assert_eq!(unhooked.rate_limit_unenforced, vec![SiteRef { package: "shop".into(), site: "shop".into() }]);

    let hook = PublicHandlerHook::new(serde_json::json!({"handler": "rate_limit"})).unwrap();
    let hooked = generate(&[shop], &DomainSettings { public_hook: Some(hook), ..settings() }).unwrap();
    assert!(hooked.rate_limit_unenforced.is_empty());

    let routes = &json(&hooked)["apps"]["http"]["servers"]["web"]["routes"];
    let public = routes[0]["handle"][0]["routes"].as_array().unwrap();
    let local = routes[1]["handle"][0]["routes"].as_array().unwrap();
    assert_eq!(public[1]["handle"][0]["handler"], "rate_limit");
    assert_eq!(public[2]["handle"][0]["handler"], "reverse_proxy");
    assert!(local.iter().all(|r| r["handle"][0]["handler"] != "rate_limit"));
    assert!(PublicHandlerHook::new(serde_json::json!({"nope": 1})).is_none());
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
