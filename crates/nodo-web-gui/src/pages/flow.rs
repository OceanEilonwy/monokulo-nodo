//! The pages every feature shares: waiting for a job, answering a question, reading a document.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Form;
use maud::html;
use nodo_ui_core::flows::setup::Setup;
use nodo_ui_core::flows::{Confirm, Notice};
use serde::Deserialize;

use super::document_page;
use crate::core_wishlist::long_confirm;
use crate::ctx::{go_to, Ctx, JobSpec, Out, Work};
use crate::dest;
use crate::layout::{self, Page};
use crate::runner::{no_flow, Reply};

#[derive(Debug, Default, Deserialize)]
pub struct WorkingQuery {
    /// The page that was waiting for the backend, to return to once the job is over.
    to: Option<String>,
}

/// `GET /working`: the wait for a job, as a page and as the fragment that page asks for every
/// second. A polling route.
pub async fn working(ctx: Ctx, Query(query): Query<WorkingQuery>) -> Response {
    // Only a path of this origin is ever followed.
    let to = query.to.as_deref().and_then(dest::local_path).map(str::to_owned);
    ctx.working(to.as_deref())
}

/// `GET /confirm`: the question waiting for an answer, as a page of its own. This is what a
/// browser without scripts gets; with htmx the same question is a dialog over the page.
pub async fn question(ctx: Ctx) -> Response {
    let pending = ctx.session.inner().question.clone();
    match pending {
        Some(pending) => {
            let body = html! { section.card.attn { (layout::question(&pending)) } };
            ctx.page(Page::plain(pending.question.title.clone(), body))
        }
        None => go_to(ctx.htmx, dest::HOME),
    }
}

#[derive(Debug, Deserialize)]
pub struct ConfirmForm {
    id: u64,
}

/// Carries out what was confirmed. The flow that asked is told first where it has something to
/// keep (`confirmed`), and gets the outcomes, since only it knows what some of them mean.
fn carry_out(work: &mut Work<'_>, confirm: &Confirm) -> Reply {
    let step = confirm.step(&work.snapshot);
    match confirm {
        // A key that could not be removed is the keys page's to say; here it is a notice.
        Confirm::RemoveSshKey(_) => {
            let mut setup = Setup::keys();
            setup.confirmed(confirm);
            let mut reply = work.run(step, &mut |outcome, _| setup.apply(outcome));
            reply.notices.extend(setup.error.take().map(Notice::error));
            reply
        }
        _ => work.run(step, &mut no_flow),
    }
}

/// `POST /confirm`: the owner confirmed the question with this id. What that does is the
/// core's (`Confirm::step`); a stale or repeated answer does nothing.
pub async fn confirm(ctx: Ctx, Form(form): Form<ConfirmForm>) -> Response {
    let pending = ctx.session.inner().answer(form.id);
    let Some(pending) = pending else {
        ctx.flash(Notice::info("That question was answered already. Nothing was done."));
        return go_to(ctx.htmx, dest::HOME);
    };
    let confirm = pending.question.confirm.clone();
    match long_confirm(&confirm) {
        Some(what) => ctx.job(JobSpec::new(what, pending.back), move |work| carry_out(work, &confirm)).await,
        None => ctx.with(pending.back, move |work| Out::Reply(carry_out(work, &confirm))).await,
    }
}

/// `GET /document`: the text an action answered with (a drive check's report), kept for this
/// one page, with the way back to where it was asked for.
pub async fn document(ctx: Ctx) -> Response {
    let kept = ctx.session.inner().stash.document.clone();
    match kept {
        Some((document, back)) => ctx.page(document_page(None, &document, &back)),
        None => ctx.page(Page::plain("Nothing to read", html! {
            section.card { h1 { "Nothing to read" } p.dim { "That text is not kept any more." } div.actions { a.btn href=(dest::HOME) { "Home" } } }
        }).status(StatusCode::NOT_FOUND)),
    }
}
