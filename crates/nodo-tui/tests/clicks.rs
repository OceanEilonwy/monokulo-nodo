//! Every click target on every design screen does something: a target that looks clickable but
//! leaves the app as it was is a dead link to the person using it.

mod common;

use common::draw;
use nodo_tui::app::{App, AppPageState, Event, MouseKind, Screen, ScreenId, Undo};
use nodo_tui::fixtures::{fixture, press};
use nodo_tui::model::FieldValue;
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
        match hit.click {
            // The tab bar's own tab, on that tab's first page: already there.
            Click::Tab(tab) if app.stack.is_empty() && app.screen.tab() == Some(tab) => continue,
            // A click that only moves the selection does nothing when the selection is already
            // there, so move it to another row first.
            Click::Pick { index: at, then: None } => {
                if let Some(other) = again.iter().find(|other| matches!(other.click, Click::Pick { index, then: None } if index != at)) {
                    if let Some((x, y)) = cell_for(&again, other) {
                        update(&mut app, Event::Mouse { kind: MouseKind::Down, column: x, row: y });
                        draw(&app, app.size.0, app.size.1);
                    }
                }
            }
            _ => {}
        }
        let before = observable(&app);
        let effects = update(&mut app, Event::Mouse { kind: MouseKind::Down, column, row });
        if effects.is_empty() && observable(&app) == before {
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
fn click_text(app: &mut App, label: &str) -> Vec<nodo_tui::app::Effect> {
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
fn long_form() -> nodo_tui::model::SettingsForm {
    use nodo_tui::model::{Field, FieldKind, SettingsForm};
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
