//! Home: what needs the owner, with one button each, then the apps and the device's health.

use std::time::Duration;

use axum::response::Response;
use axum::Form;
use maud::{html, Markup};
use nodo_ui_core::flows::unlock::Unlock;
use nodo_ui_core::flows::{Dest, Step};
use nodo_ui_core::model::{AttentionLevel, Snapshot};
use nodo_ui_core::queries::HomeItem;
use nodo_ui_core::screen::Tab;

use super::{drive, PassForm};
use crate::core_wishlist::{app_state, health, home_card, typed_secret, unlock_lines};
use crate::ctx::{Ctx, Out, Work};
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
            @for line in unlock_lines(snapshot) { p.dim { (line) } }
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
        @let facts = health(snapshot);
        @if !facts.is_empty() {
            section.card {
                h3 { "This Nodo" }
                dl.facts {
                    @for (label, value) in &facts { dt { (label) } dd { (value) } }
                }
            }
        }
    };
    Page::new(Tab::Home, "Home", body)
}

/// `GET /`.
pub async fn show(ctx: Ctx) -> Response {
    ctx.with(dest::HOME, |work| Out::Page(page(work))).await
}

/// `POST /unlock`, from the card on Home. The count of wrong tries and the wait after them are
/// the unlock flow's; the session keeps them between requests and the clock is this process's.
pub async fn unlock(ctx: Ctx, Form(form): Form<PassForm>) -> Response {
    ctx.with(dest::HOME, move |work| {
        let (mut flow, wait) = {
            let inner = work.session.inner();
            (inner.unlock.clone(), inner.unlock_retry_at.map_or(0, |at| seconds_until(at, work.now)))
        };
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
        } else {
            inner.unlock = flow;
        }
        Out::Reply(reply)
    })
    .await
}
