//! Cloudflare. Docs: <https://developers.cloudflare.com/api/resources/dns/subresources/records/>.
//!
//! The API token needs Zone Read (to find the zone id) and DNS Write. New records are DNS-only, as the
//! Web flow shows the record pointing straight at the home IP. An existing record's `proxied` setting
//! is left as the user set it.

use super::{
    decode, send, status_error, DnsError, ExistingRecord, IdBacked, IdRecords, ProviderMessage, RecordName,
    RecordType, Zone,
};
use crate::http::{encode_component, HttpRequest, HttpResponse, Method, SharedHttp};
use crate::secret::{Scrubber, Secret};
use serde::Deserialize;
use serde_json::json;
use std::net::IpAddr;
use std::sync::OnceLock;

const BASE: &str = "https://api.cloudflare.com/client/v4";
const MIN_TTL: u32 = 60;

pub struct Cloudflare {
    http: SharedHttp,
    zone: Zone,
    token: Secret,
    scrubber: Scrubber,
    zone_id: OnceLock<String>,
}

impl Cloudflare {
    pub fn new(http: SharedHttp, zone: Zone, token: Secret) -> IdBacked<Self> {
        let scrubber = Scrubber::new(&[&token]);
        IdBacked(Self { http, zone, token, scrubber, zone_id: OnceLock::new() })
    }

    fn call(&self, method: Method, path: &str, body: Option<serde_json::Value>) -> Result<HttpResponse, DnsError> {
        let mut request = HttpRequest::new(method, format!("{BASE}{path}"))
            .header("Authorization", format!("Bearer {}", self.token.expose()));
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
        let parsed: Envelope<serde_json::Value> =
            serde_json::from_slice(body).unwrap_or(Envelope { result: None, errors: Vec::new() });
        let first = parsed.errors.first();
        ProviderMessage {
            code: first.map_or_else(String::new, |e| e.code.to_string()),
            detail: first.map_or_else(String::new, |e| self.scrubber.clean(&e.message)),
        }
    }

    fn zone_id(&self) -> Result<String, DnsError> {
        if let Some(id) = self.zone_id.get() {
            return Ok(id.clone());
        }
        let response = self.call(Method::Get, &format!("/zones?name={}", encode_component(self.zone.as_str())), None)?;
        let parsed: Envelope<Vec<ZoneRecord>> = decode(&response, "Cloudflare zones")?;
        let id = parsed.result.into_iter().flatten().next().ok_or(DnsError::ZoneNotFound)?.id;
        Ok(self.zone_id.get_or_init(|| id).clone())
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    result: Option<T>,
    #[serde(default)]
    errors: Vec<ApiError>,
}

#[derive(Deserialize)]
struct ApiError {
    code: serde_json::Number,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct ZoneRecord {
    id: String,
}

#[derive(Deserialize)]
struct Record {
    id: String,
    name: String,
    #[serde(rename = "type")]
    record_type: String,
    content: String,
}

impl IdRecords for Cloudflare {
    fn min_ttl(&self) -> u32 {
        MIN_TTL
    }

    fn list(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<ExistingRecord>, DnsError> {
        let fqdn = name.fqdn(&self.zone);
        let path = format!(
            "/zones/{}/dns_records?name.exact={}&type={}&per_page=100",
            encode_component(&self.zone_id()?),
            encode_component(&fqdn),
            record_type.as_str(),
        );
        let response = self.call(Method::Get, &path, None)?;
        let parsed: Envelope<Vec<Record>> = decode(&response, "Cloudflare records")?;
        Ok(parsed
            .result
            .into_iter()
            .flatten()
            .filter(|r| r.name.eq_ignore_ascii_case(&fqdn) && record_type.matches(&r.record_type))
            .map(|r| ExistingRecord { id: r.id, content: r.content })
            .collect())
    }

    fn create(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError> {
        let body = json!({
            "name": name.fqdn(&self.zone),
            "type": RecordType::for_ip(ip).as_str(),
            "content": ip.to_string(),
            "ttl": ttl,
            "proxied": false,
        });
        let path = format!("/zones/{}/dns_records", encode_component(&self.zone_id()?));
        self.call(Method::Post, &path, Some(body))?;
        Ok(Vec::new())
    }

    fn update(&self, id: &str, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError> {
        let body = json!({
            "name": name.fqdn(&self.zone),
            "type": RecordType::for_ip(ip).as_str(),
            "content": ip.to_string(),
            "ttl": ttl,
        });
        let path = format!("/zones/{}/dns_records/{}", encode_component(&self.zone_id()?), encode_component(id));
        self.call(Method::Patch, &path, Some(body))?;
        Ok(Vec::new())
    }

    fn remove(&self, id: &str) -> Result<(), DnsError> {
        let path = format!("/zones/{}/dns_records/{}", encode_component(&self.zone_id()?), encode_component(id));
        self.call(Method::Delete, &path, None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::{Change, DnsProvider};
    use crate::http::fake::{body_json, header, FakeHttp};

    const ZONES: &str = include_str!("../../tests/fixtures/cloudflare_zones.json");
    const RECORDS: &str = include_str!("../../tests/fixtures/cloudflare_records.json");
    const EMPTY: &str = r#"{"success":true,"errors":[],"messages":[],"result":[]}"#;
    const WRITTEN: &str = r#"{"success":true,"errors":[],"messages":[],"result":{"id":"372e67954025e0ba6aaa6d586b9e0b59"}}"#;
    const ZONE_ID: &str = "023e105f4ecef8ad9ca31a8372d0c353";

    fn provider(http: std::sync::Arc<FakeHttp>) -> IdBacked<Cloudflare> {
        Cloudflare::new(http, Zone::new("example.com").unwrap(), Secret::new("cf-token"))
    }

    fn name(value: &str) -> RecordName {
        RecordName::new(value).unwrap()
    }

    #[test]
    fn finds_the_zone_once_and_creates_dns_only() {
        let http = FakeHttp::new();
        http.respond(200, ZONES).respond(200, EMPTY).respond(200, WRITTEN);
        let provider = provider(http.clone());
        let outcome = provider.upsert(&name("shop"), "203.0.113.42".parse().unwrap(), 10).unwrap();
        assert_eq!(outcome.change, Change::Created);

        assert_eq!(http.request(0).url, "https://api.cloudflare.com/client/v4/zones?name=example.com");
        assert_eq!(header(&http.request(0), "Authorization"), Some("Bearer cf-token"));
        assert_eq!(
            http.request(1).url,
            format!("{BASE}/zones/{ZONE_ID}/dns_records?name.exact=shop.example.com&type=A&per_page=100")
        );
        assert_eq!(http.request(2).url, format!("{BASE}/zones/{ZONE_ID}/dns_records"));
        assert_eq!(
            body_json(&http.request(2)),
            json!({"name": "shop.example.com", "type": "A", "content": "203.0.113.42", "ttl": 60, "proxied": false})
        );
        http.assert_drained();
    }

    #[test]
    fn update_leaves_proxied_alone() {
        let http = FakeHttp::new();
        http.respond(200, ZONES).respond(200, RECORDS).respond(200, WRITTEN);
        provider(http.clone()).upsert(&name("shop"), "203.0.113.42".parse().unwrap(), 300).unwrap();
        let patch = http.request(2);
        assert_eq!(patch.method, Method::Patch);
        assert_eq!(patch.url, format!("{BASE}/zones/{ZONE_ID}/dns_records/372e67954025e0ba6aaa6d586b9e0b59"));
        assert!(body_json(&patch).get("proxied").is_none());
    }

    #[test]
    fn delete_uses_the_record_id() {
        let http = FakeHttp::new();
        http.respond(200, ZONES).respond(200, RECORDS).respond(200, WRITTEN);
        provider(http.clone()).delete(&name("shop"), RecordType::A).unwrap();
        assert_eq!(http.request(2).method, Method::Delete);
    }

    #[test]
    fn missing_zone_is_reported() {
        let http = FakeHttp::new();
        http.respond(200, EMPTY);
        assert_eq!(provider(http).read(&name("shop"), RecordType::A).unwrap_err(), DnsError::ZoneNotFound);
    }

    #[test]
    fn auth_errors_are_scrubbed() {
        let http = FakeHttp::new();
        http.respond(401, r#"{"success":false,"errors":[{"code":10000,"message":"bad token cf-token"}]}"#);
        let error = provider(http).read(&name("shop"), RecordType::A).unwrap_err();
        assert!(matches!(error, DnsError::Unauthorized(_)));
        assert!(!format!("{error} {error:?}").contains("cf-token"));
    }
}
