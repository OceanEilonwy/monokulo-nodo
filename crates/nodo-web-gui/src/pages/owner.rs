//! An app's setup page for its owner (`flows::owner_page`): the device passphrase first, then
//! the checklist the app gives, with its values, QR codes and DNS records.
//!
//! The page holds secrets of the app, so the helper asks for the device passphrase each time it
//! is read. The flow keeps that passphrase (`held`) while the page is open, to read it again;
//! here that is in the session, on the Nodo, and it is never part of any answer. Closing the
//! page drops the flow, and so does the end of the session.
//!
//! A QR code is drawn on the server as SVG. What can be copied has a button that the page's
//! script wires to the clipboard; without scripts the value is there to select.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Form;
use maud::{html, Markup, PreEscaped};
use nodo_api::{OwnerCheck, OwnerCheckState, OwnerItem, OwnerStepState};
use nodo_ui_core::flows::owner_page::{self, OwnerPage};
use nodo_ui_core::flows::Step;
use nodo_ui_core::screen::Tab;

use super::PassForm;
use crate::glue::typed_secret;
use crate::ctx::{go_to, Ctx, Out, Work};
use crate::dest;
use crate::layout::{self, Page};

fn url(package: &str) -> String {
    format!("{}/owner", dest::app(package))
}

fn kept(work: &Work<'_>, package: &str) -> Option<OwnerPage> {
    work.session.inner().stash.owner.get(package).cloned()
}

fn keep(work: &Work<'_>, flow: &OwnerPage) {
    work.session.inner().stash.owner.insert(flow.package.clone(), flow.clone());
}

/// A QR code as SVG: one path of unit squares, black on white with the quiet zone it needs.
fn qr(data: &str) -> Option<Markup> {
    let code = qrcodegen::QrCode::encode_text(data, qrcodegen::QrCodeEcc::Low).ok()?;
    let (size, border) = (code.size(), 4);
    let mut path = String::new();
    for y in 0..size {
        for x in 0..size {
            if code.get_module(x, y) {
                path.push_str(&format!("M{},{}h1v1h-1z", x + border, y + border));
            }
        }
    }
    let side = size + 2 * border;
    // Built from numbers only; nothing of `data` is written into the markup.
    Some(PreEscaped(format!(
        r##"<svg class="qr" viewBox="0 0 {side} {side}" role="img" aria-label="QR code" xmlns="http://www.w3.org/2000/svg"><rect width="{side}" height="{side}" fill="#fff"/><path d="{path}" fill="#000"/></svg>"##
    )))
}

/// A value with its button to copy it. The value stays selectable for a browser without scripts.
fn copyable(label: &str, value: &str) -> Markup {
    html! {
        div.field {
            span.label { (label) }
            div.pieces {
                span.mono.value { (value) }
                button.btn.link.copy type="button" data-copy=(value) { "Copy" }
            }
        }
    }
}

fn check(check: &OwnerCheck) -> Markup {
    html! {
        p {
            @match check.state {
                OwnerCheckState::Ok => span.ok aria-hidden="true" { "✓ " },
                OwnerCheckState::Failed => span.err aria-hidden="true" { "✕ " },
                OwnerCheckState::Waiting => span.warn aria-hidden="true" { "◆ " },
                OwnerCheckState::Unknown => span.dim aria-hidden="true" { "? " },
            }
            (check.label)
            @if let Some(detail) = &check.detail { span.dim { "   " (detail) } }
        }
    }
}

fn item(item: &OwnerItem) -> Markup {
    html! {
        @match item {
            OwnerItem::Text(text) => p { (text) },
            OwnerItem::Value { label, value } => (copyable(label, value)),
            OwnerItem::Check(each) => (check(each)),
            OwnerItem::Qr { label, data } => {
                @match qr(data) {
                    Some(svg) => figure.qr-box { (svg) },
                    None => p.dim { "This is too long for a QR code." },
                }
                (copyable(label, data))
            },
            OwnerItem::Site(site) => {
                @match &site.host {
                    Some(host) => (copyable(if site.suggested { "Suggested address" } else { "Address" }, host)),
                    None => p.dim { (site.package) " has no web address yet." },
                }
                @if site.public { p.ok { "✓ On the internet" } }
                @for record in &site.records { (copyable(&format!("{} record {} in {}", record.kind, record.name, record.zone), &record.value)) }
                @if let Some(note) = nodo_ui_core::flows::owner_page::dns_note(site) { p.dim { (note) } }
                @for each in &site.checks { (check(each)) }
            },
            OwnerItem::Service { package, check: each } => { p.dim { (package) } (check(each)) },
            OwnerItem::Domain { domain, provider, check: each } => {
                p { span.mono { (domain) } @if let Some(provider) = provider { span.dim { "   through " (provider) } } }
                (check(each))
            },
            OwnerItem::Backup { check: each } => (check(each)),
        }
    }
}

fn page(flow: &OwnerPage) -> Page {
    let here = url(&flow.package);
    let body = match &flow.view {
        // The gate: the page holds the app's secrets, so the device passphrase opens it.
        None => html! {
            h1 { (flow.title) }
            section.card {
                p { (nodo_ui_core::flows::owner_page::WHY_PASSPHRASE) }
                p.dim { "The passphrase is kept on the Nodo while the page is open, to read it again, and dropped when you close it. Reading the page may check its sites first, up to half a minute." }
                form.stack method="post" action=(here) {
                    (layout::passphrase("owner-passphrase", "Device passphrase", flow.error.as_deref(), true))
                    div.actions.end {
                        a.btn href=(dest::app(&flow.package)) { "Cancel" }
                        button.btn.primary type="submit" { "Open" }
                    }
                }
            }
        },
        Some(view) => html! {
            div.head {
                h1 { (view.title) }
                div.actions {
                    (layout::post(&format!("{here}/refresh"), "Read it again", "btn"))
                    (layout::post(&format!("{here}/close"), "Close", "btn primary"))
                }
            }
            @if let Some(error) = &flow.error { p.notice.error { "✕ " (error) } }
            @if let Some(intro) = &view.intro { p.lead { (intro) } }
            @for (number, step) in view.steps.iter().enumerate() {
                section class=(if step.state == OwnerStepState::Problem { "card bad" } else { "card" }) {
                    h2 {
                        @match step.state {
                            OwnerStepState::Done => span.ok aria-hidden="true" { "✓ " },
                            OwnerStepState::Todo => span.warn aria-hidden="true" { "○ " },
                            OwnerStepState::Problem => span.err aria-hidden="true" { "✕ " },
                            OwnerStepState::Unknown => span.dim aria-hidden="true" { "? " },
                        }
                        (number + 1) ". " (step.title)
                    }
                    @for each in &step.items { (item(each)) }
                }
            }
        },
    };
    let status = if flow.view.is_none() && flow.error.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page::new(Tab::Apps, format!("{} · {}", flow.package, flow.title), body)
        .trail(&[("Apps", dest::APPS), (&flow.package, &dest::app(&flow.package)), (&flow.title, &here)])
        .status(status)
}

/// `GET /apps/{name}/owner`: the page if it is open, else its passphrase gate. An app without a
/// setup page says so on its own page.
pub async fn show(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::app(&name), move |work| match kept(work, &name) {
        Some(flow) => Out::Page(page(&flow)),
        // Opened through the flow, which knows whether the app has such a page and its name.
        None => Out::Reply(work.run(owner_page::open(&work.snapshot, &name), &mut crate::runner::no_flow)),
    })
    .await
}

/// `POST /apps/{name}/owner`: the device passphrase, which opens the page.
pub async fn unlock(ctx: Ctx, Path(name): Path<String>, Form(form): Form<PassForm>) -> Response {
    ctx.with(url(&name), move |work| {
        let Some(mut flow) = kept(work, &name) else {
            return Out::Go(url(&name));
        };
        let effects = flow.unlock(&mut typed_secret(form.passphrase.expose()));
        let reply = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
        if reply.login_required {
            return Out::Reply(reply);
        }
        let shown = page(&flow);
        // A refusal is said once.
        flow.error = None;
        keep(work, &flow);
        if flow.view.is_some() { Out::Go(url(&name)) } else { Out::Page(shown) }
    })
    .await
}

/// `POST /apps/{name}/owner/{refresh|close}`: reads the open page again with the passphrase it
/// was opened with, or closes it, which drops that passphrase.
pub async fn act(ctx: Ctx, Path((name, what)): Path<(String, String)>) -> Response {
    if what == "close" {
        ctx.session.inner().stash.owner.remove(&name);
        return go_to(ctx.htmx, &dest::app(&name));
    }
    ctx.with(url(&name), move |work| {
        let Some(mut flow) = kept(work, &name) else {
            return Out::Go(url(&name));
        };
        let effects = flow.refresh();
        let reply = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
        keep(work, &flow);
        Out::Reply(reply)
    })
    .await
}
