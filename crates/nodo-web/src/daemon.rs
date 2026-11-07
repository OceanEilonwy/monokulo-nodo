//! The dynamic DNS loop body: detect, compare, publish, remember.
//!
//! [`Daemon::tick`] does one pass and is driven by the binary's timer. It takes the time as an argument so
//! backoff can be tested without waiting.

use crate::detect::{detect, Consensus, Detection, Family, IpSource};
use crate::dns::{Change, DnsError, DnsProvider, RecordName, RecordType};
use crate::http::HttpClient;
use crate::reachability::{assess_connection, ConnectionAssessment, RouterWan};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use thiserror::Error;

const BACKOFF_BASE_SECS: u64 = 60;
const BACKOFF_MAX_SECS: u64 = 3600;
const STATE_FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedRecord {
    pub name: String,
    pub record_type: RecordType,
    pub ip: IpAddr,
    pub changed_at: u64,
}

/// What survives a restart. The screens read it for "Public IP", "DNS matches" and "Last changed".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DdnsState {
    /// Keyed by name and type, as `shop/A`.
    pub published: BTreeMap<String, PublishedRecord>,
    pub failures: u32,
    pub next_attempt_at: u64,
    pub last_check_at: u64,
    pub observed_v4: Option<IpAddr>,
    pub observed_v6: Option<IpAddr>,
}

fn state_key(name: &RecordName, record_type: RecordType) -> String {
    format!("{name}/{}", record_type.as_str())
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("could not read or write the state file: {0}")]
    Io(#[from] std::io::Error),
    #[error("the state file is not valid")]
    Corrupt,
}

pub trait StateStore {
    fn load(&self) -> Result<DdnsState, StateError>;
    fn save(&self, state: &DdnsState) -> Result<(), StateError>;
}

#[derive(Debug, Clone)]
pub struct FileStateStore {
    pub path: PathBuf,
}

impl StateStore for FileStateStore {
    fn load(&self) -> Result<DdnsState, StateError> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| StateError::Corrupt),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DdnsState::default()),
            Err(error) => Err(error.into()),
        }
    }

    /// Writes beside the target and renames, so a crash cannot leave half a file. The state holds the
    /// home IP, so it is private to this service's user.
    fn save(&self, state: &DdnsState) -> Result<(), StateError> {
        let temp = self.path.with_extension("tmp");
        let bytes = serde_json::to_vec_pretty(state).map_err(|_| StateError::Corrupt)?;
        let mut file =
            fs::OpenOptions::new().write(true).create(true).truncate(true).mode(STATE_FILE_MODE).open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, &self.path)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    /// One entry per name that a public site uses. Empty means no site is public.
    pub records: Vec<RecordName>,
    pub ttl: u32,
    pub ipv6: bool,
    pub sources: Vec<IpSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TickOutcome {
    /// No site is public, so nothing was contacted.
    NoPublicSites,
    BackingOff { until: u64 },
    /// The router reports a shared address, and DNS would point at an address the user does not control.
    BlockedByConnection { assessment: ConnectionAssessment },
    /// No family had an agreed address, so DNS was left alone.
    NoConsensus,
    Unchanged,
    Updated,
    /// At least one provider call failed. Retries follow the backoff.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordChange {
    pub name: String,
    pub ip: IpAddr,
    pub change: Change,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TickReport {
    pub outcome: TickOutcome,
    pub detections: Vec<Detection>,
    pub changes: Vec<RecordChange>,
    /// Scrubbed text, safe to log.
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// Records this daemon published for names no longer configured. See [`Daemon::retire_stale`].
    pub stale: Vec<String>,
}

impl TickReport {
    fn new(outcome: TickOutcome) -> Self {
        Self {
            outcome,
            detections: Vec::new(),
            changes: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            stale: Vec::new(),
        }
    }
}

pub struct Daemon<'a> {
    pub config: DaemonConfig,
    pub provider: &'a dyn DnsProvider,
    pub http: &'a dyn HttpClient,
    pub store: &'a dyn StateStore,
    pub router: Option<&'a dyn RouterWan>,
    state: DdnsState,
}

impl<'a> Daemon<'a> {
    /// A corrupt state file costs one read at the provider per record, since upsert checks before
    /// writing, so it is treated as empty rather than stopping updates. The error is returned for logging.
    pub fn new(
        config: DaemonConfig,
        provider: &'a dyn DnsProvider,
        http: &'a dyn HttpClient,
        store: &'a dyn StateStore,
        router: Option<&'a dyn RouterWan>,
    ) -> (Self, Option<StateError>) {
        let (state, error) = match store.load() {
            Ok(state) => (state, None),
            Err(error) => (DdnsState::default(), Some(error)),
        };
        (Self { config, provider, http, store, router, state }, error)
    }

    pub fn state(&self) -> &DdnsState {
        &self.state
    }

    fn stale_keys(&self) -> Vec<String> {
        self.state
            .published
            .iter()
            .filter(|(_, record)| !self.config.records.iter().any(|name| name.as_str() == record.name))
            .map(|(key, _)| key.clone())
            .collect()
    }

    fn persist(&self, report: &mut TickReport) {
        if let Err(error) = self.store.save(&self.state) {
            report.errors.push(error.to_string());
        }
    }

    pub fn tick(&mut self, now: u64) -> TickReport {
        let stale = self.stale_keys();
        // With no public site, the home IP has no business being looked up or published.
        if self.config.records.is_empty() {
            return TickReport { stale, ..TickReport::new(TickOutcome::NoPublicSites) };
        }
        if now < self.state.next_attempt_at {
            let until = self.state.next_attempt_at;
            return TickReport { stale, ..TickReport::new(TickOutcome::BackingOff { until }) };
        }

        let (mut report, retry_after) = self.run_pass(now);
        report.stale = stale;

        if matches!(report.outcome, TickOutcome::Failed | TickOutcome::NoConsensus) {
            self.state.failures = self.state.failures.saturating_add(1);
            self.state.next_attempt_at = now + backoff_secs(self.state.failures).max(retry_after);
        } else {
            self.state.failures = 0;
            self.state.next_attempt_at = 0;
        }
        self.persist(&mut report);
        report
    }

    fn run_pass(&mut self, now: u64) -> (TickReport, u64) {
        let mut detections = vec![detect(self.http, &self.config.sources, Family::V4)];
        if self.config.ipv6 {
            detections.push(detect(self.http, &self.config.sources, Family::V6));
        }
        self.state.last_check_at = now;
        self.state.observed_v4 = detections[0].agreed_ip();
        self.state.observed_v6 = detections.get(1).and_then(Detection::agreed_ip);

        if let Some(router) = self.router {
            let assessment = assess_connection(router.wan_address(), self.state.observed_v4);
            if assessment.blocks_public() {
                let outcome = TickOutcome::BlockedByConnection { assessment };
                return (TickReport { detections, ..TickReport::new(outcome) }, 0);
            }
        }

        let mut report = TickReport::new(TickOutcome::Unchanged);
        let mut retry_after = 0;
        let mut any_failed = false;
        let addresses: Vec<IpAddr> = detections.iter().filter_map(Detection::agreed_ip).collect();

        for detection in &detections {
            if detection.consensus == Consensus::Disagreement {
                report.warnings.push(format!(
                    "the {:?} address services disagree, so those records were left alone",
                    detection.family
                ));
            }
        }

        for ip in &addresses {
            for name in self.config.records.clone() {
                if let Err(error) = self.publish(&name, *ip, now, &mut report) {
                    any_failed = true;
                    retry_after = retry_after.max(error.retry_after_secs().unwrap_or(0));
                    report.errors.push(format!("{name}: {error}"));
                }
            }
        }

        report.outcome = if any_failed {
            TickOutcome::Failed
        } else if addresses.is_empty() {
            TickOutcome::NoConsensus
        } else if report.changes.is_empty() {
            TickOutcome::Unchanged
        } else {
            TickOutcome::Updated
        };
        report.detections = detections;
        (report, retry_after)
    }

    /// Calls the provider only if the address differs from what was last published here. Progress is saved
    /// after each record, so a crash halfway does not repeat finished work.
    fn publish(&mut self, name: &RecordName, ip: IpAddr, now: u64, report: &mut TickReport) -> Result<(), DnsError> {
        let record_type = RecordType::for_ip(ip);
        let key = state_key(name, record_type);
        if self.state.published.get(&key).is_some_and(|record| record.ip == ip) {
            return Ok(());
        }
        let outcome = self.provider.upsert(name, ip, self.config.ttl)?;
        report.warnings.extend(outcome.warnings);
        self.state.published.insert(
            key,
            PublishedRecord { name: name.as_str().to_owned(), record_type, ip, changed_at: now },
        );
        self.persist(report);
        if outcome.change != Change::Unchanged {
            report.changes.push(RecordChange { name: name.to_string(), ip, change: outcome.change });
        }
        Ok(())
    }

    /// Removes records this daemon published for names no longer configured. It is separate from
    /// [`Self::tick`] so that an empty or mistaken config can never delete DNS records on its own, and is
    /// meant to run when the user takes a site offline. Returns the keys it removed.
    pub fn retire_stale(&mut self) -> Result<Vec<String>, DnsError> {
        let mut report = TickReport::new(TickOutcome::Unchanged);
        let mut removed = Vec::new();
        for key in self.stale_keys() {
            let Some(record) = self.state.published.get(&key) else { continue };
            let name = RecordName::new(&record.name)?;
            self.provider.delete(&name, record.record_type)?;
            self.state.published.remove(&key);
            self.persist(&mut report);
            removed.push(key);
        }
        Ok(removed)
    }
}

fn backoff_secs(failures: u32) -> u64 {
    let exponent = failures.saturating_sub(1).min(16);
    BACKOFF_BASE_SECS.saturating_mul(1 << exponent).min(BACKOFF_MAX_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::UpsertOutcome;
    use crate::http::fake::FakeHttp;
    use crate::http::HttpError;
    use std::sync::Mutex;

    const IP_A: &str = "93.184.216.34";
    const IP_B: &str = "93.184.216.35";
    const IP6: &str = "2606:2800:220:1::1";

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[derive(Default)]
    struct FakeProvider {
        upserts: Mutex<Vec<(String, IpAddr)>>,
        deletes: Mutex<Vec<String>>,
        fail_with: Mutex<Option<DnsError>>,
    }

    impl DnsProvider for FakeProvider {
        fn min_ttl(&self) -> u32 {
            60
        }
        fn read(&self, _: &RecordName, _: RecordType) -> Result<Vec<IpAddr>, DnsError> {
            Ok(Vec::new())
        }
        fn upsert(&self, name: &RecordName, ip: IpAddr, _: u32) -> Result<UpsertOutcome, DnsError> {
            if let Some(error) = self.fail_with.lock().unwrap().clone() {
                return Err(error);
            }
            self.upserts.lock().unwrap().push((name.to_string(), ip));
            Ok(UpsertOutcome { change: Change::Updated, warnings: vec!["advisory".to_owned()] })
        }
        fn delete(&self, name: &RecordName, _: RecordType) -> Result<(), DnsError> {
            self.deletes.lock().unwrap().push(name.to_string());
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        state: Mutex<DdnsState>,
        fail_save: bool,
    }

    impl StateStore for MemoryStore {
        fn load(&self) -> Result<DdnsState, StateError> {
            Ok(self.state.lock().unwrap().clone())
        }
        fn save(&self, state: &DdnsState) -> Result<(), StateError> {
            if self.fail_save {
                return Err(StateError::Corrupt);
            }
            self.state.lock().unwrap().clone_from(state);
            Ok(())
        }
    }

    fn sources() -> Vec<IpSource> {
        ["a", "b", "c"]
            .iter()
            .flat_map(|op| {
                [
                    IpSource { operator: (*op).to_owned(), url: format!("https://{op}4.test"), family: Family::V4 },
                    IpSource { operator: (*op).to_owned(), url: format!("https://{op}6.test"), family: Family::V6 },
                ]
            })
            .collect()
    }

    fn config(records: &[&str]) -> DaemonConfig {
        DaemonConfig {
            records: records.iter().map(|r| RecordName::new(r).unwrap()).collect(),
            ttl: 300,
            ipv6: false,
            sources: sources(),
        }
    }

    fn agree(http: &FakeHttp, answer: &str) {
        for _ in 0..3 {
            http.respond(200, answer);
        }
    }

    #[test]
    fn with_no_public_site_nothing_is_contacted() {
        let (http, provider, store) = (FakeHttp::new(), FakeProvider::default(), MemoryStore::default());
        let (mut daemon, _) = Daemon::new(config(&[]), &provider, http.as_ref(), &store, None);
        assert_eq!(daemon.tick(1000).outcome, TickOutcome::NoPublicSites);
        assert!(http.requests().is_empty());
        assert!(provider.upserts.lock().unwrap().is_empty());
    }

    #[test]
    fn publishes_once_then_only_on_change() {
        let (http, provider, store) = (FakeHttp::new(), FakeProvider::default(), MemoryStore::default());
        let (mut daemon, _) = Daemon::new(config(&["shop", "pay"]), &provider, http.as_ref(), &store, None);

        agree(&http, IP_A);
        let first = daemon.tick(1000);
        assert_eq!(first.outcome, TickOutcome::Updated);
        assert_eq!(first.changes.len(), 2);
        assert_eq!(first.warnings, vec!["advisory", "advisory"]);
        assert_eq!(daemon.state().published["shop/A"].changed_at, 1000);

        agree(&http, IP_A);
        assert_eq!(daemon.tick(2000).outcome, TickOutcome::Unchanged);
        assert_eq!(provider.upserts.lock().unwrap().len(), 2);
        assert_eq!(daemon.state().published["shop/A"].changed_at, 1000);

        agree(&http, IP_B);
        assert_eq!(daemon.tick(3000).outcome, TickOutcome::Updated);
        assert_eq!(provider.upserts.lock().unwrap()[2], ("shop".to_owned(), ip(IP_B)));
        assert_eq!(daemon.state().published["shop/A"].changed_at, 3000);
    }

    #[test]
    fn state_survives_a_restart() {
        let (http, provider, store) = (FakeHttp::new(), FakeProvider::default(), MemoryStore::default());
        agree(&http, IP_A);
        let (mut daemon, _) = Daemon::new(config(&["shop"]), &provider, http.as_ref(), &store, None);
        daemon.tick(1000);
        drop(daemon);

        agree(&http, IP_A);
        let (mut restarted, error) = Daemon::new(config(&["shop"]), &provider, http.as_ref(), &store, None);
        assert!(error.is_none());
        assert_eq!(restarted.tick(2000).outcome, TickOutcome::Unchanged);
        assert_eq!(provider.upserts.lock().unwrap().len(), 1);
    }

    #[test]
    fn disagreement_changes_nothing_and_backs_off() {
        let (http, provider, store) = (FakeHttp::new(), FakeProvider::default(), MemoryStore::default());
        let (mut daemon, _) = Daemon::new(config(&["shop"]), &provider, http.as_ref(), &store, None);
        http.respond(200, IP_A).respond(200, IP_B).fail(HttpError::Timeout);

        let report = daemon.tick(1000);
        assert_eq!(report.outcome, TickOutcome::NoConsensus);
        assert!(provider.upserts.lock().unwrap().is_empty());
        assert_eq!(daemon.state().next_attempt_at, 1060);
        assert_eq!(daemon.state().observed_v4, None);
        assert!(report.warnings[0].contains("disagree"));
    }

    #[test]
    fn backoff_skips_all_work_grows_and_resets() {
        let (http, provider, store) = (FakeHttp::new(), FakeProvider::default(), MemoryStore::default());
        let (mut daemon, _) = Daemon::new(config(&["shop"]), &provider, http.as_ref(), &store, None);
        *provider.fail_with.lock().unwrap() = Some(DnsError::Server { status: 500 });

        agree(&http, IP_A);
        assert_eq!(daemon.tick(1000).outcome, TickOutcome::Failed);
        assert_eq!(daemon.state().next_attempt_at, 1060);

        let requests_before = http.requests().len();
        assert_eq!(daemon.tick(1059).outcome, TickOutcome::BackingOff { until: 1060 });
        assert_eq!(http.requests().len(), requests_before, "no IP lookups while backing off");

        agree(&http, IP_A);
        assert_eq!(daemon.tick(1060).outcome, TickOutcome::Failed);
        assert_eq!(daemon.state().next_attempt_at, 1060 + 120);
        assert_eq!(daemon.state().failures, 2);

        *provider.fail_with.lock().unwrap() = None;
        agree(&http, IP_A);
        assert_eq!(daemon.tick(1180).outcome, TickOutcome::Updated);
        assert_eq!(daemon.state().failures, 0);
        assert_eq!(daemon.state().next_attempt_at, 0);
    }

    #[test]
    fn backoff_is_capped_and_persisted_across_restart() {
        assert_eq!(backoff_secs(1), 60);
        assert_eq!(backoff_secs(2), 120);
        assert_eq!(backoff_secs(7), 3600);
        assert_eq!(backoff_secs(u32::MAX), 3600);

        let (http, provider, store) = (FakeHttp::new(), FakeProvider::default(), MemoryStore::default());
        *provider.fail_with.lock().unwrap() = Some(DnsError::Server { status: 500 });
        agree(&http, IP_A);
        let (mut daemon, _) = Daemon::new(config(&["shop"]), &provider, http.as_ref(), &store, None);
        daemon.tick(1000);
        drop(daemon);
        let (mut restarted, _) = Daemon::new(config(&["shop"]), &provider, http.as_ref(), &store, None);
        assert!(matches!(restarted.tick(1001).outcome, TickOutcome::BackingOff { .. }));
    }

    #[test]
    fn a_longer_provider_retry_after_wins() {
        let (http, provider, store) = (FakeHttp::new(), FakeProvider::default(), MemoryStore::default());
        *provider.fail_with.lock().unwrap() = Some(DnsError::RateLimited { retry_after_secs: Some(900) });
        agree(&http, IP_A);
        let (mut daemon, _) = Daemon::new(config(&["shop"]), &provider, http.as_ref(), &store, None);
        daemon.tick(1000);
        assert_eq!(daemon.state().next_attempt_at, 1900);
    }

    #[test]
    fn partial_failure_keeps_the_progress_made() {
        struct FailsOn(&'static str, FakeProvider);
        impl DnsProvider for FailsOn {
            fn min_ttl(&self) -> u32 {
                60
            }
            fn read(&self, _: &RecordName, _: RecordType) -> Result<Vec<IpAddr>, DnsError> {
                Ok(Vec::new())
            }
            fn upsert(&self, name: &RecordName, ip: IpAddr, ttl: u32) -> Result<UpsertOutcome, DnsError> {
                if name.as_str() == self.0 {
                    return Err(DnsError::Server { status: 502 });
                }
                self.1.upsert(name, ip, ttl)
            }
            fn delete(&self, _: &RecordName, _: RecordType) -> Result<(), DnsError> {
                Ok(())
            }
        }
        let (http, store) = (FakeHttp::new(), MemoryStore::default());
        let provider = FailsOn("pay", FakeProvider::default());
        agree(&http, IP_A);
        let (mut daemon, _) = Daemon::new(config(&["shop", "pay"]), &provider, http.as_ref(), &store, None);
        let report = daemon.tick(1000);
        assert_eq!(report.outcome, TickOutcome::Failed);
        assert!(report.errors[0].starts_with("pay:"));
        assert!(daemon.state().published.contains_key("shop/A"));
        assert!(!daemon.state().published.contains_key("pay/A"));
    }

    #[test]
    fn ipv6_is_handled_separately_and_its_absence_is_not_a_failure() {
        let (http, provider, store) = (FakeHttp::new(), FakeProvider::default(), MemoryStore::default());
        let mut cfg = config(&["shop"]);
        cfg.ipv6 = true;
        let (mut daemon, _) = Daemon::new(cfg, &provider, http.as_ref(), &store, None);

        agree(&http, IP_A);
        for _ in 0..3 {
            http.fail(HttpError::Connect);
        }
        assert_eq!(daemon.tick(1000).outcome, TickOutcome::Updated);
        assert_eq!(daemon.state().failures, 0);
        assert!(!daemon.state().published.contains_key("shop/AAAA"));

        agree(&http, IP_A);
        agree(&http, IP6);
        assert_eq!(daemon.tick(2000).outcome, TickOutcome::Updated);
        assert_eq!(daemon.state().published["shop/AAAA"].ip, ip(IP6));
        assert_eq!(daemon.state().published["shop/A"].changed_at, 1000);
    }

    #[test]
    fn cgnat_blocks_publishing() {
        struct Cgnat;
        impl RouterWan for Cgnat {
            fn wan_address(&self) -> Option<IpAddr> {
                Some("100.72.14.9".parse().unwrap())
            }
        }
        let (http, provider, store) = (FakeHttp::new(), FakeProvider::default(), MemoryStore::default());
        agree(&http, IP_A);
        let (mut daemon, _) = Daemon::new(config(&["shop"]), &provider, http.as_ref(), &store, Some(&Cgnat));
        assert!(matches!(daemon.tick(1000).outcome, TickOutcome::BlockedByConnection { .. }));
        assert!(provider.upserts.lock().unwrap().is_empty());
    }

    #[test]
    fn stale_records_are_reported_but_only_removed_on_request() {
        let (http, provider, store) = (FakeHttp::new(), FakeProvider::default(), MemoryStore::default());
        agree(&http, IP_A);
        let (mut daemon, _) = Daemon::new(config(&["shop"]), &provider, http.as_ref(), &store, None);
        daemon.tick(1000);
        drop(daemon);

        let (mut daemon, _) = Daemon::new(config(&[]), &provider, http.as_ref(), &store, None);
        let report = daemon.tick(2000);
        assert_eq!(report.outcome, TickOutcome::NoPublicSites);
        assert_eq!(report.stale, vec!["shop/A"]);
        assert!(provider.deletes.lock().unwrap().is_empty());

        assert_eq!(daemon.retire_stale().unwrap(), vec!["shop/A"]);
        assert_eq!(*provider.deletes.lock().unwrap(), vec!["shop"]);
        assert!(store.load().unwrap().published.is_empty());
    }

    #[test]
    fn a_failed_save_is_reported_without_losing_the_update() {
        let (http, provider) = (FakeHttp::new(), FakeProvider::default());
        let store = MemoryStore { fail_save: true, ..MemoryStore::default() };
        agree(&http, IP_A);
        let (mut daemon, _) = Daemon::new(config(&["shop"]), &provider, http.as_ref(), &store, None);
        let report = daemon.tick(1000);
        assert_eq!(report.outcome, TickOutcome::Updated);
        assert!(!report.errors.is_empty());
    }

    #[test]
    fn file_store_round_trips_privately_and_tolerates_corruption() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("nodo-web-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let store = FileStateStore { path: dir.join("state.json") };
        assert_eq!(store.load().unwrap(), DdnsState::default());

        let state = DdnsState { failures: 3, observed_v4: Some(ip(IP_A)), ..DdnsState::default() };
        store.save(&state).unwrap();
        assert_eq!(store.load().unwrap(), state);
        assert_eq!(fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);

        fs::write(&store.path, "not json").unwrap();
        assert!(matches!(store.load(), Err(StateError::Corrupt)));
        fs::remove_dir_all(&dir).unwrap();
    }
}
