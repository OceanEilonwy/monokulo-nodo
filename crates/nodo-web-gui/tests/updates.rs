//! Updates, automatic updates and undoing one, through the router.

mod common;

use axum::http::StatusCode;
use common::{App, Browser};
use nodo_ui_core::mock::MOCK_PASSPHRASE;
use nodo_ui_core::model::{UpdateItem, UpdateNote};

const UPDATES: &str = "/system/updates";

/// Whether the page has this update ticked.
fn ticked(body: &str, name: &str) -> bool {
    body.contains(&format!(r#"name="tick" value="{name}" checked"#))
}

/// A row of the Updates page, from its tick box to the next one.
fn row<'a>(body: &'a str, name: &str) -> &'a str {
    let start = body.find(&format!(r#"name="tick" value="{name}""#)).unwrap_or_else(|| panic!("no row for {name} in:\n{body}"));
    let rest = &body[start..];
    rest[1..].find(r#"name="tick""#).map_or(rest, |end| &rest[..end])
}

#[tokio::test]
async fn the_page_ticks_what_is_safe_and_holds_releases_and_checks() {
    let app = App::new();
    // A source publishes a new version of tor, which the next check finds.
    app.backends.prepare(|mock| {
        mock.published.push(UpdateItem {
            name: "tor".into(),
            from: "0.4.8".into(),
            to: "0.4.9".into(),
            source: "official".into(),
            third_party: false,
            note: UpdateNote::Ready,
            held: false,
            preselected: true,
            grows: None,
        });
    });
    let mut browser = app.login().await;

    // Home and System lead here.
    browser.get("/").await.assert_has(r#"href="/system/updates""#);
    browser.get("/system").await.assert_has(r#"<a class="row" href="/system/updates">"#);

    let page = browser.get(UPDATES).await;
    page.assert_has("<h1>Updates</h1>").assert_has("The Nodo").assert_has("2026.11  →  2026.12").assert_has("Update and restart");
    assert!(ticked(&page.body, "moneropay") && !ticked(&page.body, "wallet-relay") && !ticked(&page.body, "cuprated"));
    assert!(row(&page.body, "wallet-relay").contains("asks for something new; you'll review it first"));
    assert!(row(&page.body, "cuprated").contains("you skipped this version") && row(&page.body, "cuprated").contains("Release it"));
    page.assert_has("automatic for 3").assert_has(r#"href="/system/undo""#);
    // From an app's page only that app is ticked.
    let only = browser.get("/system/updates?only=wallet-relay").await;
    assert!(ticked(&only.body, "wallet-relay") && !ticked(&only.body, "moneropay"));
    browser.get("/system/updates/changes/moneropay").await.assert_has("<pre class=\"text\">");

    // Nothing ticked: said, and nothing is sent.
    browser.post_then(UPDATES, &[("do", "apps")]).await.assert_has("Tick at least one app first.");

    // Holding an update back unticks it; the ticks of the others are as they were posted.
    let held = browser.post_then(UPDATES, &[("do", "hold:moneropay"), ("tick", "moneropay"), ("tick", "wallet-relay")]).await;
    assert!(row(&held.body, "moneropay").contains("you skipped this version"));
    assert!(!ticked(&held.body, "moneropay") && ticked(&held.body, "wallet-relay"));
    let released = browser.post_then(UPDATES, &[("do", "release:cuprated")]).await;
    assert!(!row(&released.body, "cuprated").contains("you skipped this version") && !ticked(&released.body, "cuprated"));
    // Ticking a held one releases it, as the flow says.
    let ticked_held = browser.post_then(UPDATES, &[("do", "release:cuprated"), ("tick", "moneropay")]).await;
    assert!(row(&ticked_held.body, "moneropay").contains("Hold it back"), "{}", row(&ticked_held.body, "moneropay"));

    // Check now runs as a job; what it found is ticked as a page that opens ticks it.
    assert_eq!(browser.post(UPDATES, &[("do", "check"), ("tick", "wallet-relay")]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), UPDATES);
    let checked = browser.get(UPDATES).await;
    checked.assert_has("available.").assert_has(r#"<td class="mono">0.4.8</td><td class="mono">0.4.9</td>"#);
    assert!(ticked(&checked.body, "tor") && ticked(&checked.body, "wallet-relay") && !ticked(&checked.body, "cuprated"));
}

#[tokio::test]
async fn a_batch_is_reviewed_asks_for_the_passphrase_and_only_the_passphrase_comes_from_the_browser() {
    let app = App::new();
    // wallet-relay's update asks for something new, and here it also grows its locked storage.
    app.backends.prepare(|mock| {
        let item = mock.updates.iter_mut().find(|item| item.name == "wallet-relay").expect("the mock has it");
        item.grows = Some("256 MB to 512 MB".into());
    });
    let mut browser = app.login().await;

    // Nothing waits for a passphrase or a review yet.
    assert_eq!(browser.get("/system/updates/passphrase").await.location(), UPDATES);
    assert_eq!(browser.get("/system/updates/review").await.location(), UPDATES);
    assert_eq!(browser.post("/system/updates/passphrase", &[("passphrase", MOCK_PASSPHRASE)]).await.location(), UPDATES);

    // Ticked or not, an update that asks for something new is reviewed first.
    let started = browser.post(UPDATES, &[("do", "apps"), ("tick", "wallet-relay")]).await;
    assert_eq!(started.location(), "/system/updates/review");
    let review = browser.get(started.location()).await;
    review.assert_has("Update wallet-relay to 0.5.0?").assert_has("This version newly asks to").assert_has("Connect out to any internet address");
    review.assert_has(r#"action="/system/updates/review""#).assert_has(r#"<a class="btn" href="/system/updates">Cancel</a>"#);

    // Approved: the batch grows locked storage, so the passphrase is asked for.
    let approved = browser.post("/system/updates/review", &[]).await;
    assert_eq!(approved.location(), "/system/updates/passphrase");
    let page = browser.get(approved.location()).await;
    page.assert_has("Some apps need more locked storage").assert_has("wallet-relay: 256 MB to 512 MB").assert_has(r#"autocomplete="current-password""#);
    // The page's form carries the passphrase box and nothing about the batch.
    assert!(!page.body.contains(r#"name="names""#) && !page.body.contains(r#"name="approvals""#) && !page.body.contains("type=\"hidden\""));
    // An empty box is refused with a remark.
    browser.post_then("/system/updates/passphrase", &[("passphrase", "")]).await.assert_has("Enter the device passphrase to grow the encrypted storage.");

    // A browser that sends a batch of its own gets the one the Nodo kept: moneropay is not
    // updated, and wallet-relay is, with the approval the helper issued for its review (the
    // mock refuses that update without one).
    let forged = [("passphrase", MOCK_PASSPHRASE), ("names", "moneropay"), ("tick", "moneropay"), ("approvals", "moneropay"), ("batch", "moneropay,system base")];
    assert_eq!(browser.post("/system/updates/passphrase", &forged).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/system/updates/results");
    let results = browser.get("/system/updates/results").await;
    results.assert_has("1 update is installed").assert_has("wallet-relay 0.5.0").assert_lacks("moneropay").assert_lacks("system base");
    // The batch is used up.
    assert_eq!(browser.get("/system/updates/passphrase").await.location(), UPDATES);

    assert_eq!(browser.post("/system/updates/results/leave", &[]).await.location(), UPDATES);
    let page = browser.get(UPDATES).await;
    page.assert_has(r#"<td class="mono">2.7.0</td><td class="mono">2.7.1</td>"#).assert_lacks(r#"name="tick" value="wallet-relay""#);
    assert_eq!(browser.get("/system/updates/results").await.location(), UPDATES);
}

#[tokio::test]
async fn a_wrong_passphrase_updates_nothing_and_says_so() {
    let app = App::new();
    app.backends.prepare(|mock| mock.updates.iter_mut().for_each(|item| item.grows = Some("1 GB to 2 GB".into())));
    let mut browser = app.login().await;
    let started = browser.post(UPDATES, &[("do", "apps"), ("tick", "moneropay")]).await;
    assert_eq!(started.location(), "/system/updates/passphrase");
    assert_eq!(browser.post("/system/updates/passphrase", &[("passphrase", "not it")]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/system/updates/results");
    // Refused before anything changed, which is its own outcome: not a count of nothing.
    let results = browser.get("/system/updates/results").await;
    results.assert_has("Not updated:").assert_has("that passphrase did not unlock this device").assert_has("Nothing was changed.").assert_lacks("not it");
    results.assert_lacks("0 updates are installed");
}

/// An update the helper refuses before changing anything, as it refuses one whose data drive has
/// no room for a copy of the app's data. Apps that keep their data there can update like any
/// other; when one cannot, nothing was rolled back and nothing is held.
#[tokio::test]
async fn a_refused_update_says_not_updated_and_holds_nothing() {
    const REASON: &str = "moneropay 2.8.0 moves its data to a newer layout, which 2.7.0 cannot read, so a copy of the data \
                          is kept on the data drive to roll back to. That needs 3.3 GB free there and 1.0 GB is free. \
                          Free up 2.3 GB on the data drive, then try again.";
    let app = App::new();
    app.backends.prepare(|mock| mock.knobs.update_refusals.push(("moneropay".to_owned(), REASON.to_owned())));
    let mut browser = app.login().await;
    // It is offered and ticked like any other update.
    assert!(ticked(&browser.get(UPDATES).await.body, "moneropay"));
    assert_eq!(browser.post(UPDATES, &[("do", "apps"), ("tick", "moneropay")]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/system/updates/results");
    let refused = browser.get("/system/updates/results").await;
    refused.assert_has("Not updated: moneropay 2.8.0 moves its data").assert_has("Nothing was changed.").assert_has("select the update again in Updates.");
    for wrong in ["restored", "said", "publisher", "put back", "didn't start", "Try again", "Hold it back", "on hold", "Show log"] {
        assert!(!refused.body.contains(wrong), "a refusal changed nothing, so no {wrong:?}: {}", refused.body);
    }
    // Done leads back to Updates, where the same update is still offered and not held.
    assert_eq!(browser.post("/system/updates/results/leave", &[]).await.location(), UPDATES);
    let page = browser.get(UPDATES).await;
    assert!(ticked(&page.body, "moneropay"), "{}", row(&page.body, "moneropay"));
    assert!(!row(&page.body, "moneropay").contains("you skipped this version"));
}

#[tokio::test]
async fn a_refusal_among_updates_that_went_in_is_listed_with_them() {
    let app = App::new();
    app.backends.prepare(|mock| mock.knobs.update_refusals.push(("moneropay".to_owned(), "No room on the data drive.".to_owned())));
    let mut browser = app.login().await;
    assert_eq!(browser.post(UPDATES, &[("do", "apps"), ("tick", "moneropay"), ("tick", "cuprated")]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/system/updates/results");
    let results = browser.get("/system/updates/results").await;
    results.assert_has("1 update is installed").assert_has("Not updated: No room on the data drive.");
    results.assert_has("Nothing was changed for those not updated.").assert_has("select the update again in Updates.");
}

#[tokio::test]
async fn a_failed_update_is_put_back_and_can_be_held_reported_or_tried_again() {
    let app = App::new();
    let mut browser = app.login().await;

    // The mock's moneropay update fails its health check. The Nodo's own update is another
    // batch, with its own button.
    assert_eq!(browser.post(UPDATES, &[("do", "apps"), ("tick", "moneropay")]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/system/updates/results");
    let failed = browser.get("/system/updates/results").await;
    failed.assert_has("moneropay's update didn't start, so 2.7.0 was put back").assert_has("Version 2.7.0 and its data were restored. moneropay is running again as before.");
    failed.assert_has("database migration 14").assert_has("already exists").assert_has(r#"href="/system/logs/moneropay""#).assert_has("Try again").assert_has("Hold it back");

    // A report for the publisher: the helper's answer is said, and the results stay.
    browser.post_then("/system/updates/results/report", &[]).await.assert_has("class=\"notice").assert_has("was put back");
    // Tried again, it fails again.
    assert_eq!(browser.post("/system/updates/results/retry", &[]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/system/updates/results");
    browser.get("/system/updates/results").await.assert_has("moneropay's update didn't start");
    // Held back, it is not offered ticked again.
    let page = browser.post_then("/system/updates/results/hold", &[]).await;
    assert!(page.body.contains("<h1>Updates</h1>") && !ticked(&page.body, "moneropay"));
    assert!(row(&page.body, "moneropay").contains("you skipped this version"));

    // The Nodo's own update.
    assert_eq!(browser.post(UPDATES, &[("do", "system")]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/system/updates/results");
    browser.get("/system/updates/results").await.assert_has("1 update is installed").assert_has("2026.12").assert_has("Applied, restart when ready");
    browser.post("/system/updates/results/leave", &[]).await;
    browser.post_then(UPDATES, &[("do", "system")]).await.assert_has("The Nodo is up to date.");
}

#[tokio::test]
async fn a_job_that_finished_elsewhere_points_to_the_results() {
    let app = App::new();
    let mut browser = app.login().await;
    assert_eq!(browser.post(UPDATES, &[("do", "system")]).await.location(), "/working");
    let session = app.session(&browser).await;
    while session.running_job(app.state.clock()).is_some() {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    browser.get("/apps").await.assert_has("The updates have finished.");
    browser.get("/system/updates/results").await.assert_has("1 update is installed");
}

#[tokio::test]
async fn automatic_updates_refuse_a_window_that_is_none_and_save_what_the_form_carries() {
    let app = App::new();
    let mut browser = app.login().await;
    let auto = "/system/auto-update";
    let page = browser.get(auto).await;
    page.assert_has("Automatic updates").assert_has("Next run tonight at 03:00.").assert_has(r#"<option value="Daily" selected>"#);
    page.assert_has(r#"name="window" value="03:00 to 05:00""#).assert_has(r#"name="on" value="moneropay" checked"#).assert_has("Update the Nodo by itself");
    page.assert_has("Runs new code without your review").assert_has("Updates that need a restart always wait for you, whatever this says.");
    assert!(!page.body.contains(r#"name="on" value="wallet-relay" checked"#));
    browser.get("/system/auto-update/history").await.assert_has("<pre class=\"text\">");

    // A window that is no window: said beside the box, with what was typed and chosen kept.
    let refused = browser.post(auto, &[("do", "save"), ("frequency", "Weekly"), ("window", "25:00 to 26:00"), ("on", "wallet-relay")]).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.assert_has("✕ Use two different device times, like 03:00 to 05:00.").assert_has(r#"name="window" value="25:00 to 26:00""#);
    refused.assert_has(r#"<option value="Weekly" selected>"#).assert_has(r#"name="on" value="wallet-relay" checked"#);
    browser.get(auto).await.assert_has(r#"<option value="Daily" selected>"#);

    // Saved: back on Updates with the flow's words, and stored.
    let saved = browser.post_then(auto, &[("do", "save"), ("frequency", "Weekly"), ("window", "02:00 to 04:00"), ("on", "moneropay")]).await;
    saved.assert_has("<h1>Updates</h1>").assert_has("Auto-update choices saved.").assert_has("automatic for 1");
    assert_eq!(saved.body.matches("Auto-update choices saved.").count(), 1);
    let page = browser.get(auto).await;
    page.assert_has(r#"<option value="Weekly" selected>"#).assert_has(r#"value="02:00 to 04:00""#).assert_has("Next run Sunday at 02:00.");
    assert!(!page.body.contains(r#"name="on" value="cuprated" checked"#));

    // Cancel with nothing changed leaves at once; with a change it asks first.
    let same = [("do", "leave"), ("frequency", "Weekly"), ("window", "02:00 to 04:00"), ("on", "moneropay")];
    assert_eq!(browser.post(auto, &same).await.location(), UPDATES);
    assert_eq!(browser.post(auto, &[("do", "leave"), ("frequency", "Daily"), ("window", "02:00 to 04:00"), ("on", "moneropay")]).await.location(), "/confirm");
    let question = browser.get("/confirm").await;
    question.assert_has("Discard your changes?").assert_has("Your auto-update choices will not be saved.").assert_has(r#"href="/system/auto-update""#);
    let id = Browser::field(&question.body, "id");
    assert_eq!(browser.post("/confirm", &[("id", &id)]).await.location(), UPDATES);
    browser.get(auto).await.assert_has(r#"<option value="Weekly" selected>"#);
}

#[tokio::test]
async fn an_update_is_undone_after_its_question_and_a_blocked_one_says_why() {
    let app = App::new();
    let mut browser = app.login().await;
    let page = browser.get("/system/undo").await;
    page.assert_has("Undo an update").assert_has("2.7.0_1 → 2.6.1_1").assert_has(r#"action="/system/undo/moneropay""#);
    page.assert_has("its data is in layout 2").assert_lacks(r#"action="/system/undo/wallet-relay""#);
    page.assert_has("Updates that were rolled back").assert_has("stayed on 1.2.0_1: bell did not pass its health check");
    page.assert_has("The system runs the version it was updated to. No fallback happened.");

    // Blocked, or nothing kept: refused with the reason.
    browser.post_then("/system/undo/wallet-relay", &[]).await.assert_has("wallet-relay cannot roll back now: its data is in layout 2.");
    browser.post_then("/system/undo/tor", &[]).await.assert_has("Nothing can be rolled back now.");

    // Asked first; with htmx as a dialog over the page.
    let mut htmx = browser.clone();
    htmx.htmx = true;
    let dialog = htmx.post("/system/undo/moneropay", &[]).await;
    dialog.assert_has("Roll moneropay back?").assert_has("moneropay goes from 2.7.0_1 back to 2.6.1_1.").assert_has(r#"href="/system/undo""#);
    // Confirmed, it runs as a job and the page says how it went.
    let id = Browser::field(&dialog.body, "id");
    assert_eq!(browser.post("/confirm", &[("id", &id)]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/system/undo");
    let done = browser.get("/system/undo").await;
    done.assert_has("moneropay is back on 2.6.1_1.").assert_lacks(r#"action="/system/undo/moneropay""#);
}
