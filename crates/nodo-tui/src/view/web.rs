//! Web flow v3: the Web tab lists the sites that are online; a click opens that app's Hosting,
//! where home network, Tor and internet are ticked and, for the internet, a connected provider's
//! domain is used. Domain providers are added once, under Web › Domain providers.
//!
//! Plain words on the main screens; addresses, records and ports only behind "Show details".
//! Every row, field, tick box and button is a click target (`crate::hit`).

use crossterm::event::KeyCode;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::layout::{Item, Page};
use super::pad;
use super::v3::{heading, inset, line, margin, quiet, tick, trail, Card, Screen3};
use crate::app::*;
use crate::hit::{Click, Target};
use crate::model::*;
use crate::theme::Theme;

/// The size of the frame being drawn. Pages fit cards to it, so it is the real area, not the last
/// size the app was told about.
fn size(_app: &App) -> (u16, u16) {
    super::frame_size()
}

/// A row marker: `▸` where the keyboard focus is, so focus reads without colour.
fn marker(t: &Theme, focused: bool) -> Span<'static> {
    if focused {
        t.a("▸ ")
    } else {
        t.n("  ")
    }
}

fn label(t: &Theme, text: &str, focused: bool) -> Span<'static> {
    if focused {
        Span::styled(text.to_owned(), Style::new().fg(t.accent).add_modifier(Modifier::BOLD))
    } else {
        t.b(text.to_owned())
    }
}

/// Who can open a site, in words.
pub fn reach_words(site: &Site) -> &'static str {
    let e = site.exposure;
    match (e.local, e.onion, e.public) {
        (_, _, true) if e.local || e.onion => "Home and internet",
        (_, _, true) => "Internet",
        (true, true, false) => "Home network and Tor",
        (true, false, false) => "Home network",
        (false, true, false) => "Tor",
        (false, false, false) => "Not online",
    }
}

fn site_address(site: &Site) -> String {
    if site.exposure.public {
        if let Some(host) = &site.public_host {
            return format!("https://{host}");
        }
    }
    if site.exposure.local && !site.local_url.is_empty() {
        return site.local_url.clone();
    }
    if site.exposure.onion && !site.onion_url.is_empty() {
        return site.onion_url.clone();
    }
    String::new()
}

fn short(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        let head: String = text.chars().take(width.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn details_toggle(open: bool) -> &'static str {
    if open {
        "Hide details ‹"
    } else {
        "Show details ›"
    }
}

/// The Web tab: only the sites that are online. A click opens that app's Hosting.
pub fn sites(app: &App, t: &Theme, selected: usize) -> Page {
    let (w, h) = size(app);
    let mut screen = Screen3::new(app, t, Tab::Web, w);
    let web = &app.data.web;
    let active = app.active_sites();
    let mut card = Card::new(t, w);
    let mut card_targets = Vec::new();
    card.blank(t);
    if active.is_empty() {
        card.row(t, vec![t.d("No site is online yet.")]);
    }
    let address_width = if w >= 100 { 34 } else { 24 };
    for (index, site) in active.iter().enumerate() {
        if index > 0 {
            card.blank(t);
        }
        let focused = index == selected;
        let status = if site.locked {
            t.a("  Locked until unlocked")
        } else if site.local_waiting {
            t.a("  Waiting for a home address")
        } else {
            t.n("")
        };
        card.row(
            t,
            vec![
                marker(t, focused),
                label(t, &pad(&site.package, 15), focused),
                t.n(pad(&short(&site_address(site), address_width), address_width + 2)),
                t.d(pad(reach_words(site), 22)),
                status,
                t.d("›"),
            ],
        );
        card_targets.push(Target::row(site.package.clone(), Click::Pick { index, then: Some(KeyCode::Enter) }));
    }
    card.blank(t);
    screen.before = vec![
        Item::Blank,
        Item::Row(Line::from(vec![margin(t), t.b("Sites online")]), Line::from(vec![t.a("Domain providers ›"), t.n("    ")])),
        Item::Blank,
    ];
    screen.targets.push(Target::text("Domain providers ›", Click::Key(KeyCode::Char('p'))));
    let mut after = vec![Item::Blank, quiet(t, "To put another app online, open it in Apps.")];
    if let Some(problem) = &web.local_problem {
        after.push(Item::Blank);
        after.push(line(vec![margin(t), t.a(problem.clone())]));
        if let Some(remedy) = &web.local_remedy {
            after.push(quiet(t, remedy));
        }
    }
    if let Some(note) = &web.local_records_note {
        after.push(Item::Blank);
        after.push(quiet(t, note));
    }
    if !web.sites.iter().any(|site| site.exposure.public) {
        after.push(Item::Blank);
        after.push(quiet(t, "Nothing is on the internet, so ports 80 and 443 are closed on this device."));
    }
    screen.after = after;
    screen.finish(
        t,
        card,
        card_targets,
        Vec::new(),
        "Keys: ↑↓ choose  ↵ open  p providers  n Tor links  d certificates",
        w,
        h,
    )
}

/// An app's Hosting: home network, Tor and internet, and for the internet the address.
pub fn exposure(app: &App, t: &Theme, state: &ExposureState) -> Page {
    let (w, h) = size(app);
    let mut screen = Screen3::new(app, t, app.screen.area(), w);
    let web = &app.data.web;
    let site = web.sites.iter().find(|s| s.package == state.site);
    let name = state.site.as_str();
    let focus = state.focus();
    let order = state.order();
    let index_of = |f: HostingFocus| order.iter().position(|x| *x == f).unwrap_or(0);
    let mut card = Card::new(t, w);
    let mut ct = Vec::new();
    card.blank(t);

    let local_url = site.map(|s| s.local_url.clone()).filter(|u| !u.is_empty()).unwrap_or_else(|| format!("https://{name}.local"));
    let choices = [
        (HostingFocus::Home, "Home network", state.draft.local, format!("Phones and computers on your Wi-Fi.   {local_url}")),
        (HostingFocus::Tor, "Tor", state.draft.onion, "A private link that opens in Tor Browser. It doesn't reveal your home.".to_owned()),
        (HostingFocus::Internet, "Internet", state.draft.public, "Anyone, at a web address you own.".to_owned()),
    ];
    for (index, (which, title, on, about)) in choices.into_iter().enumerate() {
        if index > 0 {
            card.blank(t);
        }
        let focused = focus == which && !state.picking;
        if focused {
            card.focus_here();
        }
        card.row(t, vec![marker(t, focused), tick(t, on), t.n("  "), label(t, title, focused)]);
        card.row(t, vec![t.d(format!("       {about}"))]);
        ct.push(Target::row(title, Click::Pick { index: index_of(which), then: Some(KeyCode::Char(' ')) }).below(1));
    }

    let field_label = |text: &str| t.d(format!("       {}", pad(text, 12)));
    if state.draft.public {
        card.blank(t);
        if state.has_provider {
            let choices = app.domain_choices();
            let provider = choices.iter().find(|(d, _)| *d == state.domain).map(|(_, p)| p.clone()).unwrap_or_default();
            card.row(t, vec![field_label("Provider"), t.n(provider)]);
            card.blank(t);
            let mut domain = vec![field_label("Domain")];
            let domain_focus = focus == HostingFocus::Domain && !state.picking;
            if domain_focus || state.picking {
                card.focus_here();
            }
            domain.extend(inset(t, &state.domain, 28, Some(if state.picking { "▴" } else { "▾" }), domain_focus));
            card.row(t, domain);
            ct.push(Target::text(state.domain.clone(), Click::Pick { index: index_of(HostingFocus::Domain), then: Some(KeyCode::Enter) }));
            if state.picking {
                let list = Style::new().fg(t.fg).bg(t.raised);
                let marker = |i: usize| if state.pick == i { "▸" } else { " " };
                for (i, (name, provider)) in choices.iter().enumerate() {
                    let tick = if *name == state.domain { "✓" } else { " " };
                    let text = format!(" {} {tick} {} {} ", marker(i), pad(name, 22), pad(provider, 14));
                    card.row(t, vec![t.n(" ".repeat(19)), Span::styled(text, list)]);
                    ct.push(Target::text(format!("{tick} {}", pad(name, 22)), Click::Pick { index: i, then: Some(KeyCode::Enter) }));
                }
                let add = choices.len();
                card.row(
                    t,
                    vec![t.n(" ".repeat(19)), Span::styled(format!(" {} {} ", marker(add), pad("+ Add a provider…", 39)), Style::new().fg(t.accent).bg(t.raised))],
                );
                ct.push(Target::text("+ Add a provider…", Click::Pick { index: add, then: Some(KeyCode::Enter) }));
            } else {
                card.blank(t);
                if focus == HostingFocus::Address {
                    card.focus_here();
                }
                let mut address = vec![field_label("Address")];
                address.extend(inset(t, state.sub.text(), 12, None, focus == HostingFocus::Address));
                address.push(t.n(format!(" .{}", state.domain)));
                card.row(t, address);
                ct.push(Target::row(format!(".{}", state.domain), Click::Pick { index: index_of(HostingFocus::Address), then: None }));
                card.blank(t);
                card.row(t, vec![t.d("       Visitors will be able to see your home's internet address.")]);
                if state.draft.onion {
                    card.row(t, vec![t.a("       With Tor on too, visitors could tell the two addresses are the same app.")]);
                }
            }
        } else {
            card.row(t, vec![t.n("       To use a web address you own, first add the company you bought it from.")]);
            card.row(t, vec![t.d("       You'll come back here when it's added.")]);
            card.blank(t);
            let at = index_of(HostingFocus::AddProvider);
            card.inline_button(t, "Add a provider", 7, Click::Pick { index: at, then: Some(KeyCode::Enter) }, &mut ct);
        }
    }
    card.blank(t);
    let details_focus = focus == HostingFocus::Details && !state.picking;
    if details_focus {
        card.focus_here();
    }
    card.row(t, vec![marker(t, details_focus), t.a(details_toggle(state.details))]);
    ct.push(Target::text(details_toggle(state.details), Click::Pick { index: index_of(HostingFocus::Details), then: Some(KeyCode::Enter) }));
    if state.details {
        if let Some(site) = site {
            if !site.onion_url.is_empty() {
                card.row(t, vec![t.d(format!("  Tor link        {}", site.onion_url))]);
            }
            if let Some(host) = &site.public_host {
                card.row(t, vec![t.d(format!("  Internet address  {host}"))]);
            }
        }
        if !web.acme.challenge.is_empty() {
            card.row(t, vec![t.d(format!("  Certificates    {}", web.acme.challenge))]);
        }
        if !web.public_ip.is_empty() {
            card.row(t, vec![t.d(format!("  Home's internet address   {}", web.public_ip))]);
        }
    }
    card.blank(t);
    let going_public = state.draft.public && site.is_some_and(|s| !s.exposure.public);
    let primary = if going_public { "Check and save" } else { "Save" };
    if order.contains(&HostingFocus::Save) {
        // The button saves wherever the keyboard focus is: it moves the focus to itself first.
        card.buttons(t, &[(primary, true, Click::Pick { index: index_of(HostingFocus::Save), then: Some(KeyCode::Enter) })], &mut ct);
    }

    screen.before = vec![
        trail(t, &["Apps", name, "Hosting"], &mut screen.targets),
        Item::Blank,
        heading(t, "Hosting"),
        quiet(t, &format!("Choose who can open {name}. Any combination works.")),
    ];
    let keys = if state.picking { "Keys: ↑↓ choose  ↵ pick  Esc close the list" } else { "Keys: Tab move  Space tick  ↵ save  Esc back" };
    screen.finish(t, card, ct, Vec::new(), keys, w, h)
}

/// Each connected provider's name, numbered when the owner connected two accounts at one company.
fn provider_labels(connected: &[ConnectedProvider]) -> Vec<String> {
    connected
        .iter()
        .enumerate()
        .map(|(index, provider)| {
            let earlier = connected[..index].iter().filter(|p| p.name == provider.name).count();
            if earlier == 0 { provider.name.clone() } else { format!("{} {}", provider.name, earlier + 1) }
        })
        .collect()
}

fn field_name(field: &str) -> String {
    match field {
        "api_key" => "API key".to_owned(),
        "secret_key" => "Secret key".to_owned(),
        "api_token" => "API token".to_owned(),
        "personal_access_token" => "Access token".to_owned(),
        "username" => "Username".to_owned(),
        "access_key_id" => "AccessKey ID".to_owned(),
        "access_key_secret" => "AccessKey secret".to_owned(),
        other => other.replace('_', " "),
    }
}

/// Domain providers: the list, choosing a company, its key, and done.
pub fn providers(app: &App, t: &Theme, state: &ProvidersState) -> Page {
    let (w, h) = size(app);
    let mut screen = Screen3::new(app, t, Tab::Web, w);
    let mut card = Card::new(t, w);
    let mut ct = Vec::new();
    let mut after = Vec::new();
    let mut after_targets = Vec::new();
    card.blank(t);
    let keys = match &state.stage {
        ProviderStage::List => {
            let connected = &app.data.connected;
            if connected.is_empty() {
                card.row(t, vec![t.d("None yet.")]);
                card.blank(t);
            }
            let labels = provider_labels(connected);
            for (index, (provider, label_text)) in connected.iter().zip(&labels).enumerate() {
                let focused = index == state.selected;
                if focused {
                    card.focus_here();
                }
                // One row per provider, as designed: its name, its domains, and Remove at the end.
                // A domain held through another provider is dim, since it can't be picked here.
                let mut row = vec![marker(t, focused), label(t, &pad(label_text, 14), focused)];
                for (i, domain) in provider.domains.iter().enumerate() {
                    if i > 0 {
                        row.push(t.n("   "));
                    }
                    row.push(if domain.held_by.is_some() { t.d(domain.name.clone()) } else { t.n(domain.name.clone()) });
                }
                if provider.domains.is_empty() {
                    row.push(t.d("no domains".to_owned()));
                }
                let remove = super::v3::button(t, "Remove", false, 10, t.card);
                let room = card.width.saturating_sub(4);
                let used = super::v3::width_of(&row) + super::v3::width_of(&remove);
                if used + 2 <= room {
                    row.push(t.n(" ".repeat(room - used)));
                    row.extend(remove);
                    card.row(t, row);
                } else {
                    // Too many domains for one row: they wrap, and Remove gets its own row.
                    card.row(t, row);
                    let mut alone = vec![t.n(" ".repeat(room.saturating_sub(super::v3::width_of(&remove))))];
                    alone.extend(remove);
                    card.row(t, alone);
                }
                ct.push(Target::row(label_text.clone(), Click::Pick { index, then: None }));
                ct.push(Target::button("Remove", Click::Pick { index, then: Some(KeyCode::Char('x')) }));
                let held: Vec<String> = provider
                    .domains
                    .iter()
                    .filter_map(|d| d.held_by.as_ref().map(|other| format!("{} is used through {other}", d.name)))
                    .collect();
                if !held.is_empty() {
                    card.row(t, vec![t.n("   "), t.d(held.join(". "))]);
                }
                if let Some(problem) = &provider.problem {
                    let again = "Look again";
                    card.row(t, vec![t.n("   "), t.e(problem.clone()), t.n("   "), t.a(again.to_owned())]);
                    ct.push(Target::text(again, Click::Pick { index, then: Some(KeyCode::Char('r')) }));
                }
                card.blank(t);
            }
            card.buttons(t, &[("Add a provider", true, Click::Key(KeyCode::Char('a')))], &mut ct);
            screen.before = vec![
                trail(t, &["Web", "Domain providers"], &mut screen.targets),
                Item::Blank,
                heading(t, "Domain providers"),
                quiet(t, "The companies that hold your web addresses. Add each once; then any app can use their domains."),
                Item::Blank,
            ];
            after.push(Item::Blank);
            after.push(line(vec![margin(t), t.a("Certificates and home names ›")]));
            after_targets.push(Target::text("Certificates and home names ›", Click::Key(KeyCode::Char('c'))));
            "Keys: ↑↓ choose  a add  r look again  x remove  c certificates  Esc back"
        }
        ProviderStage::Pick => {
            for (index, provider) in app.data.providers.iter().enumerate() {
                let focused = index == state.selected;
                if focused {
                    card.focus_here();
                }
                let note = if !provider.is_supported() {
                    t.d("Can't be connected: no way to change records".to_owned())
                } else {
                    t.d(provider.dns01_note.clone().unwrap_or_default())
                };
                card.row(t, vec![marker(t, focused), label(t, &pad(&provider.name, 14), focused), note]);
                ct.push(Target::row(provider.name.clone(), Click::Pick { index, then: Some(KeyCode::Enter) }));
            }
            card.blank(t);
            let first = if state.return_to.is_some() { "Apps" } else { "Web" };
            screen.before = vec![
                trail(t, &[first, "Domain providers", "Add"], &mut screen.targets),
                Item::Blank,
                heading(t, "Where did you buy your web address?"),
            ];
            match &state.test {
                Some(Err(text)) => screen.before.push(line(vec![margin(t), t.e(text.clone())])),
                _ => screen.before.push(Item::Blank),
            }
            after.push(Item::Blank);
            after.push(quiet(t, "Somewhere else? It can't be connected yet, but Tor works without a web address."));
            "Click the company.   Keys: ↑↓ choose  ↵ next  Esc back"
        }
        ProviderStage::Key => {
            let provider = app.data.providers.get(state.selected);
            let name = provider.map_or("the provider", |p| p.name.as_str());
            let names: Vec<String> = provider.map(|p| p.fields.iter().map(|f| field_name(f)).collect()).unwrap_or_default();
            let lists = provider.is_some_and(|p| p.lists_domains);
            // Targets follow the boxes in order, the same order the keys move through.
            let mut index = 0;
            if !lists {
                let mut domain = vec![t.d(pad("Web address", 15))];
                domain.extend(inset(t, state.domain.text(), 34, None, state.focus == ProviderFocus::Domain));
                card.row(t, domain);
                ct.push(Target::row("Web address", Click::Pick { index, then: None }));
                index += 1;
                card.blank(t);
            }
            let first = names.first().cloned().unwrap_or_else(|| "API key".to_owned());
            let mut key = vec![t.d(pad(&first, 15))];
            key.extend(inset(t, &state.api_key.display(), 34, None, state.focus == ProviderFocus::ApiKey));
            card.row(t, key);
            ct.push(Target::row(first, Click::Pick { index, then: None }));
            index += 1;
            if let Some(second) = names.get(1) {
                card.blank(t);
                let mut secret = vec![t.d(pad(second, 15))];
                secret.extend(inset(t, &state.secret_key.display(), 34, None, state.focus == ProviderFocus::SecretKey));
                card.row(t, secret);
                ct.push(Target::row(second.clone(), Click::Pick { index, then: None }));
            }
            card.blank(t);
            match &state.test {
                Some(Err(text)) => card.row(t, vec![t.e(text.clone())]),
                _ if lists => card.row(t, vec![t.d(format!("The Nodo asks {name} which domains this key controls. The key stays here, locked, never shown again."))]),
                _ => card.row(t, vec![t.d("The key stays on this Nodo, locked with your device passphrase, and is never shown again.")]),
            }
            card.blank(t);
            card.buttons(t, &[("Connect", true, Click::Key(KeyCode::Enter))], &mut ct);
            let instructions = provider.map(|p| p.instructions.clone()).unwrap_or_default();
            screen.before = vec![
                trail(t, &["Web", "Domain providers", "Add", name], &mut screen.targets),
                Item::Blank,
                heading(t, &format!("Connect {name}")),
                quiet(t, &instructions),
                Item::Blank,
            ];
            "Keys: Tab next box  ↵ connect  Esc back"
        }
        ProviderStage::Done { provider, domains } => {
            card.row(t, vec![t.o("✓  "), t.b(format!("{provider} is connected"))]);
            card.blank(t);
            if domains.is_empty() {
                card.row(t, vec![t.d("   Its domains are already used through another provider.")]);
            } else {
                card.row(t, vec![t.d("   Apps can now use these web addresses:")]);
                for domain in domains {
                    card.row(t, vec![t.n(format!("   {domain}"))]);
                }
            }
            card.blank(t);
            let back = state.return_to.as_ref().map_or("Done".to_owned(), |site| format!("Back to {site}"));
            card.buttons(t, &[(back.as_str(), true, Click::Key(KeyCode::Enter))], &mut ct);
            screen.before =
                vec![trail(t, &["Web", "Domain providers", "Add", provider.as_str()], &mut screen.targets), Item::Blank, Item::Blank];
            "Keys: ↵ continue"
        }
    };
    screen.after = after;
    screen.finish(t, card, ct, after_targets, keys, w, h)
}

fn check_symbol(t: &Theme, state: CheckState) -> Span<'static> {
    match state {
        CheckState::Passed => t.o("✓  "),
        CheckState::Failed => t.e("✕  "),
        CheckState::Unknown => t.a("?  "),
    }
}

/// The checks found a problem, or something could not be verified.
pub fn check(app: &App, t: &Theme, state: &WebCheckState) -> Page {
    let (w, h) = size(app);
    let mut screen = Screen3::new(app, t, app.screen.area(), w);
    let mut card = Card::new(t, w);
    let mut ct = Vec::new();
    card.blank(t);
    let failed = state.checks.iter().any(|c| c.state == CheckState::Failed);
    for check in &state.checks {
        card.row(t, vec![check_symbol(t, check.state), t.n(check.text.clone())]);
        if check.state != CheckState::Passed && !check.note.is_empty() {
            card.row(t, vec![t.d(format!("   {}", check.note))]);
        }
    }
    if let Some(router) = &state.router {
        card.blank(t);
        card.row(t, vec![t.n("Your router needs to send web visitors to the Nodo. In its settings, forward:")]);
        card.row(t, vec![t.n(format!("   Port 80   {}", router.forward_http))]);
        card.row(t, vec![t.n(format!("   Port 443  {}", router.forward_https))]);
        card.row(t, vec![t.d(format!("   The router's settings page is usually {}.", router.admin_page))]);
    }
    card.blank(t);
    card.row(t, vec![t.d("A private Tor link works without any of this.")]);
    card.blank(t);
    card.row(t, vec![t.a(details_toggle(state.details))]);
    ct.push(Target::text(details_toggle(state.details), Click::Key(KeyCode::Char('d'))));
    if state.details {
        for step in &state.manual {
            card.row(t, vec![t.d(format!("  {step}"))]);
        }
        if let Some(plan) = &state.plan {
            card.row(t, vec![t.d(format!("  Record    {}", plan.dns_record))]);
            card.row(t, vec![t.d(format!("  Firewall  {}", plan.firewall))]);
        }
    }
    card.blank(t);
    let mut buttons: Vec<(&str, bool, Click)> = vec![("Check again", false, Click::Key(KeyCode::Char('r')))];
    if state.going_public && !failed && state.plan.is_some() {
        buttons.push(("Continue anyway", false, Click::Key(KeyCode::Char('c'))));
    }
    buttons.push(("Use Tor instead", true, Click::Key(KeyCode::Char('o'))));
    card.buttons(t, &buttons, &mut ct);
    let title = if failed { "The internet can't reach your Nodo yet" } else { "Some checks couldn't be confirmed" };
    screen.before = vec![
        trail(t, &["Apps", state.site.as_str(), "Hosting"], &mut screen.targets),
        Item::Blank,
        heading(t, title),
        Item::Blank,
    ];
    screen.finish(t, card, ct, Vec::new(), "Keys: ↵ use Tor  r check again  d details  ? router help  Esc back", w, h)
}

/// The checks passed: one clear confirm, starting on Cancel.
pub fn confirm(app: &App, t: &Theme, state: &WebConfirmState) -> Page {
    let (w, h) = size(app);
    let mut screen = Screen3::new(app, t, app.screen.area(), w);
    let mut card = Card::new(t, w);
    let mut ct = Vec::new();
    let web = &app.data.web;
    card.blank(t);
    let provider = if web.provider.is_empty() { "Your provider".to_owned() } else { web.provider.clone() };
    for text in [format!("{provider} accepted the key"), format!("{} points to your home", state.plan.host), "The internet can reach your Nodo".to_owned()] {
        card.row(t, vec![t.o("✓  "), t.n(text)]);
    }
    card.blank(t);
    card.row(t, vec![t.n("Anyone will be able to open "), t.b(state.plan.url.clone())]);
    card.row(t, vec![t.a("Your home's internet address becomes public.")]);
    card.row(t, vec![t.a("Records of it can stay online after you stop.")]);
    card.row(t, vec![t.a("Anyone can connect this address to your home, including your Monero node.")]);
    card.row(t, vec![t.a(format!("Public certificate logs list {} permanently.", state.plan.host))]);
    card.row(t, vec![t.d("The Nodo keeps the secure connection up to date. Take it offline at any time.")]);
    if web.sites.iter().any(|s| s.package == state.site && s.rate_limit_unenforced) {
        card.row(t, vec![t.a("Repeated requests can't be slowed down for this app.")]);
    }
    card.blank(t);
    card.row(t, vec![t.a(details_toggle(state.details))]);
    ct.push(Target::text(details_toggle(state.details), Click::Key(KeyCode::Char('d'))));
    if state.details {
        card.row(t, vec![t.d(format!("  Record    {}", state.plan.dns_record))]);
        card.row(t, vec![t.d(format!("  Firewall  {}", state.plan.firewall))]);
    }
    card.blank(t);
    let go = format!("Put {} online", state.site);
    card.buttons(
        t,
        &[("Cancel", false, Click::Pick { index: 0, then: Some(KeyCode::Enter) }), (go.as_str(), true, Click::Pick { index: 1, then: Some(KeyCode::Enter) })],
        &mut ct,
    );
    let selected = if state.focus == ConfirmFocus::Cancel { "Cancel".to_owned() } else { go.clone() };
    card.row(t, vec![t.d(format!("▸ {selected} is selected for the keyboard."))]);
    card.blank(t);
    screen.before = vec![
        trail(t, &["Apps", state.site.as_str(), "Hosting"], &mut screen.targets),
        Item::Blank,
        heading(t, &format!("Ready to put {} on the internet", state.site)),
        Item::Blank,
    ];
    screen.finish(t, card, ct, Vec::new(), "Keys: Tab choose  ↵ press  Esc cancel", w, h)
}

/// The internet provider shares one address between homes, so visitors can't reach the Nodo.
pub fn cgnat(app: &App, t: &Theme, state: &CgnatState) -> Page {
    let (w, h) = size(app);
    let mut screen = Screen3::new(app, t, app.screen.area(), w);
    let mut card = Card::new(t, w);
    let mut ct = Vec::new();
    card.blank(t);
    card.row(
        t,
        vec![t.n("Your internet provider shares one address between many homes, so visitors stop at the provider and never reach you. Router settings can't change this.")],
    );
    card.blank(t);
    card.row(t, vec![t.d("A private Tor link still works on this connection.")]);
    card.blank(t);
    card.row(t, vec![t.a(details_toggle(state.details))]);
    ct.push(Target::text(details_toggle(state.details), Click::Key(KeyCode::Char('d'))));
    if state.details {
        card.row(t, vec![t.d(format!("  Your router's outside address  {} (in {}, the shared range)", state.wan_address, state.wan_range))]);
        card.row(t, vec![t.d(format!("  What the internet sees         {}", state.internet_address))]);
        card.blank(t);
        for (index, option) in ["Ask your provider for a public address", "Use IPv6, if your provider has it", "Use a relay that forwards visitors"]
            .into_iter()
            .enumerate()
        {
            card.row(t, vec![marker(t, state.choice == index + 1), t.n(format!("{option} ›"))]);
            ct.push(Target::row(option, Click::Pick { index: index + 1, then: Some(KeyCode::Enter) }));
        }
    }
    card.blank(t);
    card.buttons(t, &[("Check again", false, Click::Key(KeyCode::Char('r'))), ("Use Tor instead", true, Click::Key(KeyCode::Char('o')))], &mut ct);
    screen.before = vec![
        trail(t, &["Apps", state.site.as_str(), "Hosting"], &mut screen.targets),
        Item::Blank,
        heading(t, "The internet can't reach your Nodo"),
        Item::Blank,
    ];
    screen.finish(t, card, ct, Vec::new(), "Keys: o use Tor  r check again  d details  Esc back", w, h)
}

/// After going public: online, with the secure connection followed until it is in place.
pub fn online(app: &App, t: &Theme, state: &CertificateState) -> Page {
    let (w, h) = size(app);
    let mut screen = Screen3::new(app, t, app.screen.area(), w);
    let web = &app.data.web;
    let mut card = Card::new(t, w);
    let mut ct = Vec::new();
    let cert = web.cert(&state.host);
    let cert_state = cert.map(|c| c.state).unwrap_or(nodo_api::CertState::Waiting);
    let broken = matches!(cert_state, nodo_api::CertState::Failed | nodo_api::CertState::Expired);
    card.blank(t);
    match cert_state {
        nodo_api::CertState::Valid => card.row(t, vec![t.o("✓  "), t.b(format!("{} is online", state.site))]),
        _ if broken => card.row(t, vec![t.e("✕  "), t.b("The secure connection isn't ready")]),
        _ => card.row(t, vec![t.a("◆  "), t.b(format!("{} is online. Getting its secure connection…", state.site))]),
    }
    card.blank(t);
    card.row(t, vec![t.n(format!("   https://{}", state.host))]);
    if let Some(site) = web.sites.iter().find(|s| s.package == state.site) {
        if site.exposure.local && !site.local_url.is_empty() {
            card.row(t, vec![t.n(format!("   {}", site.local_url))]);
        }
    }
    card.blank(t);
    match cert.and_then(|c| c.problem.clone()) {
        Some(problem) if broken => {
            card.row(t, vec![t.e(format!("   {problem}"))]);
            card.row(t, vec![t.d("   The Nodo keeps trying. Visitors see a warning until it works.")]);
        }
        _ => card.row(t, vec![t.d("   The Nodo renews the secure connection and follows your home's address by itself.")]),
    }
    card.blank(t);
    card.buttons(t, &[("Done", true, Click::Key(KeyCode::Enter))], &mut ct);
    screen.before = vec![trail(t, &["Apps", state.site.as_str(), "Hosting"], &mut screen.targets), Item::Blank, Item::Blank];
    screen.finish(t, card, ct, Vec::new(), "Keys: ↵ done  l logs  d certificates", w, h)
}
