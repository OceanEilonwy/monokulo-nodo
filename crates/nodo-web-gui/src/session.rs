//! One logged-in browser: its token, its backend, and what it keeps between requests.
//!
//! Sessions live in this process's memory and are gone when it restarts. A session keeps only
//! what a page cannot carry in its own form: the notices to show on the next page, the question
//! waiting for an answer, what the helper gave for a page that is shown next (`Stash`), the
//! unlock card's count of wrong tries, and the job that runs for it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::http::{header, HeaderMap};
use nodo_ui_core::exec::Data;
use nodo_ui_core::flows::unlock::Unlock;
use nodo_ui_core::flows::{install, Dest, Notice, Question};
use nodo_ui_core::model::{CapabilityData, Document, FeedProbe, InstallSnapshot, KeyChange, RejectionData};

use crate::clock::Clock;
use crate::runner::Reply;

/// A session ends after this long without a request that the owner made. Requests a page sends
/// by itself (the job's progress, a followed log) do not count.
pub const IDLE: Duration = Duration::from_secs(15 * 60);
/// A session ends this long after its login, however busy.
pub const HARD_LIMIT: Duration = Duration::from_secs(12 * 60 * 60);
/// More logins than this are refused until one ends.
pub const MAX_SESSIONS: usize = 4;

/// Locks a mutex whose holder may have panicked. The data is plain state that stays usable.
pub fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The session cookie's name and whether it is marked `Secure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CookieStyle {
    pub name: &'static str,
    pub secure: bool,
}

impl CookieStyle {
    /// On the device, behind Caddy's TLS. The `__Host-` prefix makes browsers refuse the cookie
    /// unless it is `Secure`, has `Path=/` and no `Domain`, so no other name on the network can
    /// set or read it.
    pub const DEVICE: CookieStyle = CookieStyle { name: "__Host-nodo", secure: true };
    /// For `--mock`, which serves plain HTTP on the loopback address. Browsers drop a `__Host-`
    /// or `Secure` cookie that arrives over plain HTTP, so development needs another name
    /// without `Secure`. Never used on the device.
    pub const MOCK: CookieStyle = CookieStyle { name: "nodo-dev", secure: false };

    fn attributes(self) -> &'static str {
        if self.secure { "Path=/; Secure; HttpOnly; SameSite=Strict" } else { "Path=/; HttpOnly; SameSite=Strict" }
    }

    /// The `Set-Cookie` value that starts a session. It has no lifetime of its own: the browser
    /// drops it when it closes, and the server ends the session by its own clock.
    pub fn set(self, token: &str) -> String {
        format!("{}={token}; {}", self.name, self.attributes())
    }

    /// The `Set-Cookie` value that removes the cookie.
    pub fn clear(self) -> String {
        format!("{}=; Max-Age=0; {}", self.name, self.attributes())
    }

    /// The token a request carries, if it carries one.
    pub fn read(self, headers: &HeaderMap) -> Option<String> {
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(';'))
            .filter_map(|pair| pair.trim().split_once('='))
            .find(|(name, _)| *name == self.name)
            .map(|(_, value)| value.to_owned())
    }
}

/// 256 random bits from the kernel, as 64 hex digits.
pub fn new_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    let token = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    zeroize::Zeroize::zeroize(&mut bytes);
    Ok(token)
}

/// Compares two tokens in time that depends only on their lengths, so a guess learns nothing
/// from how long a refusal took.
pub fn same_token(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// A question shown and not answered yet.
#[derive(Debug, Clone)]
pub struct Pending {
    /// Sent back with the answer, so a stale dialog confirms nothing.
    pub id: u64,
    pub question: Question,
    /// The page `Dest::Back` means once it is confirmed, and where its notices are shown.
    pub back: String,
    /// The page that asked, where Cancel returns.
    pub cancel: String,
}

/// What the helper gave for a page that is shown by the next request. Each holds the latest one
/// only; a page whose entry is gone or is another's sends the owner back to where it is made.
#[derive(Debug, Default)]
pub struct Stash {
    /// A source found at an address, waiting to be trusted or not. Trusting pins the key that
    /// was shown, so it is this value and not a second look at the address.
    pub probe: Option<FeedProbe>,
    pub key_change: Option<KeyChange>,
    /// What an app asks for, as it was shown. Approving quotes this back to the helper.
    pub review: Option<install::Review>,
    pub rejection: Option<RejectionData>,
    pub capability: Option<CapabilityData>,
    /// How the last install ended, or how far it is.
    pub install: Option<InstallSnapshot>,
    /// A text to read, and the page to return to after it.
    pub document: Option<(Document, String)>,
}

impl Stash {
    /// Keeps what a destination carries, for the page it leads to.
    pub fn keep(&mut self, dest: &Dest, back: &str) {
        match dest {
            Dest::ConfirmSource(probe) => self.probe = Some(probe.clone()),
            Dest::KeyChange(change) => self.key_change = Some(change.clone()),
            Dest::Review(review) => self.review = Some(review.clone()),
            Dest::Rejected(data) => self.rejection = Some(data.clone()),
            Dest::MissingCapability(data) => self.capability = Some(data.clone()),
            Dest::Installing(report) => self.install = Some(report.clone()),
            Dest::Document(document) => self.document = Some((document.clone(), back.to_owned())),
            // These carry what their URL carries, or nothing.
            Dest::Back
            | Dest::Apps
            | Dest::Updates { .. }
            | Dest::Unlock { .. }
            | Dest::Resume
            | Dest::GetApps { .. }
            | Dest::Sources { .. }
            | Dest::AppPage(_)
            | Dest::Detail(_)
            | Dest::Bundle(_)
            | Dest::ApplyUpdates { .. } => {}
        }
    }
}

/// How a job ended.
#[derive(Debug)]
pub enum JobEnd {
    /// It ran to its end: what its steps came to.
    Reply(Reply),
    /// The helper asked for a new login while it ran.
    LoginRequired,
    /// It could not run, or the device could not be read.
    Failed(String),
}

/// What a finished job says when the owner is on another page by then. It gets the reply the job
/// came to and answers with the notices for that page.
pub type Elsewhere = Box<dyn Fn(&Reply) -> Vec<Notice> + Send>;

/// A call that takes minutes, running on the blocking pool with the session's backend. One per
/// session: while it runs, the session's other pages say so instead of reading the device.
pub struct Job {
    pub id: u64,
    /// What is being done, as a heading: "Installing moneropay".
    pub what: String,
    /// What to expect meanwhile, a line each.
    pub note: Vec<String>,
    pub started: Duration,
    /// Where `Dest::Back` leads once it is over.
    pub back: String,
    pub end: Option<JobEnd>,
    pub elsewhere: Option<Elsewhere>,
}

/// A running job, as a page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobView {
    pub what: String,
    pub note: Vec<String>,
    pub seconds: u64,
}

/// The part of a session that changes, behind its mutex. Never held across an `.await` or a
/// helper call.
#[derive(Default)]
pub struct Inner {
    /// Notices for the next page drawn, then forgotten.
    pub notices: Vec<Notice>,
    pub question: Option<Pending>,
    pub stash: Stash,
    /// The unlock card's wrong tries and the reason for the last refusal.
    pub unlock: Unlock,
    /// When the unlock card accepts the next try.
    pub unlock_retry_at: Option<Duration>,
    pub job: Option<Job>,
    /// The tabs' counts as last read, for pages drawn without reading the device.
    pub badges: (usize, usize),
    next_id: u64,
}

impl Inner {
    /// Keeps a question until it is answered, replacing one still open.
    pub fn ask(&mut self, question: Question, back: &str, cancel: &str) -> Pending {
        self.next_id += 1;
        let pending = Pending { id: self.next_id, question, back: back.to_owned(), cancel: cancel.to_owned() };
        self.question = Some(pending.clone());
        pending
    }

    /// The question with this id, taken so it is answered once.
    pub fn answer(&mut self, id: u64) -> Option<Pending> {
        if self.question.as_ref().is_some_and(|pending| pending.id == id) { self.question.take() } else { None }
    }

    pub fn next_job_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

pub struct Session {
    /// For telling sessions apart in tests and logs. Never the token.
    pub id: u64,
    token: String,
    created: Duration,
    /// Milliseconds on the state's clock at the last request the owner made.
    last_active: AtomicU64,
    /// The session's own connection to the helper. Locked only on the blocking pool.
    pub backend: Arc<Mutex<Box<dyn Data>>>,
    inner: Mutex<Inner>,
}

impl Session {
    pub fn new(id: u64, token: String, backend: Box<dyn Data>, now: Duration) -> Self {
        Self {
            id,
            token,
            created: now,
            last_active: AtomicU64::new(now.as_millis() as u64),
            backend: Arc::new(Mutex::new(backend)),
            inner: Mutex::new(Inner::default()),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn has_token(&self, token: &str) -> bool {
        same_token(&self.token, token)
    }

    pub fn touch(&self, now: Duration) {
        self.last_active.store(now.as_millis() as u64, Ordering::SeqCst);
    }

    pub fn expired(&self, now: Duration) -> bool {
        let last = Duration::from_millis(self.last_active.load(Ordering::SeqCst));
        now.saturating_sub(last) >= IDLE || now.saturating_sub(self.created) >= HARD_LIMIT
    }

    pub fn inner(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    /// The job that holds the backend now, if one does.
    pub fn running_job(&self, clock: &dyn Clock) -> Option<JobView> {
        let inner = self.inner();
        let job = inner.job.as_ref().filter(|job| job.end.is_none())?;
        Some(JobView {
            what: job.what.clone(),
            note: job.note.clone(),
            seconds: clock.now().saturating_sub(job.started).as_secs(),
        })
    }

    /// Takes the job if it is over, so its result is told once.
    pub fn take_finished_job(&self) -> Option<Job> {
        let mut inner = self.inner();
        if inner.job.as_ref().is_some_and(|job| job.end.is_some()) { inner.job.take() } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_256_bits_and_differ() {
        let (a, b) = (new_token().expect("the kernel answers"), new_token().expect("the kernel answers"));
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn tokens_compare_whole() {
        assert!(same_token("abcd", "abcd"));
        assert!(!same_token("abcd", "abce"));
        assert!(!same_token("abcd", "abc"));
        assert!(!same_token("", "a"));
    }

    #[test]
    fn the_cookie_is_read_among_others_and_only_by_its_name() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, "theme=dark; __Host-nodo=abc123; x__Host-nodo=no".parse().expect("a header"));
        assert_eq!(CookieStyle::DEVICE.read(&headers).as_deref(), Some("abc123"));
        assert_eq!(CookieStyle::MOCK.read(&headers), None);
    }

    #[test]
    fn the_device_cookie_is_host_bound_secure_and_hidden_from_scripts() {
        let cookie = CookieStyle::DEVICE.set("t");
        assert_eq!(cookie, "__Host-nodo=t; Path=/; Secure; HttpOnly; SameSite=Strict");
        assert!(!cookie.contains("Domain"));
        assert!(CookieStyle::DEVICE.clear().contains("Max-Age=0"));
        assert!(!CookieStyle::MOCK.set("t").contains("Secure"));
    }
}
