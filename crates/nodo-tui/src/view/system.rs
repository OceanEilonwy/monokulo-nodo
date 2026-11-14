//! System: the Nodo itself. One page lists its parts; updates for the Nodo and the apps share one
//! review page; automatic updates; what happened to an update; and device and app logs.

use crossterm::event::KeyCode;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::layout::{Item, Page};
use super::pad;
use super::v3::{field_target, heading, inset, link, margin, quiet, tick, toggle, trail, Card, Screen3};
use crate::app::{App, AutoUpdateState, LogsState, Tab, UpdateResultsState, UpdatesState, SYSTEM_ROWS};
use crate::hit::{Click, Target};
use crate::model::*;
use crate::theme::Theme;

fn key(c: char) -> Click {
    Click::Key(KeyCode::Char(c))
}

fn pick(index: usize, then: Option<KeyCode>) -> Click {
    Click::Pick { index, then }
}

fn system_trail(t: &Theme, last: &[&str], targets: &mut Vec<Target>) -> Vec<Item> {
    let mut parts = vec!["System"];
    parts.extend_from_slice(last);
    vec![Item::Blank, trail(t, &parts, targets), Item::Blank]
}

fn nodo_update(app: &App) -> Option<&UpdateItem> {
    app.data.updates.iter().find(|item| item.name == SYSTEM_BASE && !item.held)
}

fn row_summary(app: &App, row: &str) -> String {
    match row {
        "Updates" => {
            let apps = app.data.updates.iter().filter(|u| u.name != SYSTEM_BASE && !u.held).count();
            let nodo = nodo_update(app).is_some();
            let mut parts = Vec::new();
            if apps > 0 {
                parts.push(if apps == 1 { "1 for an app".to_owned() } else { format!("{apps} for apps") });
            }
            if nodo {
                parts.push("1 for the Nodo".to_owned());
            }
            let auto = app.data.auto_update.enabled_count();
            let automatic = if auto == 0 { "automatic updates off".to_owned() } else { format!("automatic for {auto}") };
            if parts.is_empty() {
                format!("Up to date · {automatic}")
            } else {
                format!("{} · {automatic}", parts.join(", "))
            }
        }
        "Backups" => "Make one now, or on a schedule".to_owned(),
        "Network" => "Cable, Wi-Fi and Bluetooth".to_owned(),
        "Device logs" => "What the Nodo and its apps have been doing".to_owned(),
        _ => app.data.overview.device.iter().take(2).map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(" · "),
    }
}

/// The System page: a waiting Nodo update first, then the Nodo's parts.
pub fn system(app: &App, t: &Theme, selected: usize) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::System, w);
    screen.before = vec![Item::Blank, heading(t, "This Nodo"), Item::Blank];
    let mut cards = Vec::new();
    if let Some(update) = nodo_update(app) {
        let mut card = Card::new(t, w);
        card.blank(t);
        let restart = if update.note == UpdateNote::RestartNeeded { "   needs a restart" } else { "   no restart needed" };
        card.row(t, vec![t.b("A Nodo update is ready"), t.d(format!("   {}{restart}", update.to))]);
        if let UpdateNote::Withdrawal(withdrawn) = &update.note {
            card.row(t, vec![t.a("▲  "), t.n(format!("The publisher withdrew {withdrawn}, which this Nodo runs."))]);
        }
        card.blank(t);
        let mut targets = Vec::new();
        card.buttons(t, &[("Review", true, key('v'))], &mut targets);
        screen.targets.extend(targets);
        cards.push(card);
    }
    let mut parts = Card::new(t, w);
    parts.blank(t);
    for (index, row) in SYSTEM_ROWS.iter().enumerate() {
        if index > 0 {
            parts.blank(t);
        }
        if index == selected {
            parts.focus_here();
        }
        let room = parts.width.saturating_sub(6 + 20 + 2);
        let summary = row_summary(app, row);
        let summary: String = if summary.chars().count() > room { format!("{}…", summary.chars().take(room.saturating_sub(1)).collect::<String>()) } else { summary };
        let lead = if index == selected { t.a("▸ ") } else { t.n("  ") };
        parts.row(t, vec![lead, t.b(pad(row, 18)), t.d(pad(&summary, room)), t.d("›")]);
        screen.targets.push(Target::card_row(*row, pick(index, Some(KeyCode::Enter))));
    }
    parts.blank(t);
    cards.push(parts);
    screen.stack(t, cards, "Keys: ↑↓ choose  ↵ open  1-4 tabs", w, h)
}

/// One review page for the Nodo and the apps.
pub fn updates(app: &App, t: &Theme, state: &UpdatesState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::System, w);
    let mut before_targets = Vec::new();
    screen.before = system_trail(t, &["Updates"], &mut before_targets);
    screen.targets.extend(before_targets);
    let rows = app.update_rows();
    let mut cards = Vec::new();

    if let Some(prompt) = &state.passphrase {
        let mut card = Card::new(t, w);
        card.blank(t);
        card.row(t, vec![t.a("◆  "), t.b("Some apps need more locked storage")]);
        for line in &prompt.growing {
            card.row(t, vec![t.n(format!("  ·  {line}"))]);
        }
        card.blank(t);
        let mut pass = vec![t.d(pad("Passphrase", 13))];
        pass.extend(inset(t, &prompt.input.display(), 30, None, true));
        card.row(t, pass);
        card.row(t, vec![t.d("Growing locked storage needs the device passphrase. It's used for this and not kept.")]);
        card.blank(t);
        let mut targets = Vec::new();
        card.buttons(t, &[("Cancel", false, Click::Key(KeyCode::Esc)), ("Update", true, Click::Key(KeyCode::Enter))], &mut targets);
        screen.targets.extend(targets);
        return screen.card(t, card, "Keys: type the device passphrase  ↵ update  Esc cancel", w, h);
    }

    // The Nodo's own update, when there is one, is its own card with its own button.
    if let Some((index, update)) = rows.iter().enumerate().find(|(_, item)| item.name == SYSTEM_BASE) {
        let mut card = Card::new(t, w);
        card.blank(t);
        card.row(t, vec![t.b("The Nodo"), t.d(format!("   {}  →  {}", update.from, update.to))]);
        let note = match &update.note {
            UpdateNote::RestartNeeded => "Needs a restart. Apps stop safely first.".to_owned(),
            UpdateNote::Withdrawal(withdrawn) => format!("Replaces {withdrawn}, which its publisher withdrew."),
            _ => "No restart needed.".to_owned(),
        };
        card.blank(t);
        let label = if update.note == UpdateNote::RestartNeeded { "Update and restart" } else { "Update the Nodo" };
        let mut targets = Vec::new();
        card.row_with_button(t, vec![t.d(note)], label, true, Click::Pick { index, then: Some(KeyCode::Char('n')) }, &mut targets);
        card.blank(t);
        screen.targets.extend(targets);
        cards.push(card);
    }

    let apps: Vec<(usize, &&UpdateItem)> = rows.iter().enumerate().filter(|(_, item)| item.name != SYSTEM_BASE).collect();
    let mut card = Card::new(t, w);
    card.blank(t);
    card.row(t, vec![t.b("Apps")]);
    if apps.is_empty() {
        card.row(t, vec![t.d("Every app is up to date.")]);
    }
    for (index, item) in &apps {
        let selected = *index == state.selected;
        if selected {
            card.focus_here();
        }
        let ticked = state.checked.contains(&item.name);
        let note = match &item.note {
            UpdateNote::Ready if item.grows.is_some() => t.a("needs the device passphrase"),
            UpdateNote::Ready => t.d(""),
            UpdateNote::RestartNeeded => t.d("needs a restart"),
            UpdateNote::NewPermission(_) => t.a("▲ asks for something new; you'll review it first"),
            UpdateNote::Skipped => t.d("you skipped this version"),
            UpdateNote::Blocked(reason) => t.a(format!("▲ {reason}")),
            UpdateNote::Withdrawal(_) => t.a("▲ replaces a withdrawn version"),
        };
        let lead = if selected { t.a("▸ ") } else { t.n("  ") };
        let name_width = apps.iter().map(|(_, i)| i.name.chars().count()).max().unwrap_or(0).clamp(10, 20) + 2;
        let name = if item.held { t.d(pad(&item.name, name_width)) } else { t.n(pad(&item.name, name_width)) };
        let note = match (&item.note, item.held) {
            (UpdateNote::Skipped, _) => t.d("you skipped this version"),
            (_, true) => t.d("held back"),
            _ => note,
        };
        card.row(t, vec![lead, tick(t, ticked), t.n("  "), name, t.d(pad(&format!("{}  →  {}", item.from, item.to), 22)), note]);
        screen.targets.push(Target::card_row(item.name.clone(), pick(*index, Some(KeyCode::Char(' ')))));
    }
    card.blank(t);
    let count = apps.iter().filter(|(_, item)| state.checked.contains(&item.name)).count();
    let mut targets = Vec::new();
    if !apps.is_empty() {
        let label = match count {
            0 => "Update apps".to_owned(),
            1 => "Update 1 app".to_owned(),
            n => format!("Update {n} apps"),
        };
        card.row_with_button(t, vec![t.d("Each app keeps its settings and can be undone.")], &label, nodo_update(app).is_none(), Click::Key(KeyCode::Enter), &mut targets);
        card.blank(t);
    }
    screen.targets.extend(targets);
    cards.push(card);
    let mut after_targets = Vec::new();
    let mut after = vec![Item::Blank];
    let auto = match app.data.auto_update.enabled_count() {
        0 => "automatic updates are off".to_owned(),
        n => format!("automatic for {n}"),
    };
    after.push(Item::Text(ratatui::text::Line::from(vec![margin(t), t.a("Automatic updates ›"), t.d(format!("  {auto}")), t.n("     "), t.a("Undo an update ›"), t.n("     "), t.a("Check now ›")])));
    after_targets.push(Target::text("Automatic updates ›", key('a')));
    after_targets.push(Target::text("Undo an update ›", key('z')));
    after_targets.push(Target::text("Check now ›", key('r')));
    if let Some(note) = &app.data.system_note {
        after.push(Item::Blank);
        after.push(quiet(t, note));
    }
    screen.after = after;
    let mut page = screen.stack(t, cards, "Keys: ↑↓ choose  Space tick  ↵ update ticked  c changes  h hold  Esc back", w, h);
    page.targets.extend(after_targets);
    page
}

/// Automatic updates: which apps update by themselves, when, and whether the Nodo does.
pub fn auto_update(app: &App, t: &Theme, state: &AutoUpdateState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::System, w);
    let draft = &state.draft;
    let mut before_targets = Vec::new();
    let mut before = system_trail(t, &["Updates", "Automatic"], &mut before_targets);
    before.push(heading(t, "Automatic updates"));
    let next = if state.saved.next_run.is_empty() { String::new() } else { format!(" {}.", state.saved.next_run) };
    before.push(quiet(t, &format!("Only versions that ask for nothing new. Anything else waits for you.{next}")));
    before.push(Item::Blank);
    before.push(Item::Blank);
    screen.before = before;
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    // Row 0 is how often, then one row per entry (the Nodo's own last).
    if state.selected == 0 {
        card.focus_here();
    }
    let lead = |selected: bool| if selected { t.a("▸ ") } else { t.n("  ") };
    let mut when = vec![lead(state.selected == 0), t.n(pad("How often", 34))];
    when.extend(inset(t, &draft.frequency, 12, Some("◂▸"), state.selected == 0));
    card.row(t, when);
    screen.targets.push(Target::card_row("How often", pick(0, None)));
    let mut window = vec![t.n("  "), t.n(pad("Between", 34))];
    match &state.editing_window {
        Some(input) => window.extend(inset(t, input.text(), 22, None, true)),
        None => window.extend(inset(t, &format!("{} and {}", draft.window_start, draft.window_end), 22, None, false)),
    }
    card.row(t, window);
    screen.targets.push(field_target("Between", 34, 22));
    if let Some(error) = &state.error {
        card.row(t, vec![t.n(" ".repeat(36)), t.e(format!("✕ {error}"))]);
    }
    card.blank(t);
    card.row(t, vec![t.b("Update these apps by themselves")]);
    let focused = state.focused_package();
    for (index, entry) in draft.packages.iter().enumerate().filter(|(_, e)| e.name != SYSTEM_BASE) {
        let selected = focused == Some(index);
        if selected {
            card.focus_here();
        }
        let mut row = vec![lead(selected), t.n(pad(&entry.name, 34))];
        row.extend(toggle(t, entry.enabled));
        if entry.shows_warning(selected) {
            row.push(t.a("   ▲ Runs new code without your review"));
        }
        card.row(t, row);
        screen.targets.push(Target::card_row(entry.name.clone(), pick(index + 1, Some(KeyCode::Char(' ')))));
    }
    if let Some((index, entry)) = draft.packages.iter().enumerate().find(|(_, e)| e.name == SYSTEM_BASE) {
        card.blank(t);
        let selected = focused == Some(index);
        if selected {
            card.focus_here();
        }
        let mut row = vec![lead(selected), t.b(pad("Update the Nodo by itself", 34))];
        row.extend(toggle(t, entry.enabled));
        card.row(t, row);
        card.row(t, vec![t.n("  "), t.d("Updates that need a restart always wait for you, whatever this says.")]);
        screen.targets.push(Target::card_row("Update the Nodo by itself", pick(index + 1, Some(KeyCode::Char(' ')))));
    }
    card.blank(t);
    let mut targets = Vec::new();
    card.buttons(t, &[("Cancel", false, Click::Key(KeyCode::Esc)), ("Save", true, Click::Key(KeyCode::Enter))], &mut targets);
    screen.targets.extend(targets);
    let keys = if state.editing_window.is_some() { "Keys: type like 03:00 to 05:00  ↵ done  Esc cancel" } else { "Keys: ↑↓ choose  Space on/off  ←→ how often  w between  ↵ save  Esc cancel" };
    screen.card(t, card, keys, w, h)
}

/// What happened to the updates just applied, and, when one did not start, what to do.
pub fn results(app: &App, t: &Theme, state: &UpdateResultsState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::System, w);
    let report = &state.report;
    let mut before_targets = Vec::new();
    screen.before = system_trail(t, &["Updates"], &mut before_targets);
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    let mut targets = Vec::new();
    match &report.failure {
        Some(failure) => {
            card.row(t, vec![t.e("▲  "), t.b(format!("{}'s update didn't start, so {} was put back", failure.name, failure.kept_version))]);
            card.blank(t);
            let running = if failure.running { format!("{} {} is running again as before.", failure.restored, failure.name) } else { format!("{} {} isn't running; its log says why.", failure.restored, failure.name) };
            card.row(t, vec![t.n(running)]);
            if !failure.log_line.is_empty() {
                card.row(t, vec![t.d(format!("{} said: \"{}\"", failure.name, failure.log_line))]);
            }
            for result in report.results.iter().filter(|r| r.applied) {
                let name = if result.name == SYSTEM_BASE { "The Nodo" } else { result.name.as_str() };
                card.row(t, vec![t.o("✓  "), t.n(format!("{name} updated to {}", result.version))]);
            }
            card.blank(t);
            card.link_row(t, vec![t.a("Show log ›"), t.n("     "), t.a("Copy a report for the publisher ›")], "Show log ›", pick(3, Some(KeyCode::Enter)), &mut targets);
            targets.push(Target::text("Copy a report for the publisher ›", pick(2, Some(KeyCode::Enter))));
            card.blank(t);
            let again = super::v3::focused("Try again", state.choice == 1);
            let ok = super::v3::focused("OK", state.choice != 1);
            card.buttons(t, &[(again.as_str(), false, pick(1, Some(KeyCode::Enter))), (ok.as_str(), true, pick(0, Some(KeyCode::Enter)))], &mut targets);
        }
        None => {
            let applied = report.results.iter().filter(|r| r.applied).count();
            card.row(t, vec![t.o("✓  "), t.b(if applied == 1 { "1 update is installed".to_owned() } else { format!("{applied} updates are installed") })]);
            card.blank(t);
            for result in &report.results {
                let (mark, text) = if result.applied { (t.o("✓  "), t.n(format!("{} {}", result.name, result.version))) } else { (t.e("✕  "), t.e(format!("{} {}", result.name, result.version))) };
                let note = if result.note.is_empty() { String::new() } else { format!("   {}", result.note) };
                card.row(t, vec![mark, text, t.d(note)]);
            }
            card.blank(t);
            card.buttons(t, &[("Done", true, Click::Key(KeyCode::Enter))], &mut targets);
        }
    }
    screen.targets.extend(targets);
    let keys = if report.failure.is_some() { "Keys: ↑↓ choose  ↵ confirm  Esc back" } else { "Keys: ↵ done" };
    screen.card(t, card, keys, w, h)
}

/// Logs: one app's (from its page, under Apps), or the device's (under System) with a filter
/// for problems and one per source.
pub fn logs(app: &App, t: &Theme, state: &LogsState) -> Page {
    let (w, h) = super::frame_size();
    let source = app.data.log_sources.get(state.selected).cloned().unwrap_or_default();
    let mut screen = Screen3::new(app, t, if state.app.is_some() { Tab::Apps } else { Tab::System }, w);
    let mut before_targets = Vec::new();
    let mut before = match &state.app {
        Some(name) => vec![Item::Blank, trail(t, &["Apps", name.as_str(), "Logs"], &mut before_targets), Item::Blank],
        None => system_trail(t, &["Device logs"], &mut before_targets),
    };
    let mut search = vec![margin(t), t.d(pad("Search", 9))];
    search.extend(inset(t, state.find.text(), 30, None, state.finding));
    before_targets.push(field_target("Search", 9, 30));
    let follow = if state.follow { t.o("● Following live") } else { t.d("○ Paused") };
    before.push(Item::Row(ratatui::text::Line::from(search), ratatui::text::Line::from(vec![follow, t.n("     "), t.a("Save to a file ›"), t.n(super::v3::MARGIN)])));
    before_targets.push(Target::text("Save to a file ›", key('y')));
    before.push(Item::Blank);
    // The filter chips: problems only, then one per source on the device logs.
    let chip = |label: &str, on: bool| {
        if on {
            Span::styled(format!("  {label}  "), Style::new().fg(t.accent).bg(t.raised).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(format!("  {label}  "), Style::new().fg(t.fg).bg(t.field))
        }
    };
    let mut chips = vec![margin(t), chip("Problems", state.errors_only), t.n("  ")];
    before_targets.push(Target::text("Problems", key('e')));
    if state.app.is_none() {
        let mut used = 16;
        for (index, name) in app.data.log_sources.iter().enumerate() {
            used += name.chars().count() + 6;
            if used > usize::from(w).saturating_sub(8) {
                chips.push(t.d("↑↓ more"));
                break;
            }
            chips.push(chip(name, index == state.selected));
            chips.push(t.n("  "));
            before_targets.push(Target::text(name.clone(), pick(index, Some(KeyCode::F(5)))));
        }
    }
    before.push(Item::Text(ratatui::text::Line::from(chips)));
    before.push(Item::Blank);
    screen.before = before;
    screen.targets.extend(before_targets);

    let needle = state.find.text().to_lowercase();
    let lines: Vec<&LogLine> = state
        .view
        .lines
        .iter()
        .filter(|line| !state.errors_only || line.kind == LogKind::Error)
        .filter(|line| needle.is_empty() || line.text.to_lowercase().contains(&needle))
        .collect();
    let mut card = Card::new(t, w);
    card.blank(t);
    if state.app.is_none() {
        card.row(t, vec![t.b(source.clone())]);
        card.blank(t);
    }
    // The newest lines are where a failure is, so a long log shows its end.
    let room = usize::from(h).saturating_sub(20).max(3);
    let skip = lines.len().saturating_sub(room);
    for line in lines.iter().skip(skip) {
        let text = match line.kind {
            LogKind::Info => t.n(line.text.clone()),
            LogKind::Error => t.e(line.text.clone()),
            LogKind::Marker => t.a(line.text.clone()),
        };
        card.row(t, vec![text]);
    }
    if lines.is_empty() {
        let empty = if state.errors_only { "No problems in this log." } else { "Nothing in this log yet." };
        card.row(t, vec![t.d(empty)]);
    }
    card.blank(t);
    for suggestion in &state.view.suggestion {
        card.row(t, vec![t.i("▸  "), t.n(suggestion.replace('`', ""))]);
    }
    if !state.view.suggestion.is_empty() {
        card.blank(t);
    }
    let keys = if state.finding {
        "Keys: type to find  ↵ keep the filter  Esc clear"
    } else if state.app.is_some() {
        "Keys: / search  e problems  f follow  y save with secrets removed  Esc back"
    } else {
        "Keys: ↑↓ source  / search  e problems  f follow  y save  Esc back"
    };
    let _ = link;
    screen.stack(t, vec![card], keys, w, h)
}
