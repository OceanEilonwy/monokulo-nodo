//! Every click target on every design screen does something: a target that looks clickable but
//! leaves the app as it was is a dead link to the person using it.

mod common;

use common::draw;
use nodo_tui::app::{App, AppPageState, Event, MouseKind, Screen, ScreenId};
use nodo_tui::fixtures::fixture;
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
