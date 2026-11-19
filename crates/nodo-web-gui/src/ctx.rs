//! What every page behind the login is written with.
//!
//! A handler takes a [`Ctx`] and calls one of:
//!
//! - [`Ctx::with`]: runs a closure on the blocking pool with the session's backend. The closure
//!   gets a [`Work`] (the backend, the device's `Snapshot`, the step runner) and returns an
//!   [`Out`]: a page, a fragment, or a `Reply` to answer with.
//! - [`Ctx::job`]: the same for a call that takes minutes. It answers at once with the page
//!   that waits for it.
//! - [`Ctx::respond`]: answers with a `Reply` made without the backend.
//!
//! Helper calls block, for minutes at worst, so none is made on a tokio worker. A session's
//! backend is one connection behind a mutex: while a job holds it, the session's other requests
//! do not wait for it but say what is running (`Stop::Working`).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, TryLockError};
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::header::{self, HeaderValue};
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::{html, Markup};
use nodo_api::WebGuiStatus;
use nodo_ui_core::exec::{load_snapshot, Data};
use nodo_ui_core::flows::staged;
use nodo_ui_core::flows::{Notice, Step};
use nodo_ui_core::model::Snapshot;
use nodo_ui_core::screen::Tab;

use crate::backends;
use crate::glue::job_failed;
use nodo_ui_core::words::tab_badge;
use crate::dest;
use crate::layout::{self, Chrome, Page};
use crate::runner::{self, PageApply, Reply};
use crate::session::{lock, Elsewhere, Job, JobEnd, JobView, Session};
use crate::state::AppState;

/// Marks a route a page calls by itself, again and again: the job's progress, a followed log.
/// Such a request does not count as the owner's activity, so an open tab does not keep a
/// session alive. Routes carry it as a layer: `.layer(axum::Extension(Polling))`.
#[derive(Debug, Clone, Copy)]
pub struct Polling;

/// A request by a logged-in browser.
pub struct Ctx {
    pub state: AppState,
    pub session: Arc<Session>,
    /// Sent by htmx, which swaps what comes back into the page instead of loading a new one.
    pub htmx: bool,
    /// Not a GET: it changes something, and is answered with a redirect.
    pub post: bool,
    /// The path and query asked for.
    pub here: String,
}

/// A redirect after a form was posted, so a reload does not post it again.
pub fn see_other(url: &str) -> Response {
    let location = HeaderValue::from_str(url).unwrap_or(HeaderValue::from_static(dest::HOME));
    (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response()
}

/// Sends the browser to `url`. htmx follows a redirect inside its own request and would swap the
/// page it lands on into a fragment's place, so it is told with its own header to load the page.
pub fn go_to(htmx: bool, url: &str) -> Response {
    if htmx {
        let location = HeaderValue::from_str(url).unwrap_or(HeaderValue::from_static(dest::HOME));
        (StatusCode::OK, [("hx-redirect", location)]).into_response()
    } else {
        see_other(url)
    }
}

impl FromRequestParts<AppState> for Ctx {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let htmx = parts.headers.contains_key("hx-request");
        let polling = parts.extensions.get::<Polling>().is_some();
        let session = match state.config().cookie.read(&parts.headers) {
            Some(token) => state.authenticate(&token, !polling).await,
            None => None,
        };
        let Some(session) = session else {
            return Err(go_to(htmx, dest::LOGIN));
        };
        let here = parts.uri.path_and_query().map_or_else(|| parts.uri.path().to_owned(), |pq| pq.as_str().to_owned());
        Ok(Ctx { state: state.clone(), session, htmx, post: !matches!(parts.method, Method::GET | Method::HEAD), here })
    }
}

/// What a page's closure works with, on the blocking pool, holding the session's backend.
pub struct Work<'a> {
    pub backend: &'a mut dyn Data,
    /// The device as read at the start, and again after every effect that changed it.
    pub snapshot: Snapshot,
    /// Whether the web GUI is on and where, for the System page.
    pub web_gui: Option<WebGuiStatus>,
    pub session: &'a Session,
    /// The state's clock when the request came.
    pub now: Duration,
    login_required: bool,
}

impl Work<'_> {
    /// Follows a step to its end: runs its effects, hands every outcome to `page` and to the
    /// flows without state, and follows what they answer. See `runner::run_steps`.
    pub fn run(&mut self, step: Step, page: PageApply<'_>) -> Reply {
        let reply = runner::run_steps(self.backend, &mut self.snapshot, step, page);
        self.login_required |= reply.login_required;
        reply
    }
}

/// What a page's closure answers with.
pub enum Out {
    /// A whole page, drawn inside the frame.
    Page(Page),
    /// A piece of a page, for htmx to swap in.
    Fragment(Markup),
    /// What a followed step came to: notices, a question, a page to go to.
    Reply(Reply),
    /// Another page, with nothing to say.
    Go(String),
    /// What the closure found must be done takes minutes: start it as a job. For an action that
    /// is only known to be long once the device was read (updating apps is, loading the review
    /// one of them needs first is not).
    Job(JobSpec, Box<dyn FnOnce(&mut Work<'_>) -> Reply + Send>),
}

/// Why a closure did not run to its answer.
pub enum Stop {
    /// A job holds the backend.
    Working(JobView),
    /// The helper wants a new login.
    Login,
    /// The device could not be read.
    Failed(String),
}

/// Checks the gate, reads the device, and runs `work`. Shared by requests and jobs; the caller
/// holds the backend's lock.
fn open<R>(
    backend: &mut dyn Data,
    session: &Session,
    now: Duration,
    work: impl FnOnce(&mut Work<'_>) -> R,
) -> Result<R, Stop> {
    // The gate first, as a value: a helper that restarted answers "not open" here.
    let web_gui = match backends::status(backend) {
        Ok(status) if !status.open => return Err(Stop::Login),
        Ok(status) => Some(status),
        Err(error) if error.is_web_login_required() => return Err(Stop::Login),
        Err(_) => None,
    };
    // Then the device, through `ListServices`, which keeps the typed error. A refusal for want
    // of a login is checked before anything treats `Locked` as "the device is locked".
    let snapshot = match load_snapshot(backend) {
        Ok(snapshot) => snapshot,
        Err(error) if error.is_web_login_required() => return Err(Stop::Login),
        Err(error) => return Err(Stop::Failed(format!("Could not read device state: {error}"))),
    };
    let mut work_state = Work { backend, snapshot, web_gui, session, now, login_required: false };
    let out = work(&mut work_state);
    if work_state.login_required {
        return Err(Stop::Login);
    }
    let mut inner = session.inner();
    inner.badges = (tab_badge(&work_state.snapshot, Tab::Apps), tab_badge(&work_state.snapshot, Tab::System));
    inner.unsaved = staged::total(&inner.stash.staged(&work_state.snapshot));
    drop(inner);
    Ok(out)
}

/// A job to start: what it is called while it runs and where it leads.
pub struct JobSpec {
    /// A heading for the wait: "Installing moneropay".
    pub what: String,
    /// What to expect meanwhile, a line each.
    pub note: Vec<String>,
    /// Where `Dest::Back` leads once it is over.
    pub back: String,
    /// What to say if the owner is on another page when it ends. Without it the job's own
    /// notices are shown there.
    pub elsewhere: Option<Elsewhere>,
}

impl JobSpec {
    pub fn new(what: impl Into<String>, back: impl Into<String>) -> Self {
        Self { what: what.into(), note: Vec::new(), back: back.into(), elsewhere: None }
    }

    pub fn note(mut self, lines: &[&str]) -> Self {
        self.note = lines.iter().map(|line| (*line).to_owned()).collect();
        self
    }

    pub fn elsewhere(mut self, say: impl Fn(&Reply) -> Vec<Notice> + Send + 'static) -> Self {
        self.elsewhere = Some(Box::new(say));
        self
    }
}

impl Ctx {
    /// Shows a notice on the next page drawn for this session.
    pub fn flash(&self, notice: Notice) {
        self.session.inner().notices.push(notice);
    }

    fn chrome(&self) -> Chrome {
        let mut inner = self.session.inner();
        Chrome { signed_in: true, badges: inner.badges, unsaved: inner.unsaved, notices: std::mem::take(&mut inner.notices) }
    }

    /// Draws a page inside the frame, with the notices waiting for this session.
    pub fn page(&self, page: Page) -> Response {
        let status = page.status;
        (status, layout::document(&page, &self.chrome())).into_response()
    }

    fn to_login(&self) -> Response {
        self.state.drop_all();
        go_to(self.htmx, dest::LOGIN)
    }

    /// A job that ended while the owner was elsewhere is told on the page they open next.
    fn tell_finished_job(&self) {
        let Some(job) = self.session.take_finished_job() else {
            return;
        };
        match job.end {
            Some(JobEnd::Reply(reply)) => {
                let mut said = reply.notices.clone();
                if let Some(elsewhere) = &job.elsewhere {
                    said.extend(elsewhere(&reply));
                }
                let mut inner = self.session.inner();
                inner.notices.extend(said);
                // The page the job led to can still be opened.
                if let Some(dest) = &reply.go {
                    inner.stash.keep(dest, &job.back);
                }
            }
            Some(JobEnd::LoginRequired) => self.state.drop_all(),
            Some(JobEnd::Failed(reason)) => self.flash(job_failed(&job.what, &reason)),
            None => {}
        }
    }

    /// What a request gets while a job holds the backend: the page that waits for it, which
    /// then comes back here. A request that would change something is not queued behind the
    /// job; it is refused with a remark.
    fn busy(&self, job: &JobView, back: &str) -> Response {
        if self.post {
            self.flash(Notice::info(format!("The Nodo is busy: {}. Try again once that is done.", job.what)));
            return go_to(self.htmx, back);
        }
        if self.htmx {
            // A fragment asked for by a page that is still open. It keeps what it shows.
            return StatusCode::NO_CONTENT.into_response();
        }
        let page = Page::plain(job.what.clone(), layout::working(job, Some(&self.here))).waiting();
        self.page(page)
    }

    fn stopped(&self, stop: Stop, back: &str) -> Response {
        match stop {
            Stop::Login => self.to_login(),
            Stop::Working(job) => self.busy(&job, back),
            Stop::Failed(reason) => {
                let body = html! {
                    section.card.attn {
                        h1 { "The Nodo did not answer" }
                        p { (reason) }
                        p.dim { "Its helper may be restarting. Try again in a moment." }
                        a.btn href=(self.here) { "Try again" }
                    }
                };
                self.page(Page::plain("The Nodo did not answer", body).status(StatusCode::BAD_GATEWAY))
            }
        }
    }

    /// Runs `work` on the blocking pool with the session's backend and answers with what it
    /// returns. `back` is this page's parent: where `Dest::Back` leads and where a reply with no
    /// destination returns to.
    pub async fn with<F>(self, back: impl Into<String>, work: F) -> Response
    where
        F: FnOnce(&mut Work<'_>) -> Out + Send + 'static,
    {
        let back = back.into();
        self.tell_finished_job();
        if let Some(job) = self.session.running_job(self.state.clock()) {
            return self.busy(&job, &back);
        }
        let (state, session, now) = (self.state.clone(), self.session.clone(), self.state.now());
        let done = tokio::task::spawn_blocking(move || {
            let mut backend = match session.backend.try_lock() {
                Ok(backend) => backend,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                // Held by a job that started just now, or by this session's other request,
                // which is over in a moment and can be waited for here, off the workers.
                Err(TryLockError::WouldBlock) => match session.running_job(state.clock()) {
                    Some(job) => return Err(Stop::Working(job)),
                    None => lock(&session.backend),
                },
            };
            open(&mut **backend, &session, now, work)
        })
        .await;
        match done {
            Ok(Ok(Out::Page(page))) => self.page(page),
            Ok(Ok(Out::Fragment(markup))) => markup.into_response(),
            Ok(Ok(Out::Reply(reply))) => self.respond(reply, &back),
            Ok(Ok(Out::Go(url))) => go_to(self.htmx, &url),
            Ok(Ok(Out::Job(spec, work))) => self.job(spec, work).await,
            Ok(Err(stop)) => self.stopped(stop, &back),
            Err(error) => self.stopped(Stop::Failed(format!("The request stopped unexpectedly ({error}).")), &back),
        }
    }

    /// Answers with what a followed step came to:
    ///
    /// - its notices are kept for the next page drawn;
    /// - a question becomes the confirmation dialog (a fragment for htmx, its own page without);
    /// - a destination becomes a redirect, through `dest::url`, and what it carries is kept for
    ///   the page it leads to;
    /// - otherwise the browser returns to `back`.
    pub fn respond(&self, reply: Reply, back: &str) -> Response {
        if reply.login_required {
            return self.to_login();
        }
        let mut inner = self.session.inner();
        inner.notices.extend(reply.notices);
        if let Some(question) = reply.question {
            let pending = inner.ask(question, back, reply.cancel.as_deref().unwrap_or(back));
            drop(inner);
            return if self.htmx { layout::dialog(&pending).into_response() } else { see_other(dest::CONFIRM) };
        }
        let url = match &reply.go {
            Some(dest) => dest::land(dest, back, &mut inner.stash),
            None => back.to_owned(),
        };
        drop(inner);
        go_to(self.htmx, &url)
    }

    /// Starts a call that takes minutes and answers with the page that waits for it. `work`
    /// runs on the blocking pool holding the session's backend; what it returns is told when
    /// the browser next asks (`/working`), or on whatever page the owner has gone to by then.
    pub async fn job<F>(self, spec: JobSpec, work: F) -> Response
    where
        F: FnOnce(&mut Work<'_>) -> Reply + Send + 'static,
    {
        self.tell_finished_job();
        if let Some(job) = self.session.running_job(self.state.clock()) {
            return self.busy(&job, &spec.back);
        }
        let now = self.state.now();
        let id = {
            let mut inner = self.session.inner();
            let id = inner.next_job_id();
            inner.job =
                Some(Job { id, what: spec.what, note: spec.note, started: now, back: spec.back, end: None, elsewhere: spec.elsewhere });
            id
        };
        let (state, session) = (self.state.clone(), self.session.clone());
        // Not awaited: the job outlives this request. Its end is kept in the session.
        drop(tokio::task::spawn_blocking(move || {
            let ran = catch_unwind(AssertUnwindSafe(|| {
                let mut backend = lock(&session.backend);
                open(&mut **backend, &session, now, work)
            }));
            let end = match ran {
                Ok(Ok(reply)) if reply.login_required => JobEnd::LoginRequired,
                Ok(Ok(reply)) => JobEnd::Reply(reply),
                Ok(Err(Stop::Login)) => JobEnd::LoginRequired,
                Ok(Err(Stop::Failed(reason))) => JobEnd::Failed(reason),
                Ok(Err(Stop::Working(_))) => JobEnd::Failed("another job was running".to_owned()),
                Err(_) => JobEnd::Failed("it stopped unexpectedly".to_owned()),
            };
            // The idle time counts from here, not from the click that started the job.
            session.touch(state.now());
            if let Some(job) = session.inner().job.as_mut().filter(|job| job.id == id) {
                job.end = Some(end);
            }
        }));
        go_to(self.htmx, dest::WORKING)
    }

    /// The page and the fragment that wait for a job (`/working`). While it runs they show what
    /// is running and for how long. Once it is over the browser goes on: back to the page that
    /// was waiting (`to`), or to wherever the job's reply leads.
    pub fn working(self, to: Option<&str>) -> Response {
        if let Some(job) = self.session.running_job(self.state.clock()) {
            return if self.htmx {
                layout::working(&job, to).into_response()
            } else {
                self.page(Page::plain(job.what.clone(), layout::working(&job, to)).waiting())
            };
        }
        if let Some(to) = to {
            // That page tells how the job ended, as any page opened after it would.
            return go_to(self.htmx, to);
        }
        let Some(job) = self.session.take_finished_job() else {
            return go_to(self.htmx, dest::HOME);
        };
        match job.end {
            Some(JobEnd::Reply(reply)) => self.respond(reply, &job.back),
            Some(JobEnd::LoginRequired) => self.to_login(),
            Some(JobEnd::Failed(reason)) => {
                self.flash(job_failed(&job.what, &reason));
                go_to(self.htmx, &job.back)
            }
            None => go_to(self.htmx, dest::HOME),
        }
    }
}
