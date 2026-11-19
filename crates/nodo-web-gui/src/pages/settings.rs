//! An app's settings (`flows::settings`): the form and its draft, references to other apps'
//! settings, saving, and the page after a save the helper took back.
//!
//! The form is the one feature here with state between requests: the draft, the helper's
//! previews and the one step back. That is a `Settings` value kept in the session per app
//! (`Stash::settings`), made when the page is first opened and dropped when the form is saved,
//! discarded or left unchanged.
//!
//! The page is one HTML form. Every button posts all of it, and the handler first puts what was
//! typed into the draft through the flow's own actions (`sync`), then does what the button
//! says. So the page works without scripts; with htmx a changed field posts the same form to
//! `…/settings/field/{key}` and gets its own row back, with its error, preview and undo.
//!
//! There is no text cursor here, so a reference is not typed as `$(` among the text: the picker
//! puts it in (`Settings::place`, with `PickMode::Append` after the text), and each
//! reference of a field has its own buttons.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Form;
use maud::{html, Markup};
use nodo_ui_core::flows::settings::{self, PickMode, Picked, Settings};
use nodo_ui_core::flows::staged::{self, Stageable};
use nodo_ui_core::flows::{Dest, Notice, Step};
use nodo_ui_core::model::{FieldKind, FieldValue, Piece, Snapshot};
use nodo_ui_core::refs::kind_name;
use nodo_ui_core::screen::Tab;
use nodo_ui_core::secret::STORED_MASK;
use nodo_ui_core::{Effect, Outcome};
use serde::Deserialize;

use crate::glue::typed_secret;
use nodo_ui_core::words::{chip_label, picker_hint};
use crate::ctx::{go_to, Ctx, JobSpec, Out, Work};
use crate::dest;
use crate::layout::{self, Page};
use crate::runner::{follow_plain, Reply};
use crate::session::Picking;

/// What a form posts: every field, and the button pressed as `do`.
type Posted = Vec<(String, String)>;

fn posted<'a>(form: &'a Posted, name: &str) -> Option<&'a str> {
    form.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
}

fn field_url(package: &str, key: &str) -> String {
    format!("{}/field/{}", dest::settings(package), dest::seg(key))
}

fn reference_url(package: &str) -> String {
    format!("{}/reference", dest::settings(package))
}

/// The form's flow for this request: the draft kept in the session, or the form read afresh
/// with what it loads when it opens. `Err` when the app has nothing to set, with the remark.
fn open_flow(work: &mut Work<'_>, package: &str) -> Result<Settings, Reply> {
    let kept = work.session.inner().stash.settings.get(package).cloned();
    if let Some(flow) = kept {
        return Ok(flow);
    }
    let mut flow: Option<Settings> = None;
    let reply = work.run(Step::run(settings::open(package)), &mut |outcome, _| match (outcome, flow.as_mut()) {
        // The flows without state answer this one: they name the page and ask for what the
        // form loads first, whose results come back here.
        (Outcome::SettingsLoaded { form: Some(form), .. }, None) => {
            flow = Some(Settings::new(form.clone()));
            Step::Stay
        }
        (_, Some(flow)) => flow.apply(outcome),
        (_, None) => Step::Stay,
    });
    flow.ok_or(reply)
}

fn keep(work: &Work<'_>, flow: &Settings) {
    work.session.inner().stash.settings.insert(flow.form.package.clone(), flow.clone());
}

/// Puts what the form posted into the draft, field by field, through the flow's actions, and
/// returns what the helper must be asked about the new values.
fn sync(flow: &mut Settings, form: &Posted) -> Vec<Effect> {
    let mut effects = Vec::new();
    for index in 0..flow.form.fields.len() {
        let key = flow.form.fields[index].key.clone();
        let kind = flow.form.fields[index].kind.clone();
        match (&kind, flow.draft[index].clone()) {
            // A box that is not ticked posts nothing, so a marker says the switch was on the page.
            (FieldKind::Toggle, FieldValue::Bool(current)) => {
                if posted(form, &format!("t.{key}")).is_some() && posted(form, &format!("f.{key}")).is_some() != current {
                    flow.toggle(index);
                }
            }
            // An empty box keeps the stored secret; the flow sees to that.
            (FieldKind::Secret, _) => {
                if let Some(text) = posted(form, &format!("s.{key}")) {
                    flow.set_secret(index, &mut typed_secret(text));
                }
            }
            // Text around references: each piece of text has its own box, the references stay.
            (_, FieldValue::Pieces(pieces)) => {
                let text = |at: &str| posted(form, &format!("p.{key}.{at}")).map(str::to_owned);
                if form.iter().any(|(name, _)| name.starts_with(&format!("p.{key}."))) {
                    let mut rebuilt = vec![Piece::Text(text("pre").unwrap_or_default())];
                    for (at, piece) in pieces.into_iter().enumerate() {
                        rebuilt.push(match piece {
                            Piece::Text(old) => Piece::Text(text(&at.to_string()).unwrap_or(old)),
                            reference => reference,
                        });
                    }
                    rebuilt.push(Piece::Text(text("post").unwrap_or_default()));
                    effects.extend(flow.set_pieces(index, rebuilt));
                }
            }
            (_, _) => {
                if let Some(text) = posted(form, &format!("f.{key}")) {
                    effects.extend(flow.set_value(index, FieldValue::Text(text.to_owned())));
                }
            }
        }
    }
    effects
}

/// Whether text and references can sit together in a field of this kind.
fn mixes(kind: &FieldKind) -> bool {
    matches!(kind, FieldKind::Text | FieldKind::Url)
}

/// How many changes wait to be saved. With htmx it is replaced beside every field's row.
fn summary(flow: &Settings, out_of_band: bool) -> Markup {
    let changes = flow.changed_indices().len();
    html! {
        p #changes class=(if changes == 0 { "dim" } else { "warn" }) hx-swap-oob=[out_of_band.then_some("true")] {
            @match changes {
                0 => { "No changes yet" },
                1 => { "1 change, not saved yet" },
                n => { (n) " changes, not saved yet" },
            }
        }
    }
}

/// One reference of a field, with what it resolves to and what can be done with it.
fn chip(flow: &Settings, index: usize, key: &str, number: usize, app: &str, setting: &str) -> Markup {
    let resolved = flow.preview(index).and_then(|preview| preview.chips.get(number).cloned());
    html! {
        span.reference {
            span.chip.on { (chip_label(app, setting)) }
            @match resolved {
                Some(Err(why)) => span.err { "✕ " (why) },
                Some(Ok(Some(value))) => span.dim { "→ " (value) },
                Some(Ok(None)) => span.dim { "→ a secret, never shown" },
                None => span.dim { "→ checking…" },
            }
            button.btn.link type="submit" name="do" value=(format!("replace:{key}:{number}")) { "Change" }
            button.btn.link type="submit" name="do" value=(format!("type:{key}:{number}")) { "Type it instead" }
            button.btn.link type="submit" name="do" value=(format!("remove:{key}:{number}")) { "Remove" }
        }
    }
}

/// The control of one field, by its kind and by what its draft holds.
fn control(flow: &Settings, index: usize, package: &str) -> Markup {
    let field = &flow.form.fields[index];
    let key = field.key.as_str();
    let id = format!("f-{key}");
    let url = field_url(package, key);
    let target = format!("#field-{key}");
    let problem = flow.error_for(index);
    let invalid = problem.as_ref().map(|_| "true");
    match (&field.kind, &flow.draft[index]) {
        (FieldKind::Toggle, value) => html! {
            (layout::hidden(&format!("t.{key}"), "1"))
            label.switch {
                input type="checkbox" id=(id) name=(format!("f.{key}")) value="on" checked[*value == FieldValue::Bool(true)]
                    hx-post=(url) hx-trigger="change" hx-target=(target) hx-swap="outerHTML";
                span { @if *value == FieldValue::Bool(true) { "On" } @else { "Off" } }
            }
        },
        (FieldKind::Secret, value) => html! {
            // A stored secret is never sent to the browser: the box is empty, and leaving it
            // empty keeps what is stored.
            input type="password" id=(id) name=(format!("s.{key}")) autocomplete="new-password"
                placeholder=(match value {
                    FieldValue::SecretSet => STORED_MASK,
                    FieldValue::NewSecret(_) => "A new secret is typed; saving stores it",
                    _ => "Not set",
                })
                hx-post=(url) hx-trigger="change" hx-target=(target) hx-swap="outerHTML";
        },
        (FieldKind::Choice(options), FieldValue::Text(current)) => html! {
            select id=(id) name=(format!("f.{key}")) aria-invalid=[invalid] hx-post=(url) hx-trigger="change" hx-target=(target) hx-swap="outerHTML" {
                @if !options.contains(current) { option value=(current) selected { @if current.is_empty() { "Not set" } @else { (current) } } }
                @for each in options { option value=(each) selected[each == current] { (each) } }
            }
        },
        (kind, FieldValue::Pieces(pieces)) => html! {
            div.pieces {
                @if mixes(kind) && matches!(pieces.first(), Some(Piece::Ref { .. })) {
                    input type="text" name=(format!("p.{key}.pre")) value="" placeholder="text before" aria-label="Text before the reference" autocomplete="off";
                }
                @for (at, piece) in pieces.iter().enumerate() {
                    @match piece {
                        Piece::Text(text) => {
                            input type="text" name=(format!("p.{key}.{at}")) value=(text) aria-label="Text" autocomplete="off"
                                hx-post=(url) hx-trigger="change" hx-target=(target) hx-swap="outerHTML";
                        },
                        // References are counted from 0 among the field's references.
                        Piece::Ref { app, setting } => {
                            (chip(flow, index, key, pieces[..at].iter().filter(|piece| matches!(piece, Piece::Ref { .. })).count(), app, setting))
                        },
                    }
                }
                @if mixes(kind) && matches!(pieces.last(), Some(Piece::Ref { .. })) {
                    input type="text" name=(format!("p.{key}.post")) value="" placeholder="text after" aria-label="Text after the reference" autocomplete="off";
                }
            }
        },
        (kind, value) => {
            let text = match value {
                FieldValue::Text(text) => text.as_str(),
                _ => "",
            };
            let numeric = matches!(kind, FieldKind::Port | FieldKind::Number { .. });
            html! {
                input type="text" id=(id) name=(format!("f.{key}")) value=(text) autocomplete="off" autocapitalize="none" spellcheck="false"
                    inputmode=[numeric.then_some("numeric")] class=[matches!(kind, FieldKind::MoneroAddress).then_some("mono")]
                    aria-invalid=[invalid] aria-describedby=[invalid.map(|_| format!("{id}-problem"))]
                    hx-post=(url) hx-trigger="change" hx-target=(target) hx-swap="outerHTML";
            }
        }
    }
}

/// One field of the form: its label, its control, what is wrong with it, what it resolves to,
/// and the ways to take a change back.
fn field_row(flow: &Settings, index: usize, package: &str) -> Markup {
    let field = &flow.form.fields[index];
    let key = field.key.as_str();
    let changed = flow.draft[index] != field.value;
    let problem = flow.error_for(index);
    let preview = flow.preview(index);
    html! {
        div class=(if changed { "frow changed" } else { "frow" }) id=(format!("field-{key}")) {
            label for=(format!("f-{key}")) { (field.label) @if changed { " " span.tag.warn { "changed" } } }
            div.fld {
                (control(flow, index, package))
                @if let Some(problem) = &problem { p.problem id=(format!("f-{key}-problem")) { "✕ " (problem) } }
                @if let Some(preview) = preview {
                    @if let Some(problem) = &preview.problem { p.problem { "✕ " (problem) } }
                    @if let Some(resolved) = &preview.resolved { p.hint { "Now: " span.mono { (resolved) } } }
                }
                p.hint { (field.help) @if !field.requirement.is_empty() { " " (field.requirement) } }
                (field_actions(flow, index))
            }
        }
    }
}

/// The ways to change one field other than typing: references, and taking a change back.
fn field_actions(flow: &Settings, index: usize) -> Markup {
    let field = &flow.form.fields[index];
    let key = field.key.as_str();
    let changed = flow.draft[index] != field.value;
    let has_text = !matches!(&flow.draft[index], FieldValue::Text(text) if text.is_empty());
    let any = field.kind.takes_reference() || flow.undo_for(index).is_some() || changed;
    html! {
        @if any {
            div.actions {
                @if field.kind.takes_reference() {
                    @if mixes(&field.kind) && has_text {
                        button.btn.link type="submit" name="do" value=(format!("add:{key}")) { "+ reference after it" }
                        button.btn.link type="submit" name="do" value=(format!("pick:{key}")) { "Use a reference instead" }
                    } @else {
                        button.btn.link type="submit" name="do" value=(format!("pick:{key}")) { "+ reference" }
                    }
                }
                @if let Some(undo) = flow.undo_for(index) {
                    button.btn.link type="submit" name="do" value=(format!("undo:{key}")) { "↺ " (undo.label) }
                }
                @if changed {
                    button.btn.link type="submit" name="do" value=(format!("reset:{key}")) { "Put back what is saved" }
                }
            }
        }
    }
}

/// The count of changes inside the Save button. With htmx it is replaced beside every field's
/// row, as the summary is; the summary says it in words, so the count is not read out twice.
fn save_count(flow: &Settings, out_of_band: bool) -> Markup {
    count_span("save-count", flow, out_of_band)
}

/// The same count inside Save, which keeps the draft here.
fn keep_count(flow: &Settings, out_of_band: bool) -> Markup {
    count_span("keep-count", flow, out_of_band)
}

fn count_span(id: &str, flow: &Settings, out_of_band: bool) -> Markup {
    let changes = flow.changed_indices().len();
    html! {
        span id=(id) .bcnt aria-hidden="true" hidden[changes == 0] hx-swap-oob=[out_of_band.then_some("true")] { (changes) }
    }
}

fn page(snapshot: &Snapshot, flow: &Settings) -> Page {
    let package = flow.form.package.as_str();
    let here = dest::settings(package);
    let broken = flow.broken();
    let body = html! {
        div.head { h2 { "Settings" } (summary(flow, false)) }
        form.stack method="post" action=(here) {
            @if let Some(lines) = &flow.confirm {
                // Save stopped to say what the changed references allow.
                section.card.attn #confirm-save {
                    h2 { "Save " (package) "'s settings?" }
                    p { "These references change what " (package) " can reach:" }
                    ul.bullets { @for line in lines { li { span.dim aria-hidden="true" { "•" } span { (line) } } } }
                    div.actions.end {
                        button.btn type="submit" name="do" value="cancel-save" { "Cancel" }
                        button.btn.primary type="submit" name="do" value="confirm-save" { "Save" }
                    }
                }
            }
            section.sec {
                h2 { (package) }
                div.body {
                    @if flow.form.fields.is_empty() { p.dim.frow { "This app has nothing to set." } }
                    @for index in 0..flow.form.fields.len() { (field_row(flow, index, package)) }
                }
            }
            @if broken > 0 {
                p.err { "Fix " (broken) " broken " @if broken == 1 { "reference" } @else { "references" } " to save" }
            }
            div.applybar {
                // Without scripts this is how a typed value is checked before saving.
                noscript { button.btn.quiet type="submit" name="do" value="apply" { "Check what I typed" } }
                (layout::stage_buttons("Save & Apply", keep_count(flow, false), save_count(flow, false)))
            }
            (layout::stage_hint())
        }
        @if !flow.form.public_settings.is_empty() {
            p.dim.small { "Other apps can refer to: " (flow.form.public_settings.join(", ")) }
        }
    };
    let desc = format!("{package} restarts with the new settings. If it doesn't start, they're undone by themselves.");
    super::apps::app_frame(snapshot, package, "Settings", Some(&desc), format!("{package} settings"), body).trail(&[("Apps", dest::APPS), (package, &dest::app(package)), ("Settings", &here)])
}

/// `GET /apps/{name}/settings`.
pub async fn show(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::app(&name), move |work| match open_flow(work, &name) {
        Ok(flow) => {
            keep(work, &flow);
            Out::Page(page(&work.snapshot, &flow))
        }
        Err(reply) => Out::Reply(reply),
    })
    .await
}

/// A reply that returns to the form, where its notices are shown.
fn to_form(mut reply: Reply, flow: &Settings) -> Out {
    if reply.go.is_none() && reply.question.is_none() {
        reply.go = Some(Dest::Settings(flow.form.clone()));
    }
    Out::Reply(reply)
}

/// The save itself, as a job: the app restarts with the new settings, which can take a while.
/// Stored, the form is done with and the owner is back on the app's page. Refused or taken
/// back, the draft is kept for another try.
fn save_job(flow: Settings, effects: Vec<Effect>) -> Out {
    let package = flow.form.package.clone();
    let note = format!("{package} restarts with the new settings. If it doesn't start, they're undone by themselves.");
    let spec = JobSpec::new(format!("Saving {package}'s settings"), dest::app(&package)).note(&[&note]);
    Out::Job(
        spec,
        Box::new(move |work| {
            let mut flow = flow;
            let mut reply = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
            let mut inner = work.session.inner();
            if flow.changed_indices().is_empty() {
                inner.stash.settings.remove(&package);
            } else {
                if reply.go.is_none() {
                    reply.go = Some(Dest::Settings(flow.form.clone()));
                }
                inner.stash.settings.insert(package, flow);
            }
            reply
        }),
    )
}

/// Opens the picker for a field and goes to its page.
fn open_picker(work: &mut Work<'_>, flow: &mut Settings, index: usize, mode: PickMode) -> Out {
    match flow.pick(index, mode, false) {
        Err(why) => to_form(Reply::notice(Notice::error(why)), flow),
        Ok((mut picker, effects)) => {
            // What can be referenced, when it was not loaded yet, is told to both.
            let reply = work.run(Step::run(effects), &mut |outcome, _| {
                drop(picker.apply(outcome));
                flow.apply(outcome)
            });
            if reply.login_required {
                return Out::Reply(reply);
            }
            let package = flow.form.package.clone();
            work.session.inner().stash.picking = Some(Picking { picker });
            Out::Go(reference_url(&package))
        }
    }
}

/// `POST /apps/{name}/settings`: every button of the form. What was typed goes into the draft
/// first; then the button's action, named by `do`.
pub async fn act(ctx: Ctx, Path(name): Path<String>, Form(form): Form<Posted>) -> Response {
    ctx.with(dest::app(&name), move |work| {
        let mut flow = match open_flow(work, &name) {
            Ok(flow) => flow,
            Err(reply) => return Out::Reply(reply),
        };
        let effects = sync(&mut flow, &form);
        let synced = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
        if synced.login_required {
            return Out::Reply(synced);
        }
        let action = posted(&form, "do").unwrap_or("apply");
        let mut parts = action.split(':');
        let verb = parts.next().unwrap_or_default();
        let index = parts.next().and_then(|key| flow.index_of(key));
        let number = parts.next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
        let out = match (verb, index) {
            ("save", _) => match flow.save() {
                Step::Run(effects) => return save_job(flow, effects),
                // Stopped to say what the references allow: the form shows it.
                Step::Stay => to_form(Reply::default(), &flow),
                Step::Failed(notice) => {
                    // Refused: the form again, with what is wrong beside its field.
                    work.session.inner().notices.push(notice);
                    Out::Page(page(&work.snapshot, &flow).status(StatusCode::UNPROCESSABLE_ENTITY))
                }
                other => to_form(follow_plain(other), &flow),
            },
            ("confirm-save", _) => {
                let effects = flow.confirm_save();
                return save_job(flow, effects);
            }
            ("cancel-save", _) => {
                flow.cancel_save();
                to_form(Reply::default(), &flow)
            }
            // Save: the draft stays in the session (`Stash::settings`) and the owner goes back
            // to the app. A draft with nothing changed is not kept.
            ("keep", None) => {
                let key = flow.stage_key();
                let changes = flow.changes(&work.snapshot);
                let mut inner = work.session.inner();
                if changes == 0 {
                    inner.stash.settings.remove(&name);
                    inner.notices.push(staged::nothing_to_keep(&key));
                } else {
                    inner.stash.settings.insert(name.clone(), flow);
                    inner.notices.push(staged::saved_notice(&key, changes));
                }
                return Out::Go(dest::app(&name));
            }
            // Reset: the draft goes, and the form shows what is stored.
            ("reset", None) => {
                let mut inner = work.session.inner();
                inner.stash.settings.remove(&name);
                inner.notices.push(staged::reset_notice(&flow.stage_key()));
                return Out::Go(dest::settings(&name));
            }
            ("leave", _) => match flow.leave() {
                // Changes would be lost: ask. Confirmed, the draft goes (`pages::flow`).
                Step::Ask(question) => Out::Reply(Reply::ask(question).cancel_to(dest::settings(&name))),
                other => {
                    work.session.inner().stash.settings.remove(&name);
                    return Out::Reply(follow_plain(other));
                }
            },
            ("pick", Some(index)) => open_picker(work, &mut flow, index, PickMode::Insert),
            ("add", Some(index)) => open_picker(work, &mut flow, index, PickMode::Append),
            ("replace", Some(index)) => open_picker(work, &mut flow, index, PickMode::Replace(number)),
            (verb, Some(index)) => {
                let effects = match verb {
                    "reset" => flow.reset(index),
                    "undo" => flow.undo(index),
                    "type" => flow.type_instead(index, number),
                    "remove" => flow.remove_reference(index, number),
                    _ => Vec::new(),
                };
                to_form(work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome)), &flow)
            }
            _ => to_form(Reply::default(), &flow),
        };
        keep(work, &flow);
        out
    })
    .await
}

/// `POST /apps/{name}/settings/field/{key}`: a field changed, with htmx. The whole form
/// arrives as with any button; the answer is that field's row and the count of changes.
pub async fn change(ctx: Ctx, Path((name, key)): Path<(String, String)>, Form(form): Form<Posted>) -> Response {
    ctx.with(dest::settings(&name), move |work| {
        let mut flow = match open_flow(work, &name) {
            Ok(flow) => flow,
            Err(reply) => return Out::Reply(reply),
        };
        let effects = sync(&mut flow, &form);
        let reply = work.run(Step::run(effects), &mut |outcome, _| flow.apply(outcome));
        if reply.login_required {
            return Out::Reply(reply);
        }
        keep(work, &flow);
        match flow.index_of(&key) {
            Some(index) => Out::Fragment(html! { (field_row(&flow, index, &name)) (summary(&flow, true)) (save_count(&flow, true)) (keep_count(&flow, true)) }),
            None => Out::Go(dest::settings(&name)),
        }
    })
    .await
}

// The reference picker

#[derive(Debug, Default, Deserialize)]
pub struct PickerQuery {
    #[serde(default)]
    q: String,
}

fn kept_picking(ctx: &Ctx, package: &str) -> Option<Picking> {
    ctx.session.inner().stash.picking.clone().filter(|picking| picking.picker.package == package)
}

/// The list the picker shows for what is searched: those that fit first, the others with why
/// they do not, then the ones that need the owner's approval.
fn options(picking: &Picking, query: &str) -> Markup {
    let picker = &picking.picker;
    let choose = format!("{}/choose", reference_url(&picker.package));
    let listed = picker.visible(query);
    html! {
        section.card.flush #results {
            @if listed.is_empty() {
                p.pad.dim { @if picker.list.options.is_empty() { "No other app offers a setting yet." } @else { "Nothing matches that search." } }
            }
            div.rows {
                @for (option, verdict) in &listed {
                    @let locked = option.locked.is_some_and(|locked| !locked.granted);
                    @let secret = option.locked.is_some_and(|locked| locked.secret);
                    div class=(if verdict.is_ok() { "row" } else { "row off" }) {
                        span.name.mono { (option.path()) }
                        span.what {
                            span.tag { (kind_name(&option.kind)) } " "
                            @if option.locked.is_none() { span.tag { "public" } " " }
                            (option.label) @if !option.help.is_empty() { " · " (option.help) }
                            @match (verdict, &option.value) {
                                (Err(why), _) => span.note { " · " (why) },
                                (Ok(()), _) if secret => span.note { " · never shown" },
                                (Ok(()), Some(value)) => span.note.mono { " · " (value) },
                                (Ok(()), None) => {},
                            }
                        }
                        span.end {
                            @if verdict.is_ok() {
                                form method="post" action=(choose) {
                                    (layout::hidden("app", &option.app))
                                    (layout::hidden("setting", &option.setting))
                                    button.btn type="submit" { @if locked { "Needs your approval · Grant…" } @else { "Use" } }
                                }
                            }
                        }
                        span.go {}
                    }
                }
            }
        }
    }
}

fn picker_page(picking: &Picking, query: &str) -> Page {
    let picker = &picking.picker;
    let package = picker.package.as_str();
    let here = reference_url(package);
    let back = dest::settings(package);
    let body = html! {
        div.head {
            h1 { "Insert a reference" }
            div.actions { a.btn href=(back) { "Cancel" } }
        }
        p.lead { "into " (package) " · " (picker.field_label) " (" (kind_name(&picker.field_kind)) ")" }
        @if let Some(option) = &picker.granting {
            // A locked setting was chosen: nothing is read until the owner approves.
            section.card.attn {
                h2 { span.err aria-hidden="true" { "▲ " } "Let " (package) " use " (option.app) "'s " (option.label.to_lowercase()) "?" }
                @if option.locked.is_some_and(|locked| locked.secret) {
                    p { (option.setting) " is a secret: it is never shown, and only apps you approve can use it." }
                } @else {
                    p { (option.setting) " isn't public: only apps you approve can use it." }
                }
                p.dim { (package) " receives it when it starts. You can take this back in its settings." }
                div.actions.end {
                    (layout::post(&format!("{here}/decline"), "Cancel", "btn"))
                    (layout::post(&format!("{here}/grant"), "Grant", "btn primary"))
                }
            }
        } @else {
            form.searchbar method="get" action=(here) role="search"
                hx-get=(here) hx-trigger="input delay:300ms, submit" hx-target="#results" hx-swap="outerHTML" {
                input type="search" name="q" value=(query) placeholder="apps, settings, descriptions or values" aria-label="Search what can be referenced" autocomplete="off" autofocus;
                button.btn type="submit" { "Search" }
            }
            p.dim { (picker_hint(&picker.field_kind)) }
            (options(picking, query))
        }
    };
    Page::new(Tab::Apps, "Insert a reference", body).trail(&[
        ("Apps", dest::APPS),
        (package, &dest::app(package)),
        ("Settings", &back),
        ("Insert a reference", &here),
    ])
}

/// `GET /apps/{name}/settings/reference`: everything the other apps offer to reference,
/// narrowed by what is searched.
pub async fn reference(ctx: Ctx, Path(name): Path<String>, Query(query): Query<PickerQuery>) -> Response {
    match kept_picking(&ctx, &name) {
        Some(picking) if ctx.htmx => {
            use axum::response::IntoResponse;
            options(&picking, &query.q).into_response()
        }
        Some(picking) => ctx.page(picker_page(&picking, &query.q)),
        None => go_to(ctx.htmx, &dest::settings(&name)),
    }
}

/// Puts the picked reference into the field, asks what the field now resolves to, and follows
/// what the picker said comes after. The picker is done with.
fn place(work: &mut Work<'_>, flow: &mut Settings, picking: &Picking, app: &str, setting: &str, then: Step) -> Reply {
    let index = picking.picker.field_index;
    let effects = flow.place(index, picking.picker.mode, app, setting);
    let reply = work.run(Step::All(vec![Step::run(effects), then]), &mut |outcome, _| flow.apply(outcome));
    let mut inner = work.session.inner();
    inner.stash.picking = None;
    inner.stash.settings.insert(flow.form.package.clone(), flow.clone());
    reply
}

#[derive(Debug, Default, Deserialize)]
pub struct ChooseForm {
    #[serde(default)]
    app: String,
    #[serde(default)]
    setting: String,
}

/// The picker again, with what was just said.
fn to_picker(work: &Work<'_>, picking: Picking, reply: Reply) -> Out {
    if reply.login_required {
        return Out::Reply(reply);
    }
    let url = reference_url(&picking.picker.package);
    let mut inner = work.session.inner();
    inner.notices.extend(reply.notices);
    inner.stash.picking = Some(picking);
    Out::Go(url)
}

/// `POST /apps/{name}/settings/reference/choose`: the owner chooses a setting of another app.
/// One that does not fit is refused with the reason; one that is locked is asked about first;
/// any other goes into the field.
pub async fn choose(ctx: Ctx, Path(name): Path<String>, Form(form): Form<ChooseForm>) -> Response {
    let Some(mut picking) = kept_picking(&ctx, &name) else {
        return go_to(ctx.htmx, &dest::settings(&name));
    };
    ctx.with(dest::settings(&name), move |work| {
        let mut flow = match open_flow(work, &name) {
            Ok(flow) => flow,
            Err(reply) => return Out::Reply(reply),
        };
        match picking.picker.choose(&form.app, &form.setting) {
            Picked::Reference { app, setting, then } => Out::Reply(place(work, &mut flow, &picking, &app, &setting, then)),
            Picked::Nothing(step) => to_picker(work, picking, follow_plain(step)),
        }
    })
    .await
}

/// `POST /apps/{name}/settings/reference/grant`: the owner approves that this app reads the
/// locked setting chosen. Approved, the reference goes into the field.
pub async fn grant(ctx: Ctx, Path(name): Path<String>) -> Response {
    let Some(mut picking) = kept_picking(&ctx, &name) else {
        return go_to(ctx.htmx, &dest::settings(&name));
    };
    ctx.with(dest::settings(&name), move |work| {
        let mut flow = match open_flow(work, &name) {
            Ok(flow) => flow,
            Err(reply) => return Out::Reply(reply),
        };
        let mut approved: Option<(String, String, Step)> = None;
        let effects = picking.picker.approve();
        let reply = work.run(Step::run(effects), &mut |outcome, _| match picking.picker.apply(outcome) {
            Picked::Reference { app, setting, then } => {
                approved = Some((app, setting, then));
                // Followed below, once the reference is in the field. An empty list of steps,
                // and not `Stay`, so the flows without state do not say "approved" as well.
                Step::All(Vec::new())
            }
            Picked::Nothing(step) => step,
        });
        match approved {
            Some((app, setting, then)) => {
                let mut placed = place(work, &mut flow, &picking, &app, &setting, then);
                placed.login_required |= reply.login_required;
                Out::Reply(placed)
            }
            None => to_picker(work, picking, reply),
        }
    })
    .await
}

/// `POST /apps/{name}/settings/reference/decline`: the owner does not approve. The picker stays.
pub async fn decline(ctx: Ctx, Path(name): Path<String>) -> Response {
    if let Some(picking) = ctx.session.inner().stash.picking.as_mut().filter(|picking| picking.picker.package == name) {
        picking.picker.decline();
    }
    go_to(ctx.htmx, &reference_url(&name))
}

// A save that was rolled back

/// `GET /apps/{name}/settings/failed`: why the helper took a save back, and what had been
/// changed. The draft is kept, to edit again or to drop.
pub async fn failed(ctx: Ctx, Path(name): Path<String>) -> Response {
    let (failed, form) = {
        let inner = ctx.session.inner();
        (inner.stash.apply_failed.clone().filter(|failed| failed.package == name), inner.stash.settings.get(&name).map(|flow| flow.form.clone()))
    };
    let Some(failed) = failed else {
        return go_to(ctx.htmx, &dest::settings(&name));
    };
    let here = dest::settings(&name);
    // The field the helper blames, to land on when the owner goes back to edit.
    let blamed = form.and_then(|form| failed.blamed_index(&form).map(|index| form.fields[index].key.clone()));
    let edit = blamed.map_or_else(|| here.clone(), |key| format!("{here}#field-{}", dest::seg(&key)));
    let explanation = &failed.explanation;
    let body = html! {
        h1 { span.err aria-hidden="true" { "▲ " } (name) " didn't start with the new settings, so they were undone" }
        section.card.bad {
            p { "It's running again with the settings it had before. What you changed:" }
            dl.facts {
                @for row in &failed.rows {
                    dt { (row.label) }
                    dd { (row.from) "  →  " span class=[row.blamed.then_some("err")] { (row.to) } @if row.blamed { " " span.tag { "the cause" } } }
                }
            }
            @if !explanation.headline.is_empty() { p.dim { (name) " said: \"" (explanation.headline) "\"" } }
            @if !explanation.steps.is_empty() {
                ul.bullets {
                    @for (passed, step) in &explanation.steps {
                        li { @if *passed { span.ok aria-hidden="true" { "✓" } } @else { span.err aria-hidden="true" { "✕" } } span { (step) } }
                    }
                }
            }
            @if !explanation.hint.is_empty() { p { (explanation.hint) } }
            div.actions.end {
                a.btn.quiet href=(dest::log(&name)) { "Show log ›" }
                (layout::post(&format!("{here}/failed/keep"), "Keep as it is", "btn"))
                a.btn.primary href=(edit) { "Edit again" }
            }
        }
    };
    ctx.page(Page::new(Tab::Apps, format!("{name} settings"), body).trail(&[("Apps", dest::APPS), (&name, &dest::app(&name)), ("Settings", &here)]))
}

/// `POST /apps/{name}/settings/failed/keep`: the owner keeps the settings the app runs with.
/// The draft is dropped, and the app's page is where that leaves them.
pub async fn keep_as_it_is(ctx: Ctx, Path(name): Path<String>) -> Response {
    {
        let mut inner = ctx.session.inner();
        if let Some(flow) = inner.stash.settings.get_mut(&name) {
            flow.discard();
        }
        inner.stash.settings.remove(&name);
        inner.stash.apply_failed = None;
    }
    go_to(ctx.htmx, &dest::app(&name))
}
