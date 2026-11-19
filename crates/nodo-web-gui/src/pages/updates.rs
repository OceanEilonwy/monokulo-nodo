//! Updates (`flows::updates`): which are ticked, checking for more, applying a batch and what it
//! came to, the automatic-update choices, and undoing an update.
//!
//! The Updates page keeps nothing: the names ticked arrive with every post of its form. What
//! must not come from a browser is kept in the session instead: the batch that waits for the
//! device passphrase carries the approvals the helper issued for the reviews (`Stash::batch`),
//! so the passphrase page posts the passphrase and nothing else.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Form;
use maud::{html, Markup};
use nodo_api::UpdateEventKind;
use nodo_ui_core::flows::install::ReviewPurpose;
use nodo_ui_core::flows::updates::{self, AutoUpdate, Rollbacks};
use nodo_ui_core::flows::{Dest, Notice, Step};
use nodo_ui_core::model::{Snapshot, UpdateNote, UpdateReport, SYSTEM_BASE};
use nodo_ui_core::screen::Tab;
use serde::Deserialize;

use super::{document_page, install, PassForm};
use crate::glue::typed_secret;
use nodo_ui_core::words::update_note;
use nodo_ui_core::words::day;
use crate::ctx::{go_to, Ctx, JobSpec, Out, Work};
use crate::dest;
use crate::layout::{self, Page};
use crate::runner::{follow_plain, no_flow, Reply};

const TRAIL: [(&str, &str); 2] = [("System", dest::SYSTEM), ("Updates", dest::UPDATES)];

fn trail(last: &[(&str, &str)]) -> Vec<(String, String)> {
    TRAIL.iter().chain(last).map(|(name, url)| ((*name).to_owned(), (*url).to_owned())).collect()
}

/// What a form posts: fields that can repeat (the ticks), and the button pressed as `do`.
type Posted = Vec<(String, String)>;

fn posted<'a>(form: &'a Posted, name: &str) -> Option<&'a str> {
    form.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

fn all<'a>(form: &'a Posted, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    form.iter().filter(move |(key, _)| key == name).map(|(_, value)| value.as_str())
}

/// The job that applies a batch. It ends on the page of what the batch came to.
fn apply_spec() -> JobSpec {
    JobSpec::new("Updating", dest::UPDATES)
        .note(&[
            "Each app keeps its settings and can be undone. One that does not start with its new version is put back.",
            "You can leave this page; it carries on.",
        ])
        .elsewhere(|reply| match &reply.go {
            Some(Dest::UpdateResults(_)) => vec![Notice::info("The updates have finished. System › Updates › results says how they went.")],
            _ => Vec::new(),
        })
}

fn apply_job(step: Step) -> Out {
    Out::Job(apply_spec(), Box::new(move |work| work.run(step, &mut no_flow)))
}

/// Follows a step of the updates flow: at once when it only loads a review or says something,
/// as a job when it applies updates.
fn follow(work: &mut Work<'_>, step: Step) -> Out {
    if step.is_long() {
        apply_job(step)
    } else {
        Out::Reply(work.run(step, &mut no_flow))
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdatesQuery {
    /// The one app whose update the owner came for, from its page.
    only: Option<String>,
}

fn page(snapshot: &Snapshot, ticked: &[String]) -> Page {
    let rows = snapshot.update_rows();
    let nodo = rows.iter().find(|item| item.name == SYSTEM_BASE);
    let apps: Vec<_> = rows.iter().filter(|item| item.name != SYSTEM_BASE).collect();
    let automatic = match snapshot.auto_update.enabled_count() {
        0 => "automatic updates are off".to_owned(),
        n => format!("automatic for {n}"),
    };
    let body = html! {
        div.head {
            h1 { "Updates" }
            div.actions {
                // Its own form: checking needs none of the ticks to be valid.
                button.btn type="submit" form="updates" name="do" value="check" { "Check now" }
            }
        }
        form.stack #updates method="post" action=(dest::UPDATES) {
            @if let Some(update) = nodo {
                section.card.attn {
                    h2 { "The Nodo" span.dim { "   " (update.from) "  →  " (update.to) } }
                    div.head {
                        p.dim {
                            @match &update.note {
                                UpdateNote::RestartNeeded => { "Needs a restart. Apps stop safely first." },
                                UpdateNote::Withdrawal(withdrawn) => { "Replaces " (withdrawn) ", which its publisher withdrew." },
                                _ => { "No restart needed." },
                            }
                            @if update.held { " Held back." }
                        }
                        button.btn.primary type="submit" name="do" value="system" {
                            @if update.note == UpdateNote::RestartNeeded { "Update and restart" } @else { "Update the Nodo" }
                        }
                    }
                }
            }
            section.card {
                h2 { "Apps" }
                @if apps.is_empty() { p.dim { "Every app is up to date." } }
                @for item in &apps {
                    @let (note, wants_eye) = update_note(item);
                    div.setting {
                        label.check {
                            input type="checkbox" name="tick" value=(item.name) checked[ticked.contains(&item.name)];
                            strong { (item.name) }
                            span.dim { (item.from) "  →  " (item.to) }
                        }
                        @if !note.is_empty() { p class=(if wants_eye { "warn" } else { "dim" }) { (note) } }
                        div.actions {
                            a href=(format!("{}/changes/{}", dest::UPDATES, dest::seg(&item.name))) { "What changes" }
                            @if item.held {
                                button.btn.link type="submit" name="do" value=(format!("release:{}", item.name)) { "Release it" }
                            } @else {
                                button.btn.link type="submit" name="do" value=(format!("hold:{}", item.name)) { "Hold it back" }
                            }
                        }
                    }
                }
                @if !apps.is_empty() {
                    div.head {
                        p.dim { "Each app keeps its settings and can be undone." }
                        button class=(if nodo.is_none() { "btn primary" } else { "btn" }) type="submit" name="do" value="apps" { "Update the ticked apps" }
                    }
                }
            }
        }
        section.card.flush {
            div.rows {
                (layout::row(dest::AUTO_UPDATE, "Automatic updates", &automatic, html! {}))
                (layout::row(dest::UNDO, "Undo an update", "Put an app back to the version before", html! {}))
            }
        }
        @if !snapshot.overview.updates_checked.is_empty() { p.dim.small { (snapshot.overview.updates_checked) } }
    };
    Page { trail: trail(&[]), ..Page::new(Tab::System, "Updates", body) }
}

/// `GET /system/updates`. The ticks are the flow's for a page that opens, or the ones the last
/// action on the page left.
pub async fn show(ctx: Ctx, Query(query): Query<UpdatesQuery>) -> Response {
    ctx.with(dest::SYSTEM, move |work| {
        let kept = work.session.inner().stash.ticks.take();
        let ticked = kept.unwrap_or_else(|| updates::ticked_at_open(&work.snapshot, query.only.as_deref()));
        Out::Page(page(&work.snapshot, &ticked))
    })
    .await
}

/// `POST /system/updates`: every button of the page, with the names ticked.
pub async fn act(ctx: Ctx, Form(form): Form<Posted>) -> Response {
    ctx.with(dest::UPDATES, move |work| {
        // The ticks as posted, through the flow: ticking a held update releases it.
        let mut checked: Vec<String> = Vec::new();
        let mut release = Vec::new();
        for name in all(&form, "tick") {
            if !checked.iter().any(|ticked| ticked == name) {
                release.extend(updates::tick(&mut checked, &work.snapshot, name));
            }
        }
        let released = work.run(Step::run(release), &mut no_flow);
        if released.login_required {
            return Out::Reply(released);
        }
        let action = posted(&form, "do").unwrap_or_default();
        let (verb, name) = action.split_once(':').unwrap_or((action, ""));
        let out = match verb {
            "apps" => {
                let step = updates::update_apps(&work.snapshot, &checked);
                follow(work, step)
            }
            "system" => {
                let step = updates::update_system(&work.snapshot);
                follow(work, step)
            }
            "check" => {
                let spec = JobSpec::new("Checking for updates", dest::UPDATES).note(&["The sources and the Nodo's own snapshot are asked what is new."]);
                return Out::Job(
                    spec,
                    Box::new(move |work| {
                        let shown: Vec<String> = work.snapshot.update_rows().iter().map(|item| item.name.clone()).collect();
                        let reply = work.run(Step::run(updates::check_now()), &mut no_flow);
                        // What the check found is ticked as the page ticks it when it opens.
                        updates::carry_ticks(&mut checked, &shown, &work.snapshot);
                        work.session.inner().stash.ticks = Some(checked);
                        reply
                    }),
                );
            }
            "hold" | "release" => {
                let effects = updates::set_held(&mut checked, name, verb == "hold");
                Out::Reply(work.run(Step::run(effects), &mut no_flow))
            }
            _ => Out::Go(dest::UPDATES.to_owned()),
        };
        // The ticks are kept for the page this returns to. A batch that goes on leaves the page:
        // when the owner is back, it ticks what is safe as a page that opens does.
        work.session.inner().stash.ticks = if matches!(out, Out::Job(..)) { None } else { Some(checked) };
        out
    })
    .await
}

/// `GET /system/updates/changes/{name}`: what an update changes, to read.
pub async fn changes(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::UPDATES, move |work| {
        let reply = work.run(Step::run(updates::changelog(&name)), &mut no_flow);
        match reply.go {
            Some(Dest::Document(document)) => {
                let mut page = document_page(Some(Tab::System), &document, dest::UPDATES);
                page.trail = trail(&[(&name, "")]);
                Out::Page(page)
            }
            _ => Out::Reply(reply),
        }
    })
    .await
}

fn kept_review(ctx: &Ctx) -> Option<nodo_ui_core::flows::install::Review> {
    ctx.session.inner().stash.review.clone().filter(|review| matches!(review.purpose, ReviewPurpose::Update { .. }))
}

/// `GET /system/updates/review`: an update that asks for something new, to approve or not. The
/// page is the install's review, with an update's words.
pub async fn review(ctx: Ctx) -> Response {
    match kept_review(&ctx) {
        Some(review) => ctx.page(install::review_page(&review)),
        None => go_to(ctx.htmx, dest::UPDATES),
    }
}

/// `POST /system/updates/review`: the owner approves what this update newly asks for. The
/// approval is the one the helper issued with the review, kept in the session; the next update
/// that needs a review follows, or the batch goes on.
pub async fn approve(ctx: Ctx) -> Response {
    let Some(review) = kept_review(&ctx) else {
        return go_to(ctx.htmx, dest::UPDATES);
    };
    ctx.with(dest::UPDATES, move |work| {
        work.session.inner().stash.review = None;
        let step = review.approve(None, &work.snapshot);
        follow(work, step)
    })
    .await
}

/// `GET /system/updates/passphrase`: some of the batch grow encrypted storage, which needs the
/// device passphrase.
pub async fn passphrase(ctx: Ctx) -> Response {
    let Some(batch) = ctx.session.inner().stash.batch.clone() else {
        return go_to(ctx.htmx, dest::UPDATES);
    };
    let body = html! {
        h1 { "Some apps need more locked storage" }
        section.card.attn {
            ul.bullets { @for line in &batch.growing { li { span.warn aria-hidden="true" { "◆" } span { (line) } } } }
            // Only the passphrase is posted. The batch and its approvals stay on the Nodo.
            form.stack method="post" action=(dest::UPDATE_PASSPHRASE) {
                (layout::passphrase("update-passphrase", "Device passphrase", None, true))
                p.dim.small { "Growing locked storage needs the device passphrase. It's used for this and not kept." }
                div.actions.end {
                    a.btn href=(dest::UPDATES) { "Cancel" }
                    button.btn.primary type="submit" { "Update" }
                }
            }
        }
    };
    ctx.page(Page { trail: trail(&[("Passphrase", dest::UPDATE_PASSPHRASE)]), ..Page::new(Tab::System, "Updates", body) })
}

/// `POST /system/updates/passphrase`: applies the batch kept in the session with the passphrase
/// typed. Nothing else a browser sends is read.
pub async fn submit_passphrase(ctx: Ctx, Form(form): Form<PassForm>) -> Response {
    let Some(batch) = ctx.session.inner().stash.batch.clone() else {
        return go_to(ctx.htmx, dest::UPDATES);
    };
    let step = batch.submit(&mut typed_secret(form.passphrase.expose()));
    if !step.is_long() {
        return ctx.respond(follow_plain(step), dest::UPDATE_PASSPHRASE);
    }
    ctx.job(apply_spec(), move |work| work.run(step, &mut no_flow)).await
}

fn results_page(report: &UpdateReport) -> Page {
    let action = |what: &str| format!("{}/{what}", dest::UPDATE_RESULTS);
    let applied: Vec<_> = report.results.iter().filter(|result| result.applied).collect();
    let body = html! {
        @match &report.failure {
            Some(failure) => {
                h1 { span.err aria-hidden="true" { "▲ " } (failure.name) "'s update didn't start, so " (failure.kept_version) " was put back" }
                section.card.bad {
                    p {
                        (failure.restored) " " (failure.name)
                        @if failure.running { " is running again as before." } @else { " isn't running; its log says why." }
                    }
                    @if !failure.log_line.is_empty() { p.dim { (failure.name) " said: \"" (failure.log_line) "\"" } }
                    @if !failure.verdict.is_empty() { p.dim { (failure.verdict) } }
                    @for result in &applied {
                        p { span.ok aria-hidden="true" { "✓ " } @if result.name == SYSTEM_BASE { "The Nodo" } @else { (result.name) } " updated to " (result.version) }
                    }
                    div.actions.end {
                        a.btn.quiet href=(dest::log(&failure.name)) { "Show log ›" }
                        (layout::post(&action("report"), "Save a report for the publisher", "btn quiet"))
                        (layout::post(&action("hold"), "Hold it back", "btn"))
                        (layout::post(&action("retry"), "Try again", "btn"))
                        (layout::post(&action("leave"), "OK", "btn primary"))
                    }
                }
            },
            None => {
                h1 { @if applied.len() == 1 { "1 update is installed" } @else { (applied.len()) " updates are installed" } }
                section.card {
                    ul.bullets {
                        @for result in &report.results {
                            li {
                                @if result.applied { span.ok aria-hidden="true" { "✓" } } @else { span.err aria-hidden="true" { "✕" } }
                                span class=[(!result.applied).then_some("err")] {
                                    (result.name) " " (result.version)
                                    @if !result.note.is_empty() { span.note { "   " (result.note) } }
                                }
                            }
                        }
                    }
                    div.actions.end { (layout::post(&action("leave"), "Done", "btn primary")) }
                }
            },
        }
    };
    Page { trail: trail(&[("Results", dest::UPDATE_RESULTS)]), ..Page::new(Tab::System, "Updates", body) }
}

/// `GET /system/updates/results`: what the last batch came to.
pub async fn results(ctx: Ctx) -> Response {
    let report = ctx.session.inner().stash.results.clone();
    match report {
        Some(report) => ctx.page(results_page(&report)),
        None => go_to(ctx.htmx, dest::UPDATES),
    }
}

/// `POST /system/updates/results/{leave|hold|retry|report}`: away from the results; hold the
/// update that failed; try it again; or a report of it for whoever published it.
pub async fn after_results(ctx: Ctx, Path(what): Path<String>) -> Response {
    let Some(report) = ctx.session.inner().stash.results.clone() else {
        return go_to(ctx.htmx, dest::UPDATES);
    };
    if what == "report" {
        return ctx.with(dest::UPDATE_RESULTS, move |work| Out::Reply(work.run(Step::run(updates::report_failed(&report)), &mut no_flow))).await;
    }
    ctx.with(dest::UPDATES, move |work| {
        let step = match what.as_str() {
            "hold" => updates::hold_failed(&report),
            "retry" => updates::retry_failed(&report, &work.snapshot),
            _ => updates::leave_results(),
        };
        {
            let mut inner = work.session.inner();
            inner.stash.results = None;
            inner.stash.ticks = None;
        }
        follow(work, step)
    })
    .await
}

// Automatic updates

/// The draft the form carries: the stored choices with what was posted put in through the
/// flow. The bool is whether the window typed was taken.
fn auto_update_draft(snapshot: &Snapshot, form: &Posted) -> (AutoUpdate, bool) {
    let mut flow = AutoUpdate::new(snapshot.auto_update.clone());
    if let Some(frequency) = posted(form, "frequency") {
        flow.set_frequency(frequency);
    }
    let names: Vec<String> = flow.draft.packages.iter().map(|entry| entry.name.clone()).collect();
    for name in names {
        flow.set_enabled(&name, all(form, "on").any(|on| on == name));
    }
    let taken = posted(form, "window").is_none_or(|window| flow.set_window(window));
    (flow, taken)
}

fn auto_update_page(flow: &AutoUpdate, window: &str) -> Page {
    let draft = &flow.draft;
    let next = if flow.saved.next_run.is_empty() { String::new() } else { format!(" {}.", flow.saved.next_run) };
    let problem = flow.error.as_deref();
    let body = html! {
        div.head {
            h1 { "Automatic updates" }
            div.actions { a.btn.quiet href=(format!("{}/history", dest::AUTO_UPDATE)) { "What they did" } }
        }
        p.lead { "Only versions that ask for nothing new. Anything else waits for you." (next) }
        form.stack method="post" action=(dest::AUTO_UPDATE) {
            section.card {
                div.field {
                    label for="frequency" { "How often" }
                    select id="frequency" name="frequency" {
                        @for each in &draft.frequencies { option value=(each) selected[*each == draft.frequency] { (each) } }
                    }
                }
                div.field {
                    label for="window" { "Between" }
                    input type="text" id="window" name="window" value=(window) autocomplete="off" inputmode="numeric"
                        aria-invalid=[problem.map(|_| "true")] aria-describedby=[problem.map(|_| "window-problem")];
                    @if let Some(problem) = problem { p.problem #window-problem { "✕ " (problem) } }
                    p.hint { "The Nodo's own time, as 03:00 to 05:00." }
                }
            }
            section.card {
                h2 { "Update these apps by themselves" }
                @for entry in &draft.packages {
                    div.setting {
                        label.check {
                            input type="checkbox" name="on" value=(entry.name) checked[entry.enabled];
                            strong { @if entry.name == SYSTEM_BASE { "Update the Nodo by itself" } @else { (entry.name) } }
                        }
                        // Shown whether it is on or not: a browser has no row in focus.
                        @if entry.shows_warning(true) { p.warn { "▲ Runs new code without your review" } }
                        @if !entry.note.is_empty() { p.dim { (entry.note) } }
                        @if entry.name == SYSTEM_BASE { p.dim { "Updates that need a restart always wait for you, whatever this says." } }
                    }
                }
            }
            div.actions.end {
                // With htmx the question about unsaved choices comes over the form, which keeps
                // them if the answer is to stay; without, staying shows the stored choices.
                button.btn type="submit" name="do" value="leave" hx-post=(dest::AUTO_UPDATE) hx-target="#dialog" hx-swap="innerHTML" { "Cancel" }
                button.btn.primary type="submit" name="do" value="save" { "Save" }
            }
        }
    };
    let status = if problem.is_some() { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page { trail: trail(&[("Automatic", dest::AUTO_UPDATE)]), ..Page::new(Tab::System, "Automatic updates", body) }.status(status)
}

/// `GET /system/auto-update`.
pub async fn auto_update(ctx: Ctx) -> Response {
    ctx.with(dest::UPDATES, |work| {
        let flow = AutoUpdate::new(work.snapshot.auto_update.clone());
        Out::Page(auto_update_page(&flow, &flow.window()))
    })
    .await
}

/// `POST /system/auto-update`: Save, or Cancel. The form carries the whole draft, so nothing is
/// kept between requests. A window that cannot be used is said beside its box.
pub async fn save_auto_update(ctx: Ctx, Form(form): Form<Posted>) -> Response {
    ctx.with(dest::UPDATES, move |work| {
        let (mut flow, taken) = auto_update_draft(&work.snapshot, &form);
        if posted(&form, "do") == Some("leave") {
            // Leaving asks when the choices were changed; the question's Cancel returns here.
            return Out::Reply(follow_plain(flow.leave()).cancel_to(dest::AUTO_UPDATE));
        }
        if !taken {
            return Out::Page(auto_update_page(&flow, posted(&form, "window").unwrap_or_default()));
        }
        let effects = flow.save();
        Out::Reply(work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome)))
    })
    .await
}

/// `GET /system/auto-update/history`: what the automatic updates did, to read.
pub async fn auto_update_history(ctx: Ctx) -> Response {
    ctx.with(dest::AUTO_UPDATE, |work| {
        let reply = work.run(Step::run(AutoUpdate::history()), &mut no_flow);
        match reply.go {
            Some(Dest::Document(document)) => {
                let mut page = document_page(Some(Tab::System), &document, dest::AUTO_UPDATE);
                page.trail = trail(&[("Automatic", dest::AUTO_UPDATE), ("What they did", "")]);
                Out::Page(page)
            }
            _ => Out::Reply(reply),
        }
    })
    .await
}

// Undoing an update

/// How a rollback went, in a line, and whether it is good news.
fn rolled_back_line(result: &nodo_api::UpdateResult) -> (String, &'static str) {
    use nodo_ui_core::words::{rollback_result, Tone};
    match rollback_result(result) {
        (Tone::Good, words) => (format!("✓ {words}"), "ok"),
        (Tone::Working, words) => (format!("▲ {words}"), "warn"),
        (Tone::Bad, words) => (format!("✕ {words}"), "err"),
    }
}

/// The Undo page's flow with the list read.
fn rollbacks(work: &mut Work<'_>) -> (Rollbacks, Reply) {
    let mut flow = Rollbacks::new();
    let reply = work.run(Step::run(Rollbacks::load()), &mut |outcome, _| flow.apply(outcome));
    (flow, reply)
}

fn undo_page(flow: &Rollbacks, last: Option<&nodo_api::UpdateResult>) -> Markup {
    let events: Vec<_> = flow
        .view
        .iter()
        .flat_map(|view| view.events.iter())
        .filter_map(|event| match &event.kind {
            UpdateEventKind::RolledBack { reason } => Some((event, "warn", "▲", format!("stayed on {}: {reason}", event.from))),
            UpdateEventKind::RollbackFailed { reason, rollback_error } => Some((event, "err", "✕", format!("did not come back: {reason}; {rollback_error}"))),
            _ => None,
        })
        .collect();
    html! {
        h1 { "Undo an update" }
        @if let Some(result) = last {
            @let (line, class) = rolled_back_line(result);
            p class=(format!("notice {}", if class == "ok" { "ok" } else { "error" })) { (line) }
        }
        @if let Some(error) = &flow.error { p.notice.error { "✕ " (error) } }
        section.card.flush {
            @if flow.options().is_empty() {
                p.pad.dim { "Nothing to roll back. A package keeps its previous version after an update." }
            }
            div.rows {
                @for option in flow.options() {
                    div.row {
                        span.name { (option.package) }
                        span.what.mono { (option.current) " → " (option.previous) }
                        span.end {
                            @match &option.blocked {
                                Some(reason) => span.warn { "▲ " (reason) },
                                None => (layout::ask(&format!("{}/{}", dest::UNDO, dest::seg(&option.package)), "Roll back…", "btn")),
                            }
                        }
                        span.go {}
                    }
                }
            }
        }
        section.card {
            h3 { "Updates that were rolled back" }
            @if events.is_empty() { p.dim { "No update has failed." } }
            ul.bullets {
                @for (event, class, sign, text) in &events {
                    li {
                        span class=(class) aria-hidden="true" { (sign) }
                        span { (event.package) " " (event.from) " → " (event.to) " " span.note { (text) " · " (day(event.at)) } }
                    }
                }
            }
        }
        section.card {
            h3 { "System" }
            @match flow.view.as_ref().and_then(|view| view.boot_fallback.as_ref()) {
                Some(reason) => p { span.warn aria-hidden="true" { "▲ " } "Back on the previous system: " (reason) },
                None => p.dim { "The system runs the version it was updated to. No fallback happened." },
            }
        }
    }
}

/// `GET /system/undo`: the versions that can be put back, and the rollbacks that happened.
pub async fn undo(ctx: Ctx) -> Response {
    ctx.with(dest::UPDATES, |work| {
        let (flow, reply) = rollbacks(work);
        if reply.login_required {
            return Out::Reply(reply);
        }
        let last = work.session.inner().stash.rolled_back.take();
        let body = undo_page(&flow, last.as_ref());
        Out::Page(Page { trail: trail(&[("Undo an update", dest::UNDO)]), ..Page::new(Tab::System, "Undo an update", body) })
    })
    .await
}

/// `POST /system/undo/{package}`: rolling an app back asks first. One that has no version kept,
/// or whose rollback is blocked now, is refused with the reason. Confirmed, the rollback runs
/// as a job (`pages::flow`).
pub async fn roll_back(ctx: Ctx, Path(package): Path<String>) -> Response {
    ctx.with(dest::UNDO, move |work| {
        let (mut flow, _) = rollbacks(work);
        let mut reply = follow_plain(flow.roll_back(&package));
        reply.notices.extend(flow.error.take().map(Notice::error));
        Out::Reply(reply)
    })
    .await
}
