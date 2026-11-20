//! Sources (the old Feeds), under Apps › Get apps › Sources: a card per source, adding your own
//! with its key to compare, and what happens when a source's key changes.

use crossterm::event::KeyCode;

use super::layout::{Item, Page};
use super::pad;
use super::v3::{focused, heading, heading_button, inset, key_row, link, quiet, toggle, trail, Card, Screen3};
use crate::app::{AddFeedState, App, FeedKeyState, FeedRename};
use crate::hit::{Click, Target};
use nodo_ui_core::model::*;
use nodo_ui_core::screen::Tab;
use nodo_ui_core::words::{counted, is_plain_http};
use crate::theme::Theme;

/// `text` cut to `room` characters, ending in "…" when it was cut.
fn shorten(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_owned();
    }
    let kept: String = text.chars().take(room.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn key(c: char) -> Click {
    Click::Key(KeyCode::Char(c))
}

fn pick(index: usize, then: Option<KeyCode>) -> Click {
    Click::Pick { index, then }
}

fn sources_trail(t: &Theme, last: &[&str], targets: &mut Vec<Target>) -> Vec<Item> {
    let mut parts = vec!["Apps", "Get apps", "Sources"];
    parts.extend_from_slice(last);
    vec![Item::Blank, trail(t, &parts, targets), Item::Blank]
}

pub fn sources(app: &App, t: &Theme, selected: usize, keys: bool, renaming: Option<&FeedRename>) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let mut before_targets = Vec::new();
    let mut before = sources_trail(t, &[], &mut before_targets);
    before.push(heading_button(t, "Sources", "Add a source", key('a'), &mut before_targets));
    before.push(quiet(t, "Where the Nodo finds apps. Each source signs its apps; the Nodo only installs what its key signed."));
    before.push(Item::Blank);
    before.push(Item::Blank);
    screen.before = before;
    screen.targets.extend(before_targets);

    let mut cards = Vec::new();
    for (index, feed) in app.data.feeds.iter().enumerate() {
        let mut card = Card::new(t, w);
        card.blank(t);
        // The list scrolls to the selected source; without it, ↓ moved the selection off screen.
        if index == selected {
            card.focus_here();
        }
        let switch = toggle(t, feed.status != FeedStatus::Disabled);
        // The switch lines up at column 44, or further left on a narrow card: past the card's
        // edge it wrapped, "[x]" on one row and "On" on the next, and was not clickable.
        let column = 44usize.min(card.width.saturating_sub(4 + super::v3::width_of(&switch)));
        // An address can be longer than the row: it is cut short with "…" so the switch stays put.
        let built_in = if feed.key_state == KeyState::BuiltIn { 10 } else { 0 };
        let room = column.saturating_sub(2 + built_in + 2).max(8);
        let title = shorten(feed.shown(), room);
        let mut head = vec![if index == selected { t.a("▸ ") } else { t.n("") }, t.b(title.clone())];
        if feed.key_state == KeyState::BuiltIn {
            head.push(t.d("   built in"));
        }
        let used = super::v3::width_of(&head);
        head.push(t.n(" ".repeat(column.saturating_sub(used).max(2))));
        head.extend(switch);
        // Tied to the head row, before its button: found by its name anywhere, a source's target
        // also took the address of the source above it (feeds.monokulo.example/official).
        let toggle_text = if feed.status == FeedStatus::Disabled { "[ ] Off" } else { "[x] On" };
        card.on_row(card.next_row(), [Target::card_row(title, pick(index, None)), Target::text(toggle_text, pick(index, Some(KeyCode::Char(' '))))]);
        card.row_with_button(t, head, "Remove", false, pick(index, Some(KeyCode::Char('x'))));
        // Without a name of its own the address is already the title, so it is not said twice.
        let count = counted(u64::from(feed.packages), "app");
        let details = if feed.label.is_some() { format!("{}   ·   {count}", feed.address.trim_start_matches("https://")) } else { count };
        card.row(t, vec![t.d(details)]);
        if let Some(rename) = renaming.filter(|_| index == selected) {
            card.blank(t);
            let mut name = vec![t.d(pad("Name", 10))];
            name.extend(inset(t, rename.input.text(), 30, None, true));
            card.row(t, name);
            match &rename.error {
                Some(error) => card.row(t, vec![t.n(" ".repeat(10)), t.e(format!("✕ {error}"))]),
                None => card.row(t, vec![t.n(" ".repeat(10)), t.d(format!("Leave it empty to call it {} again.", feed.address.trim_start_matches("https://")))]),
            }
        }
        match &feed.status {
            FeedStatus::Unreachable(problem) => {
                card.blank(t);
                card.row_link(t, vec![t.e(format!("✕  Couldn't reach it: {problem}"))], "Try again", key('r'));
            }
            FeedStatus::Blocked => {
                card.blank(t);
                card.row_link(t, vec![t.e("▲  Its key changed. Nothing new installs from it until you decide.")], "Decide ›", pick(index, Some(KeyCode::Enter)));
            }
            _ => {}
        }
        if is_plain_http(&feed.address) {
            card.row(t, vec![t.d("Plain HTTP: signed, but updates can be held back")]);
        }
        if keys {
            card.blank(t);
            key_row(&mut card, t, "Key", &feed.fingerprint, false);
        }
        if index == selected {
            // Its problem and "Try again" scroll into view with it.
            card.focus_ends_here();
        }
        card.blank(t);
        cards.push(card);
    }
    if app.data.feeds.is_empty() {
        let mut card = Card::new(t, w);
        card.blank(t);
        card.row(t, vec![t.d("No sources yet. Add the address a publisher gave you.")]);
        card.blank(t);
        cards.push(card);
    }
    let mut after_targets = Vec::new();
    screen.after = vec![Item::Blank, link(t, if keys { "Hide keys ‹" } else { "Show keys ›" }, key('d'), &mut after_targets)];
    // Targets after the cards come last, in drawing order.
    let page_keys = if renaming.is_some() {
        "Keys: type its name  ↵ save  Esc cancel"
    } else {
        "Keys: ↑↓ choose  Space on/off  n name  a add  x remove  d keys  ↵ its apps  Esc back"
    };
    let mut page = screen.stack(t, cards, page_keys, w, h);
    page.targets.extend(after_targets);
    page
}

pub fn add_feed(app: &App, t: &Theme, state: &AddFeedState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let mut before_targets = Vec::new();
    screen.before = sources_trail(t, &["Add"], &mut before_targets);
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    let keys = match state {
        AddFeedState::Address { input, error } => {
            card.row(t, vec![t.b("Add a source")]);
            card.blank(t);
            card.row(t, vec![t.n("Paste the address the publisher gave you.")]);
            card.blank(t);
            let mut address = vec![t.d(pad("Address", 10))];
            address.extend(inset(t, input.text(), 44, None, true));
            card.row(t, address);
            if let Some(error) = error {
                card.row(t, vec![t.n(" ".repeat(10)), t.e(format!("✕ {error}"))]);
            }
            card.blank(t);
            card.row(t, vec![t.d("Nothing is installed yet: the Nodo first shows you what the source offers and its key.")]);
            card.blank(t);
            card.buttons(t, &[("Cancel", false, Click::Key(KeyCode::Esc)), ("Check", true, Click::Key(KeyCode::Enter))]);
            "Keys: paste the address  ↵ check  Esc cancel"
        }
        AddFeedState::Confirm { probe, naming, .. } => {
            let mark = if probe.signed { t.o("✓  ") } else { t.e("✕  ") };
            let mut line = vec![mark, t.b(probe.shown().to_owned())];
            if !probe.label.trim().is_empty() {
                line.push(t.d(format!("   {}", probe.address)));
            }
            card.row(t, line);
            card.blank(t);
            let mut name = vec![t.d(pad("Name", 10))];
            let typed = naming.as_ref().map_or(probe.label.as_str(), |input| input.text());
            name.extend(inset(t, if typed.is_empty() && naming.is_none() { probe.address.trim_start_matches("https://") } else { typed }, 30, None, naming.is_some()));
            card.row(t, name);
            let hint = if naming.is_some() { format!("Leave it empty to call it {}.", probe.address.trim_start_matches("https://")) } else { "Press n to give it a name of your own.".to_owned() };
            card.row(t, vec![t.n(" ".repeat(10)), t.d(hint)]);
            card.blank(t);
            card.row(t, vec![t.n(format!("It offers {}.", counted(u64::from(probe.packages), "app")))]);
            if !probe.signed {
                card.row(t, vec![t.e("Its list of apps isn't signed, so the Nodo won't use it.")]);
            }
            card.blank(t);
            key_row(&mut card, t, "Its key", &probe.fingerprint, true);
            card.blank(t);
            card.row(t, vec![t.n("Check this matches what the publisher gave you, by a way you trust (in person, their site, a message you know is from them). If it doesn't match, don't add it.")]);
            if is_plain_http(&probe.address) {
                card.blank(t);
                card.row(t, vec![t.a("▲  "), t.b("Plain HTTP")]);
                card.row(t, vec![t.n("Signatures still protect every app: nothing changed on the way can install. But anyone on the network can hold back updates or offer an older signed version, and this key arrives unprotected, so comparing it is your only check. Prefer https://.")]);
            }
            card.blank(t);
            let trust = matches!(state, AddFeedState::Confirm { trust_focused: true, .. });
            let cancel = focused("Cancel", !trust);
            let add = focused("Add source", trust);
            card.buttons(t, &[(cancel.as_str(), false, pick(0, Some(KeyCode::Enter))), (add.as_str(), true, pick(1, Some(KeyCode::Enter)))]);
            if naming.is_some() { "Keys: type its name  ↵ done" } else { "Keys: ←→ choose  ↵ confirm  n name  p its apps  Esc cancel" }
        }
    };
    screen.card(t, card, keys, w, h)
}

pub fn feed_key(app: &App, t: &Theme, state: &FeedKeyState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let change = &state.change;
    let mut before_targets = Vec::new();
    let mut before = sources_trail(t, &[change.feed.as_str()], &mut before_targets);
    before.push(heading(t, "Check before you trust"));
    before.push(Item::Blank);
    screen.before = before;
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    card.row(t, vec![t.e("▲  "), t.b(format!("{} is now signed with a different key", change.feed))]);
    card.blank(t);
    card.row(t, vec![t.n(format!(
        "This happens when a publisher replaces a lost key, and also when someone else takes over their address. Until you decide, the Nodo installs nothing new from {}.",
        change.feed
    ))]);
    card.blank(t);
    key_row(&mut card, t, "Key you trust", &change.trusted, false);
    key_row(&mut card, t, "New key", &change.now_signing, true);
    if !change.affected.is_empty() {
        card.blank(t);
        let names: Vec<&str> = change.affected.iter().map(|(name, _)| name.as_str()).collect();
        card.row(t, vec![t.d(format!("Apps from it on this Nodo: {}", names.join(", ")))]);
    }
    card.blank(t);
    let keys = if let Some(input) = &state.typing {
        card.row(t, vec![t.n("Ask the publisher, by a way you trust, whether the new key is theirs.")]);
        card.row(t, vec![t.b(format!("Type {} to confirm", change.feed))]);
        let mut typed = vec![t.d(pad("Name", 15))];
        typed.extend(inset(t, input.text(), 30, None, true));
        card.row(t, typed);
        if let Some(error) = &state.error {
            card.row(t, vec![t.n(" ".repeat(15)), t.e(format!("✕ {error}"))]);
        }
        card.blank(t);
        card.buttons(t, &[("Back", false, Click::Key(KeyCode::Esc)), ("Trust the new key", true, Click::Key(KeyCode::Enter))]);
        "Keys: type the name  ↵ trust the new key  Esc back"
    } else {
        card.row(t, vec![t.d("To trust the new key you'll type the source's name, so it can't happen by accident.")]);
        card.blank(t);
        let remove = focused("!Remove source", state.choice == 2);
        let trust = focused("Trust the new key", state.choice == 1);
        let keep = focused("Keep the old key", state.choice == 0);
        card.buttons(
            t,
            &[
                (remove.as_str(), false, pick(2, Some(KeyCode::Enter))),
                (trust.as_str(), false, pick(1, Some(KeyCode::Enter))),
                (keep.as_str(), true, pick(0, Some(KeyCode::Enter))),
            ],
        );
        "Keys: ↑↓ choose  ↵ confirm  Esc decide later"
    };
    screen.card(t, card, keys, w, h)
}
