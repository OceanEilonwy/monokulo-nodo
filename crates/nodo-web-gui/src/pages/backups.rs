//! Backups (`flows::backup::Backups`): the device's backups and a USB drive's, making one, and
//! restoring one onto this device.
//!
//! The flow is kept in the session from the lists to the end of a restore, since its steps
//! build on each other: the backup chosen, what it holds, and, while a restore waits for the
//! typed word, the backup's passphrase. That passphrase never comes back to the browser. A
//! backup is named by its name, which the flow checks against its own lists; where it is (the
//! device or the drive) is the flow's `target`, never a field of a form.
//!
//! Making, opening and restoring a backup read or write all of it, so each runs as a job.
//! The first-boot restore (`flows::backup::Restore`) has no page: the web GUI cannot be on
//! before the device is set up.

use axum::http::StatusCode;
use axum::response::Response;
use axum::Form;
use maud::{html, Markup};
use nodo_api::BackupInfo;
use nodo_ui_core::flows::backup::{
    restore_warning, restores, usb_facts, Backups, Field, Stage, CHECKED_FIRST, HOLDS, HOLDS_NOTES, PASSPHRASE_NOTES, RESTORE_WORD, USB_NOTE,
};
use nodo_ui_core::flows::Step;
use nodo_ui_core::screen::Tab;
use nodo_ui_core::words::{day, drive_name, size};
use nodo_ui_core::Effect;

use crate::glue::typed_secret;
use crate::ctx::{Ctx, JobSpec, Out, Work};
use crate::dest;
use crate::layout::{self, Page};

type Posted = Vec<(String, String)>;

fn posted<'a>(form: &'a Posted, name: &str) -> &'a str {
    form.iter().find(|(key, _)| key == name).map_or("", |(_, value)| value.as_str())
}

/// The page's flow for this request: the one kept, or a new one with the lists read.
fn open_flow(work: &mut Work<'_>) -> Backups {
    let kept = work.session.inner().stash.backups.clone();
    match kept {
        Some(flow) => flow,
        None => {
            let mut flow = Backups::of(&work.snapshot);
            let _read = work.run(Step::run(Backups::load()), &mut |outcome, _| flow.apply(outcome));
            flow
        }
    }
}

fn keep(work: &Work<'_>, flow: &Backups) {
    work.session.inner().stash.backups = Some(flow.clone());
}

/// A passphrase box of these forms. Never filled in: what was typed is not sent back.
fn secret_box(name: &str, label: &str, new: bool, marked: bool, hint: &str) -> Markup {
    html! {
        div.field {
            label for=(name) { (label) }
            input type="password" id=(name) name=(name) autocomplete=(if new { "new-password" } else { "current-password" }) aria-invalid=[marked.then_some("true")] autofocus[marked];
            @if !hint.is_empty() { p.hint { (hint) } }
        }
    }
}

fn backup_rows(list: &[BackupInfo], on_usb: bool) -> Markup {
    html! {
        div.rows {
            @for backup in list {
                div.row {
                    span.name.mono { (backup.name) }
                    span.what { (size(backup.size_bytes)) " · " (day(backup.created)) }
                    span.end {
                        form method="post" action=(dest::BACKUPS) {
                            (layout::hidden("name", &backup.name))
                            (layout::hidden("usb", if on_usb { "1" } else { "0" }))
                            button.btn type="submit" name="do" value="open" { "Restore…" }
                        }
                    }
                    span.go {}
                }
            }
        }
    }
}

fn page(flow: &Backups, refused: Option<Field>) -> Page {
    let here = dest::BACKUPS;
    let button = |action: &str, label: &str, class: &str| html! { button class=(class) type="submit" name="do" value=(action) { (label) } };
    let problem = html! { @if let Some(error) = &flow.error { p.notice.error { "✕ " (error) } } };
    let where_to = if flow.target.is_some() { "the USB drive" } else { "this device" };
    let body = match &flow.stage {
        Stage::List => html! {
            div.head {
                h1 { "Backups" }
                form.actions method="post" action=(here) { (button("reload", "Look again", "btn quiet")) }
            }
            (problem)
            section.card.flush {
                h2 { "Backups on this device" }
                @if flow.list.is_empty() { p.pad.dim { "No backups yet." } }
                (backup_rows(&flow.list, false))
            }
            form.actions.end method="post" action=(here) { (button("create", "Make a backup now", "btn primary")) }
            section.card.flush {
                h2 { "USB drive" }
                @match &flow.usb {
                    None => p.pad.dim { "No USB drive is plugged in. Plug one in and look again." },
                    Some(usb) => {
                        p.pad {
                            (drive_name(&usb.drive))
                            @if let Some(problem) = &usb.drive.problem { span.err { " · " (problem) } }
                            @if let Some(list) = &usb.list { span.dim { " · " (usb_facts(&usb.drive, list)) } }
                        }
                        @if usb.list.is_some() && flow.usb_list().is_empty() { p.pad.dim { "No backups on this drive yet." } }
                        (backup_rows(flow.usb_list(), true))
                    },
                }
            }
            @if flow.usb_device().is_some() {
                form.actions.end method="post" action=(here) { (button("create-usb", "Make a backup on the USB drive", "btn")) }
                p.dim.small { (USB_NOTE) }
            }
            section.card {
                h2 { "A backup holds" }
                ul.bullets { @for line in HOLDS { li { span.ok aria-hidden="true" { "✓" } span { (line) } } } }
                @for line in HOLDS_NOTES { p.dim { (line) } }
            }
        },
        Stage::Create => html! {
            h1 { "New backup on " (where_to) }
            (problem)
            section.card {
                form.stack method="post" action=(here) {
                    @if flow.asks_device_passphrase() {
                        (secret_box("device", "Device passphrase", false, refused == Some(Field::DevicePassphrase), "So the backup includes the device key and the backed-up storage opens."))
                    }
                    (secret_box("first", "Backup passphrase", true, refused == Some(Field::BackupPassphrase), ""))
                    (secret_box("second", "Backup passphrase, again", true, false, ""))
                    ul.bullets {
                        @for line in PASSPHRASE_NOTES { li { span.dim aria-hidden="true" { "·" } span { (line) } } }
                        @if flow.target.is_some() { li { span.dim aria-hidden="true" { "·" } span { (USB_NOTE) } } }
                    }
                    div.actions.end { (button("cancel", "Cancel", "btn")) (button("make", "Make the backup", "btn primary")) }
                }
            }
        },
        Stage::Unlock { name } => html! {
            h1 { "Open this backup" }
            (problem)
            section.card {
                p.mono { (name) @if flow.target.is_some() { span.dim { ", on the USB drive" } } }
                form.stack method="post" action=(here) {
                    (secret_box("passphrase", "Backup passphrase", false, flow.error.is_some(), ""))
                    p.dim { "Nothing changes yet. " (CHECKED_FIRST) }
                    div.actions.end { (button("cancel", "Cancel", "btn")) (button("inspect", "Open", "btn primary")) }
                }
            }
        },
        // The backup's passphrase is in the flow, on the Nodo. Nothing of it is on this page.
        Stage::Confirm { name, contents, .. } => html! {
            h1 { "Restore from this backup?" }
            (problem)
            @let (services, warning) = restore_warning(contents, false);
            section.card.bad {
                p.mono { (name) span.ok { "  ✓ verified" } }
                p { strong { (warning) "." } " The backup holds " (contents.entries) " entries." }
                h3 { "It will replace" }
                ul.bullets { @for line in restores(contents, false) { li { span.ok aria-hidden="true" { "✓" } span { (line) } } } }
                p.warn { "▲ " (services) }
                form.stack method="post" action=(here) {
                    div.field {
                        label for="typed" { "Type " (RESTORE_WORD) " to confirm" }
                        input type="text" id="typed" name="typed" autocomplete="off" autocapitalize="none" spellcheck="false" aria-invalid=[(refused == Some(Field::Word)).then_some("true")];
                    }
                    @if flow.asks_new_device_passphrase() {
                        (secret_box("device", "New device passphrase", true, refused == Some(Field::NewDevicePassphrase), "The backup brings its own device key; this passphrase opens it from now on."))
                    }
                    div.actions.end { (button("cancel", "Cancel", "btn")) (button("restore", "Restore", "btn danger")) }
                }
            }
        },
        Stage::Restored(report) => html! {
            h1 { span.ok aria-hidden="true" { "✓ " } "Restored" }
            section.card {
                dl.facts {
                    dt { "Restored" } dd { (report.restored.join(", ")) }
                    @if !report.skipped.is_empty() { dt { "Skipped" } dd { (report.skipped.join(", ")) } }
                    dt { "Stopped for it" } dd { @if report.stopped.is_empty() { "No services had to stop." } @else { (report.stopped.join(", ")) } }
                    @if !report.restarted.is_empty() { dt { "Started again" } dd { (report.restarted.join(", ")) } }
                }
                form.actions.end method="post" action=(here) { (button("finish", "Done", "btn primary")) }
            }
        },
    };
    let status = if flow.error.is_some() && flow.stage != Stage::List { StatusCode::UNPROCESSABLE_ENTITY } else { StatusCode::OK };
    Page::new(Tab::System, "Backups", body).trail(&[("System", dest::SYSTEM), ("Backups", dest::BACKUPS)]).status(status)
}

/// `GET /system/backups`. On the lists, both are read again.
pub async fn show(ctx: Ctx) -> Response {
    ctx.with(dest::SYSTEM, |work| {
        let mut flow = open_flow(work);
        if flow.stage == Stage::List {
            let effects = flow.reload();
            let _read = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
        }
        let shown = page(&flow, None);
        // A refusal is said once.
        flow.error = None;
        keep(work, &flow);
        Out::Page(shown)
    })
    .await
}

/// A step that reads or writes a whole backup, as a job. The flow goes with it and comes back
/// to the session with what the helper answered.
fn job(what: &str, note: &str, mut flow: Backups, effects: Vec<Effect>) -> Out {
    let spec = JobSpec::new(what, dest::BACKUPS).note(&[note]);
    Out::Job(
        spec,
        Box::new(move |work| {
            let reply = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
            keep(work, &flow);
            reply
        }),
    )
}

/// `POST /system/backups`: every button of the page, named by `do`. The passphrases arrive
/// with their button and go straight into the flow's action.
pub async fn act(ctx: Ctx, Form(form): Form<Posted>) -> Response {
    ctx.with(dest::BACKUPS, move |work| {
        let mut flow = open_flow(work);
        let out = match posted(&form, "do") {
            "create" | "create-usb" => {
                flow.start_create(posted(&form, "do") == "create-usb");
                Out::Go(dest::BACKUPS.to_owned())
            }
            // Chosen by name, which the flow looks up in the list it read.
            "open" => {
                flow.open(posted(&form, "name"), posted(&form, "usb") == "1");
                Out::Go(dest::BACKUPS.to_owned())
            }
            "make" => {
                let (mut device, mut first, mut second) = (typed_secret(posted(&form, "device")), typed_secret(posted(&form, "first")), typed_secret(posted(&form, "second")));
                match flow.create(&mut device, &mut first, &mut second) {
                    Ok(effects) if effects.is_empty() => Out::Go(dest::BACKUPS.to_owned()),
                    Ok(effects) => return job("Making a backup", "Everything is read, encrypted and written. This can take a few minutes.", flow, effects),
                    Err(refused) => Out::Page(page(&flow, refused)),
                }
            }
            "inspect" => {
                let effects = flow.inspect(&typed_secret(posted(&form, "passphrase")));
                if effects.is_empty() {
                    Out::Go(dest::BACKUPS.to_owned())
                } else {
                    return job("Opening the backup", "The whole backup is checked before anything is shown. Nothing changes yet.", flow, effects);
                }
            }
            "restore" => {
                let mut device = typed_secret(posted(&form, "device"));
                match flow.restore(posted(&form, "typed"), &mut device) {
                    Ok(effects) if effects.is_empty() => Out::Go(dest::BACKUPS.to_owned()),
                    Ok(effects) => return job("Restoring from the backup", "Apps stop, their settings and storage are put back, and they start again.", flow, effects),
                    Err(refused) => Out::Page(page(&flow, refused)),
                }
            }
            "cancel" => {
                flow.cancel();
                Out::Go(dest::BACKUPS.to_owned())
            }
            "finish" => {
                let step = flow.finish();
                Out::Reply(work.run(step, &mut |outcome, _| flow.apply(outcome)))
            }
            _ => {
                let effects = flow.reload();
                Out::Reply(work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome)))
            }
        };
        if let Out::Page(_) = &out {
            flow.error = None;
        }
        keep(work, &flow);
        out
    })
    .await
}
