//! Every design screen, rendered: exact copy, fit at 100x32, colours, and graceful degradation.
//!
//! Text snapshots live in `tests/snapshots`. After an intended change, regenerate them with
//! `UPDATE_SNAPSHOTS=1 cargo test -p nodo-tui --test render` and review the diff.

mod common;

use std::path::PathBuf;

use common::{draw, screen_text, text};
use nodo_tui::app::ScreenId;
use nodo_tui::fixtures::fixture;
use nodo_tui::theme::Theme;
use ratatui::style::Color;

fn snapshot_path(id: ScreenId) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots").join(format!("{}.txt", id.slug()))
}

#[test]
fn every_screen_matches_its_snapshot() {
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let mut mismatched = Vec::new();
    if update {
        std::fs::create_dir_all(snapshot_path(ScreenId::Main).parent().expect("has a parent"))
            .expect("create snapshot directory");
    }
    for id in ScreenId::ALL {
        let (app, _) = fixture(id);
        let actual = screen_text(&app);
        let path = snapshot_path(id);
        if update {
            std::fs::write(&path, &actual).expect("write snapshot");
        } else if std::fs::read_to_string(&path).ok().as_deref() != Some(actual.as_str()) {
            mismatched.push(id.slug());
        }
    }
    assert!(mismatched.is_empty(), "snapshots differ: {mismatched:?}");
}

#[test]
fn the_fixtures_cover_the_whole_design_on_the_right_screen() {
    assert_eq!(ScreenId::ALL.len(), 37, "every screen in the four tabs");
    for id in ScreenId::ALL {
        assert_eq!(fixture(id).0.screen.id(), id);
    }
}

#[test]
fn nothing_is_clipped_or_broken_at_100x32() {
    for id in ScreenId::ALL {
        let (app, _) = fixture(id);
        let (buffer, report) = draw(&app, 100, 32);
        assert!(report.fits(), "{}: {report:?}\n{}", id.slug(), text(&buffer));
    }
}

/// Design copy that must appear intact at 100x32. Sentences that wrap in a panel are checked by
/// words on the wrapped line, so each string here is one that fits on a single row.
fn required(id: ScreenId) -> &'static [&'static str] {
    match id {
        ScreenId::Main => &["Your Nodo", "Your Nodo is locked", "Unlock", "bell keeps stopping", "Show log", "Review updates"],
        ScreenId::Apps => &["Installed", "Get apps ›", "cuprated", "Update ready", "Keeps stopping"],
        ScreenId::AppPage => &["Apps  ›  moneropay", "Restart", "Remove…", "is ready", "Update", "Hosting", "Settings", "Logs"],
        ScreenId::AppRemove => &["Remove moneropay?", "Its data stays on the drive", "Cancel", "Remove moneropay"],
        ScreenId::System => &["This Nodo", "A Nodo update is ready", "Review", "Updates", "Backups", "Network", "Device logs"],
        ScreenId::Search => &["Apps  ›  Get apps", "Search", "Sources ›", "moneropay", "apps match"],
        ScreenId::Detail => &["Apps  ›  Get apps  ›  moneropay", "It will:", "Install"],
        ScreenId::Review => &["Install wallet-relay?", "a source you added", "It will be able to:", "Cancel", "Install"],
        ScreenId::Installing => &["Installing moneropay", "✓  Download package", "You can leave this page"],
        ScreenId::Unlock => &["Unlock your Nodo", "Passphrase", "Unlock"],
        ScreenId::Feeds => &["Apps  ›  Get apps  ›  Sources", "Add a source", "built in", "Remove", "more — scroll"],
        ScreenId::AddFeed => &["Its key", "Check this matches what the publisher gave you", "Add source"],
        ScreenId::FeedKey => &["Check before you trust", "is now signed with a different key", "Key you trust", "New key", "Keep the old key"],
        ScreenId::Settings => &["Settings", "restarts with the new settings", "Payment address", "Cancel", "Save"],
        ScreenId::Reference => &["Pick daemon rpc from your apps", "monero-daemon.public_rpc_url", "Use it"],
        ScreenId::ApplyFailed => &["didn't start with the new settings, so they were undone", "Show log ›", "Edit again"],
        ScreenId::Updates => &["System  ›  Updates", "The Nodo", "Update and restart", "Apps", "Automatic updates ›", "Undo an update ›"],
        ScreenId::UpdateFailed => &["update didn't start, so", "Try again", "OK"],
        ScreenId::AutoUpdate => &["Automatic updates", "How often", "Update these apps by themselves", "Update the Nodo by itself", "Save"],
        ScreenId::Logs => &["System  ›  Device logs", "Problems", "Save to a file ›", "starting version 1.2.0"],
        ScreenId::Rejected => &["can't be installed", "there's no override", "OK"],
        ScreenId::Capability => &["needs a different Monero node first", "Install monerod"],
        ScreenId::NoDrive => &["The data drive isn't connected", "Check again"],
        ScreenId::WebSites => &["Sites online"],
        ScreenId::WebExposure => &["Hosting", "Home network", "Tor", "Internet"],
        ScreenId::WebProviders => &["Domain providers"],
        ScreenId::WebCheck => &["Hosting"],
        ScreenId::WebConfirm => &["Cancel"],
        ScreenId::WebCgnat => &["Use Tor instead"],
        ScreenId::WebDomain => &["Web"],
        ScreenId::WebCertificate => &["Hosting"],
        ScreenId::WebTrust => &["Web"],
        ScreenId::Setup => &["Set up"],
        ScreenId::Backup => &["System  ›  Backups", "Backups on this device", "A backup holds"],
        ScreenId::Onion => &["Web  ›  Onion addresses", "Node", "Sites"],
        ScreenId::Restore => &["Restore", "USB drives"],
        ScreenId::Rollbacks => &["System  ›  Updates  ›  Undo an update", "can roll back"],
        ScreenId::Network => &["System  ›  Network", "Wi-Fi", "Bluetooth"],
    }
}

#[test]
fn every_screen_shows_the_design_copy_intact() {
    let mut missing = Vec::new();
    for id in ScreenId::ALL {
        let (app, _) = fixture(id);
        let screen = screen_text(&app);
        for needle in required(id) {
            if !screen.contains(needle) {
                missing.push(format!("{}: {needle}", id.slug()));
            }
        }
    }
    assert!(missing.is_empty(), "missing copy:\n{}", missing.join("\n"));
}

#[test]
fn colours_follow_the_design() {
    let theme = Theme::truecolor();
    for id in ScreenId::ALL {
        let (app, _) = fixture(id);
        let (buffer, _) = draw(&app, 100, 32);
        for cell in buffer.content() {
            assert!(
                [theme.ground, theme.accent, theme.bar, theme.card, theme.field, theme.raised, theme.danger].contains(&cell.bg),
                "{}: unexpected background {:?}",
                id.slug(),
                cell.bg
            );
        }
    }
    let (app, _) = fixture(ScreenId::Main);
    let (buffer, _) = draw(&app, 100, 32);
    assert!(buffer.content().iter().any(|cell| cell.fg == Color::Rgb(0xF2, 0xA3, 0x3A)), "amber accent is used");
}

/// A focused row keeps a symbol, so selection is readable with colour removed.
#[test]
fn selection_and_state_survive_without_colour() {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 32)).unwrap();
    let (app, _) = fixture(ScreenId::Main);
    terminal
        .draw(|frame| {
            nodo_tui::view::render(frame, &app, &Theme::mono());
        })
        .unwrap();
    let screen = text(terminal.backend().buffer());
    for symbol in ["▸", "▲"] {
        assert!(screen.contains(symbol), "missing {symbol}");
    }
    assert!(terminal.backend().buffer().content().iter().all(|cell| cell.bg == Color::Reset || cell.bg == Color::Black));
}

#[test]
fn every_screen_survives_any_size_and_keeps_its_keys_on_screen() {
    for id in ScreenId::ALL {
        for (width, height) in [(1, 1), (10, 3), (59, 40), (200, 15), (60, 16), (80, 24), (72, 20), (100, 31), (160, 50)] {
            let (mut app, _) = fixture(id);
            app.size = (width, height);
            let (buffer, _) = draw(&app, width, height);
            let screen = text(&buffer);
            if width >= 60 && height >= 16 {
                // Hints can wrap, so look at the last rows together.
                let rows: Vec<&str> = screen.lines().filter(|line| !line.trim().is_empty()).collect();
                let last = rows[rows.len().saturating_sub(3)..].join("\n");
                assert!(
                    ["↑↓", "Esc", "↵", "Space", "Tab", "←→", "type"].iter().any(|key| last.contains(key)),
                    "{} at {width}x{height}: footer lost\n{screen}",
                    id.slug()
                );
            } else {
                assert!(screen.contains("small") || width < 20, "{}: {screen}", id.slug());
            }
        }
    }
}

#[test]
fn the_focused_row_stays_visible_on_a_small_terminal() {
    let (mut app, _) = fixture(ScreenId::WebProviders);
    if let nodo_tui::app::Screen::WebProviders(state) = &mut app.screen {
        state.selected = 10;
    }
    let screen = text(&draw(&app, 60, 16).0);
    assert!(screen.contains("▸ Squarespace"), "{screen}");
}

/// The confirm buttons must not scroll off when the terminal is short.
#[test]
fn dangerous_screens_keep_their_buttons_visible_when_small() {
    for id in [ScreenId::Review, ScreenId::AddFeed] {
        let (app, _) = fixture(id);
        let screen = text(&draw(&app, 80, 16).0);
        assert!(screen.contains("▸ Cancel"), "{}:\n{screen}", id.slug());
    }
    let (app, _) = fixture(ScreenId::WebConfirm);
    let screen = text(&draw(&app, 80, 16).0);
    assert!(screen.contains("Cancel") && screen.contains("Put shop online"), "webconfirm:\n{screen}");
}

/// Real packages have labels longer than the design's, such as cuprated's "Restricted RPC on
/// the network". The value must still stand apart from its label.
#[test]
fn long_setting_labels_keep_a_gap_before_the_value() {
    let (mut app, _) = fixture(ScreenId::Settings);
    if let nodo_tui::app::Screen::Settings(state) = &mut app.screen {
        state.form.fields[1].label = "Restricted RPC on the network".to_owned();
    }
    let screen = text(&draw(&app, 100, 32).0);
    let row = screen.lines().find(|line| line.contains("Restricted RPC on the network")).expect("label shown");
    let after = row.split("Restricted RPC on the network").nth(1).unwrap_or_default();
    assert!(after.starts_with("  "), "no gap after the label:\n{screen}");
}

/// A long log shows its newest lines, which is where a failure is, and what "Following live" promises.
#[test]
fn a_long_log_shows_its_newest_lines() {
    let (mut app, _) = fixture(ScreenId::Logs);
    if let nodo_tui::app::Screen::Logs(state) = &mut app.screen {
        state.view.lines = (1..=200)
            .map(|n| nodo_tui::model::LogLine { kind: nodo_tui::model::LogKind::Info, text: format!("line number {n}") })
            .collect();
        state.view.suggestion.clear();
    }
    let screen = text(&draw(&app, 100, 32).0);
    assert!(screen.contains("line number 200"), "the newest line is hidden:\n{screen}");
    assert!(!screen.contains("line number 1 "), "the oldest line is shown instead:\n{screen}");
}

/// Packages from the helper often have no size or license. Their detail must not show empty
/// separators, and a feed package not downloaded yet is not a failed signature: the helper checks
/// it against the pinned key when it downloads it.
#[test]
fn a_feed_package_detail_reads_cleanly_before_download() {
    let (mut app, _) = fixture(ScreenId::Detail);
    if let nodo_tui::app::Screen::Detail(detail) = &mut app.screen {
        detail.installed = false;
        detail.signature_verified = false;
        detail.version = "1.0_1".to_owned();
        detail.size = String::new();
        detail.license = String::new();
    }
    let screen = text(&draw(&app, 100, 32).0);
    let row = screen.lines().find(|line| line.contains("Version 1.0_1")).expect("version shown");
    assert!(!row.contains("·  ·") && !row.trim_end().ends_with('·'), "empty parts left separators: {row}");
    assert!(!screen.contains("could not be checked"), "{screen}");
    assert!(screen.contains("signature is checked when it downloads"), "{screen}");
}

/// A fresh device has no feeds, and the helper does not record when a feed was added.
#[test]
fn feeds_read_cleanly_with_none_and_with_helper_data() {
    let (mut app, _) = fixture(ScreenId::Feeds);
    for feed in &mut app.data.feeds {
        feed.added = String::new();
    }
    let screen = text(&draw(&app, 100, 32).0);
    assert!(!screen.lines().any(|line| line.trim_matches(|c| c == '│' || c == ' ') == "Added"), "{screen}");
    app.data.feeds.clear();
    let screen = text(&draw(&app, 100, 32).0);
    assert!(screen.contains("No sources yet."), "{screen}");
}

#[test]
fn terminal_control_characters_in_data_never_reach_the_screen() {
    let (mut app, _) = fixture(ScreenId::Search);
    app.data.catalog[0].tagline = "evil\u{1b}[2Jtext".to_owned();
    let (buffer, _) = draw(&app, 100, 32);
    assert!(!text(&buffer).contains('\u{1b}'));
}

/// Plain HTTP feeds are allowed, because signatures protect every package, but the screens say
/// what HTTP leaves open, and only for HTTP.
#[test]
fn plain_http_feeds_say_what_signatures_do_not_cover() {
    use nodo_tui::app::{AddFeedState, Screen};
    let (mut app, _) = fixture(ScreenId::AddFeed);
    let (buffer, _) = draw(&app, 100, 32);
    assert!(!text(&buffer).contains("Plain HTTP"));
    if let Screen::AddFeed(AddFeedState::Confirm { probe, .. }) = &mut app.screen {
        probe.address = "http://192.168.1.5:8000/feed".to_owned();
    } else {
        panic!("the AddFeed fixture is the confirm step");
    }
    let (buffer, report) = draw(&app, 100, 32);
    let shown = text(&buffer);
    assert!(report.fits(), "{report:?}\n{shown}");
    for words in ["Plain HTTP", "nothing changed on the way can install", "hold back updates", "Prefer https://", "Add source"] {
        assert!(shown.contains(words), "{words}\n{shown}");
    }

    let (mut app, _) = fixture(ScreenId::Feeds);
    let (buffer, _) = draw(&app, 100, 32);
    assert!(!text(&buffer).contains("Plain HTTP"));
    app.data.feeds[0].address = "http://192.168.1.5:8000/feed".to_owned();
    let (buffer, _) = draw(&app, 100, 32);
    assert!(text(&buffer).contains("Plain HTTP: signed, but updates can be held back"), "{}", text(&buffer));
}

/// A request in flight (following a log polls all the time) shows "Working…" in the header's
/// right-hand text without moving anything else: every row below the header stays the same.
#[test]
fn working_never_shifts_the_layout() {
    let (mut app, _) = fixture(ScreenId::Logs);
    app.working = false;
    let idle = text(&draw(&app, 100, 32).0);
    app.working = true;
    let busy = text(&draw(&app, 100, 32).0);
    assert!(busy.contains("Working…"), "the busy mark is not shown:\n{busy}");
    let changed: Vec<usize> =
        idle.lines().zip(busy.lines()).enumerate().filter(|(_, (a, b))| a != b).map(|(row, _)| row).collect();
    assert!(changed.len() <= 1, "working moved rows {changed:?}:\n{idle}\n----\n{busy}");
}

/// Each screen's main button stays on screen in a small terminal and a large one: cards cut the
/// rows above their buttons, never the buttons.
#[test]
fn the_main_button_stays_visible_at_80x24_and_120x36() {
    let main: &[(ScreenId, &str)] = &[
        (ScreenId::Review, "Install"),
        (ScreenId::AppRemove, "Remove moneropay"),
        (ScreenId::Settings, "Save"),
        (ScreenId::AddFeed, "Add source"),
        (ScreenId::FeedKey, "Keep the old key"),
        (ScreenId::Unlock, "Unlock"),
        (ScreenId::Updates, "Update and restart"),
        (ScreenId::AutoUpdate, "Save"),
        (ScreenId::NoDrive, "Check again"),
        (ScreenId::Rejected, "OK"),
        (ScreenId::Capability, "Install monerod"),
        (ScreenId::Detail, "Install"),
        (ScreenId::ApplyFailed, "Edit again"),
        (ScreenId::UpdateFailed, "OK"),
        (ScreenId::Main, "Unlock"),
        (ScreenId::System, "Review"),
        (ScreenId::Feeds, "Add a source"),
    ];
    for (id, label) in main {
        for (width, height) in [(80, 24), (120, 36)] {
            let (mut app, _) = fixture(*id);
            app.size = (width, height);
            let screen = text(&draw(&app, width, height).0);
            assert!(screen.contains(label), "{} at {width}x{height}: {label} is not on screen\n{screen}", id.slug());
        }
    }
}

/// The tab strip is the same four tabs on every screen, with the current area marked.
#[test]
fn every_screen_shows_the_four_tabs() {
    for id in ScreenId::ALL {
        let (app, _) = fixture(id);
        let screen = screen_text(&app);
        let strip = screen.lines().take(3).collect::<Vec<_>>().join("\n");
        for tab in ["Home", "Apps", "Web", "System"] {
            assert!(strip.contains(tab), "{}: {tab} missing from the tab strip\n{strip}", id.slug());
        }
        for old in ["Packages", "Feeds", "Logs"] {
            assert!(!strip.contains(old), "{}: the old {old} tab is still there\n{strip}", id.slug());
        }
    }
}

fn draw_with(app: &nodo_tui::app::App, theme: &Theme, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            nodo_tui::view::render(frame, app, theme);
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

/// A button's click target is the button as drawn, one row and every cell of its fill (and caps),
/// never a box of padding around its label.
#[test]
fn every_button_target_is_exactly_the_drawn_button() {
    let plain = Theme::truecolor();
    for theme in [plain, plain.with_rounded(true)] {
        let fills = [theme.accent, theme.raised, theme.danger];
        let mut seen = 0;
        let mut add_provider = false;
        // Every screen, and Hosting with no provider yet, where the reported "Add a provider" is.
        let cases = ScreenId::ALL.into_iter().map(|id| (id, false)).chain([(ScreenId::WebExposure, true)]);
        for (id, no_provider) in cases {
            for (width, height) in [(100, 32), (80, 24)] {
                let (mut app, _) = fixture(id);
                if let (true, nodo_tui::app::Screen::WebExposure(state)) = (no_provider, &mut app.screen) {
                    state.has_provider = false;
                    state.draft.public = true;
                }
                app.size = (width, height);
                let buffer = draw_with(&app, &theme, width, height);
                for hit in app.hits.borrow().iter().filter(|hit| hit.button.is_some()) {
                    let r = hit.rect;
                    let label = hit.button.as_deref().unwrap();
                    assert_eq!(r.height, 1, "{} {label}: one row", id.slug());
                    let inner = if theme.rounded { r.x + 1..r.x + r.width - 1 } else { r.x..r.x + r.width };
                    let fill = buffer[(inner.start, r.y)].bg;
                    assert!(fills.contains(&fill), "{} {label}: drawn on {fill:?}", id.slug());
                    assert!(inner.clone().all(|x| buffer[(x, r.y)].bg == fill), "{} {label}: a gap in {r:?}", id.slug());
                    if theme.rounded {
                        assert_eq!(buffer[(r.x, r.y)].fg, fill, "{} {label}: left cap", id.slug());
                        assert_eq!(buffer[(r.x + r.width - 1, r.y)].fg, fill, "{} {label}: right cap", id.slug());
                    }
                    for x in [r.x.checked_sub(1), Some(r.x + r.width)].into_iter().flatten().filter(|x| *x < width) {
                        let beside = &buffer[(x, r.y)];
                        assert_ne!(beside.bg, fill, "{} {label}: the button goes on past {r:?}", id.slug());
                    }
                    add_provider |= label == "Add a provider";
                    seen += 1;
                }
            }
        }
        assert!(seen > 40, "buttons were checked: {seen}");
        assert!(add_provider, "the inline Add a provider button was checked");
    }
}

#[test]
fn hovering_a_button_lightens_the_button_itself() {
    let theme = Theme::truecolor().with_rounded(true);
    for id in [ScreenId::Review, ScreenId::WebConfirm, ScreenId::AppRemove, ScreenId::Settings, ScreenId::AppPage] {
        let (mut app, _) = fixture(id);
        let before = draw_with(&app, &theme, 100, 32);
        let hits = app.hits.borrow().clone();
        assert!(hits.iter().any(|hit| hit.button.is_some()), "{}: has buttons", id.slug());
        for hit in hits.iter().filter(|hit| hit.button.is_some()) {
            app.hover = Some(hit.rect);
            let after = draw_with(&app, &theme, 100, 32);
            let r = hit.rect;
            for x in r.x..r.x + r.width {
                let (old, new) = (&before[(x, r.y)], &after[(x, r.y)]);
                if x == r.x || x == r.x + r.width - 1 {
                    assert_eq!(new.fg, theme.lift(old.fg), "{}: cap of {:?}", id.slug(), hit.button);
                    assert_eq!(new.bg, old.bg);
                } else {
                    assert_eq!(new.bg, theme.lift(old.bg), "{}: fill of {:?}", id.slug(), hit.button);
                }
            }
            assert_eq!(after[(r.x, r.y + 1)].bg, before[(r.x, r.y + 1)].bg, "{}: the row below stays", id.slug());
        }
    }
}

#[test]
fn a_pending_button_shows_the_hourglass_takes_no_clicks_and_clears_on_its_result() {
    use crossterm::event::KeyCode;
    use nodo_tui::app::{Effect, Event, MouseKind};
    use nodo_tui::fixtures::press;
    use nodo_tui::hit::Click;
    use nodo_tui::update::update;

    let theme = Theme::truecolor();
    let (mut app, _) = fixture(ScreenId::Unlock);
    // Past the fixture's wait after a wrong passphrase.
    app.ticks = 1_000;
    for c in "correct horse".chars() {
        update(&mut app, press(KeyCode::Char(c)));
    }
    let unlock = |app: &nodo_tui::app::App| app.hits.borrow().iter().find(|hit| hit.button.as_deref() == Some("Unlock")).cloned();
    draw_with(&app, &theme, 100, 32);
    assert!(unlock(&app).is_some());

    let effects = update(&mut app, press(KeyCode::Enter));
    assert!(matches!(effects.as_slice(), [Effect::Unlock(_)]), "{effects:?}");
    assert_eq!(app.pending.as_ref().map(|p| &p.click), Some(&Click::Key(KeyCode::Enter)));
    let screen = text(&draw_with(&app, &theme, 100, 32));
    assert!(screen.contains("⧗ Unlock"), "{screen}");
    assert!(unlock(&app).is_none(), "a pending button is not a target");
    assert!(update(&mut app, press(KeyCode::Enter)).is_empty(), "its key is ignored while it runs");

    update(&mut app, Event::UnlockResult(Err(nodo_api::ApiError::Failed("wrong passphrase".to_owned()))));
    assert_eq!(app.pending, None, "the result ends it");
    let screen = text(&draw_with(&app, &theme, 100, 32));
    assert!(!screen.contains('⧗'), "{screen}");

    // A click marks it the same way, and leaving the screen ends it.
    app.ticks = 2_000;
    for c in "correct horse".chars() {
        update(&mut app, press(KeyCode::Char(c)));
    }
    draw_with(&app, &theme, 100, 32);
    let hit = unlock(&app).expect("the button is back");
    let effects = update(&mut app, Event::Mouse { kind: MouseKind::Down, column: hit.rect.x, row: hit.rect.y });
    assert!(matches!(effects.as_slice(), [Effect::Unlock(_)]), "{effects:?}");
    assert!(app.pending.is_some());
    update(&mut app, press(KeyCode::Esc));
    assert_eq!(app.pending, None, "leaving the screen ends it");
}
