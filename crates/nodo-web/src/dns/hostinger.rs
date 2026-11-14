//! Hostinger. Docs: <https://docs.hostinger.com/api-reference/endpoints/dns/zone.md> and the spec at
//! <https://developers.hostinger.com/openapi/openapi.json>.
//!
//! With `overwrite: true`, a PUT deletes and recreates only records matching both name and type, so
//! other records in the zone are safe. The endpoint returns the whole zone, so reads filter locally.

use super::{tidy_domains, 
    decode, parse_addresses, send, status_error, Change, DnsError, DnsProvider, ProviderMessage, RecordName,
    RecordType, UpsertOutcome, Zone,
};
use crate::http::{HttpRequest, HttpResponse, Method, SharedHttp};
use crate::secret::{Scrubber, Secret};
use serde::Deserialize;
use serde_json::json;
use std::net::IpAddr;

const BASE: &str = "https://developers.hostinger.com/api/dns/v1/zones";
/// The spec gives no minimum. This is a conservative floor, and a rejected value surfaces as a 422.
const MIN_TTL: u32 = 300;

pub struct Hostinger {
    http: SharedHttp,
    zone: Zone,
    token: Secret,
    scrubber: Scrubber,
}

impl Hostinger {
    pub fn new(http: SharedHttp, zone: Zone, token: Secret) -> Self {
        let scrubber = Scrubber::new(&[&token]);
        Self { http, zone, token, scrubber }
    }

    fn call(&self, method: Method, body: Option<serde_json::Value>) -> Result<HttpResponse, DnsError> {
        let mut request = HttpRequest::new(method, format!("{BASE}/{}", self.zone))
            .header("Authorization", format!("Bearer {}", self.token.expose()));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = send(self.http.as_ref(), &request)?;
        if response.is_success() {
            Ok(response)
        } else {
            Err(status_error(&response, |body| {
                let parsed: ErrorBody = serde_json::from_slice(body).unwrap_or_default();
                ProviderMessage { code: String::new(), detail: self.scrubber.clean(&parsed.message) }
            }))
        }
    }
}

#[derive(Default, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct RecordSet {
    name: String,
    #[serde(rename = "type")]
    record_type: String,
    #[serde(default)]
    records: Vec<NameRecord>,
}

#[derive(Deserialize)]
struct NameRecord {
    content: String,
    #[serde(default)]
    is_disabled: bool,
}

/// The domain portfolio. Spec: `GET /api/domains/v1/portfolio` in the OpenAPI file above.
const PORTFOLIO: &str = "https://developers.hostinger.com/api/domains/v1/portfolio";

#[derive(Deserialize)]
struct PortfolioDomain {
    /// `null` for a free domain that was never claimed.
    domain: Option<String>,
    #[serde(default)]
    status: String,
}

impl DnsProvider for Hostinger {
    fn min_ttl(&self) -> u32 {
        MIN_TTL
    }

    /// The portfolio's active domains. The spec returns the whole list without paging.
    fn list_domains(&self) -> Result<Option<Vec<String>>, DnsError> {
        let request = HttpRequest::new(Method::Get, PORTFOLIO)
            .header("Authorization", format!("Bearer {}", self.token.expose()));
        let response = send(self.http.as_ref(), &request)?;
        if !response.is_success() {
            return Err(status_error(&response, |body| {
                let parsed: ErrorBody = serde_json::from_slice(body).unwrap_or_default();
                ProviderMessage { code: String::new(), detail: self.scrubber.clean(&parsed.message) }
            }));
        }
        let listed: Vec<PortfolioDomain> = decode(&response, "Hostinger domain portfolio")?;
        Ok(Some(tidy_domains(listed.into_iter().filter(|d| d.status == "active").filter_map(|d| d.domain))))
    }

    fn read(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<IpAddr>, DnsError> {
        let response = self.call(Method::Get, None)?;
        let sets: Vec<RecordSet> = decode(&response, "Hostinger zone")?;
        let values: Vec<String> = sets
            .into_iter()
            .filter(|set| set.name == name.as_str() && record_type.matches(&set.record_type))
            .flat_map(|set| set.records)
            .filter(|record| !record.is_disabled)
            .map(|record| record.content)
            .collect();
        parse_addresses(values.iter().map(String::as_str))
    }

    fn upsert(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<UpsertOutcome, DnsError> {
        let record_type = RecordType::for_ip(ip);
        let current = self.read(name, record_type)?;
        if current == [ip] {
            return Ok(UpsertOutcome { change: Change::Unchanged, warnings: Vec::new() });
        }
        let body = json!({
            "overwrite": true,
            "zone": [{
                "name": name.as_str(),
                "type": record_type.as_str(),
                "ttl": ttl.max(MIN_TTL),
                "records": [{ "content": ip.to_string() }],
            }],
        });
        self.call(Method::Put, Some(body))?;
        let change = if current.is_empty() { Change::Created } else { Change::Updated };
        Ok(UpsertOutcome { change, warnings: Vec::new() })
    }

    fn delete(&self, name: &RecordName, record_type: RecordType) -> Result<(), DnsError> {
        let body = json!({ "filters": [{ "name": name.as_str(), "type": record_type.as_str() }] });
        self.call(Method::Delete, Some(body)).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{body_json, header, FakeHttp};

    const ZONE: &str = include_str!("../../tests/fixtures/hostinger_zone.json");
    const OK: &str = r#"{"message":"Request accepted"}"#;

    #[test]
    fn lists_active_portfolio_domains() {
        let http = FakeHttp::new();
        http.respond(200, r#"[{"id":1,"domain":"example.com","type":"domain","status":"active"},{"id":2,"domain":null,"type":"free_domain","status":"active"},{"id":3,"domain":"old.example","status":"expired"}]"#);
        let provider = Hostinger::new(http.clone(), Zone::new("example.com").unwrap(), Secret::new("hs-token"));
        assert_eq!(provider.list_domains().unwrap(), Some(vec!["example.com".to_owned()]));
        assert_eq!(http.request(0).url, "https://developers.hostinger.com/api/domains/v1/portfolio");
        assert_eq!(header(&http.request(0), "Authorization"), Some("Bearer hs-token"));
    }
    const URL: &str = "https://developers.hostinger.com/api/dns/v1/zones/example.com";

    fn provider(http: std::sync::Arc<FakeHttp>) -> Hostinger {
        Hostinger::new(http, Zone::new("example.com").unwrap(), Secret::new("hs-token"))
    }

    fn name(value: &str) -> RecordName {
        RecordName::new(value).unwrap()
    }

    #[test]
    fn read_filters_the_zone_and_skips_disabled_values() {
        let http = FakeHttp::new();
        http.respond(200, ZONE);
        let found = provider(http.clone()).read(&name("www"), RecordType::A).unwrap();
        assert_eq!(found, vec!["198.51.100.1".parse::<IpAddr>().unwrap()]);
        assert_eq!(http.request(0).url, URL);
        assert_eq!(header(&http.request(0), "Authorization"), Some("Bearer hs-token"));
    }

    #[test]
    fn upsert_overwrites_only_that_name_and_type() {
        let http = FakeHttp::new();
        http.respond(200, ZONE).respond(200, OK);
        let outcome = provider(http.clone()).upsert(&name("www"), "203.0.113.42".parse().unwrap(), 60).unwrap();
        assert_eq!(outcome.change, Change::Updated);
        let put = http.request(1);
        assert_eq!(put.method, Method::Put);
        assert_eq!(
            body_json(&put),
            json!({"overwrite": true, "zone": [{
                "name": "www", "type": "A", "ttl": 300, "records": [{"content": "203.0.113.42"}]
            }]})
        );
    }

    #[test]
    fn upsert_skips_a_correct_record() {
        let http = FakeHttp::new();
        http.respond(200, ZONE);
        let outcome = provider(http.clone()).upsert(&name("www"), "198.51.100.1".parse().unwrap(), 300).unwrap();
        assert_eq!(outcome.change, Change::Unchanged);
        assert_eq!(http.requests().len(), 1);
    }

    #[test]
    fn creates_a_missing_apex_aaaa() {
        let http = FakeHttp::new();
        http.respond(200, ZONE).respond(200, OK);
        let outcome = provider(http.clone()).upsert(&RecordName::apex(), "2001:db8::1".parse().unwrap(), 300).unwrap();
        assert_eq!(outcome.change, Change::Created);
        assert_eq!(body_json(&http.request(1))["zone"][0]["name"], "@");
    }

    #[test]
    fn delete_sends_a_filter() {
        let http = FakeHttp::new();
        http.respond(200, OK);
        provider(http.clone()).delete(&name("www"), RecordType::Aaaa).unwrap();
        let delete = http.request(0);
        assert_eq!(delete.method, Method::Delete);
        assert_eq!(body_json(&delete), json!({"filters": [{"name": "www", "type": "AAAA"}]}));
    }

    #[test]
    fn validation_errors_are_scrubbed() {
        let http = FakeHttp::new();
        http.respond(422, r#"{"message":"bad token hs-token"}"#);
        let error = provider(http).delete(&name("www"), RecordType::A).unwrap_err();
        assert!(matches!(error, DnsError::Rejected { status: 422, .. }));
        assert!(!format!("{error} {error:?}").contains("hs-token"));
    }
}
