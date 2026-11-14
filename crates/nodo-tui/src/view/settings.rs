//! An app's settings (Apps › app › Settings), picking a value from another app, and what
//! happens when new settings did not work.

use crossterm::event::KeyCode;
use ratatui::style::Style;
use ratatui::text::Span;

use super::layout::{Item, Page};
use super::pad;
use super::v3::{field_target, inset, quiet, setting_aside, toggle, trail, Card, Screen3};
use crate::app::{App, ApplyFailedState, ReferenceState, SettingsState, Tab};
use crate::hit::{Click, Target};
use crate::model::*;
use crate::secret::STORED_MASK;
use crate::theme::Theme;

/// The link on each field that takes a reference, the same as F4 or Ctrl-R.
const PICK: &str = "+ reference";

/// Long values such as addresses are cut in the form, so the column stays readable.
const VALUE_MAX: usize = 34;

/// A number's box, wide enough for a port or a count of days.
const NUMBER_WIDTH: usize = 6;

/// Text cut to `width` with an ellipsis, so a row never wraps.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() > width {
        let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
        cut.push('…');
        cut
    } else {
        text.to_owned()
    }
}

fn shown(text: &str) -> String {
    if text.chars().count() > VALUE_MAX {
        let start: String = text.chars().take(8).collect();
        let end: String = text.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        format!("{start}…{end}")
    } else {
        text.to_owned()
    }
}

/// How a reference chip reads: `app·setting`.
pub fn chip_label(app: &str, setting: &str) -> String {
    format!("{app}·{setting}")
}

/// A value's text and chips on the field's surface. Chips are accent on a raised fill, or the
/// error colour when they do not resolve; the reason is in the chip's menu, not beside it.
/// `cursor` draws the caret before that unit, while typing.
fn pieces_spans(
    t: &Theme,
    units: &[crate::refs::Unit],
    broken: &[bool],
    cursor: Option<usize>,
    focused: bool,
) -> Vec<Span<'static>> {
    let bg = if focused { t.raised } else { t.field };
    let text_style = Style::new().fg(t.fg).bg(bg);
    let mut spans = vec![Span::styled(" ", text_style)];
    let mut width = 1;
    let mut chip = 0;
    let mut text = String::new();
    let flush = |text: &mut String, spans: &mut Vec<Span<'static>>| {
        if !text.is_empty() {
            spans.push(Span::styled(std::mem::take(text), text_style));
        }
    };
    for (at, unit) in units.iter().enumerate() {
        if cursor == Some(at) {
            text.push('▏');
            width += 1;
        }
        match unit {
            crate::refs::Unit::Char(c) => {
                text.push(*c);
                width += 1;
            }
            crate::refs::Unit::Ref { app, setting } => {
                flush(&mut text, &mut spans);
                let colour = if broken.get(chip).copied().unwrap_or(false) { t.err } else { t.accent };
                let label = chip_label(app, setting);
                width += label.chars().count() + 2;
                spans.push(Span::styled(" ", text_style));
                spans.push(Span::styled(label, Style::new().fg(colour).bg(t.card)));
                spans.push(Span::styled(" ", text_style));
                chip += 1;
            }
        }
    }
    if cursor == Some(units.len()) {
        text.push('▏');
        width += 1;
    }
    flush(&mut text, &mut spans);
    let fill = 31usize.saturating_sub(width).max(1);
    spans.push(Span::styled(" ".repeat(fill), text_style));
    spans
}

fn units_of(pieces: &[Piece]) -> Vec<crate::refs::Unit> {
    crate::refs::PieceEditor::new(pieces).units
}

fn control(t: &Theme, state: &SettingsState, index: usize) -> Vec<Span<'static>> {
    let field = &state.form.fields[index];
    let value = &state.draft[index];
    let broken: Vec<bool> = state.preview(index).map(|p| p.chips.iter().map(Result::is_err).collect()).unwrap_or_default();
    if index == state.selected {
        if let Some(editor) = &state.editor {
            return pieces_spans(t, &editor.units, &broken, Some(editor.cursor), true);
        }
        if let Some(input) = &state.editing {
            return inset(t, &input.display(), 30, None, true);
        }
    }
    match (&field.kind, value) {
        (FieldKind::Toggle, FieldValue::Bool(on)) => toggle(t, *on),
        (FieldKind::Choice(_), FieldValue::Text(text)) => inset(t, text, 28, Some("▾"), false),
        (FieldKind::Number { .. } | FieldKind::Port, FieldValue::Text(text)) => {
            // One width for every number's box, so − and + line up from row to row.
            let mut spans = vec![Span::styled(format!("  {}  ", pad(text, NUMBER_WIDTH)), Style::new().fg(t.fg).bg(t.field))];
            spans.extend([t.n("   "), t.a("−"), t.n("   "), t.a("+")]);
            spans
        }
        (_, FieldValue::Text(text)) => inset(t, &shown(text), 30, None, false),
        (_, FieldValue::Pieces(pieces)) => pieces_spans(t, &units_of(pieces), &broken, None, false),
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
    if index == state.selected && state.editor.as_ref().is_some_and(|e| e.completing().is_some()) {
        return Some(t.d("Typing $( suggests references at the cursor."));
    }
    let field = &state.form.fields[index];
    match &state.draft[index] {
        FieldValue::Pieces(pieces) if pieces.iter().any(|p| matches!(p, Piece::Text(_))) => {
            // Mixed text: the only place the whole value shows, since chips carry no value here.
            let now = state.preview(index).and_then(|p| p.resolved.clone());
            return Some(t.d(match now {
                Some(now) => format!("Text and references together. Now: {now}"),
                None => "Text and references together.".to_owned(),
            }));
        }
        FieldValue::Pieces(_) => return None,
        FieldValue::NewSecret(_) => return Some(t.a("A new secret, not saved yet")),
        _ => {}
    }
    let text = if field.help.is_empty() { field.requirement.clone() } else { field.help.clone() };
    (!text.is_empty()).then(|| t.d(text))
}

pub fn settings(app: &App, t: &Theme, state: &SettingsState) -> Page {
    if let Some(bullets) = &state.confirm {
        return confirm_save(app, t, state, bullets);
    }
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
    let suggestions = crate::update::suggestions(state);
    for (index, field) in state.form.fields.iter().enumerate() {
        if index == state.selected {
            card.focus_here();
        }
        let changed_mark = if state.draft[index] != field.value && index == state.selected && state.editor.is_some() { "  ● changed" } else { "" };
        let label = if index == state.selected { format!("▸ {}{changed_mark}", field.label) } else { format!("  {}", field.label) };
        let picker = field.kind.takes_reference();
        let aside = if picker { vec![t.a(PICK)] } else { Vec::new() };
        setting_aside(&mut card, t, &label, control(t, state, index), aside, hint(t, state, index));
        screen.targets.push(Target::card_row(field.label.clone(), Click::Pick { index, then: Some(KeyCode::Enter) }));
        if picker {
            screen.targets.push(Target::text(PICK, Click::Pick { index, then: Some(KeyCode::F(4)) }));
        }
        if let Some(undo) = state.undo_for(index) {
            let text = format!("↺ {}", undo.label);
            card.link_row(t, vec![t.n("   "), t.a(text.clone())], &text, Click::Key(KeyCode::Char('u')), &mut screen.targets);
            card.blank(t);
        }
        if index == state.selected && state.editor.as_ref().is_some_and(|e| !e.suggestions_closed && e.completing().is_some()) {
            suggestion_rows(&mut card, t, state, &suggestions);
        }
        if index == state.selected {
            if let Some(menu) = &state.chip_menu {
                chip_menu_rows(&mut card, t, state, menu, &mut screen.targets);
            }
        }
    }
    let broken = state.broken();
    let mut targets = Vec::new();
    let buttons = [("Cancel", false, Click::Key(KeyCode::Esc)), ("Save", true, Click::Ctrl('s'))];
    if broken > 0 {
        let plural = if broken == 1 { "reference" } else { "references" };
        card.note_then_buttons(t, vec![t.e(format!("Fix {broken} broken {plural} to save"))], &buttons, &mut targets);
    } else {
        card.buttons(t, &buttons, &mut targets);
    }
    screen.targets.extend(targets);
    let keys = if state.chip_menu.is_some() {
        "Keys: ←→ choose  ↵ do it  r replace  t type  x remove  Esc close"
    } else if state.editor.as_ref().is_some_and(|e| e.completing().is_some() && !e.suggestions_closed) {
        "Keys: ↑↓ choose  ↵ insert  Esc keep typing"
    } else if state.editor.is_some() || state.editing.is_some() {
        "Keys: type a value  $( or Ctrl+R reference  ↵ done  Esc cancel"
    } else {
        "Keys: ↑↓ choose  ↵ edit  Ctrl+R reference  ctrl-s save  z undo  Esc back"
    };
    screen.card(t, card, keys, w, h)
}

/// The suggestions under a field while `$(` is typed, then a way to the full picker.
fn suggestion_rows(card: &mut Card, t: &Theme, state: &SettingsState, suggestions: &[(String, String)]) {
    let selected = state.editor.as_ref().map_or(0, |e| e.suggestion);
    let catalog = state.catalog.as_ref().map(|c| c.options.as_slice()).unwrap_or_default();
    for (at, (app_id, setting)) in suggestions.iter().enumerate() {
        let option = catalog.iter().find(|o| &o.app == app_id && &o.setting == setting);
        let kind = option.map_or("", |o| crate::refs::kind_name(&o.kind));
        let value = option.and_then(|o| o.value.clone()).unwrap_or_default();
        let lead = if at == selected { t.a("▸ ") } else { t.n("  ") };
        card.row(t, vec![t.n(" ".repeat(10)), lead, t.n(pad(&format!("{app_id}.{setting}"), 28)), t.d(pad(kind, 9)), t.d(shown(&value))]);
    }
    let lead = if selected >= suggestions.len() { t.a("▸ ") } else { t.n("  ") };
    card.row(t, vec![t.n(" ".repeat(10)), lead, t.a("All references…")]);
    card.blank(t);
}

/// What one reference is, and what can be done with it, opened by Enter or a click on the field.
fn chip_menu_rows(card: &mut Card, t: &Theme, state: &SettingsState, menu: &crate::app::ChipMenu, targets: &mut Vec<Target>) {
    let index = state.selected;
    let pieces = state.draft[index].pieces().unwrap_or_default();
    let Some((app_id, setting)) = crate::model::refs(&pieces).get(menu.chip).map(|(a, s)| (a.to_string(), s.to_string())) else {
        return;
    };
    let status = state.preview(index).and_then(|p| p.chips.get(menu.chip).cloned());
    let mut first = vec![t.n("   "), t.a(chip_label(&app_id, &setting))];
    match status {
        Some(Err(why)) => first.push(t.e(format!("   ✕ {why}"))),
        Some(Ok(Some(value))) => first.push(t.d(format!("  →  {value}"))),
        Some(Ok(None)) => first.push(t.d("  →  a secret, never shown")),
        None => first.push(t.d("  →  checking…")),
    }
    card.row(t, first);
    // The actions in a fixed order, the highlighted one drawn as the primary button.
    let mut spans = Vec::new();
    for (at, (label, key)) in crate::app::CHIP_ACTIONS.iter().zip(['r', 't', 'x']).enumerate() {
        if at > 0 {
            spans.push(t.n("   "));
        }
        let width = label.chars().count() + 6;
        spans.extend(super::v3::button_target(t, label, at == menu.choice, width, t.card, &Click::Key(KeyCode::Char(key)), targets));
    }
    card.row_end(t, vec![t.n("   ")], spans);
    card.blank(t);
}

/// Save, after saying in plain words what the changed references allow.
fn confirm_save(app: &App, t: &Theme, state: &SettingsState, bullets: &[String]) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let package = state.form.package.as_str();
    let mut before_targets = Vec::new();
    screen.before = vec![Item::Blank, trail(t, &["Apps", package, "Settings"], &mut before_targets), Item::Blank];
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    card.row(t, vec![t.b(format!("Save {package}'s settings?"))]);
    card.blank(t);
    card.row(t, vec![t.n(format!("These references change what {package} can reach:"))]);
    for bullet in bullets {
        card.row(t, vec![t.n("•  "), t.n(bullet.clone())]);
    }
    card.blank(t);
    let mut targets = Vec::new();
    card.buttons(t, &[("Cancel", false, Click::Key(KeyCode::Esc)), ("Save", true, Click::Key(KeyCode::Enter))], &mut targets);
    screen.targets.extend(targets);
    screen.card(t, card, "Keys: ↵ save  Esc back to the form", w, h)
}

/// The one reference picker: search what every other app offers, for any field.
pub fn reference(app: &App, t: &Theme, state: &ReferenceState) -> Page {
    if let Some(option) = &state.granting {
        return grant(app, t, state, option);
    }
    let (w, h) = super::frame_size();
    let narrow = w < 100;
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let mut before_targets = Vec::new();
    screen.before = vec![
        Item::Blank,
        trail(t, &["Apps", state.package.as_str(), "Settings", "Insert a reference"], &mut before_targets),
        Item::Blank,
    ];
    screen.targets.extend(before_targets);
    let visible = state.visible();
    let mut card = Card::new(t, w);
    card.blank(t);
    let kind = crate::refs::kind_name(&state.field_kind);
    card.row(t, vec![t.b("Insert a reference   "), t.d(format!("into {} · {} ({kind})", state.package, state.field_label))]);
    card.blank(t);
    let mut search = vec![t.d(pad("Search", 9))];
    let placeholder = if state.query.text().is_empty() && state.list_focused { "apps, settings, descriptions or values" } else { "" };
    search.extend(inset(t, if placeholder.is_empty() { state.query.text() } else { placeholder }, 38, None, !state.list_focused));
    card.row(t, search);
    screen.targets.push(field_target("Search", 9, 38));
    card.blank(t);
    if visible.iter().any(|(_, verdict)| verdict.is_err()) && visible.iter().any(|(_, v)| v.is_ok()) {
        let words = match state.field_kind {
            FieldKind::Number { .. } | FieldKind::Port => "Numbers first; the others are dimmed because a number field can't hold them.",
            _ => "Those that fit first; the others are dimmed with why.",
        };
        card.row(t, vec![t.d(words)]);
        card.blank(t);
    }
    let path_width = visible.iter().map(|(o, _)| o.path().chars().count()).max().unwrap_or(0).clamp(16, 30) + 2;
    // What is left after the name, type and "public" columns goes to the description and value.
    let rest = card.inner().saturating_sub(2 + path_width + 9 + 8);
    let value_width = if narrow { card.inner().saturating_sub(2 + path_width + 9).max(8) } else { (rest / 2).clamp(12, 26) };
    let about_width = rest.saturating_sub(value_width + 2).max(8);
    let mut heading: Option<String> = None;
    for (index, (option, verdict)) in visible.iter().enumerate() {
        let locked = option.locked.is_some_and(|l| !l.granted);
        let group = if locked {
            "SECRETS · NOT PUBLIC".to_owned()
        } else if verdict.is_err() {
            String::new()
        } else {
            option.app.to_uppercase()
        };
        if heading.as_deref() != Some(group.as_str()) {
            if heading.is_some() && !group.is_empty() {
                card.blank(t);
            }
            if !group.is_empty() {
                card.row(t, vec![t.d(group.clone())]);
            }
            heading = Some(group);
        }
        let selected = state.list_focused && index == state.selected;
        if selected {
            card.focus_here();
        }
        let lead = if selected { t.a("▸ ") } else { t.n("  ") };
        let dim = verdict.is_err() && !locked;
        let name = if dim { t.d(pad(&option.path(), path_width)) } else { t.n(pad(&option.path(), path_width)) };
        let badge = t.d(pad(crate::refs::kind_name(&option.kind), 9));
        let public = t.d(pad(if option.locked.is_none() { "public" } else { "" }, 8));
        let about = match verdict {
            Err(why) if !locked => why.clone(),
            _ if locked && option.locked.is_some_and(|l| l.secret) => "never shown".to_owned(),
            _ if !option.help.is_empty() => option.help.clone(),
            _ => option.label.clone(),
        };
        let end = if locked {
            let words = "Needs your approval · Grant…";
            if narrow || words.chars().count() > value_width { t.a("Grant…") } else { t.a(words) }
        } else {
            t.d(clip(option.value.as_deref().unwrap_or_default(), value_width))
        };
        let row = if narrow {
            vec![lead, name, badge, end]
        } else {
            vec![lead, name, badge, public, t.d(pad(&clip(&about, about_width), about_width + 2)), end]
        };
        card.row(t, row);
        if narrow {
            card.row(t, vec![t.n("    "), t.d(clip(&about, card.inner().saturating_sub(4)))]);
        }
        screen.targets.push(Target::card_row(option.path(), Click::Pick { index, then: Some(KeyCode::Enter) }));
    }
    if visible.is_empty() {
        card.row(t, vec![t.d(if state.list.options.is_empty() { "No other app offers a setting yet." } else { "Nothing matches that search." })]);
    }
    card.blank(t);
    let keys = if state.list_focused { "Keys: ↑↓ choose  ↵ insert  / search  Esc close" } else { "Keys: type to search  ↓ list  ↵ insert  Esc close" };
    screen.card(t, card, keys, w, h)
}

/// Approving one app reading another's secret or non-public setting.
fn grant(app: &App, t: &Theme, state: &ReferenceState, option: &ReferenceOption) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let mut before_targets = Vec::new();
    screen.before = vec![
        Item::Blank,
        trail(t, &["Apps", state.package.as_str(), "Settings", "Grant"], &mut before_targets),
        Item::Blank,
    ];
    screen.targets.extend(before_targets);
    let secret = option.locked.is_some_and(|l| l.secret);
    let mut card = Card::new(t, w);
    card.blank(t);
    card.row(t, vec![t.e("▲  "), t.b(format!("Let {} use {}'s {}?", state.package, option.app, option.label.to_lowercase()))]);
    card.blank(t);
    if secret {
        card.row(t, vec![t.n(format!("{} is a secret: it is never shown, and only apps you approve can use it.", option.setting))]);
    } else {
        card.row(t, vec![t.n(format!("{} isn't public: only apps you approve can use it.", option.setting))]);
    }
    card.row(t, vec![t.n(format!("{} receives it when it starts. You can take this back in its settings.", state.package))]);
    card.blank(t);
    let mut targets = Vec::new();
    card.buttons(t, &[("Cancel", false, Click::Key(KeyCode::Esc)), ("Grant", true, Click::Key(KeyCode::Enter))], &mut targets);
    screen.targets.extend(targets);
    screen.card(t, card, "Keys: ↵ grant  Esc cancel", w, h)
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
