//! Web flow proposals (v2, superseded, and v3), mock only.
//!
//! These screens are drawn by the real layout engine and theme, so the boards made from them
//! (`examples/boards.rs`) show exactly what a terminal shows. Nothing here talks to the helper:
//! every value is sample data. `nodo-tui --mock --screen web3-sites` opens one in a terminal.
//!
//! v3 follows the owner's structure: domain providers are added once (Web › Domain providers);
//! the Web tab lists only the sites that are online, and a click opens that app's Hosting, where
//! home network, Tor and internet are ticked and, for the internet, a provider's domain is picked.
//! It is designed for the mouse as crossterm reports it (left click, drag, scroll wheel; no
//! hover): every action is a whole row or a block three rows tall, and the action bar is pinned
//! to the bottom so the main button stays on screen in a small terminal. Building it means
//! turning on mouse capture in nodo-tui and hit-testing the rectangles each screen draws.

use std::io;

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Terminal;

use crate::app::{App, ScreenId, Tab};
use crate::fixtures::fixture;
use crate::theme::Theme;
use crate::view::layout::{draw_page, Item, Page, Panel};
use crate::view::{self, button_line, checkbox, hints, mark, pad, tab_bar};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    /// Today's Web overview, for comparison.
    V1Overview,
    Overview,
    App,
    Who,
    HomeDone,
    HomeDetails,
    TorDone,
    NetAddress,
    NetProvider,
    NetChecks,
    NetConfirm,
    FailProvider,
    FailKey,
    FailIsp,
    FailCertificate,
    Advanced,
    // Web flow v3: the owner's structure, mouse first.
    V3Web,
    V3Hosting,
    V3HostingInternet,
    V3PickDomain,
    V3NoProvider,
    V3Confirm,
    V3Online,
    V3Blocked,
    V3Providers,
    V3AddProvider,
    V3ProviderKey,
    V3ProviderDone,
}

impl Scene {
    pub const ALL: [Scene; 28] = [
        Scene::V1Overview,
        Scene::Overview,
        Scene::App,
        Scene::Who,
        Scene::HomeDone,
        Scene::HomeDetails,
        Scene::TorDone,
        Scene::NetAddress,
        Scene::NetProvider,
        Scene::NetChecks,
        Scene::NetConfirm,
        Scene::FailProvider,
        Scene::FailKey,
        Scene::FailIsp,
        Scene::FailCertificate,
        Scene::Advanced,
        Scene::V3Web,
        Scene::V3Hosting,
        Scene::V3HostingInternet,
        Scene::V3PickDomain,
        Scene::V3NoProvider,
        Scene::V3Confirm,
        Scene::V3Online,
        Scene::V3Blocked,
        Scene::V3Providers,
        Scene::V3AddProvider,
        Scene::V3ProviderKey,
        Scene::V3ProviderDone,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Scene::V1Overview => "v1-overview",
            Scene::Overview => "web2-overview",
            Scene::App => "web2-app",
            Scene::Who => "web2-who",
            Scene::HomeDone => "web2-home",
            Scene::HomeDetails => "web2-home-details",
            Scene::TorDone => "web2-tor",
            Scene::NetAddress => "web2-net-address",
            Scene::NetProvider => "web2-net-provider",
            Scene::NetChecks => "web2-net-checks",
            Scene::NetConfirm => "web2-net-confirm",
            Scene::FailProvider => "web2-fail-provider",
            Scene::FailKey => "web2-fail-key",
            Scene::FailIsp => "web2-fail-isp",
            Scene::FailCertificate => "web2-fail-certificate",
            Scene::Advanced => "web2-advanced",
            Scene::V3Web => "web3-sites",
            Scene::V3Hosting => "web3-hosting",
            Scene::V3HostingInternet => "web3-hosting-internet",
            Scene::V3PickDomain => "web3-pick-domain",
            Scene::V3NoProvider => "web3-no-provider",
            Scene::V3Confirm => "web3-confirm",
            Scene::V3Online => "web3-online",
            Scene::V3Blocked => "web3-blocked",
            Scene::V3Providers => "web3-providers",
            Scene::V3AddProvider => "web3-add-provider",
            Scene::V3ProviderKey => "web3-provider-key",
            Scene::V3ProviderDone => "web3-provider-done",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Scene> {
        Self::ALL.into_iter().find(|scene| scene.slug() == slug)
    }

    /// The board's title on the canvas.
    pub fn title(self) -> &'static str {
        match self {
            Scene::V1Overview => "Today (v1): Web overview, for comparison",
            Scene::Overview => "1 · Web: your apps, in words",
            Scene::App => "2 · One app: where it can be opened",
            Scene::Who => "3 · Who should be able to open it?",
            Scene::HomeDone => "4a · Home: on, plus a one-time phone step",
            Scene::HomeDetails => "4b · Home, with d Details open",
            Scene::TorDone => "5 · Private link through Tor",
            Scene::NetAddress => "6 · Internet, step 1: your web address",
            Scene::NetProvider => "7 · Internet, step 2: connect your provider",
            Scene::NetChecks => "8 · Internet, step 3: checks as plain progress",
            Scene::NetConfirm => "9 · Internet, step 4: confirm in plain words",
            Scene::FailProvider => "Problem · provider can't be set up automatically",
            Scene::FailKey => "Problem · the API key was refused",
            Scene::FailIsp => "Problem · your internet provider blocks it",
            Scene::FailCertificate => "Problem · secure connection still pending",
            Scene::Advanced => "10 · Advanced: addresses and certificates",
            Scene::V3Web => "1 · Web: only the sites that are online",
            Scene::V3Hosting => "2 · mk-lws: Hosting, home network on",
            Scene::V3HostingInternet => "3 · mk-pay: Internet ticked, provider and domain chosen",
            Scene::V3PickDomain => "4 · Choosing a domain from added providers",
            Scene::V3NoProvider => "4b · Internet ticked, no provider yet",
            Scene::V3Confirm => "5 · Checks done, one Put online button",
            Scene::V3Online => "6 · Online",
            Scene::V3Blocked => "Problem · the internet can't reach the Nodo",
            Scene::V3Providers => "7 · Domain providers (from the Web tab)",
            Scene::V3AddProvider => "8 · Add a provider: pick the company",
            Scene::V3ProviderKey => "9 · Add a provider: paste the key",
            Scene::V3ProviderDone => "10 · Added: the domains this key controls",
        }
    }

    /// Version 3 screens, for grouping on the canvas.
    pub fn is_v3(self) -> bool {
        self.slug().starts_with("web3")
    }

    /// What a click does on this screen, printed on the board.
    pub fn clickable(self) -> &'static str {
        match self {
            Scene::V3Web => "Clickable: each site row (opens the app's Hosting), the tabs, \"Domain providers\". Scroll wheel scrolls the list.",
            Scene::V3Hosting => "Clickable: each choice row toggles its box, \"Show details\", the Save button, the tabs and breadcrumb.",
            Scene::V3HostingInternet => "Clickable: choice rows, the Provider and Domain boxes (open a list), the address box (focus to type), Check and save.",
            Scene::V3PickDomain => "Clickable: each domain row picks it; \"Add a provider\" opens that flow and comes back; clicking outside closes the list.",
            Scene::V3NoProvider => "Clickable: the choice rows and the Add a provider button (comes back here when done).",
            Scene::V3Confirm => "Clickable: Put mk-pay online (the only primary button), Cancel, \"Show details\".",
            Scene::V3Online => "Clickable: the address (shown to copy), Done, \"Show details\".",
            Scene::V3Blocked => "Clickable: Use Tor instead (primary), Check again.",
            Scene::V3Providers => "Clickable: Add a provider (primary), Remove on each row.",
            Scene::V3AddProvider => "Clickable: each company row. Scroll wheel scrolls the list.",
            Scene::V3ProviderKey => "Clickable: the two key boxes (focus to type or paste), Connect.",
            Scene::V3ProviderDone => "Clickable: Done (returns to the app that asked, or to Domain providers).",
            _ => "",
        }
    }
}

/// Draws a scene into a buffer of the given terminal size.
pub fn render(scene: Scene, width: u16, height: u16, theme: &Theme) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    let (app, _) = fixture(ScreenId::WebSites);
    terminal
        .draw(|frame| {
            if scene == Scene::V1Overview {
                view::render(frame, &app, theme);
            } else {
                let area = frame.area();
                frame.buffer_mut().set_style(area, theme.base());
                draw_page(frame, area, theme, page(&app, theme, scene));
            }
        })
        .expect("draw");
    terminal.backend().buffer().clone()
}

/// Shows a scene in this terminal until a key is pressed.
pub fn show(scene: Scene, theme: Theme) -> io::Result<()> {
    use crossterm::event::{self, Event, KeyEventKind};
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };

    let (app, _) = fixture(ScreenId::WebSites);
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
    let result = (|| -> io::Result<()> {
        loop {
            terminal.draw(|frame| {
                if scene == Scene::V1Overview {
                    view::render(frame, &app, &theme);
                } else {
                    let area = frame.area();
                    frame.buffer_mut().set_style(area, theme.base());
                    draw_page(frame, area, &theme, page(&app, &theme, scene));
                }
            })?;
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    return Ok(());
                }
            }
        }
    })();
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    result
}

fn crumbs(t: &Theme, parts: &[&str]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            spans.push(t.d(" › "));
        }
        if index + 1 == parts.len() {
            spans.push(t.b((*part).to_owned()));
        } else {
            spans.push(t.n((*part).to_owned()));
        }
    }
    Line::from(spans)
}

/// "Step 2 of 4   ✓ Address  ›  Provider  ›  Checks  ›  Confirm"
fn steps(t: &Theme, current: usize) -> Line<'static> {
    let names = ["Address", "Provider", "Checks", "Confirm"];
    let mut spans = vec![t.d(format!("Step {} of 4   ", current + 1))];
    for (index, name) in names.iter().enumerate() {
        if index > 0 {
            spans.push(t.d("  ›  "));
        }
        spans.push(match index.cmp(&current) {
            std::cmp::Ordering::Less => t.o(format!("✓ {name}")),
            std::cmp::Ordering::Equal => t.a((*name).to_owned()),
            std::cmp::Ordering::Greater => t.d((*name).to_owned()),
        });
    }
    Line::from(spans)
}

fn text(t: &Theme, s: &str) -> Item {
    Item::Text(Line::from(t.n(s.to_owned())))
}

fn dim(t: &Theme, s: &str) -> Item {
    Item::Text(Line::from(t.d(s.to_owned())))
}

/// Label and value side by side, for details lists.
fn kv(t: &Theme, label: &str, value: &str) -> Item {
    Item::Text(Line::from(vec![t.d(pad(label, 16)), t.d(value.to_owned())]))
}

fn pair(t: &Theme, label: &str, value: Span<'static>) -> Item {
    Item::Row(Line::from(t.d(label.to_owned())), Line::from(value))
}

/// A text field: label, then the value in the selected style when focused.
fn field(t: &Theme, label: &str, value: &str, focused: bool) -> Item {
    let shown = format!(" {} ", pad(value, 40));
    Item::Text(Line::from(vec![
        t.d(pad(label, 14)),
        if focused {
            t.s(shown)
        } else {
            t.n(format!("[{}]", &shown[1..shown.len() - 1]))
        },
    ]))
}

fn header(app: &App, t: &Theme) -> Page {
    Page::new(tab_bar(app, t, Tab::Web), Line::from(t.d("monokulo-nodo")))
}

pub fn page(app: &App, t: &Theme, scene: Scene) -> Page {
    match scene {
        Scene::V1Overview => Page::new(Line::default(), Line::default()),
        Scene::Overview => overview(app, t),
        Scene::App => app_page(t),
        Scene::Who => who(t),
        Scene::HomeDone => home(t, false),
        Scene::HomeDetails => home(t, true),
        Scene::TorDone => tor(t),
        Scene::NetAddress => net_address(t),
        Scene::NetProvider => net_provider(t),
        Scene::NetChecks => net_checks(t),
        Scene::NetConfirm => net_confirm(t),
        Scene::FailProvider => fail_provider(t),
        Scene::FailKey => fail_key(t),
        Scene::FailIsp => fail_isp(t),
        Scene::FailCertificate => fail_certificate(t),
        Scene::Advanced => advanced(t),
        Scene::V3Web => v3_web(app, t),
        Scene::V3Hosting => v3_hosting(t, false),
        Scene::V3HostingInternet => v3_hosting(t, true),
        Scene::V3PickDomain => v3_pick_domain(t),
        Scene::V3NoProvider => v3_no_provider(t),
        Scene::V3Confirm => v3_confirm(t),
        Scene::V3Online => v3_online(t),
        Scene::V3Blocked => v3_blocked(t),
        Scene::V3Providers => v3_providers(app, t),
        Scene::V3AddProvider => v3_add_provider(t),
        Scene::V3ProviderKey => v3_provider_key(t),
        Scene::V3ProviderDone => v3_provider_done(t),
    }
}

fn overview(app: &App, t: &Theme) -> Page {
    let apps: [(&str, &str, &str, Span<'static>); 4] = [
        (
            "mk-pay",
            "Home and internet",
            "https://pay.example.com",
            t.o("✓ Working"),
        ),
        (
            "mk-lws",
            "Home only",
            "https://mk-lws.local",
            t.o("✓ Working"),
        ),
        (
            "kringle",
            "Private link (Tor)",
            "http://kringl…7qd.onion",
            t.o("✓ Working"),
        ),
        ("mk-bell", "Not online", "", t.d("Off")),
    ];
    let mut rows = vec![Item::Row(
        Line::from(t.d(format!(
            " {}{}{}",
            pad("App", 13),
            pad("Who can open it", 22),
            "Address"
        ))),
        Line::from(t.d("Status")),
    )];
    for (index, (name, who, address, status)) in apps.into_iter().enumerate() {
        let selected = index == 0;
        let left = Line::from(vec![
            t.n(mark(selected, &format!(" {}", pad(name, 12)))),
            if who == "Not online" {
                t.d(pad(who, 22))
            } else {
                t.n(pad(who, 22))
            },
            t.n(address.to_owned()),
        ]);
        rows.push(if selected {
            Item::Selected(left, Line::from(status))
        } else {
            Item::Row(left, Line::from(status))
        });
    }
    rows.push(Item::Blank);
    rows.push(dim(
        t,
        " Choose an app and press ↵ to change who can open it.",
    ));
    header(app, t)
        .panel(Panel::new("Your apps on the web", rows).grow())
        .panel(Panel::new(
            "Your web address",
            vec![
                pair(t, "example.com, managed at Porkbun", t.o("✓ Set up")),
                pair(t, "Secure connections", t.o("✓ Up to date")),
            ],
        ))
        .footer(Item::Text(hints(
            t,
            &[
                ("↵", "who can open it"),
                ("a", "advanced"),
                ("l", "web log"),
            ],
        )))
}

fn app_page(t: &Theme) -> Page {
    let rows = vec![
        Item::Row(
            Line::from(vec![t.o("✓ "), t.n("Devices at home")]),
            Line::from(t.n("https://mk-pay.local")),
        ),
        Item::Row(
            Line::from(vec![t.o("✓ "), t.n("Anyone on the internet")]),
            Line::from(t.n("https://pay.example.com")),
        ),
        Item::Row(Line::from(vec![t.d("○ "), t.d("Private link through Tor")]), Line::from(t.d("off"))),
        Item::Blank,
        text(t, "Visitors from the internet can see your home's internet address. mk-pay has its own login."),
    ];
    Page::new(crumbs(t, &["Web", "mk-pay"]), Line::from(t.o("✓ Working")))
        .panel(Panel::new("Where mk-pay can be opened", rows).grow())
        .panel(Panel::new(
            "Recent visits",
            vec![
                pair(t, "Today", t.n("38 from home, 112 from the internet")),
                pair(t, "Last problem", t.d("none")),
            ],
        ))
        .footer(Item::Text(hints(
            t,
            &[
                ("↵", "change who can open it"),
                ("x", "take offline"),
                ("l", "log"),
                ("Esc", "back"),
            ],
        )))
}

fn who(t: &Theme) -> Page {
    let option = |selected: bool, on: bool, title: &str, lines: &[&str]| -> Vec<Item> {
        let label = format!(" {} {title}", checkbox(on));
        let mut items = vec![if selected {
            Item::Selected(Line::from(t.n(mark(true, &label))), Line::default())
        } else {
            Item::Text(Line::from(vec![
                t.n(format!(" {} ", checkbox(on))),
                t.b(title.to_owned()),
            ]))
        }];
        for line in lines {
            items.push(dim(t, &format!("     {line}")));
        }
        items.push(Item::Blank);
        items
    };
    let mut rows = Vec::new();
    rows.extend(option(
        false,
        true,
        "Devices at home",
        &[
            "Phones and computers on your Wi-Fi or network cable.",
            "Address: https://mk-pay.local",
        ],
    ));
    rows.extend(option(
        false,
        false,
        "Anyone with a private link, through Tor",
        &["Opens only in Tor Browser. Doesn't reveal where the Nodo is."],
    ));
    rows.extend(option(
        true,
        true,
        "Anyone on the internet, at my own web address",
        &[
            "Needs a web address you own, like pay.example.com.",
            "Visitors can see your home's internet address.",
        ],
    ));
    rows.push(dim(t, " Pick any combination."));
    Page::new(
        crumbs(t, &["Web", "mk-pay", "Who can open it"]),
        Line::from(t.a("● 1 change")),
    )
    .panel(Panel::new("Who should be able to open mk-pay?", rows).grow())
    .footer(Item::Text(button_line(t, "Cancel", "Continue", true)))
    .footer(Item::Text(hints(
        t,
        &[
            ("Space", "tick"),
            ("↑↓", "move"),
            ("↵", "continue"),
            ("Esc", "cancel"),
        ],
    )))
}

fn home(t: &Theme, details: bool) -> Page {
    let mut rows = vec![
        Item::Row(Line::from(vec![t.o("✓ "), t.b("mk-pay is open to devices at home")]), Line::default()),
        Item::Blank,
        pair(t, "Address", t.n("https://mk-pay.local")),
        Item::Blank,
        text(t, "One time for each phone or computer: open http://nodo.local and follow \"Trust this Nodo\"."),
        text(t, "After that the browser shows a padlock instead of a warning."),
    ];
    if details {
        rows.push(Item::Blank);
        rows.push(dim(t, "Details"));
        rows.push(kv(
            t,
            "  Name",
            "mk-pay.local, announced by mDNS on 192.168.1.155",
        ));
        rows.push(kv(
            t,
            "  Certificate",
            "from this Nodo's own certificate authority, 7 days, renewed",
        ));
        rows.push(kv(
            t,
            "  Trust page",
            "http://nodo.local serves the root certificate, local network only",
        ));
        rows.push(kv(
            t,
            "  Port",
            "443 on the local network only; closed to the internet",
        ));
    }
    let hint = if details { "hide details" } else { "details" };
    Page::new(
        crumbs(t, &["Web", "mk-pay", "Devices at home"]),
        Line::from(t.o("✓ On")),
    )
    .panel(Panel::new("Devices at home", rows).grow())
    .footer(Item::Text(hints(
        t,
        &[("d", hint), ("↵", "done"), ("Esc", "back")],
    )))
}

fn tor(t: &Theme) -> Page {
    let rows = vec![
        Item::Row(Line::from(vec![t.o("✓ "), t.b("kringle has a private link")]), Line::default()),
        Item::Blank,
        text(t, "Link"),
        Item::Text(Line::from(t.a("  http://kringlqx3v7mgk2hbdu5w6zyntc4lrq3oafj2wxhs7e4ylpd7qd.onion"))),
        Item::Blank,
        text(t, "Share it with the people who should use kringle. It opens only in Tor Browser."),
        text(t, "It doesn't reveal your home's internet address, and works even if your provider blocks"),
        text(t, "visitors from the internet."),
    ];
    Page::new(
        crumbs(t, &["Web", "kringle", "Private link"]),
        Line::from(t.o("✓ On")),
    )
    .panel(Panel::new("Private link through Tor", rows).grow())
    .footer(Item::Text(hints(
        t,
        &[
            ("n", "new link"),
            ("d", "details"),
            ("↵", "done"),
            ("Esc", "back"),
        ],
    )))
}

fn net_address(t: &Theme) -> Page {
    let rows = vec![
        Item::Text(steps(t, 0)),
        Item::Blank,
        text(t, "Which web address should mk-pay use?"),
        Item::Blank,
        field(t, "Web address", "pay.example.com", true),
        Item::Blank,
        dim(
            t,
            "Use a domain you own, from a shop such as Porkbun, Namecheap or Cloudflare.",
        ),
        dim(
            t,
            "A part in front (pay.) lets other apps use the same domain.",
        ),
    ];
    Page::new(
        crumbs(t, &["Web", "mk-pay", "On the internet"]),
        Line::default(),
    )
    .panel(Panel::new("Anyone on the internet", rows).grow())
    .footer(Item::Text(button_line(t, "Back", "Next", true)))
    .footer(Item::Text(hints(t, &[("↵", "next"), ("Esc", "back")])))
}

fn net_provider(t: &Theme) -> Page {
    let rows = vec![
        Item::Text(steps(t, 1)),
        Item::Blank,
        Item::Row(
            Line::from(vec![t.o("✓ "), t.n("example.com is managed at "), t.b("Porkbun")]),
            Line::default(),
        ),
        Item::Blank,
        text(t, "So the Nodo can keep the address pointing home by itself, give it a Porkbun API key."),
        dim(t, "Create one at porkbun.com › Account › API Access, and turn on API access for example.com."),
        Item::Blank,
        field(t, "API key", "pk1_••••••••••••••••••••", false),
        field(t, "Secret key", "sk1_••••••••", true),
        Item::Blank,
        dim(t, "The keys stay on this Nodo, sealed with the device passphrase."),
    ];
    Page::new(
        crumbs(t, &["Web", "mk-pay", "On the internet"]),
        Line::default(),
    )
    .panel(Panel::new("Anyone on the internet", rows).grow())
    .footer(Item::Text(button_line(t, "Back", "Check", true)))
    .footer(Item::Text(hints(
        t,
        &[("Tab", "next field"), ("↵", "check"), ("Esc", "back")],
    )))
}

fn net_checks(t: &Theme) -> Page {
    let rows = vec![
        Item::Text(steps(t, 2)),
        Item::Blank,
        Item::Row(
            Line::from(vec![t.o("✓ "), t.n("Porkbun accepted the key")]),
            Line::default(),
        ),
        Item::Row(
            Line::from(vec![t.o("✓ "), t.n("Found your home's internet address")]),
            Line::from(t.d("203.0.113.42")),
        ),
        Item::Row(
            Line::from(vec![t.o("✓ "), t.n("pay.example.com points to your home")]),
            Line::default(),
        ),
        Item::Row(
            Line::from(vec![t.a("◆ "), t.n("The internet can reach your Nodo")]),
            Line::from(t.a("checking…")),
        ),
        Item::Row(
            Line::from(vec![t.d("○ "), t.d("Secure connection")]),
            Line::default(),
        ),
        Item::Blank,
        dim(t, "This takes about a minute. Nothing is public yet."),
    ];
    Page::new(
        crumbs(t, &["Web", "mk-pay", "On the internet"]),
        Line::from(t.a("◆ Checking")),
    )
    .panel(Panel::new("Anyone on the internet", rows).grow())
    .footer(Item::Text(hints(t, &[("d", "details"), ("Esc", "stop")])))
}

fn net_confirm(t: &Theme) -> Page {
    let rows = vec![
        Item::Text(steps(t, 3)),
        Item::Blank,
        Item::Row(
            Line::from(vec![t.o("✓ "), t.n("Everything checked out")]),
            Line::default(),
        ),
        Item::Blank,
        Item::Text(Line::from(vec![
            t.n("mk-pay will open at "),
            t.b("https://pay.example.com"),
            t.n(" for anyone."),
        ])),
        Item::Blank,
        text(t, "What changes"),
        dim(
            t,
            "  · The Nodo answers visitors from the internet, for mk-pay only.",
        ),
        dim(t, "  · Visitors can see your home's internet address."),
        dim(
            t,
            "  · The Nodo keeps the address and the secure connection up to date itself.",
        ),
        Item::Blank,
        text(
            t,
            " [ ] Ask for a password before showing mk-pay   (mk-pay already has its own login)",
        ),
        text(t, " [x] Slow down anyone making too many requests"),
    ];
    Page::new(
        crumbs(t, &["Web", "mk-pay", "On the internet"]),
        Line::default(),
    )
    .panel(Panel::accent("Put mk-pay on the internet?", rows).grow())
    .footer(Item::Text(button_line(
        t,
        "Cancel",
        "Put mk-pay online",
        false,
    )))
    .footer(Item::Text(hints(
        t,
        &[
            ("Space", "tick"),
            ("←→", "choose"),
            ("d", "details"),
            ("↵", "confirm"),
        ],
    )))
}

fn fail_provider(t: &Theme) -> Page {
    let rows = vec![
        Item::Text(steps(t, 1)),
        Item::Blank,
        Item::Row(
            Line::from(vec![t.e("✕ "), t.n("example.com is managed at "), t.b("Hover")]),
            Line::default(),
        ),
        Item::Blank,
        text(t, "Hover has no way for the Nodo to update your address automatically, so it can't be set up."),
        Item::Blank,
        text(t, "You can:"),
        Item::Selected(Line::from(t.n(mark(true, "  Move example.com's settings to a supported provider (free at Cloudflare)"))), Line::default()),
        text(t, "  Use a private link through Tor instead"),
        text(t, "  Choose another web address"),
    ];
    Page::new(
        crumbs(t, &["Web", "mk-pay", "On the internet"]),
        Line::from(t.e("✕ Can't continue")),
    )
    .panel(Panel::error("Your provider isn't supported", rows).grow())
    .footer(Item::Text(hints(
        t,
        &[
            ("↑↓", "choose"),
            ("↵", "go"),
            ("p", "supported providers"),
            ("Esc", "back"),
        ],
    )))
}

fn fail_key(t: &Theme) -> Page {
    let rows = vec![
        Item::Text(steps(t, 1)),
        Item::Blank,
        Item::Row(
            Line::from(vec![t.e("✕ "), t.n("Porkbun didn't accept the key")]),
            Line::default(),
        ),
        Item::Blank,
        text(t, "Check that:"),
        dim(
            t,
            "  · both keys were copied in full (they start with pk1_ and sk1_)",
        ),
        dim(
            t,
            "  · API access is turned on for example.com in Porkbun's domain list",
        ),
        Item::Blank,
        field(t, "API key", "pk1_••••••••••••••••••••", false),
        field(t, "Secret key", "", true),
    ];
    Page::new(
        crumbs(t, &["Web", "mk-pay", "On the internet"]),
        Line::from(t.e("✕ Key refused")),
    )
    .panel(Panel::error("Anyone on the internet", rows).grow())
    .footer(Item::Text(button_line(t, "Back", "Try again", true)))
    .footer(Item::Text(hints(
        t,
        &[
            ("Tab", "next field"),
            ("↵", "try again"),
            ("d", "details"),
            ("Esc", "back"),
        ],
    )))
}

fn fail_isp(t: &Theme) -> Page {
    let rows = vec![
        Item::Text(steps(t, 2)),
        Item::Blank,
        Item::Row(Line::from(vec![t.o("✓ "), t.n("Porkbun accepted the key")]), Line::default()),
        Item::Row(Line::from(vec![t.e("✕ "), t.n("The internet can't reach your Nodo")]), Line::default()),
        Item::Blank,
        text(t, "Your internet provider shares one address between many homes, so visitors from the"),
        text(t, "internet stop at the provider and never reach your router. Router settings can't fix this."),
        Item::Blank,
        text(t, "You can:"),
        Item::Selected(Line::from(t.n(mark(true, "  Use a private link through Tor instead (works on any connection)"))), Line::default()),
        text(t, "  Ask your provider for your own public IPv4 address, then check again"),
        Item::Blank,
        dim(t, "Press d for the technical details."),
    ];
    Page::new(
        crumbs(t, &["Web", "mk-pay", "On the internet"]),
        Line::from(t.e("✕ Blocked by your provider")),
    )
    .panel(Panel::error("This connection can't take visitors", rows).grow())
    .footer(Item::Text(hints(
        t,
        &[
            ("↑↓", "choose"),
            ("↵", "go"),
            ("c", "check again"),
            ("d", "details"),
            ("Esc", "back"),
        ],
    )))
}

fn fail_certificate(t: &Theme) -> Page {
    let rows = vec![
        Item::Row(Line::from(vec![t.o("✓ "), t.n("mk-pay is online at https://pay.example.com")]), Line::default()),
        Item::Row(
            Line::from(vec![t.a("◆ "), t.n("Getting the secure connection")]),
            Line::from(t.a("next try in 4 min")),
        ),
        Item::Blank,
        text(t, "Until it's ready, browsers show visitors a warning. This usually takes a minute or two;"),
        text(t, "the Nodo keeps trying by itself and tells you if it gives up."),
        Item::Blank,
        dim(t, "Last try: the certificate service couldn't confirm the address yet. Press d for details."),
    ];
    Page::new(
        crumbs(t, &["Web", "mk-pay"]),
        Line::from(t.a("◆ Almost ready")),
    )
    .panel(Panel::accent("Secure connection pending", rows).grow())
    .footer(Item::Text(hints(
        t,
        &[("r", "try now"), ("d", "details"), ("Esc", "back")],
    )))
}

fn advanced(t: &Theme) -> Page {
    let certs = vec![
        Item::Row(
            Line::from(t.d(format!(" {}{}", pad("Name", 26), "Issued by"))),
            Line::from(t.d("Expires")),
        ),
        Item::Row(
            Line::from(t.n(format!(
                " {}{}",
                pad("pay.example.com", 26),
                "Let's Encrypt (DNS-01, Porkbun)"
            ))),
            Line::from(t.o("✓ in 61 days")),
        ),
        Item::Row(
            Line::from(t.n(format!(
                " {}{}",
                pad("mk-pay.local", 26),
                "This Nodo's local CA"
            ))),
            Line::from(t.o("✓ in 6 days")),
        ),
        Item::Row(
            Line::from(t.n(format!(
                " {}{}",
                pad("mk-lws.local", 26),
                "This Nodo's local CA"
            ))),
            Line::from(t.o("✓ in 6 days")),
        ),
    ];
    let address = vec![
        pair(t, "Domain", t.n("example.com".to_owned())),
        pair(t, "Provider", t.n("Porkbun (API key stored)".to_owned())),
        pair(
            t,
            "Home address",
            t.n("203.0.113.42, record updated 3 days ago".to_owned()),
        ),
        pair(
            t,
            "Open ports",
            t.n("80, 443 (because mk-pay is on the internet)".to_owned()),
        ),
    ];
    let local = vec![
        pair(t, "Trust page", t.n("http://nodo.local".to_owned())),
        pair(
            t,
            "Local CA",
            t.n("created 2026-11-13, root valid 10 years".to_owned()),
        ),
        pair(t, "Tor", t.o("✓ running")),
    ];
    Page::new(
        crumbs(t, &["Web", "Advanced"]),
        Line::from(t.d("monokulo-nodo")),
    )
    .columns(vec![
        (1, vec![Panel::new("Web address", address)]),
        (1, vec![Panel::new("Home network and Tor", local)]),
    ])
    .panel(Panel::new("Certificates", certs).grow())
    .footer(Item::Text(hints(
        t,
        &[
            ("c", "change provider"),
            ("k", "replace key"),
            ("r", "renew now"),
            ("Esc", "back"),
        ],
    )))
}

// ---------------------------------------------------------------------------------------------
// Web flow v3: providers are added once; the Web tab lists only sites that are online; hosting is
// chosen in each app. Mouse first: every action is a block at least three rows tall or a whole
// row, and the keys are one dim line at the bottom. One accent colour, one primary action.

const MARGIN: &str = "    ";

/// Adds several footer items: the action bar is pinned to the bottom, so the main button is
/// always on screen, even when a small terminal hides part of the page.
trait Footers {
    fn footers(self, items: Vec<Item>) -> Self;
}

impl Footers for Page {
    fn footers(mut self, items: Vec<Item>) -> Self {
        self.footer.extend(items);
        self
    }
}
/// The fill behind text boxes and open lists: a step above the ground, below the border grey.
const FIELD_BG: Color = Color::Rgb(0x24, 0x28, 0x2C);

fn primary(t: &Theme) -> Style {
    Style::new().fg(t.ground).bg(t.accent).add_modifier(Modifier::BOLD)
}

fn secondary(t: &Theme) -> Style {
    Style::new().fg(t.fg).bg(t.border)
}

fn field_bg(t: &Theme) -> Style {
    Style::new().fg(t.fg).bg(FIELD_BG)
}

fn blank_item() -> Item {
    Item::Blank
}

fn line(spans: Vec<Span<'static>>) -> Item {
    Item::Text(Line::from(spans))
}

fn margin(t: &Theme) -> Span<'static> {
    t.n(MARGIN)
}

/// Buttons as solid blocks, three rows tall, side by side.
fn buttons(t: &Theme, indent: &str, list: &[(&str, bool)]) -> Vec<Item> {
    let mut rows: [Vec<Span<'static>>; 3] = [vec![t.n(format!("{MARGIN}{indent}"))], vec![t.n(format!("{MARGIN}{indent}"))], vec![t.n(format!("{MARGIN}{indent}"))]];
    for (index, (label, is_primary)) in list.iter().enumerate() {
        let style = if *is_primary { primary(t) } else { secondary(t) };
        let width = label.chars().count() + 8;
        if index > 0 {
            for row in rows.iter_mut() {
                row.push(t.n("   "));
            }
        }
        rows[0].push(Span::styled(" ".repeat(width), style));
        rows[1].push(Span::styled(format!("    {label}    "), style));
        rows[2].push(Span::styled(" ".repeat(width), style));
    }
    rows.into_iter().map(line).collect()
}

/// A whole-row choice: tick box, name, and a quiet line under it. Clicking anywhere toggles it.
fn choice(t: &Theme, on: bool, title: &str, note: &str) -> Vec<Item> {
    let tick = if on { t.a("[x]") } else { t.d("[ ]") };
    vec![
        line(vec![margin(t), tick, t.n("  "), t.b(title.to_owned())]),
        line(vec![margin(t), t.d(format!("     {note}"))]),
        blank_item(),
    ]
}

/// A label and a click-to-open box; `▾` marks a list.
fn select(t: &Theme, indent: &str, label: &str, value: &str, open: bool) -> Item {
    let arrow = if open { "▴" } else { "▾" };
    line(vec![
        margin(t),
        t.n(indent.to_owned()),
        t.d(pad(label, 12)),
        Span::styled(format!(" {} {arrow} ", pad(value, 30)), field_bg(t)),
    ])
}

fn input(t: &Theme, indent: &str, label: &str, value: &str, suffix: &str) -> Item {
    let width = if suffix.is_empty() { 34 } else { 12 };
    line(vec![
        margin(t),
        t.n(indent.to_owned()),
        t.d(pad(label, 12)),
        Span::styled(format!(" {} ", pad(value, width)), field_bg(t)),
        t.n(suffix.to_owned()),
    ])
}

fn heading(t: &Theme, text: &str) -> Item {
    line(vec![margin(t), t.b(text.to_owned())])
}

fn quiet(t: &Theme, text: &str) -> Item {
    line(vec![margin(t), t.d(text.to_owned())])
}

fn link(t: &Theme, text: &str) -> Item {
    line(vec![margin(t), t.a(text.to_owned())])
}

fn trail(t: &Theme, parts: &[&str]) -> Item {
    let mut spans = vec![margin(t)];
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            spans.push(t.d("  ›  "));
        }
        spans.push(if index + 1 == parts.len() { t.n((*part).to_owned()) } else { t.d((*part).to_owned()) });
    }
    line(spans)
}

fn keys(t: &Theme, text: &str) -> Item {
    Item::Text(Line::from(t.d(format!(" {text}"))))
}

fn top(app: &App, t: &Theme, tab: Tab) -> Page {
    Page::new(tab_bar(app, t, tab), Line::from(t.d("monokulo-nodo")))
}

fn v3_web(app: &App, t: &Theme) -> Page {
    let mut items = vec![
        blank_item(),
        line(vec![margin(t), t.b(pad("Sites online", 92)), t.a("Domain providers ›")]),
        blank_item(),
    ];
    for (name, address, who) in [
        ("mk-pay", "https://pay.example.com", "Home network and internet"),
        ("mk-lws", "https://mk-lws.local", "Home network"),
        ("kringle", "http://kringlqx3v7…ylpd7qd.onion", "Tor"),
    ] {
        items.push(line(vec![margin(t), t.b(pad(name, 12)), t.n(pad(address, 34)), t.d(format!("{who}   ›"))]));
        items.push(blank_item());
    }
    items.push(blank_item());
    items.push(quiet(t, "To put another app online, open it in Packages."));
    top(app, t, Tab::Web).plain(items).footer(keys(t, "Click a site to change its hosting.   Keys: ↑↓ choose  ↵ open  p providers"))
}

fn hosting_intro(t: &Theme, app_name: &str) -> Vec<Item> {
    vec![
        blank_item(),
        trail(t, &["Packages", app_name, "Hosting"]),
        blank_item(),
        heading(t, "Hosting"),
        quiet(t, &format!("Choose who can open {app_name}. Any combination works.")),
        blank_item(),
    ]
}

fn v3_hosting(t: &Theme, internet: bool) -> Page {
    let (name, home_note) = if internet {
        ("mk-pay", "Phones and computers on your Wi-Fi.   https://mk-pay.local")
    } else {
        ("mk-lws", "Phones and computers on your Wi-Fi.   https://mk-lws.local")
    };
    let mut items = hosting_intro(t, name);
    items.extend(choice(t, true, "Home network", home_note));
    items.extend(choice(t, false, "Tor", "A private link that opens in Tor Browser. It doesn't reveal your home."));
    let mut internet_rows = choice(t, internet, "Internet", "Anyone, at a web address you own.");
    if internet {
        internet_rows.pop();
        items.extend(internet_rows);
        items.push(blank_item());
        items.push(select(t, "     ", "Provider", "Porkbun", false));
        items.push(select(t, "     ", "Domain", "example.com", false));
        items.push(input(t, "     ", "Address", "pay", " .example.com"));
        items.push(blank_item());
        items.push(quiet(t, "     Visitors will be able to see your home's internet address."));
        items.push(blank_item());
    } else {
        items.extend(internet_rows);
    }
    items.push(link(t, "Show details ›"));
    items.push(blank_item());
    top_tab(t).plain(items).footer(Item::Blank).footers(buttons(t, "", &[(if internet { "Check and save" } else { "Save" }, true)])).footer(keys(t, "Keys: Tab move  Space tick  ↵ save  Esc back"))
}

fn top_tab(t: &Theme) -> Page {
    let (app, _) = fixture(ScreenId::WebSites);
    top(&app, t, Tab::Packages)
}

fn v3_pick_domain(t: &Theme) -> Page {
    let mut items = hosting_intro(t, "mk-pay");
    items.extend(choice(t, true, "Home network", "Phones and computers on your Wi-Fi.   https://mk-pay.local"));
    items.extend(choice(t, false, "Tor", "A private link that opens in Tor Browser. It doesn't reveal your home."));
    let mut internet_rows = choice(t, true, "Internet", "Anyone, at a web address you own.");
    internet_rows.pop();
    items.extend(internet_rows);
    items.push(blank_item());
    items.push(select(t, "     ", "Provider", "Porkbun", false));
    items.push(select(t, "     ", "Domain", "example.com", true));
    let indent = format!("     {}", " ".repeat(12));
    for (domain, provider, current) in
        [("example.com", "Porkbun", true), ("shop-example.org", "Porkbun", false), ("myshop.net", "Cloudflare", false)]
    {
        let mark = if current { "✓" } else { " " };
        items.push(line(vec![
            margin(t),
            t.n(indent.clone()),
            Span::styled(format!(" {mark} {} {} ", pad(domain, 18), pad(provider, 12)), field_bg(t)),
        ]));
    }
    items.push(line(vec![
        margin(t),
        t.n(indent.clone()),
        Span::styled(format!(" {} ", pad("+ Add a provider…", 35)), Style::new().fg(t.accent).bg(FIELD_BG)),
    ]));
    items.push(blank_item());
    top_tab(t).plain(items).footer(Item::Blank).footers(buttons(t, "", &[("Check and save", true)])).footer(keys(t, "Keys: ↑↓ choose  ↵ pick  Esc close"))
}

fn v3_no_provider(t: &Theme) -> Page {
    let mut items = hosting_intro(t, "mk-pay");
    items.extend(choice(t, true, "Home network", "Phones and computers on your Wi-Fi.   https://mk-pay.local"));
    items.extend(choice(t, false, "Tor", "A private link that opens in Tor Browser. It doesn't reveal your home."));
    let mut internet_rows = choice(t, true, "Internet", "Anyone, at a web address you own.");
    internet_rows.pop();
    items.extend(internet_rows);
    items.push(blank_item());
    items.push(line(vec![margin(t), t.n("     To use a web address you own, first add the company you bought it from.")]));
    items.push(blank_item());
    items.push(quiet(t, "     You'll come back here when it's added."));
    top_tab(t).plain(items).footer(Item::Blank).footers(buttons(t, "     ", &[("Add a provider", true)])).footer(keys(t, "Keys: Tab move  Space tick  ↵ add a provider  Esc back"))
}

fn v3_confirm(t: &Theme) -> Page {
    let mut items = vec![blank_item(), trail(t, &["Packages", "mk-pay", "Hosting"]), blank_item(), heading(t, "Ready to put mk-pay on the internet"), blank_item()];
    for check in ["Porkbun accepted the key", "pay.example.com points to your home", "The internet can reach your Nodo", "Secure connection ready"] {
        items.push(line(vec![margin(t), t.o("✓  "), t.n(check.to_owned())]));
    }
    items.push(blank_item());
    items.push(line(vec![margin(t), t.n("Anyone will be able to open "), t.b("https://pay.example.com")]));
    items.push(quiet(t, "Visitors can see your home's internet address. The Nodo keeps everything up to date by itself."));
    items.push(blank_item());
    items.push(link(t, "Show details ›"));
    items.push(blank_item());
    top_tab(t).plain(items).footer(Item::Blank).footers(buttons(t, "", &[("Put mk-pay online", true), ("Cancel", false)])).footer(keys(t, "Keys: ↵ put online  Esc cancel"))
}

fn v3_online(t: &Theme) -> Page {
    let items = vec![
        blank_item(),
        trail(t, &["Packages", "mk-pay", "Hosting"]),
        blank_item(),
        blank_item(),
        line(vec![margin(t), t.o("✓  "), t.b("mk-pay is online")]),
        blank_item(),
        line(vec![margin(t), t.n("   https://pay.example.com")]),
        line(vec![margin(t), t.n("   https://mk-pay.local")]),
        blank_item(),
        quiet(t, "   The Nodo renews the secure connection and follows your home's address by itself."),
        blank_item(),
        blank_item(),
    ];
    top_tab(t).plain(items).footer(Item::Blank).footers(buttons(t, "   ", &[("Done", true)])).footer(keys(t, "Keys: ↵ done"))
}

fn v3_blocked(t: &Theme) -> Page {
    let items = vec![
        blank_item(),
        trail(t, &["Packages", "mk-pay", "Hosting"]),
        blank_item(),
        heading(t, "The internet can't reach your Nodo"),
        blank_item(),
        quiet(t, "Your internet provider shares one address between many homes, so visitors stop at the"),
        quiet(t, "provider and never reach you. Router settings can't change this."),
        blank_item(),
        line(vec![margin(t), t.n("A private Tor link still works on this connection.")]),
        blank_item(),
        link(t, "Show details ›"),
        blank_item(),
    ];
    top_tab(t).plain(items).footer(Item::Blank).footers(buttons(t, "", &[("Use Tor instead", true), ("Check again", false)])).footer(keys(t, "Keys: ↵ use Tor  c check again  Esc back"))
}

fn v3_providers(app: &App, t: &Theme) -> Page {
    let mut items = vec![
        blank_item(),
        trail(t, &["Web", "Domain providers"]),
        blank_item(),
        heading(t, "Domain providers"),
        quiet(t, "The companies where you bought your web addresses. Add one once; then any app can use its domains."),
        blank_item(),
        blank_item(),
    ];
    for (name, domains) in [("Porkbun", "example.com   shop-example.org"), ("Cloudflare", "myshop.net")] {
        items.push(line(vec![margin(t), t.b(pad(name, 14)), t.n(pad(domains, 44)), Span::styled("  Remove  ", secondary(t))]));
        items.push(blank_item());
    }
    items.push(blank_item());
    top(app, t, Tab::Web).plain(items).footer(Item::Blank).footers(buttons(t, "", &[("Add a provider", true)])).footer(keys(t, "Keys: ↑↓ choose  a add  x remove  Esc back"))
}

fn v3_add_provider(t: &Theme) -> Page {
    let (app, _) = fixture(ScreenId::WebSites);
    let mut items = vec![
        blank_item(),
        trail(t, &["Web", "Domain providers", "Add"]),
        blank_item(),
        heading(t, "Where did you buy your web address?"),
        blank_item(),
    ];
    for (name, note) in [
        ("Porkbun", ""),
        ("Cloudflare", ""),
        ("Namecheap", "Needs your home's address on an allow list; we'll show you how."),
        ("Gandi", ""),
        ("deSEC", ""),
    ] {
        items.push(line(vec![margin(t), t.n("  "), t.b(name.to_owned())]));
        if !note.is_empty() {
            items.push(line(vec![margin(t), t.d(format!("  {note}"))]));
        }
        items.push(blank_item());
    }
    items.push(line(vec![margin(t), t.d("  Somewhere else? It can't be connected yet, but Tor works without a web address.")]));
    top(&app, t, Tab::Web).plain(items).footer(keys(t, "Click the company.   Keys: ↑↓ choose  ↵ next  Esc back"))
}

fn v3_provider_key(t: &Theme) -> Page {
    let (app, _) = fixture(ScreenId::WebSites);
    let items = vec![
        blank_item(),
        trail(t, &["Web", "Domain providers", "Add", "Porkbun"]),
        blank_item(),
        heading(t, "Connect Porkbun"),
        quiet(t, "Make a key at porkbun.com › Account › API Access, then paste both parts here."),
        blank_item(),
        input(t, "", "API key", "pk1_7f3a••••••••••••••••", ""),
        blank_item(),
        input(t, "", "Secret key", "", ""),
        blank_item(),
        quiet(t, "The key stays on this Nodo, locked with your device passphrase."),
        blank_item(),
    ];
    top(&app, t, Tab::Web).plain(items).footer(Item::Blank).footers(buttons(t, "", &[("Connect", true)])).footer(keys(t, "Keys: Tab next box  ↵ connect  Esc back"))
}

fn v3_provider_done(t: &Theme) -> Page {
    let (app, _) = fixture(ScreenId::WebSites);
    let items = vec![
        blank_item(),
        trail(t, &["Web", "Domain providers", "Add", "Porkbun"]),
        blank_item(),
        line(vec![margin(t), t.o("✓  "), t.b("Porkbun is connected")]),
        blank_item(),
        quiet(t, "   This key can set up these web addresses:"),
        blank_item(),
        line(vec![margin(t), t.n("   example.com")]),
        line(vec![margin(t), t.n("   shop-example.org")]),
        blank_item(),
        quiet(t, "   Missing one? Turn on API access for it at Porkbun, and it appears here."),
        blank_item(),
    ];
    top(&app, t, Tab::Web).plain(items).footer(Item::Blank).footers(buttons(t, "   ", &[("Back to mk-pay", true)])).footer(keys(t, "Keys: ↵ back to mk-pay"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_has_a_unique_slug_and_renders_at_both_sizes() {
        let t = Theme::truecolor();
        for scene in Scene::ALL {
            assert_eq!(Scene::from_slug(scene.slug()), Some(scene));
            for (w, h) in [(120, 36), (80, 24)] {
                let buffer = render(scene, w, h, &t);
                let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
                assert!(
                    text.contains("Web"),
                    "{} at {w}x{h} lost its header",
                    scene.slug()
                );
            }
        }
    }

    #[test]
    fn every_v3_screen_names_what_is_clickable_and_keeps_its_button_on_a_small_terminal() {
        let t = Theme::truecolor();
        for scene in Scene::ALL.into_iter().filter(|s| s.is_v3()) {
            assert!(!scene.clickable().is_empty(), "{}", scene.slug());
            let small = render(scene, 80, 24, &t);
            let big = render(scene, 120, 36, &t);
            let footer = |b: &Buffer, h: u16| -> String {
                (h - 5..h).flat_map(|y| (0..b.area.width).map(move |x| (x, y))).map(|p| b[p].symbol().to_owned()).collect()
            };
            assert_eq!(footer(&small, 24).split_whitespace().collect::<Vec<_>>(), footer(&big, 36).split_whitespace().collect::<Vec<_>>(), "{}: the action bar differs at 80x24", scene.slug());
        }
    }

    #[test]
    fn plain_language_screens_avoid_jargon_outside_details() {
        let t = Theme::truecolor();
        for scene in [
            Scene::Overview,
            Scene::Who,
            Scene::HomeDone,
            Scene::NetAddress,
            Scene::NetChecks,
            Scene::NetConfirm,
            Scene::FailIsp,
            Scene::FailCertificate,
            Scene::V3Web,
            Scene::V3Hosting,
            Scene::V3HostingInternet,
            Scene::V3PickDomain,
            Scene::V3NoProvider,
            Scene::V3Confirm,
            Scene::V3Online,
            Scene::V3Blocked,
            Scene::V3Providers,
            Scene::V3AddProvider,
            Scene::V3ProviderKey,
            Scene::V3ProviderDone,
        ] {
            let buffer = render(scene, 120, 36, &t);
            let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
            for word in ["DNS-01", "CGNAT", "ACME", " CA ", "mDNS"] {
                assert!(!text.contains(word), "{} shows {word}", scene.slug());
            }
        }
    }
}
