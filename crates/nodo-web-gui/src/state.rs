//! What the whole process shares: the sessions, the one login in flight, and the helper's gate.
//!
//! The helper has one gate for the web GUI, whatever the number of browsers. It opens with every
//! login and must be closed (`WebGuiLogout`) only when the last session has ended, so that one
//! browser logging out does not shut the others out. Logins and that closing take the same lock
//! (`gate`), so a logout is never sent under a login that has just succeeded.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use nodo_api::ApiError;
use nodo_ui_core::flows::unlock::Unlock;
use nodo_ui_core::secret::Secret;

use crate::backends::Backends;
use crate::clock::Clock;
use crate::core_wishlist::{login_error, typed_secret};
use crate::session::{lock, new_token, CookieStyle, Session, MAX_SESSIONS};

/// How this process is reached.
#[derive(Debug, Clone)]
pub struct Config {
    pub cookie: CookieStyle,
    /// The `Host` every request must name, when there is only one right answer: `nodo.local` on
    /// the device. `None` accepts any, for `--mock` on a port of the loopback address.
    pub host: Option<String>,
}

impl Config {
    /// Behind Caddy on the device.
    pub fn device() -> Self {
        let host = nodo_api::WEBGUI_URL.trim_start_matches("https://").to_owned();
        Self { cookie: CookieStyle::DEVICE, host: Some(host) }
    }

    /// Plain HTTP on the loopback address, for development and tests.
    pub fn mock() -> Self {
        Self { cookie: CookieStyle::MOCK, host: None }
    }

    /// The scheme an `Origin` must have: what the browser used to reach Caddy, or this process
    /// itself under `--mock`.
    pub fn scheme(&self) -> &'static str {
        if self.cookie.secure { "https" } else { "http" }
    }
}

/// Why a login did not start a session, for the login page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub status: StatusCode,
    pub message: String,
}

impl Refusal {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
}

/// Wrong tries by one address, and when it may try again.
struct Tries {
    failures: u32,
    retry_at: Duration,
}

/// An address is forgotten this long after its last wrong try.
const FORGET_TRIES: Duration = Duration::from_secs(15 * 60);
/// More addresses than a home network has. Beyond it the table is emptied; the helper's own
/// growing delay still stands between a guesser and the passphrase.
const MAX_TRACKED: usize = 1024;

struct Shared {
    backends: Arc<dyn Backends>,
    clock: Arc<dyn Clock>,
    config: Config,
    sessions: Mutex<Vec<Arc<Session>>>,
    /// Held by the one login in flight, and while the helper's gate is closed.
    gate: tokio::sync::Mutex<()>,
    tries: Mutex<HashMap<String, Tries>>,
    next_id: AtomicU64,
}

#[derive(Clone)]
pub struct AppState(Arc<Shared>);

impl AppState {
    pub fn new(backends: Arc<dyn Backends>, clock: Arc<dyn Clock>, config: Config) -> Self {
        Self(Arc::new(Shared {
            backends,
            clock,
            config,
            sessions: Mutex::new(Vec::new()),
            gate: tokio::sync::Mutex::new(()),
            tries: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }))
    }

    pub fn config(&self) -> &Config {
        &self.0.config
    }

    pub fn clock(&self) -> &dyn Clock {
        &*self.0.clock
    }

    pub fn now(&self) -> Duration {
        self.0.clock.now()
    }

    pub fn backends(&self) -> Arc<dyn Backends> {
        self.0.backends.clone()
    }

    pub fn session_count(&self) -> usize {
        lock(&self.0.sessions).len()
    }

    /// The session a request's cookie names, if it is still good. `touch` counts the request as
    /// the owner's activity; a page's own polling passes false.
    pub async fn authenticate(&self, token: &str, touch: bool) -> Option<Arc<Session>> {
        self.sweep().await;
        let now = self.now();
        // Every session is compared, so the time taken does not say which one matched.
        let found = lock(&self.0.sessions).iter().fold(None, |found, session| {
            if session.has_token(token) { Some(session.clone()) } else { found }
        })?;
        if touch {
            found.touch(now);
        }
        Some(found)
    }

    /// Ends the sessions whose time is up. Called before every look at the sessions, and by the
    /// server's timer so the helper's gate closes even when no request comes.
    pub async fn sweep(&self) {
        let now = self.now();
        let ended = {
            let mut sessions = lock(&self.0.sessions);
            let (ended, kept): (Vec<_>, Vec<_>) = sessions.drain(..).partition(|session| session.expired(now));
            *sessions = kept;
            ended
        };
        self.ended(ended).await;
    }

    /// Ends one session. The others stay.
    pub async fn logout(&self, session: &Arc<Session>) {
        let ended = {
            let mut sessions = lock(&self.0.sessions);
            let (ended, kept): (Vec<_>, Vec<_>) = sessions.drain(..).partition(|each| Arc::ptr_eq(each, session));
            *sessions = kept;
            ended
        };
        self.ended(ended).await;
    }

    /// Forgets every session without telling the helper: it said itself that its gate is closed.
    pub fn drop_all(&self) {
        lock(&self.0.sessions).clear();
    }

    /// After sessions ended: closes the helper's gate if none is left. The request goes through
    /// the last one's backend. Should that be held by a job, the closing waits for it on its own
    /// task and the request that ended the session does not.
    async fn ended(&self, ended: Vec<Arc<Session>>) {
        let Some(last) = ended.last().cloned() else {
            return;
        };
        if self.session_count() > 0 {
            return;
        }
        let busy = last.running_job(self.clock()).is_some();
        let state = self.clone();
        let close = async move {
            // Behind any login in flight. If that one succeeded, the gate stays open for it.
            let _gate = state.0.gate.lock().await;
            if state.session_count() > 0 {
                return;
            }
            let backends = state.backends();
            let closing = tokio::task::spawn_blocking(move || {
                let mut backend = lock(&last.backend);
                backends.logout(&mut **backend)
            });
            match closing.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("nodo-web-gui: the helper did not take the logout: {error}"),
                Err(error) => eprintln!("nodo-web-gui: the logout did not run: {error}"),
            }
        };
        // A login in flight may hold the gate for a minute, a job the backend for longer. The
        // request that ended the session waits for neither: the closing then runs on its own
        // task, which ends by itself.
        let login_in_flight = self.0.gate.try_lock().is_err();
        if busy || login_in_flight {
            drop(tokio::spawn(close));
        } else {
            close.await;
        }
    }

    /// How long `address` must still wait after its wrong tries.
    fn wait_for(&self, address: &str) -> u64 {
        let now = self.now();
        let tries = lock(&self.0.tries);
        tries.get(address).map_or(0, |entry| {
            let left = entry.retry_at.saturating_sub(now);
            // Whole seconds, rounded up, so "wait 1 second" is never shown as 0.
            left.as_secs() + u64::from(left.subsec_nanos() > 0)
        })
    }

    fn note_failure(&self, address: &str) {
        let now = self.now();
        let mut tries = lock(&self.0.tries);
        tries.retain(|_, entry| now.saturating_sub(entry.retry_at) < FORGET_TRIES);
        if tries.len() >= MAX_TRACKED {
            tries.clear();
        }
        let entry = tries.entry(address.to_owned()).or_insert(Tries { failures: 0, retry_at: now });
        entry.failures += 1;
        // The core's rule for the wait after wrong tries, so a browser waits as a terminal does.
        let delay = Unlock { failures: entry.failures, ..Unlock::default() }.retry_delay_seconds();
        entry.retry_at = now + Duration::from_secs(delay);
    }

    /// Checks a passphrase with the helper and starts a session.
    ///
    /// One login is in flight at a time, for the whole process: the helper may hold a guess for
    /// up to a minute, and a second one would only queue behind it. Others are told to try again
    /// shortly. An address that guessed wrong waits before its next try is forwarded at all.
    pub async fn login(&self, address: &str, passphrase: &str) -> Result<Arc<Session>, Refusal> {
        self.sweep().await;
        // The core's own check and words for an empty box and for a try that comes too soon.
        let mut check = Unlock::default();
        let mut typed = typed_secret(passphrase);
        let effects = check.submit(&mut typed, self.wait_for(address));
        let Some(nodo_ui_core::Effect::Unlock(secret)) = effects.into_iter().next() else {
            let status = if passphrase.is_empty() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::TOO_MANY_REQUESTS };
            return Err(Refusal::new(status, check.error.unwrap_or_default()));
        };
        let Ok(_gate) = self.0.gate.try_lock() else {
            return Err(Refusal::new(StatusCode::TOO_MANY_REQUESTS, "Another login is being checked. Try again shortly."));
        };
        if self.session_count() >= MAX_SESSIONS {
            return Err(Refusal::new(
                StatusCode::TOO_MANY_REQUESTS,
                format!("{MAX_SESSIONS} browsers are logged in already. Log out in one of them, or wait until one has been idle for 15 minutes."),
            ));
        }
        let backend = match self.check_passphrase(secret).await {
            Ok(backend) => backend,
            Err(error) => {
                if matches!(error, ApiError::BadPassphrase) {
                    self.note_failure(address);
                }
                let status = match error {
                    ApiError::BadPassphrase => StatusCode::UNAUTHORIZED,
                    ApiError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
                    _ => StatusCode::SERVICE_UNAVAILABLE,
                };
                return Err(Refusal::new(status, login_error(&error)));
            }
        };
        lock(&self.0.tries).remove(address);
        let token = new_token()
            .map_err(|error| Refusal::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Could not make a session: {error}")))?;
        let id = self.0.next_id.fetch_add(1, Ordering::SeqCst);
        let session = Arc::new(Session::new(id, token, backend, self.now()));
        // Still under the gate, so no logout for an earlier last session can follow this.
        lock(&self.0.sessions).push(session.clone());
        Ok(session)
    }

    async fn check_passphrase(&self, secret: Secret) -> Result<Box<dyn nodo_ui_core::exec::Data>, ApiError> {
        let backends = self.backends();
        tokio::task::spawn_blocking(move || backends.login(&secret))
            .await
            .unwrap_or_else(|error| Err(ApiError::Failed(format!("the login did not run ({error})"))))
    }
}
