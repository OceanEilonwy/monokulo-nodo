//! The pure state machine: `update(state, event)` applies one event and lists the I/O it needs.
//!
//! Nothing here touches a terminal or a backend. Safety rules that a test must be able to see,
//! such as safe default focus and typed confirmation, live in this file.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nodo_api::{ApiError, Exposure, Response, ServiceState};

use crate::app::*;
use crate::model::*;
use crate::secret::Input;

#[path = "update_more.rs"]
mod more;
#[path = "update_net.rs"]
mod net;
#[path = "update_web.rs"]
mod web;

pub fn update(app: &mut App, event: Event) -> Vec<Effect> {
    match event {
        Event::Key(key) => on_key(app, key),
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
        other => on_result(app, other),
    }
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

fn logs_state(selected: usize) -> LogsState {
    LogsState {
        selected,
        view: LogView::default(),
        errors_only: false,
        follow: true,
        find: Input::plain(""),
        finding: false,
        app: None,
    }
}

pub fn search_state(focus_query: bool) -> SearchState {
    SearchState { query: Input::plain(""), list_focused: !focus_query, selected: 0, feed_filter: None }
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
    let is_app = app.data.services.iter().any(|svc| svc.id == source);
    let mut state = logs_state(selected);
    state.app = is_app.then(|| source.to_owned());
    push(app, Screen::Logs(state));
    app.data.log_sources.get(selected).map(|name| Effect::LoadLogs(name.clone())).into_iter().collect()
}

/// The device logs under System: every source, problems first.
fn open_device_logs(app: &mut App) -> Vec<Effect> {
    let mut state = logs_state(0);
    state.errors_only = true;
    push(app, Screen::Logs(state));
    app.data.log_sources.first().map(|name| Effect::LoadLogs(name.clone())).into_iter().collect()
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
    }
}

/// The installed apps: every service except the device's own parts.
pub fn installed_apps(app: &App) -> Vec<String> {
    app.data.services.iter().map(|svc| svc.id.clone()).collect()
}

fn apps_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    if let Some(effects) = tab_key(app, key) {
        return effects;
    }
    let names = installed_apps(app);
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
    let name = state.name.clone();
    if state.more_open {
        // The folded page actions, opened from "⋯ More" at 80 columns.
        state.more_open = false;
        return match key.code {
            KeyCode::Char('r') => vec![Effect::Restart(name)],
            KeyCode::Char('s') => vec![Effect::Stop(name)],
            KeyCode::Char('x') => {
                push(app, Screen::AppRemove(AppRemoveState { name, confirm_focused: false }));
                Vec::new()
            }
            _ => Vec::new(),
        };
    }
    let has_update = app.data.updates.iter().any(|item| item.name == name && !item.held);
    match key.code {
        KeyCode::Char('?') => app.overlay = Some(Overlay { doc: help_document(), scroll: 0 }),
        KeyCode::Esc => pop(app),
        KeyCode::Down | KeyCode::Up => step(&mut state.selected, APP_SECTIONS.len(), key.code == KeyCode::Down),
        KeyCode::Char('m') => state.more_open = true,
        KeyCode::Enter => match APP_SECTIONS[state.selected.min(APP_SECTIONS.len() - 1)] {
            // An app without a web page shows Hosting as a dimmed row that says so; Enter on
            // it stays put rather than flash a notice as if it had gone somewhere.
            "Hosting" if !app.data.web.sites.iter().any(|site| site.package == name) => {}
            "Hosting" => return web::hosting_from_detail(app, &name),
            "Settings" => return vec![Effect::OpenSettings(name)],
            _ => return open_logs(app, &name),
        },
        KeyCode::Char('h') => return web::hosting_from_detail(app, &name),
        KeyCode::Char('l') => return open_logs(app, &name),
        KeyCode::Char('u') if has_update => {
            let screen = updates_screen(app);
            push(app, screen);
            if let Screen::Updates(updates) = &mut app.screen {
                updates.checked = vec![name];
            }
        }
        KeyCode::Char('u') if app.is_locked() => return open_unlock(app),
        KeyCode::Char('u') => notice(app, NoticeKind::Info, format!("{name} is up to date.")),
        KeyCode::Char('r') => return vec![Effect::Restart(name)],
        KeyCode::Char('s') => {
            let state = app.data.services.iter().find(|svc| svc.id == name).map(|svc| svc.state);
            return match state {
                Some(ServiceState::Running) => vec![Effect::Stop(name)],
                Some(ServiceState::Locked) => {
                    fail(app, format!("{name} is locked. Press u to unlock this device."));
                    Vec::new()
                }
                Some(ServiceState::Waiting) => {
                    fail(app, format!("{name} is waiting for another app it depends on."));
                    Vec::new()
                }
                _ => vec![Effect::Start(name)],
            };
        }

        KeyCode::Char('x') => push(app, Screen::AppRemove(AppRemoveState { name, confirm_focused: false })),
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
            let name = state.name.clone();
            pop(app);
            pop(app);
            return vec![Effect::RemovePackage(name)];
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
    let nodo_update = app.data.updates.iter().any(|item| item.name == crate::model::SYSTEM_BASE && !item.held);
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
                app.overlay = Some(Overlay { doc: about_document(app), scroll: 0 });
            }
        },
        KeyCode::Char('u') => return open_unlock(app),
        KeyCode::Char('k') => {
            push(app, Screen::Setup(SetupState::keys()));
            return vec![Effect::LoadSshKeys];
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

/// System › About this Nodo: what the device reports about itself.
fn about_document(app: &App) -> Document {
    let mut lines: Vec<String> = app.data.overview.device.iter().map(|(k, v)| format!("{k:<16} {v}")).collect();
    if let Some(note) = &app.data.system_note {
        lines.push(String::new());
        lines.push(note.clone());
    }
    Document { title: "About this Nodo".to_owned(), lines }
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
        Screen::WebExposure(state) if state.focus() == HostingFocus::Address && !state.picking => Some(&mut state.sub),
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
                return modal_action(app, modal.action);
            }
        }
        _ => {}
    }
    Vec::new()
}

fn modal_action(app: &mut App, action: ModalAction) -> Vec<Effect> {
    match action {
        ModalAction::RemoveFeed(name) => {
            if matches!(app.screen, Screen::FeedKey(_)) {
                pop(app);
            }
            vec![Effect::RemoveFeed(name)]
        }
        ModalAction::AllSitesOffline => app
            .data
            .web
            .sites
            .iter()
            .filter(|site| site.exposure != Exposure::default())
            .map(|site| Effect::SetExposure {
                site: site.package.clone(),
                exposure: Exposure::default(),
                after: ExposureAfter::Stay, confirmation: None
            })
            .collect(),
        ModalAction::RepairDrive => vec![Effect::Drive(DriveAction::Repair)],
        ModalAction::RollBack { package, to } => more::roll_back(app, package, to),
        action @ (ModalAction::JoinOpenWifi(_)
        | ModalAction::ForgetWifi(_)
        | ModalAction::WifiOff
        | ModalAction::RemoveBluetooth(_)) => net::modal_action(app, action),
        ModalAction::RemoveProvider(id) => vec![Effect::RemoveProvider(id)],
        ModalAction::RemoveSshKey(fingerprint) => {
            if let Screen::Setup(state) = &mut app.screen {
                state.busy = true;
            }
            vec![Effect::RemoveSshKey(fingerprint)]
        }
        ModalAction::DiscardSettings | ModalAction::DiscardAutoUpdate => {
            pop(app);
            Vec::new()
        }
    }
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
        Screen::Installing(snapshot) if snapshot.outcome == InstallOutcome::InProgress => {
            vec![Effect::PollInstall]
        }
        Screen::Logs(state) if state.follow && app.ticks % (2 * TICKS_PER_SECOND) == 0 => app
            .data
            .log_sources
            .get(state.selected)
            .map(|source| Effect::LoadLogs(source.clone()))
            .into_iter()
            .collect(),
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
            push(app, Screen::Setup(SetupState::keys()));
            return vec![Effect::LoadSshKeys];
        }
        KeyCode::Char('b') => return open_backups(app),
        KeyCode::Char('r') => return more::open_rollbacks(app),
        KeyCode::Char('w') => return net::open_network(app),
        _ => {}
    }
    Vec::new()
}

fn setup_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Setup(SetupState { step: stage, error, busy }) = &mut app.screen else {
        return Vec::new();
    };
    if *busy {
        return Vec::new();
    }
    let mut leave = false;
    let mut restore = false;
    let mut effects = Vec::new();
    match stage {
        SetupStep::Passphrase { first, second, confirming } => match key.code {
            // A new device can instead be rebuilt from a backup on a USB drive.
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                first.clear();
                second.clear();
                *confirming = false;
                *error = None;
                restore = true;
            }
            KeyCode::Enter if !*confirming => {
                if first.secret_value().expose().chars().count() < MIN_PASSPHRASE_CHARS {
                    *error = Some(format!("Use at least {MIN_PASSPHRASE_CHARS} characters."));
                } else {
                    *error = None;
                    *confirming = true;
                }
            }
            KeyCode::Enter => {
                if first.secret_value() != second.secret_value() {
                    *error = Some("The two passphrases differ. Type them again.".to_owned());
                    first.clear();
                    second.clear();
                    *confirming = false;
                } else {
                    *error = None;
                    *busy = true;
                    second.clear();
                    effects.push(Effect::InitEncryption(first.take_secret()));
                }
            }
            KeyCode::Esc if *confirming => {
                second.clear();
                *confirming = false;
            }
            _ => {
                edit_input(if *confirming { second } else { first }, key);
            }
        },
        SetupStep::Keys { keys, selected, adding } => {
            if let Some(input) = adding {
                match key.code {
                    KeyCode::Enter => {
                        let line = input.text().trim().to_owned();
                        *adding = None;
                        if !line.is_empty() {
                            *busy = true;
                            effects.push(Effect::AddSshKey(line));
                        }
                    }
                    KeyCode::Esc => *adding = None,
                    _ => {
                        edit_input(input, key);
                    }
                }
                return effects;
            }
            match key.code {
                KeyCode::Down | KeyCode::Up => step(selected, keys.len(), key.code == KeyCode::Down),
                KeyCode::Char('a') => *adding = Some(Input::plain("")),
                KeyCode::Char('d') => {
                    if let Some(chosen) = keys.get(*selected) {
                        let fingerprint = chosen.fingerprint.clone();
                        app.modal = Some(Modal {
                            title: "Remove this SSH key?".to_owned(),
                            body: vec![
                                format!("{} {}", chosen.algorithm, chosen.comment),
                                fingerprint.clone(),
                                "It will no longer be able to log in.".to_owned(),
                            ],
                            confirm_label: "Remove".to_owned(),
                            confirm_focused: false,
                            action: ModalAction::RemoveSshKey(fingerprint),
                        });
                    }
                }
                KeyCode::Esc | KeyCode::Enter => leave = true,
                _ => {}
            }
        }
    }
    if restore {
        return more::open_restore(app);
    }
    if leave {
        pop(app);
        effects.push(Effect::Refresh);
    }
    effects
}

fn open_unlock(app: &mut App) -> Vec<Effect> {
    if app.is_locked() {
        push(app, Screen::Unlock(UnlockState::default()));
    } else {
        notice(app, NoticeKind::Info, "This device is already unlocked.");
    }
    Vec::new()
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
            0 => return vec![Effect::Drive(DriveAction::Retry)],
            1 => return vec![Effect::Drive(DriveAction::ReadOnlyCheck)],
            2 => {
                app.modal = Some(Modal {
                    title: "Repair the filesystem?".to_owned(),
                    body: vec![
                        "Repairing rewrites damaged parts of the filesystem and can lose data.".to_owned(),
                        "Run the read-only check first if you have not. Chain data can be synced again."
                            .to_owned(),
                    ],
                    confirm_label: "Repair".to_owned(),
                    confirm_focused: false,
                    action: ModalAction::RepairDrive,
                });
            }
            _ => return vec![Effect::Drive(DriveAction::Diagnostics)],
        },
        KeyCode::Char('l') => return open_logs(app, "System"),
        KeyCode::Char('!') => return vec![Effect::Shell],
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
    let feeds: Vec<String> = app.data.feeds.iter().map(|feed| feed.name.clone()).collect();
    let Screen::Search(state) = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        // The first Esc clears a search or a source filter, the next goes back to Apps.
        KeyCode::Esc if state.query.is_empty() && state.feed_filter.is_none() => pop(app),
        KeyCode::Esc => {
            state.query.clear();
            state.feed_filter = None;
            state.list_focused = false;
            state.selected = 0;
        }
        KeyCode::Char('s') if state.list_focused => return open_sources(app, 0),
        KeyCode::Enter => {
            if let Some(id) = results.get(state.selected) {
                return vec![Effect::LoadDetail(id.clone())];
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
                return vec![Effect::CheckInstall(id.clone())];
            }
        }
        KeyCode::Char('f') => {
            let position = state
                .feed_filter
                .as_ref()
                .and_then(|current| feeds.iter().position(|feed| feed == current));
            state.feed_filter = match position {
                None => feeds.first().cloned(),
                Some(index) => feeds.get(index + 1).cloned(),
            };
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
        KeyCode::Char('i') if !detail.installed => vec![Effect::CheckInstall(detail.id.clone())],
        KeyCode::Char('o') if detail.installed => {
            let name = detail.id.name.clone();
            switch_tab(app, Tab::Apps);
            push(app, Screen::AppPage(AppPageState::new(&name)));
            Vec::new()
        }
        KeyCode::Char('s') if detail.installed => vec![Effect::OpenSettings(detail.id.name.clone())],
        KeyCode::Char('v') => vec![Effect::ShowDocument(DocKind::Versions(detail.id.name.clone()))],
        KeyCode::Char('c') => vec![Effect::ShowDocument(DocKind::Changelog(detail.id.name.clone()))],
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
            KeyCode::Enter if input.is_empty() => {
                notice(app, NoticeKind::Info, "Enter the device passphrase to create its encrypted storage.");
            }
            KeyCode::Enter => {
                let passphrase = Some(input.take_secret());
                state.passphrase = None;
                let (id, approval) = (state.data.id.clone(), state.data.approval.clone());
                return vec![Effect::StartInstall { id, approval, passphrase }];
            }
            _ => {
                edit_input(input, key);
            }
        }
        return Vec::new();
    }
    match key.code {
        KeyCode::Left | KeyCode::Right => state.confirm_focused = !state.confirm_focused,
        KeyCode::Char('d') => {
            return vec![Effect::ShowDocument(DocKind::FileList(state.data.id.name.clone()))];
        }
        KeyCode::Esc => pop(app),
        KeyCode::Enter => {
            if !state.confirm_focused {
                pop(app);
                return Vec::new();
            }
            match state.purpose.clone() {
                ReviewPurpose::Install if state.data.needs_passphrase => {
                    state.passphrase = Some(Input::secret());
                }
                ReviewPurpose::Install => {
                    let (id, approval) = (state.data.id.clone(), state.data.approval.clone());
                    return vec![Effect::StartInstall { id, approval, passphrase: None }];
                }
                ReviewPurpose::Update { batch, mut pending_reviews, mut approvals } => {
                    if let Some(approval) = state.data.approval.clone() {
                        approvals.push((state.data.id.name.clone(), approval));
                    }
                    if pending_reviews.is_empty() {
                        return apply_batch(app, batch, approvals);
                    }
                    let name = pending_reviews.remove(0);
                    pop(app);
                    return vec![Effect::ReviewUpdate { name, batch, pending_reviews, approvals }];
                }
            }
        }
        _ => {}
    }
    Vec::new()
}

fn installing_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Installing(snapshot) = &app.screen else {
        return Vec::new();
    };
    let name = snapshot.id.name.clone();
    match (&snapshot.outcome, key.code) {
        (_, KeyCode::Char('v')) => vec![Effect::ShowDocument(DocKind::InstallLog(name))],
        (InstallOutcome::InProgress, KeyCode::Esc) => vec![Effect::CancelInstall],
        (InstallOutcome::Succeeded, KeyCode::Enter) => vec![Effect::OpenSettings(name)],
        (InstallOutcome::InProgress, _) => Vec::new(),
        (_, KeyCode::Esc | KeyCode::Enter) => {
            pop(app);
            vec![Effect::Refresh]
        }
        _ => Vec::new(),
    }
}

fn unlock_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let ticks = app.ticks;
    let Screen::Unlock(state) = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Esc => pop(app),
        KeyCode::Enter => {
            if ticks < state.retry_at_tick {
                let seconds = (state.retry_at_tick - ticks).div_ceil(TICKS_PER_SECOND);
                state.error = Some(format!("Please wait {} before trying again.", counted(seconds, "second")));
            } else if state.input.is_empty() {
                state.error = Some("Enter the device passphrase.".to_owned());
            } else {
                return vec![Effect::Unlock(state.input.take_secret())];
            }
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
        KeyCode::Char('r') => return vec![Effect::RefreshFeeds],
        KeyCode::Char('d') => *keys = !*keys,
        KeyCode::Char('e' | ' ') => {
            if let Some(feed) = current {
                return vec![Effect::ToggleFeed(feed.name.clone())];
            }
        }
        KeyCode::Char('x') => {
            if let Some(feed) = current {
                app.modal = Some(remove_feed_modal(&feed.name));
            }
        }
        KeyCode::Enter => {
            if let Some(feed) = current {
                if feed.status == FeedStatus::Blocked {
                    return vec![Effect::LoadKeyChange(feed.name.clone())];
                }
                let filter = feed.name.clone();
                return open_get_apps(app, Some(filter));
            }
        }
        _ => {}
    }
    Vec::new()
}

fn remove_feed_modal(name: &str) -> Modal {
    Modal {
        title: format!("Remove {name}?"),
        body: vec![
            "Apps you installed from it keep running; they just won't get updates until you add it back.".to_owned(),
        ],
        confirm_label: "Remove".to_owned(),
        confirm_focused: false,
        action: ModalAction::RemoveFeed(name.to_owned()),
    }
}

fn add_feed_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::AddFeed(state) = &mut app.screen else {
        return Vec::new();
    };
    match state {
        AddFeedState::Address { input, error } => match key.code {
            KeyCode::Esc => pop(app),
            KeyCode::Enter => {
                if input.is_empty() {
                    *error = Some("Enter the feed's address.".to_owned());
                } else {
                    return vec![Effect::ProbeFeed(input.text().trim().to_owned())];
                }
            }
            _ => {
                edit_input(input, key);
            }
        },
        AddFeedState::Confirm { probe, trust_focused } => match key.code {
            KeyCode::Left | KeyCode::Right => *trust_focused = !*trust_focused,
            KeyCode::Char('p') => {
                return vec![Effect::ShowDocument(DocKind::FeedPackages(probe.name.clone()))];
            }
            KeyCode::Esc => pop(app),
            KeyCode::Enter => {
                if *trust_focused {
                    return vec![Effect::TrustFeed(probe.clone())];
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
            KeyCode::Enter => {
                if input.text() == state.change.feed {
                    let typed = input.text().to_owned();
                    return vec![Effect::TrustNewKey { change: state.change.clone(), typed }];
                }
                state.error = Some(format!("That isn't the source's name. Type {} exactly.", state.change.feed));
            }
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
                let modal = remove_feed_modal(&state.change.feed);
                app.modal = Some(modal);
            }
        },
        _ => {}
    }
    Vec::new()
}

fn settings_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    // F4 and Ctrl-. open the picker even while typing, so they never reach the box.
    if opens_picker(key) {
        return open_reference_picker(app);
    }
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    if let Some(input) = state.editing.as_mut() {
        match key.code {
            KeyCode::Esc => state.editing = None,
            KeyCode::Enter => {
                let index = state.selected;
                let is_secret = input.is_secret();
                if is_secret {
                    // Leaving a secret empty keeps the stored one instead of erasing it.
                    if !input.is_empty() {
                        state.draft[index] = FieldValue::NewSecret(input.take_secret());
                    }
                } else {
                    state.draft[index] = FieldValue::Text(input.text().to_owned());
                }
                state.editing = None;
            }
            _ => {
                edit_input(input, key);
            }
        }
        return Vec::new();
    }
    if is_ctrl(key, 's') {
        return save_settings(app);
    }
    let index = state.selected;
    let count = state.form.fields.len();
    match key.code {
        KeyCode::Down | KeyCode::Up => step(&mut state.selected, count, key.code == KeyCode::Down),
        KeyCode::Enter | KeyCode::Char(' ') => start_field_edit(state, index, 1),
        KeyCode::Left if matches!(state.form.fields[index].kind, FieldKind::Choice(_)) => {
            start_field_edit(state, index, -1);
        }
        KeyCode::Right if matches!(state.form.fields[index].kind, FieldKind::Choice(_)) => {
            start_field_edit(state, index, 1);
        }
        KeyCode::Char('$') => return open_reference_picker(app),
        KeyCode::Char('z') => state.draft[index] = state.form.fields[index].value.clone(),
        KeyCode::Esc => {
            if state.changed_indices().is_empty() {
                pop(app);
            } else {
                app.modal = Some(Modal {
                    title: "Discard your changes?".to_owned(),
                    body: vec!["Your unsaved settings will be lost.".to_owned()],
                    confirm_label: "Discard".to_owned(),
                    confirm_focused: false,
                    action: ModalAction::DiscardSettings,
                });
            }
        }
        _ => {}
    }
    Vec::new()
}

/// The keys that open the picker from the Settings form. F4 opens a drop-down in many programs
/// and reaches every terminal. Ctrl-. only arrives where the terminal reports it (the kitty
/// keyboard protocol, see `CrosstermMode`); elsewhere it types nothing or a plain dot.
pub fn opens_picker(key: KeyEvent) -> bool {
    key.code == KeyCode::F(4) || is_ctrl(key, '.')
}

/// Opens the picker for the selected field. Whatever is being typed stays, so Esc from the picker
/// goes back to it.
fn open_reference_picker(app: &mut App) -> Vec<Effect> {
    let Screen::Settings(state) = &app.screen else {
        return Vec::new();
    };
    let index = state.selected;
    let field = &state.form.fields[index];
    if !field.kind.takes_reference() {
        fail(app, "This setting cannot take a value from another app.");
        return Vec::new();
    }
    vec![Effect::LoadReferences {
        package: state.form.package.clone(),
        field_index: index,
        field_key: field.key.clone(),
        field_label: field.label.clone(),
    }]
}

fn start_field_edit(state: &mut SettingsState, index: usize, direction: isize) {
    match (&state.form.fields[index].kind, &state.draft[index]) {
        (FieldKind::Toggle, FieldValue::Bool(value)) => state.draft[index] = FieldValue::Bool(!*value),
        (FieldKind::Choice(options), FieldValue::Text(current)) => {
            let position = options.iter().position(|option| option == current).unwrap_or(0);
            let len = options.len() as isize;
            let next = (position as isize + direction).rem_euclid(len) as usize;
            state.draft[index] = FieldValue::Text(options[next].clone());
        }
        (FieldKind::Secret, _) => state.editing = Some(Input::secret()),
        (_, FieldValue::Text(current)) => state.editing = Some(Input::plain(current.clone())),
        (_, FieldValue::Reference { .. }) => state.editing = Some(Input::plain("")),
        _ => {}
    }
}

fn save_settings(app: &mut App) -> Vec<Effect> {
    let Screen::Settings(state) = &mut app.screen else {
        return Vec::new();
    };
    let changed = state.changed_indices();
    if changed.is_empty() {
        notice(app, NoticeKind::Info, "There are no changes to save.");
        return Vec::new();
    }
    if let Some(invalid) = (0..state.draft.len()).find(|&index| state.error_for(index).is_some()) {
        state.selected = invalid;
        fail(app, "Fix the marked field before saving. Nothing was changed.");
        return Vec::new();
    }
    let values = changed
        .into_iter()
        .map(|index| (state.form.fields[index].key.clone(), state.draft[index].clone()))
        .collect();
    vec![Effect::ApplySettings { package: state.form.package.clone(), values }]
}

fn reference_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::Reference(state) = &mut app.screen else {
        return Vec::new();
    };
    let visible_len = state.visible().len();
    match key.code {
        KeyCode::Esc => pop(app),
        KeyCode::Enter => {
            let chosen = state.visible().get(state.selected).map(|option| FieldValue::Reference {
                path: option.path.clone(),
                resolved: option.value.clone(),
            });
            let field_index = state.field_index;
            if let Some(value) = chosen {
                pop(app);
                if let Screen::Settings(settings) = &mut app.screen {
                    settings.draft[field_index] = value;
                    settings.editing = None;
                }
            }
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
        KeyCode::Char('t') => {
            let field_index = state.field_index;
            pop(app);
            if let Screen::Settings(settings) = &mut app.screen {
                settings.selected = field_index;
                settings.editing = Some(Input::plain(""));
            }
        }
        KeyCode::Char('$' | '/') | KeyCode::Backspace => state.list_focused = false,
        _ => {}
    }
    Vec::new()
}

fn apply_failed_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::ApplyFailed(state) = &app.screen else {
        return Vec::new();
    };
    let package = state.package.clone();
    let blamed = state.explanation.blamed_field.clone();
    match key.code {
        KeyCode::Enter => {
            pop(app);
            if let (Screen::Settings(settings), Some(blamed)) = (&mut app.screen, blamed) {
                if let Some(index) = settings.form.fields.iter().position(|field| field.key == blamed) {
                    settings.selected = index;
                }
            }
        }
        KeyCode::Char('l') => return open_logs(app, &package),
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
        KeyCode::Char('r') => return vec![Effect::CheckUpdates],
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
                app.modal = Some(Modal {
                    title: "Discard your changes?".to_owned(),
                    body: vec!["Your auto-update choices will not be saved.".to_owned()],
                    confirm_label: "Discard".to_owned(),
                    confirm_focused: false,
                    action: ModalAction::DiscardAutoUpdate,
                });
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
    if state.finding {
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
                return vec![Effect::LoadLogs(source.clone())];
            }
        }
        KeyCode::F(5) => {
            if let Some(source) = sources.get(state.selected) {
                return vec![Effect::LoadLogs(source.clone())];
            }
        }
        KeyCode::Char('/') => state.finding = true,
        KeyCode::Char('e') => state.errors_only ^= true,
        KeyCode::Char('f') => state.follow ^= true,
        KeyCode::Char('y') => {
            if let Some(source) = sources.get(state.selected) {
                return vec![Effect::CopyReport(format!("{source} log"))];
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
        KeyCode::Char('y') => vec![Effect::CopyReport(format!("{} rejection", data.id.name))],
        KeyCode::Char('f') => {
            let feed = data.id.feed.clone();
            let selected = app.data.feeds.iter().position(|entry| entry.name == feed).unwrap_or(0);
            open_sources(app, selected)
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
                let note = state.data.compatible_note.clone();
                notice(app, NoticeKind::Info, format!("No compatible version: {note}."));
            }
            _ => {
                let node = state.data.alternative_node.clone();
                return vec![Effect::CheckInstall(PackageId::new("official", &node))];
            }
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
    app.data = snapshot;
    if app.data.overview.setup_needed && matches!(app.screen, Screen::Home { .. } | Screen::NoDrive { .. }) {
        push(app, Screen::Setup(SetupState::passphrase()));
        return;
    }
    let sources = app.data.log_sources.len();
    let services = app.data.services.len();
    let feeds = app.data.feeds.len();
    let sites = app.data.web.sites.len();
    let rows = app.update_rows().len();
    let drive_missing = app.data.overview.drive.is_some();
    let clamp = |index: &mut usize, len: usize| *index = (*index).min(len.saturating_sub(1));
    match &mut app.screen {
        Screen::Home { .. } if drive_missing => app.screen = Screen::NoDrive { choice: 0 },
        Screen::NoDrive { .. } if !drive_missing => app.screen = Screen::Home { selected: 0 },
        Screen::Home { selected } => clamp(selected, services),
        Screen::Feeds { selected, .. } => clamp(selected, feeds),
        Screen::Web { selected } => clamp(selected, sites),
        Screen::Updates(state) => clamp(&mut state.selected, rows),
        Screen::Logs(state) => clamp(&mut state.selected, sources),
        _ => {}
    }
}

fn on_result(app: &mut App, event: Event) -> Vec<Effect> {
    match event {
        Event::Refreshed(snapshot) => on_refreshed(app, *snapshot),
        Event::Failed(text) => fail(app, text),
        Event::Info(text) => notice(app, NoticeKind::Info, text),
        Event::UnlockResult(result) => return on_unlock_result(app, result),
        Event::SetupDone(result) => {
            if let Screen::Setup(state) = &mut app.screen {
                match result {
                    Ok(()) => {
                        *state = SetupState::keys();
                        notice(app, NoticeKind::Ok, "Device passphrase set. Keep it somewhere safe.");
                        return vec![Effect::LoadSshKeys];
                    }
                    Err(error) => {
                        *state = SetupState::passphrase();
                        state.error = Some(error.to_string());
                    }
                }
            }
        }
        Event::SshKeysLoaded(result) => {
            if let Screen::Setup(state) = &mut app.screen {
                state.busy = false;
                match result {
                    Ok(list) => {
                        if let SetupStep::Keys { keys, selected, .. } = &mut state.step {
                            *selected = (*selected).min(list.len().saturating_sub(1));
                            *keys = list;
                        }
                    }
                    Err(error) => state.error = Some(error),
                }
            }
        }
        Event::SshKeyChanged(result) => {
            if let Screen::Setup(state) = &mut app.screen {
                state.error = result.err();
            }
        }
        Event::BackupsLoaded(_)
        | Event::BackupCreated(_)
        | Event::BackupInspected(_)
        | Event::BackupRestored(_)
        | Event::UsbLoaded(_) => return on_backup_result(app, event),
        Event::OnionLoaded(_)
        | Event::OnionChanged(_)
        | Event::RestoreDrives(_)
        | Event::RestoreBackups(_)
        | Event::RestoreInspected(_)
        | Event::RestoreFinished(_)
        | Event::RollbacksLoaded(_)
        | Event::RolledBack(_) => return more::on_result(app, event),
        Event::NetworkLoaded { .. }
        | Event::WifiChanged { .. }
        | Event::WifiScanned(_)
        | Event::BluetoothChanged { .. } => return net::on_result(app, event),
        Event::DetailLoaded(Some(detail)) => push(app, Screen::Detail(detail)),
        Event::DetailLoaded(None) => fail(app, "No details are available for that package."),
        Event::GateLoaded(gate) => match gate {
            InstallGate::Review(data) => push(app, Screen::Review(ReviewState::new(data, ReviewPurpose::Install))),
            InstallGate::Rejected(data) => push(app, Screen::Rejected(data)),
            InstallGate::MissingCapability(data) => {
                push(app, Screen::Capability(CapabilityState { data, choice: 0 }));
            }
        },
        Event::InstallProgress(snapshot) => on_install_progress(app, snapshot),
        Event::FeedProbed(result) => {
            if let Screen::AddFeed(state) = &mut app.screen {
                match result {
                    Ok(probe) => *state = AddFeedState::Confirm { probe, trust_focused: false },
                    Err(text) => {
                        if let AddFeedState::Address { error, .. } = state {
                            *error = Some(text);
                        }
                    }
                }
            }
        }
        Event::FeedTrusted(result) => match result {
            Ok(name) => {
                pop(app);
                notice(app, NoticeKind::Ok, format!("Added {name} and pinned its key."));
            }
            Err(text) => fail(app, text),
        },
        Event::KeyChangeLoaded(Some(change)) => {
            push(app, Screen::FeedKey(FeedKeyState { change, choice: 0, typing: None, error: None }));
        }
        Event::KeyChangeLoaded(None) => fail(app, "That feed has no pending key change."),
        Event::NewKeyTrusted(result) => match result {
            Ok(name) => {
                pop(app);
                notice(app, NoticeKind::Ok, format!("{name} now uses the new key."));
            }
            Err(text) => fail(app, text),
        },
        Event::SettingsLoaded { package, form } => match form {
            Some(form) => {
                // A finished install is replaced so that Esc does not return to its progress list.
                if matches!(app.screen, Screen::Installing(_)) {
                    pop(app);
                }
                push(app, Screen::Settings(SettingsState::new(form)));
            }
            None => {
                if matches!(app.screen, Screen::Installing(_)) {
                    pop(app);
                }
                notice(app, NoticeKind::Info, format!("{package} has no settings to configure."));
            }
        },
        Event::ReferencesLoaded { package, field_index, field_label, list } => push(
            app,
            Screen::Reference(ReferenceState {
                package,
                field_index,
                field_label,
                list,
                query: Input::plain(""),
                list_focused: false,
                selected: 0,
            }),
        ),
        Event::SettingsApplied(result) => on_settings_applied(app, result),
        Event::SettingsRolledBack { package, explanation } => {
            on_settings_rolled_back(app, package, explanation);
        }
        Event::UpdateReviewLoaded { review, batch, pending_reviews, approvals } => match review {
            Some(data) => push(
                app,
                Screen::Review(ReviewState::new(data, ReviewPurpose::Update { batch, pending_reviews, approvals })),
            ),
            None => fail(app, "That update could not be reviewed, so nothing was applied."),
        },
        Event::UpdatesApplied(report) => {
            if matches!(app.screen, Screen::Review(_) | Screen::UpdateResults(_)) {
                pop(app);
            }
            push(app, Screen::UpdateResults(UpdateResultsState { report, choice: 0 }));
        }
        Event::AutoUpdateSaved(result) => match result {
            Ok(()) => {
                if matches!(app.screen, Screen::AutoUpdate(_)) {
                    pop(app);
                }
                notice(app, NoticeKind::Ok, "Auto-update choices saved.");
            }
            Err(text) => fail(app, text),
        },
        Event::LogsLoaded { source, view } => {
            let current = if let Screen::Logs(state) = &app.screen {
                app.data.log_sources.get(state.selected).cloned()
            } else {
                None
            };
            if let (Screen::Logs(state), Some(current)) = (&mut app.screen, current) {
                // A slow reply for a service the user has already left must not replace the view.
                if current == source {
                    state.view = view;
                }
            }
        }
        Event::DriveResult(result) => match result {
            Ok(doc) => app.overlay = Some(Overlay { doc, scroll: 0 }),
            Err(text) => fail(app, text),
        },
        Event::ExposureSet { after, result } => return web::on_exposure_set(app, after, result),
        Event::PublicHostSet(result) => {
            if let Err(text) = result {
                fail(app, text);
            }
        }
        Event::Reachability { site, going_public, result } => {
            web::on_reachability(app, site, going_public, result);
        }
        Event::ProviderAdded(result) => return web::on_provider_added(app, result),
        Event::ProvidersLoaded(result) => web::on_providers_loaded(app, result),
        Event::ProviderRemoved(result) => return web::on_provider_removed(app, result),
        Event::ProviderRefreshed(result) => return web::on_provider_refreshed(app, result),
        Event::AcmeSaved(result) => match result {
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
        Event::LocalNamesChecked(result) => {
            if let Screen::WebDomain(state) = &mut app.screen {
                state.checks = Some(result);
            }
        }
        Event::DocumentLoaded(doc) => app.overlay = Some(Overlay { doc, scroll: 0 }),
        Event::Key(_) | Event::Mouse { .. } | Event::Paste(_) | Event::Tick | Event::Resize(..) => {}
    }
    Vec::new()
}

fn on_unlock_result(app: &mut App, result: Result<Response, ApiError>) -> Vec<Effect> {
    let ticks = app.ticks;
    let Screen::Unlock(state) = &mut app.screen else {
        return Vec::new();
    };
    match result {
        Ok(_) => {
            pop(app);
            notice(app, NoticeKind::Ok, "Unlocked. Services are starting in dependency order.");
            vec![Effect::Refresh]
        }
        Err(error) => {
            state.failures += 1;
            let seconds = (1u64 << state.failures.min(16)).min(MAX_UNLOCK_DELAY_SECONDS);
            state.retry_at_tick = ticks + seconds * TICKS_PER_SECOND;
            state.error = Some(match error {
                ApiError::BadPassphrase => "That passphrase did not unlock this device. Try again.".to_owned(),
                other => format!("Could not unlock: {other}"),
            });
            Vec::new()
        }
    }
}

fn on_install_progress(app: &mut App, snapshot: Option<InstallSnapshot>) {
    let Some(snapshot) = snapshot else {
        return;
    };
    match snapshot.outcome {
        InstallOutcome::Cancelled => {
            if matches!(app.screen, Screen::Installing(_)) {
                pop(app);
            }
            notice(app, NoticeKind::Info, "Install cancelled. What it added was removed.");
        }
        _ => match &mut app.screen {
            Screen::Installing(current) => *current = snapshot,
            Screen::Review(_) => app.screen = Screen::Installing(snapshot),
            _ => {}
        },
    }
}

fn masked_field(value: &FieldValue) -> String {
    match value {
        FieldValue::Text(text) => {
            let count = text.chars().count();
            if count > 18 {
                format!("{}…", text.chars().take(14).collect::<String>())
            } else {
                text.clone()
            }
        }
        FieldValue::Reference { path, .. } => format!("$({path})"),
        FieldValue::Bool(true) => "on".to_owned(),
        FieldValue::Bool(false) => "off".to_owned(),
        FieldValue::SecretUnset => "not set".to_owned(),
        FieldValue::SecretSet => crate::secret::STORED_MASK.to_owned(),
        FieldValue::NewSecret(_) => "new secret".to_owned(),
    }
}

fn on_settings_applied(app: &mut App, result: Result<(), ApiError>) {
    let Screen::Settings(state) = &mut app.screen else {
        return;
    };
    match result {
        Ok(()) => {
            let package = state.form.package.clone();
            for (field, value) in state.form.fields.iter_mut().zip(state.draft.iter_mut()) {
                // The typed secret is dropped here. From now on it is only known to be set.
                if matches!(value, FieldValue::NewSecret(_)) {
                    *value = FieldValue::SecretSet;
                }
                field.value = value.clone();
            }
            notice(app, NoticeKind::Ok, format!("Saved. {package} restarted and passed its health check."));
        }
        Err(error) => fail(app, format!("Not saved: {error}")),
    }
}

fn on_settings_rolled_back(app: &mut App, package: String, explanation: RollbackExplanation) {
    let Screen::Settings(state) = &app.screen else {
        return;
    };
    let rows = state
        .changed_indices()
        .into_iter()
        .map(|index| {
            let field = &state.form.fields[index];
            DraftRow {
                label: field.label.clone(),
                from: masked_field(&field.value),
                to: masked_field(&state.draft[index]),
                blamed: explanation.blamed_field.as_deref() == Some(field.key.as_str()),
            }
        })
        .collect();
    push(app, Screen::ApplyFailed(ApplyFailedState { package, explanation, rows }));
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

fn on_backup_result(app: &mut App, event: Event) -> Vec<Effect> {
    let Screen::Backup(state) = &mut app.screen else {
        return Vec::new();
    };
    match event {
        Event::BackupsLoaded(Ok(list)) => {
            state.selected = state.selected.min(list.len().saturating_sub(1));
            state.list = list;
        }
        Event::UsbLoaded(result) => {
            state.busy = false;
            state.usb = result.ok();
            let len = state.usb_list().len();
            state.usb_selected = state.usb_selected.min(len.saturating_sub(1));
            if len == 0 && !state.fresh {
                state.usb_focused = false;
            }
        }
        Event::BackupCreated(Ok(created)) => {
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
        Event::BackupInspected(Ok(contents)) => {
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
        Event::BackupRestored(Ok(report)) => {
            state.busy = false;
            state.step = BackupStep::Restored(report);
        }
        Event::BackupsLoaded(Err(e)) => {
            // On first boot the device has no backups of its own, which is no error.
            if !state.fresh {
                state.error = Some(e);
            }
        }
        Event::BackupCreated(Err(e)) | Event::BackupInspected(Err(e)) | Event::BackupRestored(Err(e)) => {
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
            click(app, hit.click)
        }
    }
}

/// A click that picks a row, a field or a choice: moves the selection there, as the arrow keys
/// would, so whatever key the click then presses acts on what was clicked.
fn pick(app: &mut App, index: usize) {
    let counts = (app.home_items().len(), installed_apps(app).len(), app.data.feeds.len(), app.update_rows().len(), app.data.log_sources.len());
    match &mut app.screen {
        Screen::Home { selected } => *selected = index.min(counts.0.saturating_sub(1)),
        Screen::NoDrive { choice } => *choice = index.min(3),
        Screen::Apps { selected } => *selected = index.min(counts.1.saturating_sub(1)),
        Screen::AppPage(state) => state.selected = index.min(APP_SECTIONS.len() - 1),
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
        Screen::Settings(state) if state.editing.is_none() => state.selected = index.min(state.form.fields.len().saturating_sub(1)),
        Screen::Reference(state) => {
            state.list_focused = true;
            state.selected = index;
        }
        Screen::Updates(state) => state.selected = index.min(counts.3.saturating_sub(1)),
        Screen::AutoUpdate(state) => state.selected = index,
        Screen::UpdateResults(state) => state.choice = index.min(3),
        Screen::Logs(state) => state.selected = index.min(counts.4.saturating_sub(1)),
        Screen::Rollbacks(state) => state.selected = index,
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

    #[test]
    fn address_validation_matches_the_design_copy() {
        let short = FieldValue::Text("48edf".to_owned());
        assert_eq!(
            validate(&FieldKind::MoneroAddress, &short).as_deref(),
            Some("Address is too short. A Monero address has 95 characters.")
        );
        let good = FieldValue::Text(crate::mock::sample_address());
        assert_eq!(validate(&FieldKind::MoneroAddress, &good), None);
    }
}

