//! GoDaddy, using the v3 zone API. Docs: <https://developer.godaddy.com/openapi/domains-v3.json> and
//! <https://developer.godaddy.com/en/docs/api-users/auth>.
//!
//! This needs a Personal Access Token with `domains.dns:update` and `domains.domain:read`. The classic
//! `sso-key` credential does not work with v3 and is on its way out for v1 and v2. GoDaddy can refuse an
//! account with a 403 even when the token is valid, so the response `code` is kept for the screens to show.

use super::{
    decode, send, status_error, DnsError, ExistingRecord, IdBacked, IdRecords, ProviderMessage, RecordName,
    RecordType, Zone,
};
use crate::http::{encode_component, HttpRequest, HttpResponse, Method, SharedHttp};
use crate::secret::{Scrubber, Secret};
use serde::Deserialize;
use serde_json::json;
use std::net::IpAddr;

const HOST: &str = "https://api.godaddy.com";
const MIN_TTL: u32 = 600;
const MAX_PAGES: usize = 50;

pub struct GoDaddy {
    http: SharedHttp,
    zone: Zone,
    token: Secret,
    scrubber: Scrubber,
}

impl GoDaddy {
    pub fn new(http: SharedHttp, zone: Zone, token: Secret) -> IdBacked<Self> {
        let scrubber = Scrubber::new(&[&token]);
        IdBacked(Self { http, zone, token, scrubber })
    }

    fn base(&self) -> String {
        format!("{HOST}/v3/domains/zones/{}/dns-records", self.zone)
    }

    fn call(&self, method: Method, url: &str, body: Option<serde_json::Value>) -> Result<HttpResponse, DnsError> {
        let mut request =
            HttpRequest::new(method, url).header("Authorization", format!("Bearer {}", self.token.expose()));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = send(self.http.as_ref(), &request)?;
        if response.is_success() {
            Ok(response)
        } else {
            Err(status_error(&response, |body| self.describe(body)))
        }
    }

    fn describe(&self, body: &[u8]) -> ProviderMessage {
        let parsed: ErrorBody = serde_json::from_slice(body).unwrap_or_default();
        ProviderMessage { code: self.scrubber.clean(&parsed.code), detail: self.scrubber.clean(&parsed.message) }
    }
}

#[derive(Default, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct RecordPage {
    items: Vec<Record>,
    #[serde(default)]
    links: Vec<Link>,
}

#[derive(Deserialize)]
struct Link {
    rel: String,
    href: String,
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "recordId")]
    record_id: String,
    name: String,
    #[serde(rename = "type")]
    record_type: String,
    data: String,
}

/// The next-page link comes from the response, so it must not be able to send the token to another host.
fn next_page_url(href: &str) -> Result<String, DnsError> {
    if href.starts_with('/') && !href.starts_with("//") {
        Ok(format!("{HOST}{href}"))
    } else if href.starts_with(&format!("{HOST}/")) {
        Ok(href.to_owned())
    } else {
        Err(DnsError::Unexpected("a next-page link pointed outside the API"))
    }
}

impl IdRecords for GoDaddy {
    fn min_ttl(&self) -> u32 {
        MIN_TTL
    }

    fn list(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<ExistingRecord>, DnsError> {
        let mut url = format!(
            "{}?name={}&type={}",
            self.base(),
            encode_component(name.as_str()),
            record_type.as_str()
        );
        let mut found = Vec::new();
        // Filters are checked again here, and pages followed, because the spec's own example for a
        // filtered request shows a record that does not match. Missing ours would create a duplicate.
        for _ in 0..MAX_PAGES {
            let response = self.call(Method::Get, &url, None)?;
            let page: RecordPage = decode(&response, "GoDaddy records")?;
            found.extend(
                page.items
                    .into_iter()
                    .filter(|r| r.name == name.as_str() && record_type.matches(&r.record_type))
                    .map(|r| ExistingRecord { id: r.record_id, content: r.data }),
            );
            match page.links.iter().find(|link| link.rel == "next") {
                Some(link) => url = next_page_url(&link.href)?,
                None => return Ok(found),
            }
        }
        Err(DnsError::Unexpected("too many pages of records"))
    }

    fn create(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError> {
        let body = json!({
            "name": name.as_str(),
            "type": RecordType::for_ip(ip).as_str(),
            "data": ip.to_string(),
            "ttl": ttl,
        });
        self.call(Method::Post, &self.base(), Some(body))?;
        Ok(Vec::new())
    }

    fn update(&self, id: &str, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError> {
        // PUT replaces the whole record and all writable fields are required.
        let body = json!({
            "name": name.as_str(),
            "type": RecordType::for_ip(ip).as_str(),
            "data": ip.to_string(),
            "ttl": ttl,
        });
        self.call(Method::Put, &format!("{}/{}", self.base(), encode_component(id)), Some(body))?;
        Ok(Vec::new())
    }

    fn remove(&self, id: &str) -> Result<(), DnsError> {
        self.call(Method::Delete, &format!("{}/{}", self.base(), encode_component(id)), None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::{Change, DnsProvider};
    use crate::http::fake::{body_json, header, FakeHttp};

    const RECORDS: &str = include_str!("../../tests/fixtures/godaddy_records.json");
    const EMPTY: &str = r#"{"items":[],"links":[{"rel":"self","href":"/v3/domains/zones/example.com/dns-records"}]}"#;
    const BASE: &str = "https://api.godaddy.com/v3/domains/zones/example.com/dns-records";

    fn provider(http: std::sync::Arc<FakeHttp>) -> IdBacked<GoDaddy> {
        GoDaddy::new(http, Zone::new("example.com").unwrap(), Secret::new("gd-pat"))
    }

    fn name(value: &str) -> RecordName {
        RecordName::new(value).unwrap()
    }

    #[test]
    fn creates_with_a_bearer_token_and_raised_ttl() {
        let http = FakeHttp::new();
        http.respond(200, EMPTY).respond(201, r#"{"recordId":"rec_new"}"#);
        let outcome = provider(http.clone()).upsert(&name("www"), "203.0.113.42".parse().unwrap(), 60).unwrap();
        assert_eq!(outcome.change, Change::Created);
        assert_eq!(http.request(0).url, format!("{BASE}?name=www&type=A"));
        assert_eq!(header(&http.request(0), "Authorization"), Some("Bearer gd-pat"));
        assert_eq!(http.request(1).url, BASE);
        assert_eq!(body_json(&http.request(1)), json!({"name": "www", "type": "A", "data": "203.0.113.42", "ttl": 600}));
    }

    #[test]
    fn apex_is_encoded_and_only_matching_records_count() {
        let http = FakeHttp::new();
        http.respond(200, RECORDS);
        let found = provider(http.clone()).read(&RecordName::apex(), RecordType::A).unwrap();
        assert_eq!(http.request(0).url, format!("{BASE}?name=%40&type=A"));
        assert_eq!(found, vec!["192.0.2.1".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn updates_with_a_full_put() {
        let http = FakeHttp::new();
        http.respond(200, RECORDS).respond(200, "{}");
        provider(http.clone()).upsert(&name("app2"), "203.0.113.42".parse().unwrap(), 600).unwrap();
        let put = http.request(1);
        assert_eq!(put.method, Method::Put);
        assert_eq!(put.url, format!("{BASE}/rec_a1b2"));
        assert_eq!(body_json(&put), json!({"name": "app2", "type": "A", "data": "203.0.113.42", "ttl": 600}));
    }

    #[test]
    fn follows_next_links_on_the_same_host_only() {
        let http = FakeHttp::new();
        http.respond(200, r#"{"items":[],"links":[{"rel":"next","href":"/v3/domains/zones/example.com/dns-records?pageToken=abc"}]}"#)
            .respond(200, EMPTY);
        provider(http.clone()).read(&name("www"), RecordType::A).unwrap();
        assert_eq!(http.request(1).url, format!("{BASE}?pageToken=abc"));

        let http = FakeHttp::new();
        http.respond(200, r#"{"items":[],"links":[{"rel":"next","href":"https://evil.test/steal"}]}"#);
        assert!(matches!(provider(http.clone()).read(&name("www"), RecordType::A), Err(DnsError::Unexpected(_))));
        assert_eq!(http.requests().len(), 1);
    }

    #[test]
    fn eligibility_403_keeps_the_code_and_scrubs_the_detail() {
        let http = FakeHttp::new();
        http.respond(403, r#"{"code":"ACCESS_DENIED","message":"account not eligible, token gd-pat"}"#);
        let error = provider(http).read(&name("www"), RecordType::A).unwrap_err();
        let shown = format!("{error} {error:?}");
        assert!(matches!(error, DnsError::Forbidden { ref code, .. } if code == "ACCESS_DENIED"));
        assert!(!shown.contains("gd-pat"));
    }

    #[test]
    fn rate_limit_reports_retry_after() {
        let http = FakeHttp::new();
        http.respond_with(HttpResponse {
            status: 429,
            headers: vec![("retry-after".to_owned(), "30".to_owned())],
            body: b"{}".to_vec(),
        });
        let error = provider(http).read(&name("www"), RecordType::A).unwrap_err();
        assert_eq!(error.retry_after_secs(), Some(30));
    }
}
