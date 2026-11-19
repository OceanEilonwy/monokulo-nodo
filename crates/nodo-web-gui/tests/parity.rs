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
}

#[tokio::test]
async fn a_path_that_is_no_page_is_not_served() {
    let app = App::new();
    let mut browser = app.login().await;
    for path in ["/system/restore", "/system/shell", "/system/web-gui", "/setup"] {
        assert_eq!(browser.get(path).await.status, StatusCode::NOT_FOUND, "{path}");
    }
}
