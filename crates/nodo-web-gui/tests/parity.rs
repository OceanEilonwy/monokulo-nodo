//! Parity with the terminal interface: every screen it has is a page here, or is on the list of
//! exceptions with the reason (design, "Parity with the TUI" and its review resolutions).

mod common;

use axum::http::StatusCode;
use common::App;
use nodo_ui_core::screen::ScreenId;

/// Where a screen of the terminal interface is here, or why it is nowhere. The match is
/// exhaustive, so a screen the core gains does not compile until it is placed.
fn place(screen: ScreenId) -> Result<&'static str, &'static str> {
    Ok(match screen {
        ScreenId::Main => "/",
        ScreenId::Search => "/apps/get",
        ScreenId::Detail => "/apps/get/official/moneropay",
        ScreenId::Review => "/apps/get/official/moneropay/review",
        ScreenId::Installing => "/apps/get/official/moneropay/installing",
        // Logging in unlocks the device; this page is for what is still locked afterwards.
        ScreenId::Unlock => "/unlock",
        ScreenId::Feeds => "/sources",
        ScreenId::AddFeed => "/sources/add",
        ScreenId::FeedKey => "/sources/of/old-tools/key",
        ScreenId::Settings => "/apps/moneropay/settings",
        ScreenId::Reference => "/apps/moneropay/settings/reference",
        ScreenId::ApplyFailed => "/apps/moneropay/settings/failed",
        ScreenId::Updates => "/system/updates",
        ScreenId::UpdateFailed => "/system/updates/results",
        ScreenId::Logs => "/system/logs",
        ScreenId::Rejected => "/apps/get/community/fast-sync-helper/rejected",
        ScreenId::Capability => "/apps/get/official/zmq-indexer/capability",
        ScreenId::NoDrive => "/system/drive",
        ScreenId::WebSites => "/web",
        ScreenId::WebExposure => "/apps/moneropay/hosting",
        ScreenId::WebProviders => "/web/providers",
        ScreenId::WebCheck => "/apps/moneropay/hosting/check",
        ScreenId::WebConfirm => "/apps/moneropay/hosting/confirm",
        ScreenId::WebCgnat => "/apps/moneropay/hosting/cgnat",
        ScreenId::AutoUpdate => "/system/auto-update",
        ScreenId::WebDomain => "/web/domain",
        ScreenId::WebCertificate => "/apps/shop/hosting/certificate",
        ScreenId::WebTrust => "/web/trust",
        ScreenId::Backup => "/system/backups",
        ScreenId::Onion => "/system/onion",
        ScreenId::Rollbacks => "/system/undo",
        ScreenId::Network => "/system/network",
        ScreenId::Apps => "/apps",
        ScreenId::AppPage => "/apps/moneropay",
        // The question before an app is removed: a dialog over its page, or this page.
        ScreenId::AppRemove => "/confirm",
        ScreenId::System => "/system",
        ScreenId::Bundle => "/apps/get/bundle",
        ScreenId::OwnerPage => "/apps/moneropay/owner",
        // What it is and whether it is on, without the switch: the helper refuses it from here.
        ScreenId::WebGui => "/system",
        ScreenId::Restore => return Err("first boot only: the web GUI cannot be turned on before the device is set up. Restoring onto a set-up device is on the Backups page"),
        ScreenId::Setup => return Err("its passphrase steps are first boot only; its SSH keys step is /system/keys, checked on its own below"),
    })
}

/// What the terminal interface has that is no `ScreenId`, and why it has no page here.
const NOT_SCREENS: [(&str, &str); 5] = [
    ("The help overlay and the key hints", "a browser has no keys to explain"),
    ("Terminal too small", "a page reflows instead"),
    ("Copying through OSC 52", "a Copy button and the clipboard instead"),
    ("Shell", "a stub in the terminal interface; SSH is the shell"),
    ("Turning the web GUI on or off", "shown on /system, changed only over SSH: the helper refuses it from this socket"),
];

#[tokio::test]
async fn every_screen_of_the_terminal_interface_has_a_page_or_a_reason() {
    let app = App::new();
    let mut browser = app.login().await;
    let mut exceptions = Vec::new();
    for screen in ScreenId::ALL {
        match place(screen) {
            Ok(path) => {
                // Served: the page itself, or a redirect to where its way starts when nothing is
                // kept for it yet (a review before the gate, a check before a save).
                let answer = browser.get(path).await;
                assert!(
                    matches!(answer.status, StatusCode::OK | StatusCode::SEE_OTHER),
                    "{} ({path}) answered {}",
                    screen.slug(),
                    answer.status
                );
                assert_ne!(answer.location(), "/login", "{path}");
            }
            Err(reason) => exceptions.push((screen.slug(), reason)),
        }
    }
    let named: Vec<&str> = exceptions.iter().map(|(slug, _)| *slug).collect();
    assert_eq!(named, ["restore"], "the only screen in the list without a page");
    // Setup is not in `ALL`: its keys step has its own page, its passphrase steps none.
    assert!(place(ScreenId::Setup).is_err());
    browser.get("/system/keys").await.assert_has("<h1>SSH keys</h1>");
    assert!(NOT_SCREENS.iter().all(|(what, why)| !what.is_empty() && !why.is_empty()));
    // The web GUI's own switch: shown, not offered.
    let system = browser.get("/system").await;
    system.assert_has("Web GUI").assert_has("nodo-tui --ctl web-gui 0|1").assert_lacks("set-web-gui");
    // What it is, as the terminal's page says it, and no switch.
    system.assert_has("The same pages as here, in a browser on your home network.").assert_has("can try the device passphrase");
    system.assert_has("System › Web GUI").assert_lacks("Turn it off").assert_lacks("Turn it on");
}

/// Pages that are no `ScreenId` of their own, walked as well.
const MORE_PAGES: [&str; 13] = [
    "/system/keys",
    "/system/about",
    "/system/logs/moneropay",
    "/system/logs/bell",
    "/system/auto-update/history",
    "/web/certificates",
    "/apps/tor",
    "/apps/wallet-relay",
    "/apps/get/official/moneropay/doc/changelog",
    "/apps/get/official/moneropay/doc/versions",
    "/system/updates/review",
    "/system/updates/passphrase",
    "/sources/of/official",
];

/// The core's words name no key; where they point at something to do they carry a `Hint`, which
/// is a link or a button here (`hints`). So no page may say "press o" or "Press s to …".
#[tokio::test]
async fn no_page_names_a_key_of_the_terminal() {
    #[track_caller]
    fn check(path: &str, answer: &common::Answer) {
        // An app's setup page shows what the package wrote. The mock's sample says "when you
        // press r", which is the package's content and not this interface's: it is taken out
        // before looking.
        let own = answer.body.replace("Each step checks itself when you press r.", "");
        let keys = common::keys_named(&own);
        assert!(keys.is_empty(), "{path} names a key: {keys:?}\n{}", answer.body);
    }
    assert_eq!(common::keys_named("Turn it on first: press o. Press s to look. Press save, or press 2."), ["Press s to look. Press save, or press 2.", "press o. Press s to look. Press save, or"]);
    assert_eq!(common::keys_named("Press `l` to read its log."), ["Press l to read its log."]);

    // A device that stayed locked, with a suite to install and an app's setup page.
    let app = App::new();
    app.backends.stay_locked.store(true, std::sync::atomic::Ordering::SeqCst);
    app.backends.prepare(|mock| {
        mock.bundles.insert("sample-tool-01".to_owned(), nodo_ui_core::mock::sample_bundle_review());
        mock.owner_views.insert("moneropay".to_owned(), ("Setup".to_owned(), nodo_ui_core::mock::sample_owner_page()));
    });
    let mut browser = app.login().await;
    let mut walked = 0;
    let paths: Vec<&str> = ScreenId::ALL.into_iter().filter_map(|screen| place(screen).ok()).chain(MORE_PAGES).collect();
    for path in &paths {
        let mut answer = browser.get(path).await;
        // A page with nothing kept for it yet leads to where its way starts; that page is read.
        for _ in 0..3 {
            if answer.status == StatusCode::SEE_OTHER {
                let next = answer.location().to_owned();
                answer = browser.get(&next).await;
            }
        }
        assert_eq!(answer.status, StatusCode::OK, "{path}");
        check(path, &answer);
        walked += 1;
    }
    assert!(walked >= 50, "{walked} pages");

    // The messages that point at something to do, each on the page that shows it.
    // A locked app: the refusal links to the unlock page.
    let locked = browser.post_then("/apps/wallet-relay/toggle", &[]).await;
    locked.assert_has("wallet-relay is locked.").assert_has(r#"<a href="/unlock">Unlock the Nodo</a>"#);
    check("a locked app", &locked);
    // The setup page, opened, with the package's own words.
    browser.post("/apps/moneropay/owner", &[("passphrase", nodo_ui_core::mock::MOCK_PASSPHRASE)]).await;
    let owner = browser.get("/apps/moneropay/owner").await;
    owner.assert_has("Pingers setup");
    check("an app's setup page", &owner);
    // A log whose advice points at unlocking: a link, where the terminal names a key.
    let log = browser.get("/system/logs/bell").await;
    log.assert_has(r#"Unlock the device. bell will start automatically. <a href="/unlock">Unlock the Nodo</a>"#);
    check("a log's advice", &log);
    // The suite's review.
    browser.post("/apps/get/official/sample-tool-01/install", &[]).await;
    check("a suite", &browser.get("/apps/get/bundle").await);
    // The radios: a network and a device that are not listed, then both radios off.
    let network = "/system/network";
    browser.get(network).await;
    let no_network = browser.post_then(network, &[("do", "join"), ("ssid", "made-up")]).await;
    no_network.assert_has("Look for networks first.");
    assert_eq!(no_network.body.matches(r#"value="scan-wifi">Look for networks</button>"#).count(), 2, "{}", no_network.body);
    check("no such network", &no_network);
    check("no such device", &browser.post_then(network, &[("do", "device"), ("address", "FF:FF:FF:FF:FF:FF")]).await);
    browser.post_then(network, &[("do", "wifi")]).await;
    browser.post_then(network, &[("do", "bluetooth")]).await;
    let wifi_off = browser.post_then(network, &[("do", "join"), ("ssid", "home-5G")]).await;
    wifi_off.assert_has("Wi-Fi is off.").assert_has(r#"<button class="btn" type="submit" name="do" value="wifi">Turn Wi-Fi on</button>"#);
    check("Wi-Fi off", &wifi_off);
    let scan_off = browser.post_then(network, &[("do", "scan-bluetooth")]).await;
    scan_off.assert_has("Turn it on first.").assert_has(r#"value="bluetooth">Turn Bluetooth on</button>"#);
    check("Bluetooth off", &scan_off);
    let pair_off = browser.post_then(network, &[("do", "device"), ("address", "AA:BB:CC:00:00:01")]).await;
    pair_off.assert_has("Bluetooth is off.").assert_has("Turn Bluetooth on");
    check("pairing with Bluetooth off", &pair_off);
    check("scanning with Wi-Fi off", &browser.post_then(network, &[("do", "scan-wifi")]).await);
}

#[tokio::test]
async fn a_path_that_is_no_page_is_not_served() {
    let app = App::new();
    let mut browser = app.login().await;
    for path in ["/system/restore", "/system/shell", "/system/web-gui", "/setup"] {
        assert_eq!(browser.get(path).await.status, StatusCode::NOT_FOUND, "{path}");
    }
}
