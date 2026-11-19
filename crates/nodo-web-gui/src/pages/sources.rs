//! Sources (`flows::sources`): the list, on and off, reading them again, adding one by probing
//! its address and trusting its key, naming one, removing one, and deciding about a changed key.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Form;
use maud::html;
use nodo_ui_core::flows::{sources, Dest, Step};
use nodo_ui_core::model::{counted, Feed, FeedProbe, FeedStatus, KeyChange, KeyState, Snapshot};
use nodo_ui_core::screen::Tab;
use serde::Deserialize;

use super::document_page;
use nodo_ui_core::words::is_plain_http;
use crate::ctx::{go_to, Ctx, Out};
use crate::dest;
use crate::layout::{self, Page, Sub};
use crate::runner::{no_flow, Reply};

const TRAIL: [(&str, &str); 3] = [("Apps", dest::APPS), ("Get apps", dest::GET_APPS), ("Sources", dest::SOURCES)];

fn trail(last: &[(&str, &str)]) -> Vec<(String, String)> {
    TRAIL.iter().chain(last).map(|(name, url)| ((*name).to_owned(), (*url).to_owned())).collect()
}

fn list_page(snapshot: &Snapshot) -> Page {
    let head = layout::heading("Sources", Some("Where the Nodo finds apps. Each source signs its apps; the Nodo only installs what its key signed."));
    let body = html! {
        div.tools {
            div.grow {}
            div.btns {
                (layout::post(&format!("{}/refresh", dest::SOURCES), "Read them again", "btn"))
                a.btn.primary href=(dest::ADD_SOURCE) { "Add a source" }
            }
        }
        @if snapshot.feeds.is_empty() {
            section.card { p.dim { "No sources yet. Add the address a publisher gave you." } }
        }
        @for feed in &snapshot.feeds {
            @let here = dest::source(&feed.name);
            @let on = feed.status != FeedStatus::Disabled;
            section.card id=(format!("source-{}", dest::seg(&feed.name))) {
                div.head {
                    h2 {
                        a href=(here) { (feed.shown()) }
                        @if feed.label.is_some() { " " span.dim { (feed.name) } }
                        @if feed.key_state == KeyState::BuiltIn { " " span.tag { "built in" } }
                    }
                    div.actions {
                        form method="post" action=(format!("{here}/toggle")) {
                            button.btn type="submit" aria-pressed=(if on { "true" } else { "false" })
                                title=(if on { "Turn this source off" } else { "Turn this source on" }) {
                                @if on { span.ok aria-hidden="true" { "●" } " On" } @else { span.dim aria-hidden="true" { "○" } " Off" }
                            }
                        }
                        a.btn href=(rename_url(&feed.name)) { "Rename" }
                        (layout::ask(&format!("{here}/remove"), "Remove", "btn danger"))
                    }
                }
                p.dim { (feed.address.trim_start_matches("https://")) "   ·   " (counted(u64::from(feed.packages), "app")) }
                @match &feed.status {
                    FeedStatus::Unreachable(problem) => div.head {
                        p.err { "✕ Couldn't reach it: " (problem) }
                        (layout::post(&format!("{}/refresh", dest::SOURCES), "Try again", "btn"))
                    },
                    FeedStatus::Blocked => div.head {
                        p.warn { "▲ Its key changed. Nothing new installs from it until you decide." }
                        a.btn.primary href=(here) { "Decide ›" }
                    },
                    FeedStatus::Healthy | FeedStatus::Disabled => {},
                }
                @if is_plain_http(&feed.address) { p.dim { "Plain HTTP: signed, but updates can be held back" } }
                @if !feed.fingerprint.is_empty() {
                    p { span.label { "Key " } (layout::fingerprint(&feed.fingerprint, false)) }
                }
            }
        }
    };
    Page { trail: trail(&[]), ..Page::new(Tab::Apps, "Sources", body) }.head(head).sub(Sub::Sources)
}

/// `GET /sources`.
pub async fn list(ctx: Ctx) -> Response {
    ctx.with(dest::GET_APPS, |work| Out::Page(list_page(&work.snapshot))).await
}

/// `POST /sources/refresh`: reads every source again.
pub async fn refresh(ctx: Ctx) -> Response {
    ctx.with(dest::SOURCES, |work| Out::Reply(work.run(Step::run(sources::refresh()), &mut no_flow))).await
}

/// `GET /sources/of/{name}`: opening a source. One whose key changed leads to that decision,
/// any other to the apps it offers; the flow says which.
pub async fn open(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::SOURCES, move |work| {
        let Some(feed) = work.snapshot.feeds.iter().find(|feed| feed.name == name).cloned() else {
            return Out::Go(dest::SOURCES.to_owned());
        };
        Out::Reply(work.run(sources::open(&feed), &mut no_flow))
    })
    .await
}

/// `POST /sources/of/{name}/toggle`: on or off.
pub async fn toggle(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::SOURCES, move |work| Out::Reply(work.run(Step::run(sources::toggle(&name)), &mut no_flow))).await
}

fn rename_url(name: &str) -> String {
    format!("{}/rename", dest::source(name))
}

#[derive(Debug, Default, Deserialize)]
pub struct NameForm {
    #[serde(default)]
    label: String,
}

/// The name box of a source: what the owner typed, the name the device made as its placeholder,
/// and why the last name was refused.
fn name_field(typed: &str, made: &str, problem: Option<&str>) -> maud::Markup {
    html! {
        div.field {
            label for="label" { "Name" }
            input type="text" id="label" name="label" value=(typed) placeholder=(made) maxlength=(sources::MAX_LABEL) autocomplete="off"
                aria-invalid=[problem.map(|_| "true")] aria-describedby=[problem.map(|_| "label-problem")];
            @if let Some(problem) = problem { p.problem #label-problem { "✕ " (problem) } }
            p.hint { "What the Nodo calls this source. Leave it empty to call it " (made) "." }
        }
    }
}

fn rename_page(feed: &Feed, typed: &str, problem: Option<&str>) -> Page {
    let here = rename_url(&feed.name);
    let body = html! {
        h1 { "Rename " (feed.shown()) }
        section.card {
            form.stack method="post" action=(here) {
                (name_field(typed, &feed.name, problem))
                p.dim { (feed.address) }
                div.actions.end {
                    a.btn href=(dest::SOURCES) { "Cancel" }
                    button.btn.primary type="submit" { "Save" }
                }
            }
        }
    };
    let status = if problem.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page { trail: trail(&[(feed.shown(), &here)]), ..Page::new(Tab::Apps, format!("Rename {}", feed.shown()), body) }.status(status)
}

/// `GET /sources/of/{name}/rename`: the source's name, to change.
pub async fn rename(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::SOURCES, move |work| match work.snapshot.feeds.iter().find(|feed| feed.name == name) {
        Some(feed) => Out::Page(rename_page(feed, feed.label.as_deref().unwrap_or_default(), None)),
        None => Out::Go(dest::SOURCES.to_owned()),
    })
    .await
}

/// `POST /sources/of/{name}/rename`: gives the source the name typed. What is wrong with it, by
/// the flow or the helper, is said beside the box.
pub async fn save_name(ctx: Ctx, Path(name): Path<String>, Form(form): Form<NameForm>) -> Response {
    ctx.with(dest::SOURCES, move |work| {
        let Some(feed) = work.snapshot.feeds.iter().find(|feed| feed.name == name).cloned() else {
            return Out::Go(dest::SOURCES.to_owned());
        };
        let effects = match sources::rename(&name, &form.label) {
            Ok(effects) => effects,
            Err(problem) => return Out::Page(rename_page(&feed, &form.label, Some(&problem))),
        };
        let reply = work.run(Step::run(effects), &mut no_flow);
        match reply.error() {
            Some(problem) => Out::Page(rename_page(&feed, &form.label, Some(problem))),
            None => Out::Reply(Reply { go: Some(Dest::Sources { select: Some(name) }), ..reply }),
        }
    })
    .await
}

/// `POST /sources/of/{name}/remove`: asks first.
pub async fn remove(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.respond(Reply::ask(sources::ask_remove(&name, false)), dest::SOURCES)
}

#[derive(Debug, Default, Deserialize)]
pub struct AddForm {
    #[serde(default)]
    address: String,
}

fn add_page(address: &str, problem: Option<&str>) -> Page {
    let body = html! {
        h1 { "Add a source" }
        section.card {
            form.stack method="post" action=(dest::ADD_SOURCE) {
                div.field {
                    label for="address" { "Address" }
                    input type="url" id="address" name="address" value=(address) required autofocus autocomplete="off" inputmode="url"
                        placeholder="https://" aria-invalid=[problem.map(|_| "true")] aria-describedby=[problem.map(|_| "address-problem")];
                    @if let Some(problem) = problem { p.problem #address-problem { "✕ " (problem) } }
                    p.hint { "Paste the address the publisher gave you." }
                }
                p.dim { "Nothing is installed yet: the Nodo first shows you what the source offers and its key." }
                div.actions.end {
                    a.btn href=(dest::SOURCES) { "Cancel" }
                    button.btn.primary type="submit" { "Check" }
                }
            }
        }
    };
    let status = if problem.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page { trail: trail(&[("Add", dest::ADD_SOURCE)]), ..Page::new(Tab::Apps, "Add a source", body) }.status(status)
}

/// `GET /sources/add`.
pub async fn add(ctx: Ctx) -> Response {
    ctx.page(add_page("", None))
}

/// `POST /sources/add`: the first step, asking what is at the address. What is wrong with the
/// address, or why it could not be read, is shown beside the box.
pub async fn probe(ctx: Ctx, Form(form): Form<AddForm>) -> Response {
    let effects = match sources::probe(&form.address) {
        Ok(effects) => effects,
        Err(problem) => return ctx.page(add_page(&form.address, Some(&problem))),
    };
    ctx.with(dest::ADD_SOURCE, move |work| {
        let reply = work.run(Step::run(effects), &mut no_flow);
        let problem = if reply.go.is_none() { reply.error().map(str::to_owned) } else { None };
        match problem {
            Some(problem) => Out::Page(add_page(&form.address, Some(&problem))),
            None => Out::Reply(reply),
        }
    })
    .await
}

fn confirm_page(probe: &FeedProbe, problem: Option<&str>) -> Page {
    let body = html! {
        h1 { "Add " (probe.shown()) "?" }
        section.card.attn {
            p {
                @if probe.signed { span.ok aria-hidden="true" { "✓ " } } @else { span.err aria-hidden="true" { "✕ " } }
                strong { (probe.name) } span.dim { "   " (probe.address) }
            }
            p { "It offers " (counted(u64::from(probe.packages), "app")) "." }
            @if !probe.signed { p.err { "Its list of apps isn't signed, so the Nodo won't use it." } }
            div.field {
                span.label { "Its key" }
                (layout::fingerprint(&probe.fingerprint, true))
            }
            p {
                "Check this matches what the publisher gave you, by a way you trust (in person, their site, a message you know is from them). If it doesn't match, don't add it."
            }
            @if is_plain_http(&probe.address) {
                p { span.warn aria-hidden="true" { "▲ " } strong { "Plain HTTP" } }
                p.dim {
                    "Signatures still protect every app: nothing changed on the way can install. But anyone on the network can hold back updates or offer an older signed version, and this key arrives unprotected."
                }
            }
            form.stack method="post" action=(dest::CONFIRM_SOURCE) {
                (name_field(&probe.label, &probe.name, problem))
                div.actions.end {
                    a.btn.quiet href=(format!("{}/packages", dest::CONFIRM_SOURCE)) { "Its apps" }
                    a.btn href=(dest::SOURCES) { "Cancel" }
                    button.btn.primary type="submit" { "Add source" }
                }
            }
        }
    };
    let status = if problem.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page { trail: trail(&[("Add", dest::ADD_SOURCE)]), ..Page::new(Tab::Apps, "Add a source", body) }.status(status)
}

fn kept_probe(ctx: &Ctx) -> Option<FeedProbe> {
    ctx.session.inner().stash.probe.clone()
}

/// `GET /sources/add/confirm`: what was found at the address, to trust or not.
pub async fn confirm(ctx: Ctx) -> Response {
    match kept_probe(&ctx) {
        Some(probe) => ctx.page(confirm_page(&probe, None)),
        None => go_to(ctx.htmx, dest::ADD_SOURCE),
    }
}

/// `POST /sources/add/confirm`: adds the source, by the name typed if any, and pins the key
/// that was shown.
pub async fn trust(ctx: Ctx, Form(form): Form<NameForm>) -> Response {
    let Some(mut probe) = kept_probe(&ctx) else {
        return go_to(ctx.htmx, dest::ADD_SOURCE);
    };
    probe.label = form.label;
    // The name is checked as a rename checks it, before anything is added.
    if let Err(problem) = sources::rename(&probe.name, &probe.label) {
        return ctx.page(confirm_page(&probe, Some(&problem)));
    }
    // Kept with its name, so a confirmation shown again after a refusal still has what was typed.
    ctx.session.inner().stash.probe = Some(probe.clone());
    // Added: back to the sources. Refused: the notice says why, on the same page.
    ctx.with(dest::SOURCES, move |work| {
        let reply = work.run(Step::run(sources::trust(&probe)), &mut no_flow);
        if reply.go.is_some() {
            work.session.inner().stash.probe = None;
            Out::Reply(reply)
        } else {
            Out::Reply(Reply { go: Some(Dest::ConfirmSource(probe)), ..reply })
        }
    })
    .await
}

/// `GET /sources/add/confirm/packages`: the apps the probed source offers, before deciding.
pub async fn probed_packages(ctx: Ctx) -> Response {
    let Some(probe) = kept_probe(&ctx) else {
        return go_to(ctx.htmx, dest::ADD_SOURCE);
    };
    ctx.with(dest::CONFIRM_SOURCE, move |work| {
        let reply = work.run(Step::run(sources::packages(&probe)), &mut no_flow);
        match reply.go {
            Some(Dest::Document(document)) => {
                let mut page = document_page(Some(Tab::Apps), &document, dest::CONFIRM_SOURCE);
                page.trail = trail(&[("Add", dest::ADD_SOURCE)]);
                Out::Page(page)
            }
            _ => Out::Reply(reply),
        }
    })
    .await
}

fn key_url(name: &str) -> String {
    format!("{}/key", dest::source(name))
}

fn key_page(change: &KeyChange, typed: &str, problem: Option<&str>) -> Page {
    let here = key_url(&change.feed);
    let affected: Vec<String> = change.affected.iter().map(|(name, version)| format!("{name} {version}")).collect();
    let body = html! {
        h1 { "Check before you trust" }
        section.card.bad {
            h2 { span.err aria-hidden="true" { "▲ " } (change.feed) " is now signed with a different key" }
            p {
                "This happens when a publisher replaces a lost key, and also when someone else takes over their address. Until you decide, the Nodo installs nothing new from "
                (change.feed) "."
            }
            div.field { span.label { "Key you trust" } (layout::fingerprint(&change.trusted, false)) }
            div.field { span.label { "New key" } (layout::fingerprint(&change.now_signing, true)) }
            @if !affected.is_empty() { p.dim { "Apps from it on this Nodo: " (affected.join(", ")) } }
            p { "Ask the publisher, by a way you trust, whether the new key is theirs." }
        }
        section.card {
            h2 { "Trust the new key" }
            form.stack method="post" action=(here) {
                div.field {
                    label for="typed" { "Type " (change.feed) " to confirm" }
                    input type="text" id="typed" name="typed" value=(typed) autocomplete="off" autocapitalize="none" spellcheck="false"
                        aria-invalid=[problem.map(|_| "true")] aria-describedby=[problem.map(|_| "typed-problem")];
                    @if let Some(problem) = problem { p.problem #typed-problem { "✕ " (problem) } }
                    p.hint { "To trust the new key you type the source's name, so it can't happen by accident." }
                }
                div.actions.end { button.btn type="submit" { "Trust the new key" } }
            }
        }
        div.actions.end {
            (layout::ask(&format!("{here}/remove"), "Remove source", "btn danger"))
            a.btn.primary href=(dest::SOURCES) { "Keep the old key" }
        }
    };
    let status = if problem.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page { trail: trail(&[(&change.feed, &here)]), ..Page::new(Tab::Apps, format!("{}: key changed", change.feed), body) }.status(status)
}

/// `GET /sources/of/{name}/key`: the decision about a source whose signing key changed. The
/// page is drawn from a fresh read through the flow that opens a source, so it is there only
/// while the source is blocked; a source that is not leads to its apps as the flow says.
/// Trusting quotes back the keys this read, kept in the session.
pub async fn key(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::SOURCES, move |work| {
        let Some(feed) = work.snapshot.feeds.iter().find(|feed| feed.name == name).cloned() else {
            return Out::Go(dest::SOURCES.to_owned());
        };
        let reply = work.run(sources::open(&feed), &mut no_flow);
        match reply.go {
            Some(Dest::KeyChange(change)) => {
                work.session.inner().stash.key_change = Some(change.clone());
                Out::Page(key_page(&change, "", None))
            }
            _ => Out::Reply(reply),
        }
    })
    .await
}

#[derive(Debug, Default, Deserialize)]
pub struct TypedForm {
    #[serde(default)]
    typed: String,
}

/// `POST /sources/of/{name}/key`: trusts the new key, if the source's name was typed exactly.
/// The keys are the ones the page showed, kept since; a page opened without them is shown again.
pub async fn trust_key(ctx: Ctx, Path(name): Path<String>, Form(form): Form<TypedForm>) -> Response {
    let kept = ctx.session.inner().stash.key_change.clone().filter(|change| change.feed == name);
    let Some(change) = kept else {
        return go_to(ctx.htmx, &key_url(&name));
    };
    let effects = match sources::trust_new_key(&change, &form.typed) {
        Ok(effects) => effects,
        Err(problem) => return ctx.page(key_page(&change, &form.typed, Some(&problem))),
    };
    ctx.with(dest::SOURCES, move |work| {
        let reply = work.run(Step::run(effects), &mut no_flow);
        if reply.go.is_some() {
            work.session.inner().stash.key_change = None;
            Out::Reply(reply)
        } else {
            // Refused by the helper: the reason is shown on the decision's page.
            Out::Reply(Reply { go: Some(Dest::KeyChange(change)), ..reply })
        }
    })
    .await
}

/// `POST /sources/of/{name}/key/remove`: removing the source from its key change page asks
/// first. Confirmed, that page has nothing more to show and the owner is back at the sources.
pub async fn remove_from_key(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.respond(Reply::ask(sources::ask_remove(&name, true)).cancel_to(key_url(&name)), dest::SOURCES)
}
