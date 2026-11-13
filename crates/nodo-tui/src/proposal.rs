//! Web flow v2: a design proposal, mock only.
//!
//! These screens are drawn by the real layout engine and theme, so the boards made from them
//! (`examples/boards.rs`) show exactly what a terminal shows. Nothing here talks to the helper:
//! every value is sample data. `nodo-tui --mock --screen web2-overview` opens one in a terminal.
//!
//! The flow starts from an app, asks one question ("Who should be able to open it?"), and keeps
//! the technical words (DNS-01, CA, ports, record values) behind `d` Details. Domains and
//! certificates move to an Advanced screen.

use std::io;

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
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
}

impl Scene {
    pub const ALL: [Scene; 16] = [
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
        ] {
            let buffer = render(scene, 120, 36, &t);
            let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
            for word in ["DNS-01", "CGNAT", "ACME", " CA ", "mDNS"] {
                assert!(!text.contains(word), "{} shows {word}", scene.slug());
            }
        }
    }
}
