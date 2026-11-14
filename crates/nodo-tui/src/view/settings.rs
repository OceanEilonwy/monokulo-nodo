//! An app's settings (Apps › app › Settings), picking a value from another app, and what
//! happens when new settings did not work.

use crossterm::event::KeyCode;
use ratatui::style::Style;
use ratatui::text::Span;

use super::layout::{Item, Page};
use super::pad;
use super::v3::{field_target, heading, inset, quiet, setting, toggle, trail, Card, Screen3};
use crate::app::{App, ApplyFailedState, ReferenceState, SettingsState, Tab};
use crate::hit::{Click, Target};
use crate::model::*;
use crate::secret::STORED_MASK;
use crate::theme::Theme;

/// Long values such as addresses are cut in the form, so the column stays readable.
const VALUE_MAX: usize = 34;

fn shown(text: &str) -> String {
    if text.chars().count() > VALUE_MAX {
        let start: String = text.chars().take(8).collect();
        let end: String = text.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        format!("{start}…{end}")
    } else {
        text.to_owned()
    }
}

fn control(t: &Theme, field: &Field, value: &FieldValue, editing: Option<&str>) -> Vec<Span<'static>> {
    if let Some(typed) = editing {
        return inset(t, typed, 30, None, true);
    }
    match (&field.kind, value) {
        (FieldKind::Toggle, FieldValue::Bool(on)) => toggle(t, *on),
        (FieldKind::Choice(_), FieldValue::Text(text)) => inset(t, text, 28, Some("▾"), false),
        (FieldKind::Number { .. } | FieldKind::Port, FieldValue::Text(text)) => {
            let mut spans = vec![Span::styled(format!("  {text}  "), Style::new().fg(t.fg).bg(t.field))];
            spans.extend([t.n("   "), t.a("−"), t.n("   "), t.a("+")]);
            spans
        }
        (_, FieldValue::Text(text)) => inset(t, &shown(text), 30, None, false),
        (_, FieldValue::Reference { path, resolved }) => {
            let label = format!("{} · {}", path.split('.').next().unwrap_or(path), shown(resolved));
            inset(t, &label, 36, Some("▾"), false)
        }
        (_, FieldValue::Bool(on)) => toggle(t, *on),
        (_, FieldValue::SecretUnset) => {
            let mut spans = inset(t, "", 30, None, false);
            spans.push(t.d("   not set"));
            spans
        }
        (_, FieldValue::SecretSet | FieldValue::NewSecret(_)) => {
            let mut spans = vec![Span::styled(format!(" {} ", pad(STORED_MASK, 30)), Style::new().fg(t.fg).bg(t.field))];
            spans.push(t.a("   Change"));
            spans
        }
    }
}

fn hint(t: &Theme, state: &SettingsState, index: usize) -> Option<Span<'static>> {
    if let Some(error) = state.error_for(index) {
        return Some(t.e(format!("✕ {error}")));
    }
    let field = &state.form.fields[index];
    match &state.draft[index] {
        FieldValue::Reference { .. } => {
            return Some(t.d(format!("Picked from your apps, so it follows {} if it changes.", field.label.to_lowercase())));
        }
        FieldValue::NewSecret(_) => return Some(t.a("A new secret, not saved yet")),
        _ => {}
    }
    let text = if field.help.is_empty() { field.requirement.clone() } else { field.help.clone() };
    (!text.is_empty()).then(|| t.d(text))
}

pub fn settings(app: &App, t: &Theme, state: &SettingsState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let package = state.form.package.as_str();
    let mut before_targets = Vec::new();
    let changed = state.changed_indices().len();
    let note = match changed {
        0 => t.d("No changes yet"),
        1 => t.a("1 change, not saved yet"),
        n => t.a(format!("{n} changes, not saved yet")),
    };
    screen.before = vec![
        Item::Blank,
        trail(t, &["Apps", package, "Settings"], &mut before_targets),
        Item::Blank,
        Item::Row(ratatui::text::Line::from(vec![super::v3::margin(t), t.b("Settings")]), ratatui::text::Line::from(vec![note, t.n(super::v3::MARGIN)])),
        quiet(t, &format!("{package} restarts with the new settings. If it doesn't start, they're undone by themselves.")),
        Item::Blank,
        Item::Blank,
    ];
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    // What the box shows while typing: an input's display, which masks a secret as it is typed.
    let editing = state.editing.as_ref().map(crate::secret::Input::display);
    for (index, field) in state.form.fields.iter().enumerate() {
        if index == state.selected {
            card.focus_here();
        }
        let typing = (index == state.selected).then_some(editing.as_deref()).flatten();
        let label = if index == state.selected { format!("▸ {}", field.label) } else { format!("  {}", field.label) };
        setting(&mut card, t, &label, control(t, field, &state.draft[index], typing), hint(t, state, index));
        screen.targets.push(Target::card_row(field.label.clone(), Click::Pick { index, then: Some(KeyCode::Enter) }));
    }
    let mut targets = Vec::new();
    card.buttons(t, &[("Cancel", false, Click::Key(KeyCode::Esc)), ("Save", true, Click::Ctrl('s'))], &mut targets);
    screen.targets.extend(targets);
    let keys = if state.editing.is_some() { "Keys: type a value  ↵ done  Esc cancel" } else { "Keys: ↑↓ choose  ↵ edit  $ pick from an app  ctrl-s save  z undo  Esc back" };
    screen.card(t, card, keys, w, h)
}

/// Picking a value from another app, such as the Monero node's address.
pub fn reference(app: &App, t: &Theme, state: &ReferenceState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let mut before_targets = Vec::new();
    screen.before = vec![
        Item::Blank,
        trail(t, &["Apps", state.package.as_str(), "Settings", state.field_label.as_str()], &mut before_targets),
        Item::Blank,
        heading(t, &format!("Pick {} from your apps", state.field_label.to_lowercase())),
        quiet(t, &format!("It follows the app you pick if its value changes. Expects {}.", state.list.expects)),
        Item::Blank,
    ];
    screen.targets.extend(before_targets);
    let visible = state.visible();
    let mut card = Card::new(t, w);
    card.blank(t);
    let mut search = vec![t.d(pad("Search", 9))];
    search.extend(inset(t, state.query.text(), 30, None, !state.list_focused));
    card.row(t, search);
    screen.targets.push(field_target("Search", 9, 30));
    card.blank(t);
    let path_width = visible.iter().map(|o| o.path.chars().count()).max().unwrap_or(0).clamp(16, 40) + 2;
    for (index, option) in visible.iter().enumerate() {
        let selected = state.list_focused && index == state.selected;
        if selected {
            card.focus_here();
        }
        let lead = if selected { t.a("✓ ") } else { t.n("  ") };
        card.row(t, vec![lead, t.n(pad(&option.path, path_width)), t.d(shown(&option.value))]);
        screen.targets.push(Target::card_row(option.path.clone(), Click::Pick { index, then: Some(KeyCode::Enter) }));
    }
    if visible.is_empty() {
        card.row(t, vec![t.d("No app offers a matching value.")]);
    }
    card.blank(t);
    if let Some(option) = visible.get(state.selected) {
        if let Some((first, second)) = &option.role_note {
            card.row(t, vec![t.i(first.clone())]);
            card.row(t, vec![t.d(second.clone())]);
            card.blank(t);
        }
    }
    let mut targets = Vec::new();
    card.buttons(t, &[("Type a value instead", false, Click::Key(KeyCode::Char('t'))), ("Use it", true, Click::Key(KeyCode::Enter))], &mut targets);
    screen.targets.extend(targets);
    let keys = if state.list_focused { "Keys: ↑↓ choose  ↵ use it  t type a value  Esc close" } else { "Type to search.   Keys: ↓ list  ↵ use it  Esc close" };
    screen.card(t, card, keys, w, h)
}

/// New settings did not work, so the previous ones came back.
pub fn apply_failed(app: &App, t: &Theme, state: &ApplyFailedState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let package = state.package.as_str();
    let mut before_targets = Vec::new();
    screen.before = vec![Item::Blank, trail(t, &["Apps", package, "Settings"], &mut before_targets), Item::Blank];
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    card.row(t, vec![t.e("▲  "), t.b(format!("{package} didn't start with the new settings, so they were undone"))]);
    card.blank(t);
    card.row(t, vec![t.n("It's running again with the settings it had before. What you changed:")]);
    for row in &state.rows {
        let to = if row.blamed { t.e(row.to.clone()) } else { t.n(row.to.clone()) };
        card.row(t, vec![t.d(pad(&format!("  {}", row.label), 26)), t.n(format!("{}  →  ", row.from)), to]);
    }
    card.blank(t);
    if !state.explanation.headline.is_empty() {
        card.row(t, vec![t.d(format!("{package} said: \"{}\"", state.explanation.headline))]);
    }
    if !state.explanation.hint.is_empty() {
        card.row(t, vec![t.d(state.explanation.hint.clone())]);
    }
    card.blank(t);
    let mut targets = Vec::new();
    card.link_row(t, vec![t.a("Show log ›")], "Show log ›", Click::Key(KeyCode::Char('l')), &mut targets);
    card.blank(t);
    card.buttons(t, &[("Keep as it is", false, Click::Key(KeyCode::Char('x'))), ("Edit again", true, Click::Key(KeyCode::Enter))], &mut targets);
    screen.targets.extend(targets);
    screen.card(t, card, "Keys: ↵ edit again  l log  x keep as it is  Esc back", w, h)
}
