//! Behaviour tests. They drive the app through key events against the mock, with no terminal, and
//! assert both the resulting screen and what would have reached the helper.

mod common;

use common::{draw, screen_text, send, text, type_text};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nodo_api::{ApiError, Exposure, Request, ServiceState, SettingChange, SettingInput, SettingLiteral};
use nodo_tui::app::*;
use nodo_tui::fixtures::{ctrl, fixture, press};
use nodo_tui::mock::{sample_address, MockBackend, MOCK_PASSPHRASE};
use nodo_tui::model::*;
use nodo_tui::runtime::{dispatch, execute_effect, load_snapshot};
use nodo_tui::update::update;

const ENTER: KeyCode = KeyCode::Enter;
const ESC: KeyCode = KeyCode::Esc;
const UP: KeyCode = KeyCode::Up;
const DOWN: KeyCode = KeyCode::Down;
const RIGHT: KeyCode = KeyCode::Right;
const TAB: KeyCode = KeyCode::Tab;
const SPACE: KeyCode = KeyCode::Char(' ');

fn key(app: &mut App, mock: &mut MockBackend, code: KeyCode) {
    dispatch(app, mock, press(code));
}

fn keys(app: &mut App, mock: &mut MockBackend, codes: &[KeyCode]) {
    send(app, mock, codes.iter().map(|code| press(*code)));
}

fn loaded() -> (App, MockBackend) {
    fixture(ScreenId::Main)
}

fn exposure_calls(mock: &MockBackend) -> Vec<Exposure> {
    mock.calls
        .iter()
        .filter_map(|call| match call {
            Request::SetExposure { exposure, .. } => Some(*exposure),
            _ => None,
        })
        .collect()
}

/// What an apply call carried, as text, so assertions read like the form.
fn sent_values(call: &Request) -> Vec<(String, String)> {
    let Request::ApplySettings { changes, .. } = call else { return Vec::new() };
    changes
        .iter()
        .map(|change| {
            let value = match &change.value {
                SettingInput::Literal(SettingLiteral::Text(text)) => text.clone(),
                SettingInput::Literal(SettingLiteral::Int(n)) => n.to_string(),
                SettingInput::Literal(SettingLiteral::Bool(b)) => b.to_string(),
                SettingInput::Reference { target, setting } => format!("$({target}.{setting})"),
                SettingInput::Secret(secret) => secret.expose().to_owned(),
                SettingInput::Reset => String::new(),
            };
            (change.name.clone(), value)
        })
        .collect()
}

fn unlock_calls(mock: &MockBackend) -> usize {
    mock.calls.iter().filter(|call| matches!(call, Request::Unlock { .. })).count()
}

fn feed(mock: &MockBackend, name: &str) -> Feed {
    mock.feeds.iter().find(|feed| feed.name == name).expect("feed exists").clone()
}

// Dangerous defaults

#[test]
fn review_starts_on_cancel_and_enter_installs_nothing() {
    let (mut app, _) = fixture(ScreenId::Review);
    let Screen::Review(state) = &app.screen else { panic!("expected review") };
    assert!(!state.confirm_focused);
    assert!(screen_text(&app).contains("▸Cancel"));
    let effects = update(&mut app, press(ENTER));
    assert!(effects.is_empty(), "{effects:?}");
    assert_ne!(app.screen.id(), ScreenId::Review);
}

#[test]
fn review_installs_only_after_moving_to_the_install_button() {
    let (mut app, _) = fixture(ScreenId::Review);
    assert!(update(&mut app, press(RIGHT)).is_empty());
    assert!(screen_text(&app).contains("▸Install and configure"));
    let effects = update(&mut app, press(ENTER));
    assert!(matches!(effects.as_slice(), [Effect::StartInstall { id, .. }] if id.name == "wallet-relay"));
}

#[test]
fn feed_key_starts_on_keeping_the_old_key() {
    let (mut app, mut mock) = fixture(ScreenId::FeedKey);
    let Screen::FeedKey(state) = &app.screen else { panic!("expected feed key") };
    assert_eq!(state.choice, 0);
    assert!(screen_text(&app).contains("▸Keep the old key and stay blocked (safe default)"));
    key(&mut app, &mut mock, ENTER);
    assert_eq!(feed(&mock, "old-tools").status, FeedStatus::Blocked);
    assert_eq!(feed(&mock, "old-tools").key_state, KeyState::Changed);
}

#[test]
fn trusting_a_new_key_needs_the_exact_feed_name() {
    let (mut app, mut mock) = fixture(ScreenId::FeedKey);
    keys(&mut app, &mut mock, &[DOWN, ENTER]);
    let screen = screen_text(&app);
    assert!(screen.contains("Type old-tools to confirm"), "{screen}");
    assert_eq!(feed(&mock, "old-tools").status, FeedStatus::Blocked);

    for wrong in ["old-tool", "OLD-TOOLS", "old-tools "] {
        type_text(&mut app, &mut mock, wrong);
        key(&mut app, &mut mock, ENTER);
        assert_eq!(feed(&mock, "old-tools").status, FeedStatus::Blocked, "accepted {wrong:?}");
        assert!(screen_text(&app).contains("That is not the feed name."));
        dispatch(&mut app, &mut mock, ctrl('u'));
    }

    type_text(&mut app, &mut mock, "old-tools");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(feed(&mock, "old-tools").status, FeedStatus::Healthy);
    assert_eq!(feed(&mock, "old-tools").key_state, KeyState::Pinned);
}

#[test]
fn every_other_risky_choice_starts_on_the_safe_answer() {
    let (mut app, mut mock) = fixture(ScreenId::AddFeed);
    let before = mock.feeds.len();
    key(&mut app, &mut mock, ENTER);
    assert_eq!(mock.feeds.len(), before, "add feed starts on Cancel");

    let (mut app, _) = fixture(ScreenId::Capability);
    let Screen::Capability(state) = &app.screen else { panic!("expected capability") };
    assert_eq!(state.choice, 0);
    assert!(update(&mut app, press(ENTER)).is_empty());

    // Removing a feed asks first, and that question also starts on Cancel.
    let (mut app, mut mock) = fixture(ScreenId::Feeds);
    key(&mut app, &mut mock, KeyCode::Char('x'));
    assert!(screen_text(&app).contains("▸Cancel"));
    key(&mut app, &mut mock, ENTER);
    assert!(mock.feeds.iter().any(|feed| feed.name == "monokulo"));
    key(&mut app, &mut mock, KeyCode::Char('x'));
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert!(!mock.feeds.iter().any(|feed| feed.name == "monokulo"));

    // Taking every site offline asks first too.
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, KeyCode::Char('o'));
    key(&mut app, &mut mock, ENTER);
    assert!(exposure_calls(&mock).is_empty());
}

#[test]
fn repairing_the_drive_explains_and_asks_before_doing_anything() {
    let (mut app, mut mock) = fixture(ScreenId::NoDrive);
    keys(&mut app, &mut mock, &[DOWN, DOWN, ENTER]);
    let screen = screen_text(&app);
    assert!(screen.contains("can lose data"), "{screen}");
    assert!(screen.contains("▸Cancel"), "{screen}");
    assert!(mock.drive.is_some());
    key(&mut app, &mut mock, ENTER);
    assert!(mock.drive.is_some(), "Cancel must not repair");

    key(&mut app, &mut mock, ENTER);
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert!(mock.drive.is_none());
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::Main, "Home returns once the drive is back");
}

#[test]
fn rejected_packages_have_no_way_to_install() {
    let (mut app, _) = fixture(ScreenId::Rejected);
    for code in [ENTER, SPACE, KeyCode::Char('i'), KeyCode::Char('o'), RIGHT, TAB] {
        let effects = update(&mut app, press(code));
        assert!(!effects.iter().any(|effect| matches!(effect, Effect::StartInstall { .. })));
        assert_eq!(app.screen.id(), ScreenId::Rejected);
    }
}

// Exposure

fn shop_exposure(mock: &MockBackend) -> Exposure {
    mock.web.sites[0].exposure
}

/// Web tab to the exposure screen for shop, with public switched off first.
fn exposure_with_public_off() -> (App, MockBackend) {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    mock.web.sites[0].exposure = Exposure { local: true, onion: true, public: false };
    app.data = load_snapshot(&mut mock).unwrap();
    mock.calls.clear();
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::WebExposure);
    (app, mock)
}

fn at_confirm_screen() -> (App, MockBackend) {
    let (mut app, mut mock) = exposure_with_public_off();
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::WebCheck, "first check finds closed ports");
    key(&mut app, &mut mock, KeyCode::Char('r'));
    assert_eq!(app.screen.id(), ScreenId::WebConfirm, "second check passes");
    (app, mock)
}

#[test]
fn exposures_toggle_independently() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, ENTER);
    let draft = |app: &App| {
        let Screen::WebExposure(state) = &app.screen else { panic!("expected exposure") };
        state.draft
    };
    assert_eq!(draft(&app), Exposure { local: true, onion: true, public: true });
    key(&mut app, &mut mock, SPACE);
    assert_eq!(draft(&app), Exposure { local: false, onion: true, public: true });
    keys(&mut app, &mut mock, &[DOWN, SPACE]);
    assert_eq!(draft(&app), Exposure { local: false, onion: false, public: true });
    keys(&mut app, &mut mock, &[UP, SPACE]);
    assert_eq!(draft(&app), Exposure { local: true, onion: false, public: true });

    // A site that is already public keeps public while another flag changes, with no retyping.
    key(&mut app, &mut mock, ENTER);
    assert_eq!(shop_exposure(&mock), Exposure { local: true, onion: false, public: true });
    assert_eq!(app.screen.id(), ScreenId::WebSites);
}

#[test]
fn turning_public_off_needs_no_confirmation() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, ENTER);
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(shop_exposure(&mock), Exposure { local: true, onion: true, public: false });
}

#[test]
fn turning_on_public_warns_and_mentions_onion_only_when_onion_is_on() {
    let (mut app, mut mock) = exposure_with_public_off();
    assert!(!screen_text(&app).contains("Turning on public internet"));
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE]);
    let screen = screen_text(&app);
    assert!(screen.contains("Turning on public internet"));
    assert!(screen.contains("Keeping onion on too can link it to your home IP."));
    keys(&mut app, &mut mock, &[UP, SPACE]);
    assert!(!screen_text(&app).contains("Keeping onion on too"));
}

#[test]
fn local_and_onion_changes_apply_but_public_waits_for_confirmation() {
    let (mut app, mut mock) = exposure_with_public_off();
    keys(&mut app, &mut mock, &[SPACE, DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(exposure_calls(&mock), [Exposure { local: false, onion: true, public: false }]);
    assert_eq!(app.screen.id(), ScreenId::WebCheck);
}

#[test]
fn going_public_requires_typing_the_exact_address() {
    let (mut app, mut mock) = at_confirm_screen();
    mock.calls.clear();
    let wrong = [
        "",
        "shop.example.co",
        "SHOP.EXAMPLE.COM",
        "shop.example.com ",
        " shop.example.com",
        "https://shop.example.com",
        "shop.example.com.",
    ];
    for attempt in wrong {
        dispatch(&mut app, &mut mock, ctrl('u'));
        type_text(&mut app, &mut mock, attempt);
        key(&mut app, &mut mock, ENTER);
        assert!(exposure_calls(&mock).is_empty(), "accepted {attempt:?}");
        assert!(screen_text(&app).contains("Type shop.example.com exactly to confirm."), "{attempt:?}");
        // The button cannot be reached and pressed around the check either.
        keys(&mut app, &mut mock, &[TAB, TAB, ENTER]);
        assert!(exposure_calls(&mock).is_empty(), "button accepted {attempt:?}");
        assert_eq!(app.screen.id(), ScreenId::WebConfirm);
    }

    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "shop.example.com");
    key(&mut app, &mut mock, ENTER);
    assert!(exposure_calls(&mock).is_empty(), "Enter in the field only moves on to the button");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(exposure_calls(&mock), [Exposure { local: true, onion: true, public: true }]);
    // Going public follows the certificate, and only says it is in place once Caddy has one.
    assert_eq!(app.screen.id(), ScreenId::WebCertificate);
    assert!(screen_text(&app).contains("✓ Certificate in place"), "{}", screen_text(&app));
    key(&mut app, &mut mock, KeyCode::Esc);
    assert_eq!(app.screen.id(), ScreenId::WebSites);
    assert!(!mock.public_access["shop"].require_password);
}

#[test]
fn confirm_default_is_cancel_even_with_the_right_text_typed() {
    let (mut app, mut mock) = at_confirm_screen();
    mock.calls.clear();
    type_text(&mut app, &mut mock, "shop.example.com");
    assert!(screen_text(&app).contains("▸Cancel"));
    key(&mut app, &mut mock, TAB);
    key(&mut app, &mut mock, ENTER);
    assert!(exposure_calls(&mock).is_empty());
    assert_ne!(app.screen.id(), ScreenId::WebConfirm);
}

#[test]
fn a_site_does_not_go_public_if_its_protections_fail() {
    let (mut app, mut mock) = at_confirm_screen();
    mock.calls.clear();
    mock.fail_public_access = true;
    type_text(&mut app, &mut mock, "shop.example.com");
    keys(&mut app, &mut mock, &[ENTER, ENTER]);
    assert!(exposure_calls(&mock).is_empty());
    assert!(screen_text(&app).contains("Could not apply the site's protections."));
}

#[test]
fn escape_from_confirm_leaves_the_site_not_public() {
    let (mut app, mut mock) = at_confirm_screen();
    key(&mut app, &mut mock, ESC);
    assert!(!shop_exposure(&mock).public);
}

#[test]
fn cgnat_blocks_public_and_offers_onion_without_touching_public() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    keys(&mut app, &mut mock, &[DOWN, ENTER, DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::WebCgnat);
    let Screen::WebCgnat(state) = &app.screen else { panic!("expected cgnat") };
    assert_eq!(state.choice, 0, "onion is the suggested way forward");
    key(&mut app, &mut mock, ENTER);
    let pay = &mock.web.sites[1];
    assert!(pay.exposure.onion && !pay.exposure.public);
    keys(&mut app, &mut mock, &[DOWN, ENTER]);
    assert!(app.overlay.is_some());
}

#[test]
fn check_screen_can_turn_on_onion_without_going_public() {
    let (mut app, mut mock) = exposure_with_public_off();
    mock.web.sites[0].exposure.onion = false;
    app.data = load_snapshot(&mut mock).unwrap();
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::WebCheck);
    key(&mut app, &mut mock, KeyCode::Char('o'));
    assert_eq!(shop_exposure(&mock), Exposure { local: true, onion: true, public: false });
}

#[test]
fn a_site_without_an_address_asks_for_one_before_checking() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    keys(&mut app, &mut mock, &[DOWN, DOWN, DOWN, ENTER, DOWN, DOWN, SPACE, ENTER]);
    let Screen::WebExposure(state) = &app.screen else { panic!("expected exposure") };
    assert!(state.editing_host.is_some());
    type_text(&mut app, &mut mock, "relay.other.org");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("The address must be under example.com."));
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "relay.example.com");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(mock.web.sites[3].public_host.as_deref(), Some("relay.example.com"));
}

#[test]
fn taking_all_sites_offline_covers_every_online_site() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, KeyCode::Char('o'));
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert!(mock.web.sites.iter().all(|site| site.exposure == Exposure::default()));
    assert!(screen_text(&app).contains("80 and 443 are closed on this device"));
}

// Providers

#[test]
fn providers_without_an_api_are_unsupported_with_no_guided_move() {
    for (name, steps) in [("Hover", 9), ("Squarespace", 10)] {
        let (mut app, mut mock) = fixture(ScreenId::WebProviders);
        for _ in 0..steps {
            key(&mut app, &mut mock, DOWN);
        }
        let screen = screen_text(&app);
        assert!(screen.contains("✕ Not supported"), "{screen}");
        assert!(screen.contains(&format!("{name} has no DNS")), "{screen}");
        assert!(!screen.contains("API key"), "{screen}");
        let lower = screen.to_lowercase();
        for banned in ["move your", "migrate", "migration", "transfer", "nameserver", "switch to"] {
            assert!(!lower.contains(banned), "{name} offers {banned}:\n{screen}");
        }
        for code in [ENTER, TAB, KeyCode::Char('t'), KeyCode::Char('?')] {
            assert!(update(&mut app, press(code)).is_empty(), "{name} acted on {code:?}");
            assert!(screen_text(&app).contains("has no DNS API, so it is not supported."));
        }
        assert_eq!(mock.stored_provider.as_ref().unwrap().provider, "Porkbun");
    }
}

#[test]
fn a_supported_provider_can_be_tested_and_saved() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, KeyCode::Char('d'));
    assert_eq!(app.screen.id(), ScreenId::WebDomain);
    // Off the email field, p opens the provider list.
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('p')]);
    assert_eq!(app.screen.id(), ScreenId::WebProviders);
    key(&mut app, &mut mock, DOWN);
    key(&mut app, &mut mock, KeyCode::Char('t'));
    assert!(screen_text(&app).contains("✕ Enter the domain and both"));
    key(&mut app, &mut mock, TAB);
    type_text(&mut app, &mut mock, "example.org");
    key(&mut app, &mut mock, TAB);
    type_text(&mut app, &mut mock, "api-value-123");
    key(&mut app, &mut mock, TAB);
    type_text(&mut app, &mut mock, "secret-value-456");
    let screen = screen_text(&app);
    assert!(!screen.contains("api-value-123") && !screen.contains("secret-value-456"), "{screen}");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(mock.stored_provider.as_ref().unwrap().provider, "Cloudflare");
    assert_eq!(app.screen.id(), ScreenId::WebDomain);
    key(&mut app, &mut mock, KeyCode::Esc);
    assert_eq!(app.screen.id(), ScreenId::WebSites);
    assert!(screen_text(&app).contains("Cloudflare"));
}

#[test]
fn the_local_ca_screen_shows_the_whole_fingerprint_and_the_trust_page() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, KeyCode::Char('d'));
    // On the email field, c is typed rather than taken as a shortcut.
    key(&mut app, &mut mock, KeyCode::Char('c'));
    assert_eq!(app.screen.id(), ScreenId::WebDomain);
    assert!(screen_text(&app).contains("c trust it on your devices"));
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('c')]);
    assert_eq!(app.screen.id(), ScreenId::WebTrust);
    let screen = screen_text(&app);
    let fingerprint = app.data.web.acme.local_ca.clone().unwrap();
    assert!(screen.contains(&fingerprint[..47]) && screen.contains(&fingerprint[48..]), "{screen}");
    assert!(screen.contains("http://nodo.local"));
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::WebDomain);
}

#[test]
fn local_name_records_that_lag_are_explained_on_the_web_tab() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    app.data.web.local_records_note = Some("The DNS records wait for the unlock.".to_owned());
    app.data.web.trust_url = None;
    assert!(screen_text(&app).contains("▲ Local names: The DNS records wait for the unlock."));
    keys(&mut app, &mut mock, &[KeyCode::Char('d'), DOWN, KeyCode::Char('c')]);
    assert!(screen_text(&app).contains("▲ Not served"));
}

#[test]
fn keys_typed_for_one_provider_are_not_kept_for_another() {
    let (mut app, mut mock) = fixture(ScreenId::WebProviders);
    keys(&mut app, &mut mock, &[TAB, TAB]);
    type_text(&mut app, &mut mock, "porkbun-key");
    keys(&mut app, &mut mock, &[ESC, DOWN]);
    let Screen::WebProviders(state) = &app.screen else { panic!("expected providers") };
    assert!(state.api_key.is_empty());
    assert!(state.test.is_none());
}

// Secrets

const SECRET: &str = "hunter2-very-secret";

fn assert_hidden(app: &App) {
    for (width, height) in [(100, 32), (80, 24), (60, 16)] {
        let mut sized = app.clone();
        sized.size = (width, height);
        let screen = text(&draw(&sized, width, height).0);
        assert!(!screen.contains(SECRET), "visible at {width}x{height}:\n{screen}");
        assert!(!screen.contains("hunter"), "partly visible at {width}x{height}:\n{screen}");
    }
    assert!(!format!("{app:?}").contains(SECRET), "visible in Debug output");
}

fn settings_app() -> (App, MockBackend) {
    let (mut app, mut mock) = loaded();
    let form = mock.settings_form("moneropay");
    dispatch(&mut app, &mut mock, Event::SettingsLoaded { package: "moneropay".to_owned(), form });
    (app, mock)
}

fn focus_field(app: &mut App, index: usize) {
    if let Screen::Settings(state) = &mut app.screen {
        state.selected = index;
    }
}

const WEBHOOK: usize = 4;
const API_PORT: usize = 1;

#[test]
fn a_secret_setting_is_never_rendered_at_any_stage() {
    let (mut app, mut mock) = settings_app();
    focus_field(&mut app, WEBHOOK);
    assert_hidden(&app);

    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, SECRET);
    assert_hidden(&app);
    assert!(screen_text(&app).contains('•'));

    key(&mut app, &mut mock, ENTER);
    assert_hidden(&app);
    assert!(screen_text(&app).contains("new secret, not saved yet"));

    // A failed apply lists the draft, which must only say a secret was entered.
    focus_field(&mut app, API_PORT);
    key(&mut app, &mut mock, ENTER);
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "18081");
    key(&mut app, &mut mock, ENTER);
    dispatch(&mut app, &mut mock, ctrl('s'));
    assert_eq!(app.screen.id(), ScreenId::ApplyFailed);
    assert_hidden(&app);
    assert!(screen_text(&app).contains("new secret"));

    // The helper does receive it, and then the form forgets it.
    key(&mut app, &mut mock, ENTER);
    focus_field(&mut app, API_PORT);
    key(&mut app, &mut mock, ENTER);
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "5001");
    key(&mut app, &mut mock, ENTER);
    dispatch(&mut app, &mut mock, ctrl('s'));
    let sent = mock.calls.iter().any(|call| {
        sent_values(call).iter().any(|(key, value)| key == "webhook_secret" && value == SECRET)
    });
    assert!(sent);
    assert_hidden(&app);
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.draft[WEBHOOK], FieldValue::SecretSet);
}

#[test]
fn effects_carrying_a_secret_are_safe_to_debug_print() {
    let (mut app, mut mock) = settings_app();
    focus_field(&mut app, WEBHOOK);
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, SECRET);
    key(&mut app, &mut mock, ENTER);
    let effects = update(&mut app, ctrl('s'));
    assert!(!effects.is_empty());
    assert!(!format!("{effects:?}").contains(SECRET));
}

#[test]
fn a_stored_secret_is_masked_and_unchanged_unless_retyped() {
    let (mut app, mut mock) = settings_app();
    focus_field(&mut app, WEBHOOK);
    key(&mut app, &mut mock, ENTER);
    key(&mut app, &mut mock, ENTER);
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.draft[WEBHOOK], FieldValue::SecretSet, "an empty edit keeps the stored value");
    assert!(state.changed_indices().is_empty());
}

#[test]
fn undo_forgets_a_typed_secret() {
    let (mut app, mut mock) = settings_app();
    focus_field(&mut app, WEBHOOK);
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, SECRET);
    key(&mut app, &mut mock, ENTER);
    key(&mut app, &mut mock, KeyCode::Char('z'));
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.draft[WEBHOOK], FieldValue::SecretSet);
}

#[test]
fn secrets_cannot_be_given_a_reference() {
    let (mut app, mut mock) = settings_app();
    focus_field(&mut app, WEBHOOK);
    key(&mut app, &mut mock, KeyCode::Char('$'));
    assert_eq!(app.screen.id(), ScreenId::Settings);
    assert!(screen_text(&app).contains("This field cannot use a reference."));
}

#[test]
fn the_reference_picker_never_lists_a_secret() {
    let (app, _) = fixture(ScreenId::Reference);
    let Screen::Reference(state) = &app.screen else { panic!("expected reference") };
    assert_eq!(state.list.hidden_secrets, 2);
    assert!(state.list.options.iter().all(|option| !option.path.contains("secret")));
}

// Unlock

fn at_unlock() -> (App, MockBackend) {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('u'));
    assert_eq!(app.screen.id(), ScreenId::Unlock);
    (app, mock)
}

#[test]
fn a_failed_unlock_shows_an_error_and_keeps_the_app_usable() {
    let (mut app, mut mock) = at_unlock();
    type_text(&mut app, &mut mock, "wrong-passphrase");
    assert!(!screen_text(&app).contains("wrong-passphrase"));
    key(&mut app, &mut mock, ENTER);

    assert_eq!(app.screen.id(), ScreenId::Unlock);
    assert!(!app.should_quit);
    let screen = screen_text(&app);
    assert!(screen.contains("✕ That passphrase did not unlock this device. Try again."), "{screen}");
    assert!(!format!("{app:?}").contains("wrong-passphrase"));
    let Screen::Unlock(state) = &app.screen else { panic!("expected unlock") };
    assert!(state.input.is_empty(), "a failed passphrase is not kept");

    // Leaving works, and the rest of the app still responds and is still locked.
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::Main);
    assert!(screen_text(&app).contains("This device is locked."));
    key(&mut app, &mut mock, TAB);
    assert_eq!(app.screen.id(), ScreenId::Search);
}

#[test]
fn other_unlock_errors_are_shown_too() {
    let (mut app, _) = at_unlock();
    update(&mut app, Event::UnlockResult(Err(ApiError::Failed("helper is not responding".to_owned()))));
    assert_eq!(app.screen.id(), ScreenId::Unlock);
    assert!(screen_text(&app).contains("✕ Could not unlock: helper is not responding"));
}

#[test]
fn the_last_second_of_the_delay_reads_as_one_second() {
    let (mut app, mut mock) = at_unlock();
    type_text(&mut app, &mut mock, "bad");
    key(&mut app, &mut mock, ENTER);
    for _ in 0..TICKS_PER_SECOND {
        dispatch(&mut app, &mut mock, Event::Tick);
    }
    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("Please wait 1 second before trying again."), "{}", screen_text(&app));
}

#[test]
fn attempts_are_delayed_after_each_failure_and_the_delay_grows() {
    let (mut app, mut mock) = at_unlock();
    type_text(&mut app, &mut mock, "bad");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(unlock_calls(&mock), 1);

    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(unlock_calls(&mock), 1, "too soon");
    assert!(screen_text(&app).contains("Please wait 2 seconds"));

    for _ in 0..(2 * TICKS_PER_SECOND) {
        dispatch(&mut app, &mut mock, Event::Tick);
    }
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "bad again");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(unlock_calls(&mock), 2);
    let Screen::Unlock(state) = &app.screen else { panic!("expected unlock") };
    assert_eq!(state.retry_at_tick - app.ticks, 4 * TICKS_PER_SECOND);
}

#[test]
fn an_empty_passphrase_is_not_sent() {
    let (mut app, mut mock) = at_unlock();
    key(&mut app, &mut mock, ENTER);
    assert_eq!(unlock_calls(&mock), 0);
    assert!(screen_text(&app).contains("Enter the device passphrase."));
}

#[test]
fn one_passphrase_unlocks_everything() {
    let (mut app, mut mock) = at_unlock();
    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(unlock_calls(&mock), 1);
    assert_eq!(app.screen.id(), ScreenId::Main);
    assert!(mock.services.iter().all(|svc| svc.state != ServiceState::Locked));
    assert!(mock.services.iter().find(|svc| svc.id == "bell").unwrap().state == ServiceState::Running);
    assert!(!screen_text(&app).contains("This device is locked."));
    assert!(screen_text(&app).contains("✓ Unlocked."));
    key(&mut app, &mut mock, KeyCode::Char('u'));
    assert_eq!(app.screen.id(), ScreenId::Main, "nothing left to unlock");
}

#[test]
fn no_screen_suggests_a_passphrase_per_package() {
    for id in ScreenId::ALL {
        let screen = screen_text(&fixture(id).0).to_lowercase();
        for banned in ["a passphrase", "package passphrase", "its passphrase", "passphrases"] {
            assert!(!screen.contains(banned), "{}: {banned}\n{screen}", id.slug());
        }
    }
    assert!(screen_text(&fixture(ScreenId::Detail).0).contains("needs the device passphrase"));
}

// Settings

#[test]
fn an_invalid_form_is_not_sent() {
    let (mut app, mut mock) = fixture(ScreenId::Settings);
    mock.calls.clear();
    dispatch(&mut app, &mut mock, ctrl('s'));
    assert!(!mock.calls.iter().any(|call| matches!(call, Request::ApplySettings { .. })));
    let screen = screen_text(&app);
    assert!(screen.contains("Fix the marked field before saving. Nothing was changed."), "{screen}");
    assert!(screen.contains("✕ Address is too short."));
}

#[test]
fn only_changed_settings_are_sent_through_the_real_api() {
    let (mut app, mut mock) = settings_app();
    focus_field(&mut app, API_PORT);
    key(&mut app, &mut mock, ENTER);
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "5001");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("● 1 unsaved change"));
    dispatch(&mut app, &mut mock, ctrl('s'));
    assert!(mock.calls.contains(&Request::ApplySettings {
        package: "moneropay".to_owned(),
        changes: vec![SettingChange {
            name: "api_port".to_owned(),
            value: SettingInput::Literal(SettingLiteral::Text("5001".to_owned())),
        }],
    }));
    assert!(screen_text(&app).contains("✓ Saved. moneropay restarted"));
}

#[test]
fn a_rollback_keeps_the_draft_for_editing_and_can_discard_it() {
    let (mut app, mut mock) = settings_app();
    focus_field(&mut app, API_PORT);
    key(&mut app, &mut mock, ENTER);
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "18081");
    key(&mut app, &mut mock, ENTER);
    keys(&mut app, &mut mock, &[DOWN, RIGHT]);
    dispatch(&mut app, &mut mock, ctrl('s'));
    assert_eq!(app.screen.id(), ScreenId::ApplyFailed);
    let screen = screen_text(&app);
    assert!(screen.contains("5000 → 18081") && screen.contains("info → debug"), "{screen}");

    key(&mut app, &mut mock, ENTER);
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.selected, API_PORT, "edit draft focuses the field that caused it");
    assert_eq!(state.changed_indices().len(), 2);

    dispatch(&mut app, &mut mock, ctrl('s'));
    key(&mut app, &mut mock, KeyCode::Char('x'));
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert!(state.changed_indices().is_empty());
}

#[test]
fn an_api_error_on_save_keeps_the_draft() {
    let (mut app, mut mock) = settings_app();
    focus_field(&mut app, API_PORT);
    key(&mut app, &mut mock, ENTER);
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "5001");
    key(&mut app, &mut mock, ENTER);
    update(&mut app, Event::SettingsApplied(Err(ApiError::Denied)));
    assert!(screen_text(&app).contains("✕ Not saved: this user may not perform that action"));
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.changed_indices().len(), 1);
}

#[test]
fn leaving_unsaved_settings_asks_first() {
    let (mut app, mut mock) = fixture(ScreenId::Settings);
    app.stack.push(Screen::Home { selected: 0 });
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::Settings);
    assert!(screen_text(&app).contains("Discard your changes?"));
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::Settings, "Cancel keeps the draft");
    key(&mut app, &mut mock, ESC);
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Main);
}

#[test]
fn using_a_reference_writes_a_reference_and_shows_what_it_resolves_to() {
    let (mut app, mut mock) = settings_app();
    key(&mut app, &mut mock, KeyCode::Char('$'));
    assert_eq!(app.screen.id(), ScreenId::Reference);
    type_text(&mut app, &mut mock, "restricted");
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("$(monero-daemon.restricted_rpc_url)"), "{screen}");
    assert!(screen.contains("Resolves to http://127.0.0.1:18089"), "{screen}");
    dispatch(&mut app, &mut mock, ctrl('s'));
    assert!(mock.calls.iter().any(|call| sent_values(call)
        .contains(&("daemon_rpc".to_owned(), "$(monero-daemon.restricted_rpc_url)".to_owned()))));
}

#[test]
fn the_reference_picker_can_fall_back_to_typing_a_value() {
    let (mut app, mut mock) = settings_app();
    key(&mut app, &mut mock, KeyCode::Char('$'));
    type_text(&mut app, &mut mock, "t");
    let Screen::Reference(state) = &app.screen else { panic!("expected reference") };
    assert_eq!(state.query.text(), "t", "t types while the search has focus");
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('t')]);
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert!(state.editing.is_some());
}

#[test]
fn address_validation_accepts_a_full_address() {
    let (mut app, mut mock) = settings_app();
    focus_field(&mut app, 3);
    key(&mut app, &mut mock, ENTER);
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, &sample_address());
    key(&mut app, &mut mock, ENTER);
    assert!(!screen_text(&app).contains("✕ Address"));
}

// Search and install

fn search(app: &mut App, mock: &mut MockBackend, query: &str) {
    key(app, mock, KeyCode::Char('/'));
    assert_eq!(app.screen.id(), ScreenId::Search);
    type_text(app, mock, query);
}

#[test]
fn search_filters_as_you_type_and_letters_type_until_the_list_has_focus() {
    let (mut app, mut mock) = loaded();
    search(&mut app, &mut mock, "pay");
    assert!(screen_text(&app).contains("3 of 41 results"));
    type_text(&mut app, &mut mock, "i");
    assert!(screen_text(&app).contains("0 of 41 results"), "i typed into the query");
    assert_eq!(app.screen.id(), ScreenId::Search);
    key(&mut app, &mut mock, KeyCode::Backspace);
    key(&mut app, &mut mock, DOWN);
    key(&mut app, &mut mock, KeyCode::Char('i'));
    assert_eq!(app.screen.id(), ScreenId::Review, "i installs once the list has focus");
    let Screen::Review(state) = &app.screen else { panic!("expected review") };
    assert_eq!(state.data.id.name, "moneropay", "the first Down did not skip the highlighted row");
}

#[test]
fn the_same_name_in_two_feeds_shows_both_and_opens_the_chosen_one() {
    let (mut app, mut mock) = loaded();
    search(&mut app, &mut mock, "node-exporter");
    let screen = screen_text(&app);
    assert!(screen.contains("2 of 41 results"), "{screen}");
    assert!(screen.contains("monokulo") && screen.contains("community"));
    keys(&mut app, &mut mock, &[DOWN, DOWN, ENTER]);
    let Screen::Detail(detail) = &app.screen else { panic!("expected detail") };
    assert_eq!(detail.id, PackageId::new("community", "node-exporter"));
}

#[test]
fn search_filter_feed_and_clear() {
    let (mut app, mut mock) = loaded();
    search(&mut app, &mut mock, "");
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('f')]);
    assert!(screen_text(&app).contains("Feed: official"));
    assert!(screen_text(&app).contains("12 of 41 results"));
    key(&mut app, &mut mock, ESC);
    assert!(screen_text(&app).contains("All feeds"));
}

#[test]
fn install_runs_to_completion_then_opens_settings() {
    let (mut app, mut mock) = loaded();
    search(&mut app, &mut mock, "moneropay");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::Detail);
    key(&mut app, &mut mock, KeyCode::Char('i'));
    assert_eq!(app.screen.id(), ScreenId::Review);
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Installing);
    assert!(screen_text(&app).contains("Step 1 of 8"));
    assert!(app.stack.iter().all(|screen| screen.id() != ScreenId::Review), "Esc must not reopen review");

    for _ in 0..200 {
        dispatch(&mut app, &mut mock, Event::Tick);
    }
    let screen = screen_text(&app);
    assert!(screen.contains("✓ Installed"), "{screen}");
    assert!(screen.contains("↵ configure"), "{screen}");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::Settings);
}

#[test]
fn a_failed_install_reports_that_it_was_undone() {
    let (mut app, mut mock) = loaded();
    search(&mut app, &mut mock, "paywall");
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('i')]);
    let screen = screen_text(&app);
    assert!(screen.contains("Third-party package"), "{screen}");
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    for _ in 0..200 {
        dispatch(&mut app, &mut mock, Event::Tick);
    }
    let screen = screen_text(&app);
    assert!(screen.contains("✕ Not installed"), "{screen}");
    assert!(screen.contains("removed"), "{screen}");
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::Search);
}

#[test]
fn an_install_can_be_cancelled() {
    let (mut app, mut mock) = fixture(ScreenId::Review);
    app.stack.push(Screen::Home { selected: 0 });
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Installing);
    key(&mut app, &mut mock, ESC);
    assert_ne!(app.screen.id(), ScreenId::Installing);
    assert!(screen_text(&app).contains("Install cancelled."));
}

#[test]
fn install_gates_route_to_the_right_screen() {
    let (mut app, mut mock) = loaded();
    search(&mut app, &mut mock, "fast-sync");
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('i')]);
    assert_eq!(app.screen.id(), ScreenId::Rejected);
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::Search);

    let (mut app, mut mock) = loaded();
    search(&mut app, &mut mock, "zmq");
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('i')]);
    assert_eq!(app.screen.id(), ScreenId::Capability);
    keys(&mut app, &mut mock, &[DOWN, ENTER]);
    assert!(screen_text(&app).contains("none found in your feeds."));
    keys(&mut app, &mut mock, &[DOWN, ENTER]);
    let Screen::Review(state) = &app.screen else { panic!("switching still goes through review") };
    assert_eq!(state.data.id.name, "monerod");
    assert!(!state.confirm_focused);
}

#[test]
fn opening_a_feed_from_a_rejected_package_selects_that_feed() {
    let (mut app, mut mock) = fixture(ScreenId::Rejected);
    key(&mut app, &mut mock, KeyCode::Char('f'));
    assert_eq!(app.screen, Screen::Feeds { selected: 2 });
}

// Home

#[test]
fn home_starts_and_stops_through_the_real_api() {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('s'));
    assert!(mock.calls.contains(&Request::StopService { id: "cuprated".to_owned() }));
    assert!(screen_text(&app).contains("stopped"));
    key(&mut app, &mut mock, KeyCode::Char('s'));
    assert!(mock.calls.contains(&Request::StartService { id: "cuprated".to_owned() }));
}

#[test]
fn a_locked_service_cannot_be_started() {
    let (mut app, mut mock) = loaded();
    keys(&mut app, &mut mock, &[DOWN, DOWN, DOWN, KeyCode::Char('s')]);
    assert!(!mock.calls.iter().any(|call| matches!(call, Request::StartService { .. })));
    assert!(screen_text(&app).contains("Press u to unlock"));
}

#[test]
fn home_l_opens_the_failing_service_log_as_the_attention_panel_promises() {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('l'));
    let screen = screen_text(&app);
    assert_eq!(app.screen.id(), ScreenId::Logs);
    assert!(screen.contains("▸bell ✕"), "{screen}");
    assert!(screen.contains("connection refused: wallet-relay is locked"), "{screen}");
}

#[test]
fn a_backend_error_on_refresh_shows_and_does_not_crash() {
    let (mut app, _) = loaded();
    update(&mut app, Event::Failed("Could not read device state".to_owned()));
    assert!(screen_text(&app).contains("✕ Could not read device state"));
}

#[test]
fn tabs_cycle_both_ways_and_q_quits_from_a_tab_only() {
    let (mut app, mut mock) = loaded();
    let order = [
        ScreenId::Search,
        ScreenId::WebSites,
        ScreenId::Feeds,
        ScreenId::Updates,
        ScreenId::Logs,
        ScreenId::Main,
    ];
    for expected in order {
        key(&mut app, &mut mock, TAB);
        assert_eq!(app.screen.id(), expected);
    }
    key(&mut app, &mut mock, KeyCode::BackTab);
    assert_eq!(app.screen.id(), ScreenId::Logs);

    let (mut app, mut mock) = fixture(ScreenId::Detail);
    key(&mut app, &mut mock, KeyCode::Char('q'));
    assert!(!app.should_quit, "q does nothing on a drill-down screen");
    let (mut app, mut mock) = at_unlock();
    type_text(&mut app, &mut mock, "q");
    assert!(!app.should_quit, "q is text in a passphrase");
    dispatch(&mut app, &mut mock, ctrl('c'));
    assert!(app.should_quit, "Ctrl-C always quits");
}

#[test]
fn control_keys_in_a_passphrase_are_not_text() {
    let (mut app, mut mock) = at_unlock();
    dispatch(&mut app, &mut mock, Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)));
    let Screen::Unlock(state) = &app.screen else { panic!("expected unlock") };
    assert!(state.input.is_empty());
}

#[test]
fn help_overlay_opens_and_closes() {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('?'));
    assert!(screen_text(&app).contains("Choices that could harm the device start on the safe answer."));
    key(&mut app, &mut mock, KeyCode::Char('s'));
    assert!(!mock.calls.iter().any(|call| matches!(call, Request::StopService { .. })), "keys go to the overlay");
    key(&mut app, &mut mock, ESC);
    assert!(app.overlay.is_none());
}

// Terminal size and paste

#[test]
fn a_too_small_terminal_refuses_every_key_but_quit() {
    let (mut app, mut mock) = fixture(ScreenId::Review);
    dispatch(&mut app, &mut mock, Event::Resize(40, 10));
    let screen = text(&draw(&app, 40, 10).0);
    assert!(screen.contains("Terminal too small: 40x10."), "{screen}");
    keys(&mut app, &mut mock, &[RIGHT, ENTER, ESC]);
    assert_eq!(app.screen.id(), ScreenId::Review);
    let Screen::Review(state) = &app.screen else { panic!("expected review") };
    assert!(!state.confirm_focused);
    key(&mut app, &mut mock, KeyCode::Char('q'));
    assert!(app.should_quit);
}

#[test]
fn pasted_text_only_goes_to_a_focused_input_and_never_confirms() {
    // Pasted letters must not fire shortcuts on a screen with no input.
    let (mut app, mut mock) = loaded();
    dispatch(&mut app, &mut mock, Event::Paste("sq".to_owned()));
    assert!(!app.should_quit);
    assert!(!mock.calls.iter().any(|call| matches!(call, Request::StopService { .. })));

    // A pasted newline does not press the button.
    let (mut app, mut mock) = at_confirm_screen();
    mock.calls.clear();
    dispatch(&mut app, &mut mock, Event::Paste("shop.example.com\n".to_owned()));
    let Screen::WebConfirm(state) = &app.screen else { panic!("expected confirm") };
    assert_eq!(state.typed.text(), "shop.example.com");
    assert_eq!(state.focus, ConfirmFocus::Address);
    assert!(exposure_calls(&mock).is_empty());

    // A pasted passphrase stays hidden.
    let (mut app, mut mock) = at_unlock();
    dispatch(&mut app, &mut mock, Event::Paste(SECRET.to_owned()));
    assert_hidden(&app);
}

// Feeds

#[test]
fn adding_a_feed_looks_it_up_then_pins_its_key_only_after_trust() {
    let (mut app, mut mock) = fixture(ScreenId::Feeds);
    key(&mut app, &mut mock, KeyCode::Char('a'));
    assert!(screen_text(&app).contains("Step 1 of 2"));
    type_text(&mut app, &mut mock, "http://insecure.example/feed");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("✕ A feed address must start with https://"));

    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "https://github.com/example/monero-tools-feed");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("Step 2 of 2"));
    assert!(!mock.feeds.iter().any(|feed| feed.name == "monero-tools"));

    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert_eq!(feed(&mock, "monero-tools").key_state, KeyState::Pinned);
    assert_eq!(app.screen.id(), ScreenId::Feeds);
    assert!(screen_text(&app).contains("✓ Added monero-tools and pinned its key."));
}

#[test]
fn feed_rows_route_blocked_feeds_to_the_key_screen_and_others_to_packages() {
    let (mut app, mut mock) = fixture(ScreenId::Feeds);
    keys(&mut app, &mut mock, &[DOWN, DOWN, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::FeedKey);

    let (mut app, mut mock) = fixture(ScreenId::Feeds);
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("Feed: monokulo"));
    assert!(screen_text(&app).contains("6 of 41 results"));
}

#[test]
fn feeds_can_be_disabled_and_enabled_again() {
    let (mut app, mut mock) = fixture(ScreenId::Feeds);
    key(&mut app, &mut mock, KeyCode::Char('e'));
    assert_eq!(feed(&mock, "monokulo").status, FeedStatus::Disabled);
    assert!(screen_text(&app).contains("e enable"));
    key(&mut app, &mut mock, KeyCode::Char('e'));
    assert_eq!(feed(&mock, "monokulo").status, FeedStatus::Healthy);
}

// Updates

#[test]
fn a_new_permission_is_reviewed_before_anything_updates() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Review);
    let screen = screen_text(&app);
    assert!(screen.contains("Update wallet-relay 0.5.0"), "{screen}");
    assert!(screen.contains("▲ Connect out to any internet address"), "{screen}");
    assert!(screen.contains("▸Cancel"), "{screen}");

    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::Updates);
    assert_eq!(mock.updates.len(), 4, "cancelling the review applies nothing, not even the rest");

    key(&mut app, &mut mock, ENTER);
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::UpdateFailed);
    assert!(!mock.updates.iter().any(|item| item.name == "wallet-relay"));
}

#[test]
fn an_update_rollback_defaults_to_keeping_and_holding() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::UpdateFailed);
    assert!(screen_text(&app).contains("1 updated, 1 rolled back"));
    let screen = screen_text(&app);
    assert!(screen.contains("▸Keep 2.7.0 and hold this version"), "{screen}");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::Updates);
    assert!(mock.updates.iter().find(|item| item.name == "moneropay").unwrap().held);
}

#[test]
fn the_update_count_drops_when_a_version_is_held() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    assert!(screen_text(&app).contains("Updates (3)"));
    key(&mut app, &mut mock, KeyCode::Char('h'));
    assert!(screen_text(&app).contains("Updates (2)"));
    assert!(screen_text(&app).contains("You skipped this version"));
}

#[test]
fn updating_with_nothing_selected_explains_itself() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    keys(&mut app, &mut mock, &[SPACE, DOWN, SPACE, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Updates);
    assert!(screen_text(&app).contains("Select at least one update first."));
}

// Auto-update

fn at_auto_update() -> (App, MockBackend) {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    key(&mut app, &mut mock, KeyCode::Char('a'));
    assert_eq!(app.screen.id(), ScreenId::AutoUpdate);
    (app, mock)
}

#[test]
fn the_updates_screen_offers_auto_update_and_counts_saved_packages() {
    let (app, _) = fixture(ScreenId::Updates);
    let screen = screen_text(&app);
    assert!(screen.lines().next().unwrap().ends_with("Auto-update on for 3 packages"), "{screen}");
    assert!(screen.contains("a auto-update"));
}

#[test]
fn packages_default_to_auto_update_off() {
    let entry = AutoUpdateEntry::new("anything", "official", false, "");
    assert!(!entry.enabled);

    // A freshly installed package joins the list switched off.
    let mut mock = MockBackend::new();
    let before = mock.auto_update.enabled_count();
    let id = PackageId::new("community", "node-exporter");
    mock.start_install(&id, None, None).unwrap();
    for _ in 0..200 {
        mock.poll_install();
    }
    let added = mock.auto_update.packages.iter().find(|entry| entry.name == "node-exporter").unwrap();
    assert!(!added.enabled);
    assert!(added.third_party);
    assert_eq!(mock.auto_update.enabled_count(), before);
}

#[test]
fn third_party_warning_shows_only_when_focused_or_enabled() {
    const WARNING: &str = "▲ Runs new code without your review";
    let entry = AutoUpdateEntry::new("relay", "community", true, "");
    assert!(!entry.shows_warning(false));
    assert!(entry.shows_warning(true));
    let enabled = AutoUpdateEntry { enabled: true, ..entry };
    assert!(enabled.shows_warning(false));
    let official = AutoUpdateEntry::new("node", "official", false, "");
    assert!(!official.shows_warning(true));

    let (mut app, mut mock) = at_auto_update();
    assert!(!screen_text(&app).contains(WARNING), "unfocused and off");
    keys(&mut app, &mut mock, &[DOWN, DOWN, DOWN, DOWN]);
    assert!(screen_text(&app).contains(WARNING), "focused");
    key(&mut app, &mut mock, SPACE);
    key(&mut app, &mut mock, UP);
    assert!(screen_text(&app).contains(WARNING), "enabled, focus elsewhere");
    keys(&mut app, &mut mock, &[DOWN, SPACE, UP]);
    assert!(!screen_text(&app).contains(WARNING), "off again");

    // A project feed's package never gets the warning, focused or not.
    let (mut app, mut mock) = at_auto_update();
    keys(&mut app, &mut mock, &[DOWN, DOWN, DOWN]);
    assert!(!screen_text(&app).contains(WARNING));
}

#[test]
fn auto_update_choices_save_and_change_the_updates_header() {
    let (mut app, mut mock) = at_auto_update();
    keys(&mut app, &mut mock, &[DOWN, SPACE, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Updates);
    assert_eq!(mock.auto_update.enabled_count(), 2);
    let screen = screen_text(&app);
    assert!(screen.contains("Auto-update on for 2 packages"), "{screen}");
    assert!(screen.contains("✓ Auto-update choices saved."), "{screen}");

    key(&mut app, &mut mock, KeyCode::Char('a'));
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE, DOWN, SPACE, ENTER]);
    assert_eq!(mock.auto_update.enabled_count(), 0);
    assert!(screen_text(&app).contains("Auto-update off"));
}

#[test]
fn the_header_counts_saved_choices_not_the_draft() {
    let (mut app, mut mock) = at_auto_update();
    key(&mut app, &mut mock, DOWN);
    key(&mut app, &mut mock, SPACE);
    key(&mut app, &mut mock, ESC);
    assert!(screen_text(&app).contains("Discard your changes?"));
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::AutoUpdate, "Cancel keeps the draft");
    key(&mut app, &mut mock, ESC);
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Updates);
    assert!(screen_text(&app).contains("Auto-update on for 3 packages"));
    assert_eq!(mock.auto_update.enabled_count(), 3);
}

#[test]
fn escape_without_changes_goes_straight_back() {
    let (mut app, mut mock) = at_auto_update();
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::Updates);
}

#[test]
fn the_install_window_is_validated_and_moves_the_next_run() {
    let (mut app, mut mock) = at_auto_update();
    key(&mut app, &mut mock, KeyCode::Char('w'));
    assert!(screen_text(&app).contains("03:00 to 05:00"));
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "25:00 to 05:00");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("✕ Use two different device times"));
    assert_eq!(mock.auto_update.window_start, "03:00");

    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "01:30 to 02:30");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("01:30 to 02:30 device time"));
    assert_eq!(mock.auto_update.window_start, "03:00", "not saved until the screen is saved");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(mock.auto_update.window_start, "01:30");
    assert_eq!(mock.auto_update.next_run, "Next run tonight at 01:30");
}

#[test]
fn frequency_cycles_and_history_opens() {
    let (mut app, mut mock) = at_auto_update();
    key(&mut app, &mut mock, SPACE);
    assert!(screen_text(&app).contains("‹ Weekly ›"));
    key(&mut app, &mut mock, KeyCode::Char('h'));
    assert!(screen_text(&app).contains("Auto-update history"));
}

// Logs and the rest

#[test]
fn logs_filter_follow_and_find() {
    let (mut app, mut mock) = fixture(ScreenId::Logs);
    key(&mut app, &mut mock, KeyCode::Char('e'));
    let screen = screen_text(&app);
    assert!(screen.contains("ERROR") && !screen.contains("INFO"), "{screen}");
    key(&mut app, &mut mock, KeyCode::Char('e'));
    key(&mut app, &mut mock, KeyCode::Char('f'));
    assert!(screen_text(&app).contains("○ Paused"));
    key(&mut app, &mut mock, KeyCode::Char('/'));
    type_text(&mut app, &mut mock, "reading");
    let screen = screen_text(&app);
    assert!(screen.contains("reading /etc/bell") && !screen.contains("exiting"), "{screen}");
    key(&mut app, &mut mock, ESC);
    assert!(screen_text(&app).contains("exiting"));
    key(&mut app, &mut mock, KeyCode::Char('u'));
    assert_eq!(app.screen.id(), ScreenId::Unlock, "the suggestion's key works");
}

#[test]
fn a_paused_log_does_not_poll() {
    let (mut app, mut mock) = fixture(ScreenId::Logs);
    key(&mut app, &mut mock, KeyCode::Char('f'));
    let mut polled = false;
    for _ in 0..40 {
        polled |= update(&mut app, Event::Tick).iter().any(|effect| matches!(effect, Effect::LoadLogs(_)));
    }
    assert!(!polled);
    key(&mut app, &mut mock, KeyCode::Char('f'));
    for _ in 0..40 {
        polled |= update(&mut app, Event::Tick).iter().any(|effect| matches!(effect, Effect::LoadLogs(_)));
    }
    assert!(polled);
}

#[test]
fn a_late_log_reply_for_another_service_is_ignored() {
    let (mut app, _) = fixture(ScreenId::Logs);
    update(&mut app, Event::LogsLoaded { source: "tor".to_owned(), view: LogView::default() });
    let Screen::Logs(state) = &app.screen else { panic!("expected logs") };
    assert!(!state.view.lines.is_empty());
}

#[test]
fn the_mock_says_when_it_does_not_really_copy() {
    let (mut app, mut mock) = fixture(ScreenId::Logs);
    key(&mut app, &mut mock, KeyCode::Char('y'));
    assert!(screen_text(&app).contains("was not copied anywhere"));
}

#[test]
fn drive_retry_failure_is_reported_and_diagnostics_open() {
    let (mut app, mut mock) = fixture(ScreenId::NoDrive);
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("✕ Mount failed: structure needs cleaning"));
    assert_eq!(app.screen.id(), ScreenId::NoDrive);
    keys(&mut app, &mut mock, &[DOWN, ENTER]);
    assert!(screen_text(&app).contains("No changes were made."));
}

#[test]
fn the_missing_drive_replaces_home_on_startup() {
    let mut mock = MockBackend::with_missing_drive();
    let mut app = App::new();
    for event in execute_effect(&mut mock, Effect::Refresh) {
        dispatch(&mut app, &mut mock, event);
    }
    assert_eq!(app.screen.id(), ScreenId::NoDrive);
    assert!(screen_text(&app).contains("✕ Node stopped"));
}

#[test]
fn backups_open_from_home_and_a_restore_needs_the_passphrase_then_the_typed_word() {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('b'));
    assert_eq!(app.screen.id(), ScreenId::Backup);
    let screen = screen_text(&app);
    assert!(screen.contains("nodo-1762560000.nodobak"), "{screen}");
    key(&mut app, &mut mock, KeyCode::Char('r'));
    type_text(&mut app, &mut mock, "wrong");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("passphrase did not open"), "{}", screen_text(&app));
    let Screen::Backup(state) = &mut app.screen else { panic!() };
    state.step =
        BackupStep::Unlock { name: "nodo-1762560000.nodobak".into(), passphrase: nodo_tui::secret::Input::secret() };
    state.error = None;
    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("Stops while restoring: moneropay, wallet-relay, tor"), "{screen}");
    assert!(screen.contains("Type restore"), "{screen}");

    // Enter alone, or the wrong word, restores nothing.
    let effects = update(&mut app, press(ENTER));
    assert!(effects.is_empty(), "{effects:?}");
    type_text(&mut app, &mut mock, "yes");
    assert!(update(&mut app, press(ENTER)).is_empty());
    let Screen::Backup(state) = &mut app.screen else { panic!() };
    let BackupStep::Confirm { typed, .. } = &mut state.step else { panic!() };
    typed.clear();
    type_text(&mut app, &mut mock, "restore");
    key(&mut app, &mut mock, TAB);
    type_text(&mut app, &mut mock, "a new device passphrase");
    let effects = update(&mut app, press(ENTER));
    let [Effect::RestoreBackup { typed, new_device_passphrase, passphrase, .. }] = effects.as_slice() else {
        panic!("{effects:?}")
    };
    assert_eq!(typed, "restore");
    assert_eq!(passphrase.expose(), MOCK_PASSPHRASE);
    assert_eq!(new_device_passphrase.as_ref().unwrap().expose(), "a new device passphrase");
    assert!(!format!("{effects:?}").contains(MOCK_PASSPHRASE), "secrets never print");
}

#[test]
fn a_new_backup_needs_matching_passphrases_and_the_device_passphrase() {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('b'));
    key(&mut app, &mut mock, KeyCode::Char('n'));
    type_text(&mut app, &mut mock, "device pass");
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, "backup passphrase one");
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, "backup passphrase two");
    assert!(update(&mut app, press(ENTER)).is_empty());
    assert!(screen_text(&app).contains("differ"), "{}", screen_text(&app));
    type_text(&mut app, &mut mock, "backup passphrase one");
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, "backup passphrase one");
    let effects = update(&mut app, press(ENTER));
    let [Effect::CreateBackup { device_passphrase: Some(device), backup_passphrase, usb: None }] = effects.as_slice() else {
        panic!("{effects:?}")
    };
    assert_eq!(device.expose(), "device pass");
    assert_eq!(backup_passphrase.expose(), "backup passphrase one");
}

#[test]
fn a_package_with_new_encrypted_storage_asks_for_the_device_passphrase_before_installing() {
    let (mut app, _) = loaded();
    let mut data = match MockBackend::default().install_gate(&PackageId::new("community", "wallet-relay")) {
        InstallGate::Review(data) => data,
        other => panic!("{other:?}"),
    };
    data.needs_passphrase = true;
    data.approval =
        Some(nodo_api::PermissionApproval { pkgver: "wallet-relay-0.4.2_1".into(), permissions: vec!["x".into()] });
    app.screen = Screen::Review(ReviewState::new(data, ReviewPurpose::Install));
    update(&mut app, press(RIGHT));
    assert!(update(&mut app, press(ENTER)).is_empty());
    assert!(screen_text(&app).contains("Device passphrase"));
    for c in "correct horse".chars() {
        update(&mut app, press(KeyCode::Char(c)));
    }
    let effects = update(&mut app, press(ENTER));
    let [Effect::StartInstall { approval: Some(approval), passphrase: Some(passphrase), .. }] = effects.as_slice()
    else {
        panic!("{effects:?}")
    };
    assert_eq!(approval.permissions, ["x"]);
    assert_eq!(passphrase.expose(), "correct horse");
}

#[test]
fn trusting_a_new_key_sends_the_typed_name_and_both_keys() {
    let (mut app, mut mock) = loaded();
    let change = mock.key_change("old-tools").expect("sample key change");
    app.screen = Screen::FeedKey(FeedKeyState { change: change.clone(), choice: 1, typing: None, error: None });
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, "old-tools");
    let effects = update(&mut app, press(ENTER));
    assert_eq!(effects, [Effect::TrustNewKey { change, typed: "old-tools".into() }]);
}

#[test]
fn a_backup_goes_to_the_usb_drive_and_a_restore_reads_from_it() {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('b'));
    let screen = screen_text(&app);
    assert!(screen.contains("USB drive · SanDisk Ultra Fit, BACKUP"), "{screen}");
    assert!(screen.contains("nodo-1762473600.nodobak"), "{screen}");
    key(&mut app, &mut mock, KeyCode::Char('u'));
    assert!(screen_text(&app).contains("New backup on SanDisk Ultra Fit, BACKUP"), "{}", screen_text(&app));
    type_text(&mut app, &mut mock, "device pass");
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, "backup passphrase one");
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, "backup passphrase one");
    let effects = update(&mut app, press(ENTER));
    let [Effect::CreateBackup { usb: Some(device), .. }] = effects.as_slice() else { panic!("{effects:?}") };
    assert_eq!(device, "sda1");
    for event in execute_effect(&mut mock, effects[0].clone()) {
        dispatch(&mut app, &mut mock, event);
    }
    // Tab moves to the drive's list, and a restore from it names the drive.
    keys(&mut app, &mut mock, &[TAB, ENTER]);
    let screen = screen_text(&app);
    assert!(screen.contains("nodo-1762473600.nodobak, on SanDisk Ultra Fit, BACKUP"), "{screen}");
    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    let effects = update(&mut app, press(ENTER));
    let [Effect::InspectBackup { name, usb: Some(device), .. }] = effects.as_slice() else { panic!("{effects:?}") };
    assert_eq!((name.as_str(), device.as_str()), ("nodo-1762473600.nodobak", "sda1"));
}

#[test]
fn without_a_usb_drive_the_screen_says_so_and_u_explains() {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('b'));
    let Screen::Backup(state) = &mut app.screen else { panic!() };
    state.usb = None;
    assert!(screen_text(&app).contains("No USB drive is plugged in."));
    let effects = update(&mut app, press(KeyCode::Char('u')));
    assert!(effects.is_empty());
    assert!(screen_text(&app).contains("Plug in a USB drive"), "{}", screen_text(&app));
}

#[test]
fn the_backup_screen_shows_its_copy() {
    let (app, _) = fixture(ScreenId::Backup);
    let screen = screen_text(&app);
    for needle in [
        "Home › Backups",
        "Backups on this device",
        "nodo-1762560000.nodobak",
        "4.8 MB",
        "A backup holds",
        "Never chain data.",
        "n new backup",
        "r restore",
    ] {
        assert!(screen.contains(needle), "{needle}\n{screen}");
    }
}

// Onion

fn open_onion() -> (App, MockBackend) {
    let (mut app, mut mock) = loaded();
    dispatch(&mut app, &mut mock, Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
    dispatch(&mut app, &mut mock, Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
    assert_eq!(app.screen.id(), ScreenId::WebSites);
    key(&mut app, &mut mock, KeyCode::Char('n'));
    assert_eq!(app.screen.id(), ScreenId::Onion);
    (app, mock)
}

#[test]
fn n_on_web_opens_the_onion_screen_with_the_node_and_sites() {
    let (app, mock) = open_onion();
    let screen = screen_text(&app);
    assert!(screen.contains("Web › Onion addresses"), "{screen}");
    assert!(screen.contains("nod…xyz.onion:18089"), "{screen}");
    assert!(screen.contains("http://abc…xyz.onion"), "{screen}");
    assert!(mock.calls.iter().any(|c| matches!(c, Request::OnionStatus)));
}

#[test]
fn the_node_onion_turns_off_and_on_through_the_helper() {
    let (mut app, mut mock) = open_onion();
    key(&mut app, &mut mock, SPACE);
    assert!(mock.calls.iter().any(|c| matches!(c, Request::SetNodeOnion { enabled: false })));
    assert!(!mock.node_onion.enabled);
    assert!(screen_text(&app).contains("[ ] cuprated"), "{}", screen_text(&app));
    key(&mut app, &mut mock, ENTER);
    assert!(mock.node_onion.enabled);
    assert!(screen_text(&app).contains("nod…xyz.onion:18089"));
}

#[test]
fn the_node_onion_needs_restricted_rpc_and_says_where_to_turn_it_on() {
    let (mut app, mut mock) = open_onion();
    key(&mut app, &mut mock, SPACE);
    mock.node_onion.restricted_rpc = false;
    key(&mut app, &mut mock, KeyCode::Char('g'));
    assert!(screen_text(&app).contains("restricted RPC is off"), "{}", screen_text(&app));
    mock.calls.clear();
    key(&mut app, &mut mock, SPACE);
    assert!(screen_text(&app).contains("Restricted RPC on the network"), "{}", screen_text(&app));
    assert!(!mock.calls.iter().any(|c| matches!(c, Request::SetNodeOnion { .. })), "nothing was asked");
    // s asks for the node's settings form, where restricted RPC is turned on.
    let effects = update(&mut app, press(KeyCode::Char('s')));
    assert_eq!(effects, vec![Effect::OpenSettings("cuprated".to_owned())]);
}

#[test]
fn a_site_onion_changes_only_the_onion_flag() {
    let (mut app, mut mock) = open_onion();
    keys(&mut app, &mut mock, &[DOWN, SPACE]);
    let sent = exposure_calls(&mock);
    assert_eq!(sent, vec![Exposure { local: true, onion: false, public: true }], "local and public stay as they were");
    assert!(!mock.web.sites[0].exposure.onion);
    assert!(screen_text(&app).contains("[ ] shop"), "{}", screen_text(&app));
}

#[test]
fn a_single_address_site_cannot_add_an_onion_beside_another_address() {
    let (mut app, mut mock) = open_onion();
    keys(&mut app, &mut mock, &[DOWN, DOWN, DOWN, SPACE]);
    assert!(screen_text(&app).contains("moneropay works from a single address"), "{}", screen_text(&app));
    assert!(exposure_calls(&mock).is_empty());
    keys(&mut app, &mut mock, &[DOWN, SPACE]);
    assert!(screen_text(&app).contains("wallet-relay is locked"), "{}", screen_text(&app));
    assert!(exposure_calls(&mock).is_empty());
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::WebSites);
}

// Rollbacks

fn open_rollbacks() -> (App, MockBackend) {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('r'));
    assert_eq!(app.screen.id(), ScreenId::Rollbacks);
    (app, mock)
}

#[test]
fn r_on_home_shows_rollbacks_and_failed_updates() {
    let (app, _) = open_rollbacks();
    let screen = screen_text(&app);
    assert!(screen.contains("moneropay       2.7.0_1      → 2.6.1_1"), "{screen}");
    assert!(screen.contains("bell did not pass its health check"), "{screen}");
}

#[test]
fn a_rollback_asks_first_with_the_safe_answer_focused_then_puts_the_version_back() {
    let (mut app, mut mock) = open_rollbacks();
    key(&mut app, &mut mock, ENTER);
    let modal = app.modal.as_ref().expect("asks first");
    assert!(!modal.confirm_focused, "Cancel has the focus");
    assert!(modal.body.iter().any(|l| l.contains("2.7.0_1 is held")), "{:?}", modal.body);
    key(&mut app, &mut mock, ENTER);
    assert!(!mock.calls.iter().any(|c| matches!(c, Request::RollbackPackage { .. })), "Enter on Cancel does nothing");
    keys(&mut app, &mut mock, &[ENTER, RIGHT, ENTER]);
    assert!(mock
        .calls
        .iter()
        .any(|c| matches!(c, Request::RollbackPackage { package, to } if package == "moneropay" && to == "2.6.1_1")));
    let screen = screen_text(&app);
    assert!(screen.contains("✓ moneropay is back on 2.6.1_1."), "{screen}");
    assert!(!screen.contains("can roll back"), "moneropay is no longer offered: {screen}");
}

#[test]
fn a_blocked_rollback_says_why_and_asks_nothing() {
    let (mut app, mut mock) = open_rollbacks();
    keys(&mut app, &mut mock, &[DOWN, ENTER]);
    assert!(app.modal.is_none());
    assert!(screen_text(&app).contains("wallet-relay cannot roll back now: its data is in layout 2."));
    assert!(!mock.calls.iter().any(|c| matches!(c, Request::RollbackPackage { .. })));
}

#[test]
fn nothing_to_roll_back_says_so() {
    let (mut app, mut mock) = loaded();
    mock.rollbacks.clear();
    mock.update_history.clear();
    key(&mut app, &mut mock, KeyCode::Char('r'));
    let screen = screen_text(&app);
    assert!(screen.contains("Nothing to roll back."), "{screen}");
    assert!(screen.contains("No update has failed."), "{screen}");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("Nothing can be rolled back now."));
}
