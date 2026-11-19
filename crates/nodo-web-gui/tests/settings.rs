//! An app's settings, through the router: every kind of field, references and the picker, saving
//! and what happens when the helper takes a save back.

mod common;

use axum::http::StatusCode;
use common::{App, Browser};
use nodo_api::{ReferenceAccess, SettingKind};
use nodo_ui_core::mock::MockBackend;
use nodo_ui_core::model::{Field, FieldKind, FieldValue};

const SETTINGS: &str = "/apps/moneropay/settings";

/// The mock's moneropay form has a URL, a port, a choice, a Monero address, a secret and a
/// switch. These add the two kinds it lacks, and a setting of another app that is not public.
fn with_every_kind(mock: &mut MockBackend) {
    let field = |key: &str, label: &str, kind, value: &str| Field {
        key: key.into(),
        label: label.into(),
        help: String::new(),
        requirement: "Optional.".into(),
        required: false,
        kind,
        value: FieldValue::Text(value.into()),
    };
    let form = mock.forms.get_mut("moneropay").expect("moneropay has settings");
    form.fields.push(field("shop_name", "Shop name", FieldKind::Text, "My shop"));
    form.fields.push(field("workers", "Workers", FieldKind::Number { min: Some(1), max: Some(10) }, "2"));
    mock.references.push(nodo_api::ReferenceOption {
        package: "cuprated".into(),
        reference: "zmq_url".into(),
        label: "ZMQ address".into(),
        help: "Where it publishes blocks".into(),
        kind: SettingKind::Url,
        access: ReferenceAccess::NeedsGrant { secret: false, granted: false },
    });
}

async fn app() -> App {
    let app = App::new();
    app.backends.prepare(with_every_kind);
    app
}

/// Presses a button of the form with these fields, and loads the page it leads to.
async fn press(browser: &mut Browser<'_>, action: &str, fields: &[(&str, &str)]) -> common::Answer {
    let mut form: Vec<(&str, &str)> = fields.to_vec();
    form.push(("do", action));
    browser.post_then(SETTINGS, &form).await
}

#[tokio::test]
async fn every_kind_of_field_has_its_control_and_a_change_is_checked_as_it_is_made() {
    let app = app().await;
    let mut browser = app.login().await;
    let page = browser.get(SETTINGS).await;
    assert_eq!(page.status, StatusCode::OK);
    page.assert_has("<h1>Settings</h1>").assert_has("No changes yet").assert_has("moneropay restarts with the new settings.");
    page.assert_has(r#"name="f.daemon_rpc" value="http://127.0.0.1:18081""#);
    page.assert_has(r#"name="f.api_port" value="5000""#).assert_has(r#"inputmode="numeric""#);
    page.assert_has(r#"<select id="f-log_level" name="f.log_level""#).assert_has(r#"<option value="info" selected>"#);
    page.assert_has(r#"name="f.payment_address""#);
    page.assert_has(r#"<input type="checkbox" id="f-allow_lan" name="f.allow_lan" value="on" checked"#);
    page.assert_has(r#"name="f.shop_name" value="My shop""#).assert_has(r#"name="f.workers" value="2""#);
    // A stored secret is never sent: its box is empty and says only that one is stored.
    page.assert_has(r#"<input type="password" id="f-webhook_secret" name="s.webhook_secret" autocomplete="new-password" placeholder="••••••••••••""#);
    assert!(!page.body.contains(r#"name="s.webhook_secret" value"#));

    // With htmx a changed field posts and gets its own row back, with what is wrong beside it
    // and the count of changes to replace the one at the top.
    let mut htmx = browser.clone();
    htmx.htmx = true;
    let row = htmx.post(&format!("{SETTINGS}/field/api_port"), &[("f.api_port", "http")]).await;
    assert_eq!(row.status, StatusCode::OK);
    assert!(row.body.starts_with(r#"<div class="setting" id="field-api_port">"#), "{}", row.body);
    row.assert_has("✕ Port must be a number from 1 to 65535.").assert_has(r#"aria-invalid="true""#).assert_has("changed");
    row.assert_has(r#"<p id="changes" class="warn" hx-swap-oob="true">1 change, not saved yet</p>"#).assert_lacks("<html");

    // The same for each kind, in the flow's words.
    for (key, value, problem) in [
        ("workers", "99", "Must be from 1 to 10."),
        ("workers", "many", "Must be a whole number."),
        ("daemon_rpc", "ftp://node", "Must start with http:// or https://."),
        ("payment_address", "48edf", "Address is too short. A Monero address has 95 characters."),
        ("api_port", "", "Required: give it a value."),
    ] {
        let row = htmx.post(&format!("{SETTINGS}/field/{key}"), &[(&format!("f.{key}"), value)]).await;
        row.assert_has(problem);
    }
    // An optional field may be left empty.
    htmx.post(&format!("{SETTINGS}/field/shop_name"), &[("f.shop_name", "")]).await.assert_lacks("class=\"problem\"");

    // Save is refused while a field does not fit, with the form shown again.
    let refused = browser.post(SETTINGS, &[("do", "save")]).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.assert_has("Fix the marked field before saving. Nothing was changed.").assert_has("Required: give it a value.");

    // Putting a field back to what is saved, one by one.
    let page = press(&mut browser, "reset:api_port", &[]).await;
    page.assert_has(r#"name="f.api_port" value="5000""#);
    for key in ["workers", "daemon_rpc", "payment_address"] {
        press(&mut browser, &format!("reset:{key}"), &[]).await;
    }
    browser.get(SETTINGS).await.assert_has("1 change, not saved yet").assert_lacks("class=\"problem\"");
}

#[tokio::test]
async fn a_choice_a_switch_a_number_and_a_secret_are_saved_and_the_secret_is_never_shown() {
    let app = app().await;
    let mut browser = app.login().await;
    browser.get(SETTINGS).await;

    // The whole form as a browser without scripts posts it: the switch unticked posts only its
    // marker, the secret box holds what was typed.
    let fields = [("f.log_level", "debug"), ("t.allow_lan", "1"), ("f.workers", "4"), ("f.shop_name", "Corner shop"), ("s.webhook_secret", "hunter2-new")];
    let checked = press(&mut browser, "apply", &fields).await;
    checked.assert_has("5 changes, not saved yet").assert_has(r#"<option value="debug" selected>"#).assert_has(r#"name="f.workers" value="4""#);
    checked.assert_has("A new secret is typed; saving stores it").assert_lacks("hunter2-new").assert_lacks(r#"value="on" checked"#);

    // Saving restarts the app, so it runs as a job and ends on the app's page.
    let started = browser.post(SETTINGS, &[("do", "save")]).await;
    assert_eq!(started.location(), "/working");
    let done = browser.wait_for_job().await;
    assert_eq!(done.location(), "/apps/moneropay");
    browser.get("/apps/moneropay").await.assert_has("Saved. moneropay restarted and passed its health check.");

    // The form is read afresh: what was saved is what is stored now.
    let page = browser.get(SETTINGS).await;
    page.assert_has("No changes yet").assert_has(r#"<option value="debug" selected>"#).assert_has(r#"value="Corner shop""#);
    page.assert_has(r#"placeholder="••••••••••••""#).assert_lacks("hunter2-new").assert_lacks(r#"value="on" checked"#);

    // Saving with nothing changed says so.
    press(&mut browser, "save", &[]).await.assert_has("There are no changes to save.");
}

#[tokio::test]
async fn a_reference_is_picked_placed_taken_back_typed_instead_and_removed() {
    let app = app().await;
    let mut browser = app.login().await;
    browser.get(SETTINGS).await.assert_has(r#"value="pick:daemon_rpc""#).assert_has(r#"value="add:daemon_rpc""#);
    // A secret, a choice and a switch take no reference.
    let page = browser.get(SETTINGS).await;
    for key in ["webhook_secret", "log_level", "allow_lan"] {
        page.assert_lacks(&format!(r#"value="pick:{key}""#));
    }

    // The picker: those that fit the field first, the others with why they do not.
    let opened = browser.post(SETTINGS, &[("do", "pick:daemon_rpc")]).await;
    assert_eq!(opened.location(), "/apps/moneropay/settings/reference");
    let picker = browser.get(opened.location()).await;
    picker.assert_has("Insert a reference").assert_has("into moneropay · Daemon RPC (url)").assert_has("cuprated.rpc_url").assert_has("http://127.0.0.1:18081");
    picker.assert_has("A number; the field takes an address.").assert_has("A secret can only go into a secret setting.").assert_has("Needs your approval · Grant…");
    let first = picker.body.find("cuprated.rpc_url").expect("listed");
    let misfit = picker.body.find("cuprated.rpc_port").expect("listed");
    assert!(first < misfit, "what fits comes first");
    // Searching narrows the list; htmx gets the list alone.
    let mut htmx = browser.clone();
    htmx.htmx = true;
    let found = htmx.get("/apps/moneropay/settings/reference?q=wallet").await;
    assert!(found.body.starts_with("<section"), "{}", found.body);
    found.assert_has("wallet-rpc.url").assert_lacks("cuprated.rpc_url");

    // One that does not fit is refused with the reason, and the picker stays.
    let refused = browser.post_then("/apps/moneropay/settings/reference/choose", &[("app", "cuprated"), ("setting", "rpc_port")]).await;
    refused.assert_has("A number; the field takes an address.").assert_has("Insert a reference");

    // One that fits goes into the field, with what it resolves to and the way back.
    let placed = browser.post_then("/apps/moneropay/settings/reference/choose", &[("app", "cuprated"), ("setting", "rpc_url")]).await;
    placed.assert_has("<h1>Settings</h1>").assert_has("cuprated·rpc_url").assert_has("→ http://127.0.0.1:18081").assert_has("1 change, not saved yet");
    placed.assert_has("↺ Undo: back to http://127.0.0.1:18081").assert_lacks(r#"name="f.daemon_rpc""#);
    let undone = press(&mut browser, "undo:daemon_rpc", &[]).await;
    undone.assert_has(r#"name="f.daemon_rpc" value="http://127.0.0.1:18081""#).assert_has("No changes yet").assert_lacks("cuprated·rpc_url");

    // Typing instead turns the reference into what it resolves to; that too can be taken back.
    browser.post(SETTINGS, &[("do", "pick:daemon_rpc")]).await;
    browser.post("/apps/moneropay/settings/reference/choose", &[("app", "wallet-rpc"), ("setting", "url")]).await;
    let typed = press(&mut browser, "type:daemon_rpc:0", &[]).await;
    typed.assert_has(r#"name="f.daemon_rpc" value="http://127.0.0.1:34512""#).assert_has("↺ Use wallet-rpc.url again");
    press(&mut browser, "reset:daemon_rpc", &[]).await.assert_has("No changes yet");

    // After the text: a text field holds text and references together, each text in its box.
    browser.post(SETTINGS, &[("do", "add:shop_name"), ("f.shop_name", "node at ")]).await;
    browser.get("/apps/moneropay/settings/reference").await.assert_has("into moneropay · Shop name (text)");
    let mixed = browser.post_then("/apps/moneropay/settings/reference/choose", &[("app", "cuprated"), ("setting", "rpc_host")]).await;
    mixed.assert_has(r#"name="p.shop_name.0" value="node at ""#).assert_has("cuprated·rpc_host").assert_has("Now: <span class=\"mono\">node at 127.0.0.1</span>");
    let edited = press(&mut browser, "apply", &[("p.shop_name.0", "host "), ("p.shop_name.post", " (local)")]).await;
    edited.assert_has(r#"name="p.shop_name.0" value="host ""#).assert_has(r#"name="p.shop_name.2" value=" (local)""#).assert_has("host 127.0.0.1 (local)");
    // Another reference in its place.
    browser.post(SETTINGS, &[("do", "replace:shop_name:0")]).await;
    let replaced = browser.post_then("/apps/moneropay/settings/reference/choose", &[("app", "wallet-rpc"), ("setting", "url")]).await;
    replaced.assert_has("wallet-rpc·url").assert_lacks("cuprated·rpc_host");
    // Removed, the text is what is left.
    let removed = press(&mut browser, "remove:shop_name:0", &[]).await;
    removed.assert_has(r#"name="f.shop_name" value="host  (local)""#).assert_has("↺ Put wallet-rpc.url back");
}

#[tokio::test]
async fn a_setting_that_is_not_public_is_asked_about_and_saving_says_what_the_reference_allows() {
    let app = app().await;
    let mut browser = app.login().await;
    browser.get(SETTINGS).await;
    browser.post(SETTINGS, &[("do", "pick:daemon_rpc")]).await;

    // Choosing a locked setting asks first, and reads nothing until the owner approves.
    let asked = browser.post_then("/apps/moneropay/settings/reference/choose", &[("app", "cuprated"), ("setting", "zmq_url")]).await;
    asked.assert_has("Let moneropay use cuprated's zmq address?").assert_has("zmq_url isn't public: only apps you approve can use it.").assert_has(">Grant<");
    // Declined: the list again, the setting still locked.
    let declined = browser.post_then("/apps/moneropay/settings/reference/decline", &[]).await;
    declined.assert_has("Needs your approval · Grant…").assert_lacks("Let moneropay use");
    browser.get(SETTINGS).await.assert_has("No changes yet");

    // Approved: the reference goes into the field and the approval is said, once.
    browser.post("/apps/moneropay/settings/reference/choose", &[("app", "cuprated"), ("setting", "zmq_url")]).await;
    let granted = browser.post_then("/apps/moneropay/settings/reference/grant", &[]).await;
    granted.assert_has("<h1>Settings</h1>").assert_has("cuprated·zmq_url").assert_has("Approved: moneropay can use cuprated.zmq_url.");
    assert_eq!(granted.body.matches("Approved:").count(), 1, "{}", granted.body);
    // The picker is done with.
    assert_eq!(browser.get("/apps/moneropay/settings/reference").await.location(), SETTINGS);

    // A public reference instead, and Save stops to say what it allows.
    press(&mut browser, "reset:daemon_rpc", &[]).await;
    browser.post(SETTINGS, &[("do", "pick:daemon_rpc")]).await;
    browser.post("/apps/moneropay/settings/reference/choose", &[("app", "cuprated"), ("setting", "rpc_url")]).await;
    let confirm = press(&mut browser, "save", &[]).await;
    confirm.assert_has("Save moneropay's settings?").assert_has("These references change what moneropay can reach:");
    confirm.assert_has("moneropay will be able to reach cuprated (port 18081/tcp).");
    confirm.assert_has("cuprated becomes a dependency: it can't be removed while moneropay uses it.");
    confirm.assert_has("moneropay follows cuprated's rpc_url when it changes.");
    // Cancelled, nothing is sent and the draft stays.
    let cancelled = press(&mut browser, "cancel-save", &[]).await;
    cancelled.assert_lacks("Save moneropay's settings?").assert_has("1 change, not saved yet");
    // Confirmed, it is saved.
    press(&mut browser, "save", &[]).await.assert_has("Save moneropay's settings?");
    assert_eq!(browser.post(SETTINGS, &[("do", "confirm-save")]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/apps/moneropay");
    browser.get(SETTINGS).await.assert_has("cuprated·rpc_url").assert_has("No changes yet");
}

#[tokio::test]
async fn a_save_the_helper_takes_back_says_why_and_keeps_the_draft() {
    let app = app().await;
    let mut browser = app.login().await;
    browser.get(SETTINGS).await;
    // The mock's daemon holds this port, so moneropay cannot start on it.
    let started = browser.post(SETTINGS, &[("do", "save"), ("f.api_port", "18081"), ("f.log_level", "debug")]).await;
    assert_eq!(started.location(), "/working");
    let done = browser.wait_for_job().await;
    assert_eq!(done.location(), "/apps/moneropay/settings/failed");
    let failed = browser.get(done.location()).await;
    failed.assert_has("moneropay didn't start with the new settings, so they were undone");
    failed.assert_has("It's running again with the settings it had before. What you changed:");
    failed.assert_has("<dt>API port</dt>").assert_has("5000  →  ").assert_has("18081").assert_has("<dt>Log level</dt>");
    failed.assert_has("address already in use").assert_has(r#"href="/system/logs/moneropay""#).assert_has("Edit again").assert_has("Keep as it is");

    // Edit again: the draft is as it was left.
    let page = browser.get(SETTINGS).await;
    page.assert_has(r#"name="f.api_port" value="18081""#).assert_has("2 changes, not saved yet");

    // Keep as it is: the draft goes, and the form shows what the app runs with.
    assert_eq!(browser.post("/apps/moneropay/settings/failed/keep", &[]).await.location(), "/apps/moneropay");
    browser.get(SETTINGS).await.assert_has(r#"name="f.api_port" value="5000""#).assert_has("No changes yet");
    assert_eq!(browser.get("/apps/moneropay/settings/failed").await.location(), SETTINGS);
}

#[tokio::test]
async fn leaving_with_changes_asks_and_the_draft_is_kept_until_the_answer() {
    let app = app().await;
    let mut browser = app.login().await;
    // The app's page leads here.
    browser.get("/apps/moneropay").await.assert_has(r#"href="/apps/moneropay/settings""#);
    browser.get(SETTINGS).await;

    // Nothing changed: Cancel leaves at once.
    assert_eq!(browser.post(SETTINGS, &[("do", "leave")]).await.location(), "/apps/moneropay");

    // Changed: the question first. What was typed with the Cancel button counts.
    let asked = browser.post(SETTINGS, &[("do", "leave"), ("f.shop_name", "Other shop")]).await;
    assert_eq!(asked.location(), "/confirm");
    let question = browser.get("/confirm").await;
    question.assert_has("Discard your changes?").assert_has("Your unsaved settings will be lost.").assert_has(r#"<a class="btn" href="/apps/moneropay/settings""#);
    // Not answered yet: the draft is still there, also after a look at another page.
    browser.get("/apps").await;
    browser.get(SETTINGS).await.assert_has(r#"value="Other shop""#).assert_has("1 change, not saved yet");

    // Discarded: back on the app's page, and the form is as stored.
    browser.post(SETTINGS, &[("do", "leave")]).await;
    let id = Browser::field(&browser.get("/confirm").await.body, "id");
    assert_eq!(browser.post("/confirm", &[("id", &id)]).await.location(), "/apps/moneropay");
    browser.get(SETTINGS).await.assert_has(r#"value="My shop""#).assert_has("No changes yet");

    // Another session has its own draft.
    let mut other = app.login().await;
    press(&mut browser, "apply", &[("f.shop_name", "Mine")]).await;
    other.get(SETTINGS).await.assert_has(r#"value="My shop""#);

    // An app with nothing to set says so on its page.
    let without = MockBackend::new().services.iter().map(|svc| svc.id.clone()).find(|id| !MockBackend::new().forms.contains_key(id)).expect("an app without settings");
    let answer = browser.get(&format!("/apps/{without}/settings")).await;
    assert_eq!(answer.location(), format!("/apps/{without}"));
    browser.get(answer.location()).await.assert_has(&format!("{without} has no settings to configure."));
}
