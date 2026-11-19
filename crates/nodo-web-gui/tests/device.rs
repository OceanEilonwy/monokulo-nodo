//! Backups, the network, bundles, apps' setup pages, the unlock page and stepped installs,
//! through the router.

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use common::{Answer, App, Browser};
use nodo_ui_core::flows::backup::RESTORE_WORD;
use nodo_ui_core::mock::{sample_bundle_review, sample_owner_page, MockBackend, MOCK_PASSPHRASE};

const BACKUPS: &str = "/system/backups";
const NETWORK: &str = "/system/network";
const BUNDLE: &str = "/apps/get/bundle";

async fn confirm(browser: &mut Browser<'_>, title: &str) -> Answer {
    let question = browser.get("/confirm").await;
    question.assert_has(title);
    let id = Browser::field(&question.body, "id");
    browser.post_then("/confirm", &[("id", &id)]).await
}

/// Posts, waits for the job it started, and loads the page the job leads to.
async fn job(browser: &mut Browser<'_>, path: &str, fields: &[(&str, &str)]) -> Answer {
    let started = browser.post(path, fields).await;
    assert_eq!(started.location(), "/working", "{}", started.body);
    let done = browser.wait_for_job().await;
    browser.get(done.location()).await
}

#[tokio::test]
async fn a_backup_is_made_with_its_own_passphrase_typed_twice() {
    let app = App::new();
    let mut browser = app.login().await;
    browser.get("/system").await.assert_has(r#"<a class="row" href="/system/backups">"#);
    let list = browser.get(BACKUPS).await;
    list.assert_has("<h1>Backups</h1>").assert_has("nodo-1762560000.nodobak").assert_has("USB drive").assert_has("Make a backup on the USB drive");

    assert_eq!(browser.post(BACKUPS, &[("do", "create")]).await.location(), BACKUPS);
    let form = browser.get(BACKUPS).await;
    // A device that is set up is asked for its passphrase too, so the backup holds its key.
    form.assert_has("New backup on this device").assert_has(r#"name="device""#).assert_has(r#"name="first""#).assert_has(r#"name="second""#);

    // Refused, with the reason and never with what was typed.
    for (fields, problem) in [
        ([("device", MOCK_PASSPHRASE), ("first", "too-short"), ("second", "too-short")], "A backup passphrase needs at least 12 characters."),
        ([("device", MOCK_PASSPHRASE), ("first", "a long passphrase one"), ("second", "a long passphrase two")], "The two backup passphrases differ. Type them again."),
        ([("device", ""), ("first", "a long passphrase one"), ("second", "a long passphrase one")], "Enter the device passphrase, so the backup includes the device key."),
    ] {
        let mut posted = fields.to_vec();
        posted.push(("do", "make"));
        let refused = browser.post(BACKUPS, &posted).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
        refused.assert_has(problem).assert_lacks("a long passphrase").assert_lacks("too-short");
    }

    // Made, as a job.
    let made = job(&mut browser, BACKUPS, &[("do", "make"), ("device", MOCK_PASSPHRASE), ("first", "a long passphrase one"), ("second", "a long passphrase one")]).await;
    made.assert_has("Backup nodo-1762646400.nodobak written.").assert_has("<h1>Backups</h1>").assert_lacks("a long passphrase");

    // On the USB drive, and giving it up.
    browser.post(BACKUPS, &[("do", "create-usb")]).await;
    browser.get(BACKUPS).await.assert_has("New backup on the USB drive");
    browser.post(BACKUPS, &[("do", "cancel")]).await;
    browser.get(BACKUPS).await.assert_has("<h1>Backups</h1>");
}

#[tokio::test]
async fn a_restore_shows_what_the_backup_holds_and_needs_the_word() {
    let app = App::new();
    let mut browser = app.login().await;
    browser.get(BACKUPS).await;

    // A name that is not on the list is not opened, whatever a browser posts.
    browser.post(BACKUPS, &[("do", "open"), ("name", "../../etc/shadow"), ("usb", "0")]).await;
    browser.get(BACKUPS).await.assert_has("<h1>Backups</h1>").assert_lacks("Open this backup");

    browser.post(BACKUPS, &[("do", "open"), ("name", "nodo-1762560000.nodobak"), ("usb", "0")]).await;
    browser.get(BACKUPS).await.assert_has("Open this backup").assert_has("nodo-1762560000.nodobak");

    // A wrong passphrase opens nothing, and says so.
    let wrong = job(&mut browser, BACKUPS, &[("do", "inspect"), ("passphrase", "not the passphrase")]).await;
    wrong.assert_has("the passphrase did not open this backup").assert_has("Open this backup").assert_lacks("not the passphrase");

    // The right one: what the backup holds and what stops, before anything changes.
    let contents = job(&mut browser, BACKUPS, &[("do", "inspect"), ("passphrase", MOCK_PASSPHRASE)]).await;
    contents.assert_has("Restore from this backup?").assert_has("This overwrites what is on the device now.").assert_has("cuprated, moneropay").assert_has("wallet-relay");
    contents.assert_has(&format!("Type {RESTORE_WORD} to confirm")).assert_has("New device passphrase");
    // The backup's passphrase is held on the Nodo. Nothing of it is on the page.
    contents.assert_lacks(&format!(r#"value="{MOCK_PASSPHRASE}""#)).assert_lacks("type=\"hidden\"");

    // Without the word, or without a device passphrase for the backup's key, nothing is restored.
    let no_word = browser.post(BACKUPS, &[("do", "restore"), ("typed", "yes"), ("device", "a new device passphrase")]).await;
    assert_eq!(no_word.status, StatusCode::UNPROCESSABLE_ENTITY);
    no_word.assert_has(&format!("Type {RESTORE_WORD} to confirm."));
    let short = browser.post(BACKUPS, &[("do", "restore"), ("typed", RESTORE_WORD), ("device", "short")]).await;
    assert_eq!(short.status, StatusCode::UNPROCESSABLE_ENTITY);
    short.assert_has("Choose a new device passphrase of at least 8 characters");

    // Restored. A name, a target or a passphrase sent beside the word is not read: the backup is
    // the one that was opened.
    let forged = [("do", "restore"), ("typed", RESTORE_WORD), ("device", "a new device passphrase"), ("name", "another.nodobak"), ("target", "sdz9"), ("usb", "1"), ("passphrase", "another")];
    let restored = job(&mut browser, BACKUPS, &forged).await;
    restored.assert_has("Restored").assert_has("Done").assert_lacks("a new device passphrase").assert_lacks("another.nodobak");
    browser.post(BACKUPS, &[("do", "finish")]).await;
    browser.get(BACKUPS).await.assert_has("<h1>Backups</h1>").assert_has("Make a backup now");
}

#[tokio::test]
async fn wifi_networks_are_joined_by_their_kind_and_forgotten_after_a_question() {
    let app = App::new();
    let mut browser = app.login().await;
    browser.get("/system").await.assert_has(r#"<a class="row" href="/system/network">"#);
    let page = browser.get(NETWORK).await;
    page.assert_has("<h1>Network</h1>").assert_has("Joined to <strong>home-5G</strong>").assert_has("192.168.1.57").assert_has("travel-hotspot").assert_has("The cable is connected too.");
    page.assert_lacks("cuts this page off");

    let scanned = browser.post_then(NETWORK, &[("do", "scan-wifi")]).await;
    scanned.assert_has("Cafe Libre").assert_has("open, not encrypted").assert_has("neighbour").assert_has("office-staff").assert_has("WPA Enterprise");

    // A network with a passphrase: asked for on the page, and only the passphrase is posted.
    let prompt = browser.post_then(NETWORK, &[("do", "join"), ("ssid", "neighbour")]).await;
    prompt.assert_has("Join neighbour").assert_has(r#"name="passphrase""#);
    browser.post_then(NETWORK, &[("do", "passphrase"), ("passphrase", ""), ("ssid", "home-5G")]).await.assert_has("Type the passphrase of neighbour.");
    let joined = browser.post_then(NETWORK, &[("do", "passphrase"), ("passphrase", "not ours at all"), ("ssid", "Cafe Libre")]).await;
    joined.assert_has("Joined neighbour.").assert_has("Joined to <strong>neighbour</strong>").assert_lacks("not ours at all");

    // One that this device cannot join says why.
    browser.post_then(NETWORK, &[("do", "join"), ("ssid", "office-staff")]).await.assert_has("needs a per-user login (WPA Enterprise)");
    // One that is not in range or saved is not joined.
    browser.post_then(NETWORK, &[("do", "join"), ("ssid", "made-up")]).await.assert_lacks("Join made-up");

    // An open network asks first.
    assert_eq!(browser.post(NETWORK, &[("do", "join"), ("ssid", "Cafe Libre")]).await.location(), "/confirm");
    let open = confirm(&mut browser, "Join Cafe Libre, an open network?").await;
    open.assert_has("Joined to <strong>Cafe Libre</strong>");

    // Forgetting asks, and says what was forgotten.
    assert_eq!(browser.post(NETWORK, &[("do", "forget"), ("ssid", "travel-hotspot")]).await.location(), "/confirm");
    let forgotten = confirm(&mut browser, "Forget travel-hotspot?").await;
    forgotten.assert_has("Forgot travel-hotspot.");
    assert!(!forgotten.body.contains(r#"<span class="name">travel-hotspot</span>"#));

    // With the cable in, Wi-Fi goes off without a question.
    let off = browser.post_then(NETWORK, &[("do", "wifi")]).await;
    off.assert_has("○ Off");
}

#[tokio::test]
async fn turning_off_the_wifi_the_browser_may_be_on_warns_and_asks() {
    let app = App::new();
    app.backends.prepare(|mock| mock.wifi.wired_up = false);
    let mut browser = app.login().await;
    let page = browser.get(NETWORK).await;
    page.assert_has("The cable is not connected.").assert_has("If your browser reaches this Nodo over home-5G, turning Wi-Fi off or forgetting that network cuts this page off.");
    // The question is the flow's.
    assert_eq!(browser.post(NETWORK, &[("do", "wifi")]).await.location(), "/confirm");
    let question = browser.get("/confirm").await;
    question.assert_has("Turn Wi-Fi off?").assert_has("The wired link is down, so this may cut the connection you are using.");
    browser.get(NETWORK).await.assert_has("● On");
    let off = confirm(&mut browser, "Turn Wi-Fi off?").await;
    off.assert_has("○ Off");
    // Forgetting the network in use says the same.
    let mut other = app.login().await;
    other.get(NETWORK).await;
    other.post(NETWORK, &[("do", "forget"), ("ssid", "home-5G")]).await;
    other.get("/confirm").await.assert_has("Forget home-5G?").assert_has("this may cut the connection you are using");
}

#[tokio::test]
async fn bluetooth_devices_are_paired_and_removed_and_the_radios_are_polled_quietly() {
    let app = App::new();
    let mut browser = app.login().await;
    browser.get(NETWORK).await.assert_has("K380 Keyboard").assert_has("WH-1000XM4").assert_lacks("Kitchen Speaker");
    let found = browser.post_then(NETWORK, &[("do", "scan-bluetooth")]).await;
    found.assert_has("Kitchen Speaker").assert_has(">Pair<");
    let paired = browser.post_then(NETWORK, &[("do", "device"), ("address", "AA:BB:CC:00:00:03")]).await;
    paired.assert_has("Paired with Kitchen Speaker. It is trusted and connected.");
    // A device that is not listed is not asked for.
    browser.post_then(NETWORK, &[("do", "device"), ("address", "FF:FF:FF:FF:FF:FF")]).await.assert_has("Put the device in its pairing mode first.");

    assert_eq!(browser.post(NETWORK, &[("do", "remove-device"), ("address", "AA:BB:CC:00:00:03")]).await.location(), "/confirm");
    let removed = confirm(&mut browser, "Remove Kitchen Speaker?").await;
    assert!(!removed.body.contains("AA:BB:CC:00:00:03"), "{}", removed.body);
    let off = browser.post_then(NETWORK, &[("do", "bluetooth")]).await;
    assert_eq!(off.body.matches("○ Off").count(), 1, "Bluetooth is off, Wi-Fi is not");

    // What the page asks by itself does not keep the session alive.
    let mut htmx = browser.clone();
    htmx.htmx = true;
    app.clock.advance(Duration::from_secs(14 * 60));
    let status = htmx.get("/system/network/status").await;
    assert_eq!(status.status, StatusCode::OK);
    assert!(status.body.starts_with(r#"<div class="stack" id="radios""#), "{}", status.body);
    app.clock.advance(Duration::from_secs(2 * 60));
    assert_eq!(browser.get(NETWORK).await.location(), "/login");
}

#[tokio::test]
async fn a_bundle_is_reviewed_answered_and_installed_and_a_secret_answer_never_comes_back() {
    let app = App::new();
    app.backends.prepare(|mock| {
        mock.bundles.insert("sample-tool-01".to_owned(), sample_bundle_review());
    });
    let mut browser = app.login().await;
    const SECRET: &str = "firebase-SECRET-do-not-echo";
    let mut seen: Vec<String> = Vec::new();
    // Nothing to install yet.
    assert_eq!(browser.get(BUNDLE).await.location(), "/apps/get");

    // The gate answers with the bundle's review.
    let gate = browser.post("/apps/get/official/sample-tool-01/install", &[]).await;
    assert_eq!(gate.location(), BUNDLE);
    let review = browser.get(BUNDLE).await;
    review.assert_has("Install pingers and its apps?").assert_has("pingers-push-relay").assert_has("Data drive").assert_has("pingers-map-host");
    review.assert_has("keeps its data on the data drive");
    browser.post_then(BUNDLE, &[("drive", "pingers-map-host")]).await.assert_has("keeps its data on the Nodo's own disk");
    seen.push(review.body);

    // The settings it asks for once. The secret one is a password box, always empty.
    let answers = browser.post_then(BUNDLE, &[("do", "proceed")]).await;
    answers.assert_has("A few settings first").assert_has("Your domain").assert_has("Firebase service account").assert_has(r#"<input type="password" id="a.1" name="a.1""#);
    let missing = browser.post(BUNDLE, &[("do", "answers"), ("a.0", "example.com"), ("a.1", "")]).await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    missing.assert_has("Fill in Firebase service account first.").assert_has(r#"name="a.0" value="example.com""#);
    let other = browser.post(BUNDLE, &[("do", "answers"), ("a.0", ""), ("a.1", SECRET)]).await;
    other.assert_has("Fill in Your domain first.");
    seen.push(other.body);

    // Answered: the device passphrase, for the new locked storage and the secret.
    let passphrase = browser.post_then(BUNDLE, &[("do", "answers"), ("a.0", "example.com"), ("a.1", SECRET)]).await;
    passphrase.assert_has("Device passphrase").assert_has(r#"autocomplete="current-password""#);
    seen.push(passphrase.body);
    browser.post_then(BUNDLE, &[("do", "passphrase"), ("passphrase", "")]).await.assert_has("Type the device passphrase.");
    // A wrong passphrase installs nothing and asks again.
    let wrong = job(&mut browser, BUNDLE, &[("do", "passphrase"), ("passphrase", "wrong")]).await;
    wrong.assert_has("That passphrase did not unlock this Nodo. Nothing was installed.").assert_has("Device passphrase");
    seen.push(wrong.body);

    // Installed, as a job, from the review kept on the Nodo: a review a browser sends is not read.
    let done = job(&mut browser, BUNDLE, &[("do", "passphrase"), ("passphrase", MOCK_PASSPHRASE), ("review", "{\"package\":\"evil\"}"), ("package", "evil")]).await;
    done.assert_has("pingers and its apps are installed").assert_has("pingers-push-relay").assert_lacks("evil");
    seen.push(done.body);
    // Done: on to the head's setup page when it has one. The mock's has none, and says so.
    let after = browser.post_then(BUNDLE, &[("do", "finish")]).await;
    after.assert_has("pingers has no setup page.");
    seen.push(after.body);
    assert_eq!(browser.get(BUNDLE).await.location(), "/apps/get", "the flow is done with");

    for body in &seen {
        assert!(!body.contains(SECRET), "a secret answer came back in:\n{body}");
        assert!(!body.contains("firebase-SECRET"));
    }
}

#[tokio::test]
async fn an_apps_setup_page_opens_with_the_passphrase_which_never_leaves_the_nodo() {
    let app = App::new();
    app.backends.prepare(|mock| {
        mock.owner_views.insert("moneropay".to_owned(), ("Setup".to_owned(), sample_owner_page()));
    });
    let mut browser = app.login().await;
    let owner = "/apps/moneropay/owner";
    browser.get("/apps/moneropay").await.assert_has(r#"<a class="row" href="/apps/moneropay/owner">"#).assert_has("Setup");
    // An app without one says so.
    assert_eq!(browser.get("/apps/tor/owner").await.location(), "/apps/tor");
    browser.get("/apps/tor").await.assert_has("tor has no setup page.");

    // The gate.
    assert_eq!(browser.get(owner).await.location(), owner);
    let gate = browser.get(owner).await;
    gate.assert_has("opens with the device passphrase").assert_has(r#"autocomplete="current-password""#).assert_lacks("Pingers setup");
    let wrong = browser.post(owner, &[("passphrase", "not it")]).await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    wrong.assert_has("That passphrase did not unlock this Nodo.").assert_lacks("not it");
    browser.post(owner, &[("passphrase", "")]).await.assert_has("Type the device passphrase.");

    // Open: the checklist with its steps, values to copy, and QR codes drawn here.
    assert_eq!(browser.post(owner, &[("passphrase", MOCK_PASSPHRASE)]).await.location(), owner);
    let page = browser.get(owner).await;
    page.assert_has("Pingers setup").assert_has("1. ").assert_has("Put the sites online").assert_has("Read it again").assert_has(r#"class="btn link copy" type="button" data-copy=""#);
    page.assert_lacks(r#"type="password""#);
    if sample_owner_page().steps.iter().any(|step| step.items.iter().any(|item| matches!(item, nodo_api::OwnerItem::Qr { .. }))) {
        page.assert_has(r#"<svg class="qr""#);
    }
    // Read again with the passphrase kept on the Nodo: the browser sends none.
    assert_eq!(browser.post(&format!("{owner}/refresh"), &[]).await.location(), owner);
    browser.get(owner).await.assert_has("Pingers setup");

    // Closed: the flow and the passphrase it held are dropped, and the gate is back.
    assert_eq!(browser.post(&format!("{owner}/close"), &[]).await.location(), "/apps/moneropay");
    assert!(app.session(&browser).await.inner().stash.owner.is_empty());
    browser.get(owner).await;
    browser.get(owner).await.assert_has("opens with the device passphrase");

    // Open again, then the session ends: nothing of it is left in the process.
    browser.post(owner, &[("passphrase", MOCK_PASSPHRASE)]).await;
    let session = app.session(&browser).await;
    assert!(session.inner().stash.owner.get("moneropay").is_some_and(|page| page.held.is_some()));
    let weak = Arc::downgrade(&session);
    drop(session);
    browser.post("/logout", &[]).await;
    assert!(weak.upgrade().is_none(), "the session, and the passphrase its page held, are freed");
}

#[tokio::test]
async fn the_unlock_page_says_why_counts_the_wait_and_returns_to_the_page_that_sent_the_owner() {
    let app = App::new();
    app.backends.stay_locked.store(true, Ordering::SeqCst);
    let mut browser = app.login().await;
    browser.post("/web/providers", &[("do", "add")]).await;
    browser.post("/web/providers", &[("choose", "Porkbun")]).await;
    // Connecting stores a key sealed with the device passphrase: unlock first.
    let sent = browser.post("/web/providers", &[("do", "connect"), ("api_key", "pk1_abc"), ("secret_key", "sk1_def")]).await;
    assert_eq!(sent.location(), "/unlock");
    let page = browser.get("/unlock").await;
    page.assert_has("Unlock your Nodo").assert_has("Connecting Porkbun stores its key locked with the device passphrase. Type it to unlock the Nodo and carry on.");
    page.assert_has(r#"href="/web/providers""#).assert_lacks("pk1_abc");

    // A wrong try: said on this page, with the wait the flow sets.
    let wrong = browser.post("/unlock", &[("from", "page"), ("passphrase", "wrong")]).await;
    assert_eq!(wrong.location(), "/unlock");
    let waiting = browser.get("/unlock").await;
    waiting.assert_has("That passphrase did not unlock this device. Try again.").assert_has("You can try again in 2 seconds.");
    browser.post("/unlock", &[("from", "page"), ("passphrase", MOCK_PASSPHRASE)]).await;
    browser.get("/unlock").await.assert_has("Please wait 2 seconds before trying again.");
    app.clock.advance(Duration::from_secs(1));
    browser.get("/unlock").await.assert_has("You can try again in 1 second.");
    app.clock.advance(Duration::from_secs(1));
    browser.get("/unlock").await.assert_lacks("You can try again");

    // Unlocked: back on the page that sent the owner, which still shows the key step.
    let unlocked = browser.post("/unlock", &[("from", "page"), ("passphrase", MOCK_PASSPHRASE)]).await;
    assert_eq!(unlocked.location(), "/web/providers");
    browser.get("/web/providers").await.assert_has("Unlocked.").assert_has("Connect Porkbun");
    // Nothing left to unlock: the page says so and leads on.
    assert_eq!(browser.get("/unlock").await.location(), "/");
    browser.get("/").await.assert_has("This device is already unlocked.");
}

#[tokio::test]
async fn an_install_shows_its_steps_and_can_be_cancelled_where_the_backend_can() {
    let app = App::new();
    app.backends.prepare(|mock| {
        mock.knobs.capabilities.install_progress = true;
        mock.knobs.capabilities.cancel_install = true;
    });
    let mut browser = app.login().await;
    let offer = "/apps/get/official/moneropay";
    browser.post(&format!("{offer}/install"), &[]).await;
    // The backend answers at once with an install under way, and its steps.
    let page = job(&mut browser, &format!("{offer}/review"), &[("passphrase", MOCK_PASSPHRASE)]).await;
    page.assert_has("Installing moneropay").assert_has(r#"hx-get="/apps/get/official/moneropay/installing/status" hx-trigger="every 1s""#).assert_has("Cancel the install");
    assert!(page.body.contains("<span class=\"warn\" aria-hidden=\"true\">›</span>"), "a step is being worked on");

    // The page asks how it is going until it is over.
    let mut htmx = browser.clone();
    htmx.htmx = true;
    let mut last = String::new();
    for _ in 0..200 {
        last = htmx.get(&format!("{offer}/installing/status")).await.body;
        if !last.contains("hx-get") {
            break;
        }
    }
    assert!(last.starts_with("<section") && last.contains("moneropay is installed") && last.contains("Set it up"), "{last}");
    browser.get(&format!("{offer}/installing")).await.assert_has("moneropay is installed").assert_lacks("hx-trigger");

    // Another one, cancelled while it runs: what it added is removed, and the page is left.
    let other = "/apps/get/official/wallet-rpc";
    browser.post(&format!("{other}/install"), &[]).await;
    job(&mut browser, &format!("{other}/review"), &[("passphrase", MOCK_PASSPHRASE)]).await.assert_has("Cancel the install");
    let cancelled = browser.post_then(&format!("{other}/installing/leave"), &[]).await;
    cancelled.assert_has("Install cancelled. What it added was removed.");
}

#[tokio::test]
async fn an_installed_app_on_offer_leads_to_its_page_and_its_hosting() {
    let app = App::new();
    let mut browser = app.login().await;
    let feed = MockBackend::new().catalog.iter().find(|pkg| pkg.id.name == "monokulo-pay").map(|pkg| pkg.id.feed.clone()).expect("on offer");
    let detail = browser.get(&format!("/apps/get/{feed}/monokulo-pay")).await;
    detail.assert_has("Open its page").assert_has(r#"href="/apps/monokulo-pay/hosting""#);
    // One that is not installed has neither.
    browser.get("/apps/get/official/moneropay").await.assert_lacks("/hosting");
}
