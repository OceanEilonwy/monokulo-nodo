//! SSH keys: who may log in over SSH (`flows::setup`, its keys stage).
//!
//! The flow is made anew for each request and filled by reading the keys, so the page keeps
//! nothing: the list is always the device's.

use axum::http::StatusCode;
use axum::response::Response;
use axum::Form;
use maud::html;
use nodo_ui_core::flows::setup::Setup;
use nodo_ui_core::flows::Step;
use nodo_ui_core::screen::Tab;
use serde::Deserialize;

use crate::ctx::{Ctx, Out, Work};
use crate::dest;
use crate::layout::{self, Page};
use crate::runner::Reply;

/// The keys page's flow with the keys read. A failure to read them is in `error`.
fn loaded(work: &mut Work<'_>) -> (Setup, Reply) {
    let mut setup = Setup::keys();
    let reply = work.run(Step::run(Setup::load_keys()), &mut |outcome, _| setup.apply(outcome));
    (setup, reply)
}

fn page(setup: &Setup, line: &str) -> Page {
    let problem = setup.error.as_deref();
    let body = html! {
        h1 { "SSH keys" }
        p.lead { "The keys that may log in to this Nodo over SSH. Whoever holds one can open the terminal interface and a shell." }
        section.card.flush {
            @if setup.keys.is_empty() { p.pad.dim { "No key may log in yet." } }
            div.rows {
                @for key in &setup.keys {
                    div.row {
                        span.name { (key.comment) }
                        span.what.mono { (key.algorithm) " " (key.fingerprint) }
                        span.end {
                            form method="post" action=(format!("{}/remove", dest::KEYS)) hx-post=(format!("{}/remove", dest::KEYS)) hx-target="#dialog" hx-swap="innerHTML" {
                                (layout::hidden("fingerprint", &key.fingerprint))
                                button.btn.danger type="submit" { "Remove" }
                            }
                        }
                        span.go {}
                    }
                }
            }
        }
        section.card {
            h2 { "Add a key" }
            form.stack method="post" action=(format!("{}/add", dest::KEYS)) {
                div.field {
                    label for="line" { "Public key" }
                    textarea id="line" name="line" required spellcheck="false" autocomplete="off" autocapitalize="none"
                        placeholder="ssh-ed25519 AAAA… you@laptop" aria-invalid=[problem.map(|_| "true")] aria-describedby=[problem.map(|_| "line-problem")] { (line) }
                    @if let Some(problem) = problem { p.problem #line-problem { "✕ " (problem) } }
                    p.hint { "One line, as in an authorized_keys file: the contents of your .pub file. Never the private key." }
                }
                div.actions.end { button.btn.primary type="submit" { "Add key" } }
            }
        }
    };
    let status = if problem.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page::new(Tab::System, "SSH keys", body).trail(&[("System", dest::SYSTEM), ("SSH keys", dest::KEYS)]).status(status)
}

/// `GET /system/keys`.
pub async fn show(ctx: Ctx) -> Response {
    ctx.with(dest::SYSTEM, |work| {
        let (setup, reply) = loaded(work);
        if reply.login_required { Out::Reply(reply) } else { Out::Page(page(&setup, "")) }
    })
    .await
}

#[derive(Debug, Default, Deserialize)]
pub struct AddForm {
    #[serde(default)]
    line: String,
}

/// `POST /system/keys/add`. A refused key is shown beside the box with what was typed.
pub async fn add(ctx: Ctx, Form(form): Form<AddForm>) -> Response {
    ctx.with(dest::KEYS, move |work| {
        let (mut setup, _) = loaded(work);
        let effects = setup.add_key(&form.line);
        let reply = work.run(Step::run(effects), &mut |outcome, _| setup.apply(outcome));
        if setup.error.is_some() { Out::Page(page(&setup, &form.line)) } else { Out::Reply(reply) }
    })
    .await
}

#[derive(Debug, Default, Deserialize)]
pub struct RemoveForm {
    #[serde(default)]
    fingerprint: String,
}

/// `POST /system/keys/remove`: asks first, naming the key. Confirming removes it
/// (`pages::flow::confirm`).
pub async fn remove(ctx: Ctx, Form(form): Form<RemoveForm>) -> Response {
    ctx.with(dest::KEYS, move |work| {
        let (setup, _) = loaded(work);
        match setup.ask_remove_key(&form.fingerprint) {
            Some(question) => Out::Reply(Reply::ask(question)),
            // Gone already: the list shows it.
            None => Out::Go(dest::KEYS.to_owned()),
        }
    })
    .await
}
