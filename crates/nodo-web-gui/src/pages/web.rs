//! The Web tab (`flows::web`): the sites that are online, domain providers, certificate
//! settings and local names, the local CA, and onion addresses.

use axum::http::StatusCode;
use axum::response::Response;
use axum::Form;
use maud::{html, Markup};
use nodo_api::{LocalNameVerdict, SiteId};
use nodo_ui_core::flows::web::certs::Trust;
use nodo_ui_core::flows::web::domain::{self, Domain, Field};
use nodo_ui_core::flows::web::onion::{Onion, Row as OnionRow};
use nodo_ui_core::flows::web::providers::{self, KeyField, ProviderStage, Providers};
use nodo_ui_core::flows::web::{self, hosting};
use nodo_ui_core::flows::{Dest, Notice, Step};
use nodo_ui_core::model::{Provider, Snapshot};
use nodo_ui_core::screen::Tab;
use nodo_ui_core::Outcome;

use super::document_page;
use crate::core_wishlist::{tone, typed_secret};
use nodo_ui_core::words::{cert_status, provider_field_name, reach_words, site_address};
use crate::ctx::{Ctx, Out, Work};
use crate::dest;
use crate::layout::{self, Page};
use crate::runner::{follow_plain, no_flow, Reply};

type Posted = Vec<(String, String)>;

fn posted<'a>(form: &'a Posted, name: &str) -> Option<&'a str> {
    form.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

fn web_trail(last: &[(&str, &str)]) -> Vec<(String, String)> {
    std::iter::once(&("Web", dest::WEB)).chain(last).map(|(name, url)| ((*name).to_owned(), (*url).to_owned())).collect()
}

// The sites

/// `GET /web`: the sites that are online, who can reach each and where.
pub async fn list(ctx: Ctx) -> Response {
    ctx.with(dest::HOME, |work| {
        let snapshot = &work.snapshot;
        let sites = snapshot.active_sites();
        let any_public = sites.iter().any(|site| site.exposure.public);
        let certificates = if snapshot.web.certificates.is_empty() { "Certificates and home names".to_owned() } else { snapshot.web.certificates.clone() };
        let body = html! {
            div.head {
                h1 { "Sites online" }
                @if web::ask_all_offline(snapshot).is_some() {
                    div.actions { (layout::ask(&format!("{}/offline", dest::WEB), "Take all offline…", "btn danger")) }
                }
            }
            section.card.flush {
                @if sites.is_empty() { p.pad.dim { "No site is online yet." } }
                div.rows {
                    @for site in &sites {
                        (layout::row(&dest::hosting(&site.package), &site.package, &site_address(site), html! {
                            (reach_words(site)) @if site.locked { " · locked until unlocked" } @if site.local_waiting { " · waiting for a home address" }
                        }))
                    }
                }
            }
            p.dim { "To put another app online, open it in " a href=(dest::APPS) { "Apps" } "." }
            @if !any_public { p.dim { "Nothing is on the internet, so ports 80 and 443 are closed on this device." } }
            @if let Some(problem) = &snapshot.web.local_problem {
                section.card.attn {
                    p { span.warn aria-hidden="true" { "▲ " } (problem) }
                    @if let Some(remedy) = &snapshot.web.local_remedy { p.dim { (remedy) } }
                }
            }
            section.card.flush {
                div.rows {
                    (layout::row(dest::PROVIDERS, "Domain providers", &snapshot.connected.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", "), html! {}))
                    (layout::row(dest::DOMAIN, "Certificates and home names", &certificates, html! {}))
                    (layout::row(dest::CERTIFICATES, "Certificates", "How each address's secure connection stands", html! {}))
                    (layout::row(dest::TRUST, "Local CA", "Trust this Nodo's .local sites on your devices", html! {}))
                    (layout::row(dest::ONION, "Onion addresses", "Tor links for the node and the sites", html! {}))
                }
            }
            @if any_public && !snapshot.web.public_ip.is_empty() {
                p.dim.small { "Home's internet address " span.mono { (snapshot.web.public_ip) } @if !snapshot.web.dns_changed.is_empty() { " · " (snapshot.web.dns_changed) } }
            }
        };
        Out::Page(Page::new(Tab::Web, "Web", body))
    })
    .await
}

/// `POST /web/offline`: taking every site offline asks first. Nothing when none is online.
pub async fn all_offline(ctx: Ctx) -> Response {
    ctx.with(dest::WEB, |work| match web::ask_all_offline(&work.snapshot) {
        Some(question) => Out::Reply(Reply::ask(question)),
        None => Out::Go(dest::WEB.to_owned()),
    })
    .await
}

// Domain providers

const MISSING_DOMAIN: &str = "Missing a domain? Allow the key to change it in the provider's account (Porkbun: API Access, on each domain), then look again under Domain providers.";

/// The provider pages' flow and the company chosen, as kept, or the list.
fn kept_providers(work: &Work<'_>) -> (Providers, Option<String>) {
    work.session.inner().stash.providers.clone().unwrap_or_else(|| (Providers::list(), None))
}

fn keep_providers(work: &Work<'_>, flow: &Providers, chosen: Option<String>) {
    work.session.inner().stash.providers = Some((flow.clone(), chosen));
}

/// The label of a key box, as the provider names its credentials.
fn key_label(provider: &Provider, field: KeyField) -> String {
    let name = |index: usize| provider.fields.get(index).map(|field| provider_field_name(field));
    match field {
        KeyField::Domain => "Web address you bought".to_owned(),
        KeyField::ApiKey => name(0).unwrap_or_else(|| "API key".to_owned()),
        KeyField::SecretKey => name(1).unwrap_or_else(|| "Secret key".to_owned()),
    }
}

fn providers_page(snapshot: &Snapshot, flow: &Providers, chosen: Option<&str>, marked: Option<KeyField>, domain: &str) -> Page {
    let here = dest::PROVIDERS;
    let button = |action: &str, label: &str, class: &str| html! { button class=(class) type="submit" name="do" value=(action) { (label) } };
    let test = html! {
        @match &flow.test {
            Some(Err(problem)) => p.notice.error { "✕ " (problem) },
            Some(Ok(said)) => p.notice.ok { "✓ " (said) },
            None => {},
        }
    };
    let provider = chosen.and_then(|name| snapshot.providers.iter().find(|provider| provider.name == name));
    let body = match (&flow.stage, provider) {
        (ProviderStage::Pick, _) => html! {
            h1 { "Where did you buy your web address?" }
            (test)
            @if let Some(error) = &snapshot.providers_error { p.notice.error { "✕ Couldn't read the providers: " (error) } }
            form method="post" action=(here) {
                section.card.flush {
                    @if snapshot.providers.is_empty() { p.pad.dim { "No providers are offered." } }
                    div.rows {
                        @for each in &snapshot.providers {
                            div class=(if each.is_supported() { "row" } else { "row off" }) {
                                span.name { (each.name) }
                                span.what { @if !each.is_supported() { "Can't be connected: no way to change records" } }
                                span.end { button.btn type="submit" name="choose" value=(each.name) { "Choose" } }
                                span.go {}
                            }
                        }
                    }
                }
                p.dim { "Somewhere else? It can't be connected yet, but Tor works without a web address." }
                div.actions.end { (button("back", "Back", "btn")) }
            }
        },
        (ProviderStage::Key, Some(provider)) => html! {
            h1 { "Connect " (provider.name) }
            (test)
            section.card {
                p { "The Nodo asks " (provider.name) " which domains this key controls. The key stays here, locked, never shown again." }
                p.dim { (provider.instructions) }
                @if let Some(note) = &provider.dns01_note { p.dim { (note) } }
                @if snapshot.is_locked() { p.warn { "This Nodo is locked. Connect asks for the device passphrase first, then stores the key." } }
                // The key boxes are always empty: a key typed is sent once and never kept.
                form.stack method="post" action=(here) {
                    @for field in providers::fields(Some(provider)) {
                        @let (name, kind) = match field { KeyField::Domain => ("domain", "text"), KeyField::ApiKey => ("api_key", "password"), KeyField::SecretKey => ("secret_key", "password") };
                        div.field {
                            label for=(name) { (key_label(provider, field)) }
                            input type=(kind) id=(name) name=(name) value=(if field == KeyField::Domain { domain } else { "" }) autocomplete="off" autocapitalize="none" spellcheck="false"
                                placeholder=[(field == KeyField::Domain).then_some("example.com")] aria-invalid=[(marked == Some(field)).then_some("true")];
                        }
                    }
                    div.actions.end {
                        (button("help", "How to make a key", "btn quiet"))
                        (button("back", "Back", "btn"))
                        (button("connect", &format!("Connect {}", provider.name), "btn primary"))
                    }
                }
            }
        },
        (ProviderStage::Done { provider, domains, held_by }, _) => html! {
            h1 { span.ok aria-hidden="true" { "✓ " } (provider) " is connected" }
            section.card {
                @if domains.is_empty() {
                    @if !held_by.is_empty() {
                        p { "Every domain this key can change is already connected through " (held_by.join(", ")) "." }
                        p.dim { "If it is the same account, remove this one: it adds nothing." }
                    }
                } @else {
                    p { "Apps can now use these web addresses:" }
                    ul.bullets { @for domain in domains { li { span.ok aria-hidden="true" { "✓" } span.mono { (domain) } } } }
                }
                p.dim.small { (MISSING_DOMAIN) }
                form.actions.end method="post" action=(here) { (button("finish", "Done", "btn primary")) }
            }
        },
        // The list, and a key step whose company is not offered any more.
        _ => html! {
            div.head {
                h1 { "Domain providers" }
                form.actions method="post" action=(here) { (button("add", "Add a provider", "btn primary")) }
            }
            p.lead { "The companies that hold your web addresses. Add each once; then any app can use their domains." }
            (test)
            @if snapshot.connected.is_empty() { section.card { p.dim { "To use a web address you own, first add the company you bought it from." } } }
            @for each in &snapshot.connected {
                section.card {
                    div.head {
                        h2 { (each.name) }
                        div.actions {
                            form method="post" action=(here) { button.btn type="submit" name="refresh" value=(each.id) { "Look again" } }
                            form method="post" action=(here) hx-post=(here) hx-target="#dialog" hx-swap="innerHTML" {
                                button.btn.danger type="submit" name="remove" value=(each.id) { "Remove…" }
                            }
                        }
                    }
                    @if let Some(problem) = &each.problem { p.err { "✕ " (problem) } }
                    ul.bullets {
                        @for domain in &each.domains {
                            li {
                                span.dim aria-hidden="true" { "·" }
                                span {
                                    span.mono { (domain.name) }
                                    @if let Some(other) = &domain.held_by { span.note { "   is used through " (other) } }
                                    @if !domain.used_by.is_empty() { span.note { "   used by " (domain.used_by.join(", ")) } }
                                }
                            }
                        }
                    }
                }
            }
            p.dim.small { "The key stays on this Nodo, locked with your device passphrase, and is never shown again. " (MISSING_DOMAIN) }
        },
    };
    let status = if marked.is_some() || matches!(flow.test, Some(Err(_))) { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page { trail: web_trail(&[("Domain providers", dest::PROVIDERS)]), ..Page::new(Tab::Web, "Domain providers", body) }.status(status)
}

/// Leaves the provider pages when a reply names a page to go to: they are done with, and
/// `back` is where `Dest::Back` and `Dest::HostingWithProvider` lead from here.
fn leave(work: &Work<'_>, reply: Reply, back: &str) -> Out {
    match &reply.go {
        Some(dest) if !reply.login_required => {
            let url = dest::url(dest, back);
            let mut inner = work.session.inner();
            inner.notices.extend(reply.notices);
            inner.stash.providers = None;
            Out::Go(url)
        }
        _ => Out::Reply(reply),
    }
}

/// `GET /web/providers`: the providers connected, or the step of adding one that was left open.
pub async fn providers_show(ctx: Ctx) -> Response {
    ctx.with(dest::WEB, |work| {
        let (mut flow, chosen) = kept_providers(work);
        // A refusal is said once: on the answer to the post that met it.
        flow.clear_test();
        Out::Page(providers_page(&work.snapshot, &flow, chosen.as_deref(), None, ""))
    })
    .await
}

/// `POST /web/providers`: every button of the provider pages. The keys arrive with the Connect
/// button and go straight to the flow, which hands them to the helper or leaves them be.
pub async fn providers_act(ctx: Ctx, Form(form): Form<Posted>) -> Response {
    ctx.with(dest::PROVIDERS, move |work| {
        let (mut flow, mut chosen) = kept_providers(work);
        flow.clear_test();
        let hosting_page = flow.return_to.as_deref().map(dest::hosting);
        // Where `Dest::Back` leads from here: the Hosting page that opened this, or the Web tab.
        let back = hosting_page.clone().unwrap_or_else(|| dest::WEB.to_owned());
        if let Some(id) = posted(&form, "refresh") {
            return Out::Reply(work.run(Step::run(providers::refresh(id)), &mut no_flow));
        }
        if let Some(id) = posted(&form, "remove") {
            return match work.snapshot.connected.iter().find(|provider| provider.id == id) {
                Some(provider) => Out::Reply(Reply::ask(providers::ask_remove(provider))),
                None => Out::Go(dest::PROVIDERS.to_owned()),
            };
        }
        if let Some(name) = posted(&form, "choose") {
            // The company chosen; the key step starts with empty boxes.
            if let Some(provider) = work.snapshot.providers.iter().find(|provider| provider.name == name).cloned() {
                chosen = flow.choose(&provider).then_some(provider.name);
            }
            keep_providers(work, &flow, chosen.clone());
            return Out::Page(providers_page(&work.snapshot, &flow, chosen.as_deref(), None, ""));
        }
        let provider = chosen.as_deref().and_then(|name| work.snapshot.providers.iter().find(|provider| provider.name == name)).cloned();
        match (posted(&form, "do").unwrap_or_default(), provider) {
            ("add", _) => {
                let effects = flow.add();
                let reply = work.run(Step::run(effects), &mut |outcome, snapshot| {
                    if let Outcome::ProvidersLoaded(result) = outcome {
                        providers::catalog_loaded(snapshot, Some(&flow), result);
                    }
                    Step::Stay
                });
                keep_providers(work, &flow, None);
                if reply.login_required { Out::Reply(reply) } else { Out::Go(dest::PROVIDERS.to_owned()) }
            }
            ("back", _) => {
                let step = flow.back();
                keep_providers(work, &flow, chosen);
                leave(work, follow_plain(step), &back)
            }
            ("finish", _) => {
                let returning = flow.return_to.clone();
                let step = flow.finish();
                let reply = work.run(step, &mut no_flow);
                keep_providers(work, &flow, None);
                // Back on the Hosting page that sent the owner here: it has a provider now.
                if let (Some(Dest::HostingWithProvider { domains }), Some(site)) = (&reply.go, returning) {
                    let mut inner = work.session.inner();
                    if let Some(draft) = inner.stash.hosting.get_mut(&site) {
                        draft.provider_connected(domains);
                    } else if let Ok(mut draft) = hosting::open(&work.snapshot, &site) {
                        draft.provider_connected(domains);
                        inner.stash.hosting.insert(site, draft);
                    }
                }
                leave(work, reply, &back)
            }
            ("help", Some(provider)) => {
                let reply = work.run(Step::run(providers::key_help(&provider)), &mut no_flow);
                match reply.go {
                    Some(Dest::Document(document)) => {
                        let mut page = document_page(Some(Tab::Web), &document, dest::PROVIDERS);
                        page.trail = web_trail(&[("Domain providers", dest::PROVIDERS), (&document.title, "")]);
                        Out::Page(page)
                    }
                    _ => Out::Reply(reply),
                }
            }
            ("connect", Some(provider)) => {
                let domain = posted(&form, "domain").unwrap_or_default().to_owned();
                let mut api_key = typed_secret(posted(&form, "api_key").unwrap_or_default());
                let mut secret_key = typed_secret(posted(&form, "secret_key").unwrap_or_default());
                match flow.submit(&provider, &domain, &mut api_key, &mut secret_key, &work.snapshot) {
                    // Something is missing: the box to fix is marked, and nothing was sent.
                    Err(field) => {
                        keep_providers(work, &flow, chosen.clone());
                        Out::Page(providers_page(&work.snapshot, &flow, chosen.as_deref(), Some(field), &domain))
                    }
                    Ok(step) => {
                        let reply = work.run(step, &mut |outcome, _| flow.apply(outcome));
                        keep_providers(work, &flow, chosen.clone());
                        // A locked device is unlocked first, on the page that says why, and the
                        // owner comes back here to press Connect again.
                        if reply.login_required || matches!(reply.go, Some(Dest::Unlock { .. })) {
                            return Out::Reply(reply);
                        }
                        // Connected, or refused in the helper's words above the boxes.
                        Out::Page(providers_page(&work.snapshot, &flow, chosen.as_deref(), None, &domain))
                    }
                }
            }
            _ => Out::Go(dest::PROVIDERS.to_owned()),
        }
    })
    .await
}

// Certificates and home names

/// What the form posted for the fields that are typed, kept for a page drawn again.
#[derive(Default)]
struct DomainTyped {
    email: Option<String>,
    local_domain: Option<String>,
}

fn domain_page(snapshot: &Snapshot, flow: &Domain, typed: &DomainTyped) -> Page {
    let acme = &snapshot.web.acme;
    let fields = domain::fields(acme);
    let validation = domain::validation(acme);
    let labels = domain::dns01_labels(acme);
    let suggested = domain::suggested_local_domain(acme, &snapshot.web.domain);
    let local_domain = typed.local_domain.clone().unwrap_or(suggested);
    let email = typed.email.clone().unwrap_or_else(|| acme.email.clone());
    let body = html! {
        div.head {
            h1 { "Certificates and home names" }
            div.actions { a.btn.quiet href=(dest::CERTIFICATES) { "Certificates ›" } a.btn.quiet href=(dest::TRUST) { "Local CA ›" } }
        }
        // The helper's refusal of a save, in its words, above the fields.
        @if let Some(error) = &flow.error { p.notice.error { "✕ " (error) } }
        form.stack method="post" action=(dest::DOMAIN) {
            section.card {
                h2 { "Let's Encrypt" }
                p.dim { (validation.method) " · " (validation.note) }
                @if let Some(server) = &acme.test_server { p.warn { "▲ Test ACME server: " (server) } }
                @for field in &fields {
                    @match field {
                        Field::Email => div.field {
                            label for="email" { "Email for expiry notices" }
                            input type="text" id="email" name="email" value=(email) autocomplete="email" inputmode="email" placeholder="none, recommended";
                        },
                        Field::Staging => div.setting {
                            label.check { input type="checkbox" name="staging" value="on" checked[flow.staging]; strong { "Use the staging server" } }
                            p.dim { "Untrusted certificates, for testing" }
                        },
                        _ => {},
                    }
                }
            }
            section.card {
                h2 { "Local site names" }
                @for field in &fields {
                    @match field {
                        Field::DotLocal => div.setting {
                            label.check { input type="radio" name="naming" value="local" checked[!flow.under_domain]; strong { "site.local, with this device's own CA" } }
                            p.dim { "Browsers warn until you " a href=(dest::TRUST) { "trust it on your devices" } "." }
                        },
                        Field::UnderDomain => div.setting {
                            label.check { input type="radio" name="naming" value="domain" checked[flow.under_domain]; strong { "Under your domain" } }
                            div.field {
                                label for="local_domain" { "Local sites go under" }
                                input type="text" id="local_domain" name="local_domain" value=(local_domain) autocomplete="off" autocapitalize="none" spellcheck="false";
                                p.hint { @if validation.dns01 { "Real certificates through DNS-01" } @else { "Needs DNS-01 from your provider" } }
                            }
                            @if let Some(address) = &acme.local_address {
                                p.warn { "▲ Each name gets a public DNS record with this device's LAN address, " (address) "." }
                            }
                            @if let Some(note) = &snapshot.web.local_records_note { p.dim { (note) } }
                        },
                        _ => {},
                    }
                }
            }
            @if fields.contains(&Field::Dns01First) {
                section.card {
                    h2 { (if snapshot.web.provider.is_empty() { "Your provider" } else { snapshot.web.provider.as_str() }) " certificate credentials" }
                    p.dim { "Needed for DNS-01. Both are stored locked and never shown again; one alone changes nothing." }
                    @for (index, name) in ["dns01_first", "dns01_second"].iter().enumerate() {
                        div.field {
                            label for=(name) { (labels.get(index).cloned().unwrap_or_else(|| "Credential".to_owned())) }
                            input type="password" id=(name) name=(name) autocomplete="new-password" placeholder=[acme.dns01_stored.then_some("stored, type to replace")];
                        }
                    }
                }
            }
            div.actions.end {
                button.btn type="submit" name="do" value="check" { "Test local names" }
                button.btn.primary type="submit" name="do" value="save" { "Save" }
            }
        }
        @match &flow.checks {
            Some(Err(problem)) => p.notice.error { "✕ " (problem) },
            Some(Ok(checks)) => section.card #local-names {
                h2 { "Local names on this network" }
                @if checks.is_empty() { p.dim { "No local site uses a name under the domain yet." } }
                ul.bullets {
                    @for check in checks {
                        li {
                            @match check.verdict {
                                LocalNameVerdict::Resolves => span.ok aria-hidden="true" { "✓" },
                                LocalNameVerdict::Blocked => span.warn aria-hidden="true" { "▲" },
                                _ => span.err aria-hidden="true" { "✕" },
                            }
                            span {
                                span.mono { (check.host) } "   "
                                @match check.verdict {
                                    LocalNameVerdict::Resolves => { "Resolves to " (check.address) },
                                    LocalNameVerdict::NotPublished => { "Not published" },
                                    LocalNameVerdict::Blocked => { "Hidden by this network" },
                                    LocalNameVerdict::WrongAddress => { "Wrong address" },
                                }
                                @if check.verdict != LocalNameVerdict::Resolves { span.note { "   " (check.explanation) } }
                            }
                        }
                    }
                }
            },
            None => {},
        }
    };
    let status = if flow.error.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page { trail: web_trail(&[("Certificates and home names", dest::DOMAIN)]), ..Page::new(Tab::Web, "Certificates and home names", body) }.status(status)
}

/// `GET /web/domain`.
pub async fn domain_show(ctx: Ctx) -> Response {
    ctx.with(dest::WEB, |work| Out::Page(domain_page(&work.snapshot, &Domain::new(&work.snapshot.web.acme), &DomainTyped::default()))).await
}

/// `POST /web/domain`: Save, or the test of the local names. The page keeps nothing: its
/// choices are made from the device's state and what the form carries. A refused save is shown
/// above the fields with what was typed; the credentials are never sent back.
pub async fn domain_act(ctx: Ctx, Form(form): Form<Posted>) -> Response {
    ctx.with(dest::DOMAIN, move |work| {
        let mut flow = Domain::new(&work.snapshot.web.acme);
        flow.set_staging(posted(&form, "staging").is_some());
        if let Some(naming) = posted(&form, "naming") {
            flow.set_under_domain(naming == "domain");
        }
        let typed = DomainTyped { email: posted(&form, "email").map(str::to_owned), local_domain: posted(&form, "local_domain").map(str::to_owned) };
        let effects = if posted(&form, "do") == Some("check") {
            domain::check_local_names()
        } else {
            let mut first = typed_secret(posted(&form, "dns01_first").unwrap_or_default());
            let mut second = typed_secret(posted(&form, "dns01_second").unwrap_or_default());
            flow.save(typed.email.as_deref().unwrap_or_default(), typed.local_domain.as_deref().unwrap_or_default(), &mut first, &mut second)
        };
        let reply = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
        if reply.login_required || (flow.error.is_none() && flow.checks.is_none()) {
            // Saved: the page again, read afresh, with the flow's words.
            return Out::Reply(reply);
        }
        Out::Page(domain_page(&work.snapshot, &flow, &typed))
    })
    .await
}

/// `GET /web/certificates`: how each address's secure connection stands.
pub async fn certificates(ctx: Ctx) -> Response {
    ctx.with(dest::WEB, |work| {
        let snapshot = &work.snapshot;
        let troubled = snapshot.web.certs.iter().filter(|row| matches!(row.state, nodo_api::CertState::Failed | nodo_api::CertState::Expired)).count();
        let waiting = snapshot.web.certs.iter().any(|row| matches!(row.state, nodo_api::CertState::Waiting | nodo_api::CertState::Obtaining));
        let body = html! {
            div.head {
                h1 { "Certificates" }
                @match (troubled, waiting) {
                    (0, false) => p.ok { "✓ Certificates in place" },
                    (0, true) => p.warn { "◆ Certificates on the way" },
                    (1, _) => p.err { "✕ 1 certificate needs attention" },
                    (n, _) => p.err { "✕ " (n) " certificates need attention" },
                }
            }
            section.card.flush {
                @if snapshot.web.certs.is_empty() { p.pad.dim { "No site is served yet." } }
                div.rows {
                    @for row in &snapshot.web.certs {
                        @let (how, words) = cert_status(row);
                        @let (sign, class) = tone(how);
                        div.row {
                            span.name.mono { (row.host) }
                            span.what {
                                (row.site) @if !row.issuer.is_empty() { " · " (row.issuer) }
                                @if let Some(problem) = &row.problem { span.note.err { " · " (problem) } }
                            }
                            span class=(format!("end {class}")) { (sign) " " (words) }
                            span.go {}
                        }
                    }
                }
            }
            div.actions.end { a.btn href=(dest::DOMAIN) { "Certificate settings" } a.btn href=(dest::TRUST) { "Local CA" } }
        };
        Out::Page(Page { trail: web_trail(&[("Certificates", dest::CERTIFICATES)]), ..Page::new(Tab::Web, "Certificates", body) })
    })
    .await
}

/// `GET /web/trust`: the local CA. Its root is offered by the page Caddy serves on the home
/// network over plain HTTP; this page links there and shows the fingerprint to compare. No file
/// is served from here.
pub async fn trust(ctx: Ctx) -> Response {
    ctx.with(dest::WEB, |work| {
        let trust = Trust::of(&work.snapshot);
        let lines = trust.fingerprint_lines();
        let body = html! {
            div.head {
                h1 { "Local CA" }
                @match &trust.url { Some(url) => p.ok { "✓ Served at " (url) }, None => p.dim { "Not served now" } }
            }
            section.card {
                h2 { "Trust page" }
                @match &trust.url {
                    Some(url) => p { "Open on a phone or computer here   " a.mono href=(url) rel="noreferrer" { (url) } },
                    None => p.dim { "The page is served while a local site uses a .local name and the local address is up." },
                }
                p { "It offers this device's root certificate and short install steps for Android, iPhone, macOS, Windows, Linux and Firefox. It needs no port and no app." }
                p.dim { "Only the local network can open it, on the local address. It is never public or on Tor." }
            }
            section.card.attn {
                h2 { "SHA-256 fingerprint" }
                @if lines.is_empty() { p.dim { "Caddy has not created the local CA yet." } }
                @for line in &lines { p.fp.check { (line) } }
                p { span.warn aria-hidden="true" { "▲ " } "The page is plain HTTP. Install the root only if the page shows this same fingerprint." }
            }
            section.card {
                h2 { "What trusting it does" }
                p { "Browsers on that device then open this device's .local sites without a warning." }
                p.dim { "The root vouches only for this device's local sites. Remove it from a device to stop trusting them. Names under your own domain get public certificates instead." }
            }
        };
        Out::Page(Page { trail: web_trail(&[("Local CA", dest::TRUST)]), ..Page::new(Tab::Web, "Local CA", body) })
    })
    .await
}

// Onion addresses

/// The onion page's flow with what Tor reports.
fn onion_loaded(work: &mut Work<'_>) -> (Onion, Reply) {
    let mut flow = Onion::new();
    let reply = work.run(Step::run(Onion::load()), &mut |outcome, _| flow.apply(outcome));
    (flow, reply)
}

fn onion_page(flow: &Onion) -> Markup {
    let action = |what: &str| format!("{}/{what}", dest::ONION);
    let switch = |on: bool| if on { "● On" } else { "○ Off" };
    html! {
        div.head {
            h1 { "Onion addresses" }
            @if let Some(status) = &flow.status {
                @if status.tor_running { p.ok { "● Tor running" } } @else if status.tor_installed { p.warn { "○ Tor is not running" } } @else { p.dim { "Tor is not installed" } }
            }
        }
        @if let Some(error) = &flow.error { p.notice.error { "✕ " (error) } }
        @if let Some(status) = &flow.status {
            @for row in flow.rows() {
                @match row {
                    OnionRow::Node => { @if let Some(node) = &status.node {
                        section.card {
                            h3 { "Node" }
                            div.head {
                                p { strong { (node.provider) } span.dim { "   restricted RPC, port " (node.port) } }
                                div.actions {
                                    (layout::post(&action("node-settings"), "Its settings", "btn quiet"))
                                    form method="post" action=(action("node")) { button.btn type="submit" aria-pressed=(if node.enabled { "true" } else { "false" }) { (switch(node.enabled)) } }
                                }
                            }
                            @if let Some(address) = &node.address { p.mono { (address) } }
                            @if !node.restricted_rpc { p.dim { "It does not say which port serves its restricted RPC." } }
                        }
                    } },
                    OnionRow::Site(index) => { @if let Some(site) = status.sites.get(index) {
                        @let others: Vec<&str> = [(site.exposure.local, "local"), (site.exposure.public, "public")].iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect();
                        section.card {
                            div.head {
                                p {
                                    strong { (site.id.package) }
                                    @if !others.is_empty() { span.dim { "   also " (others.join(", ")) } }
                                    @if !site.multi_address { span.dim { "   single address" } }
                                    @if site.locked { span.warn { "   ◆ locked" } }
                                }
                                form method="post" action=(action("site")) {
                                    (layout::hidden("package", &site.id.package))
                                    (layout::hidden("site", &site.id.site))
                                    button.btn type="submit" aria-pressed=(if site.exposure.onion { "true" } else { "false" }) { (switch(site.exposure.onion)) }
                                }
                            }
                            @if let Some(url) = &site.url { p.mono { (url) } }
                        }
                    } },
                }
            }
        }
        section.card {
            h3 { "About onion addresses" }
            ul.bullets {
                li { span.dim aria-hidden="true" { "●" } span { "An onion address is reached through Tor only. It never shows your IP and needs no open port." } }
                li { span.dim aria-hidden="true" { "●" } span { "The node's address serves only its restricted RPC, which wallets use. Never the full RPC." } }
                li { span.dim aria-hidden="true" { "●" } span { "Admin paths of sites are blocked on onion addresses. Keys are kept in backups." } }
            }
        }
    }
}

/// `GET /system/onion`.
pub async fn onion(ctx: Ctx) -> Response {
    ctx.with(dest::WEB, |work| {
        let (flow, reply) = onion_loaded(work);
        if reply.login_required {
            return Out::Reply(reply);
        }
        let page = Page { trail: web_trail(&[("Onion addresses", dest::ONION)]), ..Page::new(Tab::Web, "Onion addresses", onion_page(&flow)) };
        Out::Page(page)
    })
    .await
}

/// `POST /system/onion/{node|site|node-settings}`: the node's switch, one site's switch (named
/// by its id in the form), or the settings of the app that provides the node. What the flow
/// refuses is said as a notice.
pub async fn onion_act(ctx: Ctx, axum::extract::Path(what): axum::extract::Path<String>, Form(form): Form<Posted>) -> Response {
    ctx.with(dest::ONION, move |work| {
        let (mut flow, _) = onion_loaded(work);
        let effects = match what.as_str() {
            "node" => flow.toggle_node(),
            "site" => {
                let id = SiteId { package: posted(&form, "package").unwrap_or_default().to_owned(), site: posted(&form, "site").unwrap_or_default().to_owned() };
                flow.toggle_site(&id)
            }
            "node-settings" => flow.node_settings(),
            _ => Vec::new(),
        };
        let mut reply = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
        reply.notices.extend(flow.error.take().map(Notice::error));
        Out::Reply(reply)
    })
    .await
}
