//! Gandi LiveDNS. Docs: <https://api.gandi.net/docs/livedns/>.
//!
//! LiveDNS addresses a whole record set by name and type, so a PUT replaces exactly the values this
//! crate owns and nothing else. It uses a personal access token, as the older `Apikey` scheme is deprecated.

use super::{tidy_domains, MAX_DOMAIN_PAGES, 
    decode, parse_addresses, send, status_error, Change, DnsError, DnsProvider, ProviderMessage, RecordName,
    RecordType, UpsertOutcome, Zone,
};
use crate::http::{HttpRequest, HttpResponse, Method, SharedHttp};
use crate::secret::{Scrubber, Secret};
use serde::Deserialize;
use serde_json::json;
use std::net::IpAddr;

const BASE: &str = "https://api.gandi.net/v5/livedns";
const MIN_TTL: u32 = 300;
const MAX_TTL: u32 = 2_592_000;

pub struct Gandi {
    http: SharedHttp,
    zone: Zone,
    token: Secret,
    scrubber: Scrubber,
}

impl Gandi {
    pub fn new(http: SharedHttp, zone: Zone, token: Secret) -> Self {
        let scrubber = Scrubber::new(&[&token]);
        Self { http, zone, token, scrubber }
    }

    fn url(&self, name: &RecordName, record_type: RecordType) -> String {
        format!("{BASE}/domains/{}/records/{}/{}", self.zone, name, record_type.as_str())
    }

    fn send(&self, method: Method, url: &str, body: Option<serde_json::Value>) -> Result<HttpResponse, DnsError> {
        let mut request =
            HttpRequest::new(method, url).header("Authorization", format!("Bearer {}", self.token.expose()));
        if let Some(body) = body {
            request = request.json(&body);
        }
        send(self.http.as_ref(), &request)
    }

    fn failure(&self, response: &HttpResponse) -> DnsError {
        status_error(response, |body| {
            let parsed: ErrorBody = serde_json::from_slice(body).unwrap_or_default();
            ProviderMessage { code: self.scrubber.clean(&parsed.code), detail: self.scrubber.clean(&parsed.message) }
        })
    }
}

#[derive(Default, Deserialize)]
struct ErrorBody {
    #[serde(default, deserialize_with = "code_as_string")]
    code: String,
    #[serde(default)]
    message: String,
}

fn code_as_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(text) => text,
        other => other.to_string(),
    })
}

#[derive(Deserialize)]
struct RecordSet {
    rrset_values: Vec<String>,
}

/// Gandi pages lists with `page` and `per_page`.
const DOMAIN_PAGE: usize = 100;

#[derive(Deserialize)]
struct ListedDomain {
    fqdn: String,
}

impl DnsProvider for Gandi {
    fn min_ttl(&self) -> u32 {
        MIN_TTL
    }

    /// `GET /livedns/domains`: the domains on LiveDNS that the token can manage.
    fn list_domains(&self) -> Result<Option<Vec<String>>, DnsError> {
        let mut names = Vec::new();
        for page in 1..=MAX_DOMAIN_PAGES {
            let url = format!("{BASE}/domains?per_page={DOMAIN_PAGE}&page={page}");
            let response = self.send(Method::Get, &url, None)?;
            if !response.is_success() {
                return Err(self.failure(&response));
            }
            let listed: Vec<ListedDomain> = decode(&response, "Gandi domain list")?;
            let count = listed.len();
            names.extend(listed.into_iter().map(|d| d.fqdn));
            if count < DOMAIN_PAGE {
                break;
            }
        }
        Ok(Some(tidy_domains(names)))
    }

    fn read(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<IpAddr>, DnsError> {
        let response = self.send(Method::Get, &self.url(name, record_type), None)?;
        match response.status {
            404 => Ok(Vec::new()),
            _ if response.is_success() => {
                let set: RecordSet = decode(&response, "Gandi record")?;
                parse_addresses(set.rrset_values.iter().map(String::as_str))
            }
            _ => Err(self.failure(&response)),
        }
    }

    fn upsert(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<UpsertOutcome, DnsError> {
        let record_type = RecordType::for_ip(ip);
        let current = self.read(name, record_type)?;
        if current == [ip] {
            return Ok(UpsertOutcome { change: Change::Unchanged, warnings: Vec::new() });
        }
        let body = json!({ "rrset_values": [ip.to_string()], "rrset_ttl": ttl.clamp(MIN_TTL, MAX_TTL) });
        let response = self.send(Method::Put, &self.url(name, record_type), Some(body))?;
        if !response.is_success() {
            return Err(self.failure(&response));
        }
        let change = if current.is_empty() { Change::Created } else { Change::Updated };
        Ok(UpsertOutcome { change, warnings: Vec::new() })
    }

    fn delete(&self, name: &RecordName, record_type: RecordType) -> Result<(), DnsError> {
        let response = self.send(Method::Delete, &self.url(name, record_type), None)?;
        if response.is_success() || response.status == 404 {
            Ok(())
        } else {
            Err(self.failure(&response))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::fake::{body_json, header, FakeHttp};

    const RECORD: &str = include_str!("../../tests/fixtures/gandi_record.json");
    const CREATED: &str = r#"{"message":"DNS Record Created"}"#;

    #[test]
    fn lists_livedns_domains() {
        let http = FakeHttp::new();
        http.respond(200, r#"[{"fqdn":"example.com","domain_records_href":"x"},{"fqdn":"example.net"}]"#);
        let provider = Gandi::new(http.clone(), Zone::new("example.com").unwrap(), Secret::new("pat_abc"));
        assert_eq!(provider.list_domains().unwrap(), Some(vec!["example.com".to_owned(), "example.net".to_owned()]));
        assert_eq!(http.request(0).url, "https://api.gandi.net/v5/livedns/domains?per_page=100&page=1");
        assert_eq!(header(&http.request(0), "Authorization"), Some("Bearer pat_abc"));
    }

    #[test]
    fn a_refused_listing_is_unauthorized() {
        let http = FakeHttp::new();
        http.respond(401, r#"{"code":401,"message":"The server could not verify that you are authorized"}"#);
        let provider = Gandi::new(http, Zone::new("example.com").unwrap(), Secret::new("pat_abc"));
        assert!(matches!(provider.list_domains(), Err(DnsError::Unauthorized(_))));
    }
    const URL: &str = "https://api.gandi.net/v5/livedns/domains/example.com/records/www/A";

    fn provider(http: std::sync::Arc<FakeHttp>) -> Gandi {
        Gandi::new(http, Zone::new("example.com").unwrap(), Secret::new("pat_abc"))
    }

    fn name(value: &str) -> RecordName {
        RecordName::new(value).unwrap()
    }

    #[test]
    fn creates_when_the_record_is_missing() {
        let http = FakeHttp::new();
        http.respond(404, r#"{"code":404,"message":"Record not found"}"#).respond(201, CREATED);
        let outcome = provider(http.clone()).upsert(&name("www"), "203.0.113.42".parse().unwrap(), 60).unwrap();
        assert_eq!(outcome.change, Change::Created);
        assert_eq!(header(&http.request(0), "Authorization"), Some("Bearer pat_abc"));
        let put = http.request(1);
        assert_eq!(put.method, Method::Put);
        assert_eq!(put.url, URL);
        assert_eq!(body_json(&put), json!({"rrset_values": ["203.0.113.42"], "rrset_ttl": 300}));
    }

    #[test]
    fn replaces_a_multi_value_set_and_skips_a_correct_one() {
        let http = FakeHttp::new();
        http.respond(200, RECORD).respond(201, CREATED);
        let outcome = provider(http).upsert(&name("www"), "192.0.2.10".parse().unwrap(), 300).unwrap();
        assert_eq!(outcome.change, Change::Updated);

        let http = FakeHttp::new();
        http.respond(200, r#"{"rrset_name":"www","rrset_type":"A","rrset_values":["192.0.2.10"]}"#);
        let outcome = provider(http.clone()).upsert(&name("www"), "192.0.2.10".parse().unwrap(), 300).unwrap();
        assert_eq!(outcome.change, Change::Unchanged);
        assert_eq!(http.requests().len(), 1);
    }

    #[test]
    fn apex_uses_the_at_sign() {
        let http = FakeHttp::new();
        http.respond(404, "{}");
        provider(http.clone()).read(&RecordName::apex(), RecordType::Aaaa).unwrap();
        assert_eq!(http.request(0).url, "https://api.gandi.net/v5/livedns/domains/example.com/records/@/AAAA");
    }

    #[test]
    fn delete_treats_404_as_done() {
        let http = FakeHttp::new();
        http.respond(204, "").respond(404, "{}");
        let provider = provider(http.clone());
        provider.delete(&name("www"), RecordType::A).unwrap();
        provider.delete(&name("www"), RecordType::A).unwrap();
        assert_eq!(http.request(0).method, Method::Delete);
    }

    #[test]
    fn expired_token_is_a_scrubbed_forbidden_error() {
        let http = FakeHttp::new();
        http.respond(403, r#"{"code":403,"message":"token pat_abc expired","object":"HTTPForbidden","cause":"Forbidden"}"#);
        let error = provider(http).read(&name("www"), RecordType::A).unwrap_err();
        assert!(matches!(error, DnsError::Forbidden { .. }));
        assert!(!format!("{error} {error:?}").contains("pat_abc"));
    }
}
