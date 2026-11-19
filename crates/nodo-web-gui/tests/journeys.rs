//! One journey per group of pages, through the router, over `MockBackend`.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use common::{App, Browser};
use nodo_ui_core::mock::{MockBackend, MOCK_PASSPHRASE};

/// The feed an app on offer comes from, as the mock's catalog has it.
fn feed_of(name: &str) -> String {
    MockBackend::new().catalog.iter().find(|pkg| pkg.id.name == name).map(|pkg| pkg.id.feed.clone()).expect("the mock offers it")
}

/// Answers the question waiting on `/confirm` with yes, and loads the page that follows.
async fn confirm(browser: &mut Browser<'_>, title: &str) -> common::Answer {
    let question = browser.get("/confirm").await;
    assert_eq!(question.status, StatusCode::OK);
    question.assert_has(title).assert_has("Cancel");
    let id = Browser::field(&question.body, "id");
    browser.post_then("/confirm", &[("id", &id)]).await
}

#[tokio::test]
async fn from_adding_a_source_to_removing_an_app_it_offered() {
    let app = App::new();
    let mut browser = app.login().await;

    // Sources: an address that is wrong is said beside the box, by the flow and by the helper.
    browser.get("/sources").await.assert_has("official").assert_has("built in").assert_lacks("monero-tools");
    browser.get("/sources/add").await.assert_has(r#"<label for="address">Address</label>"#);
    let empty = browser.post("/sources/add", &[("address", "")]).await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
    empty.assert_has("Enter the feed's address.");
    let refused = browser.post("/sources/add", &[("address", "ftp://example.org/feed")]).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.assert_has("A feed address must start with https://").assert_has(r#"value="ftp://example.org/feed""#);

    // Probe, then the confirmation with the key to compare, then trust.
    let probed = browser.post("/sources/add", &[("address", "https://github.com/example/monero-tools-feed")]).await;
    assert_eq!((probed.status, probed.location()), (StatusCode::SEE_OTHER, "/sources/add/confirm"));
    let confirmation = browser.get("/sources/add/confirm").await;
    confirmation.assert_has("monero-tools").assert_has("It offers 9 apps.").assert_has("<span>c41e8d07</span>").assert_has("Add source");
    browser.get("/sources/add/confirm/packages").await.assert_has("Back");
    let sources = browser.post_then("/sources/add/confirm", &[]).await;
    sources.assert_has("Added monero-tools and pinned its key.").assert_has(r#"id="source-monero-tools""#);
    // Said once.
    browser.get("/sources").await.assert_lacks("Added monero-tools and pinned its key.");
    // The probe is used up: the confirmation has nothing to show any more.
    assert_eq!(browser.get("/sources/add/confirm").await.location(), "/sources/add");

    // Get apps: search, details, the gate, the review.
    let found = browser.get("/apps/get?q=pay").await;
    found.assert_has("3 of 41 apps match.").assert_has(r#"href="/apps/get/official/moneropay""#).assert_lacks("sample-tool-01");
    let detail = browser.get("/apps/get/official/moneropay").await;
    detail.assert_has("Payment gateway for Monero").assert_has("It will").assert_has(r#"action="/apps/get/official/moneropay/install""#);
    browser.get("/apps/get/official/moneropay/doc/versions").await.assert_has("<pre class=\"text\">");
    let gate = browser.post("/apps/get/official/moneropay/install", &[]).await;
    assert_eq!(gate.location(), "/apps/get/official/moneropay/review");
    let review = browser.get(gate.location()).await;
    review.assert_has("Install moneropay?").assert_has("It will be able to").assert_has("Cancel");
    browser.get("/apps/get/official/moneropay/doc/files").await.assert_has("Back");

    // Approving starts the install as a job; the browser waits on /working and lands on its end.
    let started = browser.post("/apps/get/official/moneropay/review", &[("passphrase", MOCK_PASSPHRASE)]).await;
    assert_eq!((started.status, started.location()), (StatusCode::SEE_OTHER, "/working"));
    let finished = browser.wait_for_job().await;
    assert_eq!(finished.location(), "/apps/get/official/moneropay/installing");
    let installed = browser.get(finished.location()).await;
    installed.assert_has("moneropay is installed").assert_has("Set it up").assert_has("Done");
    browser.get("/apps/get/official/moneropay/doc/log").await.assert_has("Back");
    let left = browser.post("/apps/get/official/moneropay/installing/leave", &[]).await;
    assert_eq!(left.location(), "/apps/moneropay");

    // The app's page: stop, then remove after the question.
    let page = browser.get("/apps/moneropay").await;
    page.assert_has("Running").assert_has(">Stop<").assert_has("Version 2.7.1 is ready");
    let stopped = browser.post_then("/apps/moneropay/toggle", &[]).await;
    stopped.assert_has("Stopped").assert_has(">Start<");
    let asked = browser.post("/apps/moneropay/remove", &[]).await;
    assert_eq!((asked.status, asked.location()), (StatusCode::SEE_OTHER, "/confirm"));
    browser.get("/apps/moneropay").await.assert_has("Stopped");
    let apps = confirm(&mut browser, "Remove moneropay?").await;
    // Back on the list of apps with what the helper said. The mock has no removal, and says so;
    // that its words are here shows the request reached it.
    assert!(apps.body.contains("<h1>Installed</h1>"), "{}", apps.body);
    apps.assert_has("remove-package");
    // Answered once: the same answer again does nothing.
    assert_eq!(browser.get("/confirm").await.location(), "/");
}

#[tokio::test]
async fn a_question_is_a_dialog_for_htmx_and_a_stale_answer_does_nothing() {
    let app = App::new();
    let mut browser = app.login().await;
    let mut htmx = browser.clone();
    htmx.htmx = true;

    let dialog = htmx.post("/apps/tor/remove", &[]).await;
    assert_eq!(dialog.status, StatusCode::OK);
    dialog.assert_has(r#"role="alertdialog""#).assert_has("Remove tor?").assert_has(r#"href="/apps/tor""#).assert_lacks("<html");
    // The same button without scripts leads to the question's own page, with the same answers.
    browser.post("/apps/tor/remove", &[]).await;
    browser.get("/confirm").await.assert_has("Remove tor?").assert_has("<html");

    // An id that is not the open question's confirms nothing.
    let stale = browser.post_then("/confirm", &[("id", "9999")]).await;
    stale.assert_has("That question was answered already. Nothing was done.");
    browser.get("/apps/tor").await.assert_has("Running");
}

#[tokio::test]
async fn home_says_what_needs_the_owner_and_unlocks_what_is_still_locked() {
    let app = App::new();
    app.backends.stay_locked.store(true, Ordering::SeqCst);
    let mut browser = app.login().await;

    let home = browser.get("/").await;
    home.assert_has("Your Nodo").assert_has("Your Nodo is locked").assert_has(r#"action="/unlock""#).assert_has("updates are ready");
    home.assert_has("Locked until you unlock");

    let wrong = browser.post_then("/unlock", &[("passphrase", "wrong")]).await;
    wrong.assert_has("That passphrase did not unlock this device. Try again.").assert_has("Your Nodo is locked");
    // The wait after a wrong try is the unlock flow's, kept by this process's clock.
    let early = browser.post_then("/unlock", &[("passphrase", MOCK_PASSPHRASE)]).await;
    early.assert_has("Please wait 2 seconds before trying again.");
    app.clock.advance(Duration::from_secs(2));
    let unlocked = browser.post_then("/unlock", &[("passphrase", MOCK_PASSPHRASE)]).await;
    unlocked.assert_has("Unlocked.").assert_lacks("Your Nodo is locked").assert_lacks(r#"action="/unlock""#);
    unlocked.assert_has("This Nodo").assert_has("412 GB of 1.9 TB");
}

#[tokio::test]
async fn an_apps_page_acts_through_the_flow_and_says_what_it_answers() {
    let app = App::new();
    let mut browser = app.login().await;

    let apps = browser.get("/apps").await;
    apps.assert_has("Installed").assert_has(r#"href="/apps/get""#).assert_has(r#"href="/apps/cuprated""#).assert_has("Update ready").assert_has("Monero node in Rust");

    // Start-or-stop is one button whose meaning the flow decides from the app's state.
    browser.get("/apps/monerod").await.assert_has("Stopped").assert_has(">Start<");
    browser.post_then("/apps/monerod/toggle", &[]).await.assert_has("Running").assert_has(">Stop<");
    browser.post_then("/apps/monerod/restart", &[]).await.assert_has("Running");

    // Update: with one offered it leads towards the Updates page, without one it says so.
    let update = browser.post("/apps/moneropay/update", &[]).await;
    assert_eq!(update.location(), "/system", "the Updates page is not built yet; System stands in");
    browser.post_then("/apps/cuprated/update", &[]).await.assert_has("cuprated is up to date.");

    // The rows under it: Logs leads to the app's log; the others say they are not here yet.
    let page = browser.get("/apps/cuprated").await;
    page.assert_has(r#"href="/system/logs/cuprated""#).assert_has("Hosting").assert_has("Settings");

    let missing = browser.get("/apps/nothing-of-that-name").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    missing.assert_has("is not installed");
}

#[tokio::test]
async fn the_gate_says_no_in_two_ways_and_neither_page_installs_the_package() {
    let app = App::new();
    let mut browser = app.login().await;

    let rejected = format!("/apps/get/{}/fast-sync-helper", feed_of("fast-sync-helper"));
    let gate = browser.post(&format!("{rejected}/install"), &[]).await;
    assert_eq!(gate.location(), format!("{rejected}/rejected"));
    let page = browser.get(gate.location()).await;
    page.assert_has("can't be installed").assert_has("Runs a setup script as root").assert_has("./INSTALL").assert_has(r#"href="/sources""#);
    page.assert_lacks("Signature matches the feed").assert_lacks(&format!(r#"action="{rejected}/install""#));
    // The report is the helper's to make; what it answers is said.
    browser.post_then(&format!("{rejected}/rejected/report"), &[]).await.assert_has("class=\"notice");

    let lacking = format!("/apps/get/{}/zmq-indexer", feed_of("zmq-indexer"));
    let gate = browser.post(&format!("{lacking}/install"), &[]).await;
    assert_eq!(gate.location(), format!("{lacking}/capability"));
    let page = browser.get(gate.location()).await;
    page.assert_has("needs a different Monero node first").assert_has("zmq-indexer needs ZMQ.").assert_has("Install monerod");
    browser.post_then(&format!("{lacking}/capability/compatible"), &[]).await.assert_has("No compatible version");
    // The node that has it goes through the gate like any other app.
    let other = browser.post(&format!("{lacking}/capability/install"), &[]).await;
    assert!(other.location().starts_with("/apps/get/official/monerod/"), "{}", other.location());

    // A page of the install opened without its way there leads back to the app's details.
    let mut fresh = app.login().await;
    for page in ["review", "rejected", "capability", "installing"] {
        assert_eq!(fresh.get(&format!("{rejected}/{page}")).await.location(), rejected, "{page}");
    }
    assert_eq!(fresh.post(&format!("{rejected}/review"), &[("passphrase", MOCK_PASSPHRASE)]).await.location(), rejected);
}

#[tokio::test]
async fn sources_are_switched_removed_and_a_changed_key_is_decided_by_typing_the_name() {
    let app = App::new();
    let mut browser = app.login().await;

    // Opening a source: its apps, or the decision about its key, as the flow says.
    assert_eq!(browser.get("/sources/of/monokulo").await.location(), "/apps/get?source=monokulo");
    browser.get("/apps/get?source=monokulo").await.assert_has("apps from monokulo match");
    assert_eq!(browser.get("/sources/of/old-tools").await.location(), "/sources/of/old-tools/key");
    assert_eq!(browser.get("/sources/of/monokulo/key").await.location(), "/apps/get?source=monokulo", "no decision is pending there");

    let list = browser.get("/sources").await;
    list.assert_has("Its key changed. Nothing new installs from it until you decide.").assert_has("Couldn't reach it");

    let decision = browser.get("/sources/of/old-tools/key").await;
    decision.assert_has("old-tools is now signed with a different key").assert_has("Key you trust").assert_has("New key").assert_has("Keep the old key");
    let mistyped = browser.post("/sources/of/old-tools/key", &[("typed", "old-tool")]).await;
    assert_eq!(mistyped.status, StatusCode::UNPROCESSABLE_ENTITY);
    mistyped.assert_has("That isn't the source's name. Type old-tools exactly.");
    let trusted = browser.post_then("/sources/of/old-tools/key", &[("typed", "old-tools")]).await;
    trusted.assert_has("old-tools now uses the new key.").assert_lacks("Its key changed.");

    // On and off.
    let off = browser.post_then("/sources/of/community/toggle", &[]).await;
    assert!(off.body.contains(r#"aria-pressed="false""#), "community is off now");
    browser.post_then("/sources/refresh", &[]).await.assert_has("Sources");

    // Removing asks; Cancel leads back; confirming removes.
    let asked = browser.post("/sources/of/community/remove", &[]).await;
    assert_eq!(asked.location(), "/confirm");
    browser.get("/confirm").await.assert_has("Remove community?").assert_has(r#"<a class="btn" href="/sources""#);
    let removed = confirm(&mut browser, "Remove community?").await;
    removed.assert_lacks(r#"id="source-community""#).assert_has(r#"id="source-monokulo""#);
}

#[tokio::test]
async fn removing_a_source_from_its_key_page_cancels_back_to_it_and_confirms_back_to_the_sources() {
    let app = App::new();
    let mut browser = app.login().await;
    browser.get("/sources/of/old-tools/key").await.assert_has("Remove source");
    let mut htmx = browser.clone();
    htmx.htmx = true;
    let dialog = htmx.post("/sources/of/old-tools/key/remove", &[]).await;
    dialog.assert_has("Remove old-tools?").assert_has(r#"href="/sources/of/old-tools/key""#);
    let id = Browser::field(&dialog.body, "id");
    let done = htmx.post("/confirm", &[("id", &id)]).await;
    assert_eq!(done.header("hx-redirect"), "/sources", "htmx is sent to the page, not handed it as a fragment");
    browser.get("/sources").await.assert_lacks(r#"id="source-old-tools""#);
}

#[tokio::test]
async fn logs_are_narrowed_by_the_url_and_followed_by_asking_again() {
    let app = App::new();
    // Left locked, the mock's bell keeps failing, which gives its log problems to list.
    app.backends.stay_locked.store(true, Ordering::SeqCst);
    let mut browser = app.login().await;

    // The device logs: every source to choose from, problems first, followed.
    let device = browser.get("/system/logs").await;
    device.assert_has("Device logs").assert_has(r#"hx-trigger="every 2s""#).assert_has("Following live").assert_has(r#"href="/system/logs/bell?problems=1&amp;follow=1&amp;device=1""#);

    // An app's log, as its page opens it: every line.
    let bell = browser.get("/system/logs/bell").await;
    bell.assert_has("bell logs").assert_has("starting version 1.2.0").assert_has(r#"<span class="e">"#).assert_has("restarted by supervisor");
    bell.assert_has("bell depends on wallet-relay, which is locked.");
    let problems = browser.get("/system/logs/bell?problems=1&follow=0").await;
    problems.assert_has("connection refused").assert_lacks("starting version 1.2.0").assert_lacks("hx-trigger").assert_has("Paused");
    let found = browser.get("/system/logs/bell?find=SUPERVISOR&follow=0").await;
    found.assert_has("restarted by supervisor").assert_lacks("connection refused");
    browser.get("/system/logs/bell?find=nothing-like-this&follow=0").await.assert_has("Nothing in this log yet.");

    // What a followed log asks for every two seconds: the lines alone.
    let mut htmx = browser.clone();
    htmx.htmx = true;
    let lines = htmx.get("/system/logs/bell/lines?problems=0&follow=1").await;
    assert!(lines.body.starts_with("<section"), "{}", lines.body);
    lines.assert_has("starting version 1.2.0").assert_has(r#"hx-get="/system/logs/bell/lines?problems=0&amp;follow=1""#);

    // Saving is the helper's; what it answers is said on the log's page.
    browser.post_then("/system/logs/bell/save", &[]).await.assert_has("class=\"notice");
}

#[tokio::test]
async fn ssh_keys_are_listed_added_and_removed_after_a_question() {
    let app = App::new();
    let mut browser = app.login().await;

    let keys = browser.get("/system/keys").await;
    keys.assert_has("SSH keys").assert_has("laptop").assert_has("SHA256:mockmock");
    let refused = browser.post("/system/keys/add", &[("line", "not a key")]).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    refused.assert_has("the line must start with a key type such as ssh-ed25519").assert_has(">not a key</textarea>");
    let added = browser.post_then("/system/keys/add", &[("line", "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeKeyForTheTests phone")]).await;
    added.assert_has("phone").assert_has("laptop");

    let asked = browser.post("/system/keys/remove", &[("fingerprint", "SHA256:mockmockmockmockmockmockmockmockmockmockmo")]).await;
    assert_eq!(asked.location(), "/confirm");
    let after = confirm(&mut browser, "Remove this SSH key?").await;
    after.assert_has("phone").assert_lacks("SHA256:mockmock");
    // A key that is not listed is not asked about.
    assert_eq!(browser.post("/system/keys/remove", &[("fingerprint", "SHA256:nobody")]).await.location(), "/system/keys");
}

#[tokio::test]
async fn a_missing_drive_takes_the_top_of_home_and_its_check_and_repair_run_as_jobs() {
    let app = App::new();
    app.backends.prepare(|mock| *mock = MockBackend::with_missing_drive());
    let mut browser = app.login().await;

    browser.get("/").await.assert_has("The data drive isn't connected").assert_has(r#"action="/system/drive/retry""#);
    let page = browser.get("/system/drive").await;
    page.assert_has("Data drive").assert_has("Check the drive without changing it").assert_has("Repair the filesystem");

    // Looking again answers at once: still not there, and why.
    browser.post_then("/system/drive/retry", &[]).await.assert_has("Mount failed: structure needs cleaning");

    // The check is a job, and ends in a report to read.
    let started = browser.post("/system/drive/check", &[]).await;
    assert_eq!(started.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/document");
    let report = browser.get("/document").await;
    report.assert_has("Read-only drive check").assert_has("No changes were made.").assert_has(r#"href="/system/drive""#);

    // Repair asks first; confirmed, it runs as a job too.
    assert_eq!(browser.post("/system/drive/repair", &[]).await.location(), "/confirm");
    let question = browser.get("/confirm").await;
    question.assert_has("Repair the filesystem?").assert_has("can lose data");
    let id = Browser::field(&question.body, "id");
    assert_eq!(browser.post("/confirm", &[("id", &id)]).await.location(), "/working");
    assert_eq!(browser.wait_for_job().await.location(), "/document");
    browser.get("/document").await.assert_has("Repair finished");
    browser.get("/system/drive").await.assert_has("The data drive is connected");
    browser.get("/").await.assert_lacks("The data drive isn't connected");
}

#[tokio::test]
async fn system_lists_its_parts_and_shows_the_web_gui_without_a_switch() {
    let app = App::new();
    let mut browser = app.login().await;

    let system = browser.get("/system").await;
    for row in ["Updates", "Backups", "Network", "Device logs", "About this Nodo", "SSH keys", "Data drive"] {
        system.assert_has(row);
    }
    system.assert_has("A Nodo update is ready").assert_has("2 for apps, 1 for the Nodo");
    system.assert_has(r#"href="/system/logs""#).assert_has(r#"href="/system/about""#).assert_has(r#"href="/system/keys""#);
    // Its own state: shown, and no way to change it from here.
    system.assert_has("Web GUI").assert_has("https://nodo.local").assert_has("nodo-tui --ctl web-gui 0|1");
    assert_eq!(system.body.matches("<form").count(), 1, "the only form on System is Log out");

    let about = browser.get("/system/about").await;
    about.assert_has("About this Nodo").assert_has("412 GB of 1.9 TB");
    browser.get("/web").await.assert_has("Web").assert_has("moneropay");
}

#[tokio::test]
async fn a_job_outlives_its_request_and_other_pages_say_what_is_running() {
    let app = App::new();
    // Every call to the helper takes a while, so the install is still running when asked about.
    app.backends.prepare(|mock| mock.knobs.delay = Some(Duration::from_millis(400)));
    let mut browser = app.login().await;
    browser.post("/apps/get/official/moneropay/install", &[]).await;
    let started = browser.post("/apps/get/official/moneropay/review", &[("passphrase", MOCK_PASSPHRASE)]).await;
    // The request that started it is answered at once.
    assert_eq!(started.location(), "/working");
    let session = app.session(&browser).await;
    assert!(session.running_job(app.state.clock()).is_some());

    // The page that waits, and the same as the fragment htmx asks for.
    app.clock.advance(Duration::from_secs(7));
    let waiting = browser.get("/working").await;
    assert_eq!(waiting.status, StatusCode::OK);
    waiting.assert_has("Installing moneropay").assert_has("Working for 7 seconds.").assert_has(r#"hx-get="/working" hx-trigger="every 1s""#);
    waiting.assert_has(r#"<noscript><meta http-equiv="refresh" content="2"></noscript>"#);
    let mut htmx = browser.clone();
    htmx.htmx = true;
    let fragment = htmx.get("/working").await;
    assert!(fragment.body.starts_with("<section"), "{}", fragment.body);

    // Another page of the same session does not wait for the backend: it says what is running,
    // and comes back to itself when that is over.
    let apps = browser.get("/apps").await;
    assert_eq!(apps.status, StatusCode::OK);
    apps.assert_has("Installing moneropay").assert_has(r#"hx-get="/working?to=%2Fapps""#).assert_lacks("<h1>Installed</h1>");
    // An action is refused with a remark, not queued behind the job.
    let refused = browser.post("/apps/tor/restart", &[]).await;
    assert_eq!(refused.location(), "/apps/tor");
    // A fragment of a page that is still open keeps what it shows.
    assert_eq!(htmx.get("/system/logs/bell/lines?follow=1").await.status, StatusCode::NO_CONTENT);
    // Another session is not held up at all.
    let mut other = app.login().await;
    other.get("/apps").await.assert_has("<h1>Installed</h1>");

    // Over: the waiting page leads to the install's own page.
    let finished = browser.wait_for_job().await;
    assert_eq!(finished.location(), "/apps/get/official/moneropay/installing");
    let installed = browser.get(finished.location()).await;
    installed.assert_has("moneropay is installed").assert_has("The Nodo is busy: Installing moneropay. Try again once that is done.");
    assert!(session.running_job(app.state.clock()).is_none());
}

#[tokio::test]
async fn a_job_that_ends_while_the_owner_is_elsewhere_is_told_on_the_next_page() {
    let app = App::new();
    let mut browser = app.login().await;
    browser.post("/apps/get/official/moneropay/install", &[]).await;
    assert_eq!(browser.post("/apps/get/official/moneropay/review", &[("passphrase", MOCK_PASSPHRASE)]).await.location(), "/working");
    let session = app.session(&browser).await;
    while session.running_job(app.state.clock()).is_some() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // A page that was waiting returns to itself (`to`), and says how the install ended in the
    // flow's words for someone who is not on the install's page.
    let mut htmx = browser.clone();
    htmx.htmx = true;
    assert_eq!(htmx.get("/working?to=%2Fapps").await.header("hx-redirect"), "/apps");
    let apps = browser.get("/apps").await;
    apps.assert_has("moneropay is installed. Open it from Apps to set it up.").assert_has("<h1>Installed</h1>");
    // Told once, and the install's page can still be opened.
    browser.get("/apps").await.assert_lacks("moneropay is installed.");
    browser.get("/apps/get/official/moneropay/installing").await.assert_has("moneropay is installed");
    // Nothing is left to wait for.
    assert_eq!(browser.get("/working").await.location(), "/");
    // Only a path of this origin is ever followed.
    assert_eq!(browser.get("/working?to=https%3A%2F%2Fevil.example%2F").await.location(), "/");
    assert_eq!(browser.get("/working?to=%2F%2Fevil.example").await.location(), "/");
}
