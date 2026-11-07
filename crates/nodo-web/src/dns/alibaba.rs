//! Alibaba Cloud DNS (Alidns 2015-01-09), signed with V3 `ACS3-HMAC-SHA256`.
//! Docs: <https://www.alibabacloud.com/help/en/dns/api-alidns-2015-01-09-overview> and
//! <https://www.alibabacloud.com/help/en/sdk/product-overview/v3-request-structure-and-signature>.
//!
//! Alibaba's docs recommend the SDK over hand signing, but there is no Rust SDK. The signing here is
//! checked against the worked example on the signature page.
//!
//! Only records on the `default` resolution line are managed, so a user's line-specific records survive.

use super::{
    decode, send, status_error, DnsError, ExistingRecord, IdBacked, IdRecords, ProviderMessage, RecordName,
    RecordType, Zone,
};
use crate::http::{encode_component, HttpRequest, HttpResponse, Method, SharedHttp};
use crate::secret::{Scrubber, Secret};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

const HOST: &str = "alidns.aliyuncs.com";
const VERSION: &str = "2015-01-09";
const ALGORITHM: &str = "ACS3-HMAC-SHA256";
const SIGNED_HEADERS: &str = "host;x-acs-action;x-acs-content-sha256;x-acs-date;x-acs-signature-nonce;x-acs-version";
/// The documented default. The docs do not state a minimum.
const MIN_TTL: u32 = 600;
const PAGE_SIZE: &str = "100";
const DEFAULT_LINE: &str = "default";

/// The timestamp and nonce for one request, injectable so that signing is testable.
pub type RequestStamp = Box<dyn Fn() -> (String, String) + Send + Sync>;

pub struct Alibaba {
    http: SharedHttp,
    zone: Zone,
    access_key_id: String,
    access_key_secret: Secret,
    scrubber: Scrubber,
    stamp: RequestStamp,
}

impl Alibaba {
    pub fn new(http: SharedHttp, zone: Zone, access_key_id: Secret, access_key_secret: Secret) -> IdBacked<Self> {
        Self::with_stamp(http, zone, access_key_id, access_key_secret, Box::new(real_stamp))
    }

    fn with_stamp(
        http: SharedHttp,
        zone: Zone,
        access_key_id: Secret,
        access_key_secret: Secret,
        stamp: RequestStamp,
    ) -> IdBacked<Self> {
        let scrubber = Scrubber::new(&[&access_key_id, &access_key_secret]);
        IdBacked(Self {
            http,
            zone,
            access_key_id: access_key_id.expose().to_owned(),
            access_key_secret,
            scrubber,
            stamp,
        })
    }

    fn call(&self, action: &str, params: &[(&str, &str)]) -> Result<HttpResponse, DnsError> {
        let (date, nonce) = (self.stamp)();
        let signed = sign(&SignInput {
            method: "POST",
            host: HOST,
            action,
            version: VERSION,
            date: &date,
            nonce: &nonce,
            params,
            access_key_id: &self.access_key_id,
            access_key_secret: self.access_key_secret.expose(),
        });
        let request = HttpRequest::new(Method::Post, format!("https://{HOST}/?{}", signed.query))
            .header("host", HOST)
            .header("x-acs-action", action)
            .header("x-acs-version", VERSION)
            .header("x-acs-date", date)
            .header("x-acs-signature-nonce", nonce)
            .header("x-acs-content-sha256", EMPTY_SHA256)
            .header("Authorization", signed.authorization);
        let response = send(self.http.as_ref(), &request)?;
        if response.is_success() {
            Ok(response)
        } else {
            Err(self.failure(&response))
        }
    }

    fn failure(&self, response: &HttpResponse) -> DnsError {
        let parsed: ErrorBody = serde_json::from_slice(&response.body).unwrap_or_default();
        let code = self.scrubber.clean(&parsed.code);
        let detail = self.scrubber.clean(&parsed.message);
        // Alibaba reports bad credentials as a 4xx with a code, not always as 401.
        if code.starts_with("InvalidAccessKeyId") || code.starts_with("SignatureDoesNotMatch") {
            return DnsError::Unauthorized(detail);
        }
        if code == "IncorrectDomainUser" || code.starts_with("InvalidDomainName.NoExist") {
            return DnsError::ZoneNotFound;
        }
        status_error(response, |_| ProviderMessage { code: code.clone(), detail: detail.clone() })
    }
}

#[derive(Default, Deserialize)]
struct ErrorBody {
    #[serde(default, rename = "Code")]
    code: String,
    #[serde(default, rename = "Message")]
    message: String,
}

#[derive(Deserialize)]
struct DescribeResponse {
    #[serde(rename = "DomainRecords")]
    domain_records: DomainRecords,
}

#[derive(Deserialize)]
struct DomainRecords {
    #[serde(rename = "Record", default)]
    record: Vec<Record>,
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "RecordId")]
    record_id: String,
    #[serde(rename = "RR")]
    rr: String,
    #[serde(rename = "Type")]
    record_type: String,
    #[serde(rename = "Value")]
    value: String,
    #[serde(rename = "Line", default)]
    line: Option<String>,
}

impl IdRecords for Alibaba {
    fn min_ttl(&self) -> u32 {
        MIN_TTL
    }

    fn list(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<ExistingRecord>, DnsError> {
        let fqdn = name.fqdn(&self.zone);
        let response = self.call(
            "DescribeSubDomainRecords",
            &[
                ("SubDomain", &fqdn),
                ("DomainName", self.zone.as_str()),
                ("Type", record_type.as_str()),
                ("Line", DEFAULT_LINE),
                ("PageSize", PAGE_SIZE),
            ],
        )?;
        let parsed: DescribeResponse = decode(&response, "Alibaba records")?;
        Ok(parsed
            .domain_records
            .record
            .into_iter()
            .filter(|r| {
                r.rr == name.as_str()
                    && record_type.matches(&r.record_type)
                    && r.line.as_deref().is_none_or(|line| line == DEFAULT_LINE)
            })
            .map(|r| ExistingRecord { id: r.record_id, content: r.value })
            .collect())
    }

    fn create(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError> {
        self.call(
            "AddDomainRecord",
            &[
                ("DomainName", self.zone.as_str()),
                ("RR", name.as_str()),
                ("Type", RecordType::for_ip(ip).as_str()),
                ("Value", &ip.to_string()),
                ("TTL", &ttl.to_string()),
                ("Line", DEFAULT_LINE),
            ],
        )?;
        Ok(Vec::new())
    }

    fn update(&self, id: &str, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError> {
        self.call(
            "UpdateDomainRecord",
            &[
                ("RecordId", id),
                ("RR", name.as_str()),
                ("Type", RecordType::for_ip(ip).as_str()),
                ("Value", &ip.to_string()),
                ("TTL", &ttl.to_string()),
                ("Line", DEFAULT_LINE),
            ],
        )?;
        Ok(Vec::new())
    }

    fn remove(&self, id: &str) -> Result<(), DnsError> {
        self.call("DeleteDomainRecord", &[("RecordId", id)]).map(|_| ())
    }
}

/// SHA-256 of the empty body. Every request here sends its parameters in the query string.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

struct SignInput<'a> {
    method: &'a str,
    host: &'a str,
    action: &'a str,
    version: &'a str,
    date: &'a str,
    nonce: &'a str,
    params: &'a [(&'a str, &'a str)],
    access_key_id: &'a str,
    access_key_secret: &'a str,
}

struct Signed {
    query: String,
    authorization: String,
}

fn canonical_query(params: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> =
        params.iter().map(|(k, v)| (encode_component(k), encode_component(v))).collect();
    pairs.sort();
    pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

fn canonical_request_hash(input: &SignInput<'_>, query: &str) -> String {
    let canonical_headers = format!(
        "host:{}\nx-acs-action:{}\nx-acs-content-sha256:{EMPTY_SHA256}\nx-acs-date:{}\nx-acs-signature-nonce:{}\nx-acs-version:{}\n",
        input.host, input.action, input.date, input.nonce, input.version,
    );
    let canonical_request =
        format!("{}\n/\n{query}\n{canonical_headers}\n{SIGNED_HEADERS}\n{EMPTY_SHA256}", input.method);
    hex::encode(Sha256::digest(canonical_request.as_bytes()))
}

fn sign(input: &SignInput<'_>) -> Signed {
    let query = canonical_query(input.params);
    let string_to_sign = format!("{ALGORITHM}\n{}", canonical_request_hash(input, &query));

    let mut mac = Hmac::<Sha256>::new_from_slice(input.access_key_secret.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(string_to_sign.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    Signed {
        query,
        authorization: format!(
            "{ALGORITHM} Credential={},SignedHeaders={SIGNED_HEADERS},Signature={signature}",
            input.access_key_id
        ),
    }
}

fn real_stamp() -> (String, String) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut nonce = [0u8; 16];
    // On failure the nonce still varies by time. A collision only makes the server reject the request.
    if getrandom::fill(&mut nonce).is_err() {
        nonce[..8].copy_from_slice(&secs.to_be_bytes());
    }
    (iso8601_utc(secs), hex::encode(nonce))
}

/// Formats Unix seconds as `YYYY-MM-DDThh:mm:ssZ`, using the civil-from-days algorithm.
fn iso8601_utc(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::{Change, DnsProvider};
    use crate::http::fake::{header, FakeHttp};

    const RECORDS: &str = include_str!("../../tests/fixtures/alibaba_records.json");
    const EMPTY: &str = r#"{"TotalCount":0,"PageSize":100,"PageNumber":1,"RequestId":"x","DomainRecords":{"Record":[]}}"#;
    const WRITTEN: &str = r#"{"RequestId":"x","RecordId":"99"}"#;

    fn provider(http: std::sync::Arc<FakeHttp>) -> IdBacked<Alibaba> {
        Alibaba::with_stamp(
            http,
            Zone::new("example.com").unwrap(),
            Secret::new("ali-id"),
            Secret::new("ali-secret"),
            Box::new(|| ("2026-01-02T03:04:05Z".to_owned(), "nonce".to_owned())),
        )
    }

    fn name(value: &str) -> RecordName {
        RecordName::new(value).unwrap()
    }

    /// The worked example from Alibaba's V3 signature documentation.
    #[test]
    fn signature_matches_the_documented_example() {
        let input = SignInput {
            method: "POST",
            host: "ecs.cn-shanghai.aliyuncs.com",
            action: "RunInstances",
            version: "2014-05-26",
            date: "2023-10-26T10:22:32Z",
            nonce: "3156853299f313e23d1673dc12e1703d",
            params: &[("RegionId", "cn-shanghai"), ("ImageId", "win2019_1809_x64_dtc_zh-cn_40G_alibase_20230811.vhd")],
            access_key_id: "YourAccessKeyId",
            access_key_secret: "YourAccessKeySecret",
        };
        let signed = sign(&input);
        assert_eq!(signed.query, "ImageId=win2019_1809_x64_dtc_zh-cn_40G_alibase_20230811.vhd&RegionId=cn-shanghai");
        assert_eq!(
            canonical_request_hash(&input, &signed.query),
            "7ea06492da5221eba5297e897ce16e55f964061054b7695beedaac1145b1e259"
        );
        assert_eq!(
            signed.authorization,
            "ACS3-HMAC-SHA256 Credential=YourAccessKeyId,\
             SignedHeaders=host;x-acs-action;x-acs-content-sha256;x-acs-date;x-acs-signature-nonce;x-acs-version,\
             Signature=06563a9e1b43f5dfe96b81484da74bceab24a1d853912eee15083a6f0f3283c0"
        );
    }

    #[test]
    fn dates_are_formatted_as_utc() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(1_698_315_752), "2023-10-26T10:22:32Z");
        assert_eq!(iso8601_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn create_sends_a_signed_add_record_request() {
        let http = FakeHttp::new();
        http.respond(200, EMPTY).respond(200, WRITTEN);
        let outcome = provider(http.clone()).upsert(&name("www"), "203.0.113.42".parse().unwrap(), 60).unwrap();
        assert_eq!(outcome.change, Change::Created);

        let list = http.request(0);
        assert_eq!(header(&list, "x-acs-action"), Some("DescribeSubDomainRecords"));
        assert_eq!(header(&list, "x-acs-version"), Some("2015-01-09"));
        assert!(list.url.starts_with("https://alidns.aliyuncs.com/?DomainName=example.com&Line=default"));
        assert!(list.url.contains("SubDomain=www.example.com"));

        let add = http.request(1);
        assert_eq!(add.method, Method::Post);
        assert_eq!(header(&add, "x-acs-action"), Some("AddDomainRecord"));
        assert_eq!(
            add.url,
            "https://alidns.aliyuncs.com/?DomainName=example.com&Line=default&RR=www&TTL=600&Type=A&Value=203.0.113.42"
        );
        assert!(header(&add, "Authorization").unwrap().starts_with("ACS3-HMAC-SHA256 Credential=ali-id,"));
    }

    #[test]
    fn apex_queries_with_the_domain_as_the_subdomain() {
        let http = FakeHttp::new();
        http.respond(200, EMPTY);
        provider(http.clone()).read(&RecordName::apex(), RecordType::A).unwrap();
        assert!(http.request(0).url.contains("SubDomain=example.com"));
    }

    #[test]
    fn line_specific_records_are_left_alone() {
        let http = FakeHttp::new();
        http.respond(200, RECORDS).respond(200, WRITTEN);
        provider(http.clone()).upsert(&name("www"), "203.0.113.42".parse().unwrap(), 600).unwrap();
        let update = http.request(1);
        assert_eq!(header(&update, "x-acs-action"), Some("UpdateDomainRecord"));
        assert!(update.url.contains("RecordId=1001"));
        http.assert_drained();
    }

    #[test]
    fn delete_removes_by_record_id() {
        let http = FakeHttp::new();
        http.respond(200, RECORDS).respond(200, WRITTEN);
        provider(http.clone()).delete(&name("www"), RecordType::A).unwrap();
        assert_eq!(header(&http.request(1), "x-acs-action"), Some("DeleteDomainRecord"));
        assert_eq!(http.request(1).url, "https://alidns.aliyuncs.com/?RecordId=1001");
    }

    #[test]
    fn bad_credentials_map_to_unauthorized_and_are_scrubbed() {
        let http = FakeHttp::new();
        http.respond(400, r#"{"Code":"InvalidAccessKeyId.NotFound","Message":"key ali-id secret ali-secret"}"#);
        let error = provider(http).read(&name("www"), RecordType::A).unwrap_err();
        assert!(matches!(error, DnsError::Unauthorized(_)));
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains("ali-id") && !shown.contains("ali-secret"), "{shown}");
    }

    #[test]
    fn foreign_domain_maps_to_zone_not_found() {
        let http = FakeHttp::new();
        http.respond(400, r#"{"Code":"IncorrectDomainUser","Message":"no"}"#);
        assert_eq!(provider(http).read(&name("www"), RecordType::A).unwrap_err(), DnsError::ZoneNotFound);
    }
}
