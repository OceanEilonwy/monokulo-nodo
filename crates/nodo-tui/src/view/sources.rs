//! Sources (the old Feeds), under Apps › Get apps › Sources: a card per source, adding your own
//! with its key to compare, and what happens when a source's key changes.

use crossterm::event::KeyCode;

use super::layout::{Item, Page};
use super::pad;
use super::v3::{focused, heading, heading_button, inset, key_row, link, quiet, toggle, trail, Card, Screen3};
use crate::app::{AddFeedState, App, FeedKeyState, Tab};
use crate::hit::{Click, Target};
use crate::model::*;
use crate::theme::Theme;

fn key(c: char) -> Click {
    Click::Key(KeyCode::Char(c))
}

fn pick(index: usize, then: Option<KeyCode>) -> Click {
    Click::Pick { index, then }
}

/// A source reached over plain HTTP. Allowed, because every package must still carry a signature
/// by the pinned key, but the person adding it should know what HTTP leaves open.
pub fn is_plain_http(address: &str) -> bool {
    address.starts_with("http://")
}

fn counted(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn sources_trail(t: &Theme, last: &[&str], targets: &mut Vec<Target>) -> Vec<Item> {
    let mut parts = vec!["Apps", "Get apps", "Sources"];
    parts.extend_from_slice(last);
    vec![Item::Blank, trail(t, &parts, targets), Item::Blank]
}

pub fn sources(app: &App, t: &Theme, selected: usize, keys: bool) -> Page {
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
        let mut head = vec![if index == selected { t.a("▸ ") } else { t.n("") }, t.b(feed.name.clone())];
        if feed.key_state == KeyState::BuiltIn {
            head.push(t.d("   built in"));
        }
        let used = super::v3::width_of(&head);
        head.push(t.n(" ".repeat(44usize.saturating_sub(used))));
        head.extend(toggle(t, feed.status != FeedStatus::Disabled));
        let mut targets = vec![Target::card_row(feed.name.clone(), pick(index, None))];
        targets.push(Target::text(if feed.status == FeedStatus::Disabled { "[ ] Off" } else { "[x] On" }, pick(index, Some(KeyCode::Char(' ')))));
        card.row_with_button(t, head, "Remove", false, pick(index, Some(KeyCode::Char('x'))), &mut targets);
        card.row(t, vec![t.d(format!("{}   ·   {}", feed.address.trim_start_matches("https://"), counted(u64::from(feed.packages), "app")))]);
        match &feed.status {
            FeedStatus::Unreachable(problem) => {
                card.blank(t);
                card.row(t, vec![t.e(format!("✕  Couldn't reach it: {problem}")), t.n("   "), t.a("Try again")]);
                targets.push(Target::text("Try again", key('r')));
            }
            FeedStatus::Blocked => {
                card.blank(t);
                card.row(t, vec![t.e("▲  Its key changed. Nothing new installs from it until you decide."), t.n("   "), t.a("Decide ›")]);
                targets.push(Target::text("Decide ›", pick(index, Some(KeyCode::Enter))));
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
        card.blank(t);
        screen.targets.extend(targets);
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
    let page_keys = "Keys: ↑↓ choose  Space on/off  a add  x remove  d keys  ↵ its apps  Esc back";
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
    let mut targets = Vec::new();
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
            card.buttons(t, &[("Cancel", false, Click::Key(KeyCode::Esc)), ("Check", true, Click::Key(KeyCode::Enter))], &mut targets);
            "Keys: paste the address  ↵ check  Esc cancel"
        }
        AddFeedState::Confirm { probe, .. } => {
            let mark = if probe.signed { t.o("✓  ") } else { t.e("✕  ") };
            card.row(t, vec![mark, t.b(probe.name.clone()), t.d(format!("   {}", probe.address))]);
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
            card.buttons(t, &[(cancel.as_str(), false, pick(0, Some(KeyCode::Enter))), (add.as_str(), true, pick(1, Some(KeyCode::Enter)))], &mut targets);
            "Keys: ←→ choose  ↵ confirm  p its apps  Esc cancel"
        }
    };
    screen.targets.extend(targets);
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
    let mut targets = Vec::new();
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
        card.buttons(t, &[("Back", false, Click::Key(KeyCode::Esc)), ("Trust the new key", true, Click::Key(KeyCode::Enter))], &mut targets);
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
            &mut targets,
        );
        "Keys: ↑↓ choose  ↵ confirm  Esc decide later"
    };
    screen.targets.extend(targets);
    screen.card(t, card, keys, w, h)
}
