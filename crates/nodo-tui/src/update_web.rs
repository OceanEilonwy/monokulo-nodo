//! Keys and results for web flow v3: the Web tab (sites that are online), an app's Hosting, the
//! internet checks and confirm, and domain providers.
//!
//! Safety rules kept from the earlier flow: exposures are independent flags; turning anything off
//! never needs a confirmation; the internet is only turned on after the checks pass and the owner
//! presses "Put … online" on a screen showing the address, with the focus starting on Cancel; and
//! provider keys are only ever sent to the provider they were typed for, and never shown again.

use crossterm::event::{KeyCode, KeyEvent};
use nodo_api::{AliasMode, Exposure};

use super::{edit_input, follow, more, notice, open_logs, pop, push, step, tab_key};
use crate::app::*;
use nodo_ui_core::flows::web::public::{CgnatWay, Found};
use nodo_ui_core::flows::web::{self, hosting};
use nodo_ui_core::flows::{Confirm, Notice, NoticeKind, Question};
use nodo_ui_core::model::*;
use nodo_ui_core::Effect;

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
    let mut state = ProvidersState::new("");
    state.return_to = return_to;
    push(app, Screen::WebProviders(state));
    vec![Effect::LoadProviders]
}

/// The provider catalog read again as the add flow opened. The list is only replaced while
/// choosing the company, since the later steps hold an index into it.
pub(super) fn on_providers_loaded(app: &mut App, result: Result<Vec<Provider>, String>) {
    let picking = matches!(&app.screen, Screen::WebProviders(state) if state.stage == ProviderStage::Pick);
    match result {
        Ok(list) if picking || app.data.providers.is_empty() => {
            app.data.providers = list;
            app.data.providers_error = None;
        }
        Ok(_) => {}
        Err(e) => app.data.providers_error = Some(e),
    }
    if let Screen::WebProviders(state) = &mut app.screen {
        if state.stage == ProviderStage::Pick {
            state.selected = state.selected.min(app.data.providers.len().saturating_sub(1));
        }
    }
}

/// The boxes of the key step: the domain only for a provider that cannot list its domains.
fn provider_fields(app: &App, selected: usize) -> Vec<ProviderFocus> {
    let provider = app.data.providers.get(selected);
    let mut order = Vec::new();
    if provider.is_some_and(|p| !p.lists_domains) {
        order.push(ProviderFocus::Domain);
    }
    order.push(ProviderFocus::ApiKey);
    if provider.is_some_and(|p| p.fields.len() > 1) {
        order.push(ProviderFocus::SecretKey);
    }
    order
}

pub(super) fn providers_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let providers = app.data.providers.clone();
    let locked = app.is_locked();
    let connected = app.data.connected.clone();
    let fields = match &app.screen {
        Screen::WebProviders(state) => provider_fields(app, state.selected),
        _ => return Vec::new(),
    };
    let Screen::WebProviders(state) = &mut app.screen else {
        return Vec::new();
    };
    match state.stage.clone() {
        ProviderStage::List => match key.code {
            KeyCode::Esc => pop(app),
            KeyCode::Down | KeyCode::Up => step(&mut state.selected, connected.len(), key.code == KeyCode::Down),
            KeyCode::Char('a') | KeyCode::Enter => {
                *state = ProvidersState { return_to: state.return_to.take(), ..ProvidersState::new("") };
                return vec![Effect::LoadProviders];
            }
            KeyCode::Char('r') => {
                if let Some(provider) = connected.get(state.selected) {
                    return vec![Effect::RefreshProvider(provider.id.clone())];
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some(provider) = connected.get(state.selected) {
                    let used: Vec<String> = provider.domains.iter().flat_map(|d| d.used_by.clone()).collect();
                    let mut body = vec![format!("The Nodo forgets the {} key. Its domains can't be picked for apps any more.", provider.name)];
                    if !used.is_empty() {
                        body.push(format!("{} still use it, so take them off the internet first.", used.join(", ")));
                    }
                    app.modal = Some(Modal::ask(Question {
                        title: format!("Remove {}?", provider.name),
                        body,
                        confirm_label: "Remove".to_owned(),
                        cancel_is_default: true,
                        confirm: Confirm::RemoveProvider(provider.id.clone()),
                    }));
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
                step(&mut state.selected, providers.len(), key.code == KeyCode::Down);
                state.test = None;
            }
            KeyCode::Enter => {
                let Some(provider) = providers.get(state.selected) else { return Vec::new() };
                if !provider.is_supported() {
                    state.test = Some(Err(format!(
                        "{} can't be connected: it has no way for the Nodo to change your records. Tor works without a web address.",
                        provider.name
                    )));
                    return Vec::new();
                }
                // Keys typed for one provider must never be sent to another: they start empty.
                let selected = state.selected;
                let return_to = state.return_to.take();
                let focus = if provider.lists_domains { ProviderFocus::ApiKey } else { ProviderFocus::Domain };
                *state = ProvidersState { stage: ProviderStage::Key, selected, focus, return_to, ..ProvidersState::new("") };
            }
            KeyCode::Esc => {
                if state.return_to.is_some() {
                    pop(app);
                } else {
                    state.stage = ProviderStage::List;
                    state.selected = 0;
                    state.test = None;
                }
            }
            _ => {}
        },
        ProviderStage::Key => {
            let at = fields.iter().position(|f| *f == state.focus).unwrap_or(0);
            match key.code {
                KeyCode::Esc => {
                    let selected = state.selected;
                    let return_to = state.return_to.take();
                    *state = ProvidersState { selected, return_to, ..ProvidersState::new("") };
                }
                KeyCode::Tab | KeyCode::Down => state.focus = fields[(at + 1) % fields.len()],
                KeyCode::BackTab | KeyCode::Up => state.focus = fields[(at + fields.len() - 1) % fields.len()],
                KeyCode::Enter => {
                    let Some(provider) = providers.get(state.selected) else { return Vec::new() };
                    let domain = state.domain.text().trim().to_owned();
                    if !provider.lists_domains && domain.is_empty() {
                        state.test = Some(Err(format!(
                            "{} can't list your domains, so type the web address you bought, such as example.com.",
                            provider.name
                        )));
                        state.focus = ProviderFocus::Domain;
                        return Vec::new();
                    }
                    if state.api_key.is_empty() || (provider.fields.len() > 1 && state.secret_key.is_empty()) {
                        state.test = Some(Err(format!("Paste the key from {}.", provider.name)));
                        return Vec::new();
                    }
                    state.test = None;
                    if locked {
                        // The keys are stored sealed, so the helper would refuse them. Ask for the
                        // passphrase here; the unlock screen sends Enter back to carry on.
                        let reason = format!("Connecting {} stores its key locked with the device passphrase.", provider.name);
                        push(app, Screen::Unlock(UnlockState::new(Some(reason))));
                        return Vec::new();
                    }
                    return vec![Effect::AddProvider {
                        provider: provider.name.clone(),
                        domain: (!provider.lists_domains).then_some(domain),
                        api_key: state.api_key.take_secret(),
                        secret_key: state.secret_key.take_secret(),
                    }];
                }
                KeyCode::Char('?') if state.api_key.is_empty() => {
                    if let Some(provider) = providers.get(state.selected) {
                        return vec![Effect::ShowDocument(DocKind::ProviderKeys(provider.name.clone()))];
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
        ProviderStage::Done { domains, .. } => {
            if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                if state.return_to.is_some() {
                    pop(app);
                    if let Screen::WebExposure(hosting) = &mut app.screen {
                        hosting.flow.provider_connected(&domains);
                        hosting.selected = 0;
                    }
                } else {
                    *state = ProvidersState::list("");
                }
                return vec![Effect::Refresh];
            }
        }
    }
    Vec::new()
}

pub(super) fn on_provider_added(app: &mut App, result: Result<ConnectedProvider, String>) -> Vec<Effect> {
    let Screen::WebProviders(state) = &mut app.screen else {
        return Vec::new();
    };
    // The keys were taken out of the boxes when they were sent; nothing typed is kept either way.
    state.api_key.clear();
    state.secret_key.clear();
    match result {
        Ok(provider) => {
            let domains = provider.domains.iter().filter(|d| d.held_by.is_none()).map(|d| d.name.clone()).collect();
            let mut held_by: Vec<String> = provider.domains.iter().filter_map(|d| d.held_by.clone()).collect();
            held_by.sort();
            held_by.dedup();
            state.stage = ProviderStage::Done { provider: provider.name, domains, held_by };
            vec![Effect::Refresh]
        }
        Err(text) => {
            state.test = Some(Err(text));
            Vec::new()
        }
    }
}

pub(super) fn on_provider_removed(app: &mut App, result: Result<(), String>) -> Vec<Effect> {
    match result {
        Ok(()) => {
            if let Screen::WebProviders(state) = &mut app.screen {
                state.selected = 0;
            }
            notice(app, NoticeKind::Ok, "The provider was removed.");
            vec![Effect::Refresh]
        }
        Err(text) => {
            notice(app, NoticeKind::Error, text);
            Vec::new()
        }
    }
}

pub(super) fn on_provider_refreshed(app: &mut App, result: Result<ConnectedProvider, String>) -> Vec<Effect> {
    match result {
        Ok(provider) => {
            match &provider.problem {
                Some(problem) => notice(app, NoticeKind::Error, problem.clone()),
                None => notice(app, NoticeKind::Ok, format!("{} lists {} domains.", provider.name, provider.domains.len())),
            }
            vec![Effect::Refresh]
        }
        Err(text) => {
            notice(app, NoticeKind::Error, text);
            Vec::new()
        }
    }
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
        Screen::WebProviders(state) => provider_fields(app, state.selected),
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
                state.test = None;
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
