//! A site's Hosting page and the way to the internet (`flows::web::hosting`, `flows::web::public`,
//! `flows::web::certs`): who can open the site, its address and other names, the reachability
//! check and what it found, the one confirmation, and the certificate that follows.
//!
//! The draft is a `Hosting` value kept in the session per site. It is made from the device's
//! state (`hosting::open`) and brought in line with it on every request (`Hosting::sync`):
//! whether the site was public and whether a provider is connected are the device's to say, and
//! no field of a form is read for them. The confirmation quotes the address of the
//! `PublicConfirm` the check produced, kept in the session; what the owner types is only
//! compared with it.
//!
//! The internet is never turned on by Save: Save applies the other choices, saves the names and
//! runs the check, as a job. A batch that moves towards public stops at its first failure
//! (`exec::run_batch`, which the step runner uses for every `Step::Run`).
//!
//! A site with one address shows only that one and an "Add another address" button; the boxes
//! for a new name open from it and close again once the name is on the list. A site that is
//! public can gain a name too: Save then asks about the new names first (`Hosting::save`), on
//! the page every question has, and the draft waits in the session for the answer.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::Response;
use axum::Form;
use maud::{html, Markup};
use nodo_api::AliasMode;
use nodo_ui_core::flows::staged::{self, Stageable};
use nodo_ui_core::flows::web::certs::{Certificate, POLL_SECONDS};
use nodo_ui_core::flows::web::hosting::{self, Hosting, Row};
use nodo_ui_core::flows::web::providers::{self, Providers};
use nodo_ui_core::flows::web::public::{Cgnat, CgnatWay, Found, PublicCheck, PublicConfirm};
use nodo_ui_core::flows::{Dest, Step};
use nodo_ui_core::model::{CheckState, Snapshot};
use nodo_ui_core::screen::Tab;
use nodo_ui_core::Outcome;
use serde::Deserialize;

use super::document_page;

use crate::ctx::{go_to, Ctx, JobSpec, Out, Work};
use crate::dest;
use crate::layout::{self, Page};
use crate::runner::{no_flow, Reply};

type Posted = Vec<(String, String)>;

fn posted<'a>(form: &'a Posted, name: &str) -> Option<&'a str> {
    form.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

fn trail(site: &str, last: Option<&str>) -> Vec<(String, String)> {
    let mut trail = vec![("Apps".to_owned(), dest::APPS.to_owned()), (site.to_owned(), dest::app(site)), ("Hosting".to_owned(), dest::hosting(site))];
    trail.extend(last.map(|last| (last.to_owned(), String::new())));
    trail
}

/// The draft for this request: the one kept in the session or a new one from the device's
/// state, either way brought in line with the providers connected now.
fn open_flow(work: &Work<'_>, site: &str) -> Result<Hosting, Reply> {
    let kept = work.session.inner().stash.hosting.get(site).cloned();
    let mut flow = match kept {
        Some(flow) => flow,
        None => hosting::open(&work.snapshot, site).map_err(Reply::notice)?,
    };
    flow.sync(&work.snapshot);
    Ok(flow)
}

fn keep(work: &Work<'_>, flow: &Hosting) {
    work.session.inner().stash.hosting.insert(flow.site.clone(), flow.clone());
}

/// Whether the site is on the internet now, which is the device's to say.
fn is_public(snapshot: &Snapshot, site: &str) -> bool {
    snapshot.web.sites.iter().any(|entry| entry.package == site && entry.exposure.public)
}

/// The form of the page. With htmx a tick posts the form and this alone is replaced, so the
/// rows that belong to the internet appear with its tick. `adding` says the boxes for a new name
/// are open; they stay closed until the owner asks for another address.
fn form(snapshot: &Snapshot, flow: &Hosting, adding: bool) -> Markup {
    let here = dest::hosting(&flow.site);
    let rows = flow.rows();
    let site = snapshot.web.sites.iter().find(|site| site.package == flow.site);
    let local_url = site.map(|site| site.local_url.as_str()).filter(|url| !url.is_empty());
    let public_now = is_public(snapshot, &flow.site);
    let going_public = flow.draft.public && !public_now;
    // A refusal is said beside the boxes, so they are open to show it.
    let adding = (adding || flow.alias_error.is_some()) && rows.contains(&Row::NewAlias);
    let added_now = flow.names_added_while_public(snapshot);
    let domains = snapshot.domain_choices();
    let provider = domains.iter().find(|(domain, _)| *domain == flow.domain).map(|(_, provider)| provider.as_str());
    let changes = flow.changes(snapshot);
    let tick = |name: &str, on: bool, label: &str, about: Markup| {
        let id = format!("reach-{name}");
        html! {
            div.frow {
                label for=(id) { (label) }
                div.fld {
                    span.check {
                        input type="checkbox" id=(id) name=(name) value="on" checked[on] hx-post=(here) hx-trigger="change" hx-target="#hosting" hx-swap="outerHTML";
                    }
                    p.hint { (about) }
                }
            }
        }
    };
    html! {
        form.stack #hosting method="post" action=(here) {
            (layout::hidden("ticks", "1"))
            section.sec {
                h2 { "Who can open it" }
                div.body {
                    (tick("local", flow.draft.local, "Home network", html! {
                        "Phones and computers on your Wi-Fi." @if let Some(url) = local_url { "   " span.mono { (url) } }
                        @if site.is_some_and(|site| site.local_waiting) { "   Waiting for a home address" }
                    }))
                    (tick("onion", flow.draft.onion, "Tor", html! { "A private link that opens in Tor Browser. It doesn't reveal your home." }))
                    (tick("public", flow.draft.public, "Internet", html! { "Anyone, at a web address you own." }))
                    @if rows.contains(&Row::AddProvider) {
                        div.frow {
                            span.lab { "Provider" }
                            div.fld {
                                p { "To use a web address you own, first add the company you bought it from." }
                                p.hint { "You'll come back here when it's added." }
                                button.btn.primary type="submit" name="do" value="add-provider" { "Add a provider" }
                            }
                        }
                    }
                }
            }
            @if rows.contains(&Row::Domain) {
                section.sec {
                    h2 { "Address" }
                    div.body {
                        @if let Some(provider) = provider {
                            div.frow { span.lab { "Provider" } div.fld { p { (provider) } } }
                        }
                        div.frow {
                            label for="sub" { "Address" }
                            div.fld {
                                div.inl {
                                    input type="text" id="sub" name="sub" value=(flow.sub) autocomplete="off" autocapitalize="none" spellcheck="false" disabled[public_now]
                                        hx-post=(here) hx-trigger="change" hx-target="#hosting" hx-swap="outerHTML";
                                    span.dim { "." }
                                    select name="domain" aria-label="Domain" disabled[public_now] hx-post=(here) hx-trigger="change" hx-target="#hosting" hx-swap="outerHTML" {
                                        @for (domain, _) in &domains { option value=(domain) selected[*domain == flow.domain] { (domain) } }
                                    }
                                }
                                p.hint {
                                    "https://" (flow.host()) "   Leave the box empty for " (flow.domain) " itself."
                                    @if public_now { " To change the address, take the site off the internet first." }
                                }
                                p.warn { "Visitors will be able to see your home's internet address." }
                                @if flow.draft.onion { p.hint { "With Tor on too, visitors could tell the two addresses are the same app." } }
                                @if site.is_some_and(|site| site.rate_limit_unenforced) { p.hint { "Repeated requests can't be slowed down for this app." } }
                            }
                        }
                        div.frow {
                            span.lab { @if !flow.aliases.is_empty() || adding { "Also answers at" } }
                            div.fld {
                                @for row in &rows {
                                    @if let Row::Alias(index) = row {
                                        @let alias = &flow.aliases[*index];
                                        div.inl {
                                            span.mono { (alias.host) }
                                            span.sub { @if alias.mode == AliasMode::Redirect { "redirects to " (flow.host()) } @else { "shows the same site" } }
                                            button.btn.danger type="submit" name="do" value=(format!("remove-alias:{}", alias.host)) { "Remove" }
                                        }
                                    }
                                }
                                @if adding {
                                    (layout::hidden("adding", "1"))
                                    div.field {
                                        label for="alias" { "New name" }
                                        div.inl {
                                            input type="text" id="alias" name="alias" value="" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="example.com"
                                                aria-invalid=[flow.alias_error.as_ref().map(|_| "true")] aria-describedby=[flow.alias_error.as_ref().map(|_| "alias-problem")];
                                            select name="mode" aria-label="What the name does" {
                                                option value="redirect" { "redirects here" }
                                                option value="serve" { "shows the same site" }
                                            }
                                            button.btn type="submit" name="do" value="add-alias" { "Add this name" }
                                            button.btn.quiet type="submit" name="do" value="close-alias" { "Cancel the new name" }
                                        }
                                        @if let Some(problem) = &flow.alias_error { p.problem #alias-problem { "✕ " (problem) } }
                                    }
                                } @else if rows.contains(&Row::NewAlias) {
                                    button.btn.link type="submit" name="do" value="open-alias" { "Add another address" }
                                }
                                @if !added_now.is_empty() {
                                    p.hint { (flow.site) " is on the internet, so a new name goes on it as soon as it is saved. Save asks first." }
                                }
                            }
                        }
                    }
                }
            }
            div.applybar {
                noscript { button.btn.quiet type="submit" name="do" value="apply" { "Show what this needs" } }
                @if rows.contains(&Row::Save) {
                    (layout::stage_buttons(if going_public { "Check & Apply" } else { "Save & Apply" }, layout::count_badge(changes), layout::count_badge(changes)))
                } @else {
                    button.btn type="submit" name="do" value="reset" { "Reset" }
                }
            }
            @if rows.contains(&Row::Save) {
                (layout::stage_hint())
            }
        }
    }
}

fn page(snapshot: &Snapshot, flow: &Hosting, adding: bool) -> Page {
    let desc = format!("Choose who can open {}. Any combination works.", flow.site);
    let page = super::apps::app_frame(snapshot, &flow.site, "Hosting", Some(&desc), format!("{} hosting", flow.site), form(snapshot, flow, adding));
    Page { trail: trail(&flow.site, None), ..page }
}

/// `GET /apps/{name}/hosting`.
pub async fn show(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::app(&name), move |work| match open_flow(work, &name) {
        Ok(flow) => {
            keep(work, &flow);
            Out::Page(page(&work.snapshot, &flow, false))
        }
        Err(reply) => Out::Reply(reply),
    })
    .await
}

/// Follows a step of the hosting flows. The checks and going public take long and run as a
/// job; everything else answers at once. A draft that was saved and left is dropped.
fn follow(work: &mut Work<'_>, step: Step, back: String) -> Out {
    // A draft that was saved is dropped where the reply lands (`Stash::keep`).
    if step.is_long() {
        let spec = JobSpec::new("Checking whether the internet can reach your Nodo", back)
            .note(&["The names are saved, then the Nodo is tried from outside. Nothing goes on the internet before you confirm."]);
        Out::Job(spec, Box::new(move |work| work.run(step, &mut no_flow)))
    } else {
        Out::Reply(work.run(step, &mut no_flow))
    }
}

/// `POST /apps/{name}/hosting`: every button of the page. The ticks and the address go into
/// the draft first, through the flow; then the button's action, named by `do`. Only the fields
/// the page has are read.
pub async fn act(ctx: Ctx, Path(name): Path<String>, Form(form_data): Form<Posted>) -> Response {
    let here = dest::hosting(&name);
    let fragment = ctx.htmx;
    ctx.with(here.clone(), move |work| {
        let mut flow = match open_flow(work, &name) {
            Ok(flow) => flow,
            Err(reply) => return Out::Reply(reply),
        };
        let on = |tick: &str| posted(&form_data, tick).is_some();
        if on("ticks") {
            flow.set_exposure(on("local"), on("onion"), on("public"));
        }
        // The address is fixed while the site is public, and its boxes are then not posted.
        let public_now = is_public(&work.snapshot, &name);
        if let (Some(sub), Some(domain), false) = (posted(&form_data, "sub"), posted(&form_data, "domain"), public_now) {
            flow.set_host(sub, domain, &work.snapshot);
        }
        // Whether the new name's boxes are open is the page's own, so the form carries it.
        let mut adding = on("adding");
        let action = posted(&form_data, "do").unwrap_or("apply");
        let (verb, argument) = action.split_once(':').unwrap_or((action, ""));
        let mut status = StatusCode::OK;
        match verb {
            // Save: the draft stays in the session and the owner goes back to the app.
            "keep" => {
                let key = flow.stage_key();
                let changes = flow.changes(&work.snapshot);
                let mut inner = work.session.inner();
                if changes == 0 {
                    inner.stash.hosting.remove(&name);
                    inner.notices.push(staged::nothing_to_keep(&key));
                } else {
                    inner.stash.hosting.insert(name.clone(), flow);
                    inner.notices.push(staged::saved_notice(&key, changes));
                }
                return Out::Go(dest::app(&name));
            }
            // Reset: the draft goes, and the page shows what the device serves.
            "reset" => {
                let mut inner = work.session.inner();
                inner.stash.hosting.remove(&name);
                inner.notices.push(staged::reset_notice(&flow.stage_key()));
                return Out::Go(here);
            }
            "save" => {
                // New names on a site that is public are asked about first (`Hosting::save`);
                // the draft waits here for the answer, and for Cancel.
                keep(work, &flow);
                let step = flow.save(&work.snapshot);
                return follow(work, step, here);
            }
            "open-alias" => {
                adding = true;
                flow.clear_alias_error();
            }
            "close-alias" => {
                adding = false;
                flow.clear_alias_error();
            }
            "add-alias" => {
                let mode = if posted(&form_data, "mode") == Some("serve") { AliasMode::Serve } else { AliasMode::Redirect };
                if flow.add_alias(posted(&form_data, "alias").unwrap_or_default().trim(), mode, &work.snapshot) {
                    // The name is on the list now; the boxes close until another one is wanted.
                    adding = false;
                } else {
                    adding = true;
                    status = StatusCode::UNPROCESSABLE_ENTITY;
                }
            }
            "remove-alias" => flow.remove_alias(argument),
            "add-provider" => {
                // The provider pages remember which Hosting page to come back to.
                keep(work, &flow);
                let mut adding = Providers::adding(Some(name.clone()));
                let reply = work.run(Step::run(providers::load()), &mut |outcome, snapshot| {
                    if let Outcome::ProvidersLoaded(result) = outcome {
                        providers::catalog_loaded(snapshot, Some(&adding), result);
                    }
                    Step::Stay
                });
                adding.clear_test();
                work.session.inner().stash.providers = Some((adding, None));
                return if reply.login_required { Out::Reply(reply) } else { Out::Go(dest::PROVIDERS.to_owned()) };
            }
            _ => {}
        }
        keep(work, &flow);
        if fragment {
            Out::Fragment(form(&work.snapshot, &flow, adding))
        } else if status == StatusCode::OK && !adding {
            Out::Go(here)
        } else {
            // The boxes open, or refused: the page again, with the reason beside the box. Said once.
            let shown = page(&work.snapshot, &flow, adding).status(status);
            flow.clear_alias_error();
            keep(work, &flow);
            Out::Page(shown)
        }
    })
    .await
}

fn kept_check(ctx: &Ctx, site: &str) -> Option<PublicCheck> {
    match &ctx.session.inner().stash.found {
        Some(Found::Problems(check)) if check.site == site => Some(check.clone()),
        _ => None,
    }
}

fn kept_cgnat(ctx: &Ctx, site: &str) -> Option<Cgnat> {
    match &ctx.session.inner().stash.found {
        Some(Found::Cgnat(cgnat)) if cgnat.site == site => Some(cgnat.clone()),
        _ => None,
    }
}

/// `GET /apps/{name}/hosting/check`: what the reachability check found, when something failed
/// or could not be verified.
pub async fn check(ctx: Ctx, Path(name): Path<String>) -> Response {
    let Some(found) = kept_check(&ctx, &name) else {
        return go_to(ctx.htmx, &dest::hosting(&name));
    };
    let here = format!("{}/check", dest::hosting(&name));
    let failed = found.checks.iter().any(|check| check.state == CheckState::Failed);
    let all_passed = found.checks.iter().all(|check| check.state == CheckState::Passed);
    let title = if failed {
        "The internet can't reach your Nodo yet"
    } else if all_passed {
        "The internet can reach your Nodo"
    } else {
        "Some checks couldn't be confirmed"
    };
    let body = html! {
        h1 { (title) }
        section.sec {
            h2 { "Reachability check" (layout::post(&format!("{here}/again"), "Check again", "btn")) }
            div.tbox {
                table {
                    thead { tr { th { "Check" } th { "Result" } th { "Detail" } } }
                    tbody {
                        @for check in &found.checks {
                            tr {
                                td { (check.text) }
                                td {
                                    @match check.state {
                                        CheckState::Passed => (layout::label("ok", "✓ Passed")),
                                        CheckState::Failed => (layout::label("bad", "✕ Failed")),
                                        CheckState::Unknown => (layout::label("warn", "? Not sure")),
                                    }
                                }
                                td.sub { (check.note) }
                            }
                        }
                    }
                }
            }
        }
        section.card {
            @if let Some(router) = &found.router {
                p { "Your router needs to send web visitors to the Nodo. In its settings, forward:" }
                dl.facts {
                    dt { "Port 80" } dd.mono { (router.forward_http) }
                    dt { "Port 443" } dd.mono { (router.forward_https) }
                }
                p.dim { "The router's settings page is usually " (router.admin_page) "." }
                p { a href=(format!("{here}/routers")) { "How to forward ports on common routers" } }
            }
            @for line in &found.manual { p { (line) } }
            @if let Some(plan) = &found.plan {
                dl.facts {
                    dt { "Internet address" } dd.mono { (plan.host) }
                    dt { "Record" } dd.mono { (plan.dns_record) }
                    dt { "Firewall" } dd { (plan.firewall) }
                }
            }
            p.dim { "A private Tor link works without any of this." }
            div.actions.end {
                a.btn.quiet href=(dest::hosting(&name)) { "Back" }
                @if found.can_continue() { (layout::post(&format!("{here}/continue"), "Continue anyway", "btn")) }
                (layout::post(&format!("{here}/tor"), "Use Tor instead", "btn primary"))
            }
        }
    };
    ctx.page(Page { trail: trail(&name, Some("Check")), ..Page::new(Tab::Apps, title, body) })
}

/// `POST /apps/{name}/hosting/check/{again|continue|tor}` and `GET …/check/routers`.
pub async fn check_act(ctx: Ctx, Path((name, what)): Path<(String, String)>) -> Response {
    let Some(found) = kept_check(&ctx, &name) else {
        return go_to(ctx.htmx, &dest::hosting(&name));
    };
    let here = format!("{}/check", dest::hosting(&name));
    // Only the guides are read; everything else here does something and is posted.
    if !ctx.post && what != "routers" {
        return go_to(ctx.htmx, &here);
    }
    ctx.with(here.clone(), move |work| match what.as_str() {
        "again" => follow(work, Step::run(found.check_again()), here),
        // Having seen what could not be verified, on to the confirmation. Nothing when a check
        // failed: the flow says so by staying.
        "continue" => Out::Reply(work.run(found.continue_anyway(), &mut no_flow)),
        "tor" => {
            let effects = found.use_tor(&work.snapshot);
            tor_instead(work, &name, effects)
        }
        "routers" => {
            let reply = work.run(Step::run(found.router_guides()), &mut no_flow);
            match reply.go {
                Some(Dest::Document(document)) => {
                    let mut page = document_page(Some(Tab::Apps), &document, &here);
                    page.trail = trail(&name, Some(&document.title));
                    Out::Page(page)
                }
                _ => Out::Reply(reply),
            }
        }
        _ => Out::Go(here),
    })
    .await
}

/// Turns Tor on instead of going public, as a check's page offers. The draft that wanted the
/// internet is dropped, and the owner is back at the list of sites.
fn tor_instead(work: &mut Work<'_>, site: &str, effects: Vec<nodo_ui_core::Effect>) -> Out {
    let mut reply = work.run(Step::run(effects), &mut no_flow);
    {
        let mut inner = work.session.inner();
        inner.stash.hosting.remove(site);
        inner.stash.found = None;
    }
    if reply.go.is_none() {
        reply.go = Some(Dest::LeaveHosting { site: site.to_owned() });
    }
    Out::Reply(reply)
}

fn way_name(way: CgnatWay) -> (&'static str, &'static str) {
    match way {
        CgnatWay::Tor => ("tor", "Use Tor instead"),
        CgnatWay::PublicIp => ("public-ip", "Ask your provider for a public address"),
        CgnatWay::Ipv6 => ("ipv6", "Use IPv6, if your provider has it"),
        CgnatWay::Relay => ("relay", "Use a relay that forwards visitors"),
    }
}

/// `GET /apps/{name}/hosting/cgnat`: the home shares its internet address, so no port can be
/// forwarded; the ways on.
pub async fn cgnat(ctx: Ctx, Path(name): Path<String>) -> Response {
    let Some(found) = kept_cgnat(&ctx, &name) else {
        return go_to(ctx.htmx, &dest::hosting(&name));
    };
    let here = format!("{}/cgnat", dest::hosting(&name));
    let body = html! {
        h1 { "The internet can't reach your Nodo" }
        section.card.attn {
            p { "Your internet provider shares one address between many homes, so visitors stop at the provider and never reach you. Router settings can't change this." }
            p { "A private Tor link still works on this connection." }
            dl.facts {
                dt { "Your router's outside address" } dd { span.mono { (found.wan_address) } " (in " (found.wan_range) ", the shared range)" }
                dt { "What the internet sees" } dd.mono { (found.internet_address) }
            }
            div.actions {
                @for way in CgnatWay::ALL {
                    @let (slug, label) = way_name(way);
                    (layout::post(&format!("{here}/{slug}"), label, if way == CgnatWay::Tor { "btn primary" } else { "btn" }))
                }
            }
            div.actions.end {
                a.btn.quiet href=(dest::hosting(&name)) { "Back" }
            }
        }
    };
    ctx.page(Page { trail: trail(&name, Some("Check")), ..Page::new(Tab::Apps, "The internet can't reach your Nodo", body) })
}

/// `POST /apps/{name}/hosting/cgnat/{way|again}`: Tor is turned on; the other ways are
/// explained, as documents.
pub async fn cgnat_act(ctx: Ctx, Path((name, what)): Path<(String, String)>) -> Response {
    let Some(found) = kept_cgnat(&ctx, &name) else {
        return go_to(ctx.htmx, &dest::hosting(&name));
    };
    let here = format!("{}/cgnat", dest::hosting(&name));
    ctx.with(here.clone(), move |work| {
        if what == "again" {
            return follow(work, Step::run(found.check_again()), here);
        }
        let Some(way) = CgnatWay::ALL.into_iter().find(|way| way_name(*way).0 == what) else {
            return Out::Go(here);
        };
        let effects = found.choose(way, &work.snapshot);
        // Tor is turned on, and that is the end of this way to the internet. The other ways are
        // explained, in a text that leads back here.
        if way == CgnatWay::Tor {
            return tor_instead(work, &name, effects);
        }
        let reply = work.run(Step::run(effects), &mut no_flow);
        Out::Reply(reply)
    })
    .await
}

fn kept_confirm(ctx: &Ctx, site: &str) -> Option<PublicConfirm> {
    ctx.session.inner().stash.going_public.clone().filter(|confirm| confirm.site == site)
}

fn confirm_page(snapshot: &Snapshot, confirm: &PublicConfirm, typed: &str, problem: Option<&str>) -> Page {
    let site = confirm.site.as_str();
    let host = confirm.plan.host.as_str();
    let here = format!("{}/confirm", dest::hosting(site));
    let entry = snapshot.web.sites.iter().find(|entry| entry.package == site);
    let aliases = entry.map(|entry| entry.public_aliases.clone()).unwrap_or_default();
    let domains = snapshot.domain_choices();
    let provider = hosting::split_host(host, &domains).and_then(|(_, domain)| domains.iter().find(|(each, _)| *each == domain)).map(|(_, provider)| provider.clone());
    let mut logged = vec![host.to_owned()];
    logged.extend(aliases.iter().map(|alias| alias.host.clone()));
    let body = html! {
        h1 { "Ready to put " (site) " on the internet" }
        section.card.attn {
            ul.bullets {
                li { span.ok aria-hidden="true" { "✓" } span { (provider.unwrap_or_else(|| "The provider".to_owned())) " accepted the key" } }
                li { span.ok aria-hidden="true" { "✓" } span { (host) " points to your home" } }
                li { span.ok aria-hidden="true" { "✓" } span { "The internet can reach your Nodo" } }
            }
            p { "Anyone will be able to open " strong.mono { (confirm.plan.url) } }
            @for alias in &aliases {
                p.dim { "and https://" (alias.host) "  " @if alias.mode == AliasMode::Redirect { "redirects to " (host) } @else { "shows the same site" } }
            }
            p.warn { "Your home's internet address becomes public." }
            p { "Records of it can stay online after you stop." }
            p { "Anyone can connect this address to your home, including your Monero node." }
            p.dim { "Public certificate logs list " (logged.join(", ")) " permanently." }
            p.dim { "The Nodo keeps the secure connection up to date. Take it offline at any time." }
            dl.facts {
                dt { "Record" } dd.mono { (confirm.plan.dns_record) }
                dt { "Firewall" } dd { (confirm.plan.firewall) }
            }
            // The address is typed, so the one step that puts a home on the internet is never a
            // stray click. Cancel comes first and has the focus.
            form.stack method="post" action=(here) {
                div.field {
                    label for="typed" { "Type " (host) " to confirm" }
                    input type="text" id="typed" name="typed" value=(typed) autocomplete="off" autocapitalize="none" spellcheck="false"
                        aria-invalid=[problem.map(|_| "true")] aria-describedby=[problem.map(|_| "typed-problem")];
                    @if let Some(problem) = problem { p.problem #typed-problem { "✕ " (problem) } }
                }
                div.actions.end {
                    a.btn href=(dest::hosting(site)) autofocus[confirm.cancel_is_default()] { "Cancel" }
                    button.btn.danger type="submit" { "Put " (site) " online" }
                }
            }
        }
    };
    let status = if problem.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page { trail: trail(site, Some("Confirm")), ..Page::new(Tab::Apps, format!("Put {site} online"), body) }.status(status)
}

/// `GET /apps/{name}/hosting/confirm`: the one confirmation, after the checks.
pub async fn confirm(ctx: Ctx, Path(name): Path<String>) -> Response {
    let Some(kept) = kept_confirm(&ctx, &name) else {
        return go_to(ctx.htmx, &dest::hosting(&name));
    };
    ctx.with(dest::hosting(&name), move |work| Out::Page(confirm_page(&work.snapshot, &kept, "", None))).await
}

#[derive(Debug, Default, Deserialize)]
pub struct ConfirmForm {
    #[serde(default)]
    typed: String,
}

/// `POST /apps/{name}/hosting/confirm`: puts the site online, if the address typed is the one
/// of the confirmation kept in the session. The protections go first and the batch stops there
/// when they fail, so the site is never public without them.
pub async fn go_public(ctx: Ctx, Path(name): Path<String>, Form(form_data): Form<ConfirmForm>) -> Response {
    let Some(kept) = kept_confirm(&ctx, &name) else {
        return go_to(ctx.htmx, &dest::hosting(&name));
    };
    let certificate = format!("{}/certificate", dest::hosting(&name));
    ctx.with(certificate.clone(), move |work| {
        let effects = match kept.confirm(form_data.typed.trim(), &work.snapshot) {
            Ok(effects) => effects,
            Err(problem) => return Out::Page(confirm_page(&work.snapshot, &kept, &form_data.typed, Some(&problem))),
        };
        // Where `Dest::WentPublic` leads: the certificate of the address just confirmed.
        let spec = JobSpec::new(format!("Putting {name} online"), certificate).note(&["Its protections are applied first, then its address is published."]);
        Out::Job(
            spec,
            Box::new(move |work| {
                let mut reply = work.run(Step::run(effects), &mut no_flow);
                // Not public after all: the confirmation again, with what was said.
                if reply.go.is_none() {
                    reply.go = Some(Dest::PublicConfirm(kept));
                }
                reply
            }),
        )
    })
    .await
}

/// The certificate followed on this site's page: the one of the address just confirmed, or,
/// for a page opened later, the site's public address as the device reports it.
fn certificate_of(work: &Work<'_>, site: &str) -> Option<Certificate> {
    let kept = work.session.inner().stash.certificate.clone().filter(|certificate| certificate.site == site);
    kept.or_else(|| {
        let entry = work.snapshot.web.sites.iter().find(|entry| entry.package == site && entry.exposure.public)?;
        Some(Certificate { site: site.to_owned(), host: entry.public_host.clone()? })
    })
}

/// How the certificate stands. While it is not there yet, htmx asks again every `POLL_SECONDS`.
fn certificate_status(snapshot: &Snapshot, certificate: &Certificate) -> Markup {
    let site = certificate.site.as_str();
    let entry = snapshot.web.sites.iter().find(|entry| entry.package == site);
    let valid = certificate.ready(snapshot);
    let (standing, heading) = certificate.heading(snapshot);
    let (sign, class) = crate::glue::tone(standing);
    let poll = format!("{}/certificate/status", dest::hosting(site));
    html! {
        section.card #certificate hx-get=[(!valid).then_some(poll)] hx-trigger=[(!valid).then(|| format!("every {POLL_SECONDS}s"))] hx-swap="outerHTML" {
            h2 { span class=(class) aria-hidden="true" { (sign) " " } (heading) }
            p.mono { "https://" (certificate.host) }
            @if let Some(entry) = entry {
                @for alias in &entry.public_aliases {
                    p.mono { "https://" (alias.host) @if alias.mode == AliasMode::Redirect { span.dim { "  redirects here" } } }
                }
                @if entry.exposure.local && !entry.local_url.is_empty() { p.mono { (entry.local_url) } }
            }
            @if let Some(problem) = certificate.problem(snapshot) { p.err { (problem) } }
            p.dim { (certificate.outlook(snapshot)) }
        }
    }
}

/// `GET /apps/{name}/hosting/certificate`: the site is public; its secure connection is
/// followed until it is there.
pub async fn certificate(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::hosting(&name), move |work| {
        let Some(certificate) = certificate_of(work, &name) else {
            return Out::Go(dest::hosting(&name));
        };
        let waiting = !certificate.ready(&work.snapshot);
        let body = html! {
            h1 { "Secure connection" }
            (certificate_status(&work.snapshot, &certificate))
            div.actions.end {
                a.btn.quiet href=(dest::log(&name)) { "Logs" }
                a.btn.quiet href=(dest::CERTIFICATES) { "Certificates" }
                a.btn.primary href=(dest::WEB) { "Done" }
            }
        };
        let page = Page { trail: trail(&name, Some("Secure connection")), ..Page::new(Tab::Apps, format!("{name} is online"), body) };
        Out::Page(if waiting { page.waiting() } else { page })
    })
    .await
}

/// `GET /apps/{name}/hosting/certificate/status`: the certificate's state alone, read again as
/// the flow says. A polling route: it does not keep a session alive.
pub async fn certificate_status_poll(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::hosting(&name), move |work| {
        let Some(certificate) = certificate_of(work, &name) else {
            return Out::Go(dest::hosting(&name));
        };
        let reply = work.run(Step::run(certificate.poll()), &mut no_flow);
        if reply.login_required {
            return Out::Reply(reply);
        }
        Out::Fragment(certificate_status(&work.snapshot, &certificate))
    })
    .await
}
