//! The list of installed apps: its tick boxes and the bar that restarts or updates the ticked
//! apps, through the router.

mod common;

use axum::http::StatusCode;
use common::App;

const APPS: &str = "/apps";

/// The row of an app in the list, from its tick box to the next one.
fn row<'a>(body: &'a str, name: &str) -> &'a str {
    let marker = format!(r#"name="app" value="{name}""#);
    let start = body.find(&marker).unwrap_or_else(|| panic!("no row for {name} in:\n{body}"));
    let rest = &body[start..];
    rest[1..].find(r#"name="app""#).map_or(rest, |end| &rest[..end])
}

#[tokio::test]
async fn every_app_has_a_tick_box_and_the_bar_acts_on_the_ticked() {
    let app = App::new();
    let mut browser = app.login().await;
    let page = browser.get(APPS).await;
    assert_eq!(page.status, StatusCode::OK);
    page.assert_has(r#"<form class="foot" id="ticked" method="post" action="/apps">"#)
        .assert_has(r#"name="do" value="restart""#)
        .assert_has(r#"name="do" value="update""#)
        .assert_has("Restart ticked")
        .assert_has("Update the ticked apps")
        .assert_has(r#"aria-label="Tick all" hidden"#)
        .assert_lacks("style=");
    for name in ["cuprated", "bell", "monerod", "wallet-relay"] {
        assert!(row(&page.body, name).contains(r#"form="ticked""#), "{name}'s box belongs to the bar's form");
    }
    // The rows keep their own buttons, outside the bar's form.
    assert!(row(&page.body, "bell").contains(r#"action="/apps/bell/restart""#));
}

#[tokio::test]
async fn ticked_apps_restart_together_and_the_others_are_explained() {
    let app = App::new();
    let mut browser = app.login().await;
    let before = browser.get(APPS).await;
    for name in ["cuprated", "tor"] {
        assert!(!row(&before.body, name).contains("up under a minute"), "{}", row(&before.body, name));
    }

    // cuprated and tor run: both restart. monerod is stopped and is left out.
    let page = browser.post_then(APPS, &[("app", "tor"), ("app", "cuprated"), ("app", "monerod"), ("do", "restart")]).await;
    page.assert_has("monerod was left out: it is stopped, so Start it instead.");
    for name in ["cuprated", "tor"] {
        assert!(row(&page.body, name).contains("Running · up under a minute"), "{name} was restarted:\n{}", row(&page.body, name));
    }
    assert!(row(&page.body, "monerod").contains("Stopped"));

    // Only apps that cannot restart: nothing is done, and it says why.
    browser
        .post_then(APPS, &[("app", "monerod"), ("do", "restart")])
        .await
        .assert_has("Nothing was restarted. monerod was left out: it is stopped, so Start it instead.");
}

#[tokio::test]
async fn nothing_ticked_asks_for_a_tick_and_sends_nothing() {
    let app = App::new();
    let mut browser = app.login().await;
    for action in ["restart", "update"] {
        browser.post_then(APPS, &[("do", action)]).await.assert_has("Tick at least one app first.");
    }
}

#[tokio::test]
async fn updating_the_ticked_apps_follows_the_updates_flow() {
    let app = App::new();
    let mut browser = app.login().await;

    // wallet-relay's update asks for something new, so it is reviewed first; the Updates page
    // then has it ticked, and only it.
    let answer = browser.post(APPS, &[("app", "wallet-relay"), ("app", "tor"), ("do", "update")]).await;
    assert_eq!(answer.status, StatusCode::SEE_OTHER, "{}", answer.body);
    let updates = browser.get("/system/updates").await;
    assert!(updates.body.contains(r#"name="tick" value="wallet-relay" checked"#), "{}", updates.body);
    assert!(!updates.body.contains(r#"name="tick" value="moneropay" checked"#));

    // moneropay needs nothing new: the batch goes on as a job and ends on what it came to.
    let answer = browser.post(APPS, &[("app", "moneropay"), ("do", "update")]).await;
    assert_eq!(answer.status, StatusCode::SEE_OTHER, "{}", answer.body);
    let end = browser.wait_for_job().await;
    assert!(end.location().starts_with("/system/updates"), "{}", end.location());

    // Ticked apps with no update ready: said, and nothing is sent.
    browser.post_then(APPS, &[("app", "tor"), ("do", "update")]).await.assert_has("None of the ticked apps has an update ready.");
}
