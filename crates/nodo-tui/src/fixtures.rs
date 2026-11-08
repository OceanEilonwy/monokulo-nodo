//! Builds the exact state each design file shows, for rendering tests and `--screen`.
//!
//! Fixtures reach their state through the same mock data and, where a flow matters, through
//! `dispatch`, so they do not drift away from what the app can really do.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nodo_api::Exposure;

use crate::app::*;
use crate::mock::{install_snapshot, reachability_checks, sample_references, MockBackend};
use crate::model::*;
use crate::runtime::load_snapshot;
use crate::secret::Input;
use crate::update::search_state;

/// A loaded app on the given screen, plus the device behind it.
pub fn fixture(id: ScreenId) -> (App, MockBackend) {
    let mut mock = match id {
        ScreenId::NoDrive => MockBackend::with_missing_drive(),
        _ => MockBackend::new(),
    };
    // The design shows these screens part-way through shop going public.
    if matches!(id, ScreenId::WebExposure | ScreenId::WebCheck | ScreenId::WebConfirm | ScreenId::WebCgnat) {
        mock.web.sites[0].exposure = Exposure { local: true, onion: true, public: false };
    }
    let mut app = App::new();
    app.data = load_snapshot(&mut mock).expect("the mock always answers");
    mock.calls.clear();
    app.screen = screen(id, &app, &mock);
    (app, mock)
}

fn gate(mock: &MockBackend, feed: &str, name: &str) -> InstallGate {
    mock.install_gate(&PackageId::new(feed, name))
}

fn typed(text: &str) -> Input {
    Input::plain(text)
}

fn settings_design_state(mock: &MockBackend) -> SettingsState {
    let form = mock.settings_form("moneropay").expect("sample form");
    let mut state = SettingsState::new(form);
    state.draft[0] = FieldValue::Reference {
        path: "monero-daemon.public_rpc_url".to_owned(),
        resolved: "http://127.0.0.1:18081".to_owned(),
    };
    state.draft[3] = FieldValue::Text("48edfHu7V9Z84Y".to_owned());
    state.selected = 3;
    state
}

fn screen(id: ScreenId, app: &App, mock: &MockBackend) -> Screen {
    match id {
        ScreenId::Main => Screen::Home { selected: 0 },
        ScreenId::Setup => Screen::Setup(SetupState::passphrase()),
        ScreenId::NoDrive => Screen::NoDrive { choice: 0 },
        ScreenId::Search => {
            let mut state = search_state(false);
            state.query = typed("pay");
            Screen::Search(state)
        }
        ScreenId::Detail => Screen::Detail(
            mock.package_detail(&PackageId::new("official", "moneropay")).expect("sample package"),
        ),
        ScreenId::Review => {
            let InstallGate::Review(data) = gate(mock, "community", "wallet-relay") else {
                unreachable!("wallet-relay always reaches review");
            };
            Screen::Review(ReviewState { data, confirm_focused: false, purpose: ReviewPurpose::Install })
        }
        ScreenId::Installing => {
            Screen::Installing(install_snapshot(&PackageId::new("official", "moneropay"), "2.7.0", 4))
        }
        ScreenId::Unlock => {
            let mut input = Input::secret();
            "twelve chars".chars().for_each(|c| input.push(c));
            Screen::Unlock(UnlockState {
                input,
                error: Some("That passphrase did not unlock this device. Try again.".to_owned()),
                failures: 1,
                retry_at_tick: 0,
            })
        }
        ScreenId::Feeds => Screen::Feeds { selected: 1 },
        ScreenId::AddFeed => Screen::AddFeed(AddFeedState::Confirm {
            probe: mock.probe_feed("https://github.com/example/monero-tools-feed").expect("sample feed"),
            trust_focused: false,
        }),
        ScreenId::FeedKey => Screen::FeedKey(FeedKeyState {
            change: mock.key_change("old-tools").expect("sample key change"),
            choice: 0,
            typing: None,
            error: None,
        }),
        ScreenId::Settings => Screen::Settings(settings_design_state(mock)),
        ScreenId::Reference => Screen::Reference(ReferenceState {
            package: "moneropay".to_owned(),
            field_index: 0,
            field_label: "Daemon RPC".to_owned(),
            list: sample_references(),
            query: typed("rpc"),
            list_focused: true,
            selected: 0,
        }),
        ScreenId::ApplyFailed => {
            let reason = "listen tcp :18081: bind: address already in use";
            Screen::ApplyFailed(ApplyFailedState {
                package: "moneropay".to_owned(),
                explanation: mock.explain_rollback("moneropay", reason),
                rows: vec![
                    DraftRow { label: "API port".to_owned(), from: "5000".to_owned(), to: "18081".to_owned(), blamed: true },
                    DraftRow { label: "Log level".to_owned(), from: "info".to_owned(), to: "debug".to_owned(), blamed: false },
                ],
            })
        }
        ScreenId::Updates => Screen::Updates(UpdatesState {
            selected: 0,
            checked: vec!["moneropay".to_owned(), "system base".to_owned()],
        }),
        ScreenId::AutoUpdate => {
            let mut state = AutoUpdateState::new(mock.auto_update());
            // wallet-relay, focused, because that is the state the spec's warning describes.
            state.selected = 4;
            Screen::AutoUpdate(state)
        }
        ScreenId::UpdateFailed => {
            let mut scratch = MockBackend::new();
            let report = scratch.apply_updates(&["moneropay".to_owned(), "system base".to_owned()]);
            Screen::UpdateResults(UpdateResultsState { report, choice: 0 })
        }
        ScreenId::Logs => Screen::Logs(LogsState {
            selected: 1,
            view: mock.logs("bell"),
            errors_only: false,
            follow: true,
            find: Input::plain(""),
            finding: false,
        }),
        ScreenId::Rejected => {
            let InstallGate::Rejected(data) = gate(mock, "community", "fast-sync-helper") else {
                unreachable!("fast-sync-helper is always rejected");
            };
            Screen::Rejected(data)
        }
        ScreenId::Capability => {
            let InstallGate::MissingCapability(data) = gate(mock, "community", "zmq-indexer") else {
                unreachable!("zmq-indexer always misses a capability");
            };
            Screen::Capability(CapabilityState { data, choice: 0 })
        }
        ScreenId::WebSites => Screen::Web { selected: 0 },
        ScreenId::WebExposure => Screen::WebExposure(ExposureState {
            site: "shop".to_owned(),
            draft: Exposure { local: true, onion: true, public: true },
            selected: 2,
            editing_host: None,
        }),
        ScreenId::WebProviders => {
            let mut state = ProvidersState::new(&app.data.web.domain);
            state.test = Some(Ok("Test passed: read and wrote a temporary record".to_owned()));
            Screen::WebProviders(state)
        }
        ScreenId::WebCheck => {
            let Reachability::Checked { checks, router, plan } = reachability_checks("shop.example.com", false) else {
                unreachable!("this helper only builds checked results");
            };
            Screen::WebCheck(WebCheckState { site: "shop".to_owned(), checks, router, plan, going_public: true })
        }
        ScreenId::WebConfirm => {
            let Reachability::Checked { plan: Some(plan), .. } = reachability_checks("shop.example.com", true) else {
                unreachable!("open ports always produce a plan");
            };
            Screen::WebConfirm(WebConfirmState {
                site: "shop".to_owned(),
                plan,
                access: PublicAccess { require_password: false, rate_limit: true, hide_admin: true },
                focus: ConfirmFocus::Address,
                typed: typed("shop.example.com"),
                error: None,
            })
        }
        ScreenId::WebCgnat => Screen::WebCgnat(CgnatState {
            site: "shop".to_owned(),
            wan_address: "100.72.14.9".to_owned(),
            wan_range: "100.64.0.0/10".to_owned(),
            internet_address: "203.0.113.42".to_owned(),
            choice: 0,
            going_public: true,
        }),
    }
}

/// A key press, for tests and for anything scripting the app.
pub fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

pub fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}
