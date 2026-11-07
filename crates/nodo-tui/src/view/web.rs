//! The six screens for putting a site online.

use nodo_api::Exposure;
use ratatui::text::{Line, Span};

use super::layout::{Item, Page, Panel};
use super::{button_line, checkbox, hints, mark, pad, tab_bar};
use crate::app::*;
use crate::model::*;
use crate::secret::{Input, STORED_MASK};
use crate::theme::Theme;

fn crumbs(t: &Theme, site: &str, leaf: &str) -> Line<'static> {
    Line::from(vec![t.n("Web "), t.d("› "), t.n(format!("{site} ")), t.d("› "), t.b(leaf.to_owned())])
}

fn online_count(exposure: Exposure) -> usize {
    usize::from(exposure.local) + usize::from(exposure.onion) + usize::from(exposure.public)
}

pub fn sites(app: &App, t: &Theme, selected: usize) -> Page {
    let web = &app.data.web;
    let mut rows = vec![Item::Row(
        Line::from(t.d(" Package        Local  Onion  Public  Public address")),
        Line::from(t.d("Status")),
    )];
    for (index, site) in web.sites.iter().enumerate() {
        let is_selected = index == selected;
        let on = |flag: bool, symbol: &'static str, span: fn(&Theme, &'static str) -> Span<'static>| {
            if flag {
                span(t, symbol)
            } else {
                t.d("·")
            }
        };
        let e = site.exposure;
        let address = if e.public {
            site.public_host.clone().unwrap_or_default()
        } else if online_count(e) > 0 {
            "Not public".to_owned()
        } else {
            "Off".to_owned()
        };
        let status = if site.locked {
            t.a("◆ Device locked")
        } else {
            match online_count(e) {
                0 => t.d("Off"),
                1 => t.o("Online"),
                _ => t.o("All online"),
            }
        };
        let left = Line::from(vec![
            t.n(mark(is_selected, &format!(" {}", pad(&site.package, 15)))),
            on(e.local, "●", |t, s| t.o(s)),
            t.n("      "),
            on(e.onion, "◉", |t, s| t.i(s)),
            t.n("      "),
            on(e.public, "▲", |t, s| t.a(s)),
            t.n("       "),
            if e.public { t.n(address) } else { t.d(address) },
        ]);
        rows.push(if is_selected {
            Item::Selected(left, Line::from(status))
        } else {
            Item::Row(left, Line::from(status))
        });
    }
    rows.push(Item::Blank);
    rows.push(Item::Text(Line::from(t.d(" ● Local network   ◉ Tor onion   ▲ Public, reveals your home IP   · off"))));
    rows.push(Item::Text(Line::from(t.d(
        " A site can use any combination. Each address is switched on and off separately.",
    ))));

    let pair = |label: &str, value: Span<'static>| Item::Row(Line::from(t.d(label.to_owned())), Line::from(value));
    let domain = Panel::new(
        "Domain",
        vec![
            pair("Domain", t.n(web.domain.clone())),
            pair("Provider", t.n(web.provider.clone())),
            pair("Certificates", t.o(web.certificates.clone())),
        ],
    );
    let ddns = Panel::new(
        "Dynamic DNS",
        vec![
            pair("Public IP", t.n(web.public_ip.clone())),
            pair("DNS matches", if web.dns_matches { t.o("✓ Yes") } else { t.e("✕ No") }),
            pair("Last changed", t.n(web.dns_changed.clone())),
        ],
    );
    let public: Vec<&str> =
        web.sites.iter().filter(|site| site.exposure.public).map(|site| site.package.as_str()).collect();
    let ports = if public.is_empty() {
        Item::Text(Line::from(t.n("80 and 443 are closed on this device, because no site is public.")))
    } else {
        let names = public.join(", ");
        let verb = if public.len() == 1 { "is" } else { "are" };
        Item::Row(
            Line::from(t.n(format!("80 and 443 are open on this device because {names} {verb} public."))),
            Line::from(if web.reachable { t.o("✓ Reachable") } else { t.e("✕ Not reachable") }),
        )
    };
    Page::new(tab_bar(app, t, Tab::Web), Line::from(t.d("monokulo-nodo")))
        .panel(Panel::new("Sites", rows).grow())
        .columns(vec![(1, vec![domain]), (1, vec![ddns])])
        .panel(Panel::new("Ports", vec![ports]))
        .footer(Item::Text(hints(
            t,
            &[
                ("↵", "change exposure"),
                ("d", "domains"),
                ("t", "test reachability"),
                ("l", "proxy log"),
                ("o", "take all sites offline"),
            ],
        )))
}

pub fn exposure(app: &App, t: &Theme, state: &ExposureState) -> Page {
    let Some(site) = app.data.web.sites.iter().find(|site| site.package == state.site) else {
        return Page::new(crumbs(t, &state.site, "Exposure"), Line::default());
    };
    let saved = site.exposure;
    let draft = state.draft;
    let changes = usize::from(draft.local != saved.local)
        + usize::from(draft.onion != saved.onion)
        + usize::from(draft.public != saved.public);
    let right = match changes {
        0 => t.d("No unsaved changes"),
        1 => t.a("● 1 unsaved change"),
        n => t.a(format!("● {n} unsaved changes")),
    };
    let status = |on_now: bool, on_draft: bool| -> Span<'static> {
        match (on_now, on_draft) {
            (true, true) => t.o("Online"),
            (false, true) => t.a("Turning on"),
            (true, false) => t.a("Turning off"),
            (false, false) => t.d("Off"),
        }
    };
    let host = site.public_host.as_deref().map_or_else(|| "no address yet".to_owned(), |host| format!("https://{host}"));
    let rows = [
        (draft.local, t.o("●"), "Local network", site.local_url.clone(), saved.local, "Devices on your Wi-Fi or LAN"),
        (draft.onion, t.i("◉"), "Tor onion", site.onion_url.clone(), saved.onion, "Anyone with the address, using Tor Browser"),
        (draft.public, t.a("▲"), "Public internet", host, saved.public, "Anyone, at your own domain"),
    ];
    let mut items = Vec::new();
    for (index, (on, symbol, label, address, was_on, note)) in rows.into_iter().enumerate() {
        let selected = index == state.selected;
        let left = Line::from(vec![t.n(mark(selected, &format!(" {} ", checkbox(on)))), symbol, t.n(format!(" {label}"))]);
        let right = Line::from(vec![t.n(format!("{address}  ")), status(was_on, on)]);
        items.push(if selected { Item::Selected(left, right) } else { Item::Row(left, right) });
        items.push(Item::Text(Line::from(t.d(format!("       {note}")))));
        if index < 2 {
            items.push(Item::Blank);
        }
    }
    if let Some(input) = &state.editing_host {
        items.push(Item::Blank);
        items.push(Item::Text(Line::from(vec![t.d(" Public address: "), t.n(input.display()), t.s(" ")])));
    }

    let mut page = Page::new(
        Line::from(vec![t.n("Web "), t.d("› "), t.b(state.site.clone()), t.n(" "), t.d("› "), t.n("Exposure")]),
        Line::from(right),
    )
    .panel(Panel::new(
        format!("Where is {} available? Turn on any combination.", state.site),
        items,
    ));
    if draft.public && !saved.public {
        let mut warning = vec![
            Item::Text(Line::from(vec![t.a("▲"), t.n(" Your home IP becomes visible in public DNS.")])),
            Item::Text(Line::from(vec![
                t.a("▲"),
                t.n(format!(" {} can be reached and attacked by anyone on the internet.", state.site)),
            ])),
            Item::Text(Line::from(vec![t.a("▲"), t.n(" Your router must forward ports 80 and 443 to this device.")])),
        ];
        if draft.onion {
            warning.push(Item::Blank);
            warning.push(Item::Text(Line::from(t.e("✕ Keeping onion on too can link it to your home IP."))));
            warning.push(Item::Text(Line::from(t.d(
                "  Identical content on both addresses lets visitors connect them. Use a separate",
            ))));
            warning.push(Item::Text(Line::from(t.d("  site for onion if that address must stay anonymous."))));
        }
        warning.push(Item::Blank);
        warning.push(Item::Text(Line::from(t.d("Local and onion stay online while public is checked and confirmed."))));
        page = page.panel(Panel::accent("Turning on public internet", warning).grow());
    }
    let keys: &[(&str, &str)] = if state.editing_host.is_some() {
        &[("type", "an address"), ("↵", "done"), ("Esc", "cancel")]
    } else {
        &[("↑↓", "move"), ("Space", "toggle"), ("↵", "save and check"), ("e", "edit address"), ("Esc", "back")]
    };
    page.footer(Item::Text(hints(t, keys)))
}

fn capability_span(t: &Theme, capability: Capability) -> Span<'static> {
    let text = match capability {
        Capability::Yes => "✓",
        Capability::Account => "▲ account",
        Capability::Limited => "▲ limited",
        Capability::PortsNeeded => "Ports needed",
        Capability::NoApi => "✕ no API",
    };
    let padded = pad(text, 14);
    match capability {
        Capability::Yes => t.o(padded),
        Capability::Account | Capability::Limited => t.a(padded),
        Capability::PortsNeeded => t.d(padded),
        Capability::NoApi => t.e(padded),
    }
}

fn credential(t: &Theme, label: &str, input: &Input, focused: bool, stored: bool, mask: &str) -> Vec<Item> {
    let mut value = vec![if input.is_empty() && stored { t.n(mask.to_owned()) } else { t.n(input.display()) }];
    if focused {
        value.push(t.s(" "));
    }
    let label = if focused { t.b(format!("▸{label}")) } else { t.d(label.to_owned()) };
    vec![Item::Text(Line::from(label)), Item::Text(Line::from(value))]
}

pub fn providers(app: &App, t: &Theme, state: &ProvidersState) -> Page {
    let mut rows = vec![Item::Text(Line::from(t.d(format!(
        "   {}{}{}",
        pad("Provider", 16),
        pad("Dynamic DNS", 14),
        "HTTPS without open ports"
    ))))];
    for (index, provider) in app.data.providers.iter().enumerate() {
        let selected = index == state.selected;
        let left = Line::from(vec![
            t.n(format!(" {} {}", if selected { "▸" } else { " " }, pad(&provider.name, 16))),
            capability_span(t, provider.dynamic_dns),
            capability_span(t, provider.https_without_ports),
        ]);
        rows.push(if selected { Item::Selected(left, Line::default()) } else { Item::Row(left, Line::default()) });
    }

    let mut detail = Vec::new();
    let current = app.data.providers.get(state.selected);
    if let Some(provider) = current {
        if provider.is_supported() {
            let stored = provider.name == app.data.web.provider;
            detail.push(Item::Text(Line::from(t.n(provider.instructions.clone()))));
            detail.push(Item::Blank);
            let long_mask = format!("{STORED_MASK}••••");
            detail.extend(credential(t, "Domain", &state.domain, state.focus == ProviderFocus::Domain, false, ""));
            detail.extend(credential(t, "API key", &state.api_key, state.focus == ProviderFocus::ApiKey, stored, &long_mask));
            detail.extend(credential(
                t,
                "Secret key",
                &state.secret_key,
                state.focus == ProviderFocus::SecretKey,
                stored,
                &long_mask,
            ));
            detail.push(Item::Blank);
            match &state.test {
                Some(Ok(text)) => detail.push(Item::Text(Line::from(t.o(format!("✓ {text}"))))),
                Some(Err(text)) => detail.push(Item::Text(Line::from(t.e(format!("✕ {text}"))))),
                None => {}
            }
            detail.push(Item::Text(Line::from(t.d("Stored as a secret. Packages cannot read it."))));
        } else {
            detail.push(Item::Text(Line::from(t.e("✕ Not supported"))));
            detail.push(Item::Blank);
            detail.push(Item::Text(Line::from(t.n(provider.instructions.clone()))));
            detail.push(Item::Blank);
            detail.push(Item::Text(Line::from(t.d("Monokulo only works with providers that have an API."))));
        }
    }
    let supported = current.is_some_and(Provider::is_supported);
    let keys: &[(&str, &str)] = if supported {
        &[("↑↓", "provider"), ("Tab", "fields"), ("t", "test"), ("↵", "save"), ("?", "where to find keys"), ("Esc", "back")]
    } else {
        &[("↑↓", "provider"), ("Esc", "back")]
    };
    let title = current.map_or_else(String::new, |provider| provider.name.clone());
    Page::new(
        Line::from(vec![t.n("Web "), t.d("› "), t.n("Domains "), t.d("› "), t.b("Connect a domain")]),
        Line::from(t.d("Bring your own domain")),
    )
    .columns(vec![
        (62, vec![Panel::new("Where is your DNS hosted?", rows).grow()]),
        (38, vec![Panel::new(title, detail).grow()]),
    ])
    .footer(Item::Text(hints(t, keys)))
}

fn check_rows(t: &Theme, checks: &[Check]) -> Vec<Item> {
    checks
        .iter()
        .map(|check| match check.state {
            CheckState::Passed => Item::Row(
                Line::from(vec![t.o("✓"), t.n(format!(" {}", check.text))]),
                Line::from(t.d(check.note.clone())),
            ),
            CheckState::Failed => Item::Row(
                Line::from(vec![t.e("✕ "), t.b(check.text.clone())]),
                Line::from(t.e(check.note.clone())),
            ),
        })
        .collect()
}

pub fn check(t: &Theme, state: &WebCheckState) -> Page {
    let failed = state.checks.iter().any(|check| check.state == CheckState::Failed);
    let right = if failed { t.a("▲ One step needs you") } else { t.o("✓ All checks passed") };
    let mut page = Page::new(crumbs(t, &state.site, "Reachability check"), Line::from(right))
        .panel(Panel::new("Checks", check_rows(t, &state.checks)));
    if let Some(router) = &state.router {
        let pair = |label: &str, value: &str| Item::Row(Line::from(t.d(label.to_owned())), Line::from(t.n(value.to_owned())));
        page = page.panel(Panel::new(
            "Set up port forwarding on your router",
            vec![
                pair("Router admin page", &router.admin_page),
                pair("Forward TCP 80", &router.forward_http),
                pair("Forward TCP 443", &router.forward_https),
                Item::Blank,
                Item::Text(Line::from(t.d(format!(
                    "Also reserve {} for this device, so the forwarding keeps working.",
                    router.device_ip
                )))),
                Item::Text(Line::from(t.d("Monokulo will not change your router automatically. UPnP is off by default."))),
            ],
        ));
    }
    if failed {
        page = page.plain(vec![Item::Text(Line::from(t.n(format!(
            " {} is not public yet. Its local and onion addresses keep working during this check.",
            state.site
        ))))]);
    }
    page.footer(Item::Text(hints(t, &[("r", "check again"), ("?", "router guides"), ("o", "turn on Tor onion"), ("Esc", "back")])))
}

pub fn confirm(t: &Theme, state: &WebConfirmState) -> Page {
    let plan = &state.plan;
    let pair = |label: &str, value: &str| Item::Row(Line::from(t.d(label.to_owned())), Line::from(t.n(value.to_owned())));
    let access_row = |focus: ConfirmFocus, on: bool, label: &str, note: &str| {
        let selected = state.focus == focus;
        let left = Line::from(t.n(mark(selected, &format!(" {} {label}", checkbox(on)))));
        if selected {
            Item::Selected(left, Line::from(note.to_owned()))
        } else {
            Item::Row(left, Line::from(t.d(note.to_owned())))
        }
    };
    let mut address = vec![Item::Text(Line::from(t.d("Type the address to confirm")))];
    let typing = state.focus == ConfirmFocus::Address;
    let mut typed = vec![t.n(state.typed.display())];
    if typing {
        typed.push(t.s(" "));
    }
    address.push(Item::Text(Line::from(typed)));
    if let Some(error) = &state.error {
        address.push(Item::Text(Line::from(t.e(format!("✕ {error}")))));
    }
    Page::new(crumbs(t, &state.site, "Go public"), Line::from(t.o("✓ All checks passed")))
        .panel(Panel::accent(
            format!("This puts {} on the public internet", state.site),
            vec![
                pair("Address", &plan.url),
                pair("DNS record", &plan.dns_record),
                pair("Device firewall", &plan.firewall),
            ],
        ))
        .panel(Panel::new(
            "Access",
            vec![
                access_row(
                    ConfirmFocus::Password,
                    state.access.require_password,
                    "Require a password in front of this site",
                    &format!("{} has its own login", state.site),
                ),
                access_row(ConfirmFocus::RateLimit, state.access.rate_limit, "Rate-limit repeated requests", "Recommended"),
                access_row(
                    ConfirmFocus::HideAdmin,
                    state.access.hide_admin,
                    "Hide admin paths from the public",
                    "/admin stays local network only",
                ),
            ],
        ))
        .panel(
            Panel::new(
                "Before you confirm",
                vec![
                    Item::Text(Line::from(vec![t.a("▲"), t.n(" Your home IP will be published, and DNS history can keep it after you stop.")])),
                    Item::Text(Line::from(vec![t.a("▲"), t.n(" Anyone can link this domain to your IP, including your Monero node's IP.")])),
                    Item::Text(Line::from(vec![t.a("▲"), t.n(format!(" Certificate logs record {} permanently.", plan.host))])),
                    Item::Blank,
                    Item::Text(Line::from(t.d("You can take this offline at any time from the Web tab."))),
                ],
            )
            .grow(),
        )
        .plain(address)
        .footer(Item::Row(
            button_line(t, "Cancel", "Go public", state.focus == ConfirmFocus::GoPublic),
            hints(t, &[("Tab", "fields"), ("Space", "toggle"), ("↵", "confirm"), ("Esc", "cancel")]),
        ))
}

pub fn cgnat(t: &Theme, state: &CgnatState) -> Page {
    let options = [
        ("◉ Turn on Tor onion", "Works now, no domain needed"),
        ("  Ask your ISP for a public IP", "Some offer it free or as an add-on"),
        ("  Use IPv6 if your ISP provides it", "Only IPv6 visitors can reach you"),
        ("  Connect your own relay tunnel", "Needs a server you control"),
    ];
    let mut choices: Vec<Item> = options
        .iter()
        .enumerate()
        .map(|(index, (label, note))| {
            let selected = index == state.choice;
            let left = Line::from(t.n(mark(selected, &format!(" {label}"))));
            if selected {
                Item::Selected(left, Line::from((*note).to_owned()))
            } else {
                Item::Row(left, Line::from(t.d((*note).to_owned())))
            }
        })
        .collect();
    choices.push(Item::Blank);
    choices.push(Item::Text(Line::from(t.d("Your domain stays connected. Run this check again if your connection changes."))));
    Page::new(crumbs(t, &state.site, "Reachability check"), Line::from(t.e("✕ Public cannot work on this connection")))
        .panel(Panel::error(
            "Your ISP shares your public IP (CGNAT)",
            vec![Item::Text(Line::from(t.n(
                "Router forwarding will not help, because traffic stops at your ISP before it reaches your router. Dynamic DNS would point visitors at an address you do not control.",
            )))],
        ))
        .panel(Panel::new(
            "Evidence",
            vec![
                Item::Row(
                    Line::from(t.d("Your router's WAN address")),
                    Line::from(vec![t.n(format!("{} ", state.wan_address)), t.d(format!("in {}", state.wan_range))]),
                ),
                Item::Row(Line::from(t.d("Address the internet sees")), Line::from(t.n(state.internet_address.clone()))),
            ],
        ))
        .panel(Panel::new("What you can do", choices).grow())
        .footer(Item::Text(hints(t, &[("↑↓", "choose"), ("↵", "continue"), ("r", "check again"), ("Esc", "back")])))
}
