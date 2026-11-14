//! Home: what needs you, with one button each, then that everything else is running. The data
//! drive being missing replaces it.

use crossterm::event::KeyCode;
use nodo_api::ServiceState;

use super::layout::{Item, Page};
use super::pad;
use super::v3::{heading, Card, Screen3};
use crate::app::{App, HomeItem, Tab};
use crate::hit::{Click, Target};
use crate::model::AttentionLevel;
use crate::theme::Theme;

/// An app's state in plain words, and whether it is a problem.
pub(crate) fn app_state(app: &App, id: &str) -> (String, bool) {
    let Some(svc) = app.data.services.iter().find(|svc| svc.id == id) else {
        return ("Not running".to_owned(), false);
    };
    let base = match svc.state {
        ServiceState::Running if svc.detail.is_empty() => "Running".to_owned(),
        ServiceState::Running => format!("Running · {}", svc.detail),
        ServiceState::Stopped => "Stopped".to_owned(),
        ServiceState::Failed => "Keeps stopping".to_owned(),
        ServiceState::Locked => "Locked until you unlock".to_owned(),
        ServiceState::Waiting => "Waiting for another app".to_owned(),
    };
    (base, svc.state == ServiceState::Failed)
}

/// What an app is for, from its source's description.
pub(crate) fn tagline(app: &App, id: &str) -> String {
    app.data.catalog.iter().find(|pkg| pkg.id.name == id).map(|pkg| pkg.tagline.clone()).unwrap_or_default()
}

/// The device's health in one line: storage, temperature, uptime.
fn health_line(app: &App) -> String {
    app.data
        .overview
        .device
        .iter()
        .take(3)
        .map(|(label, value)| format!("{label} {value}"))
        .collect::<Vec<_>>()
        .join("   ·   ")
}

pub fn home(app: &App, t: &Theme, selected: usize) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Home, w);
    let items = app.home_items();
    let needs: Vec<&HomeItem> = items.iter().filter(|item| !matches!(item, HomeItem::App(_))).collect();
    let notes = &app.data.overview.attention;
    let title = if needs.is_empty() && notes.is_empty() { "Everything is running" } else { "Your Nodo" };
    screen.before = vec![Item::Blank, heading(t, title), Item::Blank];

    let mut cards = Vec::new();
    for (index, item) in needs.iter().enumerate() {
        let mut card = Card::new(t, w);
        card.blank(t);
        let (head, body, label) = match item {
            HomeItem::Unlock => (
                "Your Nodo is locked".to_owned(),
                "Apps' data waits for the device passphrase. They start once you unlock.".to_owned(),
                "Unlock",
            ),
            HomeItem::Failing(id) => (
                format!("{id} keeps stopping"),
                "Its log usually says why. Settings or an update can be undone from its page.".to_owned(),
                "Show log",
            ),
            HomeItem::Updates => {
                let names: Vec<String> = app
                    .update_rows()
                    .iter()
                    .filter(|u| !u.held)
                    .map(|u| if u.name == crate::model::SYSTEM_BASE { "the Nodo".to_owned() } else { u.name.clone() })
                    .collect();
                let count = names.len();
                (
                    if count == 1 { "1 update is ready".to_owned() } else { format!("{count} updates are ready") },
                    format!("{}. Each keeps its settings and can be undone.", names.join(", ")),
                    "Review updates",
                )
            }
            HomeItem::App(_) => unreachable!("filtered out"),
        };
        if index == selected {
            card.focus_here();
        }
        let lead = if matches!(item, HomeItem::Failing(_)) { t.e("▲  ") } else { t.n("") };
        let mark = if index == selected { t.a("▸ ") } else { t.n("") };
        card.row(t, vec![mark, lead, t.b(head)]);
        card.row(t, vec![t.d(body)]);
        card.blank(t);
        let mut targets = Vec::new();
        card.buttons(t, &[(label, true, Click::Pick { index, then: Some(KeyCode::Enter) })], &mut targets);
        screen.targets.extend(targets);
        cards.push(card);
    }
    if !notes.is_empty() {
        let mut card = Card::new(t, w);
        card.blank(t);
        for note in notes {
            let symbol = match note.level {
                AttentionLevel::Critical => t.e("✕  "),
                AttentionLevel::Warning => t.a("▲  "),
                AttentionLevel::Info => t.i("▸  "),
            };
            card.row(t, vec![symbol, t.n(note.text.replace('`', ""))]);
        }
        card.blank(t);
        cards.push(card);
    }

    let apps: Vec<(usize, &str)> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            HomeItem::App(id) => Some((index, id.as_str())),
            _ => None,
        })
        .collect();
    let mut fine = Card::new(t, w);
    fine.blank(t);
    let problems = needs.iter().any(|item| matches!(item, HomeItem::Failing(_) | HomeItem::Unlock));
    let summary = if apps.is_empty() {
        "No apps yet"
    } else if problems {
        "The other apps"
    } else {
        "Everything else is running"
    };
    let tick = if problems || apps.is_empty() { t.n("") } else { t.o("✓  ") };
    fine.row(t, vec![tick, t.b(summary.to_owned())]);
    fine.blank(t);
    for (index, id) in &apps {
        let (state, bad) = app_state(app, id);
        let lead = if *index == selected { t.a("▸ ") } else { t.n("  ") };
        if *index == selected {
            fine.focus_here();
        }
        let state_span = if bad { t.e(state) } else { t.d(state) };
        fine.row(t, vec![lead, t.n(pad(id, 14)), state_span]);
        screen.targets.push(Target::card_row(*id, Click::Pick { index: *index, then: Some(KeyCode::Enter) }));
    }
    if apps.is_empty() {
        fine.row(t, vec![t.d("Get apps from Apps › Get apps.")]);
    }
    fine.blank(t);
    let health = health_line(app);
    if !health.is_empty() {
        fine.row(t, vec![t.d(health)]);
        fine.blank(t);
    }
    cards.push(fine);
    let keys = if needs.is_empty() { "Keys: ↑↓ choose  ↵ open  / get apps  1-4 tabs" } else { "Keys: ↑↓ choose  ↵ act on it  / get apps  1-4 tabs" };
    screen.stack(t, cards, keys, w, h)
}

const DRIVE_CHOICES: [(&str, &str); 4] = [
    ("Check again", ""),
    ("Check the drive without changing it", "read-only"),
    ("Repair the filesystem", "can lose data; explains first and asks"),
    ("Show what the Nodo sees", ""),
];

pub fn no_drive(app: &App, t: &Theme, choice: usize) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Home, w);
    screen.before = vec![Item::Blank, Item::Blank];
    let mut card = Card::new(t, w);
    card.blank(t);
    card.row(t, vec![t.e("▲  "), t.b("The data drive isn't connected")]);
    card.blank(t);
    match &app.data.overview.drive {
        Some(failure) => {
            card.row(t, vec![t.n("Your apps keep their data on the SSD, so they're stopped until it's back. Nothing on it has been touched.")]);
            card.row(t, vec![t.d(failure.message.clone())]);
            if !failure.waiting.is_empty() {
                card.blank(t);
                card.row(t, vec![t.d(format!("Stopped: {}", failure.waiting))]);
            }
        }
        None => card.row(t, vec![t.n("The drive is back.")]),
    }
    card.blank(t);
    let mut targets = Vec::new();
    for (index, (label, note)) in DRIVE_CHOICES.iter().enumerate().skip(1) {
        let lead = if index == choice { t.a("▸ ") } else { t.n("  ") };
        if index == choice {
            card.focus_here();
        }
        let note = if note.is_empty() { String::new() } else { format!("   {note}") };
        card.link_row(t, vec![lead, t.a((*label).to_owned()), t.d(note)], label, Click::Pick { index, then: Some(KeyCode::Enter) }, &mut targets);
    }
    card.blank(t);
    card.buttons(t, &[("Check again", true, Click::Pick { index: 0, then: Some(KeyCode::Enter) })], &mut targets);
    screen.targets.extend(targets);
    screen.card(t, card, "Keys: ↵ check again  ↑↓ choose  l system log", w, h)
}
