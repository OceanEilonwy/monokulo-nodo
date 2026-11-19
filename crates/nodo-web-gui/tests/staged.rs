//! Changes kept with Save and applied later (`flows::staged`): the header's count, the list of
//! them, Reset and Save & Apply, through the router.

mod common;

use common::App;

const AUTO: &str = "/system/auto-update";
const HOSTING: &str = "/apps/moneropay/hosting";
const CHANGES: &str = "/changes";

/// The apps ticked on the automatic-update page as it is now.
async fn ticked_on(browser: &mut common::Browser<'_>) -> Vec<String> {
    let body = browser.get(AUTO).await.body;
    body.split(r#"name="on" value=""#)
        .skip(1)
        .filter_map(|rest| rest.split_once('"'))
        .filter(|(_, after)| after.starts_with(" checked"))
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// The form of the automatic-update page with these choices.
fn auto_form<'a>(action: &'a str, frequency: &'a str, on: &'a [String]) -> Vec<(&'a str, &'a str)> {
    let mut form = vec![("do", action), ("frequency", frequency), ("window", "03:00 to 05:00")];
    form.extend(on.iter().map(|name| ("on", name.as_str())));
    form
}

#[tokio::test]
async fn automatic_updates_are_kept_listed_reset_and_applied() {
    let app = App::new();
    let mut browser = app.login().await;
    let on = ticked_on(&mut browser).await;
    browser.get(AUTO).await.assert_lacks("Unsaved changes").assert_has("Save &amp; Apply").assert_has(r#"value="reset""#);

    // Save keeps the draft here: the Nodo still has Daily, the page shows Weekly.
    let kept = browser.post_then(AUTO, &auto_form("keep", "Weekly", &on)).await;
    kept.assert_has("Automatic updates kept: 1 change, not on the Nodo yet.").assert_has("Unsaved changes");
    browser.get(AUTO).await.assert_has(r#"<option value="Weekly" selected>"#);
    let list = browser.get(CHANGES).await;
    list.assert_has("Automatic updates").assert_has(r#"name="key" value="auto-update""#).assert_has("Reset all");

    // Reset from the list: gone, and the page shows what the Nodo has.
    assert_eq!(browser.post(CHANGES, &[("do", "reset"), ("key", "auto-update")]).await.location(), CHANGES);
    browser.get(CHANGES).await.assert_has("Nothing is waiting.").assert_lacks(r#"class="unsaved""#);
    browser.get(AUTO).await.assert_has(r#"<option value="Daily" selected>"#);

    // Kept again, then applied: stored, and nothing waits.
    browser.post(AUTO, &auto_form("keep", "Weekly", &on)).await;
    let applied = browser.post_then(AUTO, &auto_form("save", "Weekly", &on)).await;
    applied.assert_has("Auto-update choices saved.").assert_lacks(r#"class="unsaved""#);
    browser.get(AUTO).await.assert_has(r#"<option value="Weekly" selected>"#);
}

#[tokio::test]
async fn saving_nothing_keeps_nothing() {
    let app = App::new();
    let mut browser = app.login().await;
    let on = ticked_on(&mut browser).await;
    let same = browser.post_then(AUTO, &auto_form("keep", "Daily", &on)).await;
    same.assert_has("Automatic updates has no changes to keep.").assert_lacks(r#"class="unsaved""#);
}

#[tokio::test]
async fn a_hosting_draft_is_kept_and_reset_on_its_page() {
    let app = App::new();
    let mut browser = app.login().await;
    browser.get(HOSTING).await.assert_has("Save &amp; Apply").assert_has(r#"value="keep""#);
    // Every reach turned off: at least one change from what the Nodo serves.
    let kept = browser.post_then(HOSTING, &[("ticks", "1"), ("do", "keep")]).await;
    kept.assert_has("moneropay hosting kept:").assert_has(r#"class="unsaved""#);
    browser.get(CHANGES).await.assert_has("moneropay hosting").assert_has(r#"value="hosting/moneropay""#);
    let reset = browser.post_then(HOSTING, &[("do", "reset")]).await;
    reset.assert_has("moneropay hosting: changes dropped.").assert_lacks(r#"class="unsaved""#);
}

#[tokio::test]
async fn reset_all_drops_every_draft() {
    let app = App::new();
    let mut browser = app.login().await;
    let on = ticked_on(&mut browser).await;
    browser.post(AUTO, &auto_form("keep", "Weekly", &on)).await;
    browser.post(HOSTING, &[("ticks", "1"), ("do", "keep")]).await;
    let list = browser.get(CHANGES).await;
    list.assert_has("Automatic updates").assert_has("moneropay hosting");
    let after = browser.post_then(CHANGES, &[("do", "reset-all")]).await;
    after.assert_has("Nothing is waiting.").assert_lacks(r#"class="unsaved""#);
}
