use super::*;

const HOST: &str = "shop.example.com";

/// Lines in the shape Caddy 2.11 writes them, with the field names certmagic and acmez use.
fn line(ts: u64, logger: &str, msg: &str, extra: &str) -> String {
    format!(r#"{{"level":"info","ts":{ts}.25,"logger":"{logger}","msg":"{msg}"{extra}}}"#)
}

fn obtaining(ts: u64) -> String {
    line(ts, "tls.obtain", "obtaining certificate", &format!(r#","identifier":"{HOST}""#))
}

fn trying(ts: u64, challenge: &str) -> String {
    line(
        ts,
        "http.acme_client",
        "trying to solve challenge",
        &format!(r#","identifier":"{HOST}","challenge_type":"{challenge}","ca":"https://acme.test/dir""#),
    )
}

fn challenge_failed(ts: u64, challenge: &str, kind: &str, detail: &str) -> String {
    line(
        ts,
        "http.acme_client",
        "challenge failed",
        &format!(
            r#","identifier":"{HOST}","challenge_type":"{challenge}","problem":{{"type":"urn:ietf:params:acme:error:{kind}","title":"","detail":"{detail}","instance":"","subproblems":null}}"#
        ),
    )
}

fn failed(ts: u64, error: &str) -> String {
    line(
        ts,
        "tls.obtain",
        "could not get certificate from issuer",
        &format!(r#","identifier":"{HOST}","issuer":"acme.test-dir","error":"{error}""#),
    )
}

fn obtained(ts: u64, logger: &str) -> String {
    let msg = if logger == "tls.renew" { "certificate renewed successfully" } else { "certificate obtained successfully" };
    line(ts, logger, msg, &format!(r#","identifier":"{HOST}","issuer":"acme.test-dir""#))
}

fn cert(not_before: u64, not_after: u64) -> CertInfo {
    CertInfo {
        not_before,
        not_after,
        issuer_cn: Some("R11".to_owned()),
        issuer_org: Some("Let's Encrypt".to_owned()),
        dns_names: vec![HOST.to_owned()],
    }
}

fn status(log: &[String], cert: Option<CertInfo>, dns: &DnsView, now: u64) -> HostStatus {
    let text = log.iter().map(|l| format!("2026-11-08_10:00:00.12345 {l}")).collect::<Vec<_>>().join("\n");
    host_status(HOST, &parse_log(&text), cert, dns, now)
}

#[test]
fn parses_every_line_kind_and_skips_the_rest() {
    let text = [
        obtaining(100),
        trying(101, "http-01"),
        challenge_failed(102, "http-01", "connection", "Timeout during connect"),
        failed(103, "HTTP 400 urn:ietf:params:acme:error:connection - Timeout"),
        r#"{"level":"error","ts":104.0,"logger":"tls.obtain","msg":"will retry","error":"[shop.example.com] Obtain: boom","attempt":1,"retrying_in":60}"#.to_owned(),
        r#"{"level":"debug","ts":105.0,"logger":"events","msg":"event","name":"cert_obtained","data":{"identifier":"shop.example.com","renewal":true}}"#.to_owned(),
        r#"{"level":"info","ts":106.0,"logger":"http.log","msg":"server running"}"#.to_owned(),
        "not json at all".to_owned(),
        r#"{"ts":-5,"msg":"obtaining certificate","identifier":"x"}"#.to_owned(),
    ]
    .join("\n");
    let events = parse_log(&text);
    let kinds: Vec<&EventKind> = events.iter().map(|e| &e.kind).collect();
    assert_eq!(events.len(), 6, "{events:?}");
    assert!(matches!(kinds[0], EventKind::Obtaining { renewal: false }));
    assert!(matches!(kinds[1], EventKind::Trying { challenge } if challenge == "http-01"));
    assert!(matches!(kinds[2], EventKind::ChallengeFailed { problem_type, .. } if problem_type.ends_with(":connection")));
    assert!(matches!(kinds[3], EventKind::Failed { .. }));
    assert_eq!(events[4].host, HOST);
    assert!(matches!(kinds[5], EventKind::Obtained { renewal: true }));
    assert_eq!(events[0].at, 100);
}

#[test]
fn nothing_yet_is_waiting_and_a_start_is_obtaining() {
    assert_eq!(status(&[], None, &DnsView::default(), 1000).phase, Phase::Waiting);
    let s = status(&[obtaining(100), trying(101, "tls-alpn-01")], None, &DnsView::default(), 1000);
    assert_eq!(s.phase, Phase::Obtaining);
    assert_eq!(s.challenge.as_deref(), Some("tls-alpn-01"));
    assert_eq!(s.last_error, None);
}

#[test]
fn success_needs_a_real_certificate_in_storage() {
    // A success line alone, with nothing stored, is not reported as valid.
    let log = [obtaining(100), obtained(110, "tls.obtain")];
    assert_eq!(status(&log, None, &DnsView::default(), 1000).phase, Phase::Obtaining);
    let s = status(&log, Some(cert(105, 5000)), &DnsView::default(), 1000);
    assert_eq!(s.phase, Phase::Valid);
    assert_eq!(s.last_renewal, Some(105));
    // A certificate for another name does not count.
    let mut other = cert(105, 5000);
    other.dns_names = vec!["blog.example.com".to_owned()];
    assert_ne!(status(&log, Some(other), &DnsView::default(), 1000).phase, Phase::Valid);
}

#[test]
fn expiry_and_a_failing_renewal_are_both_visible() {
    assert_eq!(status(&[], Some(cert(100, 500)), &DnsView::default(), 1000).phase, Phase::Expired);
    let log = [
        obtaining(100),
        obtained(110, "tls.obtain"),
        line(800, "tls.renew", "renewing certificate", &format!(r#","identifier":"{HOST}""#)),
        challenge_failed(801, "dns-01", "unauthorized", "No TXT record found at _acme-challenge.shop.example.com"),
        failed(802, "HTTP 403 urn:ietf:params:acme:error:unauthorized - No TXT record found"),
    ];
    let s = status(&log, Some(cert(105, 5000)), &DnsView::default(), 1000);
    assert_eq!(s.phase, Phase::Valid);
    let error = s.last_error.unwrap();
    assert_eq!(error.kind, ProblemKind::ChallengeFailed { challenges: vec!["dns-01".to_owned()] });
    assert!(error.summary.contains("DNS-01"), "{}", error.summary);
    // A renewal that then succeeds clears it.
    let mut log = log.to_vec();
    log.push(obtained(900, "tls.renew"));
    assert_eq!(status(&log, Some(cert(890, 9000)), &DnsView::default(), 1000).last_error, None);
}

#[test]
fn a_failed_port_challenge_names_both_types_and_the_ports() {
    let log = [
        obtaining(100),
        trying(101, "tls-alpn-01"),
        challenge_failed(102, "tls-alpn-01", "connection", "Timeout during connect (likely firewall problem)"),
        trying(103, "http-01"),
        challenge_failed(104, "http-01", "connection", "Timeout during connect (likely firewall problem)"),
        failed(105, "HTTP 400 urn:ietf:params:acme:error:connection - Timeout during connect"),
    ];
    let s = status(&log, None, &DnsView::default(), 1000);
    assert_eq!(s.phase, Phase::Failed);
    let error = s.last_error.unwrap();
    assert_eq!(
        error.kind,
        ProblemKind::ChallengeFailed { challenges: vec!["http-01".to_owned(), "tls-alpn-01".to_owned()] }
    );
    assert!(error.summary.contains("HTTP-01 and TLS-ALPN-01"), "{}", error.summary);
    assert!(error.summary.contains("ports 80 and 443"), "{}", error.summary);
    assert!(error.detail.contains("Timeout"));
    // A new attempt after the failure shows as obtaining again.
    let mut again = log.to_vec();
    again.push(obtaining(200));
    assert_eq!(status(&again, None, &DnsView::default(), 1000).phase, Phase::Obtaining);
}

#[test]
fn dns_pointing_elsewhere_is_named_with_both_addresses() {
    let log = [
        obtaining(100),
        challenge_failed(102, "http-01", "connection", "203.0.113.99: Connection refused"),
        failed(105, "HTTP 400 urn:ietf:params:acme:error:connection - refused"),
    ];
    let dns = DnsView {
        resolved: Some(vec!["203.0.113.99".parse().unwrap()]),
        expected: Some("203.0.113.2".parse().unwrap()),
    };
    let error = status(&log, None, &dns, 1000).last_error.unwrap();
    assert_eq!(error.kind, ProblemKind::DnsNotPointingHere);
    assert!(error.summary.contains("203.0.113.99") && error.summary.contains("203.0.113.2"), "{}", error.summary);
    let nothing = DnsView { resolved: Some(vec![]), ..dns.clone() };
    assert!(status(&log, None, &nothing, 1000).last_error.unwrap().summary.contains("no address"));
    // When DNS does point here, the challenge itself is blamed.
    let right = DnsView { resolved: Some(vec!["203.0.113.2".parse().unwrap()]), ..dns };
    assert!(matches!(status(&log, None, &right, 1000).last_error.unwrap().kind, ProblemKind::ChallengeFailed { .. }));
}

#[test]
fn rate_limits_caa_and_provider_errors_get_their_own_reason() {
    let dns = DnsView::default();
    let cases = [
        ("HTTP 429 urn:ietf:params:acme:error:rateLimited - too many certificates (5) already issued", ProblemKind::RateLimited),
        ("HTTP 403 urn:ietf:params:acme:error:caa - CAA record for example.com prevents issuance", ProblemKind::CaaBlocked),
        (
            "[shop.example.com] solving challenges: presenting for challenge: adding temporary record for zone example.com.: 403 Forbidden",
            ProblemKind::DnsProvider,
        ),
        ("context deadline exceeded", ProblemKind::Other),
    ];
    for (error, kind) in cases {
        assert_eq!(classify(HOST, 5, error, &[], &dns).kind, kind, "{error}");
    }
    let no_record = classify(HOST, 5, "HTTP 400 urn:ietf:params:acme:error:dns - NXDOMAIN looking up A", &[], &dns);
    assert_eq!(no_record.kind, ProblemKind::DnsNotPointingHere);
}

#[test]
fn issuer_keys_match_certmagic() {
    assert_eq!(issuer_key(crate::caddy::LETS_ENCRYPT), "acme-v02.api.letsencrypt.org-directory");
    assert_eq!(issuer_key(crate::caddy::LETS_ENCRYPT_STAGING), "acme-staging-v02.api.letsencrypt.org-directory");
    assert_eq!(issuer_key("https://203.0.113.1/dir"), "203.0.113.1-dir");
    assert_eq!(issuer_key("https://acme.test:14000/dir/"), "acme.test:14000-dir");
    assert_eq!(issuer_key("https://acme.test"), "acme.test");
}
