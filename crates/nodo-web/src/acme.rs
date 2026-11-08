//! Certificate state for the Web screens, from what Caddy keeps and logs.
//!
//! Caddy stores each certificate as PEM under `certificates/<issuer>/<host>/` and, through the
//! `nodo_tls` log sink [`crate::caddy`] configures, writes JSON lines when it starts obtaining a
//! certificate, tries a challenge, fails or succeeds. This module turns both into one status per
//! host with a plain reason for the last failure. It is pure, so the helper reads the files and
//! the DNS answers and hands them in.

use std::collections::BTreeSet;
use std::net::IpAddr;

use crate::cert::CertInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    Obtaining { renewal: bool },
    Obtained { renewal: bool },
    /// One challenge attempt began.
    Trying { challenge: String },
    ChallengeFailed { challenge: String, problem_type: String, detail: String },
    /// Getting the certificate failed. Caddy retries on its own.
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Unix seconds.
    pub at: u64,
    pub host: String,
    pub kind: EventKind,
}

/// Reads Caddy's JSON log lines. Lines that are not JSON or not about certificates are skipped.
pub fn parse_log(text: &str) -> Vec<Event> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<Event> {
    let start = line.find('{')?;
    let value: serde_json::Value = serde_json::from_str(&line[start..]).ok()?;
    let at = value["ts"].as_f64().filter(|t| *t >= 0.0)? as u64;
    let logger = value["logger"].as_str().unwrap_or_default();
    let msg = value["msg"].as_str().unwrap_or_default();
    let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let identifier = text("identifier");
    let renewal = logger.ends_with(".renew") || logger == "tls.renew";
    let (host, kind) = match msg {
        "obtaining certificate" | "renewing certificate" => (identifier, EventKind::Obtaining { renewal }),
        "certificate obtained successfully" | "certificate renewed successfully" => {
            (identifier, EventKind::Obtained { renewal })
        }
        "could not get certificate from issuer" => (identifier, EventKind::Failed { error: text("error") }),
        "will retry" | "final attempt; giving up" => {
            // These carry no identifier field, but the error starts with "[host] Obtain:".
            let error = text("error");
            let host = error.strip_prefix('[')?.split_once(']')?.0.to_owned();
            (host, EventKind::Failed { error })
        }
        "trying to solve challenge" => (identifier, EventKind::Trying { challenge: text("challenge_type") }),
        "challenge failed" => {
            let problem = &value["problem"];
            (
                identifier,
                EventKind::ChallengeFailed {
                    challenge: text("challenge_type"),
                    problem_type: problem["type"].as_str().unwrap_or_default().to_owned(),
                    detail: problem["detail"].as_str().unwrap_or_default().to_owned(),
                },
            )
        }
        "event" if logger == "events" => {
            let data = &value["data"];
            let host = data["identifier"].as_str()?.to_owned();
            let renewal = data["renewal"].as_bool().unwrap_or(false);
            match value["name"].as_str()? {
                "cert_obtaining" => (host, EventKind::Obtaining { renewal }),
                "cert_obtained" => (host, EventKind::Obtained { renewal }),
                _ => return None,
            }
        }
        _ => return None,
    };
    (!host.is_empty()).then_some(Event { at, host, kind })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Nothing has happened for this host yet.
    Waiting,
    Obtaining,
    /// A certificate from the expected issuer covers the host and has not expired.
    Valid,
    Failed,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemKind {
    ChallengeFailed { challenges: Vec<String> },
    RateLimited,
    DnsNotPointingHere,
    CaaBlocked,
    /// The DNS provider refused to create the DNS-01 record.
    DnsProvider,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub at: u64,
    pub kind: ProblemKind,
    /// One plain sentence for the screens.
    pub summary: String,
    /// What the CA or Caddy said, for the log view.
    pub detail: String,
}

/// What the device's resolver says about a host, next to the address it should have.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DnsView {
    /// `None` when the lookup failed outright.
    pub resolved: Option<Vec<IpAddr>>,
    pub expected: Option<IpAddr>,
}

impl DnsView {
    fn mismatch(&self) -> Option<String> {
        let expected = self.expected?;
        match self.resolved.as_deref()? {
            [] => Some("has no address in DNS".to_owned()),
            found if found.contains(&expected) => None,
            found => Some(format!(
                "points to {}, not to this device's public address {expected}",
                found.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStatus {
    pub host: String,
    pub phase: Phase,
    pub cert: Option<CertInfo>,
    /// When the certificate in use was issued, by its own validity start.
    pub last_renewal: Option<u64>,
    /// The challenge type last tried for this host.
    pub challenge: Option<String>,
    /// The latest failure since the last success. Kept while a valid certificate is in place, so a
    /// failing renewal shows before the certificate runs out.
    pub last_error: Option<Problem>,
}

/// Works out one host's status. `cert` is the newest stored certificate from the configured
/// issuer, if any.
pub fn host_status(host: &str, events: &[Event], cert: Option<CertInfo>, dns: &DnsView, now: u64) -> HostStatus {
    let mine: Vec<&Event> = events.iter().filter(|e| e.host.eq_ignore_ascii_case(host)).collect();
    let last_success = mine.iter().filter(|e| matches!(e.kind, EventKind::Obtained { .. })).map(|e| e.at).max();
    let last_start = mine.iter().filter(|e| matches!(e.kind, EventKind::Obtaining { .. })).map(|e| e.at).max();
    let challenge = mine.iter().rev().find_map(|e| match &e.kind {
        EventKind::Trying { challenge } | EventKind::ChallengeFailed { challenge, .. } => Some(challenge.clone()),
        _ => None,
    });
    let since_success: Vec<&&Event> = mine.iter().filter(|e| last_success.is_none_or(|s| e.at > s)).collect();
    let last_error = since_success.iter().rev().find(|e| matches!(e.kind, EventKind::Failed { .. })).map(|failed| {
        let EventKind::Failed { error } = &failed.kind else { unreachable!() };
        // The challenge failures of the same attempt explain the final error best.
        let attempt_start = mine
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Obtaining { .. }) && e.at <= failed.at)
            .map(|e| e.at)
            .max()
            .unwrap_or(0);
        let failures: Vec<(&String, &String, &String)> = since_success
            .iter()
            .filter(|e| e.at >= attempt_start && e.at <= failed.at)
            .filter_map(|e| match &e.kind {
                EventKind::ChallengeFailed { challenge, problem_type, detail } => Some((challenge, problem_type, detail)),
                _ => None,
            })
            .collect();
        classify(host, failed.at, error, &failures, dns)
    });
    let valid_cert = cert.as_ref().filter(|c| c.covers(host) && c.not_after > now && c.not_before <= now + 300);
    let phase = if valid_cert.is_some() {
        Phase::Valid
    } else if last_error.as_ref().is_some_and(|e| last_start.is_none_or(|s| e.at >= s)) {
        Phase::Failed
    } else if last_start.is_some() {
        Phase::Obtaining
    } else if cert.as_ref().is_some_and(|c| c.covers(host) && c.not_after <= now) {
        Phase::Expired
    } else {
        Phase::Waiting
    };
    HostStatus {
        host: host.to_owned(),
        phase,
        last_renewal: valid_cert.map(|c| c.not_before),
        cert,
        challenge,
        last_error,
    }
}

fn pretty_challenge(challenge: &str) -> String {
    match challenge {
        "http-01" => "HTTP-01".to_owned(),
        "tls-alpn-01" => "TLS-ALPN-01".to_owned(),
        "dns-01" => "DNS-01".to_owned(),
        other => other.to_owned(),
    }
}

/// Turns an ACME failure into a reason a person can act on.
pub fn classify(
    host: &str,
    at: u64,
    error: &str,
    challenge_failures: &[(&String, &String, &String)],
    dns: &DnsView,
) -> Problem {
    let all_text = std::iter::once(error.to_owned())
        .chain(challenge_failures.iter().map(|(_, kind, detail)| format!("{kind} {detail}")))
        .collect::<Vec<_>>()
        .join(" ");
    let lower = all_text.to_ascii_lowercase();
    let detail = challenge_failures
        .last()
        .map(|(_, _, detail)| (*detail).clone())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| error.to_owned());
    let problem = |kind, summary: String| Problem { at, kind, summary, detail: detail.chars().take(500).collect() };
    let challenges: Vec<String> = challenge_failures
        .iter()
        .map(|(c, _, _)| (*c).clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let port_challenges = challenges.iter().any(|c| c == "http-01" || c == "tls-alpn-01")
        || (challenges.is_empty() && !lower.contains("dns-01"));

    if lower.contains("ratelimited") || lower.contains("rate limit") || lower.contains("too many certificates") || lower.contains("too many failed") || lower.contains("too many new orders") || lower.contains("http 429") {
        return problem(
            ProblemKind::RateLimited,
            "Let's Encrypt is rate limiting this name. Caddy tries again later on its own.".to_owned(),
        );
    }
    if lower.contains("acme:error:caa") || lower.contains("caa record") {
        return problem(
            ProblemKind::CaaBlocked,
            format!("A CAA record on the domain forbids Let's Encrypt for {host}. Allow letsencrypt.org in it, or remove it."),
        );
    }
    if lower.contains("presenting for challenge") || lower.contains("temporary record") || lower.contains("dns provider") {
        return problem(
            ProblemKind::DnsProvider,
            "The DNS provider refused the challenge record. Check its API credentials and that they may edit this domain."
                .to_owned(),
        );
    }
    if port_challenges {
        if let Some(mismatch) = dns.mismatch() {
            return problem(ProblemKind::DnsNotPointingHere, format!("DNS for {host} {mismatch}."));
        }
        if lower.contains("acme:error:dns") && !challenges.iter().any(|c| c == "dns-01") {
            return problem(ProblemKind::DnsNotPointingHere, format!("The CA found no usable DNS record for {host}."));
        }
    }
    if !challenges.is_empty() {
        let names = challenges.iter().map(|c| pretty_challenge(c)).collect::<Vec<_>>().join(" and ");
        let why = if challenges.iter().all(|c| c == "dns-01") {
            "the CA could not see the challenge record in DNS".to_owned()
        } else if lower.contains("acme:error:connection") || lower.contains("timeout") || lower.contains("connection refused") {
            "the CA could not connect to ports 80 and 443 at this address. Check the port forwarding".to_owned()
        } else {
            "the CA got the wrong answer from this address".to_owned()
        };
        return problem(
            ProblemKind::ChallengeFailed { challenges },
            format!("The {names} challenge failed: {why}."),
        );
    }
    problem(ProblemKind::Other, "Getting a certificate failed. Caddy tries again later on its own.".to_owned())
}

/// Caddy's storage key for an ACME directory, as certmagic derives it: the host, then the path
/// with slashes turned into dashes.
pub fn issuer_key(directory: &str) -> String {
    let rest = directory.split_once("://").map_or(directory, |(_, rest)| rest);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = path.replace(['/', '\\'], "-");
    let path = path.trim_matches('-');
    if path.is_empty() {
        host.to_owned()
    } else {
        format!("{host}-{path}")
    }
}

#[cfg(test)]
mod tests;
