//! Home and the data drive failure that replaces it.

use nodo_api::ServiceState;
use ratatui::text::Line;

use super::layout::{Item, Page, Panel};
use super::{hints, keyed, mark, pad, service_symbol, tab_bar};
use crate::app::{App, Tab};
use crate::model::CheckState;
use crate::theme::Theme;

fn pair(t: &Theme, label: &str, value: &str) -> Item {
    Item::Row(Line::from(t.d(label.to_owned())), Line::from(t.n(value.to_owned())))
}

pub fn home(app: &App, t: &Theme, selected: usize) -> Page {
    let overview = &app.data.overview;
    let node = &overview.node;
    let mut state = pair(t, "State", "");
    if let Item::Row(_, right) = &mut state {
        *right = Line::from(t.i(format!("▲ {}", node.state)));
    }
    let node_panel = Panel::new(
        "Node",
        vec![
            pair(t, "Role", &node.role),
            pair(t, "Provider", &node.provider),
            state,
            pair(t, "Height", &node.height),
            pair(t, "Peers", &node.peers),
        ],
    );
    let device_panel =
        Panel::new("Device", overview.device.iter().map(|(label, value)| pair(t, label, value)).collect());

    let mut rows = Vec::new();
    for (index, service) in app.data.services.iter().enumerate() {
        let origin = overview
            .service_origins
            .iter()
            .find(|(id, _)| id == &service.id)
            .map_or("", |(_, origin)| origin.as_str());
        let (symbol, state_label) = service_symbol(t, service.state);
        let is_selected = index == selected;
        let lead = mark(is_selected, " ");
        let mut note = service.detail.clone();
        if service.state == ServiceState::Failed {
            note = format!("{note}, press l for logs");
        }
        let state_width = pad(&state_label.content, 10);
        let state_span = ratatui::text::Span::styled(state_width, state_label.style);
        let mut spans = vec![
            t.n(lead),
            symbol,
            t.n(format!(" {}", pad(&service.id, 17))),
            state_span,
            t.n(pad(origin, 15)),
            t.d(note),
        ];
        if is_selected {
            spans.push(t.n(" "));
            rows.push(Item::Selected(Line::from(spans), Line::from("Enter to open")));
        } else {
            rows.push(Item::Row(Line::from(spans), Line::default()));
        }
    }

    let mut attention = Vec::new();
    if app.is_locked() {
        attention.push(Item::Text(prefixed(t, true, "This device is locked. Press `u` to unlock all waiting services.")));
    }
    for service in app.data.services.iter().filter(|svc| svc.state == ServiceState::Failed) {
        attention.push(Item::Text(prefixed(
            t,
            false,
            &format!("{} keeps failing. Press `l` to read its log.", service.id),
        )));
    }

    let mut page = Page::new(tab_bar(app, t, Tab::Home), Line::from(t.d("monokulo-nodo")))
        .columns(vec![(3, vec![node_panel]), (2, vec![device_panel])])
        .panel(Panel::new("Services", rows).grow());
    if !attention.is_empty() {
        page = page.panel(Panel::accent("Needs attention", attention));
    }
    page.footer(Item::Row(
        hints(t, &[("↑↓", "move"), ("↵", "open"), ("s", "start/stop"), ("/", "search"), ("Tab", "next tab"), ("?", "help")]),
        hints(t, &[("q", "quit")]),
    ))
}

fn prefixed(t: &Theme, locked: bool, text: &str) -> Line<'static> {
    let symbol = if locked { t.a("◆") } else { t.e("✕") };
    let mut line = keyed(t, text);
    line.spans.insert(0, t.n(" "));
    line.spans.insert(0, symbol);
    line
}

const DRIVE_CHOICES: [(&str, &str); 4] = [
    ("Retry mount", ""),
    ("Run a read-only drive check", "changes nothing"),
    ("Repair the filesystem", "can lose data, explains first and asks to confirm"),
    ("Show diagnostics", ""),
];

pub fn no_drive(app: &App, t: &Theme, choice: usize) -> Page {
    let Some(failure) = &app.data.overview.drive else {
        return Page::new(tab_bar(app, t, Tab::Home), Line::default());
    };
    let checks = failure
        .checks
        .iter()
        .map(|check| {
            let (symbol, note) = match check.state {
                CheckState::Passed => (t.o("✓"), t.d(check.note.clone())),
                CheckState::Failed => (t.e("✕"), t.e(check.note.clone())),
            };
            Item::Row(Line::from(vec![symbol, t.n(format!(" {}", check.text))]), Line::from(note))
        })
        .collect();
    let mut choices: Vec<Item> = DRIVE_CHOICES
        .iter()
        .enumerate()
        .map(|(index, (label, note))| {
            let text = mark(index == choice, &format!(" {label}"));
            if index == choice {
                Item::Selected(Line::from(vec![t.n(text), t.n(if note.is_empty() { "" } else { "  " }), t.d(*note)]), Line::default())
            } else {
                Item::Text(Line::from(vec![t.n(text), t.n("  "), t.d(*note)]))
            }
        })
        .collect();
    choices.push(Item::Text(Line::from(t.d("The TUI never formats a drive without you typing its label."))));
    Page::new(tab_bar(app, t, Tab::Home), Line::from(t.e("✕ Node stopped")))
        .panel(Panel::error(
            "The data drive is not available",
            vec![Item::Text(Line::from(t.n(failure.message.clone())))],
        ))
        .panel(Panel::new("Checks", checks))
        .panel(Panel::new("Also waiting", vec![Item::Text(Line::from(t.d(failure.waiting.clone())))]))
        .panel(Panel::new("What you can do", choices).grow())
        .footer(Item::Text(hints(t, &[("↑↓", "choose"), ("↵", "confirm"), ("l", "system log"), ("!", "shell")])))
}
