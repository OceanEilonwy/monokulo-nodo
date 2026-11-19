//! Keys for web flow v3: the Web tab (sites that are online), an app's Hosting, the internet
//! checks and confirm, and domain providers.
//!
//! Only the keys, the cursors and the boxes being typed in are here. What each action does, the
//! safety rules (the internet only after the checks and a confirm that starts on Cancel; a
//! provider's keys only ever sent to that provider) and what every result means are the core's
//! `flows::web`.

use crossterm::event::{KeyCode, KeyEvent};
use nodo_api::{AliasMode, Exposure};

use super::{edit_input, follow, more, open_logs, pop, push, step, tab_key};
use crate::app::*;
use nodo_ui_core::flows::web::providers::{KeyField, ProviderStage, Providers};
use nodo_ui_core::flows::web::public::{CgnatWay, Found};
use nodo_ui_core::flows::web::{self, hosting, providers};
use nodo_ui_core::flows::Notice;
use nodo_ui_core::model::*;
use nodo_ui_core::{Effect, Outcome};

/// Opens the Hosting screen of a site, or says why there is none.
pub(super) fn open_hosting(app: &mut App, site: &str) -> Result<(), Notice> {
    let flow = hosting::open(&app.data, site)?;
    let state = ExposureState::of(flow, &app.domain_choices());
    push(app, Screen::WebExposure(state));
    Ok(())
}

pub(super) fn web_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    if let Some(effects) = tab_key(app, key) {
        return effects;
    }
    let sites: Vec<String> = app.active_sites().iter().map(|site| site.package.clone()).collect();
    let Screen::Web { selected } = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Down | KeyCode::Up => step(selected, sites.len(), key.code == KeyCode::Down),
        KeyCode::Enter => {
            if let Some(site) = sites.get(*selected).cloned() {
                // The list is of sites, so there is one; nothing to say when it just went.
                let _gone: Result<(), Notice> = open_hosting(app, &site);
            }
        }
        KeyCode::Char('p') => push(app, Screen::WebProviders(ProvidersState::list(""))),
        KeyCode::Char('d') => {
            let state = DomainState::new(&app.data.web.acme, &app.data.web.domain);
            push(app, Screen::WebDomain(state));
        }
        KeyCode::Char('l') => return open_logs(app, "System"),
        KeyCode::Char('n') => return more::open_onion(app),
        KeyCode::Char('o') => {
            if let Some(question) = web::ask_all_offline(&app.data) {
                app.modal = Some(Modal::ask(question));
            }
        }
        _ => {}
    }
    Vec::new()
}

/// The Hosting screen of one app: who can open it, and for the internet, at which address.
pub(super) fn hosting_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let choices = app.domain_choices();
    let Screen::WebExposure(state) = &mut app.screen else {
        return Vec::new();
    };
    state.sync(&app.data);
    if !app.data.web.sites.iter().any(|site| site.package == state.site) {
        pop(app);
        return Vec::new();
    }
    let site = state.site.clone();
    if state.list_open() {
        match key.code {
            KeyCode::Down | KeyCode::Up => step(&mut state.pick, choices.len() + 1, key.code == KeyCode::Down),
            KeyCode::Esc => (state.picking, state.picking_alias) = (false, false),
            KeyCode::Enter | KeyCode::Char(' ') => {
                let for_alias = state.picking_alias;
                (state.picking, state.picking_alias) = (false, false);
                match choices.get(state.pick) {
                    Some((domain, _)) if for_alias => {
                        state.alias_domain = domain.clone();
                        state.flow.clear_alias_error();
                    }
                    Some((domain, _)) => {
                        state.flow.set_domain(domain, &app.data);
                    }
                    None => return open_add_provider(app, Some(site)),
                }
            }
            _ => {}
        }
        return Vec::new();
    }
    let len = state.order().len();
    let focus = state.focus();
    match key.code {
        KeyCode::Esc => pop(app),
        KeyCode::Tab | KeyCode::Down => state.selected = (state.selected.min(len - 1) + 1).min(len - 1),
        KeyCode::BackTab | KeyCode::Up => state.selected = state.selected.min(len - 1).saturating_sub(1),
        KeyCode::Char(' ') | KeyCode::Enter if matches!(focus, HostingFocus::Home | HostingFocus::Tor | HostingFocus::Internet) && key.code == KeyCode::Char(' ') => {
            toggle(state, focus);
        }
        KeyCode::Char(' ') | KeyCode::Enter if focus == HostingFocus::Details => state.details = !state.details,
        KeyCode::Char(' ') | KeyCode::Enter if focus == HostingFocus::Domain => {
            state.picking = true;
            state.pick = choices.iter().position(|(d, _)| *d == state.domain).unwrap_or(0);
        }
        KeyCode::Char(' ') | KeyCode::Enter if focus == HostingFocus::AliasDomain => {
            state.picking_alias = true;
            state.pick = choices.iter().position(|(d, _)| *d == state.alias_domain).unwrap_or(0);
        }
        KeyCode::Char(' ') | KeyCode::Enter if focus == HostingFocus::AliasMode => {
            state.alias_mode = match state.alias_mode {
                AliasMode::Redirect => AliasMode::Serve,
                AliasMode::Serve => AliasMode::Redirect,
            };
        }
        // Enter in the name box adds it too: that is what someone who just typed a name means.
        KeyCode::Char(' ') | KeyCode::Enter if focus == HostingFocus::AliasAdd => add_alias(state, &app.data),
        KeyCode::Enter if focus == HostingFocus::AliasAddress => add_alias(state, &app.data),
        KeyCode::Char('x') | KeyCode::Delete if matches!(focus, HostingFocus::Alias(_)) => {
            if let HostingFocus::Alias(index) = focus {
                if let Some(name) = state.aliases.get(index).map(|alias| alias.host.clone()) {
                    state.flow.remove_alias(&name);
                }
                state.selected = state.selected.min(state.order().len() - 1);
            }
        }
        KeyCode::Enter if focus == HostingFocus::AddProvider => return open_add_provider(app, Some(site)),
        KeyCode::Enter => {
            let step = state.flow.save(&app.data);
            return follow(app, step);
        }
        _ if focus == HostingFocus::Address => {
            edit_input(&mut state.sub, key);
            state.flow.set_sub(state.sub.text());
        }
        _ if focus == HostingFocus::AliasAddress => {
            edit_input(&mut state.alias_sub, key);
            state.flow.clear_alias_error();
        }
        _ => {}
    }
    Vec::new()
}

/// Puts the new name on the list, when the core takes it.
fn add_alias(state: &mut ExposureState, data: &Snapshot) {
    let name = state.alias_host();
    if state.flow.add_alias(&name, state.alias_mode, data) {
        state.alias_sub.clear();
        // The new row comes before the controls, so the focus moves on one to stay where it was.
        state.selected = (state.selected + 1).min(state.order().len() - 1);
    }
}

/// Ticks or unticks one choice. Each choice is its own flag: the exposures are independent.
fn toggle(state: &mut ExposureState, focus: HostingFocus) {
    let Exposure { local, onion, public } = state.draft;
    match focus {
        HostingFocus::Home => state.flow.set_exposure(!local, onion, public),
        HostingFocus::Tor => state.flow.set_exposure(local, !onion, public),
        HostingFocus::Internet => state.flow.set_exposure(local, onion, !public),
        _ => {}
    }
    let len = state.order().len();
    state.selected = state.selected.min(len - 1);
}

fn open_add_provider(app: &mut App, return_to: Option<String>) -> Vec<Effect> {
    push(app, Screen::WebProviders(ProvidersState::of(Providers::adding(return_to), "")));
    providers::load()
}

/// The provider catalog read again as the add flow opened.
pub(super) fn on_providers_loaded(app: &mut App, result: Result<Vec<Provider>, String>) {
    let flow = match &app.screen {
        Screen::WebProviders(state) => Some(&state.flow),
        _ => None,
    };
    providers::catalog_loaded(&mut app.data, flow, &result);
    if let Screen::WebProviders(state) = &mut app.screen {
        if state.stage == ProviderStage::Pick {
            state.selected = state.selected.min(app.data.providers.len().saturating_sub(1));
        }
    }
}

/// The boxes of the key step, as what can have the focus there.
fn provider_fields(provider: Option<&Provider>) -> Vec<ProviderFocus> {
    providers::fields(provider).into_iter().map(ProviderFocus::from).collect()
}

pub(super) fn providers_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let catalog = app.data.providers.clone();
    let connected = app.data.connected.clone();
    let Screen::WebProviders(state) = &mut app.screen else {
        return Vec::new();
    };
    let fields = provider_fields(catalog.get(state.selected));
    match state.stage.clone() {
        ProviderStage::List => match key.code {
            KeyCode::Esc => {
                let step = state.flow.back();
                return follow(app, step);
            }
            KeyCode::Down | KeyCode::Up => step(&mut state.selected, connected.len(), key.code == KeyCode::Down),
            KeyCode::Char('a') | KeyCode::Enter => {
                state.empty_boxes();
                state.selected = 0;
                return state.flow.add();
            }
            KeyCode::Char('r') => {
                if let Some(provider) = connected.get(state.selected) {
                    return providers::refresh(&provider.id);
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(provider) = connected.get(state.selected) {
                    app.modal = Some(Modal::ask(providers::ask_remove(provider)));
                }
            }
            KeyCode::Char('c') => {
                let domain_state = DomainState::new(&app.data.web.acme, &app.data.web.domain);
                push(app, Screen::WebDomain(domain_state));
            }
            _ => {}
        },
        ProviderStage::Pick => match key.code {
            KeyCode::Down | KeyCode::Up => {
                step(&mut state.selected, catalog.len(), key.code == KeyCode::Down);
                state.flow.clear_test();
            }
            KeyCode::Enter => {
                let Some(provider) = catalog.get(state.selected) else { return Vec::new() };
                if state.flow.choose(provider) {
                    state.empty_boxes();
                    state.focus = provider_fields(Some(provider))[0];
                }
            }
            KeyCode::Esc => {
                let step = state.flow.back();
                if state.stage == ProviderStage::List {
                    state.selected = 0;
                }
                return follow(app, step);
            }
            _ => {}
        },
        ProviderStage::Key => {
            let at = fields.iter().position(|f| *f == state.focus).unwrap_or(0);
            match key.code {
                KeyCode::Esc => {
                    let step = state.flow.back();
                    state.empty_boxes();
                    return follow(app, step);
                }
                KeyCode::Tab | KeyCode::Down => state.focus = fields[(at + 1) % fields.len()],
                KeyCode::BackTab | KeyCode::Up => state.focus = fields[(at + fields.len() - 1) % fields.len()],
                KeyCode::Enter => {
                    let Some(provider) = catalog.get(state.selected) else { return Vec::new() };
                    let domain = state.domain.text().to_owned();
                    // On a locked device the core sends the owner to unlock; the unlock screen
                    // sends Enter back here to carry on, with the keys still in their boxes.
                    match state.flow.submit(provider, &domain, &mut state.api_key, &mut state.secret_key, &app.data) {
                        Ok(step) => return follow(app, step),
                        Err(KeyField::Domain) => state.focus = ProviderFocus::Domain,
                        Err(KeyField::ApiKey | KeyField::SecretKey) => {}
                    }
                }
                KeyCode::Char('?') if state.api_key.is_empty() => {
                    if let Some(provider) = catalog.get(state.selected) {
                        return providers::key_help(provider);
                    }
                }
                _ => {
                    let input = match state.focus {
                        ProviderFocus::Domain => &mut state.domain,
                        ProviderFocus::ApiKey => &mut state.api_key,
                        ProviderFocus::SecretKey => &mut state.secret_key,
                        ProviderFocus::List => return Vec::new(),
                    };
                    edit_input(input, key);
                }
            }
        }
        ProviderStage::Done { .. } => {
            if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                let step = state.flow.finish();
                if state.stage == ProviderStage::List {
                    *state = ProvidersState::list("");
                }
                return follow(app, step);
            }
        }
    }
    Vec::new()
}

/// Back on the Hosting screen that sent the owner to add a provider, which now has one.
pub(super) fn hosting_with_provider(app: &mut App, domains: &[String]) {
    pop(app);
    if let Screen::WebExposure(hosting) = &mut app.screen {
        hosting.flow.provider_connected(domains);
        hosting.selected = 0;
    }
}

pub(super) fn on_provider_added(app: &mut App, outcome: &Outcome) -> Vec<Effect> {
    let Screen::WebProviders(state) = &mut app.screen else {
        return Vec::new();
    };
    // The keys were taken out of the boxes when they were sent; nothing typed is kept either way.
    state.api_key.clear();
    state.secret_key.clear();
    let step = state.flow.apply(outcome);
    follow(app, step)
}

/// A provider was removed or asked again. The list starts over after a removal, since the row
/// the cursor was on is gone.
pub(super) fn on_provider_changed(app: &mut App, outcome: &Outcome) -> Vec<Effect> {
    if let (Outcome::ProviderRemoved(Ok(())), Screen::WebProviders(state)) = (outcome, &mut app.screen) {
        state.selected = 0;
    }
    follow(app, providers::apply(outcome))
}

pub(super) fn web_check_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::WebCheck(state) = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Char('r') => state.check_again(),
        KeyCode::Char('d') => {
            state.details = !state.details;
            Vec::new()
        }
        // Nothing failed, but something could not be verified. The owner decides, having seen it.
        KeyCode::Char('c') => {
            let step = state.continue_anyway();
            follow(app, step)
        }
        KeyCode::Char('?') => state.router_guides(),
        KeyCode::Char('o') | KeyCode::Enter => state.use_tor(&app.data),
        KeyCode::Esc => {
            pop(app);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

pub(super) fn web_confirm_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::WebConfirm(state) = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Esc => pop(app),
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
            state.focus = if state.focus == ConfirmFocus::Cancel { ConfirmFocus::GoPublic } else { ConfirmFocus::Cancel };
        }
        KeyCode::Char('d') => state.details = !state.details,
        KeyCode::Enter if state.focus == ConfirmFocus::Cancel => pop(app),
        // The button is under the address this screen shows, and that address is what it quotes
        // back; the core never refuses its own.
        KeyCode::Enter => return state.confirm(&state.plan.host, &app.data).unwrap_or_default(),
        _ => {}
    }
    Vec::new()
}

pub(super) fn cgnat_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::WebCgnat(state) = &mut app.screen else {
        return Vec::new();
    };
    match key.code {
        KeyCode::Down | KeyCode::Up if state.details => step(&mut state.choice, CgnatWay::ALL.len(), key.code == KeyCode::Down),
        KeyCode::Char('d') => state.details = !state.details,
        KeyCode::Char('r') => return state.check_again(),
        KeyCode::Char('o') => return state.use_tor(&app.data),
        KeyCode::Esc => pop(app),
        KeyCode::Enter => {
            let way = CgnatWay::ALL.get(state.choice).copied().unwrap_or(CgnatWay::Relay);
            return state.choose(way, &app.data);
        }
        _ => {}
    }
    Vec::new()
}

/// The page for what a check found. It takes the place of an earlier check's page, so that
/// "check again" replaces it instead of piling up.
pub(super) fn show_found(app: &mut App, found: Found) {
    if matches!(app.screen, Screen::WebCheck(_) | Screen::WebCgnat(_)) {
        pop(app);
    }
    let screen = match found {
        Found::Cgnat(flow) => Screen::WebCgnat(CgnatState { flow, choice: 0, details: false }),
        Found::Problems(flow) => Screen::WebCheck(WebCheckState { flow, details: false }),
        Found::Clear(flow) => Screen::WebConfirm(WebConfirmState::new(flow)),
    };
    push(app, screen);
}

/// The site is public: the Web tab, and on it the screen that follows the secure connection
/// until it is in place, when the confirmation is what led here.
pub(super) fn went_public(app: &mut App) {
    let target = match &app.screen {
        Screen::WebConfirm(state) => Some(state.certificate()),
        _ => None,
    };
    app.stack.clear();
    app.screen = Screen::Web { selected: 0 };
    if let Some(target) = target {
        push(app, Screen::WebCertificate(target));
    }
}

/// A click that picks a row or field on a web screen: moves the selection there.
pub(super) fn pick(app: &mut App, index: usize) {
    let fields = match &app.screen {
        Screen::WebProviders(state) => provider_fields(app.data.providers.get(state.selected)),
        _ => Vec::new(),
    };
    match &mut app.screen {
        Screen::Web { selected } => *selected = index,
        Screen::WebExposure(state) => {
            if state.list_open() {
                state.pick = index;
            } else {
                state.selected = index.min(state.order().len() - 1);
            }
        }
        Screen::WebProviders(state) => match state.stage {
            ProviderStage::List => state.selected = index,
            ProviderStage::Pick => {
                state.selected = index;
                state.flow.clear_test();
            }
            ProviderStage::Key => {
                if let Some(field) = fields.get(index) {
                    state.focus = *field;
                }
            }
            _ => {}
        },
        Screen::WebConfirm(state) => state.focus = if index == 0 { ConfirmFocus::Cancel } else { ConfirmFocus::GoPublic },
        Screen::WebCgnat(state) => state.choice = index,
        _ => {}
    }
}

/// The Hosting screen for an app's page in Packages, if the app has a site.
pub(super) fn hosting_from_detail(app: &mut App, package: &str) -> Vec<Effect> {
    if let Err(remark) = open_hosting(app, package) {
        app.notice = Some(remark);
    }
    Vec::new()
}
