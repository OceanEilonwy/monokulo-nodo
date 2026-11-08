//! Domains and certificates, and following a certificate while Caddy gets it.

use nodo_api::{CertState, LocalNameVerdict};
use ratatui::text::{Line, Span};

use super::layout::{Item, Page, Panel};
use super::{checkbox, hints, mark, pad};
use crate::app::*;
use crate::model::*;
use crate::theme::Theme;

/// One certificate in a few words, coloured by how it stands.
pub fn cert_status(t: &Theme, row: &CertRow) -> Span<'static> {
    match row.state {
        CertState::Valid => match row.days_left {
            Some(days) => t.o(format!("✓ Valid, {days} days left")),
            None => t.o("✓ Valid"),
        },
        CertState::Obtaining | CertState::Waiting => t.a("◆ Getting a certificate"),
        CertState::Failed => t.e("✕ Not issued"),
        CertState::Expired => t.e("✕ Expired"),
    }
}

/// The Certificates line of the Web overview: the first problem, else the next renewal.
pub fn overview_line(t: &Theme, web: &WebOverview) -> Span<'static> {
    let acme: Vec<&CertRow> = web.certs.iter().filter(|c| c.kind != CertKind::LocalInternal).collect();
    if let Some(bad) = acme.iter().find(|c| matches!(c.state, CertState::Failed | CertState::Expired)) {
        return t.e(format!("✕ {} has none", bad.host));
    }
    if let Some(busy) = acme.iter().find(|c| matches!(c.state, CertState::Obtaining | CertState::Waiting)) {
        return t.a(format!("◆ Getting one for {}", busy.host));
    }
    match acme.iter().filter_map(|c| c.days_left).min() {
        Some(days) => t.o(format!("✓ Renew in {days} days")),
        None if web.certificates.is_empty() => t.d("No public sites"),
        None => t.n(web.certificates.clone()),
    }
}

/// The certificate note under an address on the Exposure screen.
pub fn exposure_note(t: &Theme, row: &CertRow) -> Line<'static> {
    let mut spans = vec![t.d("       Certificate: ")];
    match row.state {
        CertState::Valid => {
            spans.push(t.o("✓ "));
            spans.push(t.n(row.issuer.clone()));
            if let Some(days) = row.days_left {
                spans.push(t.d(format!(", {days} days left")));
            }
            if let Some(renewed) = &row.renewed {
                spans.push(t.d(format!(", renewed {}", renewed.to_lowercase())));
            }
        }
        CertState::Obtaining | CertState::Waiting => {
            spans.push(t.a("◆ being obtained"));
            if let Some(challenge) = &row.challenge {
                spans.push(t.d(format!(" through {challenge}")));
            }
        }
        CertState::Failed | CertState::Expired => {
            spans.push(t.e(row.problem.clone().unwrap_or_else(|| "✕ none yet".to_owned())));
        }
    }
    Line::from(spans)
}

fn local_name_rows(t: &Theme, checks: &[nodo_api::LocalNameCheck]) -> Vec<Item> {
    checks
        .iter()
        .flat_map(|check| {
            let (symbol, right) = match check.verdict {
                LocalNameVerdict::Resolves => (t.o("✓ "), t.o(format!("Resolves to {}", check.address))),
                LocalNameVerdict::NotPublished => (t.e("✕ "), t.e("Not published")),
                LocalNameVerdict::Blocked => (t.a("▲ "), t.a("Hidden by this network")),
                LocalNameVerdict::WrongAddress => (t.e("✕ "), t.e("Wrong address")),
            };
            let mut rows = vec![Item::Row(Line::from(vec![t.n("   "), symbol, t.n(check.host.clone())]), Line::from(right))];
            if check.verdict != LocalNameVerdict::Resolves {
                rows.push(Item::Text(Line::from(t.d(format!("     {}", check.explanation)))));
            }
            rows
        })
        .collect()
}

pub fn domain(app: &App, t: &Theme, state: &DomainState) -> Page {
    let web = &app.data.web;
    let acme = &web.acme;
    let pair = |label: &str, value: Span<'static>| Item::Row(Line::from(t.d(label.to_owned())), Line::from(value));
    let attention = web.certs.iter().filter(|c| matches!(c.state, CertState::Failed | CertState::Expired)).count();
    let right = match attention {
        0 if web.certs.iter().all(|c| c.state == CertState::Valid) => t.o("✓ Certificates in place"),
        0 => t.a("◆ Certificates on the way"),
        1 => t.e("✕ 1 certificate needs attention"),
        n => t.e(format!("✕ {n} certificates need attention")),
    };

    let method = if acme.dns01 { t.o(acme.challenge.clone()) } else { t.a("HTTP-01 or TLS-ALPN-01") };
    let mut domain_rows = vec![
        pair("Domain", t.n(web.domain.clone())),
        pair("Provider", t.n(if web.provider.is_empty() { "None".to_owned() } else { web.provider.clone() })),
        pair("Validation", method),
    ];
    domain_rows.push(Item::Text(Line::from(if acme.dns01 {
        t.d("No open ports needed.")
    } else {
        t.d(format!("Needs ports 80 and 443 forwarded. {}", acme.challenge))
    })));
    if let Some(server) = &acme.test_server {
        domain_rows.push(Item::Text(Line::from(t.a(format!("▲ Test ACME server: {server}")))));
    }

    let mut cert_rows: Vec<Item> = web
        .certs
        .iter()
        .flat_map(|row| {
            let mut rows = vec![Item::Row(Line::from(t.n(pad(&row.host, 30))), Line::from(cert_status(t, row)))];
            if let Some(problem) = row.problem.as_ref().filter(|_| row.state != CertState::Valid) {
                rows.push(Item::Text(Line::from(t.e(format!("  {problem}")))));
            }
            rows
        })
        .collect();
    if cert_rows.is_empty() {
        cert_rows.push(Item::Text(Line::from(t.d("No site is served yet."))));
    }

    let focus = |f: DomainFocus| state.focus == f;
    let field = |f: DomainFocus, label: &str, input: &crate::secret::Input, empty: &str| {
        let mut value = vec![if input.is_empty() { t.d(empty.to_owned()) } else { t.n(input.display()) }];
        if focus(f) {
            value.push(t.s(" "));
        }
        let left = Line::from(t.n(mark(focus(f), &format!(" {}", pad(label, 28)))));
        if focus(f) {
            Item::Selected(left, Line::from(value))
        } else {
            Item::Row(left, Line::from(value))
        }
    };
    let toggle = |f: DomainFocus, on: bool, label: &str, note: &str| {
        let left = Line::from(t.n(mark(focus(f), &format!(" {} {label}", checkbox(on)))));
        if focus(f) {
            Item::Selected(left, Line::from(note.to_owned()))
        } else {
            Item::Row(left, Line::from(t.d(note.to_owned())))
        }
    };
    let lets_encrypt = vec![
        field(DomainFocus::Email, "Email for expiry notices", &state.email, "none, recommended"),
        toggle(DomainFocus::Staging, state.staging, "Use the staging server", "Untrusted certificates, for testing"),
    ];

    let radio = |on: bool| if on { "(•)" } else { "( )" };
    let address = acme.local_address.clone().unwrap_or_else(|| "the local address".to_owned());
    let dot_local = Line::from(t.n(mark(focus(DomainFocus::DotLocal), &format!(" {} site.local, with this device's own CA", radio(!state.under_domain)))));
    let mut local_rows = vec![if focus(DomainFocus::DotLocal) {
        Item::Selected(dot_local, Line::from("Browsers warn until you trust it"))
    } else {
        Item::Row(dot_local, Line::from(t.d("Browsers warn until you trust it")))
    }];
    let mut under = vec![t.n(mark(focus(DomainFocus::UnderDomain), &format!(" {} Under your domain: site.", radio(state.under_domain))))];
    under.push(t.n(state.local_domain.display()));
    if focus(DomainFocus::UnderDomain) {
        under.push(t.s(" "));
    }
    let note = if acme.dns01 { "Real certificates through DNS-01" } else { "Needs DNS-01 from your provider" };
    local_rows.push(if focus(DomainFocus::UnderDomain) {
        Item::Selected(Line::from(under), Line::from(note))
    } else {
        Item::Row(Line::from(under), Line::from(t.d(note)))
    });
    if state.under_domain {
        local_rows.push(Item::Text(Line::from(vec![
            t.a("   ▲"),
            t.n(format!(" Each name gets a public DNS record with this device's LAN address, {address}.")),
        ])));
        local_rows.push(Item::Text(Line::from(vec![
            t.a("   ▲"),
            t.n(" Some routers hide private addresses in DNS answers. Press t to test this network."),
        ])));
    } else if let Some(fingerprint) = &acme.local_ca {
        local_rows.push(Item::Text(Line::from(vec![
            t.d(format!("   Local CA: {}…  ", &fingerprint[..fingerprint.len().min(47)])),
            t.a("c"),
            t.d(" trust it on your devices"),
        ])));
    }
    match &state.checks {
        Some(Ok(checks)) if checks.is_empty() => {
            local_rows.push(Item::Text(Line::from(t.d("   No local site uses a name under the domain yet."))));
        }
        Some(Ok(checks)) => local_rows.extend(local_name_rows(t, checks)),
        Some(Err(text)) => local_rows.push(Item::Text(Line::from(t.e(format!("   ✕ {text}"))))),
        None => {}
    }

    let mut page = Page::new(
        Line::from(vec![t.n("Web "), t.d("› "), t.b("Domains")]),
        Line::from(right),
    )
    .columns(vec![(42, vec![Panel::new("Domain", domain_rows)]), (58, vec![Panel::new("Certificates", cert_rows)])])
    .panel(Panel::new("Let's Encrypt", lets_encrypt))
    .panel(Panel::new("Local site names", local_rows).grow());
    if !acme.dns01_fields.is_empty() {
        let label = |i: usize| acme.dns01_fields.get(i).map_or_else(String::new, |f| f.replace('_', " "));
        let stored = if acme.dns01_stored { "stored, type to replace" } else { "needed for DNS-01" };
        page = page.panel(Panel::new(
            format!("{} certificate credentials", web.provider),
            vec![
                field(DomainFocus::Dns01First, &label(0), &state.dns01_first, stored),
                field(DomainFocus::Dns01Second, &label(1), &state.dns01_second, stored),
            ],
        ));
    }
    if let Some(error) = &state.error {
        page = page.plain(vec![Item::Text(Line::from(t.e(format!(" ✕ {error}"))))]);
    }
    let keys: &[(&str, &str)] = if state.focus.is_text() {
        &[("↑↓", "fields"), ("type", "to edit"), ("↵", "save"), ("Esc", "back")]
    } else {
        &[("↑↓", "fields"), ("Space", "choose"), ("↵", "save"), ("p", "provider"), ("t", "test local names"), ("c", "local CA"), ("Esc", "back")]
    };
    page.footer(Item::Text(hints(t, keys)))
}

/// The local CA: its full fingerprint, and the page where phones and computers on the LAN get it.
pub fn trust(app: &App, t: &Theme) -> Page {
    let web = &app.data.web;
    let crumbs = Line::from(vec![t.n("Web "), t.d("› "), t.n("Domains "), t.d("› "), t.b("Local CA")]);
    let right = match &web.trust_url {
        Some(url) => t.o(format!("✓ Served at {url}")),
        None => t.a("▲ Not served"),
    };
    let page_rows = match &web.trust_url {
        Some(url) => vec![
            Item::Row(Line::from(t.d("Open on a phone or computer here")), Line::from(t.b(url.clone()))),
            Item::Text(Line::from(t.n(
                "It offers this device's root certificate and short install steps for Android, iPhone,",
            ))),
            Item::Text(Line::from(t.n("macOS, Windows, Linux and Firefox. It needs no port and no app."))),
            Item::Text(Line::from(t.d(
                "Only the local network can open it, on the local address. It is never public or on Tor.",
            ))),
        ],
        None => vec![Item::Text(Line::from(t.n(
            "The page is served while a local site uses a .local name and the local address is up.",
        )))],
    };
    let fingerprint_rows = match &web.acme.local_ca {
        Some(fingerprint) => {
            let mut rows: Vec<Item> = fingerprint_lines(fingerprint).into_iter().map(|l| Item::Text(Line::from(t.b(l)))).collect();
            rows.push(Item::Blank);
            rows.push(Item::Text(Line::from(vec![
                t.a("▲ "),
                t.n("The page is plain HTTP. Install the root only if the page shows this same fingerprint."),
            ])));
            rows
        }
        None => vec![Item::Text(Line::from(t.d("Caddy has not created the local CA yet.")))],
    };
    let after = vec![
        Item::Text(Line::from(t.n("Browsers on that device then open this device's .local sites without a warning."))),
        Item::Text(Line::from(t.d("The root vouches only for this device's local sites. Remove it from a device to"))),
        Item::Text(Line::from(t.d("stop trusting them. Names under your own domain get public certificates instead."))),
    ];
    Page::new(crumbs, Line::from(right))
        .panel(Panel::new("Trust page", page_rows))
        .panel(Panel::new("SHA-256 fingerprint", fingerprint_rows))
        .panel(Panel::new("What trusting it does", after).grow())
        .footer(Item::Text(hints(t, &[("r", "refresh"), ("Esc", "back")])))
}

/// The fingerprint in two lines of 16 bytes, as the trust page shows it.
fn fingerprint_lines(fingerprint: &str) -> Vec<String> {
    let parts: Vec<&str> = fingerprint.split(':').collect();
    parts.chunks(16).map(|chunk| chunk.join(":")).collect()
}

pub fn certificate(app: &App, t: &Theme, state: &CertificateState) -> Page {
    let web = &app.data.web;
    let acme = &web.acme;
    let row = web.cert(&state.host);
    let crumbs = Line::from(vec![t.n("Web "), t.d("› "), t.n(format!("{} ", state.site)), t.d("› "), t.b("Certificate")]);
    let cert_state = row.map(|r| r.state).unwrap_or(CertState::Waiting);
    let right = match cert_state {
        CertState::Valid => t.o("✓ Certificate in place"),
        CertState::Failed | CertState::Expired => t.e("✕ No certificate yet"),
        CertState::Obtaining | CertState::Waiting => t.a("◆ Getting a certificate"),
    };
    // The provider's name says more than the bare challenge type when DNS-01 is in use.
    let method = if acme.dns01 {
        acme.challenge.clone()
    } else {
        row.and_then(|r| r.challenge.clone()).unwrap_or_else(|| "HTTP-01 or TLS-ALPN-01".to_owned())
    };
    let step = |done: bool, failed: bool, text: String, note: String| {
        let symbol = if failed {
            t.e("✕ ")
        } else if done {
            t.o("✓ ")
        } else {
            t.d("○ ")
        };
        Item::Row(Line::from(vec![symbol, t.n(text)]), Line::from(t.d(note)))
    };
    let valid = cert_state == CertState::Valid;
    let failed = matches!(cert_state, CertState::Failed | CertState::Expired);
    let ca = if acme.test_server.is_some() {
        "the test ACME server"
    } else if acme.staging {
        "Let's Encrypt staging"
    } else {
        "Let's Encrypt"
    };
    let mut steps = vec![
        step(true, false, format!("{} is configured on this device", state.host), String::new()),
        if valid || failed {
            step(true, failed, format!("Asked {ca} to prove control of the name"), method.clone())
        } else {
            Item::Row(Line::from(vec![t.a("◆ "), t.n(format!("Asking {ca} to prove control of the name"))]), Line::from(t.d(method.clone())))
        },
        step(
            valid,
            false,
            "Certificate in place".to_owned(),
            row.filter(|_| valid)
                .map(|r| format!("{}, {} days left", r.issuer, r.days_left.unwrap_or(0)))
                .unwrap_or_default(),
        ),
    ];
    if acme.staging && acme.test_server.is_none() {
        steps.push(Item::Blank);
        steps.push(Item::Text(Line::from(t.a("▲ Staging certificates are not trusted by browsers."))));
    }
    let mut page = Page::new(crumbs, Line::from(right)).panel(Panel::new(state.host.clone(), steps));
    let explain: Vec<Item> = match (cert_state, row) {
        (CertState::Failed | CertState::Expired, Some(row)) => {
            let mut items = vec![Item::Text(Line::from(t.e(row.problem.clone().unwrap_or_else(|| "Getting the certificate failed.".to_owned()))))];
            if let Some(detail) = &row.problem_detail {
                items.push(Item::Blank);
                items.push(Item::Text(Line::from(t.d(format!("The CA said: {detail}")))));
            }
            items.push(Item::Blank);
            items.push(Item::Text(Line::from(t.n("Caddy tries again on its own. This screen updates when it does."))));
            items
        }
        (CertState::Valid, _) => vec![Item::Text(Line::from(t.n(format!(
            "https://{} now has a trusted certificate. It renews itself before it runs out.",
            state.host
        ))))],
        _ => vec![
            Item::Text(Line::from(t.n(format!("{ca} checks that you control {}. It usually takes under a minute.", state.host)))),
            Item::Text(Line::from(t.d("Browsers show an error for the address until the certificate is in place."))),
        ],
    };
    page = page.panel(
        if failed { Panel::error("What went wrong", explain) } else { Panel::new("What happens now", explain) }.grow(),
    );
    page.footer(Item::Text(hints(t, &[("r", "refresh"), ("l", "proxy log"), ("d", "domains and certificates"), ("Esc", "back")])))
}
