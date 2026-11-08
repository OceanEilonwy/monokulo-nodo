//! First boot, SSH keys, and the worker thread that keeps slow helper calls off the UI.

mod common;

use std::time::{Duration, Instant};

use common::{screen_text, type_text};
use crossterm::event::KeyCode;
use nodo_api::{Request, ServiceState};
use nodo_tui::app::*;
use nodo_tui::fixtures::{fixture, press};
use nodo_tui::mock::MockBackend;
use nodo_tui::runtime::{dispatch, dispatch_async, execute_effect, EffectWorker};

fn first_boot() -> (App, MockBackend) {
    let mut mock = MockBackend::new();
    mock.setup_needed = true;
    let mut app = App::new();
    for event in execute_effect(&mut mock, Effect::Refresh) {
        dispatch(&mut app, &mut mock, event);
    }
    (app, mock)
}

fn key(app: &mut App, mock: &mut MockBackend, code: KeyCode) {
    dispatch(app, mock, press(code));
}

fn init_calls(mock: &MockBackend) -> Vec<String> {
    mock.calls
        .iter()
        .filter_map(|call| match call {
            Request::InitEncryption { passphrase } => Some(passphrase.expose().to_owned()),
            _ => None,
        })
        .collect()
}

/// To the Confirm step of the first-boot restore, with the right backup passphrase.
fn to_confirm(app: &mut App, mock: &mut MockBackend) {
    dispatch(app, mock, nodo_tui::fixtures::ctrl('r'));
    assert_eq!(app.screen.id(), ScreenId::Restore);
    key(app, mock, KeyCode::Enter);
    key(app, mock, KeyCode::Enter);
    type_text(app, mock, nodo_tui::mock::MOCK_PASSPHRASE);
    key(app, mock, KeyCode::Enter);
    assert!(screen_text(app).contains("Set up this device from the backup"), "{}", screen_text(app));
}

#[test]
fn a_new_device_can_be_restored_from_a_usb_backup_instead_of_set_up() {
    let (mut app, mut mock) = first_boot();
    assert!(screen_text(&app).contains("Ctrl-R restore from a USB backup"), "{}", screen_text(&app));
    dispatch(&mut app, &mut mock, nodo_tui::fixtures::ctrl('r'));
    assert_eq!(app.screen.id(), ScreenId::Restore);
    let screen = screen_text(&app);
    assert!(screen.contains("Set up this device › Restore from a USB backup › Drive"), "{screen}");
    assert!(screen.contains("SanDisk Ultra Fit, BACKUP"), "{screen}");
    key(&mut app, &mut mock, KeyCode::Enter);
    let screen = screen_text(&app);
    assert!(screen.contains("Backups on sda1"), "{screen}");
    assert!(screen.contains("nodo-1762473600.nodobak"), "{screen}");
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("Open this backup"));
    type_text(&mut app, &mut mock, nodo_tui::mock::MOCK_PASSPHRASE);
    assert!(!screen_text(&app).contains(nodo_tui::mock::MOCK_PASSPHRASE), "the passphrase is never drawn");
    key(&mut app, &mut mock, KeyCode::Enter);
    let screen = screen_text(&app);
    assert!(screen.contains("✓ nodo-1762473600.nodobak verified"), "{screen}");
    assert!(screen.contains("wallet-relay encrypted storage"), "{screen}");
    // The word, then the new device passphrase twice.
    type_text(&mut app, &mut mock, "restore");
    key(&mut app, &mut mock, KeyCode::Tab);
    type_text(&mut app, &mut mock, "a new device passphrase");
    key(&mut app, &mut mock, KeyCode::Tab);
    type_text(&mut app, &mut mock, "a new device passphrase");
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("✓ Restored"), "{}", screen_text(&app));
    assert!(!mock.setup_needed);
    let sent = mock.calls.iter().any(|c| matches!(c, Request::InitEncryption { .. }));
    assert!(!sent, "the backup's key is used, no new one is made");
    key(&mut app, &mut mock, KeyCode::Enter);
    assert_eq!(app.screen.id(), ScreenId::Setup);
    assert!(screen_text(&app).contains("SSH keys"), "{}", screen_text(&app));
    assert!(init_calls(&mock).is_empty());
}

#[test]
fn the_restore_refuses_a_missing_word_and_mismatched_passphrases_before_asking_the_helper() {
    let (mut app, mut mock) = first_boot();
    to_confirm(&mut app, &mut mock);
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("Type restore to confirm."), "{}", screen_text(&app));
    type_text(&mut app, &mut mock, "restore");
    key(&mut app, &mut mock, KeyCode::Tab);
    type_text(&mut app, &mut mock, "short");
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("at least 8 characters"), "{}", screen_text(&app));
    type_text(&mut app, &mut mock, " but long now");
    key(&mut app, &mut mock, KeyCode::Tab);
    type_text(&mut app, &mut mock, "something else");
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("The two device passphrases differ"), "{}", screen_text(&app));
    assert!(mock.setup_needed, "nothing was restored");
    // Esc is the safe way out, back to the list of backups.
    key(&mut app, &mut mock, KeyCode::Esc);
    assert!(screen_text(&app).contains("Backups on sda1"));
}

#[test]
fn a_wrong_backup_passphrase_is_said_and_can_be_typed_again() {
    let (mut app, mut mock) = first_boot();
    dispatch(&mut app, &mut mock, nodo_tui::fixtures::ctrl('r'));
    key(&mut app, &mut mock, KeyCode::Enter);
    key(&mut app, &mut mock, KeyCode::Enter);
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("Type the backup passphrase."));
    type_text(&mut app, &mut mock, "not it");
    key(&mut app, &mut mock, KeyCode::Enter);
    let screen = screen_text(&app);
    assert!(screen.contains("did not open this backup"), "{screen}");
    assert!(screen.contains("Open this backup"), "still asking: {screen}");
    type_text(&mut app, &mut mock, nodo_tui::mock::MOCK_PASSPHRASE);
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("Set up this device from the backup"));
}

#[test]
fn a_drive_that_cannot_be_used_says_why_and_a_failed_restore_can_be_retried() {
    let (mut app, mut mock) = first_boot();
    dispatch(&mut app, &mut mock, nodo_tui::fixtures::ctrl('r'));
    if let Screen::Restore(state) = &mut app.screen {
        state.drives[0].problem = Some("it is read-only".to_owned());
    }
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("sda1 cannot be used: it is read-only"), "{}", screen_text(&app));
    key(&mut app, &mut mock, KeyCode::Char('g'));
    to_confirm_from_drives(&mut app, &mut mock);
    mock.restore_failure = Some("the drive was removed while reading".to_owned());
    type_text(&mut app, &mut mock, "restore");
    key(&mut app, &mut mock, KeyCode::Tab);
    type_text(&mut app, &mut mock, "a new device passphrase");
    key(&mut app, &mut mock, KeyCode::Tab);
    type_text(&mut app, &mut mock, "a new device passphrase");
    key(&mut app, &mut mock, KeyCode::Enter);
    let screen = screen_text(&app);
    assert!(screen.contains("✕ Not restored"), "{screen}");
    assert!(screen.contains("the drive was removed while reading"), "{screen}");
    assert!(screen.contains("The device is still not set up"), "{screen}");
    assert!(mock.setup_needed);
    mock.restore_failure = None;
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("Open this backup"), "retry asks for the passphrase again");
}

fn to_confirm_from_drives(app: &mut App, mock: &mut MockBackend) {
    key(app, mock, KeyCode::Enter);
    key(app, mock, KeyCode::Enter);
    type_text(app, mock, nodo_tui::mock::MOCK_PASSPHRASE);
    key(app, mock, KeyCode::Enter);
}

#[test]
fn the_restore_shows_progress_while_the_helper_works() {
    let (mut app, mut mock) = first_boot();
    to_confirm(&mut app, &mut mock);
    if let Screen::Restore(state) = &mut app.screen {
        state.step = RestoreStep::Restoring { device: "sda1".into(), name: "nodo-1.nodobak".into(), started: app.ticks };
        state.busy = true;
    }
    for _ in 0..(TICKS_PER_SECOND * 75) {
        dispatch(&mut app, &mut mock, Event::Tick);
    }
    let screen = screen_text(&app);
    assert!(screen.contains("Restoring nodo-1.nodobak from sda1"), "{screen}");
    assert!(screen.contains("1:15 so far"), "{screen}");
    assert!(screen.contains("Keep the drive plugged in"), "{screen}");
    key(&mut app, &mut mock, KeyCode::Esc);
    assert_eq!(app.screen.id(), ScreenId::Restore, "keys wait while restoring");
}

#[test]
fn leaving_the_first_boot_restore_goes_back_to_setup() {
    let (mut app, mut mock) = first_boot();
    dispatch(&mut app, &mut mock, nodo_tui::fixtures::ctrl('r'));
    key(&mut app, &mut mock, KeyCode::Esc);
    assert_eq!(app.screen.id(), ScreenId::Setup);
    assert!(screen_text(&app).contains("Device passphrase"));
}

#[test]
fn first_boot_opens_setup_and_cannot_be_skipped() {
    let (mut app, mut mock) = first_boot();
    assert_eq!(app.screen.id(), ScreenId::Setup);
    key(&mut app, &mut mock, KeyCode::Esc);
    key(&mut app, &mut mock, KeyCode::Tab);
    assert_eq!(app.screen.id(), ScreenId::Setup);
    assert!(screen_text(&app).contains("There is no default passphrase"));
}

#[test]
fn passphrase_must_be_long_enough_and_typed_twice() {
    let (mut app, mut mock) = first_boot();
    type_text(&mut app, &mut mock, "short");
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("Use at least 8 characters."));
    key(&mut app, &mut mock, KeyCode::Backspace);
    type_text(&mut app, &mut mock, " but longer");
    key(&mut app, &mut mock, KeyCode::Enter);
    type_text(&mut app, &mut mock, "something else");
    key(&mut app, &mut mock, KeyCode::Enter);
    assert!(screen_text(&app).contains("The two passphrases differ."));
    assert!(init_calls(&mock).is_empty());

    type_text(&mut app, &mut mock, "correct horse");
    key(&mut app, &mut mock, KeyCode::Enter);
    type_text(&mut app, &mut mock, "correct horse");
    key(&mut app, &mut mock, KeyCode::Enter);
    assert_eq!(init_calls(&mock), ["correct horse"]);
    assert!(!mock.setup_needed);
    let screen = screen_text(&app);
    assert!(screen.contains("SSH keys"), "{screen}");
    assert!(screen.contains("laptop"), "{screen}");
    assert!(!screen.contains("correct horse"));
    assert!(!format!("{app:?}").contains("correct horse"));
}

#[test]
fn keys_can_be_added_and_removed_only_after_confirming() {
    let (mut app, mut mock) = fixture(ScreenId::Main);
    key(&mut app, &mut mock, KeyCode::Char('k'));
    assert_eq!(app.screen.id(), ScreenId::Setup);
    key(&mut app, &mut mock, KeyCode::Char('a'));
    type_text(&mut app, &mut mock, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample phone");
    key(&mut app, &mut mock, KeyCode::Enter);
    assert_eq!(mock.ssh_keys.len(), 2);
    assert!(screen_text(&app).contains("phone"));

    key(&mut app, &mut mock, KeyCode::Char('d'));
    assert!(app.modal.is_some());
    // The modal starts on the safe answer, so Enter keeps the key.
    key(&mut app, &mut mock, KeyCode::Enter);
    assert_eq!(mock.ssh_keys.len(), 2);
    key(&mut app, &mut mock, KeyCode::Char('d'));
    key(&mut app, &mut mock, KeyCode::Right);
    key(&mut app, &mut mock, KeyCode::Enter);
    assert_eq!(mock.ssh_keys.len(), 1);

    // The last key stays, and the reason is shown.
    key(&mut app, &mut mock, KeyCode::Char('d'));
    key(&mut app, &mut mock, KeyCode::Right);
    key(&mut app, &mut mock, KeyCode::Enter);
    assert_eq!(mock.ssh_keys.len(), 1);
    assert!(screen_text(&app).contains("only key"));
    key(&mut app, &mut mock, KeyCode::Esc);
    assert_eq!(app.screen.id(), ScreenId::Main);
}

#[test]
fn a_slow_helper_call_does_not_block_the_screen() {
    let (mut app, mut mock) = fixture(ScreenId::Main);
    mock.delay = Some(Duration::from_millis(300));
    let mut worker = EffectWorker::spawn(mock);
    let started = Instant::now();
    // s stops the selected service, cuprated.
    dispatch_async(&mut app, &mut worker, press(KeyCode::Char('s')));
    assert!(started.elapsed() < Duration::from_millis(100), "the key returned at once");
    assert!(app.working);
    assert!(worker.ready().is_empty());
    // Keys still work while the call runs.
    dispatch_async(&mut app, &mut worker, press(KeyCode::Char('?')));
    assert!(app.overlay.is_some());

    let events = worker.wait_idle(Duration::from_secs(5));
    assert!(!worker.busy());
    for event in events {
        dispatch_async(&mut app, &mut worker, event);
    }
    let cuprated = app.data.services.iter().find(|s| s.id == "cuprated").unwrap();
    assert_eq!(cuprated.state, ServiceState::Stopped);
}

#[test]
fn areas_without_a_backing_system_say_so() {
    let (mut app, mut mock) = fixture(ScreenId::Main);
    app.data.unavailable = vec![(nodo_tui::model::Area::Feeds, "feeds are not connected yet".to_owned())];
    // Feeds is the fourth tab.
    for _ in 0..3 {
        key(&mut app, &mut mock, KeyCode::Tab);
    }
    assert!(screen_text(&app).contains("Not available yet: feeds are not connected yet"));
}
