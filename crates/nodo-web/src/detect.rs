//! Public IP detection by agreement between independent echo services.
//!
//! One service can be wrong, compromised or intercepted, and acting on its answer would point the user's
//! domain at someone else. So an address needs votes from at least [`MIN_AGREEING`] different operators
//! and a majority of all valid answers.

use crate::http::{HttpClient, HttpRequest, Method};
use crate::ip::{classify, IpClass};
use serde::Serialize;
use std::collections::BTreeMap;
use std::net::IpAddr;

const MIN_AGREEING: usize = 2;
/// Far above the longest address, so a misbehaving service cannot make this read much.
const MAX_ANSWER_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    V4,
    V6,
}

impl Family {
    fn matches(self, ip: IpAddr) -> bool {
        matches!((self, ip), (Self::V4, IpAddr::V4(_)) | (Self::V6, IpAddr::V6(_)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IpSource {
    /// The operator. Two URLs from one operator count as a single vote.
    pub operator: String,
    pub url: String,
    pub family: Family,
}

impl IpSource {
    fn new(operator: &str, url: &str, family: Family) -> Self {
        Self { operator: operator.to_owned(), url: url.to_owned(), family }
    }
}

/// Plain-text echo services run by separate operators. Each hostname serves one address family, so the
/// connection itself fixes which address is reported.
///
/// The operator is whoever could change the answer. Cloudflare runs icanhazip, and api.ipify.org is
/// served through Cloudflare, so the two count as one vote for IPv4. api6.ipify.org is not behind
/// Cloudflare. Checked on 2026-11-08: every IPv4 URL answered 200 with a bare address, and all agreed.
/// The IPv6 hostnames have AAAA records only, but were not fetched, as the checking machine had no IPv6.
pub fn default_sources() -> Vec<IpSource> {
    vec![
        IpSource::new("cloudflare", "https://api.ipify.org", Family::V4),
        IpSource::new("cloudflare", "https://ipv4.icanhazip.com", Family::V4),
        IpSource::new("amazon", "https://checkip.amazonaws.com", Family::V4),
        IpSource::new("ident.me", "https://v4.ident.me", Family::V4),
        IpSource::new("wtfismyip", "https://ipv4.wtfismyip.com/text", Family::V4),
        IpSource::new("ipify", "https://api6.ipify.org", Family::V6),
        IpSource::new("cloudflare", "https://ipv6.icanhazip.com", Family::V6),
        IpSource::new("ident.me", "https://v6.ident.me", Family::V6),
        IpSource::new("wtfismyip", "https://ipv6.wtfismyip.com/text", Family::V6),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum VoteResult {
    Address(IpAddr),
    /// The service answered with an address that cannot be this device's public address.
    Unusable(IpClass),
    WrongFamily,
    Malformed,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Vote {
    pub operator: String,
    pub result: VoteResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Consensus {
    Agreed { ip: IpAddr, votes: usize },
    /// Enough answers came back, but they do not agree. Nothing should change.
    Disagreement,
    /// Too few sources answered, which is normal for IPv6 on an IPv4-only line.
    TooFewAnswers,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detection {
    pub family: Family,
    pub consensus: Consensus,
    pub votes: Vec<Vote>,
}

impl Detection {
    pub fn agreed_ip(&self) -> Option<IpAddr> {
        match self.consensus {
            Consensus::Agreed { ip, .. } => Some(ip),
            Consensus::Disagreement | Consensus::TooFewAnswers => None,
        }
    }
}

pub fn detect(http: &dyn HttpClient, sources: &[IpSource], family: Family) -> Detection {
    let votes: Vec<Vote> = sources
        .iter()
        .filter(|source| source.family == family)
        .map(|source| Vote { operator: source.operator.clone(), result: ask(http, source) })
        .collect();
    Detection { family, consensus: tally(&votes), votes }
}

fn ask(http: &dyn HttpClient, source: &IpSource) -> VoteResult {
    let Ok(response) = http.send(&HttpRequest::new(Method::Get, source.url.clone())) else {
        return VoteResult::Unreachable;
    };
    if response.status != 200 {
        return VoteResult::Unreachable;
    }
    if response.body.len() > MAX_ANSWER_BYTES {
        return VoteResult::Malformed;
    }
    let parsed = std::str::from_utf8(&response.body).ok().and_then(|text| text.trim().parse::<IpAddr>().ok());
    match parsed {
        None => VoteResult::Malformed,
        Some(ip) if !source.family.matches(ip) => VoteResult::WrongFamily,
        Some(ip) => match classify(ip) {
            IpClass::Global => VoteResult::Address(ip),
            class => VoteResult::Unusable(class),
        },
    }
}

fn tally(votes: &[Vote]) -> Consensus {
    let mut by_ip: BTreeMap<IpAddr, Vec<&str>> = BTreeMap::new();
    for vote in votes {
        if let VoteResult::Address(ip) = vote.result {
            let operators = by_ip.entry(ip).or_default();
            if !operators.contains(&vote.operator.as_str()) {
                operators.push(&vote.operator);
            }
        }
    }
    let total: usize = by_ip.values().map(Vec::len).sum();
    let best = by_ip.iter().max_by_key(|(_, operators)| operators.len());
    match best {
        Some((ip, operators)) if operators.len() >= MIN_AGREEING && operators.len() * 2 > total => {
            Consensus::Agreed { ip: *ip, votes: operators.len() }
        }
        _ if total >= MIN_AGREEING => Consensus::Disagreement,
        _ => Consensus::TooFewAnswers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::FakeHttp;
    use crate::http::HttpError;

    const IP_A: &str = "93.184.216.34";
    const IP_B: &str = "93.184.216.35";

    fn v4_sources(count: usize) -> Vec<IpSource> {
        (0..count).map(|i| IpSource::new(&format!("op{i}"), &format!("https://op{i}.test"), Family::V4)).collect()
    }

    fn run(answers: &[(u16, &str)]) -> Detection {
        let http = FakeHttp::new();
        for (status, body) in answers {
            http.respond(*status, body);
        }
        detect(http.as_ref(), &v4_sources(answers.len()), Family::V4)
    }

    #[test]
    fn agreement_of_all_sources_wins_and_newlines_are_ignored() {
        let detection = run(&[(200, "93.184.216.34\n"), (200, IP_A), (200, " 93.184.216.34 ")]);
        assert_eq!(detection.consensus, Consensus::Agreed { ip: IP_A.parse().unwrap(), votes: 3 });
    }

    #[test]
    fn a_single_source_is_never_trusted() {
        let http = FakeHttp::new();
        http.respond(200, IP_A).fail(HttpError::Timeout).respond(500, "");
        let detection = detect(http.as_ref(), &v4_sources(3), Family::V4);
        assert_eq!(detection.consensus, Consensus::TooFewAnswers);
        assert_eq!(detection.agreed_ip(), None);
    }

    #[test]
    fn one_lying_source_is_outvoted() {
        let detection = run(&[(200, IP_A), (200, IP_B), (200, IP_A)]);
        assert_eq!(detection.agreed_ip(), Some(IP_A.parse().unwrap()));
    }

    #[test]
    fn an_even_split_is_a_disagreement() {
        assert_eq!(run(&[(200, IP_A), (200, IP_B)]).consensus, Consensus::Disagreement);
        assert_eq!(run(&[(200, IP_A), (200, IP_A), (200, IP_B), (200, IP_B)]).consensus, Consensus::Disagreement);
    }

    #[test]
    fn the_same_operator_counts_once() {
        let sources = vec![
            IpSource::new("same", "https://a.test", Family::V4),
            IpSource::new("same", "https://b.test", Family::V4),
        ];
        let http = FakeHttp::new();
        http.respond(200, IP_A).respond(200, IP_A);
        assert_eq!(detect(http.as_ref(), &sources, Family::V4).consensus, Consensus::TooFewAnswers);
    }

    #[test]
    fn private_cgnat_and_garbage_answers_are_not_votes() {
        let detection = run(&[
            (200, "192.168.1.1"),
            (200, "100.64.0.1"),
            (200, "203.0.113.1"),
            (200, "<html>hi</html>"),
            (200, "2606:2800:220:1::1"),
            (200, &"9".repeat(500)),
        ]);
        assert_eq!(detection.consensus, Consensus::TooFewAnswers);
        assert_eq!(detection.votes[0].result, VoteResult::Unusable(IpClass::Private));
        assert_eq!(detection.votes[1].result, VoteResult::Unusable(IpClass::Cgnat));
        assert_eq!(detection.votes[2].result, VoteResult::Unusable(IpClass::Documentation));
        assert_eq!(detection.votes[3].result, VoteResult::Malformed);
        assert_eq!(detection.votes[4].result, VoteResult::WrongFamily);
        assert_eq!(detection.votes[5].result, VoteResult::Malformed);
    }

    #[test]
    fn only_sources_of_the_requested_family_are_asked() {
        let http = FakeHttp::new();
        let sources = default_sources();
        let v6_count = sources.iter().filter(|s| s.family == Family::V6).count();
        for _ in 0..v6_count {
            http.respond(200, "2606:2800:220:1::1");
        }
        let detection = detect(http.as_ref(), &sources, Family::V6);
        assert_eq!(detection.votes.len(), v6_count);
        assert!(detection.agreed_ip().is_some());
        http.assert_drained();
    }

    #[test]
    fn default_sources_have_enough_operators_per_family() {
        for family in [Family::V4, Family::V6] {
            let mut operators: Vec<_> =
                default_sources().into_iter().filter(|s| s.family == family).map(|s| s.operator).collect();
            operators.sort();
            operators.dedup();
            assert!(operators.len() >= 4, "{family:?}");
        }
    }
}
