//! Home: what needs you, with one button each, then that everything else is running. The data
//! drive being missing replaces it.

use crossterm::event::KeyCode;

use super::layout::{Item, Page};
use super::pad;
use super::v3::{heading, Card, Screen3};
use crate::app::App;
use crate::hit::{Click, Target};
use nodo_ui_core::model::AttentionLevel;
use nodo_ui_core::queries::HomeItem;
use nodo_ui_core::words::{app_state, health_line};
use nodo_ui_core::screen::Tab;
use crate::theme::Theme;

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
                if app.data.services.iter().any(|svc| svc.state == nodo_api::ServiceState::Locked) {
                    "Apps' data waits for the device passphrase. They start once you unlock.".to_owned()
                } else {
                    "Saved keys and secret settings, such as a domain provider's, need the device passphrase.".to_owned()
                },
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
                    .map(|u| if u.name == nodo_ui_core::model::SYSTEM_BASE { "the Nodo".to_owned() } else { u.name.clone() })
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
        card.buttons(t, &[(label, true, Click::Pick { index, then: Some(KeyCode::Enter) })]);
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
    // As wide as the longest name, so a long one never runs into its state.
    let name_width = apps.iter().map(|(_, id)| id.chars().count() + 2).max().unwrap_or(0).max(14);
    for (index, id) in &apps {
        let (state, bad) = app_state(&app.data, id);
        let lead = if *index == selected { t.a("▸ ") } else { t.n("  ") };
        if *index == selected {
            fine.focus_here();
        }
        let state_span = if bad { t.e(state) } else { t.d(state) };
        // Tied to its row: found by its name anywhere, an app's row target also took the update
        // card's list of names ("moneropay, the Nodo, …") above it.
        let target = Target::card_row(*id, Click::Pick { index: *index, then: Some(KeyCode::Enter) });
        fine.row_with(t, vec![lead, t.n(pad(id, name_width)), state_span], [target]);
    }
    if apps.is_empty() {
        fine.row(t, vec![t.d("Get apps from Apps › Get apps.")]);
    }
    fine.blank(t);
    let health = health_line(&app.data);
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
    for (index, (label, note)) in DRIVE_CHOICES.iter().enumerate().skip(1) {
        let lead = if index == choice { t.a("▸ ") } else { t.n("  ") };
        if index == choice {
            card.focus_here();
        }
        let note = if note.is_empty() { String::new() } else { format!("   {note}") };
        card.link_row(t, vec![lead, t.a((*label).to_owned()), t.d(note)], label, Click::Pick { index, then: Some(KeyCode::Enter) });
    }
    card.blank(t);
    card.buttons(t, &[("Check again", true, Click::Pick { index: 0, then: Some(KeyCode::Enter) })]);
    screen.card(t, card, "Keys: ↵ check again  ↑↓ choose  l system log", w, h)
}

#[cfg(test)]
mod tests {
    use nodo_ui_core::words::uptime;

    #[test]
    fn uptime_uses_the_two_largest_units() {
        assert_eq!(uptime(0), "under a minute");
        assert_eq!(uptime(59), "under a minute");
        assert_eq!(uptime(60), "1m");
        assert_eq!(uptime(45 * 60 + 30), "45m");
        assert_eq!(uptime(3600), "1h");
        assert_eq!(uptime(3 * 3600 + 12 * 60 + 59), "3h 12m");
        assert_eq!(uptime(86400), "1d");
        assert_eq!(uptime(83166), "23h 6m");
        assert_eq!(uptime(2 * 86400 + 5 * 3600 + 59 * 60), "2d 5h");
        assert_eq!(uptime(400 * 86400), "400d");
    }
}
