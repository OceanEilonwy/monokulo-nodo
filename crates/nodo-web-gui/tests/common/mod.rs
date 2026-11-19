//! What the tests share: the router over `MockBackend` with a clock that moves when told, a
//! backend factory that records and can be made to refuse, and a small browser.
//!
//! The application is configured as on the device (`Config::device`): the cookie is
//! `__Host-nodo`, requests name `nodo.local` and posts carry `Origin: https://nodo.local`.

// Each test file uses its own part of this.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use nodo_api::{ApiError, Backend, Request as HelperRequest, WebGuiStatus};
use nodo_ui_core::exec::Data;
use nodo_ui_core::mock::{MockBackend, MOCK_PASSPHRASE};
use nodo_ui_core::secret::Secret;
use nodo_web_gui::backends::Backends;
use nodo_web_gui::clock::ManualClock;
use nodo_web_gui::session::Session;
use nodo_web_gui::{router, AppState, Config};
use tower::ServiceExt;

pub const HOST: &str = "nodo.local";
pub const ORIGIN: &str = "https://nodo.local";

/// A gate that holds whoever waits at it until it is opened.
#[derive(Default)]
pub struct Hold {
    open: Mutex<bool>,
    changed: Condvar,
    /// Somebody is waiting at it now.
    pub reached: AtomicBool,
}

impl Hold {
    pub fn wait(&self) {
        self.reached.store(true, Ordering::SeqCst);
        let mut open = self.open.lock().expect("the hold's lock");
        while !*open {
            open = self.changed.wait(open).expect("the hold's lock");
        }
    }

    pub fn release(&self) {
        *self.open.lock().expect("the hold's lock") = true;
        self.changed.notify_all();
    }
}

/// A fresh `MockBackend` per session, like `MockBackends`, that also records what the helper's
/// gate would have been asked and can be made to answer as a helper in trouble does.
pub struct TestBackends {
    /// `login` and `logout`, in the order they reached the helper.
    pub gate_calls: Mutex<Vec<&'static str>>,
    /// The next logins fail with this.
    pub login_error: Mutex<Option<ApiError>>,
    /// False once the helper has closed its gate by itself, as after a restart.
    pub gate_open: AtomicBool,
    /// Logging in leaves the device locked, as when its storage failed to open.
    pub stay_locked: AtomicBool,
    /// A login waits here, as the helper makes a guess wait.
    pub login_hold: Mutex<Option<Arc<Hold>>>,
    /// Changes each new session's device before it is used.
    pub prepare: Mutex<Box<dyn Fn(&mut MockBackend) + Send>>,
}

impl Default for TestBackends {
    fn default() -> Self {
        Self {
            gate_calls: Mutex::new(Vec::new()),
            login_error: Mutex::new(None),
            gate_open: AtomicBool::new(true),
            stay_locked: AtomicBool::new(false),
            login_hold: Mutex::new(None),
            prepare: Mutex::new(Box::new(|_| {})),
        }
    }
}

impl TestBackends {
    pub fn calls(&self) -> Vec<&'static str> {
        self.gate_calls.lock().expect("the record").clone()
    }

    pub fn prepare(&self, change: impl Fn(&mut MockBackend) + Send + 'static) {
        *self.prepare.lock().expect("the preparation") = Box::new(change);
    }
}

impl Backends for TestBackends {
    fn login(&self, passphrase: &Secret) -> Result<Box<dyn Data>, ApiError> {
        let hold = self.login_hold.lock().expect("the hold").clone();
        if let Some(hold) = hold {
            hold.wait();
        }
        if let Some(error) = self.login_error.lock().expect("the error").clone() {
            return Err(error);
        }
        if passphrase.expose() != MOCK_PASSPHRASE {
            return Err(ApiError::BadPassphrase);
        }
        let mut backend = MockBackend::new();
        if !self.stay_locked.load(Ordering::SeqCst) {
            backend.call(HelperRequest::Unlock { passphrase: nodo_api::Secret::new(passphrase.expose()) })?;
        }
        (self.prepare.lock().expect("the preparation"))(&mut backend);
        self.gate_open.store(true, Ordering::SeqCst);
        self.gate_calls.lock().expect("the record").push("login");
        Ok(Box::new(backend))
    }

    fn logout(&self, _backend: &mut dyn Data) -> Result<(), ApiError> {
        self.gate_calls.lock().expect("the record").push("logout");
        self.gate_open.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn status(&self, _backend: &mut dyn Data) -> Result<WebGuiStatus, ApiError> {
        let open = self.gate_open.load(Ordering::SeqCst);
        Ok(WebGuiStatus { enabled: true, url: open.then(|| nodo_api::WEBGUI_URL.to_owned()), waiting: false, open })
    }
}

/// An answer, read whole.
pub struct Answer {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Answer {
    pub fn header(&self, name: &str) -> &str {
        self.headers.get(name).and_then(|value| value.to_str().ok()).unwrap_or_default()
    }

    /// Where a redirect leads: `Location`, or htmx's own header.
    pub fn location(&self) -> &str {
        let location = self.header("location");
        if location.is_empty() { self.header("hx-redirect") } else { location }
    }

    #[track_caller]
    pub fn assert_has(&self, text: &str) -> &Self {
        assert!(self.body.contains(text), "expected {text:?} in ({}):\n{}", self.status, self.body);
        self
    }

    #[track_caller]
    pub fn assert_lacks(&self, text: &str) -> &Self {
        assert!(!self.body.contains(text), "did not expect {text:?} in:\n{}", self.body);
        self
    }
}

/// Every place in a page that names a key of the terminal interface ("press o", "Press s to"):
/// the word, a single lower-case letter, and then no further letter. A browser has no such keys.
pub fn keys_named(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    for marker in ["Press ", "press "] {
        for (at, _) in body.match_indices(marker) {
            let mut rest = body[at + marker.len()..].chars();
            let key = rest.next().is_some_and(|c| c.is_ascii_lowercase());
            let ends = rest.next().is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if key && ends {
                found.push(body[at..].chars().take(40).collect());
            }
        }
    }
    found
}

pub struct App {
    pub router: Router,
    pub state: AppState,
    pub clock: Arc<ManualClock>,
    pub backends: Arc<TestBackends>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self::with(Config::device())
    }

    pub fn with(config: Config) -> Self {
        let clock = Arc::new(ManualClock::default());
        let backends = Arc::new(TestBackends::default());
        let state = AppState::new(backends.clone(), clock.clone(), config);
        Self { router: router(state.clone()), state, clock, backends }
    }

    /// Sends one request, as built.
    pub async fn send(&self, request: Request<Body>) -> Answer {
        let response = self.router.clone().oneshot(request).await.expect("the router answers");
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, 4 * 1024 * 1024).await.expect("a body");
        Answer { status: parts.status, headers: parts.headers, body: String::from_utf8_lossy(&bytes).into_owned() }
    }

    /// A browser with no session yet.
    pub fn visitor(&self) -> Browser<'_> {
        Browser { app: self, cookie: None, htmx: false, address: None }
    }

    /// A browser that has logged in with the device passphrase.
    pub async fn login(&self) -> Browser<'_> {
        let mut browser = self.visitor();
        let answer = browser.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await;
        assert_eq!(answer.status, StatusCode::SEE_OTHER, "the login was refused: {}", answer.body);
        assert!(browser.cookie.is_some(), "the login set no cookie");
        browser
    }

    /// The session a browser holds, to look into or to prepare.
    pub async fn session(&self, browser: &Browser<'_>) -> Arc<Session> {
        let token = browser.token().expect("the browser is logged in");
        self.state.authenticate(&token, false).await.expect("the session exists")
    }
}

/// `application/x-www-form-urlencoded`, as a browser sends a form.
pub fn form(fields: &[(&str, &str)]) -> String {
    fields.iter().map(|(name, value)| format!("{name}={}", nodo_web_gui::dest::seg(value))).collect::<Vec<_>>().join("&")
}

/// One browser: it keeps the cookie it is given and says where its posts come from.
#[derive(Clone)]
pub struct Browser<'a> {
    pub app: &'a App,
    /// `name=value`, as sent back.
    pub cookie: Option<String>,
    /// Sends requests the way htmx does.
    pub htmx: bool,
    /// The address Caddy reports for it.
    pub address: Option<&'static str>,
}

impl Browser<'_> {
    pub fn token(&self) -> Option<String> {
        self.cookie.as_ref().and_then(|cookie| cookie.split_once('=')).map(|(_, token)| token.to_owned())
    }

    fn request(&self, method: Method, path: &str) -> axum::http::request::Builder {
        let mut request = Request::builder().method(method).uri(path).header(header::HOST, HOST);
        if let Some(cookie) = &self.cookie {
            request = request.header(header::COOKIE, cookie);
        }
        if self.htmx {
            request = request.header("hx-request", "true");
        }
        if let Some(address) = self.address {
            request = request.header("x-forwarded-for", address);
        }
        request
    }

    fn keep_cookie(&mut self, answer: &Answer) {
        let set = answer.header("set-cookie");
        if let Some(pair) = set.split(';').next().filter(|pair| !pair.is_empty()) {
            self.cookie = if set.contains("Max-Age=0") { None } else { Some(pair.to_owned()) };
        }
    }

    pub async fn get(&mut self, path: &str) -> Answer {
        let request = self.request(Method::GET, path).body(Body::empty()).expect("a request");
        let answer = self.app.send(request).await;
        self.keep_cookie(&answer);
        answer
    }

    pub async fn post(&mut self, path: &str, fields: &[(&str, &str)]) -> Answer {
        let request = self
            .request(Method::POST, path)
            .header(header::ORIGIN, ORIGIN)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form(fields)))
            .expect("a request");
        let answer = self.app.send(request).await;
        self.keep_cookie(&answer);
        answer
    }

    /// Posts, then loads the page the answer redirects to, as a browser does.
    pub async fn post_then(&mut self, path: &str, fields: &[(&str, &str)]) -> Answer {
        let answer = self.post(path, fields).await;
        assert_eq!(answer.status, StatusCode::SEE_OTHER, "{path} did not redirect: {}", answer.body);
        let next = answer.location().to_owned();
        self.get(&next).await
    }

    /// The value of the first form field of this name on a page.
    pub fn field(body: &str, name: &str) -> String {
        let marker = format!("name=\"{name}\" value=\"");
        let start = body.find(&marker).unwrap_or_else(|| panic!("no field {name} in:\n{body}")) + marker.len();
        body[start..].split('"').next().unwrap_or_default().to_owned()
    }

    /// Waits for the session's job on `/working` and returns where it leads.
    pub async fn wait_for_job(&mut self) -> Answer {
        for _ in 0..400 {
            let answer = self.get("/working").await;
            if answer.status == StatusCode::SEE_OTHER {
                return answer;
            }
            assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the job did not end");
    }
}
