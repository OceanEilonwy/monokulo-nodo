//! Home: what needs the owner, with one button each, then the apps and the device's health.

use std::time::Duration;

use axum::response::Response;
use axum::Form;
use maud::{html, Markup};
use nodo_ui_core::flows::unlock::Unlock;
use nodo_ui_core::flows::{unlock as unlock_flow, Dest, Step};
use nodo_ui_core::model::{AttentionLevel, Snapshot};
use nodo_ui_core::queries::HomeItem;
use nodo_ui_core::screen::Tab;

use super::drive;
use crate::glue::typed_secret;
use nodo_ui_core::words::{home_card, unlock_lines};
use nodo_ui_core::words::{app_state, health_line};
use crate::ctx::{Ctx, Out, Work};
use crate::runner::follow_plain;
use crate::dest;
use crate::layout::{self, Page};

/// Whole seconds left until `at`, rounded up.
fn seconds_until(at: Duration, now: Duration) -> u64 {
    let left = at.saturating_sub(now);
    left.as_secs() + u64::from(left.subsec_nanos() > 0)
}

/// The unlock card: after a login the device can still be partly locked (a helper that
/// restarted since, storage that failed to open), and this is where it is unlocked.
fn unlock_card(snapshot: &Snapshot, head: &str, body: &str, problem: Option<&str>) -> Markup {
    html! {
        section.card.attn #unlock {
            h2 { (head) }
            p { (body) }
            @for line in unlock_lines(snapshot, None) { p.dim { (line) } }
            form.stack method="post" action=(dest::UNLOCK) {
                (layout::passphrase("unlock-passphrase", "Device passphrase", problem, false))
                div.actions.end { button.btn.primary type="submit" { "Unlock" } }
            }
            p.dim.small { "A lost passphrase can't be recovered; a backup brings the data back." }
        }
    }
}

fn page(work: &mut Work<'_>) -> Page {
    let snapshot = &work.snapshot;
    let items = snapshot.home_items();
    let needs: Vec<&HomeItem> = items.iter().filter(|item| !matches!(item, HomeItem::App(_))).collect();
    let notes = &snapshot.overview.attention;
    let title = if needs.is_empty() && notes.is_empty() { "Everything is running" } else { "Your Nodo" };
    // Said once: the reason the last try was refused. The count of wrong tries stays.
    let unlock_problem = work.session.inner().unlock.error.take();
    let apps: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            HomeItem::App(id) => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let problems = needs.iter().any(|item| matches!(item, HomeItem::Failing(_) | HomeItem::Unlock));
    let summary = if apps.is_empty() {
        "No apps yet"
    } else if problems {
        "The other apps"
    } else {
        "Everything else is running"
    };
    let body = html! {
        h1 { (title) }
        @if let Some(failure) = &snapshot.overview.drive { (drive::card(failure)) }
        @for item in &needs {
            @if let Some((head, body, label)) = home_card(snapshot, item) {
                @match item {
                    HomeItem::Unlock => (unlock_card(snapshot, &head, &body, unlock_problem.as_deref())),
                    HomeItem::Failing(id) => section.card.bad {
                        h2 { span.err aria-hidden="true" { "▲ " } (head) }
                        p.dim { (body) }
                        div.actions.end { a.btn.primary href=(dest::log(id)) { (label) } }
                    },
                    HomeItem::Updates => section.card.attn {
                        h2 { (head) }
                        p.dim { (body) }
                        div.actions.end {
                            a.btn.primary href=(dest::url(&Dest::Updates { only: None }, dest::HOME)) { (label) }
                        }
                    },
                    HomeItem::App(_) => {},
                }
            }
        }
        @if !notes.is_empty() {
            section.card {
                ul.bullets {
                    @for note in notes {
                        li {
                            @match note.level {
                                AttentionLevel::Critical => span.err aria-hidden="true" { "✕" },
                                AttentionLevel::Warning => span.warn aria-hidden="true" { "▲" },
                                AttentionLevel::Info => span.dim aria-hidden="true" { "▸" },
                            }
                            span { (note.text.replace('`', "")) }
                        }
                    }
                }
            }
        }
        section.card.flush {
            h2 {
                @if !problems && !apps.is_empty() { span.ok aria-hidden="true" { "✓ " } }
                (summary)
            }
            @if apps.is_empty() {
                p.pad.dim { "Get apps from " a href=(dest::GET_APPS) { "Apps › Get apps" } "." }
            }
            div.rows {
                @for id in &apps {
                    @let (state, bad) = app_state(snapshot, id);
                    (layout::row(&dest::app(id), id, "", html! { span class=[bad.then_some("err")] { (state) } }))
                }
            }
        }
        @let health = health_line(snapshot);
        @if !health.is_empty() {
            section.card {
                h3 { "This Nodo" }
                p { (health) }
            }
        }
    };
    Page::new(Tab::Home, "Home", body)
}

/// `GET /`.
pub async fn show(ctx: Ctx) -> Response {
    ctx.with(dest::HOME, |work| Out::Page(page(work))).await
}

/// `GET /unlock`: the page a flow sends the owner to when what was asked needs the device
/// unlocked (`Dest::Unlock`). It says why, and once unlocked the owner is back on the page
/// that sent them, to press its button again. Home's card is the same form without a reason.
pub async fn unlock_page(ctx: Ctx) -> Response {
    ctx.with(dest::HOME, |work| {
        let (sent, problem, wait) = {
            let mut inner = work.session.inner();
            let wait = inner.unlock_retry_at.map_or(0, |at| seconds_until(at, work.now));
            (inner.stash.unlock_return.clone(), inner.unlock.error.take(), wait)
        };
        // The flow says whether there is anything to unlock.
        let step = unlock_flow::open(&work.snapshot);
        if !matches!(step, Step::Go(Dest::Unlock { .. })) {
            let mut inner = work.session.inner();
            inner.stash.unlock_return = None;
            inner.notices.extend(follow_plain(step).notices);
            return Out::Go(sent.map_or_else(|| dest::HOME.to_owned(), |(_, back)| back));
        }
        let body = html! {
            h1 { "Unlock your Nodo" }
            section.card.attn {
                @for line in unlock_lines(&work.snapshot, sent.as_ref().map(|(reason, _)| reason.as_str())) { p { (line) } }
                // The wait after a wrong try, counted by this process's clock.
                @if wait > 0 { p.warn role="status" { "You can try again in " (nodo_ui_core::words::counted(wait, "second")) "." } }
                form.stack method="post" action=(dest::UNLOCK) {
                    (layout::hidden("from", "page"))
                    (layout::passphrase("unlock-passphrase", "Device passphrase", problem.as_deref(), true))
                    div.actions.end {
                        a.btn href=(sent.as_ref().map_or(dest::HOME, |(_, back)| back.as_str())) { "Leave it locked" }
                        button.btn.primary type="submit" { "Unlock" }
                    }
                }
                p.dim.small { "A lost passphrase can't be recovered; a backup brings the data back." }
            }
        };
        let page = Page::plain("Unlock", body);
        Out::Page(if wait > 0 { page.waiting() } else { page })
    })
    .await
}

/// What the unlock form posts.
#[derive(Debug, Default, serde::Deserialize)]
pub struct UnlockForm {
    #[serde(default)]
    passphrase: super::Typed,
    /// `page` when it came from `/unlock` and not from Home's card.
    #[serde(default)]
    from: String,
}

/// `POST /unlock`, from the card on Home or from the unlock page. The count of wrong tries and
/// the wait after them are the unlock flow's; the session keeps them between requests and the
/// clock is this process's. Unlocked, the owner returns to the page that sent them.
pub async fn unlock(ctx: Ctx, Form(form): Form<UnlockForm>) -> Response {
    let sent = ctx.session.inner().stash.unlock_return.clone();
    let from_page = form.from == "page";
    let back = sent.as_ref().map_or_else(|| dest::HOME.to_owned(), |(_, back)| back.clone());
    ctx.with(back, move |work| {
        let (mut flow, wait) = {
            let inner = work.session.inner();
            (inner.unlock.clone(), inner.unlock_retry_at.map_or(0, |at| seconds_until(at, work.now)))
        };
        // What sent the owner here, in words: with it the flow answers "carry on" (`Resume`).
        flow.resume = sent.map(|(reason, _)| reason);
        let failures = flow.failures;
        let mut typed = typed_secret(form.passphrase.expose());
        let effects = flow.submit(&mut typed, wait);
        let reply = work.run(Step::run(effects), &mut |outcome, snapshot| flow.apply(outcome, snapshot));
        let mut inner = work.session.inner();
        if flow.failures > failures {
            inner.unlock_retry_at = Some(work.now + Duration::from_secs(flow.retry_delay_seconds()));
        }
        if reply.go.is_some() {
            // Unlocked: the next lock starts from nothing.
            inner.unlock = Unlock::default();
            inner.unlock_retry_at = None;
            inner.stash.unlock_return = None;
            return Out::Reply(reply);
        }
        inner.unlock = flow;
        inner.notices.extend(reply.notices);
        // Refused: the form again, where it was sent from.
        Out::Go(if from_page { dest::UNLOCK } else { dest::HOME }.to_owned())
    })
    .await
}
