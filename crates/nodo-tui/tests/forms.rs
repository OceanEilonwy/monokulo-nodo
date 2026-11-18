//! The awkward fixture app's settings form (tests/fixtures/nodo-fixture-forms) at every terminal
//! size and scroll position: every click lands on the field it shows, the keyboard reaches every
//! field, each optional kind edits, clears and saves, required fields are asked for, and long
//! labels and help never hide a value. The form before it had at most 10 settings; the first real
//! third-party app had 27 and broke clicks on a scrolled form and saving an empty optional int.

mod common;

use common::forms::{self, ID};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nodo_api::{Request, SettingChange, SettingInput, SettingLiteral};
use nodo_settings::SettingType;
use nodo_tui::app::{App, Event, MouseKind, Screen, SettingsState};
use nodo_tui::fixtures::{ctrl, press};
use nodo_tui::hit::{hit_at, Click, Hit};
use nodo_tui::mock::MockBackend;
use nodo_tui::model::{FieldValue, ScreenData};
use nodo_tui::runtime::dispatch;
use nodo_tui::update::update;
use ratatui::buffer::Buffer;

/// The sizes the form must work at: the smallest the TUI runs at with room for a form, the
/// classic terminal, the design size, and a big window.
const SIZES: [(u16, u16); 4] = [(60, 20), (80, 24), (100, 32), (200, 60)];

#[test]
fn the_fixture_is_valid_for_the_device_and_covers_every_kind_of_setting() {
    let manifest = forms::manifest();
    assert_eq!(manifest.id, ID);
    assert!(manifest.web.len() >= 3, "several web sites");
    let schema = forms::schema();
    let settings = &schema.settings;
    assert!(settings.len() >= 35, "{} settings", settings.len());
    let has = |what: &str, test: &dyn Fn(&nodo_settings::SettingDef) -> bool| {
        assert!(settings.iter().any(test), "no setting is {what}");
    };
    has("a string", &|d| d.ty == SettingType::String);
    has("an int with min and max", &|d| matches!(d.ty, SettingType::Int { min: Some(_), max: Some(_) }));
    has("an int with neither", &|d| matches!(d.ty, SettingType::Int { min: None, max: None }));
    has("an int with only a min", &|d| matches!(d.ty, SettingType::Int { min: Some(_), max: None }));
    has("an int with only a max", &|d| matches!(d.ty, SettingType::Int { min: None, max: Some(_) }));
    has("a bool", &|d| d.ty == SettingType::Bool);
    has("an enum", &|d| matches!(d.ty, SettingType::Enum { .. }));
    has("a monero address", &|d| d.ty == SettingType::MoneroAddress);
    has("a url", &|d| d.ty == SettingType::Url);
    has("a secret", &|d| d.ty == SettingType::Secret && d.generate.is_none());
    has("a generated secret", &|d| d.generate.is_some());
    has("required without a default", &|d| d.required && d.default.is_none() && d.generate.is_none());
    has("required with a default", &|d| d.required && d.default.is_some());
    has("optional without a default", &|d| !d.required && d.default.is_none());
    has("an optional int without a default", &|d| !d.required && d.default.is_none() && matches!(d.ty, SettingType::Int { .. }));
    has("an optional enum without a default", &|d| !d.required && d.default.is_none() && matches!(d.ty, SettingType::Enum { .. }));
    has("validated", &|d| d.has_validator && !d.required);
    has("public", &|d| d.public.is_some());
    has("a reference to another app", &|d| matches!(&d.default, Some(nodo_settings::DefaultValue::Text(t)) if t.starts_with("$(cuprated.")));
    has("text and a reference", &|d| matches!(&d.default, Some(nodo_settings::DefaultValue::Text(t)) if t.contains("$(") && !t.starts_with("$(")));
    has("a label as long as allowed", &|d| d.label.len() >= 75);
    has("help longer than any terminal", &|d| d.help.as_ref().is_some_and(|h| h.len() > 400));
    has("a unicode label", &|d| !d.label.is_ascii());
    // Every validator of an optional text setting refuses an empty string, as handicrafts' do:
    // an emptied box must be sent as a reset, never as "" (`Field::input_for`).
    let loaded = forms::loaded();
    for def in settings.iter().filter(|d| d.has_validator && matches!(d.ty, SettingType::String)) {
        let empty = nodo_settings::Value::String(String::new());
        assert!(loaded.validate_value(&def.name, &empty).is_err(), "{} refuses an empty string", def.name);
    }
}

#[test]
fn the_mock_serves_the_fixture_with_its_real_schema() {
    let mut mock = forms::crowded();
    let form = mock.settings_form(ID).expect("installed in the mock");
    let schema = forms::schema();
    let keys: Vec<&str> = form.fields.iter().map(|f| f.key.as_str()).collect();
    let names: Vec<&str> = schema.settings.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(keys, names);
    let snapshot = nodo_tui::runtime::load_snapshot(&mut mock).unwrap();
    assert!(snapshot.services.iter().any(|svc| svc.id == ID));
    assert!(snapshot.web.sites.iter().filter(|s| s.package.starts_with("forms")).count() >= 3);
}

/// The fixture's settings page at `size`, with the field `selected` focused, as when the owner
/// moved there: the form scrolls to keep it in view.
fn settings_app(size: (u16, u16), selected: usize) -> App {
    let (mut app, mock) = forms::crowded_app(size);
    let mut state = SettingsState::new(mock.settings_form(ID).expect("installed"));
    state.selected = selected;
    app.screen = Screen::Settings(state);
    app
}

fn drawn(app: &App) -> (Buffer, nodo_tui::view::layout::FitReport, Vec<Hit>) {
    let (buffer, report) = common::draw(app, app.size.0, app.size.1);
    let hits = app.hits.borrow().clone();
    (buffer, report, hits)
}

/// The line's text after the card margin and the focus mark, as a label starts.
fn body(line: &str) -> &str {
    let trimmed = line.trim_start();
    trimmed.strip_prefix("▸ ").unwrap_or(trimmed)
}

/// Which field's label starts this line, if any: the whole label followed by its control, or the
/// first row of a label too long for one row.
fn label_on(line: &str, labels: &[String]) -> Option<usize> {
    let body = body(line);
    let whole = labels.iter().enumerate().filter(|(_, label)| {
        body.strip_prefix(label.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with("  "))
    });
    if let Some((index, _)) = whole.max_by_key(|(_, label)| label.len()) {
        return Some(index);
    }
    labels.iter().position(|label| body.chars().count() >= 12 && label.starts_with(body.trim_end()) && body.trim_end() != label)
}

fn column_of(line: &str, byte: usize) -> u16 {
    unicode_width::UnicodeWidthStr::width(&line[..byte]) as u16
}

/// Every label, "+ reference" and button on screen clicks what it shows, at every size and
/// scroll position (top, middle, end and each step between), and nothing else on the form is a target. Generalises
/// clicks.rs's `a_scrolled_settings_form_clicks_each_field_it_shows` to the fixture.
#[test]
fn every_click_on_the_fixture_form_lands_on_the_field_it_shows() {
    let labels: Vec<String> = forms::form().fields.iter().map(|f| f.label.clone()).collect();
    for size in SIZES {
        // Every field focused in turn: the form at the top, in the middle, at the end, and each
        // field at the window's edge on the way.
        for selected in 0..labels.len() {
            let app = settings_app(size, selected);
            let (buffer, _, hits) = drawn(&app);
            let screen = common::text(&buffer);
            let at = format!("{}x{} at field {selected}", size.0, size.1);
            // Rows above the first label on screen belong to the field before it, whose label
            // scrolled away.
            let first = screen.lines().find_map(|line| label_on(line, &labels));
            let mut field_above: Option<usize> = first.and_then(|first| first.checked_sub(1));
            let mut checked: Vec<Hit> = Vec::new();
            let mut fields_seen = 0;
            let mut continuing: Option<(usize, String)> = None;
            for (row, line) in screen.lines().enumerate() {
                let mut expect = |byte: usize, click: &dyn Fn(&Click) -> bool, what: &str| {
                    let hit = hit_at(&hits, column_of(line, byte), row as u16);
                    match hit {
                        Some(hit) if click(&hit.click) => checked.push(hit),
                        other => panic!("{at}: {what} on row {row} clicks {:?}:\n{screen}", other.map(|h| h.click)),
                    }
                };
                let text = body(line).trim_end();
                if let Some((index, rest)) = continuing.take().filter(|(_, rest)| !text.is_empty() && rest.starts_with(text)) {
                    // The next row of a label too long for one.
                    let byte = line.find(text).expect("on the line");
                    expect(byte, &|c| *c == Click::Pick { index, then: Some(KeyCode::Enter) }, &format!("the label of {} (continued)", labels[index]));
                    continuing = Some((index, rest[text.len()..].trim_start().to_owned())).filter(|(_, rest)| !rest.is_empty());
                } else if let Some(index) = label_on(line, &labels) {
                    field_above = Some(index);
                    fields_seen += 1;
                    let byte = line.find(text).expect("on the line");
                    expect(byte, &|c| *c == Click::Pick { index, then: Some(KeyCode::Enter) }, &format!("the label of {}", labels[index]));
                    let rest = labels[index].strip_prefix(text).unwrap_or_default().trim_start();
                    continuing = Some((index, rest.to_owned())).filter(|(_, rest)| !rest.is_empty());
                }
                // The link is right-aligned; the fixture's help also says "+ reference" in words.
                if let Some(byte) = line.trim_end().strip_suffix("+ reference").map(str::len) {
                    let index = field_above.unwrap_or_else(|| panic!("{at}: a + reference with no field above it on row {row}:\n{screen}"));
                    expect(byte, &|c| *c == Click::Pick { index, then: Some(KeyCode::F(4)) }, &format!("+ reference of {}", labels[index]));
                }
                if line.trim_end().ends_with("Save") && line.contains("Cancel") {
                    expect(line.rfind("Save").unwrap(), &|c| *c == Click::Ctrl('s'), "Save");
                    expect(line.find("Cancel").unwrap(), &|c| *c == Click::Key(KeyCode::Esc), "Cancel");
                }
            }
            assert!(fields_seen >= if size.1 >= 32 { 2 } else { 1 }, "{at}: {fields_seen} fields on screen:\n{screen}");
            assert!(checked.iter().any(|h| h.click == Click::Ctrl('s')), "{at}: Save is on screen:\n{screen}");
            // Nothing on the form is a target but what was checked: a stale target would sit on
            // a row it does not belong to.
            for hit in &hits {
                let form_target = matches!(hit.click, Click::Pick { .. } | Click::Ctrl('s')) || hit.button.is_some();
                if form_target && !checked.contains(hit) {
                    let line = screen.lines().nth(usize::from(hit.rect.y)).unwrap_or_default();
                    panic!("{at}: {:?} on row {} is on \"{}\":\n{screen}", hit.click, hit.rect.y, line.trim());
                }
            }
        }
    }
}

/// Tab and the arrows reach every field in order and back, and each one is on screen, focused,
/// when it is reached, at every size.
#[test]
fn the_keyboard_reaches_every_field_and_shows_it() {
    let labels: Vec<String> = forms::form().fields.iter().map(|f| f.label.clone()).collect();
    for size in SIZES {
        let mut app = settings_app(size, 0);
        let check = |app: &App, want: usize, how: &str| {
            let Screen::Settings(state) = &app.screen else { panic!("left the form") };
            assert_eq!(state.selected, want, "{how} at {size:?}");
            let (buffer, report, _) = drawn(app);
            let screen = common::text(&buffer);
            let focused = screen.lines().find(|line| line.trim_start().starts_with("▸ ")).unwrap_or_else(|| panic!("{size:?}: no focused field:\n{screen}"));
            assert_eq!(label_on(focused, &labels), Some(want), "{size:?}: {how} shows the field it reached:\n{screen}");
            assert_eq!(report.clipped_lines, 0, "{size:?}: {how} cut the page:\n{screen}");
        };
        for want in 1..labels.len() {
            let key = if want % 2 == 0 { KeyCode::Tab } else { KeyCode::Down };
            update(&mut app, press(key));
            check(&app, want, "going down");
        }
        update(&mut app, press(KeyCode::Down));
        check(&app, labels.len() - 1, "past the end");
        for want in (0..labels.len() - 1).rev() {
            let key = if want % 2 == 0 { KeyCode::BackTab } else { KeyCode::Up };
            update(&mut app, press(key));
            check(&app, want, "going up");
        }
    }
}

/// A mock with the fixture's required fields filled in, as on a device whose owner set it up.
fn set_up_mock() -> MockBackend {
    let mut mock = forms::crowded();
    let form = mock.forms.get_mut(ID).expect("installed");
    for (key, value) in [("shop_name", "Awkward Forms"), ("daily_limit", "40"), ("theme", "dark")] {
        form.fields.iter_mut().find(|f| f.key == key).expect("a fixture field").value = FieldValue::Text(value.to_owned());
    }
    mock
}

fn open_form(mock: &mut MockBackend, size: (u16, u16)) -> App {
    let mut app = App::new();
    app.data = nodo_tui::runtime::load_snapshot(mock).unwrap();
    app.size = size;
    app.screen = Screen::Settings(SettingsState::new(mock.settings_form(ID).unwrap()));
    app
}

fn index_of(app: &App, key: &str) -> usize {
    let Screen::Settings(state) = &app.screen else { panic!("on the form") };
    state.form.fields.iter().position(|f| f.key == key).unwrap_or_else(|| panic!("no field {key}"))
}

/// Moves to a field with the keyboard, drawing as it goes, as the owner would.
fn go_to(app: &mut App, mock: &mut MockBackend, key: &str) {
    let want = index_of(app, key);
    loop {
        common::draw(app, app.size.0, app.size.1);
        let Screen::Settings(state) = &app.screen else { panic!("on the form") };
        if state.selected == want {
            return;
        }
        let code = if state.selected < want { KeyCode::Down } else { KeyCode::Up };
        dispatch(app, mock, press(code));
    }
}

/// The changes the last save sent to the helper.
fn last_saved(mock: &MockBackend) -> Vec<SettingChange> {
    mock.calls
        .iter()
        .rev()
        .find_map(|call| match call {
            Request::ApplySettings { package, changes } if package == ID => Some(changes.clone()),
            _ => None,
        })
        .expect("a save reached the helper")
}

fn save(app: &mut App, mock: &mut MockBackend, what: &str) {
    mock.calls.clear();
    dispatch(app, mock, ctrl('s'));
    let notice = app.notice.as_ref().map(|n| n.text.clone()).unwrap_or_default();
    assert!(notice.starts_with("Saved."), "{what}: {notice}");
}

/// Types `text` into the focused field in place of what it holds, by keyboard.
fn retype(app: &mut App, mock: &mut MockBackend, text: &str) {
    dispatch(app, mock, press(KeyCode::Enter));
    dispatch(app, mock, Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)));
    dispatch(app, mock, Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));
    common::type_text(app, mock, text);
    dispatch(app, mock, press(KeyCode::Enter));
}

/// Each optional kind is set and saved, then cleared and saved, on a scrolled form, by keyboard.
/// A cleared field is sent as a reset: sent as "", an int fails to parse and a validator that
/// refuses "" (every one in the fixture) fails the whole save.
#[test]
fn each_optional_kind_edits_clears_and_saves() {
    let text = |s: &str| SettingInput::Literal(SettingLiteral::Text(s.to_owned()));
    let address = nodo_tui::mock::sample_address();
    let cases: [(&str, &str, SettingInput); 6] = [
        ("smtp_port", "587", text("587")),
        ("retries", "3", text("3")),
        ("clock_offset", "-30", text("-30")),
        ("mail_from", "shop@example.com", text("shop@example.com")),
        ("analytics_url", "https://stats.example.com", text("https://stats.example.com")),
        ("payout_address", &address, text(&address)),
    ];
    for size in [(60, 20), (100, 32)] {
        let mut mock = set_up_mock();
        let mut app = open_form(&mut mock, size);
        for (key, typed, sent) in &cases {
            go_to(&mut app, &mut mock, key);
            retype(&mut app, &mut mock, typed);
            save(&mut app, &mut mock, &format!("{key} set"));
            assert_eq!(last_saved(&mock), vec![SettingChange { name: (*key).to_owned(), value: sent.clone() }]);
            retype(&mut app, &mut mock, "");
            save(&mut app, &mut mock, &format!("{key} cleared"));
            assert_eq!(last_saved(&mock), vec![SettingChange { name: (*key).to_owned(), value: SettingInput::Reset }], "{key}");
        }
        // A default is put back by clearing the box, too.
        go_to(&mut app, &mut mock, "tagline");
        retype(&mut app, &mut mock, "");
        save(&mut app, &mut mock, "tagline cleared");
        assert_eq!(last_saved(&mock), vec![SettingChange { name: "tagline".to_owned(), value: SettingInput::Reset }]);
        // A secret is typed and kept; a toggle flips; an optional choice with no value starts at
        // its first choice.
        go_to(&mut app, &mut mock, "webhook_secret");
        dispatch(&mut app, &mut mock, press(KeyCode::Enter));
        common::type_text(&mut app, &mut mock, "whsec_abc123");
        dispatch(&mut app, &mut mock, press(KeyCode::Enter));
        save(&mut app, &mut mock, "webhook secret");
        assert!(matches!(&last_saved(&mock)[..], [SettingChange { name, value: SettingInput::Secret(_) }] if name == "webhook_secret"));
        go_to(&mut app, &mut mock, "maintenance");
        dispatch(&mut app, &mut mock, press(KeyCode::Enter));
        save(&mut app, &mut mock, "maintenance");
        assert_eq!(last_saved(&mock), vec![SettingChange { name: "maintenance".to_owned(), value: SettingInput::Literal(SettingLiteral::Bool(true)) }]);
        go_to(&mut app, &mut mock, "log_level");
        dispatch(&mut app, &mut mock, press(KeyCode::Enter));
        save(&mut app, &mut mock, "log level");
        assert_eq!(last_saved(&mock), vec![SettingChange { name: "log_level".to_owned(), value: text("error") }], "Enter on an empty choice picks the first");
    }
}

/// On a fresh install the required fields with no default are empty. Save refuses, says why,
/// and moves to the first of them, scrolling it into view however far down the owner is; filled
/// in one by one, the form saves.
#[test]
fn required_empty_fields_are_refused_with_the_field_focused() {
    for size in SIZES {
        let mut mock = forms::crowded();
        let mut app = open_form(&mut mock, size);
        let last = forms::form().fields.len() - 1;
        let Screen::Settings(state) = &mut app.screen else { unreachable!() };
        state.selected = last;
        go_to(&mut app, &mut mock, "signup");
        dispatch(&mut app, &mut mock, press(KeyCode::Enter));
        for (key, value) in [("shop_name", "Awkward Forms"), ("daily_limit", "40"), ("theme", "")] {
            mock.calls.clear();
            dispatch(&mut app, &mut mock, ctrl('s'));
            assert!(!mock.calls.iter().any(|c| matches!(c, Request::ApplySettings { .. })), "{size:?}: nothing is sent while {key} is empty");
            let notice = app.notice.as_ref().map(|n| n.text.clone()).unwrap_or_default();
            assert!(notice.contains("Fix the marked field"), "{size:?}: {key}: {notice}");
            assert_eq!(selected(&app), index_of(&app, key), "{size:?}: Save moved to {key}");
            let (buffer, _, _) = drawn(&app);
            let screen = common::text(&buffer);
            let labels: Vec<String> = forms::form().fields.iter().map(|f| f.label.clone()).collect();
            let focused = screen.lines().find(|line| line.trim_start().starts_with("▸ ")).unwrap_or_default();
            assert_eq!(label_on(focused, &labels), Some(index_of(&app, key)), "{size:?}: {key} is on screen, focused:\n{screen}");
            match value {
                "" => {
                    dispatch(&mut app, &mut mock, press(KeyCode::Enter));
                }
                value => retype(&mut app, &mut mock, value),
            }
        }
        save(&mut app, &mut mock, "every required field filled in");
    }
}

/// The focused field of the form on screen.
fn selected(app: &App) -> usize {
    match &app.screen {
        Screen::Settings(state) => state.selected,
        _ => panic!("left the form"),
    }
}

/// Every field's value is on screen under or beside its label, whatever the label's or help's
/// length, and nothing runs past the card into the right margin: at every size and position.
#[test]
fn long_labels_and_help_never_hide_a_value_or_overflow() {
    let form = forms::form();
    let labels: Vec<String> = form.fields.iter().map(|f| f.label.clone()).collect();
    for size in SIZES {
        for selected in 0..labels.len() {
            let app = settings_app(size, selected);
            let (buffer, report, _) = drawn(&app);
            let screen = common::text(&buffer);
            let at = format!("{}x{} at field {selected}", size.0, size.1);
            assert_eq!(report.clipped_lines, 0, "{at}: {report:?}\n{screen}");
            // The margin right of the card is empty on every row of the card.
            for y in (0..size.1).filter(|&y| buffer[(4, y)].bg == nodo_tui::theme::Theme::truecolor().card) {
                for x in size.0 - 4..size.0 {
                    let symbol = buffer[(x, y)].symbol();
                    assert!(symbol.trim().is_empty(), "{at}: row {y} runs into the margin at {x}:\n{screen}");
                }
            }
            // The focused field's value is between its label and the next field.
            let lines: Vec<&str> = screen.lines().collect();
            let start = lines.iter().position(|line| line.trim_start().starts_with("▸ ")).expect("a focused field");
            let end = lines.iter().enumerate().skip(start + 1).find(|(_, line)| label_on(line, &labels).is_some() || line.contains("more — scroll")).map_or(lines.len(), |(row, _)| row);
            let shown = lines[start..end].join("\n");
            let value = match &form.fields[selected].value {
                FieldValue::Text(text) if text.chars().count() > 34 => text.chars().take(8).collect(),
                FieldValue::Text(text) => text.clone(),
                FieldValue::Pieces(pieces) => match pieces.iter().find_map(|p| match p {
                    nodo_tui::model::Piece::Ref { app, setting } => Some(format!("{app}·{setting}")),
                    _ => None,
                }) {
                    Some(chip) => chip,
                    None => String::new(),
                },
                FieldValue::Bool(true) => "[x]".to_owned(),
                FieldValue::Bool(false) => "[ ]".to_owned(),
                FieldValue::SecretUnset => "not set".to_owned(),
                FieldValue::SecretSet | FieldValue::NewSecret(_) => "Change".to_owned(),
            };
            assert!(shown.contains(&value), "{at}: {} shows {value:?}:\n{shown}\n---\n{screen}", labels[selected]);
        }
    }
}

/// A click on a field's label works at the bottom edge of a scrolled form too: it selects the
/// field, opens its editor, and the form keeps the clicked field in view.
#[test]
fn clicking_the_last_visible_label_edits_that_field() {
    let labels: Vec<String> = forms::form().fields.iter().map(|f| f.label.clone()).collect();
    for size in SIZES {
        let mut app = settings_app(size, labels.len() / 2);
        let (buffer, _, hits) = drawn(&app);
        let screen = common::text(&buffer);
        let (row, index) = screen
            .lines()
            .enumerate()
            .filter_map(|(row, line)| label_on(line, &labels).map(|index| (row, index)))
            .last()
            .expect("a label on screen");
        let hit = hits.iter().find(|h| h.rect.y == row as u16 && matches!(h.click, Click::Pick { .. })).expect("the label is a target");
        update(&mut app, Event::Mouse { kind: MouseKind::Down, column: hit.rect.x + 1, row: row as u16 });
        assert_eq!(selected(&app), index, "{size:?}");
        let (buffer, _, _) = drawn(&app);
        let screen = common::text(&buffer);
        let focused = screen.lines().find(|line| line.trim_start().starts_with("▸ ")).unwrap_or_default();
        assert_eq!(label_on(focused, &labels), Some(index), "{size:?}: still in view:\n{screen}");
    }
}
