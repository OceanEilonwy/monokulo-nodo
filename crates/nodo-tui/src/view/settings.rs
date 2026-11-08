//! The settings form, the reference picker and the rollback report.

use ratatui::text::{Line, Span};

use super::layout::{Item, Page, Panel};
use super::{checkbox, hints, mark, pad};
use crate::app::{ApplyFailedState, ReferenceState, SettingsState};
use crate::model::*;
use crate::secret::STORED_MASK;
use crate::theme::Theme;

const LABEL_WIDTH: usize = 18;
/// Long values such as addresses are cut in the form, so the column stays readable.
const VALUE_PREVIEW: usize = 14;
const VALUE_MAX: usize = 36;

fn value_spans(t: &Theme, field: &Field, value: &FieldValue) -> Vec<Span<'static>> {
    match value {
        FieldValue::Text(text) => {
            if matches!(field.kind, FieldKind::Choice(_)) {
                return vec![t.n(format!("‹ {text} ›"))];
            }
            if text.chars().count() > VALUE_MAX {
                let cut: String = text.chars().take(VALUE_PREVIEW).collect();
                return vec![t.n(format!("{cut}…"))];
            }
            vec![t.n(text.clone())]
        }
        FieldValue::Reference { path, .. } => vec![t.i(format!("$({path})"))],
        FieldValue::Bool(on) => vec![t.n(format!("{} {}", checkbox(*on), if *on { "on" } else { "off" }))],
        FieldValue::SecretUnset => vec![t.d("not set")],
        // A new secret's value is not read here at all, only the fact that there is one.
        FieldValue::SecretSet | FieldValue::NewSecret(_) => vec![t.n(STORED_MASK)],
    }
}

pub fn settings(t: &Theme, state: &SettingsState) -> Page {
    // Labels come from each package's schema, so the column fits the longest one with a gap.
    let label_width = state
        .form
        .fields
        .iter()
        .map(|field| unicode_width::UnicodeWidthStr::width(field.label.as_str()) + 2)
        .fold(LABEL_WIDTH, usize::max);
    let mut items = Vec::new();
    for (index, field) in state.form.fields.iter().enumerate() {
        let selected = index == state.selected;
        let label = pad(&field.label, label_width);
        let label_span = if selected { t.b(mark(true, &format!(" {label}"))) } else { t.d(format!(" {label}")) };
        let mut spans = vec![label_span];
        match (selected, &state.editing) {
            (true, Some(input)) => {
                spans.push(t.n(input.display()));
                spans.push(t.s(" "));
            }
            _ => spans.extend(value_spans(t, field, &state.draft[index])),
        }
        items.push(Item::Text(Line::from(spans)));
        let indent = " ".repeat(label_width + 1);
        if let FieldValue::Reference { resolved, .. } = &state.draft[index] {
            items.push(Item::Text(Line::from(t.d(format!("{indent}Resolves to {resolved}")))));
        }
        match &state.draft[index] {
            FieldValue::SecretSet => {
                items.push(Item::Text(Line::from(t.d(format!("{indent}secret, private to this package")))));
            }
            FieldValue::NewSecret(_) => {
                items.push(Item::Text(Line::from(t.a(format!("{indent}new secret, not saved yet")))));
            }
            _ => {}
        }
        if let Some(error) = state.error_for(index) {
            items.push(Item::Text(Line::from(t.e(format!("{indent}✕ {error}")))));
        }
        if index + 1 < state.form.fields.len() {
            items.push(Item::Blank);
        }
    }

    let focused = &state.form.fields[state.selected];
    let mut help = vec![
        Item::Text(Line::from(t.n(focused.help.clone()))),
        Item::Text(Line::from(t.d(focused.requirement.clone()))),
        Item::Blank,
        Item::Text(Line::from(t.b("Saving will"))),
        Item::Text(Line::from(t.d(format!("Restart {}", state.form.package)))),
        Item::Text(Line::from(t.d("Run its health check"))),
        Item::Text(Line::from(t.d("Restore these values if it fails"))),
    ];
    if !state.form.public_settings.is_empty() {
        help.push(Item::Blank);
        help.push(Item::Text(Line::from(t.b("Others can read"))));
        help.extend(state.form.public_settings.iter().map(|path| Item::Text(Line::from(t.d(path.clone())))));
    }

    let changed = state.changed_indices().len();
    let right = match changed {
        0 => t.d("No unsaved changes"),
        1 => t.a("● 1 unsaved change"),
        n => t.a(format!("● {n} unsaved changes")),
    };
    let keys: &[(&str, &str)] = if state.editing.is_some() {
        &[("type", "a value"), ("↵", "done"), ("Esc", "cancel")]
    } else {
        &[("↑↓", "field"), ("↵", "edit"), ("$", "use a reference"), ("ctrl-s", "save"), ("z", "undo"), ("Esc", "back")]
    };
    Page::new(
        Line::from(vec![t.n(format!("{} ", state.form.package)), t.d("› "), t.b("Settings")]),
        Line::from(right),
    )
    .columns(vec![
        (60, vec![Panel::new("Connection", items).grow()]),
        (40, vec![Panel::new(focused.label.clone(), help).grow()]),
    ])
    .footer(Item::Text(hints(t, keys)))
}

pub fn reference(t: &Theme, state: &ReferenceState) -> Page {
    let visible = state.visible();
    let mut query = vec![t.a("$"), t.n(format!(" {}", state.query.display()))];
    if !state.list_focused {
        query.push(t.s(" "));
    }
    let mut rows = Vec::new();
    for (index, option) in visible.iter().enumerate() {
        let selected = index == state.selected;
        let left = Line::from(t.n(mark(selected, &format!(" {}", option.path))));
        if selected {
            rows.push(Item::Selected(left, Line::from(t.n(option.value.clone()))));
        } else {
            rows.push(Item::Row(left, Line::from(t.d(option.value.clone()))));
        }
    }
    if visible.is_empty() {
        rows.push(Item::Text(Line::from(t.d(" No compatible settings match."))));
    }
    rows.push(Item::Blank);
    rows.push(Item::Text(Line::from(t.d(format!(
        " Not shown: {} settings of the wrong type. {} secrets, which need a grant.",
        state.list.hidden_wrong_type, state.list.hidden_secrets
    )))));
    let chosen = visible.get(state.selected);
    if let Some((first, second)) = chosen.and_then(|option| option.role_note.as_ref()) {
        rows.push(Item::Blank);
        rows.push(Item::Text(Line::from(t.i(format!(" {first}")))));
        rows.push(Item::Text(Line::from(t.d(format!(" {second}")))));
    }
    let consequences = chosen.map_or_else(Vec::new, |option| {
        option
            .consequences
            .iter()
            .map(|line| Item::Text(Line::from(vec![t.i("●"), t.n(format!(" {line}"))])))
            .collect()
    });
    let keys = if state.list_focused {
        hints(t, &[("↑↓", "move"), ("↵", "use reference"), ("t", "type a value instead"), ("Esc", "cancel")])
    } else {
        hints(t, &[("type", "to search"), ("↓", "list, then t types a value"), ("↵", "use reference"), ("Esc", "cancel")])
    };
    Page::new(
        Line::from(vec![
            t.n(format!("{} ", state.package)),
            t.d("› "),
            t.n("Settings "),
            t.d("› "),
            t.b(state.field_label.clone()),
        ]),
        Line::from(t.d(format!("Expects {}", state.list.expects))),
    )
    .panel(Panel::new("Search public settings", vec![Item::Text(Line::from(query))]))
    .panel(Panel::new("Compatible settings", rows).grow())
    .panel(Panel::new("Using this reference will", consequences))
    .footer(Item::Text(keys))
}

pub fn apply_failed(t: &Theme, state: &ApplyFailedState) -> Page {
    let explanation = &state.explanation;
    let steps = explanation
        .steps
        .iter()
        .map(|(passed, text)| {
            let symbol = if *passed { t.o("✓") } else { t.e("✕") };
            Item::Text(Line::from(vec![symbol, t.n(format!(" {text}"))]))
        })
        .collect();
    let rows = state
        .rows
        .iter()
        .map(|row| {
            let to = if row.blamed { t.e(row.to.clone()) } else { t.n(row.to.clone()) };
            Item::Row(
                Line::from(t.d(row.label.clone())),
                Line::from(vec![t.n(format!("{} ", row.from)), t.d("→ "), to]),
            )
        })
        .collect();
    Page::new(
        Line::from(vec![t.n(format!("{} ", state.package)), t.d("› "), t.b("Settings")]),
        Line::from(t.o("● Running on previous settings")),
    )
    .panel(Panel::error(
        "Your changes were not kept",
        vec![Item::Text(Line::from(t.n(format!(
            "{} failed its health check with the new settings, so its previous settings were restored.",
            state.package
        ))))],
    ))
    .panel(Panel::new("What happened", steps))
    .panel(Panel::new(
        "Likely cause",
        vec![
            Item::Text(Line::from(t.e(explanation.headline.clone()))),
            Item::Text(Line::from(t.d(explanation.hint.clone()))),
        ],
    ))
    .panel(Panel::new("Your draft, not applied", rows).grow())
    .footer(Item::Text(hints(t, &[("↵", "edit draft"), ("l", "full log"), ("x", "discard draft"), ("Esc", "back")])))
}
