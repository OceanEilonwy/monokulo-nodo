//! Dynamic DNS providers.
//!
//! Each provider implements its vendor's documented API, with the docs URL in [`catalog`]. Vendors whose
//! documentation could not be read are listed there as not implemented rather than guessed at.
//!
//! Upsert means "this name now points only at this address": other A or AAAA values at that name are
//! removed, because a stale address left beside the new one would send some visitors elsewhere. Other
//! record types and other names are never touched.

pub mod catalog;

use crate::http::{HttpError, HttpRequest, HttpResponse};
use crate::names;
use crate::secret::Scrubber;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use thiserror::Error;

pub const APEX: &str = "@";

/// 192.0.2.1 is reserved for documentation, so the test record never publishes the user's real address.
const TEST_RECORD_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
const TEST_RECORD_PREFIX: &str = "monokulo-test-";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RecordType {
    A,
    #[serde(rename = "AAAA")]
    Aaaa,
}

impl RecordType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::Aaaa => "AAAA",
        }
    }

    pub fn for_ip(ip: IpAddr) -> Self {
        if ip.is_ipv4() {
            Self::A
        } else {
            Self::Aaaa
        }
    }

    pub fn matches(self, text: &str) -> bool {
        text.eq_ignore_ascii_case(self.as_str())
    }
}

/// A validated domain, such as `example.com`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone(String);

impl Zone {
    pub fn new(value: &str) -> Result<Self, DnsError> {
        if names::is_public_name(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(DnsError::InvalidName(names::quoted(value)))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Zone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A record name relative to the zone: `@` for the domain itself, otherwise dotted labels. Validation
/// means names can go straight into URLs and request bodies.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct RecordName(String);

impl RecordName {
    pub fn new(value: &str) -> Result<Self, DnsError> {
        if value == APEX || names::is_dns_name(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(DnsError::InvalidName(names::quoted(value)))
        }
    }

    pub fn apex() -> Self {
        Self(APEX.to_owned())
    }

    /// `@` or the relative name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_apex(&self) -> bool {
        self.0 == APEX
    }

    /// The relative name, with the apex as an empty string.
    pub fn relative(&self) -> &str {
        if self.is_apex() {
            ""
        } else {
            &self.0
        }
    }

    pub fn fqdn(&self, zone: &Zone) -> String {
        if self.is_apex() {
            zone.as_str().to_owned()
        } else {
            format!("{}.{}", self.0, zone)
        }
    }
}

impl fmt::Display for RecordName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Errors hold only scrubbed text, so they are safe to log or show.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DnsError {
    #[error("not a valid DNS name: \"{0}\"")]
    InvalidName(String),
    #[error("the provider rejected these credentials: {0}")]
    Unauthorized(String),
    #[error("the provider refused this request ({code}): {detail}")]
    Forbidden { code: String, detail: String },
    #[error("the provider does not manage this domain")]
    ZoneNotFound,
    #[error("the provider is rate limiting requests")]
    RateLimited { retry_after_secs: Option<u64> },
    #[error("the provider rejected this request (HTTP {status}, {code}): {detail}")]
    Rejected { status: u16, code: String, detail: String },
    #[error("the provider had a server error (HTTP {status})")]
    Server { status: u16 },
    #[error(transparent)]
    Transport(#[from] HttpError),
    #[error("the provider sent a response this client does not understand: {0}")]
    Unexpected(&'static str),
    #[error("the temporary record was written but read back {found} addresses")]
    TestReadBack { found: usize },
    #[error("the temporary record {name} could not be removed, so please delete it by hand: {source}")]
    TestCleanup {
        name: String,
        #[source]
        source: Box<DnsError>,
    },
}

impl DnsError {
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after_secs } => *retry_after_secs,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Created,
    Updated,
    Unchanged,
}

/// `warnings` carries provider advisories that don't fail the call. Porkbun uses these to say a write
/// was stored but changes nothing, because the domain is delegated elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpsertOutcome {
    pub change: Change,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestReport {
    pub record: String,
    pub warnings: Vec<String>,
}

pub trait DnsProvider: Send + Sync {
    /// The smallest TTL this provider accepts. Lower requests are raised to it.
    fn min_ttl(&self) -> u32;

    /// The addresses currently published at this name and type.
    fn read(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<IpAddr>, DnsError>;

    /// Makes `ip` the only address of its type at this name, and writes nothing if it already is.
    fn upsert(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<UpsertOutcome, DnsError>;

    /// Removes all records of this type at this name. A missing record is success.
    fn delete(&self, name: &RecordName, record_type: RecordType) -> Result<(), DnsError>;

    /// Writes, reads back and removes a temporary record, proving the credentials can do real work.
    fn test(&self) -> Result<TestReport, DnsError> {
        let mut nonce = [0u8; 4];
        // A fixed fallback only risks reusing the label, and the record is removed right after.
        let _ = getrandom::fill(&mut nonce);
        let label = format!("{TEST_RECORD_PREFIX}{}", hex::encode(nonce));
        self.test_with_label(&RecordName::new(&label)?)
    }

    /// The body of [`Self::test`], split out so tests can fix the label.
    fn test_with_label(&self, name: &RecordName) -> Result<TestReport, DnsError> {
        let outcome = self.upsert(name, TEST_RECORD_IP, self.min_ttl())?;
        let read_back = self.read(name, RecordType::A);
        // Clean up before judging the read, so a failed test leaves nothing behind.
        if let Err(source) = self.delete(name, RecordType::A) {
            return Err(DnsError::TestCleanup { name: name.to_string(), source: Box::new(source) });
        }
        let addresses = read_back?;
        if addresses != [TEST_RECORD_IP] {
            return Err(DnsError::TestReadBack { found: addresses.len() });
        }
        Ok(TestReport { record: name.to_string(), warnings: outcome.warnings })
    }
}

/// One record at a provider with per-record identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingRecord {
    pub id: String,
    pub content: String,
}

impl ExistingRecord {
    fn is(&self, ip: IpAddr) -> bool {
        self.content.parse::<IpAddr>() == Ok(ip)
    }
}

/// The primitive operations of a per-record API. [`IdBacked`] turns these into a [`DnsProvider`].
pub trait IdRecords: Send + Sync {
    fn min_ttl(&self) -> u32;
    fn list(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<ExistingRecord>, DnsError>;
    /// These return advisory warnings.
    fn create(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError>;
    fn update(&self, id: &str, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<Vec<String>, DnsError>;
    fn remove(&self, id: &str) -> Result<(), DnsError>;
}

#[derive(Debug)]
pub struct IdBacked<S>(pub S);

impl<S: IdRecords> DnsProvider for IdBacked<S> {
    fn min_ttl(&self) -> u32 {
        self.0.min_ttl()
    }

    fn read(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<IpAddr>, DnsError> {
        let records = self.0.list(name, record_type)?;
        parse_addresses(records.iter().map(|r| r.content.as_str()))
    }

    fn upsert(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<UpsertOutcome, DnsError> {
        let ttl = ttl.max(self.0.min_ttl());
        let existing = self.0.list(name, RecordType::for_ip(ip))?;
        // Keep a record that is already right. Otherwise update the first, so the name never goes empty.
        let keep = existing.iter().position(|r| r.is(ip)).or(if existing.is_empty() { None } else { Some(0) });

        let (change, warnings) = match keep {
            None => (Change::Created, self.0.create(name, ip, ttl)?),
            Some(index) if existing[index].is(ip) => (Change::Unchanged, Vec::new()),
            Some(index) => (Change::Updated, self.0.update(&existing[index].id, name, ip, ttl)?),
        };
        let mut removed_any = false;
        for (index, record) in existing.iter().enumerate() {
            if Some(index) != keep {
                self.0.remove(&record.id)?;
                removed_any = true;
            }
        }
        let change = if change == Change::Unchanged && removed_any { Change::Updated } else { change };
        Ok(UpsertOutcome { change, warnings })
    }

    fn delete(&self, name: &RecordName, record_type: RecordType) -> Result<(), DnsError> {
        for record in self.0.list(name, record_type)? {
            self.0.remove(&record.id)?;
        }
        Ok(())
    }
}

/// A non-address value in an A or AAAA record means the response is not what the docs describe.
pub(crate) fn parse_addresses<'a>(values: impl Iterator<Item = &'a str>) -> Result<Vec<IpAddr>, DnsError> {
    values.map(|v| v.parse().map_err(|_| DnsError::Unexpected("a record value was not an IP address"))).collect()
}

/// A provider's error body reduced to its code and message, already scrubbed.
pub(crate) struct ProviderMessage {
    pub code: String,
    pub detail: String,
}

/// Maps a failed HTTP status to an error. Messages are scrubbed by `describe`, which sees the raw body.
pub(crate) fn status_error(
    response: &HttpResponse,
    describe: impl Fn(&[u8]) -> ProviderMessage,
) -> DnsError {
    let ProviderMessage { code, detail } = describe(&response.body);
    match response.status {
        401 => DnsError::Unauthorized(detail),
        403 => DnsError::Forbidden { code, detail },
        429 => DnsError::RateLimited {
            retry_after_secs: response.header("Retry-After").and_then(|v| v.trim().parse().ok()),
        },
        status @ 500..=599 => DnsError::Server { status },
        status => DnsError::Rejected { status, code, detail },
    }
}

pub(crate) fn decode<T: DeserializeOwned>(response: &HttpResponse, what: &'static str) -> Result<T, DnsError> {
    serde_json::from_slice(&response.body).map_err(|_| DnsError::Unexpected(what))
}

pub(crate) fn send(http: &dyn crate::http::HttpClient, request: &HttpRequest) -> Result<HttpResponse, DnsError> {
    Ok(http.send(request)?)
}

pub(crate) fn clean_warnings(warnings: Vec<String>, scrubber: &Scrubber) -> Vec<String> {
    warnings.iter().map(|w| scrubber.clean(w)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Memory {
        records: Mutex<Vec<(String, String, String)>>,
        next_id: Mutex<u32>,
        fail_remove: bool,
    }

    impl IdRecords for Memory {
        fn min_ttl(&self) -> u32 {
            300
        }
        fn list(&self, name: &RecordName, record_type: RecordType) -> Result<Vec<ExistingRecord>, DnsError> {
            Ok(self
                .records
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.1 == format!("{name}/{}", record_type.as_str()))
                .map(|r| ExistingRecord { id: r.0.clone(), content: r.2.clone() })
                .collect())
        }
        fn create(&self, name: &RecordName, ip: IpAddr, _: u32) -> Result<Vec<String>, DnsError> {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            let key = format!("{name}/{}", RecordType::for_ip(ip).as_str());
            self.records.lock().unwrap().push((next.to_string(), key, ip.to_string()));
            Ok(vec!["note".to_owned()])
        }
        fn update(&self, id: &str, _: &RecordName, ip: IpAddr, _: u32) -> Result<Vec<String>, DnsError> {
            let mut records = self.records.lock().unwrap();
            records.iter_mut().find(|r| r.0 == id).unwrap().2 = ip.to_string();
            Ok(Vec::new())
        }
        fn remove(&self, id: &str) -> Result<(), DnsError> {
            if self.fail_remove {
                return Err(DnsError::Server { status: 500 });
            }
            self.records.lock().unwrap().retain(|r| r.0 != id);
            Ok(())
        }
    }

    fn name(value: &str) -> RecordName {
        RecordName::new(value).unwrap()
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    fn seeded(values: &[&str]) -> IdBacked<Memory> {
        let memory = Memory::default();
        for (index, value) in values.iter().enumerate() {
            memory.records.lock().unwrap().push((format!("seed{index}"), "www/A".to_owned(), (*value).to_owned()));
        }
        IdBacked(memory)
    }

    #[test]
    fn creates_when_empty_and_is_idempotent() {
        let provider = seeded(&[]);
        assert_eq!(provider.upsert(&name("www"), ip("192.0.2.5"), 60).unwrap().change, Change::Created);
        assert_eq!(provider.upsert(&name("www"), ip("192.0.2.5"), 60).unwrap().change, Change::Unchanged);
        assert_eq!(provider.read(&name("www"), RecordType::A).unwrap(), vec![ip("192.0.2.5")]);
    }

    #[test]
    fn updates_in_place_and_removes_stale_extras() {
        let provider = seeded(&["192.0.2.5", "192.0.2.6"]);
        assert_eq!(provider.upsert(&name("www"), ip("192.0.2.7"), 300).unwrap().change, Change::Updated);
        assert_eq!(provider.read(&name("www"), RecordType::A).unwrap(), vec![ip("192.0.2.7")]);
    }

    #[test]
    fn keeps_the_matching_record_and_removes_the_rest() {
        let provider = seeded(&["192.0.2.5", "192.0.2.6"]);
        assert_eq!(provider.upsert(&name("www"), ip("192.0.2.6"), 300).unwrap().change, Change::Updated);
        assert_eq!(provider.read(&name("www"), RecordType::A).unwrap(), vec![ip("192.0.2.6")]);
    }

    #[test]
    fn aaaa_does_not_touch_a() {
        let provider = seeded(&["192.0.2.5"]);
        provider.upsert(&name("www"), ip("2001:db8::1"), 300).unwrap();
        assert_eq!(provider.read(&name("www"), RecordType::A).unwrap(), vec![ip("192.0.2.5")]);
        assert_eq!(provider.read(&name("www"), RecordType::Aaaa).unwrap(), vec![ip("2001:db8::1")]);
        provider.delete(&name("www"), RecordType::Aaaa).unwrap();
        assert_eq!(provider.read(&name("www"), RecordType::A).unwrap(), vec![ip("192.0.2.5")]);
    }

    #[test]
    fn delete_of_a_missing_record_succeeds() {
        assert!(seeded(&[]).delete(&name("www"), RecordType::A).is_ok());
    }

    #[test]
    fn test_writes_reads_and_removes_using_a_documentation_address() {
        let provider = seeded(&[]);
        let report = provider.test().unwrap();
        assert!(report.record.starts_with("monokulo-test-"));
        assert_eq!(report.warnings, vec!["note"]);
        assert!(provider.0.records.lock().unwrap().is_empty());
    }

    #[test]
    fn test_reports_a_record_it_could_not_remove() {
        let provider = IdBacked(Memory { fail_remove: true, ..Memory::default() });
        let error = provider.test_with_label(&name("monokulo-test-1")).unwrap_err();
        assert!(matches!(error, DnsError::TestCleanup { .. }));
        assert!(error.to_string().contains("monokulo-test-1"));
    }

    #[test]
    fn hostile_names_are_rejected() {
        for bad in ["a b", "a\n", "../a", "{x}", "a/b", "*", "", "A", "a?b=c", "a%2fb"] {
            assert!(matches!(RecordName::new(bad), Err(DnsError::InvalidName(_))), "{bad:?}");
        }
        for bad in ["a b.com", "com", "shop.local", "{x}.com"] {
            assert!(matches!(Zone::new(bad), Err(DnsError::InvalidName(_))), "{bad:?}");
        }
    }

    #[test]
    fn names_render_for_each_vendor_style() {
        let zone = Zone::new("example.com").unwrap();
        assert_eq!(RecordName::apex().fqdn(&zone), "example.com");
        assert_eq!(RecordName::apex().relative(), "");
        assert_eq!(name("a.b").fqdn(&zone), "a.b.example.com");
    }
}
