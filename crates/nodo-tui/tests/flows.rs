//! Behaviour tests. They drive the app through key events against the mock, with no terminal, and
//! assert both the resulting screen and what would have reached the helper.

mod common;

use common::{draw, screen_text, send, text, type_text};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nodo_api::{ApiError, Backend, Exposure, Request, ServiceState, SettingChange, SettingInput, SettingLiteral};
use nodo_tui::app::*;
use nodo_tui::fixtures::{ctrl, fixture, press};
use nodo_ui_core::mock::{sample_address, MockBackend, MOCK_PASSPHRASE};
use nodo_ui_core::model::*;
use nodo_ui_core::{Effect, Outcome};
use nodo_ui_core::exec::{execute_effect, load_snapshot};
use nodo_ui_core::screen::ScreenId;
use nodo_tui::runtime::dispatch;
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
                SettingInput::Template(parts) => parts
                    .iter()
                    .map(|part| match part {
                        nodo_api::TemplatePart::Text(text) => text.clone(),
                        nodo_api::TemplatePart::Reference { target, setting } => format!("$({target}.{setting})"),
                    })
                    .collect(),
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
    assert!(screen_text(&app).contains("▸ Cancel"));
    let effects = update(&mut app, press(ENTER));
    assert!(effects.is_empty(), "{effects:?}");
    assert_ne!(app.screen.id(), ScreenId::Review);
}

#[test]
fn review_installs_only_after_moving_to_the_install_button() {
    let (mut app, _) = fixture(ScreenId::Review);
    assert!(update(&mut app, press(RIGHT)).is_empty());
    assert!(screen_text(&app).contains("▸ Install"));
    let effects = update(&mut app, press(ENTER));
    assert!(matches!(effects.as_slice(), [Effect::StartInstall { id, .. }] if id.name == "wallet-relay"));
}

#[test]
fn feed_key_starts_on_keeping_the_old_key() {
    let (mut app, mut mock) = fixture(ScreenId::FeedKey);
    let Screen::FeedKey(state) = &app.screen else { panic!("expected feed key") };
    assert_eq!(state.choice, 0);
    assert!(screen_text(&app).contains("▸ Keep the old key"));
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
        assert!(screen_text(&app).contains("That isn't the source's name."));
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

// Web, Hosting and domain providers (web flow v3)

fn shop_exposure(mock: &MockBackend) -> Exposure {
    mock.web.sites[0].exposure
}

/// Web tab to shop's Hosting, with the internet switched off first.
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
fn the_web_tab_lists_only_the_sites_that_are_online() {
    let (app, _mock) = fixture(ScreenId::WebSites);
    let screen = screen_text(&app);
    for online in ["shop", "monokulo-pay", "moneropay"] {
        assert!(screen.contains(online), "{online} missing:\n{screen}");
    }
    assert!(!screen.contains("wallet-relay"), "an app that is not online is not listed:\n{screen}");
    assert!(screen.contains("Home and internet") && screen.contains("Home network and Tor"), "{screen}");
    assert!(screen.contains("To put another app online, open it in Apps."), "{screen}");
    assert!(screen.contains("Domain providers ›"), "{screen}");
}

#[test]
fn exposures_toggle_independently() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, ENTER);
    let draft = |app: &App| {
        let Screen::WebExposure(state) = &app.screen else { panic!("expected hosting") };
        state.draft
    };
    assert_eq!(draft(&app), Exposure { local: true, onion: true, public: true });
    key(&mut app, &mut mock, SPACE);
    assert_eq!(draft(&app), Exposure { local: false, onion: true, public: true });
    keys(&mut app, &mut mock, &[DOWN, SPACE]);
    assert_eq!(draft(&app), Exposure { local: false, onion: false, public: true });
    keys(&mut app, &mut mock, &[UP, SPACE]);
    assert_eq!(draft(&app), Exposure { local: true, onion: false, public: true });

    // A site already on the internet keeps it while another choice changes, with no confirm.
    key(&mut app, &mut mock, ENTER);
    assert_eq!(shop_exposure(&mock), Exposure { local: true, onion: false, public: true });
    assert_eq!(app.screen.id(), ScreenId::WebSites);
}

#[test]
fn turning_the_internet_off_needs_no_confirmation() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, ENTER);
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(shop_exposure(&mock), Exposure { local: true, onion: true, public: false });
}

#[test]
fn hosting_speaks_plainly_and_shows_the_address_it_will_use() {
    let (mut app, mut mock) = exposure_with_public_off();
    let screen = screen_text(&app);
    assert!(screen.contains("Choose who can open shop. Any combination works."), "{screen}");
    assert!(!screen.contains("Provider"), "the internet's fields only show once it is ticked");
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE]);
    let screen = screen_text(&app);
    for expected in ["Provider", "Porkbun", "example.com", "shop", "Visitors will be able to see your home's internet address.", "Check and save"] {
        assert!(screen.contains(expected), "{expected} missing:\n{screen}");
    }
    for jargon in ["DNS-01", "CGNAT", "ACME", "port 80", "A record"] {
        assert!(!screen.contains(jargon), "{jargon} on the main screen:\n{screen}");
    }
}

#[test]
fn home_and_tor_changes_apply_but_the_internet_waits_for_the_checks() {
    let (mut app, mut mock) = exposure_with_public_off();
    keys(&mut app, &mut mock, &[SPACE, DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(exposure_calls(&mock), [Exposure { local: false, onion: true, public: false }]);
    assert_eq!(app.screen.id(), ScreenId::WebCheck);
}

#[test]
fn going_online_takes_one_confirm_that_starts_on_cancel() {
    let (mut app, mut mock) = at_confirm_screen();
    mock.calls.clear();
    let screen = screen_text(&app);
    assert!(screen.contains("Ready to put shop on the internet"), "{screen}");
    assert!(screen.contains("Put shop online") && screen.contains("https://shop.example.com"), "{screen}");
    assert!(screen.contains("▸ Cancel is selected"), "{screen}");
    key(&mut app, &mut mock, ENTER);
    assert!(exposure_calls(&mock).is_empty(), "Enter on the default does nothing risky");
    assert_ne!(app.screen.id(), ScreenId::WebConfirm);

    let (mut app, mut mock) = at_confirm_screen();
    mock.calls.clear();
    keys(&mut app, &mut mock, &[TAB, ENTER]);
    assert_eq!(exposure_calls(&mock), [Exposure { local: true, onion: true, public: true }]);
    // Going online follows the secure connection, and says so only once it is in place.
    assert_eq!(app.screen.id(), ScreenId::WebCertificate);
    assert!(screen_text(&app).contains("shop is online"), "{}", screen_text(&app));
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::WebSites);
    assert!(!mock.public_access["shop"].require_password);
}

#[test]
fn a_site_does_not_go_public_if_its_protections_fail() {
    let (mut app, mut mock) = at_confirm_screen();
    mock.calls.clear();
    mock.knobs.fail_public_access = true;
    keys(&mut app, &mut mock, &[TAB, ENTER]);
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
fn a_shared_isp_address_blocks_the_internet_and_offers_tor_without_touching_it() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    keys(&mut app, &mut mock, &[DOWN, ENTER, DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::WebCgnat);
    let screen = screen_text(&app);
    assert!(screen.contains("shares one address between many homes"), "{screen}");
    assert!(screen.contains("Use Tor instead"), "{screen}");
    let Screen::WebCgnat(state) = &app.screen else { panic!("expected cgnat") };
    assert_eq!(state.choice, 0, "Tor is the suggested way forward");
    key(&mut app, &mut mock, ENTER);
    let pay = &mock.web.sites[1];
    assert!(pay.exposure.onion && !pay.exposure.public);
    keys(&mut app, &mut mock, &[KeyCode::Char('d'), DOWN, ENTER]);
    assert!(app.overlay.is_some(), "the other ways out are behind the details");
}

#[test]
fn the_check_screen_can_turn_on_tor_without_going_public() {
    let (mut app, mut mock) = exposure_with_public_off();
    mock.web.sites[0].exposure.onion = false;
    app.data = load_snapshot(&mut mock).unwrap();
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::WebCheck);
    key(&mut app, &mut mock, KeyCode::Char('o'));
    assert_eq!(shop_exposure(&mock), Exposure { local: true, onion: true, public: false });
}

#[test]
fn the_address_part_is_saved_under_the_providers_domain() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    keys(&mut app, &mut mock, &[DOWN, ENTER, DOWN, DOWN, SPACE, DOWN, DOWN]);
    let Screen::WebExposure(state) = &app.screen else { panic!("expected hosting") };
    assert_eq!(state.focus(), HostingFocus::Address);
    assert_eq!(state.sub.text(), "pay", "the saved address starts the field");
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "payments");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(mock.web.sites[1].public_host.as_deref(), Some("payments.example.com"));
}

#[test]
fn an_app_page_in_packages_opens_its_hosting_even_when_offline() {
    let (mut app, mut mock) = fixture(ScreenId::Detail);
    let Screen::Detail(detail) = &mut app.screen else { panic!("expected detail") };
    detail.id.name = "wallet-relay".to_owned();
    detail.installed = true;
    assert!(screen_text(&app).contains("Open its page"), "{}", screen_text(&app));
    key(&mut app, &mut mock, KeyCode::Char('h'));
    let Screen::WebExposure(state) = &app.screen else { panic!("expected hosting") };
    assert_eq!(state.site, "wallet-relay");
    assert_eq!(state.draft, Exposure::default());
}

#[test]
fn taking_all_sites_offline_covers_every_online_site() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, KeyCode::Char('o'));
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert!(mock.web.sites.iter().all(|site| site.exposure == Exposure::default()));
    assert!(screen_text(&app).contains("80 and 443 are closed on this device"));
}

// Domain providers

fn without_a_provider() -> (App, MockBackend) {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    mock.web.provider.clear();
    mock.web.domain.clear();
    mock.connected.clear();
    app.data = load_snapshot(&mut mock).unwrap();
    (app, mock)
}

fn select_provider(app: &mut App, mock: &mut MockBackend, name: &str) {
    for _ in 0..20 {
        let Screen::WebProviders(state) = &app.screen else { panic!("expected providers") };
        if app.data.providers[state.selected].name == name {
            return;
        }
        key(app, mock, DOWN);
    }
    panic!("{name} is not in the catalog");
}

#[test]
fn providers_without_an_api_cannot_be_connected_and_offer_no_guided_move() {
    for name in ["Hover", "Squarespace"] {
        let (mut app, mut mock) = fixture(ScreenId::WebProviders);
        select_provider(&mut app, &mut mock, name);
        key(&mut app, &mut mock, ENTER);
        let screen = screen_text(&app);
        assert!(screen.contains(&format!("{name} can't be connected")), "{screen}");
        assert!(!screen.contains("API key"), "{screen}");
        let lower = screen.to_lowercase();
        for banned in ["move your", "migrate", "migration", "transfer", "nameserver", "switch to"] {
            assert!(!lower.contains(banned), "{name} offers {banned}:\n{screen}");
        }
        let Screen::WebProviders(state) = &app.screen else { panic!("expected providers") };
        assert_eq!(state.stage, ProviderStage::Pick);
        assert_eq!(mock.connected.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Porkbun"]);
    }
}

#[test]
fn adding_a_provider_says_why_there_is_no_list_and_reads_it_again() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    mock.knobs.providers_failure = Some("the helper is not running".to_owned());
    app.data = load_snapshot(&mut mock).unwrap();
    keys(&mut app, &mut mock, &[KeyCode::Char('p'), KeyCode::Char('a')]);
    let screen = screen_text(&app);
    assert!(screen.contains("Couldn't read the providers: the helper is not running"), "{screen}");
    assert!(!screen.contains("Cloudflare"), "{screen}");

    // Opening the add flow again asks the helper again.
    mock.knobs.providers_failure = None;
    keys(&mut app, &mut mock, &[ESC, KeyCode::Char('p'), KeyCode::Char('a')]);
    let screen = screen_text(&app);
    assert!(screen.contains("Cloudflare"), "{screen}");
    assert!(!screen.contains("Couldn't read the providers"), "{screen}");
}

#[test]
fn an_empty_provider_catalog_says_so() {
    let (mut app, _mock) = fixture(ScreenId::WebProviders);
    app.data.providers.clear();
    let screen = screen_text(&app);
    assert!(screen.contains("No providers are offered."), "{screen}");
}

#[test]
fn hosting_without_a_provider_adds_one_and_comes_back() {
    let (mut app, mut mock) = without_a_provider();
    keys(&mut app, &mut mock, &[DOWN, ENTER, DOWN, DOWN, SPACE]);
    let screen = screen_text(&app);
    assert!(screen.contains("first add the company you bought it from"), "{screen}");
    assert!(screen.contains("Add a provider"), "{screen}");
    keys(&mut app, &mut mock, &[DOWN, ENTER]);
    let Screen::WebProviders(state) = &app.screen else { panic!("expected providers") };
    assert_eq!(state.return_to.as_deref(), Some("monokulo-pay"));
    select_provider(&mut app, &mut mock, "Cloudflare");
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("Connect Cloudflare"), "{screen}");
    // Cloudflare lists the domains its key controls, so there is no web address to type.
    assert!(!screen.contains("Web address"), "{screen}");
    assert!(screen.contains("This Nodo is locked. Connect asks for the device passphrase first"), "{screen}");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("Paste the key from Cloudflare"), "{}", screen_text(&app));
    type_text(&mut app, &mut mock, "api-value-123");
    key(&mut app, &mut mock, TAB);
    type_text(&mut app, &mut mock, "secret-value-456");
    let screen = screen_text(&app);
    assert!(!screen.contains("api-value-123") && !screen.contains("secret-value-456"), "{screen}");
    // wallet-relay waits for the passphrase, so the device is locked: Connect asks for it first,
    // then carries on with the keys already typed.
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("Unlock your Nodo") && screen.contains("Connecting Cloudflare stores its key locked"), "{screen}");
    assert!(mock.connected.is_empty(), "nothing is sent before the unlock");
    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(mock.connected.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Cloudflare"]);
    let screen = screen_text(&app);
    assert!(screen.contains("Cloudflare is connected") && screen.contains("Back to monokulo-pay"), "{screen}");
    assert!(screen.contains("example.org") && screen.contains("myshop.net"), "the discovered domains are listed: {screen}");
    key(&mut app, &mut mock, ENTER);
    let Screen::WebExposure(state) = &app.screen else { panic!("expected hosting again") };
    assert_eq!(state.site, "monokulo-pay");
    assert!(state.draft.public, "the internet stays ticked");
    assert!(screen_text(&app).contains(".example.org"), "{}", screen_text(&app));
}

#[test]
fn a_provider_that_cannot_list_asks_for_the_web_address() {
    let (mut app, mut mock) = fixture(ScreenId::WebProviders);
    unlock_mock(&mut app, &mut mock);
    select_provider(&mut app, &mut mock, "GoDaddy");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("Web address"), "{}", screen_text(&app));
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("can't list your domains"), "{}", screen_text(&app));
    type_text(&mut app, &mut mock, "example.net");
    key(&mut app, &mut mock, TAB);
    type_text(&mut app, &mut mock, "key");
    key(&mut app, &mut mock, TAB);
    type_text(&mut app, &mut mock, "secret");
    key(&mut app, &mut mock, ENTER);
    let godaddy = mock.connected.iter().find(|p| p.name == "GoDaddy").expect("connected");
    assert_eq!(godaddy.domains[0].name, "example.net");
}

fn unlock_mock(app: &mut App, mock: &mut MockBackend) {
    assert_eq!(mock.call(Request::Unlock { passphrase: nodo_api::Secret::new(MOCK_PASSPHRASE) }), Ok(nodo_api::Response::Ok));
    app.data = load_snapshot(mock).unwrap();
}

#[test]
fn a_device_locked_with_no_app_waiting_still_unlocks_from_home_and_from_connect() {
    // After a restart with no encrypted app installed yet: nothing waits, but the helper has no
    // device key, so DNS keys can't be stored. The TUI must still offer the unlock.
    let (mut app, mut mock) = without_a_provider();
    unlock_mock(&mut app, &mut mock);
    mock.key_locked = true;
    app.data = load_snapshot(&mut mock).unwrap();
    assert!(mock.services.iter().all(|svc| svc.state != ServiceState::Locked));
    assert!(app.is_locked());

    app.screen = Screen::Home { selected: 0 };
    let screen = screen_text(&app);
    assert!(screen.contains("Your Nodo is locked") && screen.contains("domain provider"), "{screen}");

    app.screen = Screen::WebProviders(ProvidersState::new(""));
    select_provider(&mut app, &mut mock, "Porkbun");
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, "example.net");
    key(&mut app, &mut mock, TAB);
    type_text(&mut app, &mut mock, "pk1_key");
    key(&mut app, &mut mock, TAB);
    type_text(&mut app, &mut mock, "sk1_secret");
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("Unlock your Nodo") && screen.contains("Connecting Porkbun"), "{screen}");
    type_text(&mut app, &mut mock, "wrong passphrase");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("did not unlock"), "{}", screen_text(&app));
    assert!(mock.connected.is_empty());
    let Screen::Unlock(state) = &mut app.screen else { panic!("still unlocking") };
    state.retry_at_tick = 0;
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    key(&mut app, &mut mock, ENTER);
    assert!(!mock.key_locked);
    assert_eq!(mock.connected.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Porkbun"]);
    assert!(screen_text(&app).contains("Porkbun is connected"), "{}", screen_text(&app));
}

#[test]
fn hosting_picks_a_domain_from_any_connected_provider() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    mock.add_provider("Gandi", None, &nodo_ui_core::secret::Secret::new("k"), &nodo_ui_core::secret::Secret::new("s")).unwrap();
    app.data = load_snapshot(&mut mock).unwrap();
    let names: Vec<_> = app.domain_choices().into_iter().map(|(d, p)| format!("{d} {p}")).collect();
    assert_eq!(names, ["example.com Porkbun", "example.org Gandi", "myshop.net Gandi"]);
    let shop = app.data.web.sites.iter().find(|s| s.package == "shop").unwrap().clone();
    let mut state = ExposureState::new("shop", shop.exposure, shop.public_host.as_deref(), &app.domain_choices());
    assert_eq!(state.domain, "example.com", "the saved address's domain is chosen");
    state.draft.public = true;
    app.screen = Screen::WebExposure(state);
    let Screen::WebExposure(state) = &mut app.screen else { panic!("expected hosting") };
    state.selected = state.order().iter().position(|f| *f == HostingFocus::Domain).unwrap();
    keys(&mut app, &mut mock, &[ENTER]);
    let screen = screen_text(&app);
    assert!(screen.contains("myshop.net") && screen.contains("Gandi") && screen.contains("+ Add a provider"), "{screen}");
    keys(&mut app, &mut mock, &[DOWN, DOWN, ENTER]);
    let Screen::WebExposure(state) = &app.screen else { panic!("expected hosting") };
    assert_eq!(state.domain, "myshop.net");
    assert!(state.host().ends_with(".myshop.net"));
    assert!(screen_text(&app).contains(".myshop.net"));
}

#[test]
fn the_providers_list_shows_every_provider_and_removes_one_after_a_confirm() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    mock.add_provider("Gandi", None, &nodo_ui_core::secret::Secret::new("k"), &nodo_ui_core::secret::Secret::new("s")).unwrap();
    app.data = load_snapshot(&mut mock).unwrap();
    key(&mut app, &mut mock, KeyCode::Char('p'));
    let screen = screen_text(&app);
    assert!(screen.contains("Porkbun") && screen.contains("Gandi") && screen.contains("myshop.net"), "{screen}");
    assert_eq!(screen.matches("Remove").count(), 2, "one Remove per provider: {screen}");
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('x')]);
    assert!(app.modal.is_some(), "removing asks first");
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert_eq!(mock.connected.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Porkbun"]);
    assert!(!screen_text(&app).contains("Gandi"), "{}", screen_text(&app));
}

#[test]
fn a_refused_key_says_so_in_the_card() {
    let (mut app, _mock) = fixture(ScreenId::WebProviders);
    let Screen::WebProviders(state) = &mut app.screen else { panic!("expected providers") };
    state.stage = ProviderStage::Key;
    update(&mut app, Event::Done(Outcome::ProviderAdded(Err("Porkbun refused the key: invalid API key".to_owned()))));
    assert!(screen_text(&app).contains("Porkbun refused the key: invalid API key"), "{}", screen_text(&app));
}

#[test]
fn keys_typed_for_one_provider_are_not_kept_for_another() {
    let (mut app, mut mock) = fixture(ScreenId::WebProviders);
    keys(&mut app, &mut mock, &[ENTER, TAB]);
    type_text(&mut app, &mut mock, "porkbun-key");
    keys(&mut app, &mut mock, &[ESC, DOWN, ENTER]);
    let Screen::WebProviders(state) = &app.screen else { panic!("expected providers") };
    assert!(state.api_key.is_empty());
    assert!(state.test.is_none());
}

#[test]
fn the_local_ca_screen_shows_the_whole_fingerprint_and_the_trust_page() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, KeyCode::Char('d'));
    // On the email field, c is typed rather than taken as a shortcut.
    key(&mut app, &mut mock, KeyCode::Char('c'));
    assert_eq!(app.screen.id(), ScreenId::WebDomain);
    let tall = text(&draw(&app, 120, 50).0);
    assert!(tall.contains("c trust it on your devices"), "{tall}");
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('c')]);
    assert_eq!(app.screen.id(), ScreenId::WebTrust);
    let screen = screen_text(&app);
    let fingerprint = app.data.web.acme.local_ca.clone().unwrap();
    assert!(screen.contains(&fingerprint[..47]) && screen.contains(&fingerprint[48..]), "{screen}");
    assert!(screen.contains("http://nodo.local"));
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::WebDomain);
}

/// A refused save on Domains is said where it is seen. At the foot of the page it was below the
/// fold on a 100x32 terminal, so a mistyped email looked as if it had been taken.
#[test]
fn a_refused_email_on_domains_is_on_screen() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, KeyCode::Char('d'));
    assert_eq!(app.screen.id(), ScreenId::WebDomain);
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "not an email");
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("✕ that email address is not valid"), "{screen}");
}

#[test]
fn local_name_records_that_lag_are_explained_on_the_web_tab() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    app.data.web.local_records_note = Some("The DNS records wait for the unlock.".to_owned());
    app.data.web.trust_url = None;
    assert!(screen_text(&app).contains("The DNS records wait for the unlock."));
    keys(&mut app, &mut mock, &[KeyCode::Char('d'), DOWN, KeyCode::Char('c')]);
    assert!(screen_text(&app).contains("▲ Not served"));
}

// Mouse

/// Where `needle` is drawn, as (column, row), at 120x36.
fn find(app: &App, needle: &str) -> (u16, u16) {
    let (buffer, _) = draw(app, 120, 36);
    for (row, line) in text(&buffer).lines().enumerate() {
        if let Some(byte) = line.find(needle) {
            let column = line[..byte].chars().count();
            return (column as u16, row as u16);
        }
    }
    panic!("{needle} is not on screen:\n{}", text(&buffer));
}

fn click(app: &mut App, mock: &mut MockBackend, needle: &str) {
    let (column, row) = find(app, needle);
    dispatch(app, mock, Event::Mouse { kind: MouseKind::Down, column, row });
}

#[test]
fn clicks_open_a_site_tick_a_choice_and_switch_tabs() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    click(&mut app, &mut mock, "monokulo-pay");
    let Screen::WebExposure(state) = &app.screen else { panic!("a row click opens hosting") };
    assert_eq!(state.site, "monokulo-pay");
    click(&mut app, &mut mock, "Tor");
    let Screen::WebExposure(state) = &app.screen else { panic!("expected hosting") };
    assert!(!state.draft.onion, "a click on a choice ticks it like Space");
    click(&mut app, &mut mock, "Show details");
    let Screen::WebExposure(state) = &app.screen else { panic!("expected hosting") };
    assert!(state.details);
    click(&mut app, &mut mock, "Save");
    assert_eq!(mock.web.sites[1].exposure, Exposure { local: true, onion: false, public: false });
    click(&mut app, &mut mock, "System");
    assert_eq!(app.screen.id(), ScreenId::System);
}

#[test]
fn the_confirm_buttons_and_breadcrumbs_take_clicks() {
    let (mut app, mut mock) = at_confirm_screen();
    mock.calls.clear();
    click(&mut app, &mut mock, "Put shop online");
    assert_eq!(exposure_calls(&mock), [Exposure { local: true, onion: true, public: true }]);

    let (mut app, mut mock) = exposure_with_public_off();
    click(&mut app, &mut mock, "Apps  ›");
    assert_eq!(app.screen.id(), ScreenId::Apps, "the first crumb opens its tab");
}

#[test]
fn the_wheel_scrolls_like_the_arrow_keys() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    draw(&app, 120, 36);
    dispatch(&mut app, &mut mock, Event::Mouse { kind: MouseKind::ScrollDown, column: 10, row: 10 });
    assert_eq!(app.screen, Screen::Web { selected: 1 });
    dispatch(&mut app, &mut mock, Event::Mouse { kind: MouseKind::ScrollUp, column: 10, row: 10 });
    assert_eq!(app.screen, Screen::Web { selected: 0 });
}

#[test]
fn hovering_a_row_lifts_it_and_moving_off_clears_it() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    let (column, row) = find(&app, "moneropay");
    let theme = nodo_tui::theme::Theme::truecolor();
    let before = draw(&app, 120, 36).0[(column + 20, row)].bg;
    assert_eq!(before, theme.card);
    dispatch(&mut app, &mut mock, Event::Mouse { kind: MouseKind::Moved, column, row });
    assert!(app.hover.is_some());
    let (buffer, _) = draw(&app, 120, 36);
    assert_eq!(buffer[(column + 20, row)].bg, theme.raised, "the whole row is one surface lighter");
    assert_eq!(buffer[(column + 20, row - 2)].bg, theme.card, "other rows are not");
    // Hover is not selection: the keyboard focus stays on the first site.
    assert_eq!(app.screen, Screen::Web { selected: 0 });
    dispatch(&mut app, &mut mock, Event::Mouse { kind: MouseKind::Moved, column: 0, row: 35 });
    assert!(app.hover.is_none());
    assert_eq!(draw(&app, 120, 36).0[(column + 20, row)].bg, theme.card);
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
    dispatch(&mut app, &mut mock, Event::Done(Outcome::SettingsLoaded { package: "moneropay".to_owned(), form }));
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
    assert!(screen_text(&app).contains("This setting cannot take a value from another app."));
}

#[test]
fn the_reference_picker_lists_secrets_locked_and_never_their_value() {
    let (app, _) = fixture(ScreenId::Reference);
    let Screen::Reference(state) = &app.screen else { panic!("expected reference") };
    let secret = state.list.options.iter().find(|o| o.setting == "rpc_password").expect("listed");
    assert!(secret.locked.is_some_and(|l| l.secret && !l.granted));
    assert_eq!(secret.value, None);
    let screen = screen_text(&app);
    assert!(screen.contains("SECRETS · NOT PUBLIC") && screen.contains("Grant…"), "{screen}");
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
    assert!(screen_text(&app).contains("Your Nodo is locked"));
    key(&mut app, &mut mock, TAB);
    assert_eq!(app.screen.id(), ScreenId::Apps);
}

#[test]
fn other_unlock_errors_are_shown_too() {
    let (mut app, _) = at_unlock();
    update(&mut app, Event::Done(Outcome::UnlockResult(Err(ApiError::Failed("helper is not responding".to_owned())))));
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
    assert!(screen_text(&app).contains("1 change, not saved yet"));
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
    assert!(screen.contains("5000  →  18081") && screen.contains("info  →  debug"), "{screen}");

    // The helper's explanation names no field, so editing again leaves the focus where it was.
    let Screen::ApplyFailed(failed) = &app.screen else { panic!("expected the failure") };
    assert_eq!(failed.explanation.blamed_field, None);
    key(&mut app, &mut mock, ENTER);
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.selected, API_PORT + 1, "the focus stays on the field last edited");
    assert_eq!(state.changed_indices().len(), 2);

    // An explanation that does name a field, which only a future helper could give, focuses it.
    dispatch(&mut app, &mut mock, ctrl('s'));
    let Screen::ApplyFailed(failed) = &mut app.screen else { panic!("expected the failure") };
    failed.explanation.blamed_field = Some("api_port".to_owned());
    key(&mut app, &mut mock, ENTER);
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.selected, API_PORT, "edit draft focuses the field that caused it");

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
    update(&mut app, Event::Done(Outcome::SettingsApplied(Err(ApiError::Denied))));
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

/// Saves, confirming what the references allow when the form asks.
fn save_and_confirm(app: &mut App, mock: &mut MockBackend) {
    dispatch(app, mock, ctrl('s'));
    if matches!(&app.screen, Screen::Settings(state) if state.confirm.is_some()) {
        key(app, mock, ENTER);
    }
}

#[test]
fn using_a_reference_writes_a_reference_and_confirms_what_it_allows() {
    let (mut app, mut mock) = settings_app();
    key(&mut app, &mut mock, KeyCode::Char('$'));
    assert_eq!(app.screen.id(), ScreenId::Reference);
    type_text(&mut app, &mut mock, "rpc_url");
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("cuprated·rpc_url"), "{screen}");
    dispatch(&mut app, &mut mock, ctrl('s'));
    let screen = screen_text(&app);
    assert!(screen.contains("Save moneropay's settings?"), "{screen}");
    assert!(screen.contains("moneropay will be able to reach cuprated (port 18081/tcp)."), "{screen}");
    assert!(screen.contains("cuprated becomes a dependency"), "{screen}");
    key(&mut app, &mut mock, ENTER);
    assert!(mock.calls.iter().any(|call| sent_values(call).contains(&("daemon_rpc".to_owned(), "$(cuprated.rpc_url)".to_owned()))));
}

#[test]
fn esc_from_the_picker_goes_back_to_what_was_being_typed() {
    let (mut app, mut mock) = settings_app();
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, "/x");
    dispatch(&mut app, &mut mock, ctrl('r'));
    type_text(&mut app, &mut mock, "t");
    let Screen::Reference(state) = &app.screen else { panic!("expected reference") };
    assert_eq!(state.query.text(), "t", "t types while the search has focus");
    key(&mut app, &mut mock, ESC);
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    let editor = state.editor.as_ref().expect("still typing");
    assert_eq!(editor.pieces(), vec![Piece::Text("http://127.0.0.1:18081/x".into())]);
}

#[test]
fn the_reference_link_on_a_field_opens_the_picker_with_the_search_focused() {
    let (mut app, mut mock) = settings_app();
    assert!(screen_text(&app).contains("+ reference"));
    click(&mut app, &mut mock, "+ reference");
    let Screen::Reference(state) = &app.screen else { panic!("the link opens the picker") };
    assert!(!state.list_focused, "typing filters straight away");
    type_text(&mut app, &mut mock, "34512");
    let Screen::Reference(state) = &app.screen else { panic!("expected reference") };
    assert_eq!(state.visible().iter().filter(|(_, fit)| fit.is_ok()).count(), 1, "the search matches values too");
    key(&mut app, &mut mock, ENTER);
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.draft[0], FieldValue::reference("wallet-rpc", "url"));
    assert!(screen_text(&app).contains("↺ Undo: back to http://127.0.0.1:18081"));
    key(&mut app, &mut mock, KeyCode::Char('u'));
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.draft[0], FieldValue::Text("http://127.0.0.1:18081".into()));
}

#[test]
fn typing_dollar_paren_suggests_references_and_mixes_them_with_text() {
    let (mut app, mut mock) = settings_app();
    key(&mut app, &mut mock, ENTER);
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "http://$(cu");
    let screen = screen_text(&app);
    assert!(screen.contains("Typing $( suggests references at the cursor."), "{screen}");
    assert!(screen.contains("cuprated.rpc_host") && screen.contains("All references…"), "{screen}");
    // rpc_url is first for an address field; the host is text, which mixes into an address.
    keys(&mut app, &mut mock, &[DOWN, ENTER]);
    type_text(&mut app, &mut mock, ":");
    keys(&mut app, &mut mock, &[KeyCode::F(4)]);
    type_text(&mut app, &mut mock, "rpc_port");
    keys(&mut app, &mut mock, &[ENTER]);
    type_text(&mut app, &mut mock, "/json_rpc");
    key(&mut app, &mut mock, ENTER);
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(
        state.draft[0],
        FieldValue::Pieces(vec![
            Piece::Text("http://".into()),
            Piece::Ref { app: "cuprated".into(), setting: "rpc_host".into() },
            Piece::Text(":".into()),
            Piece::Ref { app: "cuprated".into(), setting: "rpc_port".into() },
            Piece::Text("/json_rpc".into()),
        ])
    );
    assert!(screen_text(&app).contains("Now: http://127.0.0.1:18081/json_rpc"));
    save_and_confirm(&mut app, &mut mock);
    assert!(mock.calls.iter().any(|call| sent_values(call).contains(&(
        "daemon_rpc".to_owned(),
        "http://$(cuprated.rpc_host):$(cuprated.rpc_port)/json_rpc".to_owned()
    ))));
}

#[test]
fn a_broken_reference_is_red_explained_on_enter_and_blocks_saving() {
    let (mut app, mut mock) = settings_app();
    if let Screen::Settings(state) = &mut app.screen {
        state.draft[0] = FieldValue::Pieces(vec![
            Piece::Ref { app: "old-shop".into(), setting: "webhook_url".into() },
            Piece::Text("/orders".into()),
        ]);
    }
    key(&mut app, &mut mock, KeyCode::Char('z'));
    if let Screen::Settings(state) = &mut app.screen {
        state.draft[0] = FieldValue::Pieces(vec![
            Piece::Ref { app: "old-shop".into(), setting: "webhook_url".into() },
            Piece::Text("/orders".into()),
        ]);
    }
    let effects = {
        let Screen::Settings(state) = &app.screen else { panic!() };
        vec![Effect::PreviewSetting { package: "moneropay".into(), field_index: 0, key: "daemon_rpc".into(), value: state.draft[0].clone() }]
    };
    for effect in effects {
        for event in nodo_ui_core::exec::execute_effect(&mut mock, effect) {
            dispatch(&mut app, &mut mock, Event::Done(event));
        }
    }
    let screen = screen_text(&app);
    assert!(screen.contains("old-shop·webhook_url"), "{screen}");
    assert!(screen.contains("Fix 1 broken reference to save"), "{screen}");
    assert!(!screen.contains("isn't installed"), "the reason is not inline: {screen}");
    dispatch(&mut app, &mut mock, ctrl('s'));
    assert!(!mock.calls.iter().any(|call| matches!(call, Request::ApplySettings { .. })));
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("old-shop isn't installed."), "{screen}");
    assert!(screen.contains("Type a value instead"), "{screen}");
    key(&mut app, &mut mock, KeyCode::Char('x'));
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.draft[0], FieldValue::Text("/orders".into()));
    assert!(screen_text(&app).contains("↺ Put old-shop.webhook_url back"));
}

#[test]
fn type_a_value_instead_turns_a_chip_into_its_value_with_a_way_back() {
    let (mut app, mut mock) = settings_app();
    key(&mut app, &mut mock, KeyCode::Char('$'));
    type_text(&mut app, &mut mock, "rpc_url");
    key(&mut app, &mut mock, ENTER);
    keys(&mut app, &mut mock, &[KeyCode::Char('c'), KeyCode::Char('t')]);
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert!(state.editor.is_some(), "ready to edit");
    assert_eq!(state.draft[0], FieldValue::Text("http://127.0.0.1:18081".into()));
    assert!(screen_text(&app).contains("↺ Use cuprated.rpc_url again"));
    key(&mut app, &mut mock, ENTER);
    key(&mut app, &mut mock, KeyCode::Char('u'));
    let Screen::Settings(state) = &app.screen else { panic!("expected settings") };
    assert_eq!(state.draft[0], FieldValue::reference("cuprated", "rpc_url"));
}

#[test]
fn a_locked_setting_asks_for_approval_before_it_can_be_used() {
    let (mut app, mut mock) = settings_app();
    if let Some(option) = mock.references.iter_mut().find(|o| o.reference == "rpc_password") {
        option.kind = nodo_api::SettingKind::Url;
        option.access = nodo_api::ReferenceAccess::NeedsGrant { secret: false, granted: false };
        option.reference = "admin_url".into();
        option.label = "Admin address".into();
    }
    if let Screen::Settings(state) = &mut app.screen {
        state.catalog = None;
    }
    focus_field(&mut app, 0);
    dispatch(&mut app, &mut mock, ctrl('r'));
    type_text(&mut app, &mut mock, "admin_url");
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("Let moneropay use cuprated's admin address?"), "{screen}");
    key(&mut app, &mut mock, ENTER);
    assert!(mock.calls.iter().any(|call| matches!(call, Request::SetGrant { approved: true, .. })));
    let Screen::Settings(state) = &app.screen else { panic!("back to the form") };
    assert_eq!(state.draft[0], FieldValue::reference("cuprated", "admin_url"));
}

#[test]
fn a_port_field_can_take_a_number_from_another_app() {
    let (mut app, mut mock) = settings_app();
    focus_field(&mut app, API_PORT);
    key(&mut app, &mut mock, KeyCode::F(4));
    let Screen::Reference(state) = &app.screen else { panic!("a port offers the picker") };
    assert_eq!(state.field_kind, FieldKind::Port);
    let (first, fit) = &state.visible()[0];
    assert!(fit.is_ok() && first.setting == "rpc_port", "numbers first");
    assert!(screen_text(&app).contains("Numbers first"));
    key(&mut app, &mut mock, ENTER);
    save_and_confirm(&mut app, &mut mock);
    assert!(mock.calls.iter().any(|call| sent_values(call).contains(&("api_port".to_owned(), "$(cuprated.rpc_port)".to_owned()))));
}

#[test]
fn toggles_choices_and_secrets_offer_no_picker() {
    let (mut app, mut mock) = settings_app();
    for index in [2, WEBHOOK, 5] {
        focus_field(&mut app, index);
        key(&mut app, &mut mock, KeyCode::F(4));
        assert_eq!(app.screen.id(), ScreenId::Settings);
    }
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
    assert!(screen_text(&app).contains("3 of 41 apps match"));
    type_text(&mut app, &mut mock, "i");
    assert!(screen_text(&app).contains("0 of 41 apps match"), "i typed into the query");
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
    assert!(screen.contains("2 of 41 apps match"), "{screen}");
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
    assert!(screen_text(&app).contains("12 of 41 apps from official match"), "{}", screen_text(&app));
    key(&mut app, &mut mock, ESC);
    assert!(screen_text(&app).contains("41 of 41 apps match"));
}

/// As on the device: the install is one request, so the page goes from waiting to installed with
/// nothing to poll in between.
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
    assert!(app.stack.iter().all(|screen| screen.id() != ScreenId::Review), "Esc must not reopen review");
    let screen = screen_text(&app);
    assert!(screen.contains("is installed"), "{screen}");
    assert!(screen.contains("↵ set it up"), "{screen}");
    assert!(update(&mut app, Event::Tick).is_empty(), "a finished install is not polled");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::Settings);
}

/// The helper cannot report progress, so a waiting install page asks it for none.
#[test]
fn a_waiting_install_is_not_polled_where_the_backend_reports_no_progress() {
    let (mut app, _mock) = fixture(ScreenId::Installing);
    assert!(!app.data.capabilities.install_progress);
    for _ in 0..20 {
        assert!(update(&mut app, Event::Tick).is_empty());
    }
    // A snapshot that claims it can cancel still gets no cancel from a backend that cannot.
    let Screen::Installing(snapshot) = &app.screen else { panic!("the installing fixture") };
    let mut claims = snapshot.clone();
    claims.can_cancel = true;
    update(&mut app, Event::Done(Outcome::InstallProgress(Some(claims))));
    let Screen::Installing(snapshot) = &app.screen else { panic!("still installing") };
    assert!(!snapshot.can_cancel);
}

/// A backend that reports steps, which the helper does not: a test knob on the mock.
#[test]
fn install_steps_show_where_the_backend_reports_them() {
    let (mut app, _) = loaded();
    let mut mock = MockBackend::with_stepped_installs();
    app.data = load_snapshot(&mut mock).unwrap();
    search(&mut app, &mut mock, "moneropay");
    keys(&mut app, &mut mock, &[ENTER, KeyCode::Char('i'), RIGHT, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Installing);
    assert!(screen_text(&app).contains("Step 1 of 8"), "{}", screen_text(&app));
    for _ in 0..200 {
        dispatch(&mut app, &mut mock, Event::Tick);
    }
    let screen = screen_text(&app);
    assert!(screen.contains("is installed"), "{screen}");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::Settings);
}

#[test]
fn a_failed_install_reports_that_it_was_undone() {
    let (mut app, mut mock) = loaded();
    search(&mut app, &mut mock, "paywall");
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('i')]);
    let screen = screen_text(&app);
    assert!(screen.contains("a source you added"), "{screen}");
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    for _ in 0..200 {
        dispatch(&mut app, &mut mock, Event::Tick);
    }
    let screen = screen_text(&app);
    assert!(screen.contains("wasn't installed"), "{screen}");
    assert!(screen.contains("removed"), "{screen}");
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::Search);
}

/// Only a backend that can cancel offers it, which the helper cannot: a test knob on the mock.
#[test]
fn an_install_can_be_cancelled_where_the_backend_can_cancel() {
    let (mut app, _) = fixture(ScreenId::Review);
    let mut mock = MockBackend::with_stepped_installs();
    app.data = load_snapshot(&mut mock).unwrap();
    app.stack.push(Screen::Home { selected: 0 });
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Installing);
    key(&mut app, &mut mock, ESC);
    assert_ne!(app.screen.id(), ScreenId::Installing);
    assert!(screen_text(&app).contains("Install cancelled."));
}

/// The helper installs in one request that answers minutes later. The install page shows as soon
/// as Install is confirmed, offers no cancel it cannot do, and when the owner leaves it, the
/// result still reaches them as a notice instead of being dropped.
#[test]
fn an_install_shows_at_once_and_reports_its_end_after_the_page_is_left() {
    let (mut app, _mock) = fixture(ScreenId::Review);
    app.stack.push(Screen::Home { selected: 0 });
    update(&mut app, press(RIGHT));
    let effects = update(&mut app, press(ENTER));
    assert!(matches!(effects.as_slice(), [Effect::StartInstall { .. }]), "{effects:?}");
    let Screen::Installing(snapshot) = &app.screen else { panic!("on the install page at once, not {:?}", app.screen.id()) };
    assert_eq!(snapshot.outcome, InstallOutcome::InProgress);
    let id = snapshot.id.clone();
    let screen = screen_text(&app);
    assert!(screen.contains("This can take a few minutes"), "{screen}");
    assert!(!screen.contains("cancel"), "no cancel the helper cannot do: {screen}");
    assert!(update(&mut app, press(ESC)).is_empty(), "Esc leaves rather than cancels");
    assert_eq!(app.screen.id(), ScreenId::Main);

    let mut done = InstallSnapshot::starting(&id, "1.0");
    done.outcome = InstallOutcome::Succeeded;
    update(&mut app, Event::Done(Outcome::InstallProgress(Some(done.clone()))));
    let notice = app.notice.clone().expect("the result is told");
    assert_eq!(notice.kind, NoticeKind::Ok);
    assert!(notice.text.contains(&format!("{} is installed", id.name)), "{}", notice.text);

    done.outcome = InstallOutcome::Failed("the health check failed".into());
    update(&mut app, Event::Done(Outcome::InstallProgress(Some(done))));
    let notice = app.notice.clone().expect("the failure is told");
    assert_eq!(notice.kind, NoticeKind::Error);
    assert!(notice.text.contains("the health check failed"), "{}", notice.text);
}

/// An install the backend refuses ends the install page with the reason, rather than leaving it
/// spinning.
#[test]
fn a_refused_install_ends_the_install_page_with_the_reason() {
    let (mut app, mut mock) = fixture(ScreenId::Review);
    let Screen::Review(state) = &mut app.screen else { panic!("the review fixture") };
    state.data.id.name = "no-such-app".into();
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    let Screen::Installing(snapshot) = &app.screen else { panic!("on the install page, not {:?}", app.screen.id()) };
    assert_eq!(snapshot.outcome, InstallOutcome::Failed("unknown package no-such-app".into()));
    assert!(screen_text(&app).contains("wasn't installed"), "{}", screen_text(&app));
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
    assert_eq!(app.screen, Screen::Feeds { selected: 2, keys: false });
}

// Home

#[test]
fn an_app_page_starts_and_stops_its_app_through_the_real_api() {
    let (mut app, mut mock) = loaded();
    app.screen = Screen::AppPage(AppPageState::new("cuprated"));
    key(&mut app, &mut mock, KeyCode::Char('s'));
    assert!(mock.calls.contains(&Request::StopService { id: "cuprated".to_owned() }));
    assert!(screen_text(&app).contains("Stopped"));
    key(&mut app, &mut mock, KeyCode::Char('s'));
    assert!(mock.calls.contains(&Request::StartService { id: "cuprated".to_owned() }));
}

#[test]
fn a_locked_service_cannot_be_started() {
    let (mut app, mut mock) = loaded();
    app.screen = Screen::AppPage(AppPageState::new("wallet-relay"));
    key(&mut app, &mut mock, KeyCode::Char('s'));
    assert!(!mock.calls.iter().any(|call| matches!(call, Request::StartService { .. })));
    assert!(screen_text(&app).contains("Press u to unlock"));
}

#[test]
fn home_l_opens_the_failing_service_log_as_the_attention_panel_promises() {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('l'));
    let screen = screen_text(&app);
    assert_eq!(app.screen.id(), ScreenId::Logs);
    assert!(screen.contains("Apps  ›  bell  ›  Logs"), "{screen}");
    assert!(screen.contains("connection refused: wallet-relay is locked"), "{screen}");
}

#[test]
fn a_backend_error_on_refresh_shows_and_does_not_crash() {
    let (mut app, _) = loaded();
    update(&mut app, Event::Done(Outcome::Failed("Could not read device state".to_owned())));
    assert!(screen_text(&app).contains("✕ Could not read device state"));
}

#[test]
fn tabs_cycle_both_ways_and_q_quits_from_a_tab_only() {
    let (mut app, mut mock) = loaded();
    let order = [ScreenId::Apps, ScreenId::WebSites, ScreenId::System, ScreenId::Main];
    for expected in order {
        key(&mut app, &mut mock, TAB);
        assert_eq!(app.screen.id(), expected);
    }
    key(&mut app, &mut mock, KeyCode::BackTab);
    assert_eq!(app.screen.id(), ScreenId::System);
    key(&mut app, &mut mock, KeyCode::Char('2'));
    assert_eq!(app.screen.id(), ScreenId::Apps, "number keys pick a tab");

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
    assert_eq!(state.focus, ConfirmFocus::Cancel);
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
    assert!(screen_text(&app).contains("Add a source"));
    type_text(&mut app, &mut mock, "http://insecure.example/feed");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("✕ A feed address must start with https://"));

    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "https://github.com/example/monero-tools-feed");
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("Its key"));
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
    assert!(screen_text(&app).contains("6 of 41 apps from monokulo match"));
}

#[test]
fn feeds_can_be_disabled_and_enabled_again() {
    let (mut app, mut mock) = fixture(ScreenId::Feeds);
    key(&mut app, &mut mock, KeyCode::Char('e'));
    assert_eq!(feed(&mock, "monokulo").status, FeedStatus::Disabled);
    assert!(screen_text(&app).contains("[ ] Off"));
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
    assert!(screen.contains("Update wallet-relay to 0.5.0?"), "{screen}");
    assert!(screen.contains("▲  Connect out to any internet address"), "{screen}");
    assert!(screen.contains("▸ Cancel"), "{screen}");

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
    assert!(screen_text(&app).contains("update didn't start, so"));
    let screen = screen_text(&app);
    assert!(screen.contains("▸ OK"), "{screen}");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::Updates);
    assert!(mock.updates.iter().find(|item| item.name == "moneropay").unwrap().held);
}

#[test]
fn the_update_count_drops_when_a_version_is_held() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    assert!(screen_text(&app).contains("Apps 2"));
    key(&mut app, &mut mock, KeyCode::Char('h'));
    assert!(screen_text(&app).contains("Apps 1"));
    assert!(screen_text(&app).contains("you skipped this version"));
}

#[test]
fn updating_with_nothing_selected_explains_itself() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    keys(&mut app, &mut mock, &[SPACE, DOWN, SPACE, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Updates);
    assert!(screen_text(&app).contains("Tick at least one app first."));
}

fn updates_state(app: &App) -> &UpdatesState {
    let Screen::Updates(state) = &app.screen else { panic!("on Updates, not {:?}", app.screen.id()) };
    state
}

fn selected_update(app: &App) -> String {
    app.update_rows()[updates_state(app).selected].name.clone()
}

/// Check now says what it found, and while it runs the link shows it is working and a second
/// press does not queue another check. It used to give no word at all, success or failure.
#[test]
fn check_now_shows_it_is_running_and_says_what_it_found() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    assert!(screen_text(&app).contains("Check now ›"));
    let effects = update(&mut app, press(KeyCode::Char('r')));
    assert_eq!(effects, vec![Effect::CheckUpdates]);
    let screen = screen_text(&app);
    assert!(screen.contains("Checking…") && !screen.contains("Check now ›"), "{screen}");
    assert!(update(&mut app, press(KeyCode::Char('r'))).is_empty(), "a second press waits for the first");
    for event in execute_effect(&mut mock, Effect::CheckUpdates) {
        dispatch(&mut app, &mut mock, Event::Done(event));
    }
    let count = app.update_rows().len();
    assert!(screen_text(&app).contains(&format!("{count} updates available.")), "{}", screen_text(&app));
    assert!(screen_text(&app).contains("Check now ›"), "the link is back once the check is done");

    // Up to date only once every source could be read: the crowded mock has one that cannot.
    mock.updates.clear();
    for feed in &mut mock.feeds {
        if matches!(feed.status, FeedStatus::Unreachable(_)) {
            feed.status = FeedStatus::Healthy;
        }
    }
    key(&mut app, &mut mock, KeyCode::Char('r'));
    let notice = app.notice.clone().expect("told");
    assert_eq!((notice.kind, notice.text.as_str()), (NoticeKind::Ok, "Up to date."));
}

/// On a device that cannot reach the snapshot server the system check fails every time. Check
/// now used to say only that, even when the apps' check had found an update; now it says both.
#[test]
fn a_failed_system_check_does_not_hide_what_the_apps_check_found() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    mock.feeds.retain(|feed| !matches!(feed.status, FeedStatus::Unreachable(_)));
    mock.knobs.system_check_failure = Some("could not download the snapshot manifest.".to_owned());
    let mut new = mock.updates.iter().find(|item| item.note == UpdateNote::Ready).expect("a plain update").clone();
    (new.name, new.source, new.held, new.preselected) = ("fast-sync".to_owned(), "community".to_owned(), false, true);
    mock.updates.clear();
    mock.published.push(new);
    key(&mut app, &mut mock, KeyCode::Char('r'));
    let notice = app.notice.clone().expect("told");
    assert_eq!(notice.kind, NoticeKind::Error, "something could not be checked");
    assert_eq!(notice.text, "1 update available. The system check failed: could not download the snapshot manifest.");
    assert!(updates_state(&app).checked.contains(&"fast-sync".to_owned()), "what was found is ticked all the same");
    assert!(screen_text(&app).contains("The system check failed"), "{}", screen_text(&app));

    // With nothing found, only the apps are called up to date.
    mock.updates.clear();
    key(&mut app, &mut mock, KeyCode::Char('r'));
    let notice = app.notice.clone().expect("told");
    assert_eq!(notice.text, "Apps up to date. The system check failed: could not download the snapshot manifest.");
}

/// A source the helper could not read is marked on the source, and the check goes on. Check now
/// names it, and Sources shows it with the helper's problem.
#[test]
fn check_now_names_a_source_it_could_not_read() {
    let (mut app, mut mock) = fixture(ScreenId::Feeds);
    let mut unseen = mock.updates[0].clone();
    (unseen.name, unseen.source) = ("fast-sync".to_owned(), "community".to_owned());
    mock.updates.clear();
    mock.published.push(unseen);
    mock.knobs.unreadable_feeds = vec![("community".to_owned(), "the feed index could not be downloaded".to_owned())];
    let events = execute_effect(&mut mock, Effect::CheckUpdates);
    for event in events {
        dispatch(&mut app, &mut mock, Event::Done(event));
    }
    let notice = app.notice.clone().expect("told");
    assert_eq!(notice.kind, NoticeKind::Error);
    assert!(notice.text.starts_with("No updates in the sources that could be read."), "{}", notice.text);
    assert!(notice.text.contains("Could not read community: the feed index could not be downloaded."), "{}", notice.text);
    assert!(!notice.text.contains("Up to date"), "{}", notice.text);
    assert!(mock.updates.is_empty(), "what the unread source published is not seen yet");
    let screen = screen_text(&app);
    assert!(screen.contains("Couldn't reach it: the feed index could not be downloaded"), "{screen}");

    // Once it can be read again, its update is found.
    mock.knobs.unreadable_feeds.clear();
    mock.feeds.iter_mut().filter(|feed| feed.name == "community").for_each(|feed| feed.status = FeedStatus::Healthy);
    for event in execute_effect(&mut mock, Effect::CheckUpdates) {
        dispatch(&mut app, &mut mock, Event::Done(event));
    }
    assert_eq!(mock.updates.len(), 1);
}

/// A failed check, and a failed source refresh, switch or removal, say why instead of nothing.
#[test]
fn a_failed_check_or_source_change_says_why() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    mock.knobs.feeds_failure = Some("official: the feed did not answer".to_owned());
    key(&mut app, &mut mock, KeyCode::Char('r'));
    let notice = app.notice.clone().expect("told");
    assert_eq!(notice.kind, NoticeKind::Error);
    assert!(notice.text.contains("Could not check for updates: official: the feed did not answer"), "{}", notice.text);
    let name = mock.feeds[0].name.clone();
    for effect in [Effect::RefreshFeeds, Effect::ToggleFeed(name.clone()), Effect::RemoveFeed(name)] {
        let events = execute_effect(&mut mock, effect.clone());
        assert!(
            events.iter().any(|event| matches!(event, Outcome::Failed(text) if text.contains("did not answer"))),
            "{effect:?}: {events:?}"
        );
    }
}

/// After a check, new updates are ticked as when the list opens, ones that went away are
/// unticked, and the selection stays on the same app though the rows moved.
#[test]
fn a_check_ticks_new_updates_and_keeps_the_selection_on_its_app() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    key(&mut app, &mut mock, DOWN);
    key(&mut app, &mut mock, DOWN);
    let selected = selected_update(&app);
    let gone = app.update_rows()[0].name.clone();
    assert!(updates_state(&app).checked.contains(&gone), "the first update starts ticked");
    let mut new = mock.updates[0].clone();
    new.name = "fast-sync".to_owned();
    new.preselected = true;
    mock.published.push(new);
    mock.updates.retain(|item| item.name != gone);
    key(&mut app, &mut mock, KeyCode::Char('r'));
    assert_eq!(selected_update(&app), selected, "still on the same app");
    let checked = &updates_state(&app).checked;
    assert!(checked.contains(&"fast-sync".to_owned()), "{checked:?}");
    assert!(!checked.contains(&gone), "{checked:?}");
}

fn reload(app: &mut App, mock: &mut MockBackend) {
    app.data = load_snapshot(mock).unwrap();
}

fn passphrase_calls(mock: &MockBackend) -> Vec<String> {
    mock.calls
        .iter()
        .filter_map(|call| match call {
            Request::ApplyUpdatesWithPassphrase { passphrase, .. } => Some(passphrase.expose().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_update_that_grows_encrypted_storage_asks_for_the_device_passphrase_first() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    mock.updates[0].grows = Some("256 MB to 512 MB of encrypted storage".to_owned());
    reload(&mut app, &mut mock);
    assert!(screen_text(&app).contains("needs the device passphrase"), "{}", screen_text(&app));

    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::Updates);
    let screen = screen_text(&app);
    assert!(screen.contains("need more locked storage"), "{screen}");
    assert!(screen.contains("moneropay: 256 MB to 512 MB of encrypted storage"), "{screen}");
    assert!(screen.contains("device passphrase"), "{screen}");
    assert!(screen.contains("Esc cancel"), "{screen}");
    assert_eq!(mock.updates.len(), 4, "nothing is applied before the passphrase");

    // Every key types: q does not quit, and the passphrase never shows.
    type_text(&mut app, &mut mock, "q");
    assert!(!app.should_quit);
    assert!(!screen_text(&app).contains("Device passphrase   q"));
    key(&mut app, &mut mock, ESC);
    assert!(!screen_text(&app).contains("Encrypted storage grows"), "Esc cancels the whole batch");
    assert_eq!(mock.updates.len(), 4);
    assert!(passphrase_calls(&mock).is_empty());

    key(&mut app, &mut mock, ENTER);
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("Enter the device passphrase"), "an empty passphrase is not sent");
    type_text(&mut app, &mut mock, "wrong horse");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::UpdateFailed);
    assert!(screen_text(&app).contains("did not unlock"), "{}", screen_text(&app));
    assert_eq!(mock.updates.len(), 4);

    key(&mut app, &mut mock, ESC);
    key(&mut app, &mut mock, ENTER);
    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::UpdateFailed);
    assert!(screen_text(&app).contains("update didn't start"), "{}", screen_text(&app));
    assert_eq!(passphrase_calls(&mock), ["wrong horse", MOCK_PASSPHRASE]);
}

#[test]
fn a_reviewed_update_that_grows_storage_still_asks_for_the_passphrase() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    mock.updates[2].grows = Some("256 MB to 320 MB of encrypted storage".to_owned());
    reload(&mut app, &mut mock);
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Review);
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert_eq!(app.screen.id(), ScreenId::Updates, "back on Updates, asking for the passphrase");
    let screen = screen_text(&app);
    assert!(screen.contains("wallet-relay: 256 MB to 320 MB of encrypted storage"), "{screen}");
    assert_eq!(mock.updates.len(), 4);
    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(app.screen.id(), ScreenId::UpdateFailed);
    assert!(!mock.updates.iter().any(|item| item.name == "wallet-relay"));
}

#[test]
fn a_withdrawn_system_snapshot_is_explained_before_it_is_replaced() {
    let (mut app, mut mock) = fixture(ScreenId::Updates);
    mock.updates[1].note = UpdateNote::Withdrawal("2026.11".to_owned());
    reload(&mut app, &mut mock);
    let screen = text(&draw(&app, 120, 40).0);
    assert!(screen.contains("Replaces 2026.11, which its publisher withdrew."), "{screen}");
    assert!(screen.contains("Update the Nodo"), "{screen}");
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
    assert!(screen.contains("automatic for 3"), "{screen}");
    assert!(screen.contains("Automatic updates ›"));
}

#[test]
fn packages_default_to_auto_update_off() {
    let entry = AutoUpdateEntry::new("anything", "official", false, "");
    assert!(!entry.enabled);

    // A freshly installed package joins the list switched off.
    let mut mock = MockBackend::new();
    let before = mock.auto_update.enabled_count();
    let id = PackageId::new("community", "node-exporter");
    let InstallGate::Review(review) = mock.install_gate(&id) else { panic!("node-exporter reaches review") };
    assert!(mock.start_install(&id, None, None).is_err(), "nothing installs without the review's approval");
    let done = mock.start_install(&id, review.approval.as_ref(), None).unwrap();
    assert_eq!(done.outcome, InstallOutcome::Succeeded);
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
    assert!(screen.contains("automatic for 2"), "{screen}");
    assert!(screen.contains("✓ Auto-update choices saved."), "{screen}");

    key(&mut app, &mut mock, KeyCode::Char('a'));
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE, DOWN, SPACE, ENTER]);
    assert_eq!(mock.auto_update.enabled_count(), 0);
    assert!(screen_text(&app).contains("automatic updates are off"));
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
    assert!(screen_text(&app).contains("automatic for 3"));
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
    assert!(screen_text(&app).contains("01:30 and 02:30"));
    assert_eq!(mock.auto_update.window_start, "03:00", "not saved until the screen is saved");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(mock.auto_update.window_start, "01:30");
    assert_eq!(mock.auto_update.next_run, "Next run tonight at 01:30");
}

#[test]
fn frequency_cycles_and_history_opens() {
    let (mut app, mut mock) = at_auto_update();
    key(&mut app, &mut mock, SPACE);
    assert!(screen_text(&app).contains("Weekly"));
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
    update(&mut app, Event::Done(Outcome::LogsLoaded { source: "tor".to_owned(), view: LogView::default() }));
    let Screen::Logs(state) = &app.screen else { panic!("expected logs") };
    assert!(!state.view.lines.is_empty());
}

#[test]
fn copying_a_report_says_it_is_not_available_as_on_the_device() {
    let (mut app, mut mock) = fixture(ScreenId::Logs);
    key(&mut app, &mut mock, KeyCode::Char('y'));
    assert!(screen_text(&app).contains("Copying reports is not available yet."), "{}", screen_text(&app));

    // A backend that can copy, through the test knob, says where the report went.
    let (mut app, mut mock) = fixture(ScreenId::Logs);
    mock.knobs.capabilities.copy_report = true;
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
        dispatch(&mut app, &mut mock, Event::Done(event));
    }
    assert_eq!(app.screen.id(), ScreenId::NoDrive);
    assert!(screen_text(&app).contains("The data drive isn't connected"));
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
        BackupStep::Unlock { name: "nodo-1762560000.nodobak".into(), passphrase: nodo_ui_core::secret::Input::secret() };
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
    assert!(screen_text(&app).contains("device passphrase"));
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
        dispatch(&mut app, &mut mock, Event::Done(event));
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
        "System  ›  Backups",
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
    dispatch(&mut app, &mut mock, Event::Key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE)));
    assert_eq!(app.screen.id(), ScreenId::WebSites);
    key(&mut app, &mut mock, KeyCode::Char('n'));
    assert_eq!(app.screen.id(), ScreenId::Onion);
    (app, mock)
}

#[test]
fn n_on_web_opens_the_onion_screen_with_the_node_and_sites() {
    let (app, mock) = open_onion();
    let screen = screen_text(&app);
    assert!(screen.contains("Web  ›  Onion addresses"), "{screen}");
    assert!(screen.contains("nod…xyz.onion:18081"), "{screen}");
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
    assert!(screen_text(&app).contains("nod…xyz.onion:18081"));
}

#[test]
fn the_node_onion_is_offered_with_no_setting_to_turn_on_first() {
    // cuprated always serves restricted RPC, so the node can go on Tor straight away.
    let (mut app, mut mock) = open_onion();
    key(&mut app, &mut mock, SPACE);
    assert!(!mock.node_onion.enabled);
    mock.calls.clear();
    key(&mut app, &mut mock, SPACE);
    assert!(mock.calls.contains(&Request::SetNodeOnion { enabled: true }), "{:?}", mock.calls);
    assert!(mock.node_onion.enabled);
    assert!(!screen_text(&app).contains("settings first"), "{}", screen_text(&app));
}

#[test]
fn a_node_without_a_restricted_rpc_port_is_never_put_on_tor() {
    let (mut app, mut mock) = open_onion();
    key(&mut app, &mut mock, SPACE);
    mock.node_onion.restricted_rpc = false;
    key(&mut app, &mut mock, KeyCode::Char('g'));
    assert!(screen_text(&app).contains("no restricted RPC port to serve"), "{}", screen_text(&app));
    mock.calls.clear();
    key(&mut app, &mut mock, SPACE);
    assert!(screen_text(&app).contains("does not say which port serves its restricted RPC"), "{}", screen_text(&app));
    assert!(!mock.calls.iter().any(|c| matches!(c, Request::SetNodeOnion { .. })), "nothing was asked");
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

// Network

fn open_network() -> (App, MockBackend) {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('w'));
    assert_eq!(app.screen.id(), ScreenId::Network);
    (app, mock)
}

fn wifi_calls(mock: &MockBackend) -> Vec<&Request> {
    mock.calls
        .iter()
        .filter(|c| matches!(c, Request::JoinWifi(_) | Request::ForgetWifi { .. } | Request::SetWifiEnabled { .. }))
        .collect()
}

#[test]
fn w_on_home_opens_the_network_screen_from_the_helper() {
    let (mut app, mut mock) = open_network();
    let screen = screen_text(&app);
    assert!(screen.contains("System  ›  Network"), "{screen}");
    assert!(screen.contains("● Joined home-5G"), "{screen}");
    assert!(screen.contains("travel-hotspot"), "{screen}");
    assert!(screen.contains("K380 Keyboard"), "{screen}");
    assert!(mock.calls.iter().any(|c| matches!(c, Request::WifiStatus)));
    assert!(mock.calls.iter().any(|c| matches!(c, Request::BluetoothStatus)));
    key(&mut app, &mut mock, ESC);
    assert_eq!(app.screen.id(), ScreenId::Main);
}

#[test]
fn scanning_lists_networks_and_a_wrong_passphrase_saves_nothing() {
    let (mut app, mut mock) = open_network();
    key(&mut app, &mut mock, KeyCode::Char('s'));
    assert!(mock.calls.iter().any(|c| matches!(c, Request::ScanWifi)));
    let screen = screen_text(&app);
    assert!(screen.contains("neighbour") && screen.contains("WPA3"), "{screen}");
    // The joined network says so instead of joining again.
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("already joined to home-5G"), "{}", screen_text(&app));
    keys(&mut app, &mut mock, &[DOWN, DOWN, ENTER]);
    let screen = screen_text(&app);
    assert!(screen.contains("Join neighbour") && screen.contains("Passphrase"), "{screen}");
    // An empty passphrase never reaches the helper.
    key(&mut app, &mut mock, ENTER);
    assert!(screen_text(&app).contains("Type the passphrase of neighbour."));
    type_text(&mut app, &mut mock, "wrong words");
    let screen = screen_text(&app);
    assert!(screen.contains("•••••••••••") && !screen.contains("wrong words"), "{screen}");
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("neighbour refused the passphrase. Nothing was saved."), "{screen}");
    assert_eq!(mock.wifi.saved.len(), 2);
    assert!(!format!("{:?}", app.screen).contains("wrong words"), "no state keeps the passphrase in clear");
}

#[test]
fn the_right_passphrase_joins_and_says_the_address() {
    let (mut app, mut mock) = open_network();
    key(&mut app, &mut mock, KeyCode::Char('s'));
    keys(&mut app, &mut mock, &[DOWN, DOWN, ENTER]);
    type_text(&mut app, &mut mock, "not ours at all");
    key(&mut app, &mut mock, ENTER);
    let screen = screen_text(&app);
    assert!(screen.contains("Joined neighbour. Its address is 192.168.1.57."), "{screen}");
    assert!(screen.contains("● Joined neighbour"), "{screen}");
    let Some(Request::JoinWifi(join)) = wifi_calls(&mock).last().copied() else { panic!("{:?}", mock.calls) };
    assert_eq!(join.ssid, "neighbour");
    assert_eq!(join.security, nodo_api::WifiSecurity::Wpa3Personal);
    assert_eq!(join.passphrase.as_ref().map(|p| p.expose()), Some("not ours at all"));
    assert!(!join.accept_open);
    assert!(mock.wifi.saved.iter().any(|s| s.ssid == "neighbour"));
}

#[test]
fn an_open_network_asks_first_with_cancel_focused() {
    let (mut app, mut mock) = open_network();
    key(&mut app, &mut mock, KeyCode::Char('s'));
    keys(&mut app, &mut mock, &[DOWN, ENTER]);
    let modal = app.modal.clone().expect("a question");
    assert!(modal.title.contains("open network"), "{modal:?}");
    assert!(modal.body[0].contains("anyone nearby can read"), "{modal:?}");
    assert!(!modal.confirm_focused);
    key(&mut app, &mut mock, ENTER);
    assert!(wifi_calls(&mock).is_empty(), "Enter on Cancel joins nothing");
    keys(&mut app, &mut mock, &[ENTER, RIGHT, ENTER]);
    let Some(Request::JoinWifi(join)) = wifi_calls(&mock).last().copied() else { panic!("{:?}", mock.calls) };
    assert!(join.accept_open && join.passphrase.is_none());
    assert!(screen_text(&app).contains("● Joined Cafe Libre"), "{}", screen_text(&app));
}

#[test]
fn enterprise_networks_are_refused_without_asking_the_helper() {
    let (mut app, mut mock) = open_network();
    key(&mut app, &mut mock, KeyCode::Char('s'));
    keys(&mut app, &mut mock, &[DOWN, DOWN, DOWN, ENTER]);
    assert!(screen_text(&app).contains("WPA Enterprise"), "{}", screen_text(&app));
    assert!(wifi_calls(&mock).is_empty());
}

#[test]
fn forgetting_asks_and_only_saved_networks_can_be_forgotten() {
    let (mut app, mut mock) = open_network();
    key(&mut app, &mut mock, KeyCode::Char('s'));
    keys(&mut app, &mut mock, &[DOWN, KeyCode::Char('f')]);
    assert!(app.modal.is_none());
    assert!(screen_text(&app).contains("Cafe Libre is not saved."));
    // travel-hotspot is saved but out of range, at the end of the list.
    keys(&mut app, &mut mock, &[DOWN, DOWN, DOWN, KeyCode::Char('f')]);
    let modal = app.modal.clone().expect("a question");
    assert_eq!(modal.title, "Forget travel-hotspot?");
    assert!(!modal.confirm_focused);
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert!(mock.calls.iter().any(|c| matches!(c, Request::ForgetWifi { ssid } if ssid == "travel-hotspot")));
    let screen = screen_text(&app);
    assert!(screen.contains("Forgot travel-hotspot.") && !screen.contains("saved, not in range"), "{screen}");
}

#[test]
fn turning_wifi_off_while_it_may_carry_this_session_asks_first() {
    let (mut app, mut mock) = open_network();
    mock.wifi.wired_up = false;
    key(&mut app, &mut mock, KeyCode::Char('g'));
    assert!(screen_text(&app).contains("▲ Wired link down"));
    key(&mut app, &mut mock, KeyCode::Char('o'));
    let modal = app.modal.clone().expect("a question");
    assert!(modal.body[0].contains("may cut the connection you are using"), "{modal:?}");
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert!(mock.calls.iter().any(|c| matches!(c, Request::SetWifiEnabled { enabled: false })));
    assert!(screen_text(&app).contains("[ ] off"), "{}", screen_text(&app));
    // Off asks nothing to turn on again.
    key(&mut app, &mut mock, KeyCode::Char('o'));
    assert!(app.modal.is_none());
    assert!(mock.wifi.enabled);
}

#[test]
fn bluetooth_scans_pairs_connects_and_removes() {
    let (mut app, mut mock) = open_network();
    key(&mut app, &mut mock, RIGHT);
    assert!(screen_text(&app).contains("↵ pair or connect"), "{}", screen_text(&app));
    key(&mut app, &mut mock, KeyCode::Char('s'));
    assert!(screen_text(&app).contains("Kitchen Speaker"), "{}", screen_text(&app));
    keys(&mut app, &mut mock, &[DOWN, DOWN, ENTER]);
    assert!(mock.calls.iter().any(|c| matches!(c, Request::PairBluetooth { address } if address == "AA:BB:CC:00:00:03")));
    assert!(
        screen_text(&app).contains("Paired with Kitchen Speaker. It is trusted and connected."),
        "{}",
        screen_text(&app)
    );
    // The headphones are paired and not connected: Enter connects them.
    keys(&mut app, &mut mock, &[UP, ENTER]);
    assert!(mock.calls.iter().any(|c| matches!(c, Request::ConnectBluetooth { connect: true, .. })));
    key(&mut app, &mut mock, KeyCode::Char('x'));
    let modal = app.modal.clone().expect("a question");
    assert_eq!(modal.title, "Remove WH-1000XM4?");
    assert!(!modal.confirm_focused);
    keys(&mut app, &mut mock, &[RIGHT, ENTER]);
    assert!(mock
        .calls
        .iter()
        .any(|c| matches!(c, Request::RemoveBluetooth { address } if address == "AA:BB:CC:00:00:02")));
    assert!(!screen_text(&app).contains("WH-1000XM4"));
}

#[test]
fn a_pairing_shows_the_passkey_to_type_and_polls_until_done() {
    let (mut app, mut mock) = open_network();
    key(&mut app, &mut mock, RIGHT);
    let mut status = mock.bluetooth.clone();
    status.pairing = Some(nodo_api::BluetoothPairing {
        address: "AA:BB:CC:00:00:01".to_owned(),
        stage: nodo_api::PairingStage::TypePasskey("482913".to_owned()),
    });
    dispatch(&mut app, &mut mock, Event::Done(Outcome::BluetoothChanged { result: Ok(status), quiet: false }));
    let screen = screen_text(&app);
    assert!(screen.contains("Type 482913 on K380 Keyboard, then press its Enter key."), "{screen}");
    // Every second the screen asks again while the pairing waits for the owner.
    let mut polled = false;
    for _ in 0..TICKS_PER_SECOND {
        polled |= update(&mut app, Event::Tick).contains(&Effect::PollBluetooth);
    }
    assert!(polled);
}

#[test]
fn radios_that_are_off_or_missing_say_so() {
    let (mut app, mut mock) = loaded();
    mock.wifi = nodo_api::WifiStatus {
        unavailable: Some("No Wi-Fi hardware was found.".to_owned()),
        interface: None,
        enabled: false,
        link: nodo_api::WifiLink::Off,
        ssid: None,
        signal_dbm: None,
        frequency_mhz: None,
        security: None,
        address: None,
        wired_up: true,
        saved: Vec::new(),
    };
    mock.bluetooth.enabled = false;
    key(&mut app, &mut mock, KeyCode::Char('w'));
    let screen = screen_text(&app);
    assert!(screen.contains("▲ No Wi-Fi hardware was found."), "{screen}");
    assert!(screen.contains("Nothing can pair with the device while it is off."), "{screen}");
    key(&mut app, &mut mock, KeyCode::Char('o'));
    assert!(!mock.calls.iter().any(|c| matches!(c, Request::SetWifiEnabled { .. })));
    key(&mut app, &mut mock, KeyCode::Char('s'));
    assert!(!mock.calls.iter().any(|c| matches!(c, Request::ScanWifi)));
    key(&mut app, &mut mock, RIGHT);
    key(&mut app, &mut mock, KeyCode::Char('o'));
    assert!(mock.bluetooth.enabled);
    assert!(screen_text(&app).contains("Not discoverable"), "{}", screen_text(&app));
}

#[test]
fn radios_the_helper_cannot_read_say_why_and_can_be_asked_again() {
    let (mut app, mut mock) = loaded();
    mock.knobs.radios_failure = Some("the helper is not running".to_owned());
    key(&mut app, &mut mock, KeyCode::Char('w'));
    let screen = screen_text(&app);
    assert!(!screen.contains("Asking the helper"), "{screen}");
    assert_eq!(screen.matches("The helper could not say: the helper is not running").count(), 2, "{screen}");
    assert!(screen.contains("Try again") && screen.contains("r try again"), "{screen}");
    let Screen::Network(state) = &app.screen else { panic!("expected network") };
    assert!(state.busy.is_none(), "{state:?}");

    // The keyboard asks again...
    mock.calls.clear();
    key(&mut app, &mut mock, KeyCode::Char('r'));
    assert!(mock.calls.iter().any(|c| matches!(c, Request::WifiStatus)));
    assert!(screen_text(&app).contains("could not say"));

    // ...and so does a click, which finds the helper back.
    mock.knobs.radios_failure = None;
    click(&mut app, &mut mock, "Try again");
    let screen = screen_text(&app);
    assert!(screen.contains("● Joined home-5G"), "{screen}");
    assert!(screen.contains("K380 Keyboard"), "{screen}");
    assert!(!screen.contains("Try again"), "{screen}");
}

#[test]
fn radios_with_no_answer_say_so_after_a_while_instead_of_asking_forever() {
    let (mut app, _mock) = loaded();
    // The answer never arrives: the effect is left unperformed, as behind a slow request.
    assert_eq!(update(&mut app, press(KeyCode::Char('w'))), vec![Effect::LoadNetwork]);
    assert!(screen_text(&app).contains("Asking the helper…"));
    for _ in 0..NO_ANSWER_TICKS - 1 {
        update(&mut app, Event::Tick);
    }
    assert!(screen_text(&app).contains("Asking the helper…"), "not before the wait is over");
    update(&mut app, Event::Tick);
    let screen = screen_text(&app);
    assert!(!screen.contains("Asking the helper"), "{screen}");
    assert_eq!(screen.matches("No answer from the helper yet.").count(), 2, "{screen}");
    // Other keys wait for the answer, but asking again does not.
    assert!(update(&mut app, press(KeyCode::Char('s'))).is_empty());
    assert_eq!(update(&mut app, press(KeyCode::Char('r'))), vec![Effect::LoadNetwork]);
    assert!(screen_text(&app).contains("Asking the helper…"));
}

#[test]
fn the_help_overlay_names_the_network_screen() {
    let (mut app, mut mock) = loaded();
    key(&mut app, &mut mock, KeyCode::Char('?'));
    let overlay = app.overlay.clone().expect("help");
    assert!(overlay.doc.lines.iter().any(|l| l.contains("Wi-Fi and Bluetooth, from Home")));
}

#[test]
fn connecting_the_same_account_again_says_so_and_how_to_find_a_missing_domain() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, KeyCode::Char('p'));
    let screen = screen_text(&app);
    assert!(
        screen.contains("Missing a domain?") && screen.contains("API Access"),
        "the list says where missing domains are: {screen}"
    );
    // A second Porkbun connection with the same key lists only what the first already holds.
    let again = ConnectedProvider {
        id: "porkbun-2".to_owned(),
        name: "Porkbun".to_owned(),
        domains: vec![ConnectedDomain {
            name: "example.org".to_owned(),
            used_by: Vec::new(),
            held_by: Some("Porkbun".to_owned()),
        }],
        lists_domains: true,
        problem: None,
    };
    update(&mut app, Event::Done(Outcome::ProviderAdded(Ok(again))));
    let screen = screen_text(&app);
    assert!(
        screen.contains("already connected through Porkbun"),
        "{screen}"
    );
    assert!(
        screen.contains("If it is the same account, remove this one"),
        "{screen}"
    );
    assert!(screen.contains("Missing a domain?"), "{screen}");
}

// A site's other names, on its Hosting screen

fn redirect(host: &str) -> nodo_api::PublicAlias {
    nodo_api::PublicAlias { host: host.to_owned(), mode: nodo_api::AliasMode::Redirect }
}

fn serve(host: &str) -> nodo_api::PublicAlias {
    nodo_api::PublicAlias { host: host.to_owned(), mode: nodo_api::AliasMode::Serve }
}

/// The Hosting screen tall enough that nothing scrolls away.
fn hosting_text(app: &App) -> String {
    text(&draw(app, 100, 70).0)
}

fn hosting(app: &App) -> &ExposureState {
    let Screen::WebExposure(state) = &app.screen else { panic!("expected hosting, got {:?}", app.screen.id()) };
    state
}

/// Moves the keyboard focus down to `target`, as Tab would, failing if it is not on the way.
fn focus_on(app: &mut App, mock: &mut MockBackend, target: HostingFocus) {
    for _ in 0..30 {
        if hosting(app).focus() == target {
            return;
        }
        key(app, mock, DOWN);
    }
    panic!("{target:?} is not reachable with the keyboard: {:?}", hosting(app).order());
}

/// A site's Hosting with the internet ticked, before it has gone public. The site is the `row`th
/// on the Web tab; `prepare` changes the device first.
fn hosting_of(row: usize, name: &str, prepare: impl FnOnce(&mut MockBackend)) -> (App, MockBackend) {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    prepare(&mut mock);
    app.data = load_snapshot(&mut mock).unwrap();
    mock.calls.clear();
    for _ in 0..row {
        key(&mut app, &mut mock, DOWN);
    }
    key(&mut app, &mut mock, ENTER);
    assert_eq!(hosting(&app).site, name);
    keys(&mut app, &mut mock, &[DOWN, DOWN, SPACE]);
    assert!(hosting(&app).draft.public);
    (app, mock)
}

/// monokulo-pay's Hosting, going public.
fn pay_hosting(prepare: impl FnOnce(&mut MockBackend)) -> (App, MockBackend) {
    hosting_of(1, "monokulo-pay", prepare)
}

fn site_of<'a>(mock: &'a MockBackend, name: &str) -> &'a Site {
    mock.web.sites.iter().find(|s| s.package == name).unwrap()
}

fn pay(mock: &MockBackend) -> &Site {
    site_of(mock, "monokulo-pay")
}

/// The sample shop answers at example.com; these flows give that name to monokulo-pay.
fn without_shop_aliases(mock: &mut MockBackend) {
    mock.web.sites.iter_mut().for_each(|site| site.public_aliases.clear());
}

#[test]
fn an_apex_redirect_is_added_saved_and_listed_on_the_going_public_confirmation() {
    // moneropay, whose checks pass in the mock (monokulo-pay's find a shared address).
    let (mut app, mut mock) = hosting_of(2, "moneropay", without_shop_aliases);
    assert!(hosting_text(&app).contains("Also answers at"), "{}", hosting_text(&app));
    // The box left empty is the domain itself, and a new name redirects unless told otherwise.
    focus_on(&mut app, &mut mock, HostingFocus::AliasAddress);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(hosting(&app).aliases, [redirect("example.com")]);
    assert_eq!(hosting(&app).focus(), HostingFocus::AliasAddress, "the focus stays on the box for the next name");
    assert!(hosting_text(&app).contains("example.com  → moneropay.example.com"), "{}", hosting_text(&app));
    assert!(site_of(&mock, "moneropay").public_aliases.is_empty(), "nothing is saved before Save");

    focus_on(&mut app, &mut mock, HostingFocus::Save);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(site_of(&mock, "moneropay").public_aliases, [redirect("example.com")]);
    assert_eq!(app.screen.id(), ScreenId::WebConfirm, "the names were saved, then the checks passed");
    let screen = hosting_text(&app);
    assert!(screen.contains("and https://example.com  redirects to moneropay.example.com"), "{screen}");
    assert!(screen.contains("Public certificate logs list moneropay.example.com, example.com permanently."), "{screen}");
    keys(&mut app, &mut mock, &[TAB, ENTER]);
    assert!(site_of(&mock, "moneropay").exposure.public, "confirmed with the primary host, as the helper wants");
}

#[test]
fn a_name_under_a_second_providers_domain_can_serve_the_same_site() {
    let (mut app, mut mock) = pay_hosting(|mock| {
        mock.add_provider("Gandi", None, &nodo_ui_core::secret::Secret::new("k"), &nodo_ui_core::secret::Secret::new("s")).unwrap();
    });
    focus_on(&mut app, &mut mock, HostingFocus::AliasDomain);
    key(&mut app, &mut mock, ENTER);
    assert!(hosting(&app).picking_alias, "the same domain list as the main address");
    assert!(hosting_text(&app).contains("myshop.net"), "{}", hosting_text(&app));
    keys(&mut app, &mut mock, &[DOWN, ENTER]);
    assert_eq!(hosting(&app).alias_domain, "example.org");
    assert_eq!(hosting(&app).domain, "example.com", "the main address keeps its domain");
    focus_on(&mut app, &mut mock, HostingFocus::AliasAddress);
    type_text(&mut app, &mut mock, "pay");
    focus_on(&mut app, &mut mock, HostingFocus::AliasMode);
    key(&mut app, &mut mock, SPACE);
    focus_on(&mut app, &mut mock, HostingFocus::AliasAdd);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(hosting(&app).aliases, [serve("pay.example.org")]);
    assert!(hosting_text(&app).contains("pay.example.org  same site"), "{}", hosting_text(&app));
    focus_on(&mut app, &mut mock, HostingFocus::Save);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(pay(&mock).public_aliases, [serve("pay.example.org")]);
    assert_eq!(pay(&mock).public_host.as_deref(), Some("pay.example.com"));
}

#[test]
fn a_public_site_can_lose_a_name_but_not_gain_one() {
    let (mut app, mut mock) = fixture(ScreenId::WebSites);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(hosting(&app).site, "shop");
    assert_eq!(hosting(&app).aliases, [redirect("example.com")]);
    let order = hosting(&app).order();
    assert!(!order.contains(&HostingFocus::AliasAdd) && !order.contains(&HostingFocus::AliasAddress), "{order:?}");
    focus_on(&mut app, &mut mock, HostingFocus::Alias(0));
    assert!(hosting_text(&app).contains("take the site off the internet first"), "{}", hosting_text(&app));
    key(&mut app, &mut mock, KeyCode::Char('x'));
    assert!(hosting(&app).aliases.is_empty());
    focus_on(&mut app, &mut mock, HostingFocus::Save);
    key(&mut app, &mut mock, ENTER);
    let shop = mock.web.sites.iter().find(|s| s.package == "shop").unwrap();
    assert!(shop.public_aliases.is_empty() && shop.exposure.public, "removed while it stays public");
    assert_eq!(app.notice.as_ref().map(|n| n.text.as_str()), Some("Saved."));
    assert_eq!(app.screen.id(), ScreenId::WebSites);
}

#[test]
fn a_saved_name_outside_the_connected_domains_is_refused_and_nothing_goes_on() {
    // A name saved while its provider was connected; the provider has gone since.
    let (mut app, mut mock) = pay_hosting(|mock| {
        without_shop_aliases(mock);
        mock.web.sites[1].public_aliases = vec![redirect("old.myshop.net")];
    });
    assert_eq!(hosting(&app).aliases, [redirect("old.myshop.net")]);
    focus_on(&mut app, &mut mock, HostingFocus::AliasAddress);
    type_text(&mut app, &mut mock, "www");
    key(&mut app, &mut mock, ENTER);
    let error = hosting(&app).alias_error.clone().expect("the new name is checked with the saved ones");
    assert!(error.contains("old.myshop.net is not under a domain of a connected provider"), "{error}");
    // Going public with the stale name is refused by the device before any check runs.
    focus_on(&mut app, &mut mock, HostingFocus::Save);
    key(&mut app, &mut mock, ENTER);
    let notice = app.notice.clone().expect("the refusal is told");
    assert_eq!(notice.kind, NoticeKind::Error);
    assert!(notice.text.contains("old.myshop.net is not under a domain of a connected provider"), "{}", notice.text);
    assert_eq!(app.screen.id(), ScreenId::WebExposure, "the screen stays, to fix the name");
    assert!(!mock.calls.iter().any(|c| matches!(c, Request::SetExposure { .. })), "nothing went on after the refusal");
    assert_eq!(pay(&mock).public_aliases, [redirect("old.myshop.net")]);
    assert!(!pay(&mock).exposure.public);
}

#[test]
fn a_name_twice_or_another_sites_name_is_refused_on_the_spot() {
    let (mut app, mut mock) = pay_hosting(|_| {});
    // The shop already answers at example.com.
    focus_on(&mut app, &mut mock, HostingFocus::AliasAddress);
    key(&mut app, &mut mock, ENTER);
    assert_eq!(hosting(&app).alias_error.as_deref(), Some("example.com is already an address of shop."));
    assert!(hosting_text(&app).contains("example.com is already an address of shop."), "{}", hosting_text(&app));
    assert!(hosting(&app).aliases.is_empty());
    type_text(&mut app, &mut mock, "www");
    assert_eq!(hosting(&app).alias_error, None, "typing clears the refusal");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(hosting(&app).aliases, [redirect("www.example.com")]);
    type_text(&mut app, &mut mock, "www");
    key(&mut app, &mut mock, ENTER);
    assert_eq!(hosting(&app).alias_error.as_deref(), Some("www.example.com is listed twice."));
    type_text(&mut app, &mut mock, "");
    dispatch(&mut app, &mut mock, ctrl('u'));
    type_text(&mut app, &mut mock, "pay");
    key(&mut app, &mut mock, ENTER);
    assert!(hosting(&app).alias_error.as_deref().is_some_and(|e| e.contains("main address")), "{:?}", hosting(&app).alias_error);
    assert_eq!(hosting(&app).aliases.len(), 1);
}
