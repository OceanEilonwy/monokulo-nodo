//! Logs (`flows::logs`): one source's lines, only its problems, a search through them, and
//! following it live.
//!
//! The page keeps nothing on the server: which source, whether only problems are listed, what is
//! searched for and whether the log is followed are all in the URL, and the flow is made from
//! them for each request. Following is a fragment that asks for the lines again every
//! `FOLLOW_SECONDS`; that route does not count as the owner's activity.

use axum::extract::{Path, Query};
use axum::response::Response;
use maud::{html, Markup};
use nodo_ui_core::flows::logs::{Logs, FOLLOW_SECONDS};
use nodo_ui_core::flows::{documents, Step};
use nodo_ui_core::model::{LogKind, Snapshot};
use nodo_ui_core::screen::Tab;
use serde::Deserialize;

use crate::ctx::{Ctx, Out, Work};
use crate::dest;
use crate::layout::{self, Page};
use crate::runner::no_flow;

/// The newest lines are where a failure is, so a long log shows its end.
const MAX_LINES: usize = 400;

#[derive(Debug, Default, Clone, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    find: String,
    /// `1` lists only lines that report a problem, `0` every line.
    problems: Option<u8>,
    /// `1` reads the log again every few seconds, `0` leaves it as read.
    follow: Option<u8>,
    /// `1` when the source was chosen among the device logs, and not opened from an app's page.
    device: Option<u8>,
}

/// The flow for a request: the device logs or one source's, then what the URL says.
fn flow_for(snapshot: &Snapshot, source: &str, query: &LogQuery) -> Logs {
    let mut flow = if query.device == Some(1) { Logs::device() } else { Logs::of(snapshot, source) };
    if query.problems.is_some_and(|wanted| (wanted == 1) != flow.errors_only) {
        flow.toggle_errors_only();
    }
    if query.follow.is_some_and(|wanted| (wanted == 1) != flow.follow) {
        flow.toggle_follow();
    }
    flow
}

/// The URL that shows `flow` as it is, for `source`.
fn url(base: &str, flow: &Logs, find: &str) -> String {
    let mut url = format!("{base}?problems={}&follow={}", u8::from(flow.errors_only), u8::from(flow.follow));
    if flow.app.is_none() {
        url.push_str("&device=1");
    }
    if !find.is_empty() {
        url.push_str(&format!("&find={}", dest::seg(find)));
    }
    url
}

/// The lines themselves. While the log is followed, htmx replaces this with a fresh read.
fn lines(flow: &Logs, source: &str, find: &str) -> Markup {
    let listed = flow.lines(find);
    let skip = listed.len().saturating_sub(MAX_LINES);
    let poll = url(&format!("{}/lines", dest::log(source)), flow, find);
    html! {
        section.stack #log hx-get=[flow.follow.then_some(poll)] hx-trigger=[flow.follow.then(|| format!("every {FOLLOW_SECONDS}s"))] hx-swap="outerHTML" {
            @if listed.is_empty() {
                p.dim { (flow.empty_text()) }
            } @else {
                pre.text {
                    @for line in listed.iter().skip(skip) {
                        @match line.kind {
                            LogKind::Info => span { (line.text) },
                            LogKind::Error => span.e { (line.text) },
                            LogKind::Marker => span.m { (line.text) },
                        }
                        "\n"
                    }
                }
            }
            @if !flow.view.suggestion.is_empty() {
                ul.bullets {
                    @for suggestion in &flow.view.suggestion {
                        li { span.dim aria-hidden="true" { "▸" } span { (suggestion.replace('`', "")) } }
                    }
                }
            }
        }
    }
}

/// Reads the source's log into a flow made for this request.
fn read(work: &mut Work<'_>, source: &str, query: &LogQuery) -> Logs {
    let mut flow = flow_for(&work.snapshot, source, query);
    // The reply carries nothing: a log that cannot be read is an empty one with a suggestion.
    let _reply = work.run(Step::run(Logs::load(source)), &mut |outcome, _| flow.apply(outcome, Some(source)));
    flow
}

fn page(snapshot: &Snapshot, flow: &Logs, source: &str, find: &str) -> Page {
    let base = dest::log(source);
    let toggled = |change: fn(&mut Logs)| {
        let mut other = flow.clone();
        change(&mut other);
        url(&base, &other, find)
    };
    let body = html! {
        div.head {
            h1 { @if flow.app.is_some() { (source) " logs" } @else { "Device logs" } }
            div.actions { (layout::post(&format!("{base}/save"), "Save to a file", "btn")) }
        }
        form.searchbar method="get" action=(base) role="search" {
            input type="search" name="find" value=(find) placeholder="Search this log" aria-label="Search this log" autocomplete="off";
            (layout::hidden("problems", &u8::from(flow.errors_only).to_string()))
            (layout::hidden("follow", &u8::from(flow.follow).to_string()))
            @if flow.app.is_none() { (layout::hidden("device", "1")) }
            button.btn type="submit" { "Search" }
        }
        nav.chips aria-label="What is listed" {
            a.chip href=(toggled(Logs::toggle_errors_only)) aria-current=[flow.errors_only.then_some("true")] { "Problems only" }
            a.chip href=(toggled(Logs::toggle_follow)) aria-current=[flow.follow.then_some("true")] {
                @if flow.follow { "● Following live" } @else { "○ Paused" }
            }
        }
        @if flow.app.is_none() {
            nav.chips aria-label="Sources" {
                @for name in &snapshot.log_sources {
                    a.chip href=(url(&dest::log(name), flow, find)) aria-current=[(name == source).then_some("true")] { (name) }
                }
            }
        }
        section.card {
            @if flow.app.is_none() { h2 { (source) } }
            (lines(flow, source, find))
        }
    };
    let page = match &flow.app {
        Some(name) => Page::new(Tab::Apps, format!("{name} logs"), body).trail(&[("Apps", dest::APPS), (name, &dest::app(name)), ("Logs", &base)]),
        None => Page::new(Tab::System, "Device logs", body).trail(&[("System", dest::SYSTEM), ("Device logs", dest::LOGS)]),
    };
    // Without scripts a followed log is read again by loading the page again.
    if flow.follow { page.waiting() } else { page }
}

/// `GET /system/logs`: the device logs, every source to choose from, problems first.
pub async fn device(ctx: Ctx, Query(query): Query<LogQuery>) -> Response {
    ctx.with(dest::SYSTEM, move |work| {
        let Some(source) = work.snapshot.log_sources.first().cloned() else {
            let body = html! { h1 { "Device logs" } section.card { p.dim { "Nothing here keeps a log yet." } } };
            return Out::Page(Page::new(Tab::System, "Device logs", body).trail(&[("System", dest::SYSTEM), ("Device logs", dest::LOGS)]));
        };
        let query = LogQuery { device: Some(1), ..query };
        let flow = read(work, &source, &query);
        Out::Page(page(&work.snapshot, &flow, &source, &query.find))
    })
    .await
}

/// `GET /system/logs/{source}`: one source's log, an app's or a device part's.
pub async fn show(ctx: Ctx, Path(source): Path<String>, Query(query): Query<LogQuery>) -> Response {
    ctx.with(dest::LOGS, move |work| {
        let flow = read(work, &source, &query);
        Out::Page(page(&work.snapshot, &flow, &source, &query.find))
    })
    .await
}

/// `GET /system/logs/{source}/lines`: the lines alone, for a followed log. A polling route.
pub async fn poll(ctx: Ctx, Path(source): Path<String>, Query(query): Query<LogQuery>) -> Response {
    ctx.with(dest::LOGS, move |work| {
        let mut flow = flow_for(&work.snapshot, &source, &query);
        // The flow says whether a followed log is read again; a paused one keeps what it shows.
        let effects = flow.follow_up(Some(&source));
        let _reply = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome, Some(&source)));
        Out::Fragment(lines(&flow, &source, &query.find))
    })
    .await
}

/// `POST /system/logs/{source}/save`: saves the log where the owner can fetch it. The helper
/// answers with where it put it, as a notice.
pub async fn save(ctx: Ctx, Path(source): Path<String>) -> Response {
    ctx.with(dest::log(&source), move |work| Out::Reply(work.run(Step::run(documents::log_report(&source)), &mut no_flow))).await
}
