//! Porkbun. Docs: <https://porkbun.com/llms/dns> and <https://porkbun.com/api/json/v3/spec>.
//!
//! The domain must have API access switched on in the Porkbun account. Keys go in headers rather than
//! the JSON body, so reads can use GET and bodies never hold a credential.

use super::{
    clean_warnings, decode, send, status_error, DnsError, ExistingRecord, IdBacked, IdRecords, ProviderMessage,
    RecordName, RecordType, Zone,
};
use crate::http::{HttpRequest, HttpResponse, Method, SharedHttp};
use crate::secret::{Scrubber, Secret};
use serde::Deserialize;
use serde_json::json;
use std::net::IpAddr;

const BASE: &str = "https://api.porkbun.com/api/json/v3";
/// The docs say the minimum comes from account settings and is typically 600.
const MIN_TTL: u32 = 600;

pub struct Porkbun {
    http: SharedHttp,
    zone: Zone,
    api_key: Secret,
    secret_key: Secret,
    scrubber: Scrubber,
}

impl Porkbun {
    pub fn new(http: SharedHttp, zone: Zone, api_key: Secret, secret_key: Secret) -> IdBacked<Self> {
        let scrubber = Scrubber::new(&[&api_key, &secret_key]);
        IdBacked(Self { http, zone, api_key, secret_key, scrubber })
    }

    fn call(&self, method: Method, path: &str, body: Option<serde_json::Value>) -> Result<HttpResponse, DnsError> {
        let mut request = HttpRequest::new(method, format!("{BASE}{path}"))
            .header("X-API-Key", self.api_key.expose())
            .header("X-Secret-API-Key", self.secret_key.expose());
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = send(self.http.as_ref(), &request)?;
        let status = serde_json::from_slice::<Status>(&response.body).ok();
        // Every documented response has a status field, and an error can carry either HTTP status.
        if response.is_success() && status.is_none_or(|s| s.status == "SUCCESS") {
            Ok(response)
        } else {
            Err(status_error(&response, |body| self.describe(body)))
        }
    }

    fn describe(&self, body: &[u8]) -> ProviderMessage {
        let parsed: ErrorBody = serde_json::from_slice(body).unwrap_or_default();
        ProviderMessage { code: self.scrubber.clean(&parsed.code), detail: self.scrubber.clean(&parsed.message) }
    }

    fn write_warnings(&self, response: &HttpResponse) -> Result<Vec<String>, DnsError> {
        let parsed: WriteResponse = decode(response, "Porkbun write response")?;
        Ok(clean_warnings(parsed.warnings, &self.scrubber))
    }
}

#[derive(Deserialize)]
struct Status {
    status: String,
}

#[derive(Default, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct RetrieveResponse {
    records: Vec<Record>,
}

#[derive(Deserialize)]
struct Record {
    id: String,
    #[serde(rename = "type")]
    record_type: String,
    content: String,
}

#[derive(Deserialize)]
struct WriteResponse {
    #[serde(default)]
    warnings: Vec<String>,
}

impl IdRecords for Porkbun {
    fn min_ttl(&self) -> u32 {
        MIN_TTL
    }

    fn list(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<ExistingRecord>, DnsError> {
        // The docs make the subdomain optional, and leaving it out means the root.
        let path = format!("/dns/retrieveByNameType/{}/{}/{}", self.zone, record_type.as_str(), name.relative());
        let response = self.call(Method::Get, path.trim_end_matches('/'), None)?;
        let parsed: RetrieveResponse = decode(&response, "Porkbun records")?;
        Ok(parsed
            .records
            .into_iter()
            .filter(|r| record_type.matches(&r.record_type))
            .map(|r| ExistingRecord { id: r.id, content: r.content })
            .collect())
    }

    fn create(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError> {
        let body = json!({
            "type": RecordType::for_ip(ip).as_str(),
            "name": name.relative(),
            "content": ip.to_string(),
            "ttl": ttl.to_string(),
        });
        let response = self.call(Method::Post, &format!("/dns/create/{}", self.zone), Some(body))?;
        self.write_warnings(&response)
    }

    fn update(&self, id: &str, _name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError> {
        // Omitted fields keep their values on this endpoint, so notes and the name survive.
        let body = json!({ "content": ip.to_string(), "ttl": ttl.to_string() });
        let path = format!("/dns/edit/{}/{}", self.zone, crate::http::encode_component(id));
        let response = self.call(Method::Post, &path, Some(body))?;
        self.write_warnings(&response)
    }

    fn remove(&self, id: &str) -> Result<(), DnsError> {
        let path = format!("/dns/delete/{}/{}", self.zone, crate::http::encode_component(id));
        self.call(Method::Post, &path, None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::{Change, DnsProvider};
    use crate::http::fake::{body_json, header, FakeHttp};

    const RECORDS: &str = include_str!("../../tests/fixtures/porkbun_records.json");
    const EMPTY: &str = r#"{"status":"SUCCESS","cloudflare":"disabled","records":[]}"#;
    const OK: &str = r#"{"status":"SUCCESS"}"#;

    fn provider(http: std::sync::Arc<FakeHttp>) -> IdBacked<Porkbun> {
        Porkbun::new(http, Zone::new("example.com").unwrap(), Secret::new("pk1_key"), Secret::new("sk1_key"))
    }

    fn name(value: &str) -> RecordName {
        RecordName::new(value).unwrap()
    }

    #[test]
    fn creates_a_record_with_headers_not_body_credentials() {
        let http = FakeHttp::new();
        http.respond(200, EMPTY).respond(200, r#"{"status":"SUCCESS","id":123}"#);
        let outcome = provider(http.clone()).upsert(&name("www"), "203.0.113.42".parse().unwrap(), 60).unwrap();
        assert_eq!(outcome.change, Change::Created);

        let list = http.request(0);
        assert_eq!(list.method, Method::Get);
        assert_eq!(list.url, "https://api.porkbun.com/api/json/v3/dns/retrieveByNameType/example.com/A/www");
        assert_eq!(header(&list, "X-API-Key"), Some("pk1_key"));
        assert_eq!(header(&list, "X-Secret-API-Key"), Some("sk1_key"));

        let create = http.request(1);
        assert_eq!(create.url, "https://api.porkbun.com/api/json/v3/dns/create/example.com");
        assert_eq!(
            body_json(&create),
            json!({"type": "A", "name": "www", "content": "203.0.113.42", "ttl": "600"})
        );
        http.assert_drained();
    }

    #[test]
    fn apex_omits_the_subdomain() {
        let http = FakeHttp::new();
        http.respond(200, EMPTY);
        provider(http.clone()).read(&RecordName::apex(), RecordType::A).unwrap();
        assert_eq!(http.request(0).url, "https://api.porkbun.com/api/json/v3/dns/retrieveByNameType/example.com/A");
    }

    #[test]
    fn updates_by_id_and_removes_extras() {
        let http = FakeHttp::new();
        http.respond(200, RECORDS).respond(200, OK).respond(200, OK);
        let outcome = provider(http.clone()).upsert(&name("www"), "203.0.113.42".parse().unwrap(), 600).unwrap();
        assert_eq!(outcome.change, Change::Updated);
        assert_eq!(http.request(1).url, "https://api.porkbun.com/api/json/v3/dns/edit/example.com/106926652");
        assert_eq!(body_json(&http.request(1)), json!({"content": "203.0.113.42", "ttl": "600"}));
        assert_eq!(http.request(2).url, "https://api.porkbun.com/api/json/v3/dns/delete/example.com/106926653");
        http.assert_drained();
    }

    #[test]
    fn surfaces_the_delegation_warning() {
        let http = FakeHttp::new();
        http.respond(200, EMPTY).respond(
            200,
            r#"{"status":"SUCCESS","id":1,"warnings":["Saved, but this domain is delegated to other nameservers."]}"#,
        );
        let outcome = provider(http).upsert(&name("www"), "203.0.113.42".parse().unwrap(), 600).unwrap();
        assert!(outcome.warnings[0].contains("delegated"));
    }

    #[test]
    fn deletes_every_record_at_the_name() {
        let http = FakeHttp::new();
        http.respond(200, RECORDS).respond(200, OK).respond(200, OK);
        provider(http.clone()).delete(&name("www"), RecordType::A).unwrap();
        assert_eq!(http.requests().len(), 3);
    }

    #[test]
    fn error_body_with_http_400_is_scrubbed() {
        let http = FakeHttp::new();
        http.respond(400, r#"{"status":"ERROR","code":"INVALID_API_KEYS_002","message":"bad secret sk1_key"}"#);
        let error = provider(http).read(&name("www"), RecordType::A).unwrap_err();
        let shown = format!("{error} {error:?}");
        assert!(shown.contains("INVALID_API_KEYS_002"));
        assert!(!shown.contains("sk1_key"), "{shown}");
    }

    #[test]
    fn error_status_with_http_200_is_still_an_error() {
        let http = FakeHttp::new();
        http.respond(200, r#"{"status":"ERROR","message":"no"}"#);
        assert!(provider(http).read(&name("www"), RecordType::A).is_err());
    }
}
