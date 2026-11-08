//! The feed list, adding a feed, and a changed signing key.

use ratatui::text::Line;

use super::layout::{Item, Page, Panel};
use super::{button_line, hints, mark, pad, tab_bar};
use crate::app::{AddFeedState, App, FeedKeyState, Tab};
use crate::model::*;
use crate::theme::Theme;

pub fn feeds(app: &App, t: &Theme, selected: usize) -> Page {
    let mut rows = vec![Item::Text(Line::from(t.d(format!(
        "   {}{}   {}{}",
        pad("Name", 14),
        pad("Packages", 8),
        pad("Key", 10),
        "Checked"
    ))))];
    for (index, feed) in app.data.feeds.iter().enumerate() {
        let (symbol, status) = match &feed.status {
            FeedStatus::Healthy => (t.o("✓"), t.n(feed.checked.clone())),
            FeedStatus::Blocked => (t.e("✕"), t.e("blocked, press Enter")),
            FeedStatus::Unreachable(text) => (t.a("▲"), t.a(text.clone())),
            FeedStatus::Disabled => (t.d("○"), t.d("disabled")),
        };
        let key = match feed.key_state {
            KeyState::BuiltIn => t.n(pad("built in", 10)),
            KeyState::Pinned => t.n(pad("pinned", 10)),
            KeyState::Changed => t.e(pad("changed", 10)),
        };
        let is_selected = index == selected;
        let line = Line::from(vec![
            t.n(mark(is_selected, " ")),
            symbol,
            t.n(format!(" {}{:>8}   ", pad(&feed.name, 14), feed.packages)),
            key,
            status,
        ]);
        rows.push(if is_selected { Item::Selected(line, Line::default()) } else { Item::Row(line, Line::default()) });
    }

    let mut page = Page::new(tab_bar(app, t, Tab::Feeds), Line::from(t.d("monokulo-nodo")))
        .panel(Panel::new("Your feeds", rows));
    if let Some(feed) = app.data.feeds.get(selected) {
        let installed = if feed.installed_from.is_empty() { "Nothing".to_owned() } else { feed.installed_from.join(", ") };
        let pair = |label: &str, value: String| Item::Row(Line::from(t.d(label.to_owned())), Line::from(t.n(value)));
        page = page.panel(
            Panel::new(
                feed.name.clone(),
                {
                    let mut rows = vec![pair("Address", feed.address.clone())];
                    if is_plain_http(&feed.address) {
                        rows.push(pair("Connection", "plain HTTP: signed, but updates can be held back".to_owned()));
                    }
                    rows.extend([
                        pair("Key", feed.fingerprint.clone()),
                        pair("Added", feed.added.clone()),
                        pair("Installed from it", installed),
                    ]);
                    rows
                },
            )
            .grow(),
        );
    }
    let disabled = app.data.feeds.get(selected).is_some_and(|feed| feed.status == FeedStatus::Disabled);
    let toggle = if disabled { "enable" } else { "disable" };
    page.footer(Item::Text(Line::from(t.d("Removing a feed keeps installed packages, without updates."))))
        .footer(Item::Text(hints(
            t,
            &[("a", "add"), ("r", "refresh"), ("e", toggle), ("x", "remove"), ("↵", "packages"), ("Tab", "next tab")],
        )))
}

pub fn add_feed(t: &Theme, state: &AddFeedState) -> Page {
    let header = |step: &str| {
        Page::new(
            Line::from(vec![t.n("Feeds "), t.d("› "), t.b("Add a feed")]),
            Line::from(t.d(step.to_owned())),
        )
    };
    match state {
        AddFeedState::Address { input, error } => {
            let mut items = vec![Item::Text(Line::from(vec![t.n(input.display()), t.s(" ")]))];
            if let Some(error) = error {
                items.push(Item::Text(Line::from(t.e(format!("✕ {error}")))));
            }
            items.push(Item::Text(Line::from(t.d("Paste the address the publisher gave you."))));
            header("Step 1 of 2")
                .panel(Panel::new("Address", items).grow())
                .footer(Item::Text(hints(t, &[("↵", "look up"), ("Esc", "back")])))
        }
        AddFeedState::Confirm { probe, trust_focused } => {
            let signed = if probe.signed { "signed" } else { "NOT signed" };
            let found = format!("✓ Found feed \"{}\", {} packages, {signed}", probe.name, probe.packages);
            let mut page = header("Step 2 of 2");
            if is_plain_http(&probe.address) {
                page = page.panel(Panel::error("Plain HTTP", plain_http_warning(t)));
            }
            page
                .panel(Panel::new(
                    "Address",
                    vec![Item::Text(Line::from(t.n(probe.address.clone()))), Item::Text(Line::from(t.o(found)))],
                ))
                .panel(Panel::accent(
                    "Check this key",
                    vec![
                        Item::Text(Line::from(t.b(probe.fingerprint.clone()))),
                        Item::Text(Line::from(t.d("Compare it with the key the publisher shows on a site you already trust."))),
                        Item::Text(Line::from(t.d("This device will only accept this feed's packages if they are signed by this key."))),
                    ],
                ))
                .panel(
                    Panel::new(
                        "What trusting it allows",
                        vec![
                            Item::Text(Line::from(vec![t.i("●"), t.n(" Its packages appear in search and can offer updates.")])),
                            Item::Text(Line::from(vec![t.i("●"), t.n(" You still review each package's permissions before it installs.")])),
                            Item::Blank,
                            Item::Text(Line::from(vec![t.o("✓"), t.n(" It cannot replace system packages or another feed's packages.")])),
                            Item::Text(Line::from(vec![t.o("✓"), t.n(" It cannot install anything without you.")])),
                        ],
                    )
                    .grow(),
                )
                .footer(Item::Text(Line::from(t.d("Packages run on your node, near your wallet data. Only add publishers you trust."))))
                .footer(Item::Row(
                    button_line(t, "Cancel", "Trust this key", *trust_focused),
                    hints(t, &[("←→", "choose"), ("↵", "confirm"), ("p", "list packages"), ("Esc", "back")]),
                ))
        }
    }
}

/// A feed reached over plain HTTP. Allowed, because every package must still carry a signature
/// by the pinned key, but the person adding it should know what HTTP leaves open.
pub fn is_plain_http(address: &str) -> bool {
    address.starts_with("http://")
}

fn plain_http_warning(t: &Theme) -> Vec<Item> {
    [
        "Signatures still protect every package: nothing changed on the way can install.",
        "Anyone on the network can hold back updates or offer an older signed version, and",
        "this key arrives unprotected, so comparing it is your only check. Prefer https://.",
    ]
    .into_iter()
    .map(|line| Item::Text(Line::from(t.n(line))))
    .collect()
}

pub fn feed_key(t: &Theme, state: &FeedKeyState) -> Page {
    let change = &state.change;
    let affected = change
        .affected
        .iter()
        .map(|(name, note)| Item::Row(Line::from(t.n(name.clone())), Line::from(t.d(note.clone()))))
        .collect();
    let options = [
        ("Keep the old key and stay blocked (safe default)", ""),
        ("Trust the new key", "you must type the feed name to confirm"),
        ("Remove this feed", ""),
    ];
    let mut choices: Vec<Item> = options
        .iter()
        .enumerate()
        .map(|(index, (label, note))| {
            let selected = index == state.choice;
            let line = Line::from(vec![
                t.n(mark(selected, &format!(" {label}"))),
                t.n(if note.is_empty() { "" } else { "  " }),
                t.d(*note),
            ]);
            if selected {
                Item::Selected(line, Line::default())
            } else {
                Item::Text(line)
            }
        })
        .collect();
    let keys: &[(&str, &str)] = if let Some(input) = &state.typing {
        choices.push(Item::Blank);
        choices.push(Item::Text(Line::from(vec![
            t.d(format!("Type {} to confirm: ", change.feed)),
            t.n(input.display()),
            t.s(" "),
        ])));
        &[("↵", "trust the new key"), ("Esc", "go back")]
    } else {
        &[("↑↓", "choose"), ("↵", "confirm"), ("Esc", "decide later")]
    };
    if let Some(error) = &state.error {
        choices.push(Item::Text(Line::from(t.e(format!("✕ {error}")))));
    }
    Page::new(
        Line::from(vec![t.n("Feeds "), t.d("› "), t.b(change.feed.clone())]),
        Line::from(t.e("✕ Updates blocked")),
    )
    .panel(Panel::error(
        "This feed's signing key changed",
        vec![Item::Text(Line::from(t.n(
            "Publishers sometimes rotate keys. It can also mean the feed was taken over. Nothing new from this feed will install until you decide.",
        )))],
    ))
    .panel(Panel::new(
        "Keys",
        vec![
            Item::Row(Line::from(t.d("Trusted")), Line::from(t.n(change.trusted.clone()))),
            Item::Row(Line::from(t.e("Now signing")), Line::from(t.e(change.now_signing.clone()))),
        ],
    ))
    .panel(Panel::new("Affected", affected))
    .plain(vec![Item::Text(Line::from(t.n(" Check the publisher's announcement through a channel other than this feed.")))])
    .panel(Panel::new("Your choice", choices).grow())
    .footer(Item::Text(hints(t, keys)))
}
