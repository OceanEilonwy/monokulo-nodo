//! Apps: the installed apps, each app's own page, Get apps (search, about, install review and
//! progress), unlocking, and the screens that stop an install.

use crossterm::event::KeyCode;
use nodo_api::ServiceState;

use super::home::{app_state, tagline};
use super::layout::{Item, Page};
use super::pad;
use super::v3::{focused, heading_link, inset, link, margin, progress, quiet, trail, trail_actions, Card, Screen3};
use crate::app::{App, AppPageState, AppRemoveState, CapabilityState, ReviewPurpose, ReviewState, SearchState, Tab, UnlockState, APP_SECTIONS};
use crate::hit::{Click, Target};
use crate::model::*;
use crate::theme::Theme;

fn pick(index: usize) -> Click {
    Click::Pick { index, then: Some(KeyCode::Enter) }
}

fn key(c: char) -> Click {
    Click::Key(KeyCode::Char(c))
}

/// The installed apps, one row each with what it is for and its state.
pub fn apps(app: &App, t: &Theme, selected: usize) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let mut before_targets = Vec::new();
    screen.before = vec![Item::Blank, heading_link(t, t.b("Installed"), "Get apps ›", key('g'), &mut before_targets), Item::Blank];
    screen.targets.extend(before_targets);
    let names = crate::update::installed_apps(app);
    let mut card = Card::new(t, w);
    card.blank(t);
    let name_width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0).clamp(10, 20) + 2;
    let what_width = usize::from(w).saturating_sub(56 + name_width).clamp(16, 34);
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            card.blank(t);
        }
        if index == selected {
            card.focus_here();
        }
        let has_update = app.data.updates.iter().any(|u| &u.name == name && !u.held);
        let (state, bad) = app_state(app, name);
        let status = if has_update {
            t.a(pad("Update ready", 16))
        } else if bad {
            t.e(pad(&state, 16))
        } else {
            t.d(pad(&truncate(&state, 15), 16))
        };
        let lead = if index == selected { t.a("▸ ") } else { t.n("  ") };
        let what = truncate(&tagline(app, name), what_width - 2);
        card.row(t, vec![lead, t.b(pad(name, name_width)), t.n(pad(&what, what_width)), status, t.d("›")]);
        screen.targets.push(Target::card_row(name.clone(), pick(index)));
    }
    if names.is_empty() {
        card.row(t, vec![t.d("No apps yet. Get apps finds them in your sources.")]);
    }
    card.blank(t);
    screen.stack(t, vec![card], "Click an app to open it.   Keys: ↑↓ choose  ↵ open  g get apps", w, h)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// One app's page: its state and update, then Hosting, Settings and Logs. Restart, Stop and
/// Remove sit on the breadcrumb row (folded into "⋯ More" under 100 columns).
pub fn app_page(app: &App, t: &Theme, state: &AppPageState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let name = state.name.as_str();
    let running = app.data.services.iter().any(|svc| svc.id == name && svc.state == ServiceState::Running);
    let stop_label = if running { "Stop" } else { "Start" };
    let mut before_targets = Vec::new();
    let trail_row = trail_actions(
        t,
        &["Apps", name],
        &[("Restart", key('r')), (stop_label, key('s')), ("!Remove…", key('x'))],
        key('m'),
        w,
        &mut before_targets,
    );
    screen.before = vec![Item::Blank, trail_row, Item::Blank];
    screen.targets.extend(before_targets);
    if state.more_open {
        let mut card = Card::new(t, w);
        card.blank(t);
        card.row(t, vec![t.b("More for "), t.b(name.to_owned())]);
        card.blank(t);
        let mut targets = Vec::new();
        card.link_row(t, vec![t.a("Restart"), t.d("   r")], "Restart", key('r'), &mut targets);
        card.link_row(t, vec![t.a(stop_label.to_owned()), t.d("   s")], stop_label, key('s'), &mut targets);
        card.link_row(t, vec![t.e("Remove…"), t.d("   x")], "Remove…", key('x'), &mut targets);
        card.blank(t);
        screen.targets.extend(targets);
        return screen.card(t, card, "Keys: r restart  s stop  x remove  Esc close", w, h);
    }

    let mut status = Card::new(t, w);
    status.blank(t);
    status.row(t, vec![t.b(name.to_owned()), t.d(format!("   {}", tagline(app, name)))]);
    let (words, bad) = app_state(app, name);
    let site = app.data.web.sites.iter().find(|s| s.package == name);
    let mut line = vec![if bad { t.e("●  ") } else if running { t.o("●  ") } else { t.d("○  ") }, if bad { t.e(words) } else { t.n(words) }];
    if let Some(url) = site.and_then(|s| s.public_host.clone().map(|h| format!("https://{h}")).or_else(|| (!s.local_url.is_empty() && s.exposure.local).then(|| s.local_url.clone()))) {
        line.push(t.n(format!("   ·   open at {url}")));
    }
    status.row(t, line);
    status.blank(t);
    let mut targets = Vec::new();
    match app.data.updates.iter().find(|u| u.name == name && !u.held) {
        Some(update) => status.row_with_button(t, vec![t.a(format!("Version {} is ready", update.to)), t.d(format!("   from {}", update.from))], "Update", true, key('u'), &mut targets),
        None => status.row(t, vec![t.d("Up to date")]),
    }
    status.blank(t);
    screen.targets.extend(targets);

    let mut sections = Card::new(t, w);
    sections.blank(t);
    let summaries = [hosting_summary(app, name), "Change how it works".to_owned(), log_summary(app, name)];
    for (index, (section, summary)) in APP_SECTIONS.iter().zip(summaries).enumerate() {
        if index > 0 {
            sections.blank(t);
        }
        if index == state.selected {
            sections.focus_here();
        }
        let lead = if index == state.selected { t.a("▸ ") } else { t.n("  ") };
        sections.row(t, vec![lead, t.b(pad(section, 12)), t.d(pad(&summary, 40)), t.d("›")]);
        screen.targets.push(Target::card_row(*section, pick(index)));
    }
    sections.blank(t);
    screen.stack(t, vec![status, sections], "Keys: ↑↓ choose  ↵ open  u update  r restart  Esc back", w, h)
}

fn hosting_summary(app: &App, name: &str) -> String {
    let Some(site) = app.data.web.sites.iter().find(|s| s.package == name) else {
        return "No web page".to_owned();
    };
    let mut parts = Vec::new();
    if site.exposure.local {
        parts.push("Home network");
    }
    if site.exposure.onion {
        parts.push("Tor");
    }
    if site.exposure.public {
        parts.push("internet");
    }
    if parts.is_empty() {
        return "Not online".to_owned();
    }
    let joined = parts.join(", ");
    let mut chars = joined.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn log_summary(app: &App, name: &str) -> String {
    match app.data.services.iter().find(|svc| svc.id == name).map(|svc| svc.state) {
        Some(ServiceState::Failed) => "It keeps stopping; the log says why".to_owned(),
        _ => "What it has been doing".to_owned(),
    }
}

/// Removing an app asks first, and says what goes and what stays.
pub fn app_remove(app: &App, t: &Theme, state: &AppRemoveState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let name = state.name.as_str();
    let mut before_targets = Vec::new();
    screen.before = vec![Item::Blank, trail(t, &["Apps", name, "Remove"], &mut before_targets), Item::Blank];
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    card.row(t, vec![t.e("▲  "), t.b(format!("Remove {name}?"))]);
    card.blank(t);
    card.row(t, vec![t.n(format!("{name} stops and its program is removed. Anything that relies on it stops working."))]);
    card.row(t, vec![t.d("Its data stays on the drive, so installing it again brings everything back. A site still online has to be taken offline first, in Hosting.")]);
    card.blank(t);
    let mut targets = Vec::new();
    let cancel = focused("Cancel", !state.confirm_focused);
    let confirm = focused(&format!("!Remove {name}"), state.confirm_focused);
    card.buttons(t, &[(cancel.as_str(), false, pick(0)), (confirm.as_str(), true, pick(1))], &mut targets);
    screen.targets.extend(targets);
    let keys = if state.confirm_focused { "Keys: ↵ remove  ←→ choose  Esc cancel" } else { "Keys: ←→ choose  Esc cancel  (Enter cancels until you move to Remove)" };
    screen.card(t, card, keys, w, h)
}

/// What an empty result list says: a search that matched nothing, or why the catalog itself is
/// empty (no sources, sources still being read, or a source that could not be read).
pub fn empty_catalog_line(app: &App, state: &SearchState) -> String {
    if !app.data.catalog.is_empty() || !state.query.text().is_empty() {
        return "Nothing matches that search.".to_owned();
    }
    let enabled: Vec<_> = app.data.feeds.iter().filter(|f| f.status != FeedStatus::Disabled).collect();
    if let Some((name, problem)) = enabled.iter().find_map(|f| match &f.status {
        FeedStatus::Unreachable(problem) => Some((&f.name, problem)),
        _ => None,
    }) {
        return format!("Could not read the source {name}: {problem}");
    }
    if enabled.iter().any(|f| f.checked.is_empty()) {
        return "Checking sources…".to_owned();
    }
    if enabled.is_empty() {
        return "No sources are on. Add or turn one on under Sources.".to_owned();
    }
    "The sources offer no apps yet.".to_owned()
}

/// Apps › Get apps: a search box, the results, and Sources.
pub fn search(app: &App, t: &Theme, state: &SearchState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let results = app.search_results(state);
    let mut before_targets = Vec::new();
    let trail_row = trail(t, &["Apps", "Get apps"], &mut before_targets);
    let mut search = vec![margin(t), t.d(pad("Search", 9))];
    search.extend(inset(t, state.query.text(), 40, None, !state.list_focused));
    before_targets.push(Target::text("Search", Click::Key(KeyCode::Char('/'))));
    let search_row = heading_link(t, t.n(""), "Sources ›", Click::Key(KeyCode::Char('s')), &mut Vec::new());
    let Item::Row(_, right) = search_row else { unreachable!() };
    before_targets.push(Target::text("Sources ›", Click::Key(KeyCode::Char('s'))));
    let scope = state.feed_filter.as_deref().map_or_else(String::new, |feed| format!(" from {feed}"));
    let count = quiet(t, &format!("{} of {} apps{scope} match. Click one to see what it does before installing.", results.len(), app.data.catalog.len()));
    screen.before = vec![Item::Blank, trail_row, Item::Blank, Item::Row(ratatui::text::Line::from(search), right), Item::Blank, count, Item::Blank];
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    let name_width = results.iter().map(|pkg| pkg.id.name.chars().count()).max().unwrap_or(0).clamp(10, 20) + 2;
    let what_width = usize::from(w).saturating_sub(35 + name_width).clamp(20, 44);
    for (index, pkg) in results.iter().enumerate() {
        if index > 0 {
            card.blank(t);
        }
        let selected = state.list_focused && index == state.selected;
        if selected {
            card.focus_here();
        }
        let tag = match pkg.status {
            PackageStatus::Installed => "Installed",
            PackageStatus::UpdateReady => "Update ready",
            PackageStatus::Available => "",
        };
        let lead = if selected { t.a("▸ ") } else { t.n("  ") };
        card.row(t, vec![lead, t.b(pad(&pkg.id.name, name_width)), t.n(pad(&truncate(&pkg.tagline, what_width - 2), what_width)), t.d(tag.to_owned())]);
        screen.targets.push(Target::card_row(pkg.id.name.clone(), Click::Pick { index, then: Some(KeyCode::Enter) }));
    }
    if results.is_empty() {
        card.row(t, vec![t.d(empty_catalog_line(app, state))]);
    }
    card.blank(t);
    let keys = if state.list_focused { "Keys: ↑↓ choose  ↵ open  i install  s sources  Esc back" } else { "Type to search.   Keys: ↓ results  ↵ open  Esc back" };
    screen.stack(t, vec![card], keys, w, h)
}

/// An app before installing (or an installed one from Get apps): what it does and what it needs.
pub fn detail(app: &App, t: &Theme, detail: &PackageDetail) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let name = detail.id.name.as_str();
    let mut before_targets = Vec::new();
    screen.before = vec![Item::Blank, trail(t, &["Apps", "Get apps", name], &mut before_targets), Item::Blank];
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    let version = if detail.version.is_empty() { String::new() } else { format!("Version {}", detail.version) };
    let facts: Vec<String> = [version, format!("from {}", detail.id.feed), detail.size.clone()].into_iter().filter(|p| !p.is_empty()).collect();
    card.row(t, vec![t.b(name.to_owned()), t.d(format!("   {}", facts.join(" · ")))]);
    card.blank(t);
    if !detail.about.is_empty() {
        card.row(t, vec![t.n(detail.about.clone())]);
        card.blank(t);
    }
    let mut asks: Vec<String> = detail.needs.iter().map(|need| format!("Use {}", need.name)).collect();
    asks.extend(detail.asks.iter().map(|(label, value)| format!("{label}: {value}")));
    if detail.encrypted_storage {
        asks.push("Keep its data locked; it needs the device passphrase after a restart".to_owned());
    }
    if !asks.is_empty() {
        card.row(t, vec![t.d("It will:")]);
        for ask in asks {
            card.row(t, vec![t.n(format!("  ·  {ask}"))]);
        }
        card.blank(t);
    }
    let signature = match (detail.signature_verified, detail.installed) {
        (true, _) => t.o("✓ Its source's signature checks out"),
        (false, false) => t.d("Its source's signature is checked when it downloads"),
        (false, true) => t.e("✕ Its signature could not be checked"),
    };
    card.row(t, vec![signature]);
    card.blank(t);
    let mut targets = Vec::new();
    let keys = if detail.installed {
        card.buttons(t, &[("Open its page", true, key('o'))], &mut targets);
        "Keys: o open its page  c changes  Esc back"
    } else {
        card.buttons(t, &[("Install", true, key('i'))], &mut targets);
        "Keys: i install  c changes  v versions  Esc back"
    };
    screen.targets.extend(targets);
    screen.card(t, card, keys, w, h)
}

/// The install review: what the app may do, in plain words, and the one button.
pub fn review(app: &App, t: &Theme, state: &ReviewState) -> Page {
    let (w, h) = super::frame_size();
    let data = &state.data;
    let is_update = matches!(state.purpose, ReviewPurpose::Update { .. });
    let mut screen = Screen3::new(app, t, if is_update { Tab::System } else { Tab::Apps }, w);
    let name = data.id.name.as_str();
    let mut before_targets = Vec::new();
    let parts: Vec<&str> = if is_update { vec!["System", "Updates", name] } else { vec!["Apps", "Get apps", name, "Install"] };
    screen.before = vec![Item::Blank, trail(t, &parts, &mut before_targets), Item::Blank];
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    card.row(t, vec![t.b(if is_update { format!("Update {name} to {}?", data.version) } else { format!("Install {name}?") })]);
    card.blank(t);
    if data.third_party {
        card.row(t, vec![t.a("▲  "), t.n(format!("It comes from {}, a source you added. Nobody has reviewed its code for you.", data.id.feed))]);
        card.blank(t);
    }
    card.row(t, vec![t.n(if is_update { "This version newly asks to:" } else { "It will be able to:" })]);
    for ask in &data.asks {
        let mark = match ask.level {
            AskLevel::Normal => t.o("✓  "),
            AskLevel::Caution => t.a("▲  "),
            AskLevel::Encrypted => t.a("◆  "),
        };
        let note = if ask.note.is_empty() { String::new() } else { format!("   {}", ask.note) };
        card.row(t, vec![mark, t.n(ask.text.clone()), t.d(note)]);
    }
    card.blank(t);
    for guarantee in &data.guarantees {
        card.row(t, vec![t.d(guarantee.clone())]);
    }
    if !data.guarantees.is_empty() {
        card.blank(t);
    }
    let mut targets = Vec::new();
    if let Some(input) = &state.passphrase {
        let mut pass = vec![t.d(pad("Passphrase", 13))];
        pass.extend(inset(t, &input.display(), 30, None, true));
        card.row(t, pass);
        card.row(t, vec![t.d("Its locked storage is made now and opens with your device passphrase.")]);
        card.blank(t);
        let verb = if is_update { "Update" } else { "Install" };
        card.buttons(t, &[("Cancel", false, Click::Key(KeyCode::Esc)), (verb, true, Click::Key(KeyCode::Enter))], &mut targets);
        screen.targets.extend(targets);
        return screen.card(t, card, "Keys: type the passphrase  ↵ install  Esc back", w, h);
    }
    let cancel = focused("Cancel", !state.confirm_focused);
    let verb = focused(if is_update { "Update" } else { "Install" }, state.confirm_focused);
    card.buttons(t, &[(cancel.as_str(), false, pick(0)), (verb.as_str(), true, pick(1))], &mut targets);
    screen.targets.extend(targets);
    let keys = if state.confirm_focused { "Keys: ↵ confirm  ←→ choose  d files  Esc cancel" } else { "Keys: ←→ choose  ↵ cancel  d files  Esc cancel" };
    screen.card(t, card, keys, w, h)
}

/// Install progress in plain steps, with a solid bar.
pub fn installing(app: &App, t: &Theme, snapshot: &InstallSnapshot) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let name = snapshot.id.name.as_str();
    let mut before_targets = Vec::new();
    screen.before = vec![Item::Blank, trail(t, &["Apps", name], &mut before_targets), Item::Blank];
    screen.targets.extend(before_targets);
    let total = snapshot.steps.len().max(1);
    let done = snapshot.steps.iter().filter(|step| step.state == StepState::Done).count();
    let mut card = Card::new(t, w);
    card.blank(t);
    let title = match &snapshot.outcome {
        InstallOutcome::InProgress => format!("Installing {name}"),
        InstallOutcome::Succeeded => format!("{name} is installed"),
        InstallOutcome::Failed(_) => format!("{name} wasn't installed"),
        InstallOutcome::Cancelled => "Install cancelled".to_owned(),
    };
    let step = if snapshot.outcome == InstallOutcome::InProgress { format!("   Step {} of {}", (done + 1).min(total), snapshot.steps.len()) } else { String::new() };
    card.row(t, vec![t.b(title), t.d(step)]);
    card.blank(t);
    let fraction = done as f32 / total as f32;
    let mut bar = progress(t, fraction, 50.min(card.width.saturating_sub(16)));
    bar.push(t.d(format!("   {}%", (fraction * 100.0).round())));
    card.row(t, bar);
    card.blank(t);
    for step in &snapshot.steps {
        let (mark, text) = match step.state {
            StepState::Done => (t.o("✓  "), t.n(step.name.clone())),
            StepState::Working => (t.a("›  "), t.n(step.name.clone())),
            StepState::Pending => (t.d("○  "), t.d(step.name.clone())),
            StepState::Failed => (t.e("✕  "), t.e(step.name.clone())),
        };
        card.row(t, vec![mark, text]);
    }
    card.blank(t);
    let mut targets = Vec::new();
    let keys = match &snapshot.outcome {
        InstallOutcome::InProgress => {
            card.row(t, vec![t.d("You can leave this page; it carries on. A failed step removes what this install added.")]);
            card.blank(t);
            "Keys: Esc cancel  v full log"
        }
        InstallOutcome::Succeeded => {
            card.buttons(t, &[("Done", false, Click::Key(KeyCode::Esc)), ("Set it up", true, Click::Key(KeyCode::Enter))], &mut targets);
            "Keys: ↵ set it up  v full log  Esc done"
        }
        InstallOutcome::Failed(reason) => {
            card.row(t, vec![t.e(format!("✕ {reason}"))]);
            card.blank(t);
            card.buttons(t, &[("Back", true, Click::Key(KeyCode::Esc))], &mut targets);
            "Keys: v full log  Esc back"
        }
        InstallOutcome::Cancelled => {
            card.buttons(t, &[("Back", true, Click::Key(KeyCode::Esc))], &mut targets);
            "Keys: Esc back"
        }
    };
    screen.targets.extend(targets);
    screen.card(t, card, keys, w, h)
}

/// Unlocking after a restart: one card with the passphrase box and Unlock inside it.
pub fn unlock(app: &App, t: &Theme, state: &UnlockState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Home, w);
    screen.before = vec![Item::Blank, Item::Blank];
    let overview = &app.data.overview;
    let mut card = Card::new(t, w);
    card.blank(t);
    card.row(t, vec![t.b("Unlock your Nodo")]);
    card.blank(t);
    let restarted = if overview.restarted.is_empty() { "The Nodo restarted.".to_owned() } else { format!("{}.", overview.restarted.trim_end_matches('.')) };
    card.row(t, vec![t.n(format!("{restarted} Your apps' data is locked until you type the device passphrase."))]);
    let waiting: Vec<&str> = overview.unlock_targets.iter().filter(|target| target.encrypted).map(|target| target.name.as_str()).collect();
    if !waiting.is_empty() {
        card.row(t, vec![t.d(format!("{} start after you unlock.", waiting.join(", ")))]);
    }
    card.blank(t);
    let mut pass = vec![t.d(pad("Passphrase", 13))];
    pass.extend(inset(t, &state.input.display(), 30, None, true));
    card.row(t, pass);
    if let Some(error) = &state.error {
        card.row(t, vec![t.n(" ".repeat(13)), t.e(format!("✕ {error}"))]);
    }
    card.blank(t);
    card.row(t, vec![t.d("A lost passphrase can't be recovered; a backup brings the data back.")]);
    card.blank(t);
    let mut targets = Vec::new();
    card.buttons(t, &[("Unlock", true, Click::Key(KeyCode::Enter))], &mut targets);
    screen.targets.extend(targets);
    screen.card(t, card, "Keys: type the passphrase  ↵ unlock  Esc leave locked", w, h)
}

/// The shared look of a screen that stops something: a plain reason and one way forward.
#[allow(clippy::too_many_arguments)]
fn blocked(app: &App, t: &Theme, parts: &[&str], title: &str, body: &[String], details: &[String], buttons: &[(&str, bool, Click)], keys: &str) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let mut before_targets = Vec::new();
    screen.before = vec![Item::Blank, trail(t, parts, &mut before_targets), Item::Blank];
    screen.targets.extend(before_targets);
    let mut card = Card::new(t, w);
    card.blank(t);
    card.row(t, vec![t.e("▲  "), t.b(title.to_owned())]);
    card.blank(t);
    for line in body {
        card.row(t, vec![t.n(line.clone())]);
    }
    if !details.is_empty() {
        card.blank(t);
        for line in details {
            card.row(t, vec![t.d(line.clone())]);
        }
    }
    card.blank(t);
    let mut targets = Vec::new();
    card.buttons(t, buttons, &mut targets);
    screen.targets.extend(targets);
    screen.card(t, card, keys, w, h)
}

pub fn rejected(app: &App, t: &Theme, data: &RejectionData) -> Page {
    let name = data.id.name.as_str();
    let broken: Vec<String> = data.violations.iter().filter(|v| !v.passed).map(|v| format!("✕ {} — {}", v.rule, v.evidence)).collect();
    blocked(
        app,
        t,
        &["Apps", "Get apps", name],
        &format!("{name} can't be installed"),
        &[format!("Its source signed it, but it breaks the Nodo's rules, and there's no override. Nothing was installed. Tell the publisher of {} if you think this is a mistake.", data.id.feed)],
        &broken,
        &[("Copy report", false, key('y')), ("OK", true, Click::Key(KeyCode::Esc))],
        "Keys: Esc OK  y copy report  f open its source",
    )
}

pub fn capability(app: &App, t: &Theme, state: &CapabilityState) -> Page {
    let data = &state.data;
    let name = data.id.name.as_str();
    let mut details = vec![data.message.clone()];
    if !data.switch_note.is_empty() {
        details.push(data.switch_note.clone());
    }
    let install = format!("Install {}", data.alternative_node);
    blocked(
        app,
        t,
        &["Apps", "Get apps", name],
        &format!("{name} needs a different Monero node first"),
        &[format!("It needs something {} doesn't provide. {} does; it syncs the chain, then {name} can be installed.", data.current_node, data.alternative_node)],
        &details,
        &[("Cancel", false, pick(0)), (install.as_str(), true, pick(2))],
        &format!("Keys: ↵ install {}  Esc cancel", data.alternative_node),
    )
}

/// The link under a card that leads somewhere else.
#[allow(dead_code)]
pub(crate) fn link_after(t: &Theme, text: &str, click: Click, targets: &mut Vec<Target>) -> Vec<Item> {
    vec![Item::Blank, link(t, text, click, targets)]
}
