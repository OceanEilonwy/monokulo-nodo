//! Name.com, using the current Core API. Docs: <https://docs.name.com/api/v1/reference/dns/list-records.md>
//! and the create, update and delete pages beside it.

use super::{tidy_domains, MAX_DOMAIN_PAGES, 
    decode, send, status_error, DnsError, ExistingRecord, IdBacked, IdRecords, ProviderMessage, RecordName,
    RecordType, Zone,
};
use crate::http::{encode_component, HttpRequest, HttpResponse, Method, SharedHttp};
use crate::secret::{Scrubber, Secret};
use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use std::net::IpAddr;

const BASE: &str = "https://api.name.com/core/v1/domains";
const MIN_TTL: u32 = 300;
const PER_PAGE: u32 = 1000;
const MAX_PAGES: u32 = 50;

pub struct NameCom {
    http: SharedHttp,
    zone: Zone,
    /// Base64 of `username:token`, kept as a secret since it decodes straight to the credential.
    basic: Secret,
    scrubber: Scrubber,
}

impl NameCom {
    pub fn new(http: SharedHttp, zone: Zone, username: &str, token: Secret) -> IdBacked<Self> {
        let basic = Secret::new(
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{}", token.expose())),
        );
        let scrubber = Scrubber::new(&[&token, &basic]);
        IdBacked(Self { http, zone, basic, scrubber })
    }

    fn call(&self, method: Method, url: &str, body: Option<serde_json::Value>) -> Result<HttpResponse, DnsError> {
        let mut request = HttpRequest::new(method, url).header("Authorization", format!("Basic {}", self.basic.expose()));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = send(self.http.as_ref(), &request)?;
        if response.is_success() {
            Ok(response)
        } else {
            Err(status_error(&response, |body| {
                let parsed: ErrorBody = serde_json::from_slice(body).unwrap_or_default();
                ProviderMessage {
                    code: String::new(),
                    detail: self.scrubber.clean(format!("{} {}", parsed.message, parsed.details).trim()),
                }
            }))
        }
    }

    fn records_url(&self) -> String {
        format!("{BASE}/{}/records", self.zone)
    }
}

#[derive(Default, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    details: String,
}

#[derive(Deserialize)]
struct DomainPage {
    #[serde(default)]
    domains: Vec<ListedDomain>,
    #[serde(rename = "nextPage")]
    next_page: Option<u32>,
}

#[derive(Deserialize)]
struct ListedDomain {
    #[serde(rename = "domainName")]
    domain_name: String,
}

#[derive(Deserialize)]
struct RecordPage {
    #[serde(default)]
    records: Vec<Record>,
    #[serde(rename = "nextPage")]
    next_page: Option<u32>,
}

#[derive(Deserialize)]
struct Record {
    id: u64,
    #[serde(default)]
    host: String,
    #[serde(rename = "type")]
    record_type: String,
    #[serde(default)]
    answer: String,
}

impl IdRecords for NameCom {
    fn min_ttl(&self) -> u32 {
        MIN_TTL
    }

    fn list(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<ExistingRecord>, DnsError> {
        let mut found = Vec::new();
        let mut page = 1;
        // This API has no server-side filter, so pages must be read until the last.
        for _ in 0..MAX_PAGES {
            let url = format!("{}?perPage={PER_PAGE}&page={page}", self.records_url());
            let response = self.call(Method::Get, &url, None)?;
            let parsed: RecordPage = decode(&response, "Name.com records")?;
            found.extend(
                parsed
                    .records
                    .into_iter()
                    // The docs allow an apex host to be either an empty string or `@`.
                    .filter(|r| {
                        (r.host == name.relative() || (name.is_apex() && r.host == super::APEX))
                            && record_type.matches(&r.record_type)
                    })
                    .map(|r| ExistingRecord { id: r.id.to_string(), content: r.answer }),
            );
            match parsed.next_page {
                Some(next) if next > page => page = next,
                Some(_) => return Err(DnsError::Unexpected("the next page number did not advance")),
                None => return Ok(found),
            }
        }
        Err(DnsError::Unexpected("too many pages of records"))
    }

    fn create(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError> {
        let body = json!({
            "host": name.as_str(),
            "type": RecordType::for_ip(ip).as_str(),
            "answer": ip.to_string(),
            "ttl": ttl,
        });
        self.call(Method::Post, &self.records_url(), Some(body))?;
        Ok(Vec::new())
    }

    fn update(&self, id: &str, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError> {
        let body = json!({
            "host": name.as_str(),
            "type": RecordType::for_ip(ip).as_str(),
            "answer": ip.to_string(),
            "ttl": ttl,
        });
        let url = format!("{}/{}", self.records_url(), encode_component(id));
        self.call(Method::Put, &url, Some(body))?;
        Ok(Vec::new())
    }

    fn remove(&self, id: &str) -> Result<(), DnsError> {
        let url = format!("{}/{}", self.records_url(), encode_component(id));
        self.call(Method::Delete, &url, None).map(|_| ())
    }

    /// `GET /core/v1/domains`, following `nextPage`. Docs:
    /// <https://docs.name.com/api/v1/reference/domains/list-domains.md>.
    fn list_domains(&self) -> Result<Option<Vec<String>>, DnsError> {
        let mut names = Vec::new();
        let mut page = 1;
        for _ in 0..MAX_DOMAIN_PAGES {
            let response = self.call(Method::Get, &format!("{BASE}?perPage={PER_PAGE}&page={page}"), None)?;
            let parsed: DomainPage = decode(&response, "Name.com domain list")?;
            names.extend(parsed.domains.into_iter().map(|d| d.domain_name));
            match parsed.next_page {
                Some(next) if next > page => page = next,
                _ => break,
            }
        }
        Ok(Some(tidy_domains(names)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::{Change, DnsProvider};
    use crate::http::fake::{body_json, header, FakeHttp};

    const RECORDS: &str = include_str!("../../tests/fixtures/namecom_records.json");
    const EMPTY: &str = r#"{"records":[],"from":0,"to":0,"totalCount":0}"#;
    const RECORD: &str = r#"{"id":12345,"domainName":"example.org","host":"blog","type":"A","answer":"192.0.2.10","ttl":300}"#;
    const URL: &str = "https://api.name.com/core/v1/domains/example.org/records";

    #[test]
    fn lists_domains_following_next_page() {
        let http = FakeHttp::new();
        http.respond(200, r#"{"domains":[{"domainName":"example.org"}],"nextPage":2,"lastPage":2,"totalCount":2}"#);
        http.respond(200, r#"{"domains":[{"domainName":"example.net"}],"totalCount":2}"#);
        let provider = NameCom::new(http.clone(), Zone::new("example.org").unwrap(), "alice", Secret::new("nc-token"));
        assert_eq!(provider.list_domains().unwrap(), Some(vec!["example.net".to_owned(), "example.org".to_owned()]));
        assert_eq!(http.request(0).url, "https://api.name.com/core/v1/domains?perPage=1000&page=1");
        assert_eq!(http.request(1).url, "https://api.name.com/core/v1/domains?perPage=1000&page=2");
    }

    fn provider(http: std::sync::Arc<FakeHttp>) -> IdBacked<NameCom> {
        NameCom::new(http, Zone::new("example.org").unwrap(), "alice", Secret::new("nc-token"))
    }

    fn name(value: &str) -> RecordName {
        RecordName::new(value).unwrap()
    }

    #[test]
    fn creates_using_basic_auth() {
        let http = FakeHttp::new();
        http.respond(200, EMPTY).respond(200, RECORD);
        let outcome = provider(http.clone()).upsert(&name("blog"), "192.0.2.10".parse().unwrap(), 60).unwrap();
        assert_eq!(outcome.change, Change::Created);
        assert_eq!(http.request(0).url, format!("{URL}?perPage=1000&page=1"));
        // alice:nc-token
        assert_eq!(header(&http.request(0), "Authorization"), Some("Basic YWxpY2U6bmMtdG9rZW4="));
        assert_eq!(
            body_json(&http.request(1)),
            json!({"host": "blog", "type": "A", "answer": "192.0.2.10", "ttl": 300})
        );
    }

    #[test]
    fn follows_pages_and_filters_locally() {
        let http = FakeHttp::new();
        http.respond(200, r#"{"records":[],"from":0,"to":0,"totalCount":3,"nextPage":2,"lastPage":2}"#).respond(200, RECORDS);
        let found = provider(http.clone()).read(&name("blog"), RecordType::A).unwrap();
        assert_eq!(found, vec!["198.51.100.1".parse::<IpAddr>().unwrap()]);
        assert_eq!(http.request(1).url, format!("{URL}?perPage=1000&page=2"));
    }

    #[test]
    fn apex_matches_either_documented_spelling() {
        let http = FakeHttp::new();
        http.respond(200, RECORDS);
        let found = provider(http).read(&RecordName::apex(), RecordType::A).unwrap();
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn updates_and_deletes_by_id() {
        let http = FakeHttp::new();
        http.respond(200, RECORDS).respond(200, RECORD);
        provider(http.clone()).upsert(&name("blog"), "192.0.2.10".parse().unwrap(), 300).unwrap();
        assert_eq!(http.request(1).method, Method::Put);
        assert_eq!(http.request(1).url, format!("{URL}/101"));

        let http = FakeHttp::new();
        http.respond(200, RECORDS).respond(204, "");
        provider(http.clone()).delete(&name("blog"), RecordType::A).unwrap();
        assert_eq!(http.request(1).method, Method::Delete);
        assert_eq!(http.request(1).url, format!("{URL}/101"));
    }

    #[test]
    fn errors_are_scrubbed() {
        let http = FakeHttp::new();
        http.respond(401, r#"{"message":"Unauthorized","details":"token nc-token invalid"}"#);
        let error = provider(http).read(&name("blog"), RecordType::A).unwrap_err();
        assert!(matches!(error, DnsError::Unauthorized(_)));
        assert!(!format!("{error} {error:?}").contains("nc-token"));
    }
}
