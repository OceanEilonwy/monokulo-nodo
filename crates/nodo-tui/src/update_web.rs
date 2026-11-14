//! Keys and results for web flow v3: the Web tab (sites that are online), an app's Hosting, the
//! internet checks and confirm, and domain providers.
//!
//! Safety rules kept from the earlier flow: exposures are independent flags; turning anything off
//! never needs a confirmation; the internet is only turned on after the checks pass and the owner
//! presses "Put … online" on a screen showing the address, with the focus starting on Cancel; and
//! provider keys are only ever sent to the provider they were typed for, and never shown again.

use crossterm::event::{KeyCode, KeyEvent};
use nodo_api::{ApiError, Exposure};

use super::{edit_input, fail, more, notice, open_logs, pop, push, step, tab_key};
use crate::app::*;
use crate::model::*;

/// Opens the Hosting screen of a site. Returns false when there is no such site.
pub(super) fn open_hosting(app: &mut App, site: &str) -> bool {
    let choices = app.domain_choices();
    let Some(entry) = app.data.web.sites.iter().find(|entry| entry.package == site) else {
        return false;
    };
    let state = ExposureState::new(&entry.package, entry.exposure, entry.public_host.as_deref(), &choices);
    push(app, Screen::WebExposure(state));
    true
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
                open_hosting(app, &site);
            }
        }
        KeyCode::Char('p') => push(app, Screen::WebProviders(ProvidersState::list(""))),
        KeyCode::Char('d') => {
            let state = DomainState::new(&app.data.web.acme, &app.data.web.domain);
            push(app, Screen::WebDomain(state));
        }
        KeyCode::Char('l') => return open_logs(app, "System"),
        KeyCode::Char('n') => return more::open_onion(app),
        KeyCode::Char('o') if !sites.is_empty() => {
            app.modal = Some(Modal {
                title: "Take all sites offline?".to_owned(),
                body: vec![
                    "Home network, Tor and internet addresses all stop, for every site.".to_owned(),
                    "Records of your home's address can stay in public DNS history after this.".to_owned(),
                ],
                confirm_label: "Take offline".to_owned(),
                confirm_focused: false,
                action: ModalAction::AllSitesOffline,
            });
        }
        _ => {}
    }
    Vec::new()
}

/// The Hosting screen of one app: who can open it, and for the internet, at which address.
pub(super) fn hosting_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let choices = app.domain_choices();
    let sites = app.data.web.sites.clone();
    let Screen::WebExposure(state) = &mut app.screen else {
        return Vec::new();
    };
    state.has_provider = !choices.is_empty();
    if state.domain.is_empty() || !choices.iter().any(|(d, _)| *d == state.domain) {
        state.domain = choices.first().map(|(d, _)| d.clone()).unwrap_or_default();
    }
    let Some(site) = sites.iter().find(|site| site.package == state.site).cloned() else {
        pop(app);
        return Vec::new();
    };
    if state.picking {
        match key.code {
            KeyCode::Down | KeyCode::Up => step(&mut state.pick, choices.len() + 1, key.code == KeyCode::Down),
            KeyCode::Esc => state.picking = false,
            KeyCode::Enter | KeyCode::Char(' ') => {
                state.picking = false;
                match choices.get(state.pick) {
                    Some((domain, _)) => state.domain = domain.clone(),
                    None => return open_add_provider(app, Some(site.package)),
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
        KeyCode::Enter if focus == HostingFocus::AddProvider => return open_add_provider(app, Some(site.package)),
        KeyCode::Enter => return save(app, &site),
        _ if focus == HostingFocus::Address => {
            edit_input(&mut state.sub, key);
        }
        _ => {}
    }
    Vec::new()
}

/// Ticks or unticks one choice. Each choice is its own flag: the exposures are independent.
fn toggle(state: &mut ExposureState, focus: HostingFocus) {
    match focus {
        HostingFocus::Home => state.draft.local ^= true,
        HostingFocus::Tor => state.draft.onion ^= true,
        HostingFocus::Internet => state.draft.public ^= true,
        _ => {}
    }
    let len = state.order().len();
    state.selected = state.selected.min(len - 1);
}

fn save(app: &mut App, site: &Site) -> Vec<Effect> {
    let Screen::WebExposure(state) = &app.screen else {
        return Vec::new();
    };
    let draft = state.draft;
    let host = state.host();
    let name = state.site.clone();
    if draft.public && !state.has_provider {
        notice(app, NoticeKind::Info, "Add a domain provider first, so the internet can use your web address.");
        return Vec::new();
    }
    let host_changes = draft.public && site.public_host.as_deref() != Some(host.as_str());
    if draft == site.exposure && !host_changes {
        notice(app, NoticeKind::Info, "There are no changes to save.");
        return Vec::new();
    }
    let mut effects = Vec::new();
    if host_changes {
        effects.push(Effect::SetPublicHost { site: name.clone(), host });
    }
    if draft.public && !site.exposure.public {
        // The internet only comes on after the checks and a confirm. Home network and Tor apply
        // now, since they stay as chosen while the internet is checked.
        let others = Exposure { public: false, ..draft };
        if others != site.exposure {
            effects.push(Effect::SetExposure { site: name.clone(), exposure: others, after: ExposureAfter::Stay, confirmation: None });
        }
        effects.push(Effect::CheckReachability { site: name, going_public: true });
        return effects;
    }
    if draft != site.exposure {
        effects.push(Effect::SetExposure { site: name, exposure: draft, after: ExposureAfter::Back, confirmation: None });
    }
    effects
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
                    app.modal = Some(Modal {
                        title: format!("Remove {}?", provider.name),
                        body,
                        confirm_label: "Remove".to_owned(),
                        confirm_focused: false,
                        action: ModalAction::RemoveProvider(provider.id.clone()),
                    });
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
                        hosting.has_provider = true;
                        hosting.draft.public = true;
                        hosting.selected = 0;
                        if let Some(first) = domains.first() {
                            hosting.domain = first.clone();
                        }
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
            state.stage = ProviderStage::Done { provider: provider.name, domains };
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

/// Tor on, every other choice as it is. Tor is the fallback that never reveals the home address.
fn with_onion(app: &App, site: &str) -> Option<Effect> {
    let entry = app.data.web.sites.iter().find(|entry| entry.package == site)?;
    Some(Effect::SetExposure {
        site: site.to_owned(),
        exposure: Exposure { onion: true, ..entry.exposure },
        after: ExposureAfter::Stay,
        confirmation: None,
    })
}

pub(super) fn web_check_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::WebCheck(state) = &mut app.screen else {
        return Vec::new();
    };
    let site = state.site.clone();
    let going_public = state.going_public;
    let continue_with =
        state.plan.clone().filter(|_| going_public && !state.checks.iter().any(|c| c.state == CheckState::Failed));
    match key.code {
        KeyCode::Char('r') => vec![Effect::CheckReachability { site, going_public }],
        KeyCode::Char('d') => {
            state.details = !state.details;
            Vec::new()
        }
        // Nothing failed, but something could not be verified. The owner decides, having seen it.
        KeyCode::Char('c') => {
            let Some(plan) = continue_with else { return Vec::new() };
            push(app, confirm_screen(site, plan));
            Vec::new()
        }
        KeyCode::Char('?') => vec![Effect::ShowDocument(DocKind::RouterGuides)],
        KeyCode::Char('o') | KeyCode::Enter => with_onion(app, &site).into_iter().collect(),
        KeyCode::Esc => {
            pop(app);
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn confirm_screen(site: String, plan: PublicPlan) -> Screen {
    Screen::WebConfirm(WebConfirmState {
        site,
        plan,
        access: PublicAccess { require_password: false },
        focus: ConfirmFocus::Cancel,
        details: false,
    })
}

pub(super) fn web_confirm_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let sites = app.data.web.sites.clone();
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
        KeyCode::Enter => {
            let Some(site) = sites.iter().find(|site| site.package == state.site) else {
                return Vec::new();
            };
            // Protections go first, so the site is never public without them. The helper wants
            // the address quoted back: it is the one this screen shows.
            return vec![
                Effect::SetPublicAccess { site: state.site.clone(), access: state.access.clone() },
                Effect::SetExposure {
                    site: state.site.clone(),
                    exposure: Exposure { public: true, ..site.exposure },
                    after: ExposureAfter::WentPublic,
                    confirmation: Some(state.plan.host.clone()),
                },
            ];
        }
        _ => {}
    }
    Vec::new()
}

pub(super) fn cgnat_key(app: &mut App, key: KeyEvent) -> Vec<Effect> {
    let Screen::WebCgnat(state) = &mut app.screen else {
        return Vec::new();
    };
    let site = state.site.clone();
    match key.code {
        KeyCode::Down | KeyCode::Up if state.details => step(&mut state.choice, 4, key.code == KeyCode::Down),
        KeyCode::Char('d') => state.details = !state.details,
        KeyCode::Char('r') => {
            let going_public = state.going_public;
            return vec![Effect::CheckReachability { site, going_public }];
        }
        KeyCode::Char('o') => return with_onion(app, &site).into_iter().collect(),
        KeyCode::Esc => pop(app),
        KeyCode::Enter => match state.choice {
            0 => return with_onion(app, &site).into_iter().collect(),
            1 => return vec![Effect::ShowDocument(DocKind::PublicIpFromIsp)],
            2 => return vec![Effect::ShowDocument(DocKind::Ipv6)],
            _ => return vec![Effect::ShowDocument(DocKind::RelayTunnel)],
        },
        _ => {}
    }
    Vec::new()
}

/// Takes the check screen off the stack so that "check again" replaces it instead of piling up.
fn drop_check_screen(app: &mut App) {
    if matches!(app.screen, Screen::WebCheck(_) | Screen::WebCgnat(_)) {
        pop(app);
    }
}

pub(super) fn on_exposure_set(app: &mut App, after: ExposureAfter, result: Result<(), ApiError>) -> Vec<Effect> {
    if let Err(error) = result {
        fail(app, format!("Not changed: {error}"));
        return Vec::new();
    }
    match after {
        ExposureAfter::Back => {
            if matches!(app.screen, Screen::WebExposure(_)) {
                pop(app);
            }
            notice(app, NoticeKind::Ok, "Saved.");
        }
        ExposureAfter::Stay => notice(app, NoticeKind::Ok, "Saved."),
        ExposureAfter::WentPublic => {
            let target = match &app.screen {
                Screen::WebConfirm(state) => Some(CertificateState { site: state.site.clone(), host: state.plan.host.clone() }),
                _ => None,
            };
            app.stack.clear();
            app.screen = Screen::Web { selected: 0 };
            // The online screen follows the secure connection until it is in place.
            if let Some(target) = target {
                push(app, Screen::WebCertificate(target));
            }
        }
    }
    Vec::new()
}

pub(super) fn on_reachability(app: &mut App, site: String, going_public: bool, result: Reachability) {
    drop_check_screen(app);
    match result {
        Reachability::Cgnat { wan_address, wan_range, internet_address } => push(
            app,
            Screen::WebCgnat(CgnatState { site, wan_address, wan_range, internet_address, choice: 0, going_public, details: false }),
        ),
        // Unknown results, such as untested ports, are shown before confirming, never skipped.
        Reachability::Checked { plan: Some(plan), checks, .. }
            if going_public && checks.iter().all(|c| c.state == CheckState::Passed) =>
        {
            push(app, confirm_screen(site, plan));
        }
        Reachability::Checked { checks, router, plan, manual } => {
            push(app, Screen::WebCheck(WebCheckState { site, checks, router, plan, going_public, manual, details: false }));
        }
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
            if state.picking {
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
    if !open_hosting(app, package) {
        notice(app, NoticeKind::Info, format!("{package} has no web page to put online."));
    }
    Vec::new()
}
