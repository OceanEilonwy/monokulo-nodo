//! Installing a suite of apps together (`flows::bundle::Bundle`): the review, the settings it
//! asks for once, the device passphrase, the install as a job, and what it came to.
//!
//! The flow is kept in the session from the gate's answer to the result. The review the helper
//! is asked to install is the one it gave, held there; a browser sends only the choices and the
//! answers. An answer to a secret ask is never sent back to the browser, on any page.

use axum::http::StatusCode;
use axum::response::Response;
use axum::Form;
use maud::html;
use nodo_ui_core::flows::bundle::{unreviewed, AFTER_FAILURE, ASKED_ONCE, GUARANTEES, IN_TURN, SECRET_ANSWER};
use nodo_ui_core::flows::bundle::{Bundle, BundleStep};
use nodo_ui_core::flows::Step;
use nodo_ui_core::screen::Tab;
use nodo_ui_core::Effect;

use crate::glue::typed_secret;
use crate::ctx::{go_to, Ctx, JobSpec, Out};
use crate::dest;
use crate::layout::{self, Page};
use crate::runner::follow_plain;

type Posted = Vec<(String, String)>;

fn posted<'a>(form: &'a Posted, name: &str) -> &'a str {
    form.iter().find(|(key, _)| key == name).map_or("", |(_, value)| value.as_str())
}

/// The page of the step the flow is at. `typed` holds what was typed into the asks that are
/// not secret, to show again when one is missing; `unanswered` is the ask the flow named.
fn page(flow: &Bundle, typed: &Posted, unanswered: Option<usize>) -> Page {
    let here = dest::BUNDLE;
    let review = &flow.review;
    let button = |action: &str, label: &str, class: &str| html! { button class=(class) type="submit" name="do" value=(action) { (label) } };
    let problem = html! { @if let Some(error) = &flow.error { p.notice.error { "✕ " (error) } } };
    let body = match &flow.step {
        BundleStep::Review => html! {
            h1 { "Install " (review.package) " and its apps?" }
            (problem)
            p.warn { "▲ " (unreviewed(&review.feed)) }
            @for member in &review.members {
                section.card {
                    h2 { (member.review.package) span.dim { "   " (member.review.pkgver) } }
                    @if let Some(version) = &member.installed { p.dim { (version) " is installed already, so it is left as it is." } }
                    ul.bullets { @for permission in &member.review.permissions { li { span.dim aria-hidden="true" { "·" } span { (permission) } } } }
                    @if member.review.encrypted_storage { p.dim { "◆ Keeps its data in encrypted storage, which locks at a restart." } }
                }
            }
            @if !review.grants.is_empty() {
                section.card {
                    h3 { "They may read from each other" }
                    ul.bullets { @for grant in &review.grants { li { span.dim aria-hidden="true" { "·" } span { (grant.text) } } } }
                }
            }
            form.stack method="post" action=(here) {
                @if !review.data_drive.is_empty() {
                    section.card {
                        h3 { "Data drive" }
                        @if !review.data_drive_present { p.dim { "No data drive is connected, so their data stays on the Nodo's own disk." } }
                        @for (app, on) in review.data_drive.iter().zip(&flow.drive) {
                            div.setting {
                                p { strong { (app) } " keeps its data on " @if *on { "the data drive" } @else { "the Nodo's own disk" } "." }
                                div.actions { button.btn.link type="submit" name="drive" value=(app) { @if *on { "Use the Nodo's own disk" } @else { "Use the data drive" } } }
                            }
                        }
                    }
                }
                @for guarantee in GUARANTEES { p.dim { (guarantee) } }
                div.actions.end { (button("back", "Cancel", "btn")) (button("proceed", "Continue", "btn primary")) }
            }
        },
        BundleStep::Answers => html! {
            h1 { "A few settings first" }
            p.lead { (ASKED_ONCE) }
            (problem)
            section.card {
                form.stack method="post" action=(here) {
                    @for index in flow.open_asks() {
                        @let ask = &review.asks[index];
                        @let name = format!("a.{index}");
                        div.field {
                            label for=(name) { (ask.label) span.dim { "   " (ask.package) } }
                            // A secret answer is typed into an empty box every time.
                            @if ask.secret {
                                input type="password" id=(name) name=(name) autocomplete="new-password" aria-invalid=[(unanswered == Some(index)).then_some("true")];
                            } @else {
                                input type="text" id=(name) name=(name) value=(posted(typed, &name)) autocomplete="off" autocapitalize="none" spellcheck="false" aria-invalid=[(unanswered == Some(index)).then_some("true")];
                            }
                            @if !ask.help.is_empty() { p.hint { (ask.help) } }
                            @if ask.secret { p.hint { (SECRET_ANSWER) } }
                        }
                    }
                    div.actions.end { (button("back", "Back", "btn")) (button("answers", "Continue", "btn primary")) }
                }
            }
        },
        BundleStep::Passphrase => html! {
            h1 { "Device passphrase" }
            (problem)
            section.card {
                p { "New locked storage and secret settings are sealed with the device passphrase. It is used for this and not kept." }
                form.stack method="post" action=(here) {
                    (layout::passphrase("bundle-passphrase", "Device passphrase", None, true))
                    div.actions.end { (button("back", "Back", "btn")) (button("passphrase", "Install", "btn primary")) }
                }
            }
        },
        BundleStep::Installing => html! {
            h1 { "Installing " (review.package) }
            section.card { p { "The apps are being installed." } p.dim { (IN_TURN) } div.actions.end { a.btn href=(dest::WORKING) { "See how far it is" } } }
        },
        BundleStep::Done(report) => html! {
            h1 { @if report.problem.is_none() { span.ok aria-hidden="true" { "✓ " } (report.package) " and its apps are installed" } @else { span.err aria-hidden="true" { "✕ " } (report.package) " was not installed completely" } }
            section.card {
                @if let Some(problem) = &report.problem { p.err { (problem) } p.dim { (AFTER_FAILURE) } }
                ul.bullets {
                    @for member in &report.members {
                        li {
                            @match member.state {
                                nodo_api::StepState::Done => span.ok aria-hidden="true" { "✓" },
                                nodo_api::StepState::Failed => span.err aria-hidden="true" { "✕" },
                                _ => span.dim aria-hidden="true" { "○" },
                            }
                            span {
                                (member.package) span.note { "   " (member.note) }
                                @if let Some(inner) = &member.report { @for detail in &inner.details { span.note { "   " (detail) } } }
                            }
                        }
                    }
                }
                form.actions.end method="post" action=(here) { (button("finish", "Done", "btn primary")) }
            }
        },
    };
    let status = if flow.error.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page::new(Tab::Apps, format!("Install {}", review.package), body)
        .trail(&[("Apps", dest::APPS), ("Get apps", dest::GET_APPS), (&review.package, dest::BUNDLE)])
        .status(status)
}

/// `GET /apps/get/bundle`: the step the install is at.
pub async fn show(ctx: Ctx) -> Response {
    let kept = ctx.session.inner().stash.bundle.clone();
    match kept {
        Some(mut flow) => {
            let shown = page(&flow, &Vec::new(), None);
            // A refusal is said once.
            flow.error = None;
            ctx.session.inner().stash.bundle = Some(flow);
            ctx.page(Page { status: StatusCode::OK, ..shown })
        }
        None => go_to(ctx.htmx, dest::GET_APPS),
    }
}

/// The install itself, as a job. The flow goes with it and comes back with the result.
fn install(mut flow: Bundle, effects: Vec<Effect>) -> Out {
    let spec = JobSpec::new(format!("Installing {} and its apps", flow.review.package), dest::BUNDLE)
        .note(&[IN_TURN, "This can take several minutes. You can leave this page; it carries on."]);
    Out::Job(
        spec,
        Box::new(move |work| {
            // The page shows how it went, so the result is the flow's alone to tell.
            let reply = work.run(Step::run(effects), &mut |outcome, _| {
                let step = flow.apply(outcome);
                if matches!(outcome, nodo_ui_core::Outcome::BundleInstalled(_)) { Step::All(Vec::new()) } else { step }
            });
            work.session.inner().stash.bundle = Some(flow);
            reply
        }),
    )
}

/// `POST /apps/get/bundle`: every button of the bundle's pages.
pub async fn act(ctx: Ctx, Form(form): Form<Posted>) -> Response {
    let Some(mut flow) = ctx.session.inner().stash.bundle.clone() else {
        return go_to(ctx.htmx, dest::GET_APPS);
    };
    ctx.with(dest::GET_APPS, move |work| {
        let keep = |flow: &Bundle| work.session.inner().stash.bundle = Some(flow.clone());
        let drive = posted(&form, "drive");
        if !drive.is_empty() {
            flow.toggle_drive(drive);
            keep(&flow);
            return Out::Go(dest::BUNDLE.to_owned());
        }
        let effects = match posted(&form, "do") {
            "proceed" => flow.proceed(),
            "answers" => {
                // One box per ask of the review, in its order, filled from the form.
                let mut boxes = flow.answer_boxes();
                for (index, input) in boxes.iter_mut().enumerate() {
                    posted(&form, &format!("a.{index}")).chars().for_each(|c| input.push(c));
                }
                match flow.answer(&boxes) {
                    Ok(effects) => effects,
                    Err(unanswered) => {
                        let shown = page(&flow, &form, Some(unanswered));
                        flow.error = None;
                        keep(&flow);
                        return Out::Page(shown);
                    }
                }
            }
            "passphrase" => flow.submit_passphrase(&mut typed_secret(posted(&form, "passphrase"))),
            "back" => {
                let reply = follow_plain(flow.back());
                if reply.go.is_some() {
                    // Left from the review: nothing was installed, and the flow is done with.
                    work.session.inner().stash.bundle = None;
                    return Out::Reply(reply);
                }
                Vec::new()
            }
            "finish" => {
                // On to the head's setup page when it has one; either way the flow is done with.
                let step = flow.finish(&work.snapshot);
                work.session.inner().stash.bundle = None;
                return Out::Reply(follow_plain(step));
            }
            _ => Vec::new(),
        };
        if effects.is_empty() {
            keep(&flow);
            return Out::Go(dest::BUNDLE.to_owned());
        }
        keep(&flow);
        install(flow, effects)
    })
    .await
}
