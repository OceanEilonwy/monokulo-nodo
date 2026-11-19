//! The pure state machine: `update(state, event)` applies one event and lists the I/O it needs.
//!
//! Nothing here touches a terminal or a backend. Safety rules that a test must be able to see,
//! such as safe default focus and typed confirmation, live in this file.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nodo_api::ServiceState;

use crate::app::*;
use nodo_ui_core::model::*;
use nodo_ui_core::secret::Input;
use nodo_ui_core::{Effect, Outcome};
use nodo_ui_core::flows::setup::{Setup, MIN_PASSPHRASE_CHARS};
use nodo_ui_core::flows::install::{self, Search};
use nodo_ui_core::flows::logs::{self, Logs};
use nodo_ui_core::flows::settings::{self, PickMode, Picked};
use nodo_ui_core::flows::{self, documents, drive, services, sources, unlock, Confirm, Dest, Notice, NoticeKind, Question, Step};
use nodo_ui_core::queries::{HomeItem, APP_SECTIONS};
use nodo_ui_core::screen::{ScreenId, Tab};

#[path = "update_more.rs"]
mod more;
#[path = "update_net.rs"]
mod net;
#[path = "update_owner.rs"]
mod owner;
#[path = "update_web.rs"]
mod web;

pub fn update(app: &mut App, event: Event) -> Vec<Effect> {
    let before = app.screen.id();
    let input = matches!(event, Event::Key(_) | Event::Mouse { .. } | Event::Paste(_) | Event::Tick | Event::Resize(..));
    let effects = match event {
        Event::Key(key) => pressed(app, key),
        Event::Mouse { kind, column, row } => on_mouse(app, kind, column, row),
        Event::Paste(text) => {
            on_paste(app, &text);
            Vec::new()
        }
        Event::Tick => on_tick(app),
        Event::Resize(width, height) => {
            app.size = (width, height);
            Vec::new()
        }
        Event::Done(outcome) => on_result(app, outcome),
    };
    // A result ends the work a button started, and a button on another screen is not that one.
    // The runtime also clears it whenever the worker is idle, so it can never stay stuck.
    if !input || app.screen.id() != before {
        app.pending = None;
    }
    effects
}

/// The pending button on the current screen, while no modal covers it.
fn pending_here(app: &App) -> Option<&crate::hit::Click> {
    app.pending.as_ref().filter(|p| p.screen == app.screen.id() && app.modal.is_none()).map(|p| &p.click)
}

/// Whether a key does what a button's click does.
fn presses(click: &crate::hit::Click, key: KeyEvent) -> bool {
    use crate::hit::Click;
    match click {
        Click::Key(code) => key.code == *code && !key.modifiers.contains(KeyModifiers::CONTROL),
        Click::Ctrl(c) => is_ctrl(key, *c),
        Click::Pick { then: Some(code), .. } => key.code == *code && !key.modifiers.contains(KeyModifiers::CONTROL),
        Click::Pick { then: None, .. } | Click::Tab(_) | Click::Focus => false,
    }
}

/// A key from the keyboard. While a button's work runs, its key is ignored. A key that starts
/// work on a button drawn on this screen (its own key, or Enter on the focused one) marks that
/// button pending, as a click on it would.
fn pressed(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    if pending_here(app).is_some_and(|click| presses(click, key)) {
        return Vec::new();
    }
    let before = app.screen.id();
    let modal = app.modal.is_some();
    let effects = on_key(app, key);
    if effects.is_empty() || modal || app.modal.is_some() || app.screen.id() != before {
        return effects;
    }
    let button = {
        let hits = app.hits.borrow();
        let buttons = || hits.iter().filter(|hit| hit.button.is_some());
        buttons()
            .find(|hit| matches!(hit.click, crate::hit::Click::Key(_) | crate::hit::Click::Ctrl(_)) && presses(&hit.click, key))
            .or_else(|| {
                buttons().find(|hit| {
                    matches!(hit.click, crate::hit::Click::Pick { .. })
                        && presses(&hit.click, key)
                        && hit.button.as_deref().is_some_and(|label| label.starts_with('▸'))
                })
            })
            .map(|hit| hit.click.clone())
    };
    if let Some(click) = button {
        app.pending = Some(Pending { click, screen: before });
    }
    effects
}

fn notice(app: &mut App, kind: NoticeKind, text: impl Into<String>) {
    app.notice = Some(Notice { kind, text: text.into() });
}

fn fail(app: &mut App, text: impl Into<String>) {
    notice(app, NoticeKind::Error, text);
}

fn push(app: &mut App, screen: Screen) {
    let previous = std::mem::replace(&mut app.screen, screen);
    app.stack.push(previous);
}

fn pop(app: &mut App) {
    app.screen = app.stack.pop().unwrap_or(Screen::Home { selected: 0 });
}

fn step(selected: &mut usize, len: usize, down: bool) {
    if down {
        if *selected + 1 < len {
            *selected += 1;
        }
    } else {
        *selected = selected.saturating_sub(1);
    }
}

fn home_screen(app: &App) -> Screen {
    if app.data.overview.drive.is_some() {
        Screen::NoDrive { choice: 0 }
    } else {
        Screen::Home { selected: 0 }
    }
}

fn updates_screen(app: &App) -> Screen {
    Screen::Updates(UpdatesState {
        selected: 0,
        checked: app
            .update_rows()
            .iter()
            .filter(|item| item.preselected && !item.held)
            .map(|item| item.name.clone())
            .collect(),
        passphrase: None,
    })
}

/// Applies a batch of updates. When one of them grows encrypted storage, the Updates screen
/// first asks for the device passphrase, which LUKS needs to resize the volume.
fn apply_batch(app: &mut App, names: Vec<String>, approvals: Vec<(String, nodo_api::PermissionApproval)>) -> Vec<Effect> {
    let growing: Vec<String> = app
        .data
        .updates
        .iter()
        .filter(|item| names.contains(&item.name))
        .filter_map(|item| item.grows.as_ref().map(|grows| format!("{}: {grows}", item.name)))
        .collect();
    if growing.is_empty() {
        return vec![Effect::ApplyUpdates { names, approvals }];
    }
    if matches!(app.screen, Screen::Review(_) | Screen::UpdateResults(_)) {
        pop(app);
    }
    let Screen::Updates(state) = &mut app.screen else {
        // The helper refuses a growing update without the passphrase, and says why.
        return vec![Effect::ApplyUpdates { names, approvals }];
    };
    state.passphrase = Some(UpdatePassphrase { names, approvals, growing, input: Input::secret() });
    Vec::new()
}

pub fn search_state(focus_query: bool) -> SearchState {
    SearchState { flow: Search::default(), query: Input::plain(""), list_focused: !focus_query, selected: 0 }
}

fn switch_tab(app: &mut App, tab: Tab) -> Vec<Effect> {
    app.stack.clear();
    app.screen = match tab {
        Tab::Home => home_screen(app),
        Tab::Apps => Screen::Apps { selected: 0 },
        Tab::Web => Screen::Web { selected: 0 },
        Tab::System => Screen::System { selected: 0 },
    };
    Vec::new()
}

/// One source's log: an app's from its page (under Apps), or a device part's (under System).
fn open_logs(app: &mut App, source: &str) -> Vec<Effect> {
    let selected = app.data.log_sources.iter().position(|name| name == source).unwrap_or(0);
    let state = LogsState::new(Logs::of(&app.data, source), selected);
    push(app, Screen::Logs(state));
    app.data.log_sources.get(selected).map(|name| Logs::load(name)).unwrap_or_default()
}

/// The device logs under System: every source, problems first.
fn open_device_logs(app: &mut App) -> Vec<Effect> {
    push(app, Screen::Logs(LogsState::new(Logs::device(), 0)));
    app.data.log_sources.first().map(|name| Logs::load(name)).unwrap_or_default()
}

/// Apps › Get apps: searching what the sources offer.
fn open_get_apps(app: &mut App, feed_filter: Option<String>) -> Vec<Effect> {
    switch_tab(app, Tab::Apps);
    let mut state = search_state(feed_filter.is_none());
    state.list_focused = feed_filter.is_some();
    state.feed_filter = feed_filter;
    push(app, Screen::Search(state));
    Vec::new()
}

/// Apps › Get apps › Sources.
fn open_sources(app: &mut App, selected: usize) -> Vec<Effect> {
    if !matches!(app.screen, Screen::Search(_)) {
        open_get_apps(app, None);
    }
    push(app, Screen::Feeds { selected, keys: false });
    Vec::new()
}

/// System › Updates.
fn open_updates(app: &mut App) -> Vec<Effect> {
    if !matches!(app.screen, Screen::System { .. }) {
        switch_tab(app, Tab::System);
    }
    let screen = updates_screen(app);
    push(app, screen);
    Vec::new()
}

fn cycle_tab(app: &mut App, forward: bool) -> Vec<Effect> {
    let Some(current) = app.screen.tab() else {
        return Vec::new();
    };
    let index = Tab::ALL.iter().position(|tab| *tab == current).unwrap_or(0);
    let count = Tab::ALL.len();
    let next = if forward { (index + 1) % count } else { (index + count - 1) % count };
    switch_tab(app, Tab::ALL[next])
}

fn help_document() -> Document {
    Document {
        title: "Help".to_owned(),
        lines: [
            "1 2 3 4          Home, Apps, Web, System",
            "Tab / Shift-Tab  next or previous tab",
            "↑↓               move",
            "↵                open or confirm",
            "Esc              back or cancel",
            "/                get apps",
            "u                unlock this device",
            "k                SSH keys that may log in",
            "b                backups, from Home",
            "r                undo an update, from Home",
            "w                Wi-Fi and Bluetooth, from Home",
            "n                onion (Tor) addresses, from Web",
            "q                quit, from a tab",
            "F4 or Ctrl-.     pick a setting's value from another app, in Settings",
            "",
            "The mouse works too: click, scroll, and hover to see what can be clicked.",
            "Ctrl-C           quit, from anywhere",
            "",
            "Choices that could harm the device start on the safe answer.",
        ]
        .map(str::to_owned)
        .to_vec(),
    }
}

/// Keys every tab shares. Returns `None` when the key is not one of them.
fn tab_key(app: &mut App, key: KeyEvent) -> Option<Vec<Effect>> {
    match key.code {
        KeyCode::Tab => Some(cycle_tab(app, true)),
        KeyCode::BackTab => Some(cycle_tab(app, false)),
        KeyCode::Char(c @ '1'..='4') => Some(switch_tab(app, Tab::ALL[usize::from(c as u8 - b'1')])),
        KeyCode::Char('q') => {
            app.should_quit = true;
            Some(Vec::new())
        }
        KeyCode::Char('?') => {
            app.overlay = Some(Overlay { doc: help_document(), scroll: 0 });
            Some(Vec::new())
        }
        _ => None,
    }
}

fn is_ctrl(key: KeyEvent, c: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(c)
}

fn on_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    if is_ctrl(key, 'c') {
        app.should_quit = true;
        return Vec::new();
    }
    // The screen cannot show what a key would act on, so nothing but quitting is allowed.
    if app.too_small() {
        if key.code == KeyCode::Char('q') {
            app.should_quit = true;
        }
        return Vec::new();
    }
    app.notice = None;
    if app.modal.is_some() {
        return modal_key(app, key);
    }
    if app.overlay.is_some() {
        overlay_key(app, key);
        return Vec::new();
    }
    match app.screen.id() {
        ScreenId::Main => home_key(app, key),
        ScreenId::NoDrive => no_drive_key(app, key),
        ScreenId::Search => search_key(app, key),
        ScreenId::Detail => detail_key(app, key),
        ScreenId::Review => review_key(app, key),
        ScreenId::Installing => installing_key(app, key),
        ScreenId::Unlock => unlock_key(app, key),
        ScreenId::Feeds => feeds_key(app, key),
        ScreenId::AddFeed => add_feed_key(app, key),
        ScreenId::FeedKey => feed_key_key(app, key),
        ScreenId::Settings => settings_key(app, key),
        ScreenId::Reference => reference_key(app, key),
        ScreenId::ApplyFailed => apply_failed_key(app, key),
        ScreenId::Updates => updates_key(app, key),
        ScreenId::AutoUpdate => auto_update_key(app, key),
        ScreenId::UpdateFailed => update_results_key(app, key),
        ScreenId::Logs => logs_key(app, key),
        ScreenId::Rejected => rejected_key(app, key),
        ScreenId::Capability => capability_key(app, key),
        ScreenId::WebSites => web::web_key(app, key),
        ScreenId::WebExposure => web::hosting_key(app, key),
        ScreenId::WebProviders => web::providers_key(app, key),
        ScreenId::WebCheck => web::web_check_key(app, key),
        ScreenId::WebConfirm => web::web_confirm_key(app, key),
        ScreenId::WebCgnat => web::cgnat_key(app, key),
        ScreenId::WebDomain => domain_key(app, key),
        ScreenId::WebCertificate => certificate_key(app, key),
        ScreenId::WebTrust => trust_key(app, key),
        ScreenId::Setup => setup_key(app, key),
        ScreenId::Backup => backup_key(app, key),
        ScreenId::Onion => more::onion_key(app, key),
        ScreenId::Restore => more::restore_key(app, key),
        ScreenId::Rollbacks => more::rollbacks_key(app, key),
        ScreenId::Network => net::network_key(app, key),
        ScreenId::Apps => apps_key(app, key),
        ScreenId::AppPage => app_page_key(app, key),
        ScreenId::AppRemove => app_remove_key(app, key),
        ScreenId::System => system_key(app, key),
        ScreenId::Bundle => owner::bundle_key(app, key),
        ScreenId::OwnerPage => owner::owner_key(app, key),
    }
}

fn apps_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    if let Some(effects) = tab_key(app, key) {
        return effects;
    }
    let names = services::installed_apps(&app.data);
    let Screen::Apps { selected } = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Down | KeyCode::Up => step(selected, names.len(), key.code == KeyCode::Down),
        KeyCode::Enter => {
            if let Some(name) = names.get(*selected) {
                push(app, Screen::AppPage(AppPageState::new(name)));
            }
        }
        KeyCode::Char('g' | '/') => return open_get_apps(app, None),
        KeyCode::Char('u') => return open_unlock(app),
        _ => {}
    }
    Vec::new()
}

fn app_page_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::AppPage(state) = &mut app.screen else {
        return Vec::new();
    };
    let page = state.page.clone();
    let name = page.name.clone();
    if state.more_open {
        // The folded page actions, opened from "⋯ More" at 80 columns.
        state.more_open = false;
        return match key.code {
            KeyCode::Char('r') => page.restart(),
            KeyCode::Char('s') => page.stop(),
            KeyCode::Char('x') => {
                push(app, Screen::AppRemove(AppRemoveState::new(&name)));
                Vec::new()
            }
            _ => Vec::new(),
        };
    }
    let sections = app.data.app_sections(&name);
    let owner_row = sections.len() > APP_SECTIONS.len();
    match key.code {
        KeyCode::Char('?') => app.overlay = Some(Overlay { doc: help_document(), scroll: 0 }),
        KeyCode::Esc => pop(app),
        KeyCode::Down | KeyCode::Up => step(&mut state.selected, sections.len(), key.code == KeyCode::Down),
        KeyCode::Char('m') => state.more_open = true,
        KeyCode::Enter if owner_row && state.selected == 0 => return owner::open_owner_page(app, &name),
        KeyCode::Enter => match sections[state.selected.min(sections.len() - 1)].as_str() {
            // An app without a web page shows Hosting as a dimmed row that says so; Enter on
            // it stays put rather than flash a notice as if it had gone somewhere.
            "Hosting" if !page.has_site(&app.data) => {}
            "Hosting" => return web::hosting_from_detail(app, &name),
            "Settings" => return vec![Effect::OpenSettings(name)],
            _ => return open_logs(app, &name),
        },
        KeyCode::Char('h') => return web::hosting_from_detail(app, &name),
        KeyCode::Char('l') => return open_logs(app, &name),
        KeyCode::Char('u') => {
            let step = page.update(&app.data);
            return follow(app, step);
        }
        KeyCode::Char('r') => return page.restart(),
        KeyCode::Char('s') => {
            let step = page.start_or_stop(&app.data);
            return follow(app, step);
        }
        KeyCode::Char('x') => push(app, Screen::AppRemove(AppRemoveState::new(&name))),
        _ => {}
    }
    Vec::new()
}

fn app_remove_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::AppRemove(state) = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Esc => pop(app),
        KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => state.confirm_focused = !state.confirm_focused,
        // Enter only removes once the user moved to the Remove button (or clicked it).
        KeyCode::Enter if state.confirm_focused => {
            let step = state.question.confirm.step(&app.data);
            return follow(app, step);
        }
        KeyCode::Enter => pop(app),
        _ => {}
    }
    Vec::new()
}

fn system_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    if let Some(effects) = tab_key(app, key) {
        return effects;
    }
    let nodo_update = app.data.updates.iter().any(|item| item.name == nodo_ui_core::model::SYSTEM_BASE && !item.held);
    let Screen::System { selected } = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Down | KeyCode::Up => step(selected, SYSTEM_ROWS.len(), key.code == KeyCode::Down),
        KeyCode::Char('v') if nodo_update => return open_updates(app),
        KeyCode::Enter => match SYSTEM_ROWS[(*selected).min(SYSTEM_ROWS.len() - 1)] {
            "Updates" => return open_updates(app),
            "Backups" => return open_backups(app),
            "Network" => return net::open_network(app),
            "Device logs" => return open_device_logs(app),
            _ => {
                app.overlay = Some(Overlay { doc: documents::about(&app.data), scroll: 0 });
            }
        },
        KeyCode::Char('u') => return open_unlock(app),
        KeyCode::Char('k') => {
            return open_ssh_keys(app);
        }
        _ => {}
    }
    Vec::new()
}

/// System › Backups.
fn open_backups(app: &mut App) -> Vec<Effect> {
    let device_set_up = !app.data.overview.setup_needed;
    push(app, Screen::Backup(BackupState::new(device_set_up)));
    vec![Effect::LoadBackups]
}

/// The input that currently has keyboard focus. Pasted text only ever goes here, because pasted
/// characters must not trigger the single-letter shortcuts of a screen.
fn focused_input(screen: &mut Screen) -> Option<&mut Input> {
    match screen {
        Screen::Search(state) if !state.list_focused => Some(&mut state.query),
        Screen::Unlock(state) => Some(&mut state.input),
        Screen::AddFeed(AddFeedState::Address { input, .. }) => Some(input),
        Screen::FeedKey(state) => state.typing.as_mut(),
        Screen::Settings(state) => state.editing.as_mut(),
        Screen::Reference(state) if !state.list_focused => Some(&mut state.query),
        Screen::Logs(state) if state.finding => Some(&mut state.find),
        Screen::AutoUpdate(state) => state.editing_window.as_mut(),
        Screen::Updates(state) => state.passphrase.as_mut().map(|prompt| &mut prompt.input),
        Screen::WebExposure(state) if state.list_open() => None,
        Screen::WebExposure(state) => match state.focus() {
            HostingFocus::Address => Some(&mut state.sub),
            HostingFocus::AliasAddress => Some(&mut state.alias_sub),
            _ => None,
        },
        Screen::WebProviders(state) if state.stage != ProviderStage::Key => None,
        Screen::WebProviders(state) => match state.focus {
            ProviderFocus::List => None,
            ProviderFocus::Domain => Some(&mut state.domain),
            ProviderFocus::ApiKey => Some(&mut state.api_key),
            ProviderFocus::SecretKey => Some(&mut state.secret_key),
        },
        Screen::WebDomain(state) => match state.focus {
            DomainFocus::Email => Some(&mut state.email),
            DomainFocus::UnderDomain => Some(&mut state.local_domain),
            DomainFocus::Dns01First => Some(&mut state.dns01_first),
            DomainFocus::Dns01Second => Some(&mut state.dns01_second),
            DomainFocus::Staging | DomainFocus::DotLocal => None,
        },
        Screen::Setup(SetupState { step: SetupStep::Passphrase { first, second, confirming }, .. }) => {
            Some(if *confirming { second } else { first })
        }
        Screen::Setup(SetupState { step: SetupStep::Keys { adding, .. }, .. }) => adding.as_mut(),
        Screen::Review(state) => state.passphrase.as_mut(),
        Screen::Backup(state) => backup_input(&mut state.step),
        Screen::Restore(state) => more::restore_input(&mut state.step),
        Screen::Network(state) => net::network_input(state),
        Screen::Bundle(state) => match state.step {
            BundleStep::Answers => state.open_asks().get(state.focus).copied().map(|i| &mut state.answers[i]),
            BundleStep::Passphrase => Some(&mut state.passphrase),
            _ => None,
        },
        Screen::OwnerPage(state) if state.view.is_none() => Some(&mut state.passphrase),
        _ => None,
    }
}

fn on_paste(app: &mut App, text: &str) {
    if app.too_small() || app.modal.is_some() || app.overlay.is_some() {
        return;
    }
    if let Some(input) = focused_input(&mut app.screen) {
        // Input::push drops control characters, so a pasted newline cannot confirm anything.
        text.chars().for_each(|c| input.push(c));
    }
}

/// Applies a typing key to an input. Returns true if the key was consumed.
fn edit_input(input: &mut Input, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => input.clear(),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => input.push(c),
        KeyCode::Backspace => input.pop(),
        _ => return false,
    }
    true
}

fn modal_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Some(modal) = app.modal.as_mut() else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Left | KeyCode::Right => modal.confirm_focused = !modal.confirm_focused,
        KeyCode::Esc => app.modal = None,
        KeyCode::Enter => {
            let modal = app.modal.take().expect("modal checked above");
            if modal.confirm_focused {
                return modal_action(app, modal.question.confirm);
            }
        }
        _ => {}
    }
    Vec::new()
}

/// The owner confirmed a question. The screens that show the work as running are marked first;
/// what confirming does comes from the core.
fn modal_action(app: &mut App, confirm: Confirm) -> Vec<Effect> {
    match &confirm {
        Confirm::RollBack { .. } => more::mark_rolling_back(app),
        Confirm::JoinOpenWifi(_) | Confirm::ForgetWifi(_) | Confirm::WifiOff | Confirm::RemoveBluetooth(_) => {
            if !net::mark_confirmed(app, &confirm) {
                return Vec::new();
            }
        }
        Confirm::RemoveSshKey(_) => {
            if let Screen::Setup(state) = &mut app.screen {
                state.flow.confirmed(&confirm);
            }
        }
        _ => {}
    }
    let step = confirm.step(&app.data);
    follow(app, step)
}

/// Does what a step from the core says: shows its notice or its question, goes where it sends,
/// and returns the effects it carries, whose outcomes come back through `on_result`.
fn follow(app: &mut App, step: Step) -> Vec<Effect> {
    match step {
        Step::Stay => Vec::new(),
        Step::Done(notice) | Step::Failed(notice) => {
            app.notice = Some(notice);
            Vec::new()
        }
        Step::Ask(question) => {
            app.modal = Some(Modal::ask(question));
            Vec::new()
        }
        Step::Go(dest) => go(app, dest),
        Step::Run(effects) => effects,
        Step::All(steps) => steps.into_iter().flat_map(|step| follow(app, step)).collect(),
    }
}

/// Maps a destination of the core to this interface's screens.
fn go(app: &mut App, dest: Dest) -> Vec<Effect> {
    match dest {
        Dest::Back => pop(app),
        // Only removing an app sends here, from the question under the app's page.
        Dest::Apps => {
            if matches!(app.screen, Screen::AppRemove(_)) {
                pop(app);
                pop(app);
            }
        }
        Dest::Updates { only } => {
            let screen = updates_screen(app);
            push(app, screen);
            if let (Screen::Updates(updates), Some(name)) = (&mut app.screen, only) {
                updates.checked = vec![name];
            }
        }
        Dest::Unlock { resume } => push(app, Screen::Unlock(UnlockState::new(resume))),
        // The screen below asked for the unlock on Enter, so it gets its Enter again.
        Dest::GetApps { source } => return open_get_apps(app, source),
        Dest::ConfirmSource(probe) => {
            if let Screen::AddFeed(state) = &mut app.screen {
                *state = AddFeedState::Confirm { probe, trust_focused: false };
            }
        }
        Dest::KeyChange(change) => push(app, Screen::FeedKey(FeedKeyState { change, choice: 0, typing: None, error: None })),
        Dest::Sources { select } => {
            let selected = select.and_then(|name| app.data.feeds.iter().position(|feed| feed.name == name)).unwrap_or(0);
            return open_sources(app, selected);
        }
        Dest::AppPage(name) => {
            switch_tab(app, Tab::Apps);
            push(app, Screen::AppPage(AppPageState::new(&name)));
        }
        Dest::Detail(detail) => push(app, Screen::Detail(detail)),
        Dest::Review(review) => push(app, Screen::Review(ReviewState::of(review))),
        Dest::Bundle(review) => owner::open_bundle(app, *review),
        Dest::Rejected(data) => push(app, Screen::Rejected(data)),
        Dest::MissingCapability(data) => push(app, Screen::Capability(CapabilityState { data, choice: 0 })),
        // The install's page takes the place of the review that started it.
        Dest::Installing(report) => app.screen = Screen::Installing(report),
        Dest::ApplyUpdates { names, approvals } => return apply_batch(app, names, approvals),
        Dest::Settings(form) => push(app, Screen::Settings(SettingsState::new(form))),
        Dest::ApplyFailed(failed) => push(app, Screen::ApplyFailed(failed)),
        Dest::Document(doc) => app.overlay = Some(Overlay { doc, scroll: 0 }),
        Dest::Resume => {
            pop(app);
            return on_key(app, KeyEvent::from(KeyCode::Enter));
        }
    }
    Vec::new()
}

fn overlay_key(app: &mut App, key: KeyEvent) {
    let Some(overlay) = app.overlay.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Down => {
            let len = overlay.doc.lines.len();
            step(&mut overlay.scroll, len, true);
        }
        KeyCode::Up => overlay.scroll = overlay.scroll.saturating_sub(1),
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | '?') => app.overlay = None,
        _ => {}
    }
}

fn on_tick(app: &mut App) -> Vec<Effect> {
    app.ticks += 1;
    match &app.screen {
        // Only a backend that reports progress is polled. The helper's install answers once it
        // is over, and polling it would only queue behind that answer.
        Screen::Installing(snapshot) => install::poll(snapshot, &app.data.capabilities),
        Screen::Logs(state) if app.ticks % (logs::FOLLOW_SECONDS * TICKS_PER_SECOND) == 0 => {
            state.follow_up(app.data.log_sources.get(state.selected).map(String::as_str))
        }
        Screen::Home { .. } if app.ticks % (10 * TICKS_PER_SECOND) == 0 => vec![Effect::Refresh],
        // Caddy works on the certificate in the background, so its progress is read again.
        Screen::WebCertificate(_) if app.ticks % (2 * TICKS_PER_SECOND) == 0 => vec![Effect::Refresh],
        Screen::Network(_) => net::on_tick(app),
        _ => Vec::new(),
    }
}

fn home_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    if let Some(effects) = tab_key(app, key) {
        return effects;
    }
    let items = app.home_items();
    let Screen::Home { selected } = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Down | KeyCode::Up => step(selected, items.len(), key.code == KeyCode::Down),
        KeyCode::Enter => match items.get(*selected).cloned() {
            Some(HomeItem::Unlock) => return open_unlock(app),
            Some(HomeItem::Failing(id)) => return open_logs(app, &id),
            Some(HomeItem::Updates) => return open_updates(app),
            Some(HomeItem::App(id)) => {
                switch_tab(app, Tab::Apps);
                push(app, Screen::AppPage(AppPageState::new(&id)));
            }
            None => {}
        },
        KeyCode::Char('/' | 'g') => return open_get_apps(app, None),
        KeyCode::Char('u') => return open_unlock(app),
        // The failing app's log, wherever the selection is.
        KeyCode::Char('l') => {
            let failing = app.data.services.iter().find(|svc| svc.state == ServiceState::Failed).map(|svc| svc.id.clone());
            if let Some(id) = failing {
                return open_logs(app, &id);
            }
        }
        KeyCode::Char('k') => {
            return open_ssh_keys(app);
        }
        KeyCode::Char('b') => return open_backups(app),
        KeyCode::Char('r') => return more::open_rollbacks(app),
        KeyCode::Char('w') => return net::open_network(app),
        _ => {}
    }
    Vec::new()
}

fn setup_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Setup(SetupState { flow, step: stage }) = &mut app.screen else {
        return Vec::new();
    };
    if flow.busy {
        return Vec::new();
    }
    match stage {
        SetupStep::Passphrase { first, second, confirming } => match key.code {
            // A new device can instead be rebuilt from a backup on a USB drive.
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                first.clear();
                second.clear();
                *confirming = false;
                flow.error = None;
                return more::open_restore(app);
            }
            KeyCode::Enter if !*confirming => *confirming = flow.first_entered(&first.secret_value()),
            KeyCode::Enter => {
                let effects = flow.choose_passphrase(first, second);
                // Two that differ are typed again from the first.
                *confirming = !effects.is_empty();
                return effects;
            }
            KeyCode::Esc if *confirming => {
                second.clear();
                *confirming = false;
            }
            _ => {
                edit_input(if *confirming { second } else { first }, key);
            }
        },
        SetupStep::Keys { selected, adding } => {
            if let Some(input) = adding {
                match key.code {
                    KeyCode::Enter => {
                        let effects = flow.add_key(input.text());
                        *adding = None;
                        return effects;
                    }
                    KeyCode::Esc => *adding = None,
                    _ => {
                        edit_input(input, key);
                    }
                }
                return Vec::new();
            }
            match key.code {
                KeyCode::Down | KeyCode::Up => step(selected, flow.keys.len(), key.code == KeyCode::Down),
                KeyCode::Char('a') => *adding = Some(Input::plain("")),
                KeyCode::Char('d') => {
                    let question = flow.keys.get(*selected).and_then(|chosen| flow.ask_remove_key(&chosen.fingerprint));
                    app.modal = question.map(Modal::ask);
                }
                KeyCode::Esc | KeyCode::Enter => {
                    let step = flow.finish();
                    return follow(app, step);
                }
                _ => {}
            }
        }
    }
    Vec::new()
}

fn open_unlock(app: &mut App) -> Vec<Effect> {
    let step = unlock::open(&app.data);
    follow(app, step)
}

/// The SSH keys page, from Home or System.
fn open_ssh_keys(app: &mut App) -> Vec<Effect> {
    push(app, Screen::Setup(SetupState::keys()));
    Setup::load_keys()
}

fn no_drive_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    if let Some(effects) = tab_key(app, key) {
        return effects;
    }
    let Screen::NoDrive { choice } = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Down | KeyCode::Up => step(choice, 4, key.code == KeyCode::Down),
        KeyCode::Enter => match *choice {
            0 => return drive::retry(),
            1 => return drive::check(),
            2 => app.modal = Some(Modal::ask(drive::ask_repair())),
            _ => return drive::diagnostics(),
        },
        KeyCode::Char('l') => return open_logs(app, "System"),
        KeyCode::Char('!') => return drive::shell(),
        _ => {}
    }
    Vec::new()
}

fn search_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Search(state) = &app.screen else {
        return Vec::new();
    };
    let state_snapshot = state.clone();
    let results: Vec<PackageId> =
        app.search_results(&state_snapshot).iter().map(|pkg| pkg.id.clone()).collect();
    let Screen::Search(state) = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        // The first Esc clears a search or a source filter, the next goes back to Apps.
        KeyCode::Esc if state.query.is_empty() && state.feed_filter.is_none() => pop(app),
        KeyCode::Esc => {
            state.query.clear();
            state.flow.clear();
            state.list_focused = false;
            state.selected = 0;
        }
        // Ctrl+S also works from the search box, where a plain s is typed: the Sources link sends it.
        KeyCode::Char('s') if state.list_focused || key.modifiers.contains(KeyModifiers::CONTROL) => return open_sources(app, 0),
        KeyCode::Enter => {
            if let Some(id) = results.get(state.selected) {
                return install::detail(id);
            }
        }
        KeyCode::Down | KeyCode::Up => {
            // The first Down only moves focus, so the highlighted row is not skipped over.
            if state.list_focused {
                if key.code == KeyCode::Up && state.selected == 0 {
                    state.list_focused = false;
                } else {
                    step(&mut state.selected, results.len(), key.code == KeyCode::Down);
                }
            } else if key.code == KeyCode::Down {
                state.list_focused = true;
            }
        }
        _ if !state.list_focused => {
            if edit_input(&mut state.query, key) {
                state.selected = 0;
            }
        }
        KeyCode::Char('/') => state.list_focused = false,
        KeyCode::Backspace => {
            state.list_focused = false;
            state.query.pop();
            state.selected = 0;
        }
        KeyCode::Char('i') => {
            if let Some(id) = results.get(state.selected) {
                return install::check(id);
            }
        }
        KeyCode::Char('f') => {
            state.flow.next_source(&app.data);
            state.selected = 0;
        }
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('?') => {
            app.overlay = Some(Overlay { doc: help_document(), scroll: 0 });
        }
        _ => {}
    }
    Vec::new()
}

fn detail_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Detail(detail) = &app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Char('i') => install::install(detail),
        KeyCode::Char('o') => {
            let step = install::open_installed(detail);
            follow(app, step)
        }
        KeyCode::Char('s') => install::settings(detail),
        KeyCode::Char('v') => install::versions(detail),
        KeyCode::Char('c') => install::changelog(detail),
        KeyCode::Char('h') if detail.installed => {
            let package = detail.id.name.clone();
            web::hosting_from_detail(app, &package)
        }
        KeyCode::Esc => {
            pop(app);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn review_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Review(state) = &mut app.screen else {
        return Vec::new();
    };
    if let Some(input) = state.passphrase.as_mut() {
        match key.code {
            KeyCode::Esc => state.passphrase = None,
            KeyCode::Enter => {
                // An empty box is refused with a remark and stays open.
                let step = state.flow.approve(Some(input));
                return follow(app, step);
            }
            _ => {
                edit_input(input, key);
            }
        }
        return Vec::new();
    }
    match key.code {
        KeyCode::Left | KeyCode::Right => state.confirm_focused = !state.confirm_focused,
        KeyCode::Char('d') => return state.flow.files(),
        KeyCode::Esc => pop(app),
        KeyCode::Enter => {
            if !state.confirm_focused {
                pop(app);
                return Vec::new();
            }
            if state.flow.needs_passphrase() {
                state.passphrase = Some(Input::secret());
                return Vec::new();
            }
            let step = state.flow.approve(None);
            return follow(app, step);
        }
        _ => {}
    }
    Vec::new()
}

fn installing_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Installing(snapshot) = &app.screen else {
        return Vec::new();
    };
    let step = match key.code {
        KeyCode::Char('v') => return install::log(snapshot),
        KeyCode::Esc => install::leave(snapshot),
        KeyCode::Enter => install::carry_on(snapshot),
        _ => return Vec::new(),
    };
    follow(app, step)
}

fn unlock_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let ticks = app.ticks;
    let Screen::Unlock(state) = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Esc => pop(app),
        KeyCode::Enter => {
            let wait = state.retry_at_tick.saturating_sub(ticks).div_ceil(TICKS_PER_SECOND);
            return state.flow.submit(&mut state.input, wait);
        }
        _ => {
            edit_input(&mut state.input, key);
        }
    }
    Vec::new()
}

fn feeds_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    if key.code == KeyCode::Esc {
        pop(app);
        return Vec::new();
    }
    if let Some(effects) = tab_key(app, key) {
        return effects;
    }
    let feeds = app.data.feeds.clone();
    let Screen::Feeds { selected, keys } = &mut app.screen else {
        return Vec::new();
    };
    let current = feeds.get(*selected);
    match key.code {
        KeyCode::Down | KeyCode::Up => step(selected, feeds.len(), key.code == KeyCode::Down),
        KeyCode::Char('a') => {
            push(app, Screen::AddFeed(AddFeedState::Address { input: Input::plain(""), error: None }));
        }
        KeyCode::Char('r') => return sources::refresh(),
        KeyCode::Char('d') => *keys = !*keys,
        KeyCode::Char('e' | ' ') => {
            if let Some(feed) = current {
                return sources::toggle(&feed.name);
            }
        }
        KeyCode::Char('x') => {
            if let Some(feed) = current {
                app.modal = Some(Modal::ask(sources::ask_remove(&feed.name, false)));
            }
        }
        KeyCode::Enter => {
            if let Some(feed) = current {
                let step = sources::open(feed);
                return follow(app, step);
            }
        }
        _ => {}
    }
    Vec::new()
}

fn add_feed_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::AddFeed(state) = &mut app.screen else {
        return Vec::new();
    };
    match state {
        AddFeedState::Address { input, error } => match key.code {
            KeyCode::Esc => pop(app),
            KeyCode::Enter => match sources::probe(input.text()) {
                Ok(effects) => return effects,
                Err(text) => *error = Some(text),
            },
            _ => {
                edit_input(input, key);
            }
        },
        AddFeedState::Confirm { probe, trust_focused } => match key.code {
            KeyCode::Left | KeyCode::Right => *trust_focused = !*trust_focused,
            KeyCode::Char('p') => return sources::packages(probe),
            KeyCode::Esc => pop(app),
            KeyCode::Enter => {
                if *trust_focused {
                    return sources::trust(probe);
                }
                pop(app);
            }
            _ => {}
        },
    }
    Vec::new()
}

fn feed_key_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::FeedKey(state) = &mut app.screen else {
        return Vec::new();
    };
    if let Some(input) = state.typing.as_mut() {
        match key.code {
            KeyCode::Esc => {
                state.typing = None;
                state.error = None;
            }
            KeyCode::Enter => match sources::trust_new_key(&state.change, input.text()) {
                Ok(effects) => return effects,
                Err(text) => state.error = Some(text),
            },
            _ => {
                edit_input(input, key);
            }
        }
        return Vec::new();
    }
    match key.code {
        KeyCode::Down | KeyCode::Up => step(&mut state.choice, 3, key.code == KeyCode::Down),
        KeyCode::Esc => pop(app),
        KeyCode::Enter => match state.choice {
            0 => pop(app),
            1 => state.typing = Some(Input::plain("")),
            _ => {
                let question = sources::ask_remove(&state.change.feed, true);
                app.modal = Some(Modal::ask(question));
            }
        },
        _ => {}
    }
    Vec::new()
}

fn settings_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    // F4, Ctrl-R and Ctrl-. open the picker even while typing, so they never reach the box.
    if opens_picker(key) {
        return open_reference_picker(app, PickMode::Insert);
    }
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    if state.confirm.is_some() {
        return match key.code {
            KeyCode::Enter => state.flow.confirm_save(),
            KeyCode::Esc => {
                state.flow.cancel_save();
                Vec::new()
            }
            _ => Vec::new(),
        };
    }
    // Ctrl+Z undoes even while a field is being typed in: the undo link sends it.
    if is_ctrl(key, 'z') {
        return undo_step(app);
    }
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    if let Some(input) = state.editing.as_mut() {
        match key.code {
            KeyCode::Esc => state.editing = None,
            KeyCode::Enter => {
                state.flow.set_secret(state.selected, input);
                state.editing = None;
            }
            _ => {
                edit_input(input, key);
            }
        }
        return Vec::new();
    }
    if state.editor.is_some() {
        return editor_key(app, key);
    }
    if state.chip_menu.is_some() {
        return chip_menu_key(app, key);
    }
    if is_ctrl(key, 's') {
        return save_settings(app);
    }
    let index = state.selected;
    let count = state.form.fields.len();
    let is_choice = matches!(state.form.fields[index].kind, FieldKind::Choice(_));
    match key.code {
        KeyCode::Down | KeyCode::Up => step(&mut state.selected, count, key.code == KeyCode::Down),
        KeyCode::Tab | KeyCode::BackTab => step(&mut state.selected, count, key.code == KeyCode::Tab),
        // A field with a broken reference opens what is wrong with it; anything else edits.
        KeyCode::Enter if state.first_broken(index).is_some() => {
            let chip = state.first_broken(index).unwrap_or(0);
            state.chip_menu = Some(ChipMenu { chip, choice: 0 });
        }
        KeyCode::Enter | KeyCode::Char(' ') => start_field_edit(state, index, true),
        KeyCode::Char('c') if matches!(state.draft[index], FieldValue::Pieces(_)) => {
            state.chip_menu = Some(ChipMenu { chip: 0, choice: 0 });
        }
        KeyCode::Left if is_choice => start_field_edit(state, index, false),
        KeyCode::Right if is_choice => start_field_edit(state, index, true),
        KeyCode::Char('$') => return open_reference_picker(app, PickMode::Insert),
        KeyCode::Char('u') => return undo_step(app),
        KeyCode::Char('z') => return state.flow.reset(index),
        KeyCode::Esc => {
            let step = state.flow.leave();
            return follow(app, step);
        }
        _ => {}
    }
    Vec::new()
}

/// Typing in a field: text, reference chips, and the suggestions after `$(`.
fn editor_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    let index = state.selected;
    let suggestions = suggestions(state);
    let Some(editor) = state.editor.as_mut() else {
        return Vec::new();
    };
    let suggesting = !suggestions.is_empty() && !editor.suggestions_closed;
    match key.code {
        KeyCode::Down if suggesting => editor.suggestion = (editor.suggestion + 1).min(suggestions.len()),
        KeyCode::Up if suggesting => editor.suggestion = editor.suggestion.saturating_sub(1),
        KeyCode::Enter if suggesting => {
            // The last row is "All references…", which opens the picker with the typed name.
            match suggestions.get(editor.suggestion) {
                Some((app_id, setting)) => editor.complete(app_id, setting),
                None => {
                    let query = editor.completing().map(|(_, q)| q).unwrap_or_default();
                    if let Some((dollar, _)) = editor.completing() {
                        let cursor = editor.cursor;
                        editor.units.drain(dollar..cursor);
                        editor.cursor = dollar;
                    }
                    let effects = open_reference_picker(app, PickMode::Insert);
                    if let Screen::Reference(picker) = &mut app.screen {
                        picker.query = Input::plain(query);
                    }
                    return effects;
                }
            }
        }
        KeyCode::Esc if suggesting => editor.suggestions_closed = true,
        KeyCode::Esc => {
            state.editor = None;
        }
        KeyCode::Enter => {
            let pieces = editor.pieces();
            state.editor = None;
            return state.flow.set_pieces(index, pieces);
        }
        KeyCode::Left => editor.left(),
        KeyCode::Right => editor.right(),
        KeyCode::Home => editor.home(),
        KeyCode::End => editor.end(),
        KeyCode::Backspace => editor.backspace(),
        KeyCode::Delete => editor.delete(),
        // Clears what is before the cursor, as in a shell.
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let cursor = editor.cursor;
            editor.units.drain(..cursor);
            editor.cursor = 0;
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) && !c.is_control() => editor.insert_char(c),
        _ => {}
    }
    Vec::new()
}

/// What `$(…` before the cursor suggests in the field being typed in. The rule is the core's.
pub fn suggestions(state: &SettingsState) -> Vec<(String, String)> {
    state.editor.as_ref().map(|editor| state.flow.suggestions(state.selected, editor)).unwrap_or_default()
}

fn chip_menu_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    let chips = state.flow.references(state.selected).len();
    let Some(menu) = state.chip_menu.as_mut() else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Esc => state.chip_menu = None,
        KeyCode::Left | KeyCode::BackTab => menu.choice = menu.choice.saturating_sub(1),
        KeyCode::Right => menu.choice = (menu.choice + 1).min(CHIP_ACTIONS.len() - 1),
        KeyCode::Tab => menu.chip = (menu.chip + 1) % chips.max(1),
        KeyCode::Enter => {
            let (chip, choice) = (menu.chip, menu.choice);
            return chip_action(app, chip, choice);
        }
        KeyCode::Char(c @ ('r' | 't' | 'x')) => {
            let choice = match c {
                'r' => 0,
                't' => 1,
                _ => 2,
            };
            let chip = menu.chip;
            return chip_action(app, chip, choice);
        }
        _ => {}
    }
    Vec::new()
}

/// Replace…, Type a value instead, or Remove, on one reference of the selected field.
fn chip_action(app: &mut App, chip: usize, choice: usize) -> Vec<Effect> {
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    state.chip_menu = None;
    if choice == 0 {
        return open_reference_picker(app, PickMode::Replace(chip));
    }
    let index = state.selected;
    if chip >= state.references(index).len() {
        return Vec::new();
    }
    if choice == 1 {
        // The chip becomes what it resolves to now, ready to edit.
        let effects = state.flow.type_instead(index, chip);
        let mut editor = nodo_ui_core::refs::PieceEditor::new(&state.draft[index].pieces().unwrap_or_default());
        editor.end();
        state.editor = Some(editor);
        effects
    } else {
        state.flow.remove_reference(index, chip)
    }
}

/// Takes the last step back: the link under the field.
fn undo_step(app: &mut App) -> Vec<Effect> {
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    let Some(field) = state.undo.as_ref().map(|undo| undo.field) else {
        return Vec::new();
    };
    let applies = state.undo_for(field).is_some();
    let effects = state.flow.undo(field);
    if applies {
        state.editor = None;
        state.editing = None;
        state.selected = field;
    }
    effects
}

/// The keys that open the picker from the Settings form. F4 opens a drop-down in many programs
/// and reaches every terminal, and so does Ctrl-R. Ctrl-. only arrives where the terminal reports
/// it (the kitty keyboard protocol, see `CrosstermMode`); elsewhere it types nothing or a dot.
pub fn opens_picker(key: KeyEvent) -> bool {
    key.code == KeyCode::F(4) || is_ctrl(key, '.') || is_ctrl(key, 'r')
}

/// Opens the one picker for the selected field. Whatever is being typed stays, so Esc from the
/// picker goes back to it, and the picked reference goes in at its cursor.
fn open_reference_picker(app: &mut App, mode: PickMode) -> Vec<Effect> {
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    state.chip_menu = None;
    let beside_typed = state.editor.as_ref().is_some_and(|editor| !editor.is_empty());
    match state.flow.pick(state.selected, mode, beside_typed) {
        Ok((picker, load)) => {
            let picker = ReferenceState { flow: picker, query: Input::plain(""), list_focused: false, selected: 0 };
            push(app, Screen::Reference(picker));
            load
        }
        Err(why) => {
            fail(app, why);
            Vec::new()
        }
    }
}

/// Enter, Space or an arrow on a field: a switch flips, a choice steps, and anything else opens
/// its box to type in.
fn start_field_edit(state: &mut SettingsState, index: usize, forward: bool) {
    let kind = state.form.fields[index].kind.clone();
    match (&kind, &state.flow.draft[index]) {
        (FieldKind::Toggle, FieldValue::Bool(_)) => state.flow.toggle(index),
        (FieldKind::Choice(_), FieldValue::Text(_)) => state.flow.step_choice(index, forward),
        // A secret with a reference is replaced by typing a new secret.
        (FieldKind::Secret, _) => state.editing = Some(Input::secret()),
        (_, value) => {
            if let Some(pieces) = value.pieces() {
                state.editor = Some(nodo_ui_core::refs::PieceEditor::new(&pieces));
            }
        }
    }
}

fn save_settings(app: &mut App) -> Vec<Effect> {
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    let step = state.flow.save();
    // A refused save puts the focus on the field it stopped at.
    if matches!(step, Step::Failed(_)) {
        if let Some(index) = state.blocking_field() {
            state.selected = index;
        }
    }
    follow(app, step)
}

fn reference_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Reference(state) = &mut app.screen else {
        return Vec::new();
    };
    if state.granting.is_some() {
        match key.code {
            KeyCode::Esc => state.flow.decline(),
            KeyCode::Enter => return state.flow.approve(),
            _ => {}
        }
        return Vec::new();
    }
    let visible_len = state.visible().len();
    match key.code {
        KeyCode::Esc => pop(app),
        KeyCode::Enter => {
            let chosen = state.visible().get(state.selected).map(|(option, _)| (option.app.clone(), option.setting.clone()));
            let Some((app_id, setting)) = chosen else {
                return Vec::new();
            };
            let picked = state.flow.choose(&app_id, &setting);
            return on_picked(app, picked);
        }
        KeyCode::Down | KeyCode::Up => {
            if state.list_focused {
                if key.code == KeyCode::Up && state.selected == 0 {
                    state.list_focused = false;
                } else {
                    step(&mut state.selected, visible_len, key.code == KeyCode::Down);
                }
            } else if key.code == KeyCode::Down {
                state.list_focused = true;
            }
        }
        _ if !state.list_focused => {
            if edit_input(&mut state.query, key) {
                state.selected = 0;
            }
        }
        KeyCode::Char('$' | '/') | KeyCode::Backspace => state.list_focused = false,
        _ => {}
    }
    Vec::new()
}

/// What a choice in the picker, or a result there, came to: a reference to place, or only a step.
fn on_picked(app: &mut App, picked: Picked) -> Vec<Effect> {
    match picked {
        Picked::Nothing(step) => follow(app, step),
        Picked::Reference { app: app_id, setting, then } => {
            let mut effects = place_reference(app, &app_id, &setting);
            effects.extend(follow(app, then));
            effects
        }
    }
}

/// Puts a picked reference where the picker was opened for, and closes the picker: at the cursor
/// of what is being typed, or through the core into the field's value.
fn place_reference(app: &mut App, app_id: &str, setting: &str) -> Vec<Effect> {
    let Screen::Reference(picker) = &app.screen else {
        return Vec::new();
    };
    let (field_index, mode) = (picker.field_index, picker.mode);
    pop(app);
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    state.selected = field_index;
    match (mode, state.editor.as_mut()) {
        (PickMode::Insert, Some(editor)) => {
            editor.insert_ref(app_id, setting);
            Vec::new()
        }
        (PickMode::Insert, None) => state.flow.place(field_index, mode, app_id, setting),
        (PickMode::Replace(_), _) => {
            state.editor = None;
            state.flow.place(field_index, mode, app_id, setting)
        }
    }
}

fn apply_failed_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::ApplyFailed(state) = &app.screen else {
        return Vec::new();
    };
    let failed = state.clone();
    match key.code {
        KeyCode::Enter => {
            pop(app);
            if let Screen::Settings(settings) = &mut app.screen {
                if let Some(index) = failed.blamed_index(&settings.form) {
                    settings.selected = index;
                }
            }
        }
        KeyCode::Char('l') => return open_logs(app, &failed.package),
        KeyCode::Char('x') => {
            pop(app);
            if let Screen::Settings(settings) = &mut app.screen {
                *settings = SettingsState::new(settings.form.clone());
            }
        }
        KeyCode::Esc => pop(app),
        _ => {}
    }
    Vec::new()
}

/// Typing the device passphrase for updates that grow encrypted storage. Every key goes to the
/// input, so letters such as q never act as shortcuts here.
fn update_passphrase_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Updates(state) = &mut app.screen else {
        return Vec::new();
    };
    let Some(prompt) = state.passphrase.as_mut() else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Esc => state.passphrase = None,
        KeyCode::Enter if prompt.input.is_empty() => {
            notice(app, NoticeKind::Info, "Enter the device passphrase to grow the encrypted storage.");
        }
        KeyCode::Enter => {
            let passphrase = prompt.input.take_secret();
            let prompt = state.passphrase.take().expect("prompt checked above");
            return vec![Effect::ApplyUpdatesWithPassphrase { names: prompt.names, approvals: prompt.approvals, passphrase }];
        }
        _ => {
            edit_input(&mut prompt.input, key);
        }
    }
    Vec::new()
}

fn updates_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    if matches!(&app.screen, Screen::Updates(UpdatesState { passphrase: Some(_), .. })) {
        return update_passphrase_key(app, key);
    }
    if key.code == KeyCode::Esc {
        pop(app);
        return Vec::new();
    }
    if let Some(effects) = tab_key(app, key) {
        return effects;
    }
    let rows: Vec<UpdateItem> = app.update_rows().into_iter().cloned().collect();
    let auto_update = app.data.auto_update.clone();
    let Screen::Updates(state) = &mut app.screen else {
        return Vec::new();
    };
    let current = rows.get(state.selected);
    match key.code {
        KeyCode::Down | KeyCode::Up => step(&mut state.selected, rows.len(), key.code == KeyCode::Down),
        KeyCode::Char(' ') => {
            if let Some(item) = current {
                if let Some(position) = state.checked.iter().position(|name| name == &item.name) {
                    state.checked.remove(position);
                } else {
                    state.checked.push(item.name.clone());
                    if item.held {
                        return vec![Effect::SetHeld { name: item.name.clone(), held: false }];
                    }
                }
            }
        }
        // The Nodo's own update has its own button, apart from the apps.
        KeyCode::Char('n') => {
            if rows.iter().any(|item| item.name == SYSTEM_BASE && !item.held) {
                return apply_batch(app, vec![SYSTEM_BASE.to_owned()], Vec::new());
            }
            notice(app, NoticeKind::Info, "The Nodo is up to date.");
        }
        KeyCode::Char('z') => return more::open_rollbacks(app),
        KeyCode::Enter => {
            // Keep the on-screen order, not the order the boxes were ticked in. The Nodo's own
            // update is never part of the apps' batch: it has its own button.
            let batch: Vec<String> = rows
                .iter()
                .filter(|item| item.name != SYSTEM_BASE && state.checked.contains(&item.name))
                .map(|item| item.name.clone())
                .collect();
            if batch.is_empty() {
                notice(app, NoticeKind::Info, "Tick at least one app first.");
                return Vec::new();
            }
            // A new permission is always approved first, even when the box is ticked.
            let mut pending_reviews: Vec<String> = rows
                .iter()
                .filter(|item| {
                    batch.contains(&item.name) && matches!(item.note, UpdateNote::NewPermission(_))
                })
                .map(|item| item.name.clone())
                .collect();
            if pending_reviews.is_empty() {
                return apply_batch(app, batch, Vec::new());
            }
            let name = pending_reviews.remove(0);
            return vec![Effect::ReviewUpdate { name, batch, pending_reviews, approvals: Vec::new() }];
        }
        KeyCode::Char('c') => {
            if let Some(item) = current {
                return vec![Effect::ShowDocument(DocKind::Changelog(item.name.clone()))];
            }
        }
        KeyCode::Char('r') => {
            // Check now is a link, not a button, so it is marked as running here: it shows the
            // hourglass and a second press waits for the first instead of queueing another check.
            app.pending = Some(Pending { click: crate::hit::Click::Key(KeyCode::Char('r')), screen: ScreenId::Updates });
            return vec![Effect::CheckUpdates];
        }
        KeyCode::Char('h') => {
            if let Some(item) = current {
                let name = item.name.clone();
                let held = !item.held;
                state.checked.retain(|checked| checked != &name);
                state.selected = 0;
                return vec![Effect::SetHeld { name, held }];
            }
        }
        KeyCode::Char('a') => {
            push(app, Screen::AutoUpdate(AutoUpdateState::new(auto_update)));
        }
        _ => {}
    }
    Vec::new()
}

fn auto_update_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::AutoUpdate(state) = &mut app.screen else {
        return Vec::new();
    };
    if let Some(input) = state.editing_window.as_mut() {
        match key.code {
            KeyCode::Esc => {
                state.editing_window = None;
                state.error = None;
            }
            KeyCode::Enter => match parse_window(input.text()) {
                Some((start, end)) => {
                    state.draft.window_start = start;
                    state.draft.window_end = end;
                    state.editing_window = None;
                    state.error = None;
                }
                None => {
                    state.error =
                        Some("Use two different device times, like 03:00 to 05:00.".to_owned());
                }
            },
            _ => {
                edit_input(input, key);
            }
        }
        return Vec::new();
    }
    let rows = state.draft.packages.len() + 1;
    match key.code {
        KeyCode::Down | KeyCode::Up => step(&mut state.selected, rows, key.code == KeyCode::Down),
        KeyCode::Char(' ') => match state.focused_package() {
            Some(index) => state.draft.packages[index].enabled ^= true,
            None => cycle_frequency(&mut state.draft),
        },
        KeyCode::Left | KeyCode::Right if state.selected == 0 => cycle_frequency(&mut state.draft),
        KeyCode::Char('w') => {
            let current = format!("{} to {}", state.draft.window_start, state.draft.window_end);
            state.editing_window = Some(Input::plain(current));
        }
        KeyCode::Char('h') => return vec![Effect::ShowDocument(DocKind::AutoUpdateHistory)],
        KeyCode::Enter => return vec![Effect::SaveAutoUpdate(state.draft.clone())],
        KeyCode::Esc => {
            if state.draft == state.saved {
                pop(app);
            } else {
                app.modal = Some(Modal::ask(Question {
                    title: "Discard your changes?".to_owned(),
                    body: vec!["Your auto-update choices will not be saved.".to_owned()],
                    confirm_label: "Discard".to_owned(),
                    cancel_is_default: true,
                    confirm: Confirm::DiscardAutoUpdate,
                }));
            }
        }
        _ => {}
    }
    Vec::new()
}

fn cycle_frequency(prefs: &mut AutoUpdatePrefs) {
    let position = prefs.frequencies.iter().position(|option| option == &prefs.frequency).unwrap_or(0);
    if let Some(next) = prefs.frequencies.get((position + 1) % prefs.frequencies.len().max(1)) {
        prefs.frequency = next.clone();
    }
}

fn update_results_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::UpdateResults(state) = &mut app.screen else {
        return Vec::new();
    };
    let failed = state.report.failure.as_ref().map(|failure| failure.name.clone());
    match key.code {
        KeyCode::Down | KeyCode::Up => {
            if failed.is_some() {
                step(&mut state.choice, 4, key.code == KeyCode::Down);
            }
        }
        KeyCode::Esc => {
            pop(app);
            return vec![Effect::Refresh];
        }
        KeyCode::Enter => {
            let Some(name) = failed else {
                pop(app);
                return vec![Effect::Refresh];
            };
            match state.choice {
                0 => {
                    pop(app);
                    return vec![Effect::SetHeld { name, held: true }];
                }
                // A failed version is held, and the helper only retries it once released.
                1 => {
                    let mut effects = vec![Effect::SetHeld { name: name.clone(), held: false }];
                    effects.extend(apply_batch(app, vec![name], Vec::new()));
                    return effects;
                }
                2 => return vec![Effect::CopyReport(format!("{name} update"))],
                _ => return open_logs(app, &name),
            }
        }
        _ => {}
    }
    Vec::new()
}

fn logs_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Logs(state) = &mut app.screen else {
        return Vec::new();
    };
    // Ctrl+E and Ctrl+Y act even while typing a search, where a plain e or y is typed: the
    // Problems chip and the Save link send them.
    if state.finding && !is_ctrl(key, 'e') && !is_ctrl(key, 'y') {
        match key.code {
            KeyCode::Enter => state.finding = false,
            KeyCode::Esc => {
                state.find.clear();
                state.finding = false;
            }
            _ => {
                edit_input(&mut state.find, key);
            }
        }
        return Vec::new();
    }
    if key.code == KeyCode::Esc {
        pop(app);
        return Vec::new();
    }
    if let Some(effects) = tab_key(app, key) {
        return effects;
    }
    let sources = app.data.log_sources.clone();
    let Screen::Logs(state) = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Down | KeyCode::Up => {
            step(&mut state.selected, sources.len(), key.code == KeyCode::Down);
            if let Some(source) = sources.get(state.selected) {
                return Logs::load(source);
            }
        }
        KeyCode::F(5) => {
            if let Some(source) = sources.get(state.selected) {
                return Logs::load(source);
            }
        }
        KeyCode::Char('/') => state.finding = true,
        KeyCode::Char('e') => state.flow.toggle_errors_only(),
        KeyCode::Char('f') => state.flow.toggle_follow(),
        KeyCode::Char('y') => {
            if let Some(source) = sources.get(state.selected) {
                return documents::log_report(source);
            }
        }
        KeyCode::Char('u') => return open_unlock(app),
        _ => {}
    }
    Vec::new()
}

fn rejected_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Rejected(data) = &app.screen else {
        return Vec::new();
    };
    // There is deliberately no key here that installs anything.
    match key.code {
        KeyCode::Char('y') => install::report_rejection(data),
        KeyCode::Char('f') => {
            let step = install::rejected_source(data);
            follow(app, step)
        }
        KeyCode::Esc => {
            pop(app);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn capability_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Capability(state) = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Down | KeyCode::Up => step(&mut state.choice, 3, key.code == KeyCode::Down),
        KeyCode::Esc => pop(app),
        KeyCode::Enter => match state.choice {
            0 => pop(app),
            1 => {
                let step = install::compatible_version(&state.data);
                return follow(app, step);
            }
            _ => return install::install_alternative(&state.data),
        },
        _ => {}
    }
    Vec::new()
}

fn domain_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let acme = app.data.web.acme.clone();
    let domain = app.data.web.domain.clone();
    let order = DomainState::order(&acme);
    let Screen::WebDomain(state) = &mut app.screen else {
        return Vec::new();
    };
    let at = order.iter().position(|f| *f == state.focus).unwrap_or(0);
    match key.code {
        KeyCode::Esc => {
            pop(app);
            return Vec::new();
        }
        KeyCode::Tab | KeyCode::Down => state.focus = order[(at + 1) % order.len()],
        KeyCode::BackTab | KeyCode::Up => state.focus = order[(at + order.len() - 1) % order.len()],
        KeyCode::Char(' ') if !state.focus.is_text() || state.focus == DomainFocus::UnderDomain => match state.focus {
            DomainFocus::Staging => state.staging = !state.staging,
            DomainFocus::DotLocal => state.under_domain = false,
            _ => state.under_domain = true,
        },
        KeyCode::Char('p') if !state.focus.is_text() => {
            push(app, Screen::WebProviders(ProvidersState::new(&domain)));
            return vec![Effect::LoadProviders];
        }
        KeyCode::Char('t') if !state.focus.is_text() => return vec![Effect::CheckLocalNames],
        KeyCode::Char('c') if !state.focus.is_text() => push(app, Screen::WebTrust),
        KeyCode::Enter => {
            let text = |input: &Input| Some(input.text().trim().to_owned()).filter(|t| !t.is_empty());
            let settings = nodo_api::AcmeSettingsInput {
                email: text(&state.email),
                staging: state.staging,
                local_domain: if state.under_domain { text(&state.local_domain) } else { None },
            };
            let dns01 = (!state.dns01_first.is_empty() && !state.dns01_second.is_empty())
                .then(|| (state.dns01_first.take_secret(), state.dns01_second.take_secret()));
            return vec![Effect::SaveAcme { settings, dns01 }];
        }
        _ => {
            if state.focus == DomainFocus::UnderDomain {
                state.under_domain = true;
            }
            if let Some(input) = focused_input(&mut app.screen) {
                edit_input(input, key);
            }
        }
    }
    Vec::new()
}

fn trust_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc | KeyCode::Enter => pop(app),
        KeyCode::Char('r') => return vec![Effect::Refresh],
        _ => {}
    }
    Vec::new()
}

fn certificate_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc | KeyCode::Enter => pop(app),
        KeyCode::Char('r') => return vec![Effect::Refresh],
        KeyCode::Char('l') => return open_logs(app, "System"),
        KeyCode::Char('d') => {
            let state = DomainState::new(&app.data.web.acme, &app.data.web.domain);
            push(app, Screen::WebDomain(state));
        }
        _ => {}
    }
    Vec::new()
}







fn on_refreshed(app: &mut App, snapshot: Snapshot) {
    // What the Updates list showed, to carry the ticks and the selection over by app name.
    let shown: Vec<String> = app.update_rows().iter().map(|item| item.name.clone()).collect();
    let was_selected = match &app.screen {
        Screen::Updates(state) => shown.get(state.selected).cloned(),
        _ => None,
    };
    app.data = snapshot;
    if app.data.overview.setup_needed && matches!(app.screen, Screen::Home { .. } | Screen::NoDrive { .. }) {
        push(app, Screen::Setup(SetupState::passphrase()));
        return;
    }
    let sources = app.data.log_sources.len();
    let services = app.data.services.len();
    let feeds = app.data.feeds.len();
    let sites = app.data.web.sites.len();
    let update_rows: Vec<(String, bool)> =
        app.update_rows().iter().map(|item| (item.name.clone(), item.preselected && !item.held)).collect();
    let rows = update_rows.len();
    let drive_missing = app.data.overview.drive.is_some();
    let clamp = |index: &mut usize, len: usize| *index = (*index).min(len.saturating_sub(1));
    match &mut app.screen {
        Screen::Home { .. } if drive_missing => app.screen = Screen::NoDrive { choice: 0 },
        Screen::NoDrive { .. } if !drive_missing => app.screen = Screen::Home { selected: 0 },
        Screen::Home { selected } => clamp(selected, services),
        Screen::Feeds { selected, .. } => clamp(selected, feeds),
        Screen::Web { selected } => clamp(selected, sites),
        // A check can add and remove updates. New ones are ticked as the list ticks them when it
        // opens, gone ones are unticked, and the selection stays on the same app rather than
        // the same row number, which would now be another app.
        Screen::Updates(state) => {
            state.checked.retain(|name| update_rows.iter().any(|(row, _)| row == name));
            for (name, preselected) in &update_rows {
                if *preselected && !shown.contains(name) && !state.checked.contains(name) {
                    state.checked.push(name.clone());
                }
            }
            match was_selected.and_then(|name| update_rows.iter().position(|(row, _)| *row == name)) {
                Some(position) => state.selected = position,
                None => clamp(&mut state.selected, rows),
            }
        }
        Screen::Logs(state) => clamp(&mut state.selected, sources),
        _ => {}
    }
}

fn on_result(app: &mut App, event: Outcome) -> Vec<Effect> {
    match event {
        Outcome::Refreshed(snapshot) => on_refreshed(app, *snapshot),
        Outcome::Failed(_) | Outcome::Info(_) => {
            let step = flows::apply(&event);
            return follow(app, step);
        }
        Outcome::UnlockResult(_) => return on_unlock_result(app, &event),
        Outcome::SetupDone(_) | Outcome::SshKeysLoaded(_) | Outcome::SshKeyChanged(_) => {
            if let Screen::Setup(state) = &mut app.screen {
                let step = state.flow.apply(&event);
                // The boxes and the selection follow the page the flow is on now.
                match (&event, &mut state.step) {
                    (Outcome::SetupDone(Ok(())), _) => state.step = SetupState::keys().step,
                    (Outcome::SetupDone(Err(_)), _) => state.step = SetupState::passphrase().step,
                    (Outcome::SshKeysLoaded(_), SetupStep::Keys { selected, .. }) => {
                        *selected = (*selected).min(state.flow.keys.len().saturating_sub(1));
                    }
                    _ => {}
                }
                return follow(app, step);
            }
        }
        Outcome::BackupsLoaded(_)
        | Outcome::BackupCreated(_)
        | Outcome::BackupInspected(_)
        | Outcome::BackupRestored(_)
        | Outcome::UsbLoaded(_) => return on_backup_result(app, event),
        Outcome::OnionLoaded(_)
        | Outcome::OnionChanged(_)
        | Outcome::RestoreDrives(_)
        | Outcome::RestoreBackups(_)
        | Outcome::RestoreInspected(_)
        | Outcome::RestoreFinished(_)
        | Outcome::RollbacksLoaded(_)
        | Outcome::RolledBack(_) => return more::on_result(app, event),
        Outcome::NetworkLoaded { .. }
        | Outcome::WifiChanged { .. }
        | Outcome::WifiScanned(_)
        | Outcome::BluetoothChanged { .. } => return net::on_result(app, event),
        Outcome::DetailLoaded(_) | Outcome::GateLoaded(_) | Outcome::UpdateReviewLoaded { .. } => {
            let step = install::apply(&event);
            return follow(app, step);
        }
        Outcome::InstallProgress(Some(report)) => {
            // A review still on screen when a report that is not a cancel arrives becomes the
            // install's page, as the install's own page does.
            let on_install_page = match &app.screen {
                Screen::Installing(_) => true,
                Screen::Review(_) => report.outcome != InstallOutcome::Cancelled,
                _ => false,
            };
            let step = install::progress(&report, &app.data.capabilities, on_install_page);
            return follow(app, step);
        }
        Outcome::InstallProgress(None) => {}
        Outcome::UpdatesChecked(report) => {
            let summary = report.summary(app.update_rows().len(), &app.data.feeds);
            let kind = match (summary.trouble, summary.found) {
                (true, _) => NoticeKind::Error,
                (false, 0) => NoticeKind::Ok,
                (false, _) => NoticeKind::Info,
            };
            notice(app, kind, summary.text);
        }
        Outcome::FeedProbed(_) => {
            if let Screen::AddFeed(state) = &mut app.screen {
                match sources::apply(&event) {
                    // Why the address gave no feed is said beside the box it was typed in.
                    Step::Failed(refusal) => {
                        if let AddFeedState::Address { error, .. } = state {
                            *error = Some(refusal.text);
                        }
                    }
                    step => return follow(app, step),
                }
            }
        }
        Outcome::FeedTrusted(_) | Outcome::KeyChangeLoaded(_) | Outcome::NewKeyTrusted(_) => {
            let step = sources::apply(&event);
            return follow(app, step);
        }
        Outcome::SettingsLoaded { .. } => {
            // A finished install is replaced so that Esc does not return to its progress list.
            if matches!(app.screen, Screen::Installing(_)) {
                pop(app);
            }
            let step = settings::apply(&event);
            return follow(app, step);
        }
        // The catalog for suggestions and the picker, and what each reference resolves to. The
        // form may be under the picker, or under a failed save's page.
        Outcome::ReferencesLoaded { .. } | Outcome::SettingPreviewed { .. } => {
            for screen in std::iter::once(&mut app.screen).chain(app.stack.iter_mut()) {
                if let Screen::Settings(state) = screen {
                    state.flow.apply(&event);
                }
            }
            if let Screen::Reference(picker) = &mut app.screen {
                let picked = picker.flow.apply(&event);
                return on_picked(app, picked);
            }
        }
        Outcome::Granted { .. } => {
            if let Screen::Reference(picker) = &mut app.screen {
                let picked = picker.flow.apply(&event);
                return on_picked(app, picked);
            }
            let step = settings::apply(&event);
            return follow(app, step);
        }
        Outcome::SettingsApplied(_) | Outcome::SettingsRolledBack { .. } => {
            if let Screen::Settings(state) = &mut app.screen {
                let step = state.flow.apply(&event);
                return follow(app, step);
            }
        }
        Outcome::UpdatesApplied(report) => {
            if matches!(app.screen, Screen::Review(_) | Screen::UpdateResults(_)) {
                pop(app);
            }
            push(app, Screen::UpdateResults(UpdateResultsState { report, choice: 0 }));
        }
        Outcome::AutoUpdateSaved(result) => match result {
            Ok(()) => {
                if matches!(app.screen, Screen::AutoUpdate(_)) {
                    pop(app);
                }
                notice(app, NoticeKind::Ok, "Auto-update choices saved.");
            }
            Err(text) => fail(app, text),
        },
        Outcome::LogsLoaded { .. } => {
            if let Screen::Logs(state) = &mut app.screen {
                let shown = app.data.log_sources.get(state.selected).map(String::as_str);
                let step = state.flow.apply(&event, shown);
                return follow(app, step);
            }
        }
        Outcome::DriveResult(_) => {
            let step = drive::apply(&event);
            return follow(app, step);
        }
        Outcome::ExposureSet { after, result } => return web::on_exposure_set(app, after, result),
        Outcome::PublicHostSet { result, then } => return web::on_public_host_set(app, result, then),
        Outcome::Reachability { site, going_public, result } => {
            web::on_reachability(app, site, going_public, result);
        }
        Outcome::ProviderAdded(result) => return web::on_provider_added(app, result),
        Outcome::ProvidersLoaded(result) => web::on_providers_loaded(app, result),
        Outcome::ProviderRemoved(result) => return web::on_provider_removed(app, result),
        Outcome::ProviderRefreshed(result) => return web::on_provider_refreshed(app, result),
        Outcome::AcmeSaved(result) => match result {
            Ok(()) => {
                if let Screen::WebDomain(state) = &mut app.screen {
                    state.error = None;
                    state.dns01_first.clear();
                    state.dns01_second.clear();
                }
                notice(app, NoticeKind::Ok, "Certificate settings saved and applied.");
            }
            Err(text) => {
                if let Screen::WebDomain(state) = &mut app.screen {
                    state.error = Some(text);
                } else {
                    fail(app, text);
                }
            }
        },
        Outcome::LocalNamesChecked(result) => {
            if let Screen::WebDomain(state) = &mut app.screen {
                state.checks = Some(result);
            }
        }
        Outcome::DocumentLoaded(_) => {
            let step = documents::apply(&event);
            return follow(app, step);
        }
        Outcome::BundleInstalled(result) => owner::on_bundle_installed(app, result),
        Outcome::OwnerPageLoaded { package, result } => owner::on_owner_page(app, package, result),
    }
    Vec::new()
}

fn on_unlock_result(app: &mut App, outcome: &Outcome) -> Vec<Effect> {
    let ticks = app.ticks;
    let Screen::Unlock(state) = &mut app.screen else {
        return Vec::new();
    };
    let failures = state.failures;
    let step = state.flow.apply(outcome, &mut app.data);
    if state.failures > failures {
        state.retry_at_tick = ticks + state.retry_delay_seconds() * TICKS_PER_SECOND;
    }
    follow(app, step)
}

fn backup_input(step: &mut BackupStep) -> Option<&mut Input> {
    match step {
        BackupStep::Create { device, first, second, field } => Some(match field {
            0 => device,
            1 => first,
            _ => second,
        }),
        BackupStep::Unlock { passphrase, .. } => Some(passphrase),
        BackupStep::Confirm { typed, device, field, .. } => Some(if *field == 0 { typed } else { device }),
        BackupStep::List | BackupStep::Restored(_) => None,
    }
}

/// Backups and restore. A restore asks for the backup passphrase, shows what the verified backup
/// holds and which services stop, then needs the typed word. The helper checks the word again.
/// Tab moves between the device's backups and a USB drive's. On first boot only the USB drive is
/// offered, and the restore sets the whole device up.
fn backup_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Backup(state) = &mut app.screen else {
        return Vec::new();
    };
    if state.busy {
        return Vec::new();
    }
    let device_set_up = state.device_set_up;
    let fresh = state.fresh;
    let mut effects = Vec::new();
    let mut leave = false;
    let mut finished_fresh = false;
    let mut next = None;
    let usb_device = state.usb.as_ref().filter(|u| u.list.is_some()).map(|u| u.drive.device.clone());
    match &mut state.step {
        BackupStep::List => match key.code {
            KeyCode::Tab if !fresh && usb_device.is_some() => state.usb_focused = !state.usb_focused,
            KeyCode::Down | KeyCode::Up => {
                let down = key.code == KeyCode::Down;
                if state.usb_focused {
                    let len = state.usb_list().len();
                    step(&mut state.usb_selected, len, down);
                } else {
                    step(&mut state.selected, state.list.len(), down);
                }
            }
            KeyCode::Char(c @ ('n' | 'u')) if !fresh => {
                if c == 'u' && usb_device.is_none() {
                    state.error = Some("Plug in a USB drive formatted as FAT32, exFAT or ext4 first.".to_owned());
                } else {
                    state.target = if c == 'u' { usb_device.clone() } else { None };
                    let field = if device_set_up { 0 } else { 1 };
                    next = Some(BackupStep::Create {
                        device: Input::secret(),
                        first: Input::secret(),
                        second: Input::secret(),
                        field,
                    });
                }
            }
            KeyCode::Char('r') | KeyCode::Enter => {
                let chosen = if state.usb_focused {
                    state.usb_list().get(state.usb_selected).map(|b| b.name.clone())
                } else {
                    state.list.get(state.selected).map(|b| b.name.clone())
                };
                if let Some(name) = chosen {
                    state.target = if state.usb_focused { usb_device.clone() } else { None };
                    next = Some(BackupStep::Unlock { name, passphrase: Input::secret() });
                }
            }
            KeyCode::Char('g') => {
                state.busy = true;
                effects.push(Effect::LoadBackups);
            }
            KeyCode::Esc => leave = true,
            _ => {}
        },
        BackupStep::Create { device, first, second, field } => match key.code {
            KeyCode::Esc => next = Some(BackupStep::List),
            KeyCode::Tab | KeyCode::Down => *field = if *field >= 2 { if device_set_up { 0 } else { 1 } } else { *field + 1 },
            KeyCode::Enter if *field < 2 => *field += 1,
            KeyCode::Enter => {
                let backup = first.secret_value();
                if backup.expose().chars().count() < MIN_BACKUP_PASSPHRASE_CHARS {
                    state.error = Some(format!("A backup passphrase needs at least {MIN_BACKUP_PASSPHRASE_CHARS} characters."));
                } else if backup != second.secret_value() {
                    state.error = Some("The two backup passphrases differ. Type them again.".to_owned());
                    first.clear();
                    second.clear();
                    *field = 1;
                } else if device_set_up && device.is_empty() {
                    state.error = Some("Enter the device passphrase, so the backup includes the device key.".to_owned());
                    *field = 0;
                } else {
                    state.error = None;
                    state.busy = true;
                    let device_passphrase = device_set_up.then(|| device.take_secret());
                    second.clear();
                    effects.push(Effect::CreateBackup {
                        device_passphrase,
                        backup_passphrase: first.take_secret(),
                        usb: state.target.clone(),
                    });
                }
            }
            _ => {
                let input = match field {
                    0 => device,
                    1 => first,
                    _ => second,
                };
                edit_input(input, key);
            }
        },
        BackupStep::Unlock { name, passphrase } => match key.code {
            KeyCode::Esc => next = Some(BackupStep::List),
            KeyCode::Enter if !passphrase.is_empty() => {
                state.busy = true;
                state.error = None;
                effects.push(Effect::InspectBackup {
                    name: name.clone(),
                    passphrase: passphrase.secret_value(),
                    usb: state.target.clone(),
                });
            }
            _ => {
                edit_input(passphrase, key);
            }
        },
        BackupStep::Confirm { name, passphrase, contents, typed, device, field } => {
            // A device that is not set up always needs a passphrase of its own.
            let asks_device = contents.has_device_key || fresh;
            match key.code {
                KeyCode::Esc => next = Some(BackupStep::List),
                KeyCode::Tab if asks_device => *field = 1 - *field,
                KeyCode::Enter => {
                    if typed.text() != RESTORE_WORD {
                        state.error = Some(format!("Type {RESTORE_WORD} to confirm."));
                        *field = 0;
                    } else if asks_device && device.secret_value().expose().chars().count() < MIN_PASSPHRASE_CHARS {
                        state.error = Some(format!(
                            "Choose a new device passphrase of at least {MIN_PASSPHRASE_CHARS} characters for the backup's key."
                        ));
                        *field = 1;
                    } else {
                        state.error = None;
                        state.busy = true;
                        let new_device_passphrase = asks_device.then(|| device.take_secret());
                        effects.push(Effect::RestoreBackup {
                            name: name.clone(),
                            passphrase: passphrase.clone(),
                            new_device_passphrase,
                            typed: typed.text().to_owned(),
                            usb: state.target.clone(),
                        });
                    }
                }
                _ => {
                    edit_input(if *field == 0 { typed } else { device }, key);
                }
            }
        }
        BackupStep::Restored(_) => {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                if fresh {
                    finished_fresh = true;
                } else {
                    next = Some(BackupStep::List);
                    state.busy = true;
                    effects.push(Effect::LoadBackups);
                }
            }
        }
    }
    if let Some(step) = next {
        state.error = None;
        state.step = step;
    }
    if finished_fresh {
        // Set up from the backup. SSH keys are offered next, as after a first-boot setup.
        pop(app);
        if let Screen::Setup(setup) = &mut app.screen {
            *setup = SetupState::keys();
        }
        return vec![Effect::LoadSshKeys, Effect::Refresh];
    }
    if leave {
        pop(app);
        effects.push(Effect::Refresh);
    }
    effects
}

/// The helper enforces the same minimum. Checking here saves a round trip and a KDF run.
const MIN_BACKUP_PASSPHRASE_CHARS: usize = 12;

fn on_backup_result(app: &mut App, event: Outcome) -> Vec<Effect> {
    let Screen::Backup(state) = &mut app.screen else {
        return Vec::new();
    };
    match event {
        Outcome::BackupsLoaded(Ok(list)) => {
            state.selected = state.selected.min(list.len().saturating_sub(1));
            state.list = list;
        }
        Outcome::UsbLoaded(result) => {
            state.busy = false;
            state.usb = result.ok();
            let len = state.usb_list().len();
            state.usb_selected = state.usb_selected.min(len.saturating_sub(1));
            if len == 0 && !state.fresh {
                state.usb_focused = false;
            }
        }
        Outcome::BackupCreated(Ok(created)) => {
            state.step = BackupStep::List;
            state.busy = true;
            let place = if state.target.take().is_some() { " to the USB drive" } else { "" };
            let mut text = format!("Backup {} written{place}.", created.info.name);
            if !created.locked.is_empty() {
                text.push_str(&format!(" Locked storage was left out: {}.", created.locked.join(", ")));
            }
            notice(app, NoticeKind::Ok, text);
            return vec![Effect::LoadBackups];
        }
        Outcome::BackupInspected(Ok(contents)) => {
            state.busy = false;
            if let BackupStep::Unlock { name, passphrase } = &mut state.step {
                state.step = BackupStep::Confirm {
                    name: name.clone(),
                    passphrase: passphrase.take_secret(),
                    contents,
                    typed: Input::plain(""),
                    device: Input::secret(),
                    field: 0,
                };
            }
        }
        Outcome::BackupRestored(Ok(report)) => {
            state.busy = false;
            state.step = BackupStep::Restored(report);
        }
        Outcome::BackupsLoaded(Err(e)) => {
            // On first boot the device has no backups of its own, which is no error.
            if !state.fresh {
                state.error = Some(e);
            }
        }
        Outcome::BackupCreated(Err(e)) | Outcome::BackupInspected(Err(e)) | Outcome::BackupRestored(Err(e)) => {
            state.busy = false;
            state.error = Some(e);
        }
        _ => {}
    }
    Vec::new()
}

/// A mouse event. A click does what its target says; a move updates the hover; the wheel scrolls
/// like the arrow keys. While the terminal is too small nothing is clickable.
fn on_mouse(app: &mut App, kind: MouseKind, column: u16, row: u16) -> Vec<Effect> {
    if app.too_small() {
        return Vec::new();
    }
    let hit = crate::hit::hit_at(&app.hits.borrow(), column, row);
    match kind {
        MouseKind::Moved => {
            app.hover = hit.map(|hit| hit.rect);
            Vec::new()
        }
        MouseKind::ScrollUp | MouseKind::ScrollDown => {
            app.hover = None;
            let code = if kind == MouseKind::ScrollUp { KeyCode::Up } else { KeyCode::Down };
            if app.overlay.is_some() || app.modal.is_some() {
                return on_key(app, KeyEvent::from(code));
            }
            on_key(app, KeyEvent::from(code))
        }
        MouseKind::Down => {
            let Some(hit) = hit else { return Vec::new() };
            app.hover = None;
            if pending_here(app) == Some(&hit.click) {
                return Vec::new();
            }
            let before = app.screen.id();
            let modal = app.modal.is_some();
            let effects = click(app, hit.click.clone());
            // A button that started work shows it until the work is done (`App::pending`).
            if hit.button.is_some() && !effects.is_empty() && !modal && app.modal.is_none() && app.screen.id() == before {
                app.pending = Some(Pending { click: hit.click, screen: before });
            }
            effects
        }
    }
}

/// A click that picks a row, a field or a choice: moves the selection there, as the arrow keys
/// would, so whatever key the click then presses acts on what was clicked.
fn pick(app: &mut App, index: usize) {
    let sections = match &app.screen {
        Screen::AppPage(state) => app.data.app_sections(&state.name).len(),
        _ => APP_SECTIONS.len(),
    };
    let counts = (
        app.home_items().len(),
        services::installed_apps(&app.data).len(),
        app.data.feeds.len(),
        app.update_rows().len(),
        app.data.log_sources.len(),
        sections,
    );
    match &mut app.screen {
        Screen::Home { selected } => *selected = index.min(counts.0.saturating_sub(1)),
        Screen::NoDrive { choice } => *choice = index.min(3),
        Screen::Apps { selected } => *selected = index.min(counts.1.saturating_sub(1)),
        Screen::AppPage(state) => state.selected = index.min(counts.5.saturating_sub(1)),
        Screen::AppRemove(state) => state.confirm_focused = index == 1,
        Screen::System { selected } => *selected = index.min(SYSTEM_ROWS.len() - 1),
        Screen::Search(state) => {
            state.list_focused = true;
            state.selected = index;
        }
        Screen::Feeds { selected, .. } => *selected = index.min(counts.2.saturating_sub(1)),
        Screen::AddFeed(AddFeedState::Confirm { trust_focused, .. }) => *trust_focused = index == 1,
        Screen::FeedKey(state) => state.choice = index.min(2),
        Screen::Review(state) => state.confirm_focused = index == 1,
        Screen::Capability(state) => state.choice = index.min(2),
        Screen::Settings(state) if state.editing.is_none() && state.editor.is_none() => {
            state.selected = index.min(state.form.fields.len().saturating_sub(1));
        }
        Screen::Reference(state) => {
            state.list_focused = true;
            state.selected = index;
        }
        Screen::Updates(state) => state.selected = index.min(counts.3.saturating_sub(1)),
        Screen::AutoUpdate(state) => state.selected = index,
        Screen::UpdateResults(state) => state.choice = index.min(3),
        Screen::Logs(state) => state.selected = index.min(counts.4.saturating_sub(1)),
        Screen::Rollbacks(state) => state.selected = index,
        Screen::Bundle(_) | Screen::OwnerPage(_) => owner::pick(app, index),
        _ => web::pick(app, index),
    }
}

/// A click on a text box: gives it the cursor, as `/` (or `w` for the update window) would from
/// the rest of the screen, but presses nothing, so a box that already has the cursor keeps what
/// was typed.
fn focus_input_box(app: &mut App) -> Vec<Effect> {
    match &mut app.screen {
        Screen::Search(state) => state.list_focused = false,
        Screen::Reference(state) => state.list_focused = false,
        Screen::Logs(state) => state.finding = true,
        Screen::AutoUpdate(state) if state.editing_window.is_none() => {
            return on_key(app, KeyEvent::from(KeyCode::Char('w')));
        }
        _ => {}
    }
    Vec::new()
}

fn click(app: &mut App, click: crate::hit::Click) -> Vec<Effect> {
    use crate::hit::Click;
    match click {
        Click::Tab(tab) => {
            if app.modal.is_some() || app.overlay.is_some() {
                return Vec::new();
            }
            app.notice = None;
            switch_tab(app, tab)
        }
        Click::Key(code) => on_key(app, KeyEvent::from(code)),
        Click::Ctrl(c) => on_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
        Click::Focus => {
            if app.modal.is_some() || app.overlay.is_some() {
                return Vec::new();
            }
            app.notice = None;
            focus_input_box(app)
        }
        Click::Pick { index, then } => {
            app.notice = None;
            match &mut app.modal {
                Some(modal) => modal.confirm_focused = index == 1,
                None => pick(app, index),
            }
            match then {
                Some(code) => on_key(app, KeyEvent::from(code)),
                None => Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_parsing_accepts_only_two_different_valid_times() {
        assert_eq!(parse_window("03:00 to 05:00"), Some(("03:00".to_owned(), "05:00".to_owned())));
        assert_eq!(parse_window("23:00 to 01:00"), Some(("23:00".to_owned(), "01:00".to_owned())));
        assert_eq!(parse_window("03:00 to 03:00"), None);
        assert_eq!(parse_window("25:00 to 05:00"), None);
        assert_eq!(parse_window("3am to 5am"), None);
    }

    fn mail_form() -> SettingsForm {
        let field = |key: &str, kind: FieldKind, required: bool, value: &str| Field {
            key: key.to_owned(),
            label: key.to_owned(),
            help: String::new(),
            requirement: String::new(),
            required,
            kind,
            value: FieldValue::Text(value.to_owned()),
        };
        let mail = FieldKind::Choice(vec!["off".to_owned(), "smtp".to_owned(), "tuta".to_owned()]);
        SettingsForm {
            package: "shop".to_owned(),
            fields: vec![
                field("mail", mail, false, "off"),
                field("smtp_port", FieldKind::Number { min: Some(1), max: Some(65535) }, false, ""),
                field("smtp_host", FieldKind::Text, false, "smtp.example.com"),
                field("public_url", FieldKind::Url, true, "https://shop.example"),
            ],
            public_settings: Vec::new(),
        }
    }

    fn save(app: &mut App) -> Vec<Effect> {
        update(app, Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)))
    }

    /// An empty optional int (handicrafts' SMTP port) used to fail "Must be a whole number" and
    /// block every save, such as switching Email from off to smtp to tuta.
    #[test]
    fn an_empty_optional_field_never_blocks_saving() {
        let mut app = App::new();
        let mut state = SettingsState::new(mail_form());
        assert_eq!(state.error_for(1), None, "an empty optional int is fine");
        state.draft[0] = FieldValue::Text("tuta".to_owned());
        app.screen = Screen::Settings(state);
        let effects = save(&mut app);
        assert_eq!(
            effects,
            vec![Effect::ApplySettings {
                package: "shop".to_owned(),
                values: vec![("mail".to_owned(), Some(FieldValue::Text("tuta".to_owned())))],
            }],
            "{:?}",
            app.notice
        );
    }

    /// Clearing a stored optional value sends a reset, not "", which the helper would refuse.
    /// A required field still has to be filled in.
    #[test]
    fn an_emptied_optional_field_is_reset_and_a_required_one_is_still_refused() {
        let mut app = App::new();
        let mut state = SettingsState::new(mail_form());
        state.draft[2] = FieldValue::Text(String::new());
        app.screen = Screen::Settings(state);
        assert_eq!(
            save(&mut app),
            vec![Effect::ApplySettings { package: "shop".to_owned(), values: vec![("smtp_host".to_owned(), None)] }]
        );
        let Screen::Settings(state) = &mut app.screen else { panic!("still on Settings") };
        state.draft[3] = FieldValue::Text(String::new());
        assert!(state.error_for(3).is_some(), "a required URL cannot be empty");
        assert!(save(&mut app).is_empty());
    }
}

