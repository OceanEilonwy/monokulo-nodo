//! Package search, detail, and the two screens that stop an install before review.

use ratatui::text::{Line, Span};

use super::layout::{Item, Page, Panel};
use super::{hints, mark, pad, tab_bar};
use crate::app::{App, CapabilityState, SearchState, Tab};
use crate::model::*;
use crate::theme::Theme;

fn feed_span(t: &Theme, feed: &str, width: usize) -> Span<'static> {
    let text = pad(feed, width);
    match feed {
        "official" => t.n(text),
        "monokulo" => t.i(text),
        _ => t.a(text),
    }
}

fn need_item(t: &Theme, need: &Need) -> Item {
    let symbol = if need.met { t.o("✓") } else { t.e("✕") };
    let mut spans = vec![symbol, t.n(format!(" {}", need.name))];
    if let Some(source) = &need.source {
        spans.push(t.d(format!("  {source}")));
    }
    Item::Text(Line::from(spans))
}

pub fn search(app: &App, t: &Theme, state: &SearchState) -> Page {
    let results = app.search_results(state);
    let scope = state.feed_filter.as_deref().map_or_else(|| "All feeds".to_owned(), |feed| format!("Feed: {feed}"));
    let count = format!("{scope} · Any status · {} of {} results", results.len(), app.data.catalog.len());
    let mut query = vec![t.a("/"), t.n(format!(" {}", state.query.display()))];
    if !state.list_focused {
        query.push(t.s(" "));
    }
    let search_panel = Panel::new("Search", vec![Item::Row(Line::from(query), Line::from(t.d(count)))]);

    let mut rows = Vec::new();
    for (index, pkg) in results.iter().enumerate() {
        let symbol = match pkg.status {
            PackageStatus::Installed => "●",
            PackageStatus::Available => "○",
            PackageStatus::UpdateReady => "▲",
        };
        let selected = index == state.selected;
        let left = Line::from(vec![
            t.n(mark(selected, &format!(" {symbol} {}", pad(&pkg.id.name, 18)))),
            feed_span(t, &pkg.id.feed, 11),
        ]);
        let right = Line::from(if selected { t.n(pkg.tagline.clone()) } else { t.d(pkg.tagline.clone()) });
        rows.push(if selected { Item::Selected(left, right) } else { Item::Row(left, right) });
    }
    if results.is_empty() {
        rows.push(Item::Text(Line::from(t.d(" Nothing matches that search."))));
    }
    rows.push(Item::Blank);
    rows.push(Item::Text(Line::from(t.d(" ● installed   ○ available   ▲ update ready"))));
    rows.push(Item::Text(Line::from(t.d(" Feed names show where a package came from."))));
    rows.push(Item::Text(Line::from(t.d(" Same name in two feeds? Both are shown, and you pick one."))));

    let mut preview = Vec::new();
    if let Some(pkg) = results.get(state.selected) {
        preview.push(Item::Text(Line::from(t.b(pkg.id.name.clone()))));
        preview.push(Item::Text(Line::from(t.d(format!("From the {} feed", pkg.id.feed)))));
        preview.push(Item::Blank);
        preview.extend(pkg.description.iter().map(|line| Item::Text(Line::from(t.n(line.clone())))));
        preview.push(Item::Blank);
        preview.push(Item::Row(Line::from(t.d("Version")), Line::from(t.n(pkg.version.clone()))));
        preview.push(Item::Row(Line::from(t.d("Size")), Line::from(t.n(pkg.size.clone()))));
        for (index, need) in pkg.needs.iter().enumerate() {
            let label = if index == 0 { "Needs" } else { "" };
            let value = if need.met { t.o(format!("✓ {}", need.name)) } else { t.e(format!("✕ {}", need.name)) };
            preview.push(Item::Row(Line::from(t.d(label)), Line::from(value)));
        }
    }

    let keys = if state.list_focused {
        hints(
            t,
            &[("type", "to filter"), ("↑↓", "move"), ("↵", "details"), ("i", "install"), ("f", "filter feed"), ("Esc", "clear")],
        )
    } else {
        hints(t, &[("type", "to filter"), ("↓", "results, then i installs"), ("↵", "details"), ("Esc", "clear")])
    };
    Page::new(tab_bar(app, t, Tab::Packages), Line::from(t.d("monokulo-nodo")))
        .panel(search_panel)
        .columns(vec![(67, vec![Panel::new("Results", rows).grow()]), (33, vec![Panel::new("Preview", preview).grow()])])
        .footer(Item::Text(keys))
}

pub fn detail(t: &Theme, detail: &PackageDetail) -> Page {
    let about = vec![
        Item::Text(Line::from(t.n(detail.about.clone()))),
        Item::Text(Line::from(t.d(format!("Version {} · {} · {}", detail.version, detail.size, detail.license)))),
    ];
    let signature =
        if detail.signature_verified { t.o("✓ signature verified") } else { t.e("✕ signature not verified") };

    let mut settings: Vec<Item> =
        detail.public_settings.iter().map(|path| Item::Text(Line::from(t.n(path.clone())))).collect();
    if settings.is_empty() {
        settings.push(Item::Text(Line::from(t.d("None."))));
    } else {
        settings.push(Item::Text(Line::from(t.d("Other packages can reference this."))));
    }
    let mut needs: Vec<Item> = detail.needs.iter().map(|need| need_item(t, need)).collect();
    if needs.is_empty() {
        needs.push(Item::Text(Line::from(t.d("Nothing else."))));
    }

    let mut asks: Vec<Item> = detail
        .asks
        .iter()
        .map(|(label, value)| Item::Row(Line::from(t.d(label.clone())), Line::from(t.n(value.clone()))))
        .collect();
    let storage = if detail.encrypted_storage {
        t.a("◆ Yes, needs the device passphrase after reboot")
    } else {
        t.n("No")
    };
    asks.push(Item::Row(Line::from(t.d("Encrypted storage")), Line::from(storage)));

    let keys: &[(&str, &str)] = if detail.installed {
        &[("s", "settings"), ("v", "choose version"), ("c", "changelog"), ("Esc", "back")]
    } else {
        &[("i", "install"), ("v", "choose version"), ("c", "changelog"), ("Esc", "back")]
    };
    Page::new(
        Line::from(vec![t.n("Packages "), t.d("› "), t.b(detail.id.name.clone())]),
        Line::from(vec![t.d(format!("{} feed · ", detail.id.feed)), signature]),
    )
    .panel(Panel::new("About", about))
    .columns(vec![(1, vec![Panel::new("Needs", needs)]), (1, vec![Panel::new("Public settings", settings)])])
    .panel(Panel::new("It will ask for", asks).grow())
    .footer(Item::Text(hints(t, keys)))
}

pub fn rejected(t: &Theme, data: &RejectionData) -> Page {
    let mut reasons = Vec::new();
    for (index, violation) in data.violations.iter().enumerate() {
        if index > 0 {
            reasons.push(Item::Blank);
        }
        let symbol = if violation.passed { t.o("✓") } else { t.e("✕") };
        reasons.push(Item::Text(Line::from(vec![symbol, t.n(format!(" {}", violation.rule))])));
        reasons.push(Item::Text(Line::from(t.d(format!("   {}", violation.evidence)))));
    }
    Page::new(
        Line::from(vec![t.n("Install "), t.b(data.id.name.clone()), t.n(format!(" {}", data.version))]),
        Line::from(t.a(format!("{} feed", data.id.feed))),
    )
    .panel(Panel::error(
        "Not installed",
        vec![Item::Text(Line::from(t.n("This package breaks the install rules, so nothing was changed on this device.")))],
    ))
    .panel(Panel::new("Why it was rejected", reasons).grow())
    .panel(Panel::new(
        "What you can do",
        vec![Item::Text(Line::from(t.d(
            "There is no override for these rules. Report it to the feed's publisher, or remove the feed if you no longer trust it.",
        )))],
    ))
    .footer(Item::Text(hints(t, &[("y", "copy report"), ("f", "open feed"), ("Esc", "back to search")])))
}

pub fn capability(t: &Theme, state: &CapabilityState) -> Page {
    let data = &state.data;
    let mut matrix = vec![Item::Text(Line::from(t.d(format!(
        "{}{}{}",
        pad("", 24),
        pad(&data.current_node, 11),
        data.alternative_node
    ))))];
    let tick = |on: bool| if on { t.o(pad("✓", 11)) } else { t.e(pad("✕", 11)) };
    for (name, current, alternative) in &data.matrix {
        matrix.push(Item::Text(Line::from(vec![t.n(pad(name, 24)), tick(*current), tick(*alternative)])));
    }
    let options = [
        (format!("Cancel and keep {}", data.current_node), String::new()),
        ("Check for a compatible version".to_owned(), data.compatible_note.clone()),
        (format!("Install {} and switch", data.alternative_node), data.switch_note.clone()),
    ];
    let mut choices: Vec<Item> = options
        .iter()
        .enumerate()
        .map(|(index, (label, note))| {
            let line = Line::from(vec![
                t.n(mark(index == state.choice, &format!(" {label}"))),
                t.n(if note.is_empty() { "" } else { "  " }),
                t.d(note.clone()),
            ]);
            if index == state.choice {
                Item::Selected(line, Line::default())
            } else {
                Item::Text(line)
            }
        })
        .collect();
    choices.push(Item::Blank);
    choices.push(Item::Text(Line::from(t.d("Switching stops services that need a node until it finishes syncing."))));
    choices.push(Item::Text(Line::from(t.d("Both chains are kept, so you can switch back without syncing again."))));
    Page::new(
        Line::from(vec![t.n("Install "), t.b(data.id.name.clone()), t.n(format!(" {}", data.version))]),
        Line::from(t.a("▲ Needs a change first")),
    )
    .panel(Panel::accent("Your node cannot serve this package", vec![Item::Text(Line::from(t.n(data.message.clone())))]))
    .panel(Panel::new("What each node provides", matrix))
    .panel(Panel::new("Your options", choices).grow())
    .footer(Item::Text(hints(t, &[("↑↓", "choose"), ("↵", "confirm"), ("Esc", "cancel")])))
}
