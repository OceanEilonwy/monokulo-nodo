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

/// A settings form drawn tall enough for every field, at `width` columns, with some labels
/// replaced and the port's value set.
fn settings_at(width: u16, relabel: &[(usize, &str)], port: Option<&str>) -> String {
    let (mut app, _) = fixture(ScreenId::Settings);
    if let nodo_tui::app::Screen::Settings(state) = &mut app.screen {
        for (index, label) in relabel {
            state.form.fields[*index].label = (*label).to_owned();
        }
        if let Some(port) = port {
            state.draft[1] = nodo_tui::model::FieldValue::Text(port.to_owned());
        }
    }
    app.size = (width, 60);
    text(&draw(&app, width, 60).0)
}

/// Every control ends on the card's inner right padding, three cells inside the card, which
/// ends on the page margin: its last cell, text or an inset box's fill, is at `width - 8`,
/// whatever the terminal's width.
#[test]
fn settings_controls_are_right_aligned_at_any_width() {
    let labels = ["Daemon RPC", "API port", "Log level", "Payment address", "Webhook secret", "Allow LAN access"];
    let field = Theme::truecolor().field;
    for width in [80u16, 100, 160] {
        let (mut app, _) = fixture(ScreenId::Settings);
        app.size = (width, 60);
        let buffer = draw(&app, width, 60).0;
        let screen = text(&buffer);
        for label in labels {
            let mut y = screen.lines().position(|line| line.contains(label)).unwrap_or_else(|| panic!("{label} at {width}:\n{screen}"));
            // A wide control at 80 columns takes the row under its label.
            if screen.lines().nth(y).is_some_and(|line| line.ends_with(label)) {
                y += 1;
            }
            let last = (0..width).rev().find(|&x| {
                let cell = &buffer[(x, y as u16)];
                cell.symbol() != " " || cell.bg == field
            });
            // "[x] On " ends in a blank, so that On and Off have the same width.
            let end = if label == "Allow LAN access" { width - 9 } else { width - 8 };
            assert_eq!(last, Some(end), "{label} at {width} does not end on the padding:\n{screen}");
        }
    }
}

/// A number's box has one width, so − and + stay in the same columns whatever the value.
#[test]
fn number_steppers_line_up_whatever_the_value() {
    let column = |port: &str| {
        let screen = settings_at(100, &[], Some(port));
        let row = screen.lines().find(|line| line.contains("API port")).expect("label shown").to_owned();
        (row.chars().position(|c| c == '−'), row.chars().position(|c| c.is_ascii_digit()))
    };
    assert_eq!(column("8"), column("18081"));
}

/// The help sits under the label, in the left column, never under the control.
#[test]
fn settings_help_wraps_under_the_label() {
    for width in [80u16, 100, 160] {
        let screen = settings_at(width, &[], None);
        let lines: Vec<&str> = screen.lines().collect();
        let at = lines.iter().position(|line| line.contains("API port")).expect("label shown");
        let label_col = lines[at].find("API port").expect("label");
        assert_eq!(lines[at + 1].find("Port for the local payment API."), Some(label_col), "help not under the label at {width}:\n{screen}");
        let at = lines.iter().position(|line| line.contains("Log level")).expect("label shown");
        let control_col = lines[at].find("info").expect("value");
        for help in lines[at + 1..].iter().take_while(|line| !line.trim().is_empty()) {
            assert_eq!(help.find(|c: char| c != ' '), Some(label_col), "help not under the label at {width}:\n{screen}");
            assert!(help.chars().count() < control_col, "help runs under the control at {width}:\n{screen}");
        }
    }
}

/// Labels longer than the old 24-cell column ("Delete groups after (days)") never touch their
/// control: beside it with a gap when there is room, or with the control on the next row.
#[test]
fn long_setting_labels_never_overlap_their_control() {
    let long = [(0, "Restricted RPC on the network"), (1, "Delete groups after (days)"), (3, "Address to send payouts to")];
    for width in [80u16, 100, 160] {
        let screen = settings_at(width, &long, None);
        for (_, label) in long {
            let row = screen.lines().find(|line| line.contains(label)).unwrap_or_else(|| panic!("{label} at {width}:\n{screen}"));
            let after = row.split(label).nth(1).unwrap_or_default();
            assert!(after.is_empty() || after.starts_with("   "), "{label} touches its control at {width}:\n{screen}");
        }
        let row = screen.lines().find(|line| line.contains("Delete groups after (days)")).expect("label shown");
        assert!(row.ends_with("−   +"), "the number left its label's row at {width}:\n{screen}");
    }
}

/// The installed apps fill a wide terminal, "›" on the card's right padding, and still fit on
/// one row each at 80 columns, the tagline cut before the status.
#[test]
fn app_rows_fill_the_width_and_fit_at_80() {
    for width in [80u16, 100, 160] {
        let (mut app, _) = fixture(ScreenId::Apps);
        app.size = (width, 40);
        let (buffer, report) = draw(&app, width, 40);
        let screen = text(&buffer);
        assert!(report.fits(), "{width}: {report:?}\n{screen}");
        for (name, status) in [("cuprated", "Running"), ("moneropay", "Update ready"), ("bell", "Keeps stopping"), ("monerod", "Stopped")] {
            let row = screen.lines().find(|line| line.contains(name)).unwrap_or_else(|| panic!("{name} at {width}:\n{screen}"));
            assert!(row.contains(status) && row.ends_with('›'), "{name} at {width}: status or › missing:\n{screen}");
            assert_eq!(row.chars().count(), usize::from(width) - 7, "{name} at {width}: › is not on the padding:\n{screen}");
        }
    }
    let (mut app, _) = fixture(ScreenId::Apps);
    app.size = (160, 40);
    let screen = text(&draw(&app, 160, 40).0);
    assert!(screen.contains("Alerts when a wallet receives"), "a tagline is cut at 160:\n{screen}");
}

/// A long tagline and a long status at 80 columns: the tagline is cut first, the status kept.
#[test]
fn app_rows_cut_the_tagline_before_the_status_at_80() {
    let (mut app, _) = fixture(ScreenId::Apps);
    if let Some(pkg) = app.data.catalog.iter_mut().find(|pkg| pkg.id.name == "cuprated") {
        pkg.tagline = "A Monero node written in Rust, fast to sync and light on memory".to_owned();
    }
    if let Some(svc) = app.data.services.iter_mut().find(|svc| svc.id == "cuprated") {
        svc.detail = "synced to block 3,141,592".to_owned();
    }
    app.size = (80, 40);
    let (buffer, report) = draw(&app, 80, 40);
    let screen = text(&buffer);
    assert!(report.fits(), "{report:?}\n{screen}");
    let row = screen.lines().find(|line| line.contains("cuprated")).expect("shown");
    assert!(row.contains("A Monero node") && row.contains('…'), "the tagline is not cut:\n{screen}");
    assert!(row.contains("Running · synced to block") && row.ends_with('›'), "the status is lost:\n{screen}");
    assert_eq!(row.chars().count(), 73, "the row does not fit:\n{screen}");
}
