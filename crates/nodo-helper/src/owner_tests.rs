//! Owner pages through `Helper::handle`, with a scripted web side and package answers.

use nodo_api::{
    CertProblem, CertProblemKind, CertificateReport, ChallengeChoice, Dns01Support, DomainProvider,
    HostCertificate, HostKind, ProviderDomain, ReachabilityDetail, Request, SiteId, WebStatus,
};

use super::*;
use crate::exposure::{ExposureApplier, WebInput};
use crate::fake::{FakeSystem, TestPolicy, TEST_PATHS};
use crate::helper::{AllowedUids, Timing};

const PASS: &str = "correct horse battery";
const NOW: u64 = 1_800_000_000;

const PINGERS: &str = r#"api_version = 1
id = "pingers"
version = "1"
exec = ["bin/pingers"]

[owner_page]
title = "Setup"
url = "http://127.0.0.1:9475/owner"

[[network.inbound]]
port = 9475
protocol = "tcp"
scope = "local"
"#;

const SERVER: &str = r#"api_version = 1
id = "pingers-server"
version = "1"
exec = ["bin/server"]

[health.tcp]
port = 9480

[[web]]
name = "api"
upstream = "127.0.0.1:9480"
suggested_subdomain = "pingers"

[[web]]
name = "spare"
upstream = "127.0.0.1:9481"
"#;

/// The web side as a device with one provider, one public site and its certificate has it.
struct Web;

impl ExposureApplier for Web {
    fn apply(&self, _input: &WebInput) -> Result<(), String> {
        Ok(())
    }

    fn status(&self, _input: &WebInput) -> Result<WebStatus, ApiError> {
        Ok(WebStatus {
            sites: vec![],
            local_address: None,
            local_unavailable: None,
            local_remedy: None,
            domain: Some("example.org".into()),
            provider: Some("cloudflare".into()),
            ddns_running: true,
            public_ip: Some("203.0.113.5".into()),
            dns_changed_at: None,
            stale_records: vec![],
        })
    }

    fn domain_providers(&self, _input: &WebInput) -> Result<Vec<DomainProvider>, ApiError> {
        Ok(vec![DomainProvider {
            id: "cloudflare".into(),
            kind: "cloudflare".into(),
            name: "Cloudflare".into(),
            domains: vec![ProviderDomain {
                name: "example.org".into(),
                used_by: vec![],
                held_by: None,
            }],
            lists_domains: true,
            checked_at: None,
            problem: None,
            dns01: Dns01Support::Bundled,
            key: None,
            blocked: vec![],
        }])
    }

    fn certificates(&self, _input: &WebInput) -> Result<CertificateReport, ApiError> {
        let cert = |host: &str, state: CertState| HostCertificate {
            site: SiteId {
                package: "pingers-server".into(),
                site: "api".into(),
            },
            host: host.into(),
            kind: HostKind::Public,
            state,
            issuer: None,
            not_after: Some(1_803_000_000),
            last_renewal: None,
            challenge: None,
            last_error: (state == CertState::Failed).then(|| CertProblem {
                at: 1,
                kind: CertProblemKind::RateLimited,
                summary: "Let's Encrypt asked to wait\u{1b}[2J".into(),
                detail: String::new(),
            }),
        };
        Ok(CertificateReport {
            hosts: vec![
                cert("pingers.example.org", CertState::Valid),
                cert("broken.example.org", CertState::Failed),
            ],
            challenge: ChallengeChoice::Dns01 {
                provider: "cloudflare".into(),
            },
            staging: false,
            test_server: false,
            local_ca_fingerprint: None,
        })
    }

    fn device_address(&self) -> Option<String> {
        Some("192.168.1.20".into())
    }
}

struct Clock;

impl crate::pkgsys::PackageBackend for Clock {
    fn index(&self, _: &str) -> Result<crate::pkgsys::FeedIndex, String> {
        Err("no feeds".into())
    }
    fn review(
        &self,
        _: &mut nodo_feed::PinStore,
        _: &str,
        _: &str,
        _: Option<&nodo_install::Permissions>,
    ) -> Result<nodo_install::Reviewed, nodo_install::InstallError> {
        unreachable!()
    }
    fn install(
        &self,
        _: &mut nodo_feed::PinStore,
        _: &str,
        _: &str,
        _: &nodo_install::Permissions,
        _: &str,
        _: &std::path::Path,
    ) -> Result<(nodo_install::Outcome, Option<std::path::PathBuf>), nodo_install::InstallError>
    {
        unreachable!()
    }
    fn reinstall(&self, _: &std::path::Path) -> Result<String, nodo_install::InstallError> {
        unreachable!()
    }
    fn inspect_kept(
        &self,
        _: &std::path::Path,
    ) -> Result<(String, nodo_install::Permissions), nodo_install::InstallError> {
        unreachable!()
    }
    fn remove(&self, _: &str) -> Result<(), nodo_install::InstallError> {
        unreachable!()
    }
    fn now(&self) -> u64 {
        NOW
    }
    fn local_minute(&self) -> u16 {
        0
    }
    fn copy_tree(
        &self,
        _: &std::path::Path,
        _: &std::path::Path,
        _: Option<&str>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn restore_tree(
        &self,
        _: &std::path::Path,
        _: &std::path::Path,
        _: Option<&str>,
    ) -> Result<(), String> {
        Ok(())
    }
    fn remove_tree(&self, _: &std::path::Path) -> Result<(), String> {
        Ok(())
    }
    fn tree_size(&self, _: &std::path::Path) -> u64 {
        0
    }
    fn free_bytes(&self, _: &std::path::Path) -> u64 {
        0
    }
    fn kept_exists(&self, _: &std::path::Path) -> bool {
        false
    }
    fn remove_kept(&self, _: &std::path::Path) {}
}

fn setup(web: Box<dyn ExposureApplier>) -> Helper<FakeSystem> {
    let sys = FakeSystem::default();
    sys.add_manifest(PINGERS);
    sys.add_manifest(SERVER);
    sys.put(
        "/s/public-hosts.toml",
        b"[pingers-server]\napi = \"pingers.example.org\"\n",
    );
    sys.put(
        "/s/exposure.toml",
        b"[pingers-server]\napi = { local = false, onion = false, public = true }\n",
    );
    let h = Helper::new(sys, AllowedUids::new([1000]), Box::new(TestPolicy), web)
        .with_paths(TEST_PATHS())
        .with_kdf(crate::keys::KdfParams {
            memory_kib: 64,
            iterations: 1,
            lanes: 1,
        })
        .with_timing(Timing {
            health_attempts: 1,
            ..Timing::default()
        })
        .with_radios(crate::radio_tests::no_radios())
        .with_packages(Box::new(Clock))
        .with_manage_paths(crate::manage::ManagePaths {
            feeds: "/s/feeds.toml".into(),
            last_backup: "/s/last-backup.toml".into(),
            backups: "/nonexistent/nodo-test-backups".into(),
            ..crate::manage::ManagePaths::default()
        });
    h.init_device(&Secret::new(PASS)).unwrap();
    h
}

fn answer(h: &Helper<FakeSystem>, body: &str) {
    h.sys.put("page 9475/owner", body.as_bytes());
}

fn page(h: &Helper<FakeSystem>) -> OwnerPageView {
    match h.handle(
        1000,
        Request::OwnerPage {
            package: "pingers".into(),
            passphrase: Secret::new(PASS),
        },
    ) {
        Ok(Response::OwnerPage(view)) => view,
        other => panic!("{other:?}"),
    }
}

fn checks(item: &OwnerItem) -> Vec<(String, OwnerCheckState, String)> {
    item.checks()
        .into_iter()
        .map(|c| {
            (
                c.label.clone(),
                c.state,
                c.detail.clone().unwrap_or_default(),
            )
        })
        .collect()
}

const FULL: &str = r#"{"version": 1, "title": "Pingers setup", "intro": "Four steps.",
  "steps": [
    {"title": "Put the sites online", "state": "auto", "items": [
      {"kind": "text", "text": "Point the phones at the server."},
      {"kind": "site", "package": "pingers-server", "site": "api"},
      {"kind": "service", "package": "pingers-server"},
      {"kind": "domain", "domain": "example.org"}
    ]},
    {"title": "Connect the phones", "state": "auto", "items": [
      {"kind": "value", "label": "PINGERS_SERVER_HOST", "value": "https://pingers.example.org"},
      {"kind": "qr", "label": "Scan with the phone's camera", "data": "https://pingers.example.org/j#s1abc"},
      {"kind": "check", "label": "A phone joined", "state": "waiting"}
    ]},
    {"title": "Missing pieces", "state": "auto", "items": [
      {"kind": "site", "package": "pingers-maps", "site": "tiles"},
      {"kind": "service", "package": "pingers-maps"},
      {"kind": "site", "package": "pingers-server", "site": "nope"}
    ]},
    {"title": "Back up", "state": "auto", "items": [{"kind": "backup"}]},
    {"title": "Read the guide", "state": "done", "items": [{"kind": "text", "text": "Done by hand."}]},
    {"title": "Nothing to check", "state": "auto", "items": [{"kind": "text", "text": "Just words."}]}
  ]}"#;

#[test]
fn the_passphrase_is_checked_like_an_unlock_before_anything_is_fetched() {
    let h = setup(Box::new(Web));
    answer(&h, FULL);
    h.sys.clear_calls();
    let wrong = h.handle(
        1000,
        Request::OwnerPage {
            package: "pingers".into(),
            passphrase: Secret::new("wrong one"),
        },
    );
    assert_eq!(wrong, Err(ApiError::BadPassphrase));
    assert!(
        !h.sys.calls().iter().any(|c| c.starts_with("page")),
        "{:?}",
        h.sys.calls()
    );
    assert!(
        !h.sys.sleeps().is_empty(),
        "a wrong passphrase pays the unlock delay"
    );
    let none = h.handle(
        1000,
        Request::OwnerPage {
            package: "pingers-server".into(),
            passphrase: Secret::new(PASS),
        },
    );
    assert!(
        matches!(&none, Err(ApiError::Failed(m)) if m.contains("no setup page")),
        "{none:?}"
    );
}

#[test]
fn a_full_page_is_filled_in_from_the_helpers_own_state() {
    let h = setup(Box::new(Web));
    answer(&h, FULL);
    h.sys.put("dns pingers.example.org", b"203.0.113.5\n");
    *lock(&h.last_reachability) = Some((
        NOW - 600,
        ReachabilityDetail {
            checks: vec![],
            verdict: "ready".into(),
            public_ip: Some("203.0.113.5".into()),
            router_wan: None,
            device_ip: Some("192.168.1.20".into()),
            gateway: None,
            manual_check: vec![],
            warnings: vec![],
        },
    ));
    h.sys.put(
        "/s/last-backup.toml",
        b"created = 1800000000\nhas_device_key = true\nusb = true\n",
    );
    let view = page(&h);
    assert_eq!(view.title, "Pingers setup");
    assert_eq!(view.intro.as_deref(), Some("Four steps."));
    let first = &view.steps[0];
    let OwnerItem::Site(site) = &first.items[1] else {
        panic!("{:?}", first.items[1])
    };
    assert_eq!(site.host.as_deref(), Some("pingers.example.org"));
    assert!(site.public && !site.suggested);
    assert_eq!(site.automatic_dns.as_deref(), Some("Cloudflare"));
    assert_eq!(
        site.records,
        [DnsRecord {
            kind: "A".into(),
            name: "pingers".into(),
            zone: "example.org".into(),
            value: "203.0.113.5".into()
        }]
    );
    let states: Vec<(String, OwnerCheckState)> = checks(&first.items[1])
        .into_iter()
        .map(|(l, s, _)| (l, s))
        .collect();
    assert_eq!(
        states,
        [
            ("Internet address chosen".to_owned(), OwnerCheckState::Ok),
            ("Online on the internet".to_owned(), OwnerCheckState::Ok),
            ("Name points here".to_owned(), OwnerCheckState::Ok),
            ("Certificate".to_owned(), OwnerCheckState::Ok),
            (
                "Router lets the internet in".to_owned(),
                OwnerCheckState::Ok
            ),
        ]
    );
    assert!(
        checks(&first.items[1])[3]
            .2
            .contains("Valid until 2027-02-19"),
        "{:?}",
        checks(&first.items[1])
    );
    assert!(checks(&first.items[1])[4].2.contains("10 minutes ago"));
    assert!(
        matches!(&first.items[2], OwnerItem::Service { check, .. } if check.state == OwnerCheckState::Ok)
    );
    assert!(
        matches!(&first.items[3], OwnerItem::Domain { provider: Some(p), check, .. } if p == "Cloudflare" && check.state == OwnerCheckState::Ok)
    );
    assert_eq!(first.state, OwnerStepState::Done);
    assert_eq!(view.steps[1].state, OwnerStepState::Todo, "a waiting check");
    let missing = &view.steps[2];
    assert_eq!(missing.state, OwnerStepState::Problem);
    assert!(checks(&missing.items[0])[0]
        .2
        .contains("pingers-maps is not installed"));
    assert!(checks(&missing.items[1])[0]
        .2
        .contains("pingers-maps is not installed"));
    assert!(checks(&missing.items[2])[0]
        .2
        .contains("no site named nope"));
    assert_eq!(
        view.steps[3].state,
        OwnerStepState::Done,
        "{:?}",
        view.steps[3]
    );
    assert!(checks(&view.steps[3].items[0])[0]
        .2
        .contains("on a USB drive"));
    assert_eq!(
        view.steps[4].state,
        OwnerStepState::Done,
        "the package's own state"
    );
    assert_eq!(
        view.steps[5].state,
        OwnerStepState::Unknown,
        "auto with no checks"
    );
}

#[test]
fn dns_certificates_and_reachability_say_what_is_wrong() {
    let h = setup(Box::new(Web));
    answer(&h, FULL);
    h.sys.put("dns pingers.example.org", b"198.51.100.7\n");
    let view = page(&h);
    let site = checks(&view.steps[0].items[1]);
    assert_eq!(site[2].1, OwnerCheckState::Failed);
    assert_eq!(
        site[2].2,
        "pingers.example.org resolves to 198.51.100.7, not 203.0.113.5"
    );
    assert_eq!(
        site[4].1,
        OwnerCheckState::Unknown,
        "never checked since the start: {:?}",
        site[4]
    );
    assert_eq!(view.steps[0].state, OwnerStepState::Problem);
    h.sys.remove("dns pingers.example.org");
    let site = checks(&page(&h).steps[0].items[1]);
    assert_eq!(site[2].1, OwnerCheckState::Waiting, "no answer yet");
    // A failed certificate is a failure, shown without the escapes it came with.
    h.sys.put(
        "/s/public-hosts.toml",
        b"[pingers-server]\napi = \"broken.example.org\"\n",
    );
    let site = checks(&page(&h).steps[0].items[1]);
    assert_eq!(site[3].1, OwnerCheckState::Failed);
    assert!(!site[3].2.contains('\u{1b}'), "{:?}", site[3]);
    // No backup yet: waiting, with how to make one.
    let backup = checks(&page(&h).steps[3].items[0]);
    assert_eq!(backup[0].1, OwnerCheckState::Waiting);
    assert!(backup[0].2.contains("System › Backups"));
}

#[test]
fn without_a_web_side_or_a_chosen_address_the_checks_wait_rather_than_fail() {
    let h = setup(Box::new(crate::exposure::NoCaddyYet));
    h.sys.remove("/s/public-hosts.toml");
    h.sys.remove("/s/exposure.toml");
    answer(&h, FULL);
    let view = page(&h);
    let OwnerItem::Site(site) = &view.steps[0].items[1] else {
        panic!()
    };
    // The app's suggestion, with no domain to put it under yet.
    assert_eq!(site.host.as_deref(), Some("pingers"));
    assert!(site.suggested && !site.public && site.records.is_empty());
    let states: Vec<OwnerCheckState> = site.checks.iter().map(|c| c.state).collect();
    assert!(
        !states.contains(&OwnerCheckState::Failed),
        "{:?}",
        site.checks
    );
    assert!(
        matches!(&view.steps[0].items[3], OwnerItem::Domain { check, .. } if check.state == OwnerCheckState::Unknown)
    );
}

#[test]
fn an_app_that_does_not_answer_gets_one_problem_step_pointing_at_its_log() {
    let h = setup(Box::new(Web));
    let view = page(&h);
    assert_eq!(view.title, "Setup");
    assert_eq!(view.steps.len(), 1);
    assert_eq!(view.steps[0].state, OwnerStepState::Problem);
    let OwnerItem::Text(text) = &view.steps[0].items[0] else {
        panic!()
    };
    assert!(text.contains("Apps › pingers › Logs"), "{text}");
}

#[test]
fn a_page_breaking_the_contract_is_shown_as_a_problem_without_its_escapes() {
    let h = setup(Box::new(Web));
    for body in [
        "not json",
        r#"{"version": 2, "title": "t", "steps": []}"#,
        r#"{"version": 1, "title": "t\u001b[2J", "steps": []}"#,
        r#"{"version": 1, "title": "t", "steps": [], "shell": "rm"}"#,
        r#"{"version": 1, "title": "t", "steps": [{"title": "s", "state": "auto", "items": [{"kind": "x\u001b]52;c;eA==\u0007"}]}]}"#,
    ] {
        answer(&h, body);
        let view = page(&h);
        assert_eq!(view.steps.len(), 1, "{body}");
        assert_eq!(view.steps[0].state, OwnerStepState::Problem);
        let shown = format!("{view:?}");
        assert!(
            !shown.contains('\u{1b}') && !shown.contains('\u{7}'),
            "{shown}"
        );
    }
}

#[test]
fn the_devices_own_values_are_put_into_text_and_values() {
    let h = setup(Box::new(Web));
    h.sys
        .put("/s/bindings.toml", b"data_drive = [\"pingers-server\"]\n");
    answer(
        &h,
        r#"{"version": 1, "title": "t", "steps": [{"title": "s", "state": "todo", "items": [
            {"kind": "text", "text": "ssh root@{device_address} — {public_address} — {data_dir:pingers} {data_dir:pingers-server} {data_dir:ghost} {other} {data_dir:Bad} {"},
            {"kind": "value", "label": "Host {device_address}", "value": "scp x root@{device_address}:{data_dir:pingers}/"},
            {"kind": "check", "label": "{device_address}", "state": "ok"}
        ]}]}"#,
    );
    let items = &page(&h).steps[0].items;
    assert_eq!(
        items[0],
        OwnerItem::Text(
            "ssh root@192.168.1.20 — 203.0.113.5 — /var/lib/apps/pingers /media/monero/pingers-server unknown {other} {data_dir:Bad} {"
                .into()
        )
    );
    assert_eq!(
        items[1],
        OwnerItem::Value {
            label: "Host 192.168.1.20".into(),
            value: "scp x root@192.168.1.20:/var/lib/apps/pingers/".into()
        }
    );
    assert!(
        matches!(&items[2], OwnerItem::Check(c) if c.label == "{device_address}"),
        "only text and values"
    );
    // With nothing known yet, the words say so.
    let h = setup(Box::new(crate::exposure::NoCaddyYet));
    answer(
        &h,
        r#"{"version": 1, "title": "t", "steps": [{"title": "s", "state": "todo", "items": [{"kind": "text", "text": "{device_address} {public_address}"}]}]}"#,
    );
    assert_eq!(
        page(&h).steps[0].items[0],
        OwnerItem::Text("unknown unknown".into())
    );
}

#[test]
fn a_value_made_too_long_by_a_substitution_is_refused() {
    let h = setup(Box::new(Web));
    let long = "{data_dir:pingers}".repeat(owner_page::MAX_VALUE_LEN / 18);
    answer(
        &h,
        &format!(
            r#"{{"version": 1, "title": "t", "steps": [{{"title": "s", "state": "todo", "items": [{{"kind": "value", "label": "l", "value": "{long}"}}]}}]}}"#
        ),
    );
    let view = page(&h);
    assert_eq!(view.steps[0].state, OwnerStepState::Problem);
    assert!(format!("{view:?}").contains("after putting in this Nodo's values"));
}

#[test]
fn dates_are_days_in_utc() {
    assert_eq!(date(0), "1970-01-01");
    assert_eq!(date(951_782_400), "2000-02-29");
    assert_eq!(date(1_803_000_000), "2027-02-19");
}
