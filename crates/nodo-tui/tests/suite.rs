//! Installing a bundle of apps, and an app's setup page: the flows, the clicks and the QR code.

mod common;

use common::{draw, screen_text, send, text, type_text};
use crossterm::event::KeyCode;
use nodo_tui::app::{App, BundleStep, Event, MouseKind, Screen, ScreenId};
use nodo_tui::fixtures::{fixture, press};
use nodo_ui_core::mock::{sample_bundle_review, sample_owner_page, MockBackend, MOCK_PASSPHRASE};
use nodo_ui_core::model::{PackageId, ScreenData};
use nodo_tui::runtime::{dispatch, load_snapshot};
use nodo_tui::view::owner::qr_lines;
use ratatui::style::Color;

const KEY_FILE: &str = "{\n  \"type\": \"service_account\",\n  \"private_key\": \"-----BEGIN PRIVATE KEY-----\\nMIIE\\n\"\n}\n";

fn find(app: &App, needle: &str) -> (u16, u16) {
    let (buffer, _) = draw(app, 100, 32);
    for (row, line) in text(&buffer).lines().enumerate() {
        if let Some(byte) = line.find(needle) {
            return (line[..byte].chars().count() as u16, row as u16);
        }
    }
    panic!("{needle} is not on screen:\n{}", text(&buffer));
}

/// Get apps with the pingers bundle reviewed, and a setup page for its head once installed.
fn at_bundle_review() -> (App, MockBackend) {
    let (mut app, mut mock) = fixture(ScreenId::Search);
    let review = sample_bundle_review();
    mock.bundles.insert("pingers".to_owned(), review);
    mock.owner_views.insert("pingers".to_owned(), ("Setup".to_owned(), sample_owner_page()));
    let gate = mock.install_gate(&PackageId::new("pingers", "pingers"));
    dispatch(&mut app, &mut mock, Event::GateLoaded(gate));
    (app, mock)
}

fn bundle(app: &App) -> &nodo_tui::app::BundleState {
    let Screen::Bundle(state) = &app.screen else { panic!("not the bundle screen: {:?}", app.screen.id()) };
    state
}

#[test]
fn a_bundle_review_starts_on_cancel_and_lists_every_app_and_grant() {
    let (mut app, mut mock) = at_bundle_review();
    let screen = screen_text(&app);
    assert!(screen.contains("Install pingers?") && screen.contains("Installs 4 apps"), "{screen}");
    assert!(screen.contains("pingers-push-relay") && screen.contains("Connects to the internet"), "{screen}");
    // The grants and the data drive are further down; the page scrolls to them.
    send(&mut app, &mut mock, [press(KeyCode::PageDown), press(KeyCode::PageDown)]);
    let screen = screen_text(&app);
    assert!(screen.contains("pingers-server may read the setting server_credential of pingers-push-relay"), "{screen}");
    assert!(screen.contains("[x] pingers-map-host"), "on by default with a data drive present: {screen}");
    // A stray Enter cancels.
    dispatch(&mut app, &mut mock, press(KeyCode::Enter));
    assert_eq!(app.screen.id(), ScreenId::Search);
    assert!(mock.bundle_installs.is_empty());
}

#[test]
fn a_bundle_installs_with_answers_a_pasted_key_file_and_the_passphrase_then_opens_setup() {
    let (mut app, mut mock) = at_bundle_review();
    // Untick the data drive, then continue.
    send(&mut app, &mut mock, [press(KeyCode::Up), press(KeyCode::Char(' ')), press(KeyCode::Down), press(KeyCode::Down)]);
    assert_eq!(bundle(&app).drive, [false]);
    dispatch(&mut app, &mut mock, press(KeyCode::Enter));
    assert_eq!(bundle(&app).step, BundleStep::Answers);
    // An empty answer is refused, with the field named.
    send(&mut app, &mut mock, [press(KeyCode::Tab), press(KeyCode::Enter)]);
    assert!(bundle(&app).error.as_deref().unwrap().contains("Your domain"));
    assert_eq!(bundle(&app).focus, 0, "the focus goes to the empty field");
    type_text(&mut app, &mut mock, "example.com");
    dispatch(&mut app, &mut mock, press(KeyCode::Tab));
    dispatch(&mut app, &mut mock, Event::Paste(KEY_FILE.to_owned()));
    let screen = screen_text(&app);
    assert!(!screen.contains("service_account") && !screen.contains("PRIVATE"), "a secret is never shown: {screen}");
    assert!(screen.contains("characters"), "its length is: {screen}");
    dispatch(&mut app, &mut mock, press(KeyCode::Enter));
    assert_eq!(bundle(&app).step, BundleStep::Passphrase);
    type_text(&mut app, &mut mock, "wrong one");
    dispatch(&mut app, &mut mock, press(KeyCode::Enter));
    assert_eq!(bundle(&app).step, BundleStep::Passphrase, "a wrong passphrase asks again");
    assert!(bundle(&app).error.as_deref().unwrap().contains("did not unlock"));
    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    dispatch(&mut app, &mut mock, press(KeyCode::Enter));
    let BundleStep::Done(report) = &bundle(&app).step else { panic!("{:?}", bundle(&app).step) };
    assert_eq!(report.problem, None);
    let (answers, drive) = mock.bundle_installs.last().unwrap();
    assert!(drive.is_empty());
    assert_eq!(answers[0].value.expose(), "example.com");
    // A pasted file arrives without its line breaks, which in JSON are only spacing.
    assert_eq!(answers[1].value.expose(), KEY_FILE.replace('\n', ""));
    assert!(!format!("{:?}", mock.bundle_installs).contains("PRIVATE"));
    let screen = screen_text(&app);
    assert!(screen.contains("pingers is installed") && screen.contains("Open setup"), "{screen}");
    dispatch(&mut app, &mut mock, press(KeyCode::Enter));
    let Screen::OwnerPage(state) = &app.screen else { panic!("{:?}", app.screen.id()) };
    assert_eq!(state.package, "pingers");
    assert!(state.view.is_none(), "it asks for the passphrase first");
}

#[test]
fn get_apps_says_how_many_apps_a_bundle_installs() {
    let (mut app, mut mock) = fixture(ScreenId::Search);
    mock.catalog[0].bundle = Some(3);
    let name = mock.catalog[0].id.name.clone();
    app.data = load_snapshot(&mut mock).unwrap();
    dispatch(&mut app, &mut mock, press(KeyCode::Down));
    let screen = screen_text(&app);
    let row = screen.lines().find(|l| l.contains(&name)).unwrap();
    assert!(row.contains("Installs 4 apps"), "{row}");
}

/// moneropay, installed in the sample data, with a setup page.
fn app_page_with_setup() -> (App, MockBackend) {
    let (mut app, mut mock) = fixture(ScreenId::AppPage);
    let mut view = sample_owner_page();
    view.package = "moneropay".to_owned();
    mock.owner_views.insert("moneropay".to_owned(), ("Setup".to_owned(), view));
    app.data = load_snapshot(&mut mock).unwrap();
    (app, mock)
}

#[test]
fn the_setup_row_asks_for_the_passphrase_then_shows_the_checklist() {
    let (mut app, mut mock) = app_page_with_setup();
    let screen = screen_text(&app);
    assert!(screen.contains("Setup") && screen.contains("Hosting"), "{screen}");
    dispatch(&mut app, &mut mock, press(KeyCode::Enter));
    assert_eq!(app.screen.id(), ScreenId::OwnerPage);
    assert!(screen_text(&app).contains("Passphrase"));
    type_text(&mut app, &mut mock, "nope");
    dispatch(&mut app, &mut mock, press(KeyCode::Enter));
    let Screen::OwnerPage(state) = &app.screen else { panic!() };
    assert!(state.view.is_none() && state.held.is_none());
    assert!(screen_text(&app).contains("did not unlock"));
    type_text(&mut app, &mut mock, MOCK_PASSPHRASE);
    dispatch(&mut app, &mut mock, press(KeyCode::Enter));
    let screen = screen_text(&app);
    assert!(screen.contains("Pingers setup") && screen.contains("1.  Put the sites online"), "{screen}");
    // Tab moves to the next value and c copies it to the terminal's clipboard.
    send(&mut app, &mut mock, [press(KeyCode::Tab), press(KeyCode::Char('c'))]);
    assert_eq!(app.clipboard.as_deref(), Some("203.0.113.5"));
    assert!(screen_text(&app).contains("Copied"));
    // r asks again with the passphrase held while the page is open.
    dispatch(&mut app, &mut mock, press(KeyCode::Char('r')));
    let Screen::OwnerPage(state) = &app.screen else { panic!() };
    assert!(state.view.is_some() && !state.loading);
    // Leaving drops the page and the passphrase with it.
    dispatch(&mut app, &mut mock, press(KeyCode::Esc));
    assert_eq!(app.screen.id(), ScreenId::AppPage);
    assert!(!app.stack.iter().any(|s| s.id() == ScreenId::OwnerPage), "nothing keeps the page or its passphrase");
}

#[test]
fn the_setup_page_scrolls_to_the_qr_code_and_copy_takes_a_click() {
    let (mut app, mut mock) = fixture(ScreenId::OwnerPage);
    for _ in 0..10 {
        dispatch(&mut app, &mut mock, press(KeyCode::PageDown));
    }
    let screen = screen_text(&app);
    assert!(screen.contains("https://pingers.example.com/j#s1Xk3"), "the data stays as text: {screen}");
    let (buffer, _) = draw(&app, 100, 32);
    assert!(buffer.content().iter().any(|c| c.symbol() == "▀" && c.bg == Color::Rgb(255, 255, 255)), "the code is drawn");
    // The last Copy on screen is the QR data's.
    let (_, row) = find(&app, "The same, as text");
    let line = text(&buffer).lines().nth(usize::from(row)).unwrap().to_owned();
    let column = line[..line.find("Copy").unwrap()].chars().count() as u16;
    dispatch(&mut app, &mut mock, Event::Mouse { kind: MouseKind::Down, column, row });
    assert_eq!(app.clipboard.as_deref(), Some("https://pingers.example.com/j#s1Xk3"));
    // The wheel scrolls like the arrows.
    dispatch(&mut app, &mut mock, Event::Mouse { kind: MouseKind::ScrollUp, column: 1, row: 10 });
    let Screen::OwnerPage(state) = &app.screen else { panic!() };
    assert!(!state.follow);
}

#[test]
fn a_qr_code_has_a_white_quiet_zone_and_its_modules_in_half_blocks() {
    let data = "https://pingers.example.org/j#s1abcdef";
    let lines = qr_lines(data).unwrap();
    let code = qrcodegen::QrCode::encode_text(data, qrcodegen::QrCodeEcc::Low).unwrap();
    let size = code.size() as usize;
    let width = size + 8;
    assert_eq!(lines.len(), width.div_ceil(2));
    let white = Color::Rgb(255, 255, 255);
    let black = Color::Rgb(0, 0, 0);
    // Each cell: fg is the upper module, bg the lower.
    let cells: Vec<Vec<(Color, Color)>> = lines
        .iter()
        .map(|spans| spans.iter().flat_map(|s| s.content.chars().map(move |_| (s.style.fg.unwrap(), s.style.bg.unwrap()))).collect())
        .collect();
    assert!(cells.iter().all(|row| row.len() == width));
    assert!(cells[0].iter().all(|&(fg, bg)| fg == white && bg == white), "four modules of quiet zone on top");
    for y in 0..size {
        for x in 0..size {
            let (fg, bg) = cells[(y + 4) / 2][x + 4];
            let drawn = if (y + 4) % 2 == 0 { fg } else { bg };
            let want = if code.get_module(x as i32, y as i32) { black } else { white };
            assert_eq!(drawn, want, "module {x},{y}");
        }
    }
    assert!(lines.iter().all(|spans| spans.iter().all(|s| s.content.chars().all(|c| c == '▀'))));
    assert!(qr_lines(&"x".repeat(3000)).is_none(), "too long for any code");
}

#[test]
fn osc52_carries_the_copied_text() {
    let mut app = App::new();
    app.clipboard = Some("abc".to_owned());
    assert_eq!(nodo_tui::clipboard::osc52(app.clipboard.as_deref().unwrap()), "\x1b]52;c;YWJj\x07");
}
