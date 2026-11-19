//! Domains and certificates, and following a certificate while Caddy gets it.

use nodo_api::{CertState, LocalNameVerdict};
use ratatui::text::{Line, Span};

use super::layout::{Item, Page, Panel};
use super::{checkbox, hints, mark, pad};
use crate::app::*;
use nodo_ui_core::flows::web::certs::Trust;
use nodo_ui_core::flows::web::domain::{self, Field};
use nodo_ui_core::model::*;
use nodo_ui_core::words::Tone;
use crate::theme::Theme;

/// One certificate in a few words, coloured by how it stands.
pub fn cert_status(t: &Theme, row: &CertRow) -> Span<'static> {
    match nodo_ui_core::words::cert_status(row) {
        (Tone::Good, words) => t.o(format!("✓ {words}")),
        (Tone::Working, words) => t.a(format!("◆ {words}")),
        (Tone::Bad, words) => t.e(format!("✕ {words}")),
    }
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

    let validation = domain::validation(acme);
    let method = if validation.dns01 { t.o(validation.method.clone()) } else { t.a(validation.method.clone()) };
    let mut domain_rows = vec![
        pair("Domain", t.n(web.domain.clone())),
        pair("Provider", t.n(if web.provider.is_empty() { "None".to_owned() } else { web.provider.clone() })),
        pair("Validation", method),
    ];
    domain_rows.push(Item::Text(Line::from(t.d(validation.note))));
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

    let focus = |f: Field| state.focus == f;
    let field = |f: Field, label: &str, input: &nodo_ui_core::secret::Input, empty: &str| {
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
    let toggle = |f: Field, on: bool, label: &str, note: &str| {
        let left = Line::from(t.n(mark(focus(f), &format!(" {} {label}", checkbox(on)))));
        if focus(f) {
            Item::Selected(left, Line::from(note.to_owned()))
        } else {
            Item::Row(left, Line::from(t.d(note.to_owned())))
        }
    };
    // A refused save is said just above the fields that were saved, where it is seen: at the foot
    // of the page it was below the fold on a 32-row terminal, and a refused email looked taken.
    let mut lets_encrypt: Vec<Item> = state.error.iter().map(|error| Item::Text(Line::from(t.e(format!(" ✕ {error}"))))).collect();
    lets_encrypt.push(field(Field::Email, "Email for expiry notices", &state.email, "none, recommended"));
    lets_encrypt.push(toggle(Field::Staging, state.staging, "Use the staging server", "Untrusted certificates, for testing"));

    let radio = |on: bool| if on { "(•)" } else { "( )" };
    let address = acme.local_address.clone().unwrap_or_else(|| "the local address".to_owned());
    let dot_local = Line::from(t.n(mark(focus(Field::DotLocal), &format!(" {} site.local, with this device's own CA", radio(!state.under_domain)))));
    let mut local_rows = vec![if focus(Field::DotLocal) {
        Item::Selected(dot_local, Line::from("Browsers warn until you trust it"))
    } else {
        Item::Row(dot_local, Line::from(t.d("Browsers warn until you trust it")))
    }];
    let mut under = vec![t.n(mark(focus(Field::UnderDomain), &format!(" {} Under your domain: site.", radio(state.under_domain))))];
    under.push(t.n(state.local_domain.display()));
    if focus(Field::UnderDomain) {
        under.push(t.s(" "));
    }
    let note = if acme.dns01 { "Real certificates through DNS-01" } else { "Needs DNS-01 from your provider" };
    local_rows.push(if focus(Field::UnderDomain) {
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
        let labels = domain::dns01_labels(acme);
        let label = |i: usize| labels.get(i).cloned().unwrap_or_default();
        let stored = if acme.dns01_stored { "stored, type to replace" } else { "needed for DNS-01" };
        page = page.panel(Panel::new(
            format!("{} certificate credentials", web.provider),
            vec![
                field(Field::Dns01First, &label(0), &state.dns01_first, stored),
                field(Field::Dns01Second, &label(1), &state.dns01_second, stored),
            ],
        ));
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
    let trust = Trust::of(&app.data);
    let fingerprint_rows = match &trust.fingerprint {
        Some(_) => {
            let mut rows: Vec<Item> = trust.fingerprint_lines().into_iter().map(|l| Item::Text(Line::from(t.b(l)))).collect();
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
