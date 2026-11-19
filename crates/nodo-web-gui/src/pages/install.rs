//! Get apps and install one (`flows::install`): the search, an app's details, the gate, the
//! review and its approval, the install as a job, and the pages for how the gate or the install
//! ended.
//!
//! The pages of one app on offer sit under `/apps/get/{feed}/{package}`. What the helper gave
//! for the next page (the review to approve, the rejection, the install's report) is kept in the
//! session's `Stash`; a page opened without it sends the owner back to the app's details, where
//! the way starts.

use axum::extract::{Path, Query};
use axum::response::Response;
use axum::Form;
use maud::{html, Markup};
use nodo_ui_core::flows::install::{self, Review, ReviewPurpose, Search};
use nodo_ui_core::flows::{Dest, Notice, Step};
use nodo_ui_core::model::{
    AskLevel, CapabilityData, InstallOutcome, InstallSnapshot, PackageDetail, PackageId, RejectionData, Snapshot, StepState,
};
use nodo_ui_core::screen::Tab;
use nodo_ui_core::Outcome;
use serde::Deserialize;

use super::{document_page, PassForm};
use crate::glue::typed_secret;
use nodo_ui_core::words::status_word;
use nodo_ui_core::words::empty_catalog_line;
use crate::ctx::{Ctx, JobSpec, Out, Work};
use crate::dest;
use crate::layout::{self, Page, Sub};
use crate::runner::{follow_plain, no_flow, Reply};

#[derive(Debug, Default, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    source: String,
}

fn search_url(query: &str, source: Option<&str>) -> String {
    let mut parts = Vec::new();
    if !query.is_empty() {
        parts.push(format!("q={}", dest::seg(query)));
    }
    if let Some(source) = source {
        parts.push(format!("source={}", dest::seg(source)));
    }
    if parts.is_empty() { dest::GET_APPS.to_owned() } else { format!("{}?{}", dest::GET_APPS, parts.join("&")) }
}

/// The list under the search box. With htmx it alone is replaced as the owner types.
fn results(snapshot: &Snapshot, search: &Search, query: &str) -> Markup {
    let found = search.results(snapshot, query);
    let scope = search.feed_filter.as_deref().map_or_else(String::new, |feed| format!(" from {feed}"));
    html! {
        section.stack #results {
            p.dim role="status" {
                (found.len()) " of " (snapshot.catalog.len()) " apps" (scope) " match. Open one to see what it does before installing."
            }
            section.card.flush {
                @if found.is_empty() { p.pad.dim { (empty_catalog_line(snapshot, query)) } }
                div.rows {
                    @for pkg in &found {
                        @let what = match pkg.bundle {
                            Some(members) => format!("Installs {} apps · {}", members + 1, pkg.tagline),
                            None => pkg.tagline.clone(),
                        };
                        (layout::row(&dest::offer(&pkg.id), &pkg.id.name, &what, html! { (status_word(pkg.status)) }))
                    }
                }
            }
        }
    }
}

/// `GET /apps/get`: what the sources offer, narrowed by the words typed and by one source.
pub async fn search(ctx: Ctx, Query(query): Query<SearchQuery>) -> Response {
    let fragment = ctx.htmx;
    ctx.with(dest::APPS, move |work| {
        let snapshot = &work.snapshot;
        let search = Search { feed_filter: Some(query.source.clone()).filter(|source| !source.is_empty()) };
        let list = results(snapshot, &search, &query.q);
        if fragment {
            return Out::Fragment(list);
        }
        let source = search.feed_filter.as_deref();
        let body = html! {
            form.searchbar method="get" action=(dest::GET_APPS) role="search"
                hx-get=(dest::GET_APPS) hx-trigger="input delay:300ms, submit" hx-target="#results" hx-swap="outerHTML" {
                input type="search" name="q" value=(query.q) placeholder="Search apps" aria-label="Search apps" autocomplete="off";
                @if let Some(source) = source { (layout::hidden("source", source)) }
                button.btn type="submit" { "Search" }
            }
            @if snapshot.feeds.len() > 1 {
                nav.chips aria-label="Sources" {
                    a.chip href=(search_url(&query.q, None)) aria-current=[source.is_none().then_some("true")] { "Every source" }
                    @for feed in &snapshot.feeds {
                        a.chip href=(search_url(&query.q, Some(&feed.name))) aria-current=[(source == Some(feed.name.as_str())).then_some("true")] {
                            (feed.name)
                        }
                    }
                }
            }
            (list)
        };
        let head = layout::heading("Get apps", Some("What your sources offer. Open one to see what it does before installing."));
        Out::Page(Page::new(Tab::Apps, "Get apps", body).trail(&[("Apps", dest::APPS), ("Get apps", dest::GET_APPS)]).head(head).sub(Sub::GetApps))
    })
    .await
}

fn trail_of(id: &PackageId, last: Option<&str>) -> Vec<(String, String)> {
    let mut trail = vec![
        ("Apps".to_owned(), dest::APPS.to_owned()),
        ("Get apps".to_owned(), dest::GET_APPS.to_owned()),
        (id.name.clone(), dest::offer(id)),
    ];
    if let Some(last) = last {
        trail.push((last.to_owned(), String::new()));
    }
    trail
}

fn offered(id: &PackageId, title: impl Into<String>, last: Option<&str>, body: Markup) -> Page {
    Page { trail: trail_of(id, last), ..Page::new(Tab::Apps, title, body) }
}

/// Reads an app's details through the flow, as the details page and its documents need them.
fn load_detail(work: &mut Work<'_>, id: &PackageId) -> Result<PackageDetail, Reply> {
    let reply = work.run(Step::run(install::detail(id)), &mut no_flow);
    match reply.go {
        Some(Dest::Detail(detail)) => Ok(detail),
        _ => Err(reply),
    }
}

fn detail_page(detail: &PackageDetail, has_site: bool) -> Page {
    let id = &detail.id;
    let here = dest::offer(id);
    let version = if detail.version.is_empty() { String::new() } else { format!("Version {}", detail.version) };
    let facts: Vec<String> =
        [version, format!("from {}", id.feed), detail.size.clone()].into_iter().filter(|part| !part.is_empty()).collect();
    let mut asks: Vec<String> = detail.needs.iter().map(|need| format!("Use {}", need.name)).collect();
    asks.extend(detail.asks.iter().map(|(label, value)| format!("{label}: {value}")));
    if detail.encrypted_storage {
        asks.push("Keep its data locked; it needs the device passphrase after a restart".to_owned());
    }
    let body = html! {
        div.head {
            h1 { (id.name) }
            div.actions {
                // An installed app is opened, not installed again: the flow says which.
                @if let Step::Go(dest) = install::open_installed(detail) {
                    @if has_site { a.btn href=(dest::hosting(&id.name)) { "Hosting" } }
                    a.btn.primary href=(dest::url(&dest, &here)) { "Open its page" }
                } @else {
                    (layout::post(&format!("{here}/install"), "Install", "btn primary"))
                }
            }
        }
        section.card {
            p.dim { (facts.join(" · ")) }
            p { (detail.about) }
            // What the description points at, as the button that does it.
            @if detail.about_hint == Some(nodo_ui_core::flows::Hint::Install) && !detail.installed {
                (layout::post(&format!("{here}/install"), "Download and check it", "btn"))
            }
            @if !asks.is_empty() {
                h3 { "It will" }
                ul.bullets { @for ask in &asks { li { span.dim aria-hidden="true" { "·" } span { (ask) } } } }
            }
            @match (detail.signature_verified, detail.installed) {
                (true, _) => p.ok { "✓ Its source's signature checks out" },
                (false, false) => p.dim { "Its source's signature is checked when it downloads" },
                (false, true) => p.err { "✕ Its signature could not be checked" },
            }
            @if !detail.license.is_empty() { p.dim.small { "Licence: " (detail.license) } }
            div.actions {
                a.btn.quiet href=(format!("{here}/doc/changelog")) { "What changed" }
                a.btn.quiet href=(format!("{here}/doc/versions")) { "Versions" }
            }
        }
    };
    offered(id, id.name.clone(), None, body)
}

/// `GET /apps/get/{feed}/{package}`.
pub async fn detail(ctx: Ctx, Path((feed, package)): Path<(String, String)>) -> Response {
    ctx.with(dest::GET_APPS, move |work| match load_detail(work, &PackageId::new(&feed, &package)) {
        Ok(detail) => {
            // An installed app with a web page: who can open it is one step away.
            let has_site = detail.installed && nodo_ui_core::flows::services::AppPage::new(&detail.id.name).has_site(&work.snapshot);
            Out::Page(detail_page(&detail, has_site))
        }
        Err(reply) => Out::Reply(reply),
    })
    .await
}

/// `POST /apps/get/{feed}/{package}/install`: the gate every install goes through. It answers
/// with the review to approve, a rejection, a missing capability or a bundle, each a page.
pub async fn gate(ctx: Ctx, Path((feed, package)): Path<(String, String)>) -> Response {
    let id = PackageId::new(&feed, &package);
    ctx.with(dest::offer(&id), move |work| match load_detail(work, &id) {
        Ok(detail) => Out::Reply(work.run(Step::run(install::install(&detail)), &mut no_flow)),
        Err(reply) => Out::Reply(reply),
    })
    .await
}

/// The review kept for this app, if the gate was passed for it and nothing else since.
fn kept_review(ctx: &Ctx, id: &PackageId) -> Option<Review> {
    ctx.session.inner().stash.review.clone().filter(|review| review.data.id == *id && review.purpose == ReviewPurpose::Install)
}

/// The review of what an app asks for: before an install, or for an update that asks for
/// something new. The same page, with the words and the ways out of each.
pub fn review_page(review: &Review) -> Page {
    let data = &review.data;
    let id = &data.id;
    let is_update = review.purpose != ReviewPurpose::Install;
    let (here, cancel, verb) = if is_update {
        (dest::UPDATE_REVIEW.to_owned(), dest::UPDATES.to_owned(), "Update")
    } else {
        (format!("{}/review", dest::offer(id)), dest::offer(id), "Install")
    };
    let body = html! {
        h1 { @if is_update { "Update " (id.name) " to " (data.version) "?" } @else { "Install " (id.name) "?" } }
        section.card {
            @if data.third_party {
                p { span.warn aria-hidden="true" { "▲ " } "It comes from " (id.feed) ", a source you added. Nobody has reviewed its code for you." }
            }
            h3 { @if is_update { "This version newly asks to" } @else { "It will be able to" } }
            ul.bullets {
                @for ask in &data.asks {
                    li {
                        @match ask.level {
                            AskLevel::Normal => span.ok aria-hidden="true" { "✓" },
                            AskLevel::Caution => span.warn aria-hidden="true" { "▲" },
                            AskLevel::Encrypted => span.warn aria-hidden="true" { "◆" },
                        }
                        span { (ask.text) @if !ask.note.is_empty() { span.note { "   " (ask.note) } } }
                    }
                }
            }
            @for line in &data.guarantees { p.dim { (line) } }
            @if !is_update { p { a href=(format!("{}/doc/files", dest::offer(id))) { "The files it installs" } } }
            form.stack method="post" action=(here) {
                @if review.needs_passphrase() {
                    (layout::passphrase("review-passphrase", "Device passphrase", None, false))
                    p.dim.small { "Its locked storage is made now and opens with your device passphrase." }
                }
                div.actions.end {
                    a.btn href=(cancel) { "Cancel" }
                    button.btn.primary type="submit" { (verb) }
                }
            }
        }
    };
    if is_update {
        Page::new(Tab::System, format!("Update {}", id.name), body).trail(&[
            ("System", dest::SYSTEM),
            ("Updates", dest::UPDATES),
            (id.name.as_str(), dest::UPDATE_REVIEW),
        ])
    } else {
        offered(id, format!("Install {}", id.name), Some("Install"), body)
    }
}

/// `GET /apps/get/{feed}/{package}/review`.
pub async fn review(ctx: Ctx, Path((feed, package)): Path<(String, String)>) -> Response {
    let id = PackageId::new(&feed, &package);
    match kept_review(&ctx, &id) {
        Some(review) => ctx.page(review_page(&review)),
        None => crate::ctx::go_to(ctx.htmx, &dest::offer(&id)),
    }
}

/// `POST /apps/get/{feed}/{package}/review`: the owner approves what the review showed.
///
/// A refusal (the passphrase is needed and was not typed) is answered at once. Otherwise the
/// install starts as a job: with the helper it is one request that answers when it is over.
pub async fn approve(ctx: Ctx, Path((feed, package)): Path<(String, String)>, Form(form): Form<PassForm>) -> Response {
    let id = PackageId::new(&feed, &package);
    let here = format!("{}/review", dest::offer(&id));
    let Some(review) = kept_review(&ctx, &id) else {
        return crate::ctx::go_to(ctx.htmx, &dest::offer(&id));
    };
    let mut typed = typed_secret(form.passphrase.expose());
    // An install's approval reads nothing from the device's state; only an update's does.
    let step = review.approve(review.needs_passphrase().then_some(&mut typed), &Snapshot::default());
    if step.effects().is_empty() {
        return ctx.respond(follow_plain(step), &here);
    }
    let name = id.name.clone();
    let spec = JobSpec::new(format!("Installing {name}"), dest::offer(&id))
        .note(&[
            "Downloading, checking and installing. This can take a few minutes.",
            "You can leave this page; it carries on and says when it is done. A failed step removes what this install added.",
        ])
        // On another page only the end is said, in the flow's words for that.
        .elsewhere(|reply| match &reply.go {
            Some(Dest::Installing(report)) => {
                follow_plain(install::progress(report, &nodo_ui_core::model::Capabilities::default(), false)).notices
            }
            _ => Vec::new(),
        });
    ctx.job(spec, move |work| {
        // The job's page is the install's page, so every report is shown there.
        work.run(step, &mut on_install_page)
    })
    .await
}

fn kept_install(ctx: &Ctx, id: &PackageId) -> Option<InstallSnapshot> {
    ctx.session.inner().stash.install.clone().filter(|report| report.id == *id)
}

fn install_title(report: &InstallSnapshot) -> String {
    let name = &report.id.name;
    match &report.outcome {
        InstallOutcome::InProgress => format!("Installing {name}"),
        InstallOutcome::Succeeded => format!("{name} is installed"),
        InstallOutcome::Failed(_) => format!("{name} wasn't installed"),
        InstallOutcome::Cancelled => "Install cancelled".to_owned(),
    }
}

/// The install's report: its steps and how it stands. A backend that reports progress gives
/// steps while the install runs; htmx then asks for this again every second.
fn install_report(report: &InstallSnapshot) -> Markup {
    let id = &report.id;
    let here = format!("{}/installing", dest::offer(id));
    let stepping = report.outcome == InstallOutcome::InProgress && !report.steps.is_empty();
    html! {
        section.card #install hx-get=[stepping.then(|| format!("{here}/status"))] hx-trigger=[stepping.then_some("every 1s")] hx-swap="outerHTML" {
            h2 { (install_title(report)) }
            @if !report.steps.is_empty() {
                ul.bullets {
                    @for step in &report.steps {
                        li {
                            @match step.state {
                                StepState::Done => span.ok aria-hidden="true" { "✓" },
                                StepState::Working => span.warn aria-hidden="true" { "›" },
                                StepState::Pending => span.dim aria-hidden="true" { "○" },
                                StepState::Failed => span.err aria-hidden="true" { "✕" },
                            }
                            span { (step.name) @if !step.note.is_empty() { span.note { "   " (step.note) } } }
                        }
                    }
                }
            }
            @for line in &report.details { p.dim { (line) } }
            @match &report.outcome {
                InstallOutcome::InProgress => {
                    p { "Downloading, checking and installing. This can take a few minutes." }
                    p.dim { "You can leave this page; it carries on. A failed step removes what this install added." }
                    div.actions.end {
                        @if stepping && report.can_cancel {
                            // Leaving a running install that can be stopped cancels it.
                            (layout::post(&format!("{here}/leave"), "Cancel the install", "btn danger"))
                        } @else if !stepping {
                            a.btn href=(dest::WORKING) { "See how far it is" }
                        }
                    }
                },
                InstallOutcome::Succeeded => {
                    p.ok { "✓ It is installed and starting." }
                    div.actions.end {
                        a.btn.quiet href=(format!("{}/doc/log", dest::offer(id))) { "Full log" }
                        (layout::post(&format!("{here}/leave"), "Done", "btn"))
                        (layout::post(&format!("{here}/next"), "Set it up", "btn primary"))
                    }
                },
                InstallOutcome::Failed(reason) => {
                    p.err { "✕ " (reason) }
                    div.actions.end {
                        a.btn.quiet href=(format!("{}/doc/log", dest::offer(id))) { "Full log" }
                        (layout::post(&format!("{here}/leave"), "Back", "btn primary"))
                    }
                },
                InstallOutcome::Cancelled => {
                    div.actions.end { (layout::post(&format!("{here}/leave"), "Back", "btn primary")) }
                },
            }
        }
    }
}

fn installing_page(report: &InstallSnapshot) -> Page {
    let stepping = report.outcome == InstallOutcome::InProgress && !report.steps.is_empty();
    let body = html! {
        h1 { (report.id.name) }
        (install_report(report))
    };
    let page = offered(&report.id, install_title(report), Some("Install"), body);
    if stepping { page.waiting() } else { page }
}

/// What a report about the install means on the install's own page, in the flow's words.
fn on_install_page(outcome: &Outcome, snapshot: &mut Snapshot) -> Step {
    match outcome {
        Outcome::InstallProgress(Some(report)) => install::progress(report, &snapshot.capabilities, true),
        _ => Step::Stay,
    }
}

/// `GET /apps/get/{feed}/{package}/installing/status`: asks how the install is going, where the
/// backend can say, and answers with the report alone. A polling route.
pub async fn installing_poll(ctx: Ctx, Path((feed, package)): Path<(String, String)>) -> Response {
    let id = PackageId::new(&feed, &package);
    let Some(report) = kept_install(&ctx, &id) else {
        return crate::ctx::go_to(ctx.htmx, &dest::offer(&id));
    };
    ctx.with(dest::offer(&id), move |work| {
        let effects = install::poll(&report, &work.snapshot.capabilities);
        let reply = work.run(Step::run(effects), &mut on_install_page);
        match &reply.go {
            Some(Dest::Installing(now)) => {
                work.session.inner().stash.install = Some(now.clone());
                Out::Fragment(install_report(now))
            }
            // Cancelled meanwhile: the flow leaves the page.
            Some(_) => Out::Reply(reply),
            None => Out::Fragment(install_report(&report)),
        }
    })
    .await
}

/// `GET /apps/get/{feed}/{package}/installing`: how the install ended.
pub async fn installing(ctx: Ctx, Path((feed, package)): Path<(String, String)>) -> Response {
    let id = PackageId::new(&feed, &package);
    match kept_install(&ctx, &id) {
        Some(report) => ctx.page(installing_page(&report)),
        None => crate::ctx::go_to(ctx.htmx, &dest::offer(&id)),
    }
}

/// `POST /apps/get/{feed}/{package}/installing/{leave|next}`: away from the install's page, or
/// on to setting the new app up.
pub async fn after_install(ctx: Ctx, Path((feed, package, what)): Path<(String, String, String)>) -> Response {
    let id = PackageId::new(&feed, &package);
    let Some(report) = kept_install(&ctx, &id) else {
        return crate::ctx::go_to(ctx.htmx, &dest::offer(&id));
    };
    // An installed app's own page is where "back" leads; a failed install returns to the offer.
    let back = if report.outcome == InstallOutcome::Succeeded { dest::app(&id.name) } else { dest::offer(&id) };
    let step = if what == "next" {
        // `carry_on` opens the new app's settings, or says it has none.
        install::carry_on(&report)
    } else {
        install::leave(&report)
    };
    ctx.with(back, move |work| Out::Reply(work.run(step, &mut on_install_page))).await
}

fn kept_rejection(ctx: &Ctx, id: &PackageId) -> Option<RejectionData> {
    ctx.session.inner().stash.rejection.clone().filter(|data| data.id == *id)
}

/// `GET /apps/get/{feed}/{package}/rejected`: why the package cannot be installed at all. There
/// is deliberately nothing on this page that installs anything.
pub async fn rejected(ctx: Ctx, Path((feed, package)): Path<(String, String)>) -> Response {
    let id = PackageId::new(&feed, &package);
    let Some(data) = kept_rejection(&ctx, &id) else {
        return crate::ctx::go_to(ctx.htmx, &dest::offer(&id));
    };
    let here = format!("{}/rejected", dest::offer(&id));
    let broken = data.violations.iter().filter(|violation| !violation.passed);
    let body = html! {
        h1 { span.err aria-hidden="true" { "▲ " } (id.name) " can't be installed" }
        section.card.bad {
            p {
                "Its source signed it, but it breaks the Nodo's rules, and there's no override. Nothing was installed. Tell the publisher of "
                (id.feed) " if you think this is a mistake."
            }
            ul.bullets {
                @for violation in broken {
                    li { span.err aria-hidden="true" { "✕" } span { (violation.rule) span.note { " — " (violation.evidence) } } }
                }
            }
            div.actions.end {
                @if let Step::Go(dest) = install::rejected_source(&data) {
                    a.btn.quiet href=(dest::url(&dest, &here)) { "Its source" }
                }
                (layout::post(&format!("{here}/report"), "Save a report", "btn"))
                a.btn.primary href=(dest::GET_APPS) { "OK" }
            }
        }
    };
    ctx.page(offered(&id, format!("{} can't be installed", id.name), None, body))
}

/// `POST /apps/get/{feed}/{package}/rejected/report`: a report for whoever published it.
pub async fn report_rejection(ctx: Ctx, Path((feed, package)): Path<(String, String)>) -> Response {
    let id = PackageId::new(&feed, &package);
    let Some(data) = kept_rejection(&ctx, &id) else {
        return crate::ctx::go_to(ctx.htmx, &dest::offer(&id));
    };
    ctx.with(format!("{}/rejected", dest::offer(&id)), move |work| {
        Out::Reply(work.run(Step::run(install::report_rejection(&data)), &mut no_flow))
    })
    .await
}

fn kept_capability(ctx: &Ctx, id: &PackageId) -> Option<CapabilityData> {
    ctx.session.inner().stash.capability.clone().filter(|data| data.id == *id)
}

/// `GET /apps/get/{feed}/{package}/capability`: what the package needs that the node installed
/// now lacks, and the node that has it.
pub async fn capability(ctx: Ctx, Path((feed, package)): Path<(String, String)>) -> Response {
    let id = PackageId::new(&feed, &package);
    let Some(data) = kept_capability(&ctx, &id) else {
        return crate::ctx::go_to(ctx.htmx, &dest::offer(&id));
    };
    let here = format!("{}/capability", dest::offer(&id));
    let name = &id.name;
    let body = html! {
        h1 { span.warn aria-hidden="true" { "▲ " } (name) " needs a different Monero node first" }
        section.card.attn {
            p {
                "It needs something " (data.current_node) " doesn't provide. " (data.alternative_node)
                " does; it syncs the chain, then " (name) " can be installed."
            }
            p.dim { (data.message) }
            @if !data.matrix.is_empty() {
                dl.facts {
                    @for (what, current, alternative) in &data.matrix {
                        dt { (what) }
                        dd {
                            (data.current_node) " " @if *current { span.ok { "✓" } } @else { span.err { "✕" } }
                            "   " (data.alternative_node) " " @if *alternative { span.ok { "✓" } } @else { span.err { "✕" } }
                        }
                    }
                }
            }
            @if !data.switch_note.is_empty() { p.dim { (data.switch_note) } }
            div.actions.end {
                a.btn href=(dest::offer(&id)) { "Cancel" }
                (layout::post(&format!("{here}/compatible"), "Is there a version that fits?", "btn quiet"))
                (layout::post(&format!("{here}/install"), &format!("Install {}", data.alternative_node), "btn primary"))
            }
        }
    };
    ctx.page(offered(&id, format!("{name} needs a different node"), None, body))
}

/// `POST /apps/get/{feed}/{package}/capability/{install|compatible}`: the node that has what is
/// needed goes through the gate like any other app; the other button asks about older versions.
pub async fn capability_act(ctx: Ctx, Path((feed, package, what)): Path<(String, String, String)>) -> Response {
    let id = PackageId::new(&feed, &package);
    let Some(data) = kept_capability(&ctx, &id) else {
        return crate::ctx::go_to(ctx.htmx, &dest::offer(&id));
    };
    let here = format!("{}/capability", dest::offer(&id));
    if what == "install" {
        ctx.with(here, move |work| Out::Reply(work.run(Step::run(install::install_alternative(&data)), &mut no_flow))).await
    } else {
        ctx.respond(follow_plain(install::compatible_version(&data)), &here)
    }
}

/// `GET /apps/get/{feed}/{package}/doc/{kind}`: a text about the app, to read. `changelog` and
/// `versions` are about any app on offer; `files` belongs to the review being shown and `log`
/// to the install that ran.
pub async fn document(ctx: Ctx, Path((feed, package, kind)): Path<(String, String, String)>) -> Response {
    let id = PackageId::new(&feed, &package);
    let review = kept_review(&ctx, &id);
    let report = kept_install(&ctx, &id);
    let back = match kind.as_str() {
        "files" => format!("{}/review", dest::offer(&id)),
        "log" => format!("{}/installing", dest::offer(&id)),
        _ => dest::offer(&id),
    };
    ctx.with(back.clone(), move |work| {
        let effects = match (kind.as_str(), review, report) {
            ("files", Some(review), _) => review.files(),
            ("log", _, Some(report)) => install::log(&report),
            ("changelog" | "versions", _, _) => match load_detail(work, &id) {
                Ok(detail) if kind == "changelog" => install::changelog(&detail),
                Ok(detail) => install::versions(&detail),
                Err(reply) => return Out::Reply(reply),
            },
            _ => return Out::Reply(Reply::notice(Notice::info("That text is not available any more."))),
        };
        let reply = work.run(Step::run(effects), &mut no_flow);
        match reply.go {
            Some(Dest::Document(document)) => {
                let mut page = document_page(Some(Tab::Apps), &document, &back);
                page.trail = trail_of(&id, Some(&document.title));
                Out::Page(page)
            }
            _ => Out::Reply(reply),
        }
    })
    .await
}
