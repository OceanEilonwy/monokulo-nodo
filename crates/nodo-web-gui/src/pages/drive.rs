//! The data drive: what is wrong with it, and looking again, checking and repairing it
//! (`flows::drive`). Its card also takes the top of Home while the drive is missing.

use axum::extract::Path;
use axum::response::Response;
use maud::{html, Markup};
use nodo_ui_core::flows::{drive, Step};
use nodo_ui_core::model::{CheckState, DriveFailure};
use nodo_ui_core::screen::Tab;

use crate::ctx::{Ctx, JobSpec, Out};
use crate::dest;
use crate::layout::{self, Page, Sub};
use crate::runner::{no_flow, Reply};

fn action(name: &str) -> String {
    format!("{}/{name}", dest::DRIVE)
}

/// Why the drive cannot be used, and what can be done about it.
pub fn card(failure: &DriveFailure) -> Markup {
    html! {
        section.card.bad {
            h2 { span.err aria-hidden="true" { "▲ " } "The data drive isn't connected" }
            p { "Your apps keep their data on the SSD, so they're stopped until it's back. Nothing on it has been touched." }
            p.dim { (failure.message) }
            @if !failure.checks.is_empty() {
                ul.bullets {
                    @for check in &failure.checks {
                        li {
                            @match check.state {
                                CheckState::Passed => span.ok aria-hidden="true" { "✓" },
                                CheckState::Failed => span.err aria-hidden="true" { "✕" },
                                CheckState::Unknown => span.dim aria-hidden="true" { "○" },
                            }
                            span { (check.text) @if !check.note.is_empty() { span.note { "   " (check.note) } } }
                        }
                    }
                }
            }
            @if !failure.waiting.is_empty() { p.dim { "Stopped: " (failure.waiting) } }
            div.actions {
                (layout::post(&action("retry"), "Check again", "btn primary"))
                (layout::post(&action("check"), "Check the drive without changing it", "btn"))
                (layout::ask(&action("repair"), "Repair the filesystem…", "btn danger"))
                (layout::post(&action("diagnostics"), "Show what the Nodo sees", "btn quiet"))
            }
            p.dim.small { "The check is read-only. Repairing can lose data; it explains first and asks." }
        }
    }
}

/// `GET /system/drive`.
pub async fn show(ctx: Ctx) -> Response {
    ctx.with(dest::SYSTEM, |work| {
        let body = html! {
            @match &work.snapshot.overview.drive {
                Some(failure) => (card(failure)),
                None => section.card {
                    h2 { span.ok aria-hidden="true" { "✓ " } "The data drive is connected" }
                    p.dim { "Apps keep their data on it. If it goes missing, this page says why and what can be done." }
                    div.actions { (layout::post(&action("diagnostics"), "Show what the Nodo sees", "btn")) }
                },
            }
        };
        Out::Page(Page::new(Tab::System, "Data drive", body).trail(&[("System", dest::SYSTEM), ("Data drive", dest::DRIVE)]).head(layout::heading("Data drive", None)).sub(Sub::Drive))
    })
    .await
}

/// `POST /system/drive/{action}`. A check reads the whole filesystem, so it runs as a job;
/// looking again and the diagnostics answer at once. Each ends in a report to read
/// (`Dest::Document`) or a notice why not.
pub async fn act(ctx: Ctx, Path(name): Path<String>) -> Response {
    let back = dest::DRIVE;
    match name.as_str() {
        "retry" => ctx.with(back, |work| Out::Reply(work.run(Step::run(drive::retry()), &mut no_flow))).await,
        "diagnostics" => ctx.with(back, |work| Out::Reply(work.run(Step::run(drive::diagnostics()), &mut no_flow))).await,
        "check" => {
            let spec = JobSpec::new("Checking the data drive", back).note(&["Nothing on the drive is changed."]);
            ctx.job(spec, |work| work.run(Step::run(drive::check()), &mut no_flow)).await
        }
        // The repair itself runs once confirmed (`pages::flow::confirm`), as a job.
        "repair" => ctx.respond(Reply::ask(drive::ask_repair()), back),
        _ => ctx.page(super::missing(Tab::System, "No such action", "The data drive has no such action.", ("Data drive", back))),
    }
}
