//! Every click target on every design screen does something: a target that looks clickable but
//! leaves the app as it was is a dead link to the person using it.

mod common;

use common::draw;
use nodo_tui::app::{App, AppPageState, Event, MouseKind, Screen};
use nodo_tui::fixtures::{fixture, press};
use nodo_ui_core::flows::settings::Undo;
use nodo_ui_core::model::FieldValue;
use nodo_ui_core::screen::ScreenId;
use nodo_tui::hit::{hit_at, Click, Hit};
use nodo_tui::update::update;
use ratatui::buffer::Buffer;

/// Targets known to do nothing on their fixture, as (screen, the text the target covers). Each
/// needs a comment saying why it is allowed. Empty: every target does something.
const ALLOWED_DEAD: &[(&str, &str)] = &[];

/// The app as a click can change it: everything but where the last frame was drawn and hovered.
fn observable(app: &App) -> App {
    let mut app = app.clone();
    app.hits = Default::default();
    app.hover = None;
    app
}

/// A cell where `hit` is what a click finds, so a smaller target over it does not stand in.
fn cell_for(hits: &[Hit], hit: &Hit) -> Option<(u16, u16)> {
    let rect = hit.rect;
    (rect.y..rect.y + rect.height)
        .flat_map(|y| (rect.x..rect.x + rect.width).map(move |x| (x, y)))
        .find(|&(x, y)| hit_at(hits, x, y).as_ref() == Some(hit))
}

fn covered_text(buffer: &Buffer, hit: &Hit) -> String {
    let rect = hit.rect;
    let mut text = String::new();
    for x in rect.x..rect.x + rect.width {
        text.push_str(buffer[(x, rect.y)].symbol());
    }
    text.trim().to_owned()
}

fn drawn(app: App) -> (App, Buffer, Vec<Hit>) {
    let (width, height) = app.size;
    let (buffer, _) = draw(&app, width, height);
    let hits = app.hits.borrow().clone();
    (app, buffer, hits)
}

/// Clicks each target `make` draws, each on a fresh app, and lists those that change nothing.
fn dead_targets(name: &str, make: impl Fn() -> App) -> Vec<String> {
    let mut dead = Vec::new();
    let (_, buffer, hits) = drawn(make());
    for (index, hit) in hits.iter().enumerate() {
        let text = covered_text(&buffer, hit);
        if ALLOWED_DEAD.contains(&(name, text.as_str())) {
            continue;
        }
        let Some((column, row)) = cell_for(&hits, hit) else {
            dead.push(format!("{name}: {:?} \"{text}\" is covered by other targets", hit.click));
            continue;
        };
        let (mut app, _, again) = drawn(make());
        assert_eq!(again[index], *hit, "{name}: the same app draws the same targets");
        let (mut column, mut row) = (column, row);
        // Whether it may rightly do nothing: a row that only takes the focus, already on it,
        // with no other row on screen to move the focus to first (a short terminal).
        let mut alone = false;
        match hit.click {
            // The tab bar's own tab, on that tab's first page: already there.
            Click::Tab(tab) if app.stack.is_empty() && app.screen.tab() == Some(tab) => continue,
            // A click that only moves the selection does nothing when the selection is already
            // there, so move it to another row first.
            Click::Pick { index: at, then: None } => {
                match again.iter().find(|other| matches!(other.click, Click::Pick { index, then: None } if index != at)) {
                    Some(other) => {
                        if let Some((x, y)) = cell_for(&again, other) {
                            update(&mut app, Event::Mouse { kind: MouseKind::Down, column: x, row: y });
                            draw(&app, app.size.0, app.size.1);
                            // A list that follows the focus may have scrolled: click the target
                            // where it is now, or skip it if it scrolled out of view.
                            let moved = app.hits.borrow().clone();
                            match moved.iter().find(|h| h.click == hit.click).and_then(|h| cell_for(&moved, h)) {
                                Some(cell) => (column, row) = cell,
                                None => continue,
                            }
                        }
                    }
                    None => alone = true,
                }
            }
            _ => {}
        }
        let before = observable(&app);
        let effects = update(&mut app, Event::Mouse { kind: MouseKind::Down, column, row });
        if effects.is_empty() && observable(&app) == before && !alone {
            dead.push(format!("{name}: {:?} \"{text}\" does nothing", hit.click));
        }
    }
    dead
}

#[test]
fn every_click_target_on_every_screen_does_something() {
    let dead: Vec<String> = ScreenId::ALL.into_iter().flat_map(|id| dead_targets(id.slug(), || fixture(id).0)).collect();
    assert!(dead.is_empty(), "dead click targets:\n{}", dead.join("\n"));
}

/// cuprated has no web page, so its page cannot open Hosting.
fn page_without_a_site() -> App {
    let (mut app, _) = fixture(ScreenId::AppPage);
    assert!(!app.data.web.sites.iter().any(|site| site.package == "cuprated"), "the mock gives cuprated no site");
    app.screen = Screen::AppPage(AppPageState::new("cuprated"));
    app
}

#[test]
fn hosting_on_an_app_without_a_web_page_is_not_a_link() {
    let dead = dead_targets("cuprated page", page_without_a_site);
    assert!(dead.is_empty(), "dead click targets:\n{}", dead.join("\n"));
    let (app, buffer, hits) = drawn(page_without_a_site());
    let line = common::text(&buffer).lines().find(|line| line.contains("Hosting")).expect("the Hosting row is drawn").to_owned();
    assert!(line.contains("no web page"), "it says why: {line}");
    assert!(!line.contains('›'), "it does not look like a link: {line}");
    let row = common::text(&buffer).lines().position(|line| line.contains("Hosting")).expect("drawn") as u16;
    assert!(hits.iter().all(|hit| hit.rect.y > row || hit.rect.y + hit.rect.height <= row), "nothing to click on it");
    // Enter on it (Hosting is the first row, selected when the page opens) stays put.
    let mut app = app;
    let before = observable(&app);
    let effects = update(&mut app, nodo_tui::fixtures::press(crossterm::event::KeyCode::Enter));
    assert!(effects.is_empty() && observable(&app) == before, "Enter does not pretend to open it");
}

#[test]
fn clicking_a_search_box_focuses_it_and_types_nothing() {
    for id in [ScreenId::Search, ScreenId::Reference, ScreenId::Logs] {
        let (mut app, _) = fixture(id);
        let (width, height) = app.size;
        let (buffer, _) = draw(&app, width, height);
        let hits = app.hits.borrow().clone();
        let field = hits.iter().find(|hit| hit.click == Click::Focus).unwrap_or_else(|| panic!("{}: a search box target", id.slug()));
        // Into the box, well right of the label.
        let column = field.rect.x + field.rect.width - 4;
        assert_ne!(buffer[(column, field.rect.y)].symbol(), "S", "{}: the click lands in the box", id.slug());
        for _ in 0..2 {
            update(&mut app, Event::Mouse { kind: MouseKind::Down, column, row: field.rect.y });
            draw(&app, width, height);
        }
        let (focused, text) = match &app.screen {
            Screen::Search(state) => (!state.list_focused, state.query.text().to_owned()),
            Screen::Reference(state) => (!state.list_focused, state.query.text().to_owned()),
            Screen::Logs(state) => (state.finding, state.find.text().to_owned()),
            other => panic!("{}: still on the screen, not {:?}", id.slug(), other.id()),
        };
        assert!(focused, "{}: the box has the cursor", id.slug());
        assert!(!text.contains('/'), "{}: no key was typed into the box: {text:?}", id.slug());
    }
}

/// The Sources link opens Sources while the search box has the cursor (as it does when Get apps
/// opens), rather than typing its key into the box.
#[test]
fn clicking_sources_while_typing_a_search_opens_sources() {
    let (mut app, _) = fixture(ScreenId::Search);
    if let Screen::Search(state) = &mut app.screen {
        state.list_focused = false;
    }
    let (_, buffer, hits) = drawn(app.clone());
    draw(&app, app.size.0, app.size.1);
    let link = hits.iter().find(|hit| covered_text(&buffer, hit) == "Sources ›").expect("the Sources link is a target");
    let (column, row) = cell_for(&hits, link).expect("nothing covers it");
    update(&mut app, Event::Mouse { kind: MouseKind::Down, column, row });
    assert_eq!(app.screen.id(), ScreenId::Feeds, "the click opens Sources");
}

/// Clicks the target covering `label` on what `app` draws.
fn click_text(app: &mut App, label: &str) -> Vec<nodo_ui_core::Effect> {
    let (_, buffer, hits) = drawn(app.clone());
    draw(app, app.size.0, app.size.1);
    let target = hits.iter().find(|hit| covered_text(&buffer, hit) == label).unwrap_or_else(|| panic!("\"{label}\" is a target"));
    let (column, row) = cell_for(&hits, target).expect("nothing covers it");
    update(app, Event::Mouse { kind: MouseKind::Down, column, row })
}

/// Problems and Save to a file work while the log search box has the cursor, rather than typing
/// their keys into it.
#[test]
fn log_links_work_while_typing_a_search() {
    let (mut app, _) = fixture(ScreenId::Logs);
    let Screen::Logs(state) = &mut app.screen else { panic!("the logs fixture") };
    state.finding = true;
    let was = state.errors_only;
    click_text(&mut app, "Problems");
    let Screen::Logs(state) = &app.screen else { panic!("still on Logs") };
    assert_eq!(state.errors_only, !was, "Problems toggles");
    assert_eq!(state.find.text(), "", "nothing typed into the search");
    let effects = click_text(&mut app, "Save to a file ›");
    assert!(!effects.is_empty(), "Save to a file does something");
    let Screen::Logs(state) = &app.screen else { panic!("still on Logs") };
    assert_eq!(state.find.text(), "", "nothing typed into the search");
}

/// The undo link undoes while another field's editor is open, rather than typing a u into it.
#[test]
fn undo_link_works_while_a_field_is_being_edited() {
    let (mut app, _) = fixture(ScreenId::Settings);
    let Screen::Settings(state) = &mut app.screen else { panic!("the settings fixture") };
    let after = state.draft[0].clone();
    state.undo = Some(Undo { field: 0, before: FieldValue::Text("http://old".into()), after, label: "Put it back".into() });
    state.selected = 1;
    update(&mut app, press(crossterm::event::KeyCode::Enter));
    let Screen::Settings(state) = &app.screen else { panic!("still on Settings") };
    assert!(state.editor.is_some(), "the second field's editor is open");
    click_text(&mut app, "↺ Put it back");
    let Screen::Settings(state) = &app.screen else { panic!("still on Settings") };
    assert_eq!(state.draft[0], FieldValue::Text("http://old".into()), "the undo happened");
    assert!(state.undo.is_none() && state.editor.is_none(), "the step is used and the editor closed");
}

/// A settings form like the handicrafts shop's: far taller than the screen, with a "+ reference"
/// link on most fields.
fn long_form() -> nodo_ui_core::model::SettingsForm {
    use nodo_ui_core::model::{Field, FieldKind, SettingsForm};
    let labels = [
        "Public address", "Admin user", "Admin password hash", "Notifications to", "Email", "Send from", "Sender name",
        "Tuta account", "Tuta password", "SMTP server", "SMTP port", "SMTP security", "SMTP user", "SMTP password",
        "PayID", "Account name", "BSB", "Account number",
    ];
    let fields = labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let (kind, value) = match label {
                l if l.contains("password") => (FieldKind::Secret, FieldValue::SecretUnset),
                &"Email" | &"SMTP security" => (FieldKind::Choice(vec!["off".into(), "on".into()]), FieldValue::Text("off".into())),
                &"SMTP port" => (FieldKind::Number { min: Some(1), max: Some(65535) }, FieldValue::Text(String::new())),
                _ => (FieldKind::Text, FieldValue::Text(String::new())),
            };
            Field {
                key: format!("field_{index}"),
                label: (*label).to_owned(),
                help: "Optional".to_owned(),
                requirement: "Optional".to_owned(),
                required: false,
                kind,
                value,
            }
        })
        .collect();
    SettingsForm { package: "shop".into(), fields, public_settings: Vec::new() }
}

/// Scrolled to the middle or the end, every field label and "+ reference" link on screen picks
/// its own field. Targets of rows scrolled away used to stay in the list, so a hidden field's
/// "+ reference" took the first one on screen (opening the picker for the hidden field) and the
/// labels above it found no target at all: clicking them did nothing.
#[test]
fn a_scrolled_settings_form_clicks_each_field_it_shows() {
    let form = long_form();
    let labels: Vec<String> = form.fields.iter().map(|f| f.label.clone()).collect();
    for selected in [0, 9, form.fields.len() - 1] {
        let mut app = App::new();
        app.size = (100, 30);
        let mut state = nodo_tui::app::SettingsState::new(form.clone());
        state.selected = selected;
        app.screen = Screen::Settings(state);
        let (_, buffer, hits) = drawn(app.clone());
        let screen = common::text(&buffer);
        assert!(screen.contains("more — scroll"), "the form is taller than the screen:\n{screen}");
        let mut field_above: Option<usize> = None;
        let mut checked = 0;
        for (row, line) in screen.lines().enumerate() {
            let trimmed = line.trim_start().trim_start_matches("▸ ");
            let own = labels.iter().position(|label| {
                trimmed.strip_prefix(label.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with("  "))
            });
            let mut click = |column: usize, expect: usize, what: &str| {
                let column = line[..column].chars().count() as u16;
                let hit = hit_at(&hits, column, row as u16);
                let Some(Click::Pick { index, .. }) = hit.as_ref().map(|hit| hit.click.clone()) else {
                    panic!("selected {selected}: {what} on row {row} is not a target ({hit:?}):\n{screen}");
                };
                assert_eq!(index, expect, "selected {selected}: {what} on row {row} picks field {index}:\n{screen}");
                checked += 1;
            };
            if let Some(index) = own {
                field_above = Some(index);
                click(line.find(labels[index].as_str()).expect("on the line"), index, &labels[index]);
            }
            if let (Some(at), Some(index)) = (line.find("+ reference"), field_above) {
                click(at, index, "+ reference");
            }
        }
        assert!(checked >= 6, "selected {selected}: enough of the form is on screen to check ({checked})");
    }
}

/// The same for a list of cards (`Screen3::stack`): with more updates than fit, scrolled to the
/// end, every app row on screen picks its own update, even when the name of an app scrolled away
/// ("app-1") is part of one on screen ("app-12").
#[test]
fn a_scrolled_update_list_clicks_each_app_it_shows() {
    let (mut app, _) = nodo_tui::fixtures::fixture(ScreenId::Updates);
    let template = app.data.updates.iter().find(|item| item.name == "moneropay").expect("a sample update").clone();
    for n in 0..30 {
        let mut item = template.clone();
        item.name = format!("app-{n}");
        app.data.updates.push(item);
    }
    app.size = (100, 30);
    let last = app.update_rows().len() - 1;
    let Screen::Updates(state) = &mut app.screen else { panic!("the updates fixture") };
    state.selected = last;
    let names: Vec<String> = app.update_rows().iter().map(|item| item.name.clone()).collect();
    let (_, buffer, hits) = drawn(app.clone());
    let screen = common::text(&buffer);
    assert!(screen.contains("— scroll") && !screen.contains("app-0 "), "the list is scrolled:\n{screen}");
    let mut checked = 0;
    for (row, line) in screen.lines().enumerate() {
        let Some(index) = names.iter().position(|name| line.split_whitespace().any(|word| word == name)) else { continue };
        let column = line[..line.find(names[index].as_str()).expect("on the line")].chars().count() as u16;
        let click = hit_at(&hits, column, row as u16).map(|hit| hit.click);
        assert!(matches!(click, Some(Click::Pick { index: at, .. }) if at == index), "row {row} ({}) clicks {click:?}:\n{screen}", names[index]);
        checked += 1;
    }
    assert!(checked >= 5, "{checked} rows checked:\n{screen}");
}

/// A screen's text with the focus marks and spacing taken out, to recognise a row whether or not
/// it is the focused one.
fn plain(line: &str) -> String {
    line.replace(['▸', '▏'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The rows that show the focus: the ▸ mark, or a text box's cursor.
fn marked(screen: &str) -> Vec<String> {
    screen.lines().filter(|line| line.contains('▸') || line.contains('▏')).map(plain).collect()
}

/// Every screen on a device with long lists (the awkward fixture app installed, two dozen more
/// apps, updates and sites, a dozen sources, eight DNS accounts), at small sizes, scrolled to the
/// top, part way and the end with the arrow keys. On each: every target does something, and a
/// click on a row picks that row, not one scrolled away: picking it puts the focus mark on the
/// row that was clicked.
#[test]
fn every_click_target_on_every_scrolled_screen_does_what_it_shows() {
    let snapshot = {
        let mut mock = common::forms::crowded();
        nodo_ui_core::exec::load_snapshot(&mut mock).expect("the mock answers")
    };
    let mut screens: Vec<(String, App)> = ScreenId::ALL.into_iter().map(|id| (id.slug().to_owned(), fixture(id).0)).collect();
    // The fixture's own pages: its settings form and its app page.
    let mock = common::forms::crowded();
    let form = nodo_ui_core::model::ScreenData::settings_form(&mock, common::forms::ID).expect("installed");
    let mut settings = App::new();
    settings.screen = Screen::Settings(nodo_tui::app::SettingsState::new(form));
    screens.push(("fixture settings".to_owned(), settings));
    let mut page = App::new();
    page.screen = Screen::AppPage(AppPageState::new(common::forms::ID));
    screens.push(("fixture app page".to_owned(), page));
    // The reference picker and automatic updates keep their lists in the screen, not the data.
    let (mut picker, _) = fixture(ScreenId::Reference);
    if let Screen::Reference(state) = &mut picker.screen {
        state.query = nodo_ui_core::secret::Input::plain("");
        let template = state.list.options[0].clone();
        for n in 0..30 {
            let mut option = template.clone();
            option.app = format!("crowd-app-{n:02}");
            state.list.options.push(option);
        }
    }
    screens.push(("long reference list".to_owned(), picker));
    let (mut auto, _) = fixture(ScreenId::AutoUpdate);
    if let Screen::AutoUpdate(state) = &mut auto.screen {
        // On the first app: "How often" only moves the focus, which does nothing where it is.
        state.selected = 1;
        for n in 0..24 {
            state.draft.packages.insert(0, nodo_ui_core::model::AutoUpdateEntry::new(&format!("crowd-app-{n:02}"), "community", true, ""));
        }
        state.saved = state.draft.clone();
    }
    screens.push(("long automatic updates".to_owned(), auto));
    screens.push(("hosting with other names".to_owned(), hosting_with_names(&snapshot, 0)));
    // Each screen on a thread of its own: some 400 states, each clicked target by target.
    let failures: Vec<String> = std::thread::scope(|scope| {
        let workers: Vec<_> = screens
            .into_iter()
            .map(|(name, base)| {
                let snapshot = snapshot.clone();
                scope.spawn(move || {
                    let mut failures = Vec::new();
                    for size in [(60, 20), (80, 24), (100, 32)] {
                        for downs in [0, 5, 60] {
                            let mut app = base.clone();
                            app.data = snapshot.clone();
                            app.size = size;
                            for _ in 0..downs {
                                draw(&app, size.0, size.1);
                                update(&mut app, press(crossterm::event::KeyCode::Down));
                            }
                            let at = format!("{name} {}x{} after {downs} downs", size.0, size.1);
                            let prepared = app.clone();
                            failures.extend(dead_targets(&at, || prepared.clone()));
                            failures.extend(misplaced_picks(&at, &prepared));
                        }
                    }
                    failures
                })
            })
            .collect();
        workers.into_iter().flat_map(|worker| worker.join().expect("no panic")).collect()
    });
    assert!(failures.is_empty(), "{} problems:\n{}", failures.len(), failures.join("\n"));
}

/// Picks each row target without pressing anything after, and says where the focus mark did not
/// land on the row clicked.
fn misplaced_picks(at: &str, prepared: &App) -> Vec<String> {
    let mut problems = Vec::new();
    let (_, buffer, hits) = drawn(prepared.clone());
    let before = common::text(&buffer);
    let card_width = prepared.size.0.saturating_sub(8);
    for (index, hit) in hits.iter().enumerate() {
        let Click::Pick { index: pick, .. } = hit.click else { continue };
        // Rows and labels: the targets as wide as the card or the screen.
        if hit.rect.width < card_width {
            continue;
        }
        let Some((column, row)) = cell_for(&hits, hit) else { continue };
        let clicked = plain(before.lines().nth(usize::from(row)).unwrap_or_default());
        let mut app = prepared.clone();
        draw(&app, app.size.0, app.size.1);
        app.hits.borrow_mut()[index].click = Click::Pick { index: pick, then: None };
        update(&mut app, Event::Mouse { kind: MouseKind::Down, column, row });
        let (buffer, _) = draw(&app, app.size.0, app.size.1);
        let now = marked(&common::text(&buffer));
        // A screen that shows no focus for what was picked (a link among buttons) is left out;
        // a focus that moved must have moved to the row clicked. A focused row may say more.
        if now != marked(&before) && !now.iter().any(|line| line.starts_with(&clicked) || clicked.starts_with(line.as_str())) {
            problems.push(format!("{at}: picking \"{clicked}\" (row {row}, index {pick}) marks {now:?}"));
        }
    }
    problems
}

/// On a narrow terminal a source's on/off switch and its "Try again" link stay whole on one row
/// and clickable. Padded to column 44, the switch ran past a 60-column card and wrapped ("[x]"
/// on one row, "On" on the next), and "Try again" wrapped with the problem before it: neither
/// was a target any more.
#[test]
fn a_sources_switch_and_link_stay_clickable_when_narrow() {
    for size in [(60, 20), (80, 24), (100, 32)] {
        let (mut app, _) = fixture(ScreenId::Feeds);
        app.size = size;
        let feeds = app.data.feeds.clone();
        for (index, feed) in feeds.iter().enumerate() {
            app.screen = Screen::Feeds { selected: index, keys: false };
            let (_, buffer, hits) = drawn(app.clone());
            let screen = common::text(&buffer);
            let lines: Vec<&str> = screen.lines().collect();
            let head = lines.iter().position(|line| line.trim_start().trim_start_matches("▸ ").starts_with(&format!("{} ", feed.name))).unwrap_or_else(|| panic!("{size:?}: {} is on screen:\n{screen}", feed.name));
            let switch = if feed.status == nodo_ui_core::model::FeedStatus::Disabled { "[ ] Off" } else { "[x] On" };
            let column = lines[head].find(switch).unwrap_or_else(|| panic!("{size:?}: {}'s switch is whole on its row:\n{screen}", feed.name));
            let click = hit_at(&hits, lines[head][..column].chars().count() as u16, head as u16).map(|hit| hit.click);
            assert_eq!(click, Some(Click::Pick { index, then: Some(crossterm::event::KeyCode::Char(' ')) }), "{size:?}: {}'s switch", feed.name);
            // At 20 rows the window shows the source's head and address only.
            if matches!(feed.status, nodo_ui_core::model::FeedStatus::Unreachable(_)) && size.1 >= 24 {
                let row = lines.iter().position(|line| line.contains("Try again")).unwrap_or_else(|| panic!("{size:?}: Try again is whole on a row:\n{screen}"));
                let column = lines[row][..lines[row].find("Try again").unwrap()].chars().count() as u16;
                assert_eq!(hit_at(&hits, column, row as u16).map(|hit| hit.click), Some(Click::Key(crossterm::event::KeyCode::Char('r'))), "{size:?}");
            }
        }
    }
}

/// monokulo-pay's Hosting, going public, with a redirecting and a serving name already listed and
/// the controls to add another. `focus` is where the keyboard focus starts.
fn hosting_with_names(snapshot: &nodo_ui_core::model::Snapshot, focus: usize) -> App {
    use nodo_api::{AliasMode, PublicAlias};
    use nodo_tui::app::ExposureState;
    let mut app = App::new();
    app.data = snapshot.clone();
    let site = app.data.web.sites.iter().find(|s| s.package == "monokulo-pay").expect("a sample site").clone();
    let mut state = ExposureState::new("monokulo-pay", site.exposure, site.public_host.as_deref(), &app.domain_choices());
    state.draft.public = true;
    state.aliases = vec![
        PublicAlias { host: "pay-old.example.com".into(), mode: AliasMode::Redirect },
        PublicAlias { host: "pay.crowd1.example".into(), mode: AliasMode::Serve },
    ];
    state.adding_alias = true;
    state.selected = focus;
    app.screen = Screen::WebExposure(state);
    app
}

/// From the smallest terminal to a large one, each of the other names' controls is on screen
/// once the focus reaches it, and a click on it does what it says.
#[test]
fn a_sites_other_names_click_at_every_size() {
    use nodo_tui::app::{ExposureState, HostingFocus};
    let snapshot = {
        let mut mock = common::forms::crowded();
        nodo_ui_core::exec::load_snapshot(&mut mock).expect("the mock answers")
    };
    let state = |app: &App| -> ExposureState {
        let Screen::WebExposure(state) = &app.screen else { panic!("hosting") };
        state.clone()
    };
    let at = |focus: HostingFocus| {
        let app = hosting_with_names(&snapshot, 0);
        state(&app).order().iter().position(|f| *f == focus).expect("in the order")
    };
    for size in [(60, 20), (80, 24), (100, 32), (200, 60)] {
        let click_on = |focus: HostingFocus, label: &str| -> App {
            let mut app = hosting_with_names(&snapshot, at(focus));
            app.size = size;
            let (buffer, _) = draw(&app, size.0, size.1);
            let screen = common::text(&buffer);
            let hits = app.hits.borrow().clone();
            let (row, line) = screen.lines().enumerate().find(|(_, line)| line.contains(label)).unwrap_or_else(|| {
                panic!("{}x{}: \"{label}\" is on screen with the focus on it:\n{screen}", size.0, size.1)
            });
            let column = line[..line.find(label).unwrap()].chars().count() as u16 + 1;
            assert!(hit_at(&hits, column, row as u16).is_some(), "{}x{}: \"{label}\" is a target:\n{screen}", size.0, size.1);
            update(&mut app, Event::Mouse { kind: MouseKind::Down, column, row: row as u16 });
            app
        };
        // The controls are closed until "Add another address" is clicked.
        let mut closed = hosting_with_names(&snapshot, 0);
        if let Screen::WebExposure(s) = &mut closed.screen {
            s.adding_alias = false;
            s.selected = s.order().iter().position(|f| *f == HostingFocus::AliasOpen).expect("the button is in the order");
        }
        closed.size = size;
        let (buffer, _) = draw(&closed, size.0, size.1);
        let screen = common::text(&buffer);
        assert!(!screen.contains("New name"), "{}x{}: the controls are closed:\n{screen}", size.0, size.1);
        let hits = closed.hits.borrow().clone();
        let (row, line) = screen.lines().enumerate().find(|(_, line)| line.contains("Add another address")).expect("the button that opens them");
        let column = line[..line.find("Add another address").unwrap()].chars().count() as u16 + 1;
        assert!(hit_at(&hits, column, row as u16).is_some(), "{}x{}: the button is a target:\n{screen}", size.0, size.1);
        update(&mut closed, Event::Mouse { kind: MouseKind::Down, column, row: row as u16 });
        assert!(state(&closed).adding_alias, "{}x{}: a click opens the controls", size.0, size.1);
        assert_eq!(state(&closed).focus(), HostingFocus::AliasDomain, "{}x{}", size.0, size.1);
        let removed = click_on(HostingFocus::Alias(1), "Remove");
        assert_eq!(state(&removed).aliases.len(), 1, "{}x{}: a Remove click removes a name", size.0, size.1);
        let toggled = click_on(HostingFocus::AliasMode, "redirects");
        assert_eq!(state(&toggled).alias_mode, nodo_api::AliasMode::Serve, "{}x{}", size.0, size.1);
        let opened = click_on(HostingFocus::AliasDomain, "New name");
        assert_eq!(state(&opened).focus(), HostingFocus::AliasDomain, "{}x{}", size.0, size.1);
        let mut typed = hosting_with_names(&snapshot, at(HostingFocus::AliasAddress));
        for c in "shop2".chars() {
            update(&mut typed, press(crossterm::event::KeyCode::Char(c)));
        }
        typed.size = size;
        let added = {
            let mut app = typed;
            if let Screen::WebExposure(s) = &mut app.screen {
                s.selected = at(HostingFocus::AliasAdd);
            }
            let (buffer, _) = draw(&app, size.0, size.1);
            let screen = common::text(&buffer);
            let hits = app.hits.borrow().clone();
            let (row, line) = screen.lines().enumerate().find(|(_, line)| line.contains("Add this name")).expect("the Add button");
            let column = line[..line.find("Add this name").unwrap()].chars().count() as u16 + 1;
            assert!(hit_at(&hits, column, row as u16).is_some(), "{}x{}: Add is a target:\n{screen}", size.0, size.1);
            update(&mut app, Event::Mouse { kind: MouseKind::Down, column, row: row as u16 });
            app
        };
        let names: Vec<String> = state(&added).aliases.iter().map(|a| a.host.clone()).collect();
        assert!(names.iter().any(|n| n.starts_with("shop2.")), "{}x{}: Add puts the typed name on the list: {names:?}", size.0, size.1);
    }
}
