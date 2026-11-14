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
    V3Buttons,
    B1Home,
    B1Search,
    B1Detail,
    B1Review,
    B1Installing,
    B1Unlock,
    T2Nav,
    T2Home,
    T2Apps,
    T2GetApps,
    T2AppAbout,
    T2Review,
    T2Installing,
    T2AppPage,
    T2Unlock,
    B1Feeds,
    B1AddFeed,
    B1FeedKey,
    T2Sources,
    T2SourcesDetails,
    T2SourceAdd,
    T2SourceCheck,
    T2SourceErrors,
    T2SourceKeyChanged,
    T2SourceRemoveBuiltin,
    B1Settings,
    B1Reference,
    B1ApplyFailed,
    T2AppRemove,
    T2Settings,
    T2SettingsPick,
    T2SettingsRolledBack,
}

impl Scene {
    pub const ALL: [Scene; 61] = [
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
        Scene::V3Buttons,
        Scene::B1Home,
        Scene::B1Search,
        Scene::B1Detail,
        Scene::B1Review,
        Scene::B1Installing,
        Scene::B1Unlock,
        Scene::T2Nav,
        Scene::T2Home,
        Scene::T2Apps,
        Scene::T2GetApps,
        Scene::T2AppAbout,
        Scene::T2Review,
        Scene::T2Installing,
        Scene::T2AppPage,
        Scene::T2Unlock,
        Scene::B1Feeds,
        Scene::B1AddFeed,
        Scene::B1FeedKey,
        Scene::T2Sources,
        Scene::T2SourcesDetails,
        Scene::T2SourceAdd,
        Scene::T2SourceCheck,
        Scene::T2SourceErrors,
        Scene::T2SourceKeyChanged,
        Scene::T2SourceRemoveBuiltin,
        Scene::B1Settings,
        Scene::B1Reference,
        Scene::B1ApplyFailed,
        Scene::T2AppRemove,
        Scene::T2Settings,
        Scene::T2SettingsPick,
        Scene::T2SettingsRolledBack,
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
            Scene::V3Buttons => "web3-buttons",
            Scene::B1Home => "tui1-home",
            Scene::B1Search => "tui1-search",
            Scene::B1Detail => "tui1-detail",
            Scene::B1Review => "tui1-review",
            Scene::B1Installing => "tui1-installing",
            Scene::B1Unlock => "tui1-unlock",
            Scene::T2Nav => "tui2-nav",
            Scene::T2Home => "tui2-home",
            Scene::T2Apps => "tui2-apps",
            Scene::T2GetApps => "tui2-get-apps",
            Scene::T2AppAbout => "tui2-app-about",
            Scene::T2Review => "tui2-review",
            Scene::T2Installing => "tui2-installing",
            Scene::T2AppPage => "tui2-app-page",
            Scene::T2Unlock => "tui2-unlock",
            Scene::B1Feeds => "tui1-feeds",
            Scene::B1AddFeed => "tui1-add-feed",
            Scene::B1FeedKey => "tui1-feed-key",
            Scene::T2Sources => "tui2-sources",
            Scene::T2SourcesDetails => "tui2-sources-details",
            Scene::T2SourceAdd => "tui2-source-add",
            Scene::T2SourceCheck => "tui2-source-check",
            Scene::T2SourceErrors => "tui2-source-errors",
            Scene::T2SourceKeyChanged => "tui2-source-key-changed",
            Scene::T2SourceRemoveBuiltin => "tui2-source-remove-builtin",
            Scene::B1Settings => "tui1-settings",
            Scene::B1Reference => "tui1-reference",
            Scene::B1ApplyFailed => "tui1-apply-failed",
            Scene::T2AppRemove => "tui2-app-remove",
            Scene::T2Settings => "tui2-settings",
            Scene::T2SettingsPick => "tui2-settings-pick",
            Scene::T2SettingsRolledBack => "tui2-settings-rolled-back",
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
            Scene::V3Buttons => "Buttons: rounded flat fill, and the fallback",
            Scene::B1Home => "Before: Home today",
            Scene::B1Search => "Before: Search packages",
            Scene::B1Detail => "Before: Package detail",
            Scene::B1Review => "Before: Review permissions",
            Scene::B1Installing => "Before: Install progress",
            Scene::B1Unlock => "Before: Unlock after reboot",
            Scene::T2Nav => "Navigation: four tabs instead of six",
            Scene::T2Home => "Home: what needs you, then that the rest is fine",
            Scene::T2Apps => "Apps: installed apps",
            Scene::T2GetApps => "Get apps: search what you can install",
            Scene::T2AppAbout => "An app before installing: what it does, what it needs",
            Scene::T2Review => "Install review: permissions in plain words",
            Scene::T2Installing => "Installing: progress in plain steps",
            Scene::T2AppPage => "An app's page: status, its one action, then Hosting, Settings, Logs",
            Scene::T2Unlock => "Unlock after a restart",
            Scene::B1Feeds => "Before: Feeds",
            Scene::B1AddFeed => "Before: Add a feed",
            Scene::B1FeedKey => "Before: Feed key changed",
            Scene::T2Sources => "Sources: where the Nodo finds apps",
            Scene::T2SourcesDetails => "Sources with details open: each key, comparable",
            Scene::T2SourceAdd => "Add a source: paste the address",
            Scene::T2SourceCheck => "Add a source: what it offers, and its key to compare",
            Scene::T2SourceErrors => "Add a source: when it can't be added",
            Scene::T2SourceKeyChanged => "A source's key changed: check before you trust",
            Scene::T2SourceRemoveBuiltin => "The built-in monokulo source can be removed",
            Scene::B1Settings => "Before: Package settings",
            Scene::B1Reference => "Before: Reference picker",
            Scene::B1ApplyFailed => "Before: Settings rolled back",
            Scene::T2AppRemove => "Removing an app asks first, in plain words",
            Scene::T2Settings => "An app's Settings: every kind of field, an error shown in place",
            Scene::T2SettingsPick => "Settings: picking a value from another app (cuprated)",
            Scene::T2SettingsRolledBack => "Settings undone because the app didn't start",
        }
    }

    /// Version 3 screens, for grouping on the canvas.
    pub fn is_v3(self) -> bool {
        self.slug().starts_with("web3")
    }

    /// Screens drawn with the surface palette: web flow v3 and the simplified TUI.
    pub fn is_surface(self) -> bool {
        self.is_v3() || self.slug().starts_with("tui2")
    }

    /// Today's screen a "before" board shows, rendered by the real nodo-tui.
    pub fn before(self) -> Option<ScreenId> {
        Some(match self {
            Scene::V1Overview => ScreenId::WebSites,
            Scene::B1Home => ScreenId::Main,
            Scene::B1Search => ScreenId::Search,
            Scene::B1Detail => ScreenId::Detail,
            Scene::B1Review => ScreenId::Review,
            Scene::B1Installing => ScreenId::Installing,
            Scene::B1Unlock => ScreenId::Unlock,
            Scene::B1Feeds => ScreenId::Feeds,
            Scene::B1AddFeed => ScreenId::AddFeed,
            Scene::B1FeedKey => ScreenId::FeedKey,
            Scene::B1Settings => ScreenId::Settings,
            Scene::B1Reference => ScreenId::Reference,
            Scene::B1ApplyFailed => ScreenId::ApplyFailed,
            _ => return None,
        })
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
            Scene::V3Buttons => "The chosen rounded buttons, the no-Nerd-Font fallback, and the first v3 blocks for comparison.",
            Scene::T2Nav => "Clickable: the four tabs; each section line opens it.",
            Scene::T2Home => "Clickable: Review updates (the one primary), each app row (opens its page), the tabs. Hover shown on mk-pay's row.",
            Scene::T2Apps => "Clickable: each app row (opens its page), \"Get apps\", the tabs. Scroll wheel scrolls. Hover shown on mk-pay's row.",
            Scene::T2GetApps => "Clickable: the search box (focus to type), each result (opens it), \"Sources\" (the old Feeds). Hover shown on feather-ws.",
            Scene::T2AppAbout => "Clickable: Install (primary), \"Show details\" (permissions in full), the breadcrumb.",
            Scene::T2Review => "Clickable: the passphrase box, Install (primary), Cancel, \"Show details\".",
            Scene::T2Installing => "Clickable: the breadcrumb and tabs; installing carries on if you leave.",
            Scene::T2AppPage => "Clickable: Update (primary), Restart, Stop, Remove (destructive, asks first), each section row. Hover shown on the Settings row.",
            Scene::T2Unlock => "Clickable: the passphrase box and Unlock.",
            Scene::T2Sources => "Clickable: Add a source (primary), each On/Off box, Remove on each row, Try again, \"Show details\" (keys). Hover shown on Alice's apps.",
            Scene::T2SourcesDetails => "Clickable: the same, and \"Hide details\".",
            Scene::T2SourceAdd => "Clickable: the address box (focus to paste), Check (primary), Cancel.",
            Scene::T2SourceCheck => "Clickable: Add source (primary), Cancel.",
            Scene::T2SourceErrors => "Clickable: in the Add card, the address box and Check again.",
            Scene::T2SourceKeyChanged => "Clickable: Keep the old key (primary), the name box, Trust the new key (only once the name is typed).",
            Scene::T2SourceRemoveBuiltin => "Clickable: Remove (primary), Cancel.",
            Scene::T2AppRemove => "Clickable: Remove mk-pay (destructive), Cancel.",
            Scene::T2Settings => "Clickable: each box (focus to type), Currency and Monero node (open a list), Show, − and +, the On box, Save (primary), Cancel.",
            Scene::T2SettingsPick => "Clickable: each choice in the list; clicking outside closes it.",
            Scene::T2SettingsRolledBack => "Clickable: Edit again (primary), Keep as it is, \"Show log\".",
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
            if let Some(id) = scene.before() {
                let (before, _) = fixture(id);
                view::render(frame, &before, theme);
            } else if scene.is_surface() {
                let w = frame.area().width;
                let h = frame.area().height;
                let page = if scene.is_v3() { v3_page(&app, theme, scene, w, h) } else { tui2_page(theme, scene, w, h) };
                draw_v3(frame, theme, page);
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
                if let Some(id) = scene.before() {
                    let (before, _) = fixture(id);
                    view::render(frame, &before, &theme);
                } else if scene.is_surface() {
                    let w = frame.area().width;
                    let h = frame.area().height;
                    let page = if scene.is_v3() { v3_page(&app, &theme, scene, w, h) } else { tui2_page(&theme, scene, w, h) };
                    draw_v3(frame, &theme, page);
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
        _ => Page::new(Line::default(), Line::default()),
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
// chosen in each app. Mouse first: every action is a whole row or a block three rows tall, and the
// keys are one dim line at the bottom. One accent colour, one primary action.
//
// Areas are separated by surfaces, not lines: the tab bar and the action bar are strips of their
// own, content sits on cards, text boxes are inset. Every surface is a plain rectangle of whole
// cells. Buttons are flat fills whose ends are the Powerline rounded caps U+E0B6 and U+E0B4 drawn
// in the button's colour; those need a Nerd Font, so `rounded` can be off (config or detection),
// and the ends are then plain padding. On a 256-colour terminal each surface has an indexed
// fallback; on a 16-colour console (the Linux console) the surfaces all become black and only
// the accent, the spacing and the button shapes remain.

const MARGIN: &str = "    ";

/// The surfaces, darkest to lightest: inset fields sit below the app background, cards and bars
/// rise above it, and selected or open things are raised further. Steps are small to stay calm;
/// body text keeps at least 7:1 and dim text at least 4.5:1 on every one of them.
#[derive(Debug, Clone, Copy)]
pub struct Surfaces {
    pub field: Color,
    pub bar: Color,
    pub card: Color,
    /// A row or button under the mouse pointer: one step lighter than what it sits on.
    pub hover: Color,
    pub raised: Color,
    /// A secondary button under the pointer.
    pub raised_hover: Color,
    /// The fill of a destructive button (Remove): red-tinted, its label in the error colour.
    pub danger: Color,
}

impl Surfaces {
    pub const fn truecolor() -> Self {
        Self {
            field: Color::Rgb(0x0B, 0x0D, 0x0F),
            bar: Color::Rgb(0x17, 0x1A, 0x1D),
            card: Color::Rgb(0x1D, 0x21, 0x25),
            hover: Color::Rgb(0x26, 0x2B, 0x30),
            raised: Color::Rgb(0x2C, 0x32, 0x38),
            raised_hover: Color::Rgb(0x3A, 0x41, 0x48),
            danger: Color::Rgb(0x3D, 0x22, 0x24),
        }
    }

    /// For terminals without RGB: the nearest grays of the 256-colour cube.
    pub const fn ansi256() -> Self {
        Self {
            field: Color::Indexed(232),
            bar: Color::Indexed(234),
            card: Color::Indexed(235),
            hover: Color::Indexed(236),
            raised: Color::Indexed(237),
            raised_hover: Color::Indexed(239),
            danger: Color::Indexed(52),
        }
    }
}

const S: Surfaces = Surfaces::truecolor();

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

fn on(span: Span<'static>, bg: Color) -> Span<'static> {
    let style = span.style.bg(bg);
    span.style(style)
}

fn width_of(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.content.as_ref())).sum()
}

fn line(spans: Vec<Span<'static>>) -> Item {
    Item::Text(Line::from(spans))
}

fn margin(t: &Theme) -> Span<'static> {
    t.n(MARGIN)
}

fn quiet(t: &Theme, text: &str) -> Item {
    line(vec![margin(t), t.d(text.to_owned())])
}

fn heading(t: &Theme, text: &str) -> Item {
    line(vec![margin(t), t.b(text.to_owned())])
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

/// A card: a filled rectangle the width of the page less its margins, every row the same width,
/// three cells of padding on the left. Each row may be clicked.
struct Card {
    width: usize,
    rows: Vec<Item>,
    /// Where the card's own button row starts, so fitting a short terminal keeps it.
    buttons_at: Option<usize>,
}

impl Card {
    fn new(page_width: u16) -> Self {
        let width = usize::from(page_width).saturating_sub(2 * MARGIN.len());
        Card { width, rows: Vec::new(), buttons_at: None }
    }

    /// One row of the card. Text too wide for the card wraps at spaces onto more card rows,
    /// keeping its indent, so the fill always stays a rectangle.
    fn row(&mut self, t: &Theme, spans: Vec<Span<'static>>) {
        self.row_on(t, spans, S.card);
    }

    /// A row as it looks under the mouse pointer: the whole row one step lighter. The keyboard's
    /// selection is shown differently (the name in the accent with a ▸), so the two never mix up.
    fn hover_row(&mut self, t: &Theme, spans: Vec<Span<'static>>) {
        self.row_on(t, spans, S.hover);
    }

    /// A row with one button at its right end, on the card's inner padding.
    fn row_with_button(&mut self, t: &Theme, mut spans: Vec<Span<'static>>, label: &str, is_primary: bool) {
        let width = label.trim_start_matches(['!', '~']).chars().count() + 6;
        let [_, button, _] = button_rows(t, label, is_primary, ButtonStyle::Rounded, width, S.card);
        let room = self.width.saturating_sub(6);
        let used = width_of(&spans) + width_of(&button);
        spans.push(t.n(" ".repeat(room.saturating_sub(used))));
        spans.extend(button);
        self.row(t, spans);
    }

    fn row_on(&mut self, t: &Theme, spans: Vec<Span<'static>>, bg: Color) {
        let room = self.width.saturating_sub(4);
        let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
        if width_of(&spans) <= room {
            lines[0] = spans;
        } else {
            let indent = spans.first().map_or(0, |s| s.content.len() - s.content.trim_start().len());
            let mut used = 0;
            for span in spans {
                let style = span.style;
                for (index, word) in span.content.split(' ').enumerate() {
                    let piece = if index == 0 { word.to_owned() } else { format!(" {word}") };
                    let w = unicode_width::UnicodeWidthStr::width(piece.as_str());
                    if used + w > room && used > indent {
                        lines.push(vec![Span::styled(" ".repeat(indent), style)]);
                        used = indent;
                        let trimmed = piece.trim_start().to_owned();
                        used += unicode_width::UnicodeWidthStr::width(trimmed.as_str());
                        lines.last_mut().expect("just pushed").push(Span::styled(trimmed, style));
                    } else {
                        used += w;
                        lines.last_mut().expect("not empty").push(Span::styled(piece, style));
                    }
                }
            }
        }
        for spans in lines {
            let mut out = vec![margin(t), on(t.n("   "), bg)];
            let used = 3 + width_of(&spans);
            out.extend(spans.into_iter().map(|s| if s.style.bg.is_none() { on(s, bg) } else { s }));
            out.push(on(t.n(" ".repeat(self.width.saturating_sub(used))), bg));
            self.rows.push(line(out));
        }
    }

    fn blank(&mut self, t: &Theme) {
        self.row(t, Vec::new());
    }

    /// The card's buttons, after the content they act on: right-aligned on the card's inner
    /// padding, the primary rightmost, three cells apart, followed by the card's bottom padding.
    fn buttons(&mut self, t: &Theme, list: &[(&str, bool)]) {
        if list.is_empty() {
            return;
        }
        self.buttons_at = Some(self.rows.len());
        let room = self.width.saturating_sub(6);
        let bar = button_bar(t, list, ButtonStyle::Rounded, S.card);
        // `button_bar` gives three rows (padding, buttons, padding); the card already has its own
        // padding rows, so only the middle one is used, without the bar's right margin.
        if let Some(Item::Row(_, right)) = bar.into_iter().nth(1) {
            let mut spans = right.spans;
            spans.pop();
            let used = width_of(&spans);
            let mut out = vec![t.n(" ".repeat(room.saturating_sub(used)))];
            out.extend(spans);
            self.row(t, out);
        }
        self.blank(t);
    }

    /// One button at a given indent inside the card, for a button that belongs to a part of the
    /// card rather than to the whole of it.
    fn inline_button(&mut self, t: &Theme, label: &str, indent: usize) {
        let width = label.chars().count() + 6;
        let [_, middle, _] = button_rows(t, label, true, ButtonStyle::Rounded, width, S.card);
        let mut out = vec![t.n(" ".repeat(indent))];
        out.extend(middle);
        self.row(t, out);
        if self.buttons_at.is_none() {
            self.buttons_at = Some(self.rows.len() - 1);
        }
    }

    /// Fits the card into `max` rows: when it is taller, the rows above its buttons are cut and a
    /// line says how many scroll, so the buttons stay on screen.
    fn fit(&mut self, t: &Theme, max: usize) {
        if self.rows.len() <= max {
            return;
        }
        let tail = self.buttons_at.map_or(0, |at| self.rows.len() - at);
        let head = max.saturating_sub(tail + 1);
        let hidden = self.rows.len() - tail - head;
        let tail_rows: Vec<Item> = self.rows.split_off(self.rows.len() - tail);
        self.rows.truncate(head);
        let at = self.rows.len();
        self.row(t, vec![t.d(format!("↓ {hidden} more — scroll"))]);
        self.buttons_at = self.buttons_at.map(|_| at + 1);
        self.rows.extend(tail_rows);
    }

    fn items(self, _t: &Theme) -> Vec<Item> {
        self.rows
    }
}

/// An inset box for typing or picking, on a card.
fn inset(t: &Theme, value: &str, width: usize, arrow: Option<&str>) -> Vec<Span<'static>> {
    let inset = Style::new().fg(t.fg).bg(S.field);
    let mut text = format!(" {} ", pad(value, width));
    if let Some(arrow) = arrow {
        text = format!(" {} {arrow} ", pad(value, width));
    }
    vec![Span::styled(text, inset)]
}

/// How a button is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    /// A flat fill, one row tall, with rounded ends: the Powerline caps U+E0B6 and U+E0B4 in the
    /// button's colour. Needs a Nerd Font in the terminal.
    Rounded,
    /// The same without the caps, for terminals without a Nerd Font: the fill with a cell of
    /// padding each side. `[tui] rounded_buttons = "auto" | "on" | "off"` picks between them.
    Plain,
    /// A plain filled block, three full rows (the first v3 boards).
    Flat,
}

/// Every button in a bar has the same width: the longest label plus padding.
fn button_width(list: &[(&str, bool)]) -> usize {
    list.iter().map(|(label, _)| label.trim_start_matches(['!', '~']).chars().count()).max().unwrap_or(0) + 6
}

fn centred(label: &str, width: usize) -> String {
    let len = label.chars().count();
    let left = (width - len) / 2;
    format!("{}{label}{}", " ".repeat(left), " ".repeat(width - len - left))
}

const CAP_LEFT: &str = "\u{E0B6}";
const CAP_RIGHT: &str = "\u{E0B4}";

/// One button's three rows, drawn on the surface `under`. Rounded and plain buttons are one row
/// tall with the rows above and below left empty: those still count for a click.
/// A label starting with `!` is destructive (red-tinted fill, label in the error colour); one
/// starting with `~` is drawn as under the mouse pointer.
fn button_rows(t: &Theme, label: &str, is_primary: bool, style: ButtonStyle, width: usize, under: Color) -> [Vec<Span<'static>>; 3] {
    let danger = label.starts_with('!');
    let hovered = label.contains('~');
    let label = label.trim_start_matches(['!', '~']);
    let (colour, ink) = if danger {
        (S.danger, t.err)
    } else if is_primary {
        (if hovered { Color::Rgb(0xF7, 0xB8, 0x5E) } else { t.accent }, t.ground)
    } else {
        (if hovered { S.raised_hover } else { S.raised }, t.fg)
    };
    let weight = if is_primary { Modifier::BOLD } else { Modifier::empty() };
    let fill = Style::new().fg(ink).bg(colour).add_modifier(weight);
    let cap = Style::new().fg(colour).bg(under);
    let space = |n: usize| Span::styled(" ".repeat(n), Style::new().bg(under));
    match style {
        ButtonStyle::Rounded => [
            vec![space(width + 2)],
            vec![Span::styled(CAP_LEFT, cap), Span::styled(centred(label, width), fill), Span::styled(CAP_RIGHT, cap)],
            vec![space(width + 2)],
        ],
        ButtonStyle::Plain => [
            vec![space(width + 2)],
            vec![Span::styled(centred(label, width + 2), fill)],
            vec![space(width + 2)],
        ],
        ButtonStyle::Flat => [
            vec![Span::styled(" ".repeat(width + 2), fill)],
            vec![Span::styled(centred(label, width + 2), fill)],
            vec![Span::styled(" ".repeat(width + 2), fill)],
        ],
    }
}

/// A bar of buttons, right-aligned on the page margin, three cells apart, primary rightmost.
fn button_bar(t: &Theme, list: &[(&str, bool)], style: ButtonStyle, under: Color) -> Vec<Item> {
    let width = button_width(list);
    let mut rows: [Vec<Span<'static>>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut ordered: Vec<&(&str, bool)> = list.iter().filter(|(_, p)| !p).collect();
    ordered.extend(list.iter().filter(|(_, p)| *p));
    for (index, (label, is_primary)) in ordered.into_iter().enumerate() {
        if index > 0 {
            for row in rows.iter_mut() {
                row.push(on(t.n("   "), under));
            }
        }
        for (row, part) in rows.iter_mut().zip(button_rows(t, label, *is_primary, style, width, under)) {
            row.extend(part);
        }
    }
    rows.into_iter()
        .map(|mut spans| {
            spans.push(on(t.n(MARGIN), under));
            Item::Row(Line::default(), Line::from(spans))
        })
        .collect()
}

/// The bottom line: only the keys, dim. Buttons live in the card they act on.
fn action_bar(t: &Theme, keys: &str) -> Vec<Item> {
    vec![Item::Text(Line::from(t.d(format!("{MARGIN}{keys}"))))]
}

/// How many screen rows items take at width `w`, as the layout wraps them.
fn rows_of(items: &[Item], w: u16) -> usize {
    let w = usize::from(w.max(1));
    items
        .iter()
        .map(|item| match item {
            Item::Blank => 1,
            Item::Text(line) => width_of(&line.spans).div_ceil(w).max(1),
            Item::Row(left, right) | Item::Selected(left, right) => {
                if width_of(&left.spans) + width_of(&right.spans) + 1 > w { 2 } else { 1 }
            }
        })
        .sum()
}

/// Puts a page together: what comes before the card, the card fitted to the rows left, what
/// comes after, and the one line of keys at the bottom.
#[allow(clippy::too_many_arguments)]
fn fit_page(t: &Theme, page: Page, before: Vec<Item>, mut card: Card, after: Vec<Item>, keys: &str, w: u16, h: u16) -> Page {
    let used = usize::from(STRIP_ROWS) + rows_of(&action_bar(t, keys), w) + rows_of(&before, w) + rows_of(&after, w);
    card.fit(t, usize::from(h).saturating_sub(used));
    let mut items = before;
    items.extend(card.items(t));
    items.extend(after);
    page.plain(items).footers(action_bar(t, keys))
}

/// The tab bar: a strip of its own with an empty row above and below, the tabs on the page margin
/// with room between them, the current one a flat filled rectangle padded two cells each side.
/// Each tab is a click target.
fn top(app: &App, t: &Theme, tab: Tab, w: u16) -> Page {
    let mut tabs = vec![t.n("  ")];
    for each in Tab::ALL {
        let pending = app.pending_updates();
        let extra = if each == Tab::Updates && pending > 0 { format!(" {pending}") } else { String::new() };
        if each == tab {
            tabs.push(Span::styled(format!("  {}{extra}  ", each.label()), Style::new().fg(t.accent).bg(S.raised).add_modifier(Modifier::BOLD)));
        } else {
            tabs.push(t.n(format!("  {}", each.label())));
            if !extra.is_empty() {
                tabs.push(t.d(extra));
            }
            tabs.push(t.n("  "));
        }
        tabs.push(t.n(" "));
    }
    // The device name only where it fits beside the tabs without crowding them.
    let row = if w >= 100 {
        Item::Row(Line::from(tabs), Line::from(vec![t.d("monokulo-nodo"), t.n(MARGIN)]))
    } else {
        Item::Text(Line::from(tabs))
    };
    Page::new(Line::default(), Line::default()).plain(vec![row, Item::Blank])
}

/// Rows from the top that belong to the tab strip.
const STRIP_ROWS: u16 = 3;

/// Draws a v3 page and paints its surfaces: the tab strip and the action bar. Cards and fields
/// carry their own fills.
fn draw_v3(frame: &mut ratatui::Frame, t: &Theme, page: Page) {
    let area = frame.area();
    let footer_rows = page.footer.len() as u16;
    frame.buffer_mut().set_style(area, t.base());
    draw_page(frame, area, t, page);
    let buffer = frame.buffer_mut();
    let paint = |buffer: &mut Buffer, rows: std::ops::Range<u16>| {
        for y in rows {
            for x in 0..area.width {
                let cell = &mut buffer[(x, y)];
                if cell.bg == t.ground {
                    cell.set_bg(S.bar);
                }
            }
        }
    };
    paint(buffer, 0..STRIP_ROWS.min(area.height));
    paint(buffer, area.height.saturating_sub(footer_rows)..area.height);
}

fn v3_page(app: &App, t: &Theme, scene: Scene, w: u16, h: u16) -> Page {
    match scene {
        Scene::V3Web => v3_web(app, t, w, h),
        Scene::V3Hosting => v3_hosting(app, t, w, h, Internet::Off),
        Scene::V3HostingInternet => v3_hosting(app, t, w, h, Internet::Chosen),
        Scene::V3PickDomain => v3_hosting(app, t, w, h, Internet::Picking),
        Scene::V3NoProvider => v3_hosting(app, t, w, h, Internet::NoProvider),
        Scene::V3Confirm => v3_confirm(app, t, w, h),
        Scene::V3Online => v3_online(app, t, w, h),
        Scene::V3Blocked => v3_blocked(app, t, w, h),
        Scene::V3Providers => v3_providers(app, t, w, h),
        Scene::V3AddProvider => v3_add_provider(app, t, w, h),
        Scene::V3ProviderKey => v3_provider_key(app, t, w, h),
        Scene::V3ProviderDone => v3_provider_done(app, t, w, h),
        Scene::V3Buttons => v3_buttons(app, t, w, h),
        _ => Page::new(Line::default(), Line::default()),
    }
}

fn v3_web(app: &App, t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    let sites = [
        ("mk-pay", "https://pay.example.com", "Home and internet"),
        ("mk-lws", "https://mk-lws.local", "Home network"),
        ("kringle", "http://kringlqx3v7…ylpd7qd.onion", "Tor"),
    ];
    for (index, (name, address, who)) in sites.into_iter().enumerate() {
        if index > 0 {
            card.blank(t);
        }
        card.row(t, vec![t.b(pad(name, 11)), t.n(pad(address, 34)), t.d(pad(who, 20)), t.d("›")]);
    }
    card.blank(t);
    let items = vec![
        Item::Blank,
        Item::Row(Line::from(vec![margin(t), t.b("Sites online")]), Line::from(vec![t.a("Domain providers ›"), t.n(MARGIN)])),
        Item::Blank,
    ];
    let after = vec![Item::Blank, quiet(t, "To put another app online, open it in Packages.")];
    fit_page(t, top(app, t, Tab::Web, w), items, card, after, "Click a site to change its hosting.   Keys: ↑↓ choose  ↵ open  p providers", w, h)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Internet {
    Off,
    Chosen,
    Picking,
    NoProvider,
}

fn tick(t: &Theme, on_: bool) -> Span<'static> {
    if on_ {
        t.a("[x]")
    } else {
        t.d("[ ]")
    }
}

fn v3_hosting(app: &App, t: &Theme, w: u16, h: u16, internet: Internet) -> Page {
    let name = if internet == Internet::Off { "mk-lws" } else { "mk-pay" };
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![tick(t, true), t.n("  "), t.b("Home network")]);
    card.row(t, vec![t.d(format!("     Phones and computers on your Wi-Fi.   https://{name}.local"))]);
    card.blank(t);
    card.row(t, vec![tick(t, false), t.n("  "), t.b("Tor")]);
    card.row(t, vec![t.d("     A private link that opens in Tor Browser. It doesn't reveal your home.")]);
    card.blank(t);
    card.row(t, vec![tick(t, internet != Internet::Off), t.n("  "), t.b("Internet")]);
    card.row(t, vec![t.d("     Anyone, at a web address you own.")]);
    let label = |text: &str| t.d(format!("     {}", pad(text, 12)));
    match internet {
        Internet::Off => {}
        Internet::Chosen | Internet::Picking => {
            card.blank(t);
            let mut provider = vec![label("Provider")];
            provider.extend(inset(t, "Porkbun", 28, Some("▾")));
            card.row(t, provider);
            card.blank(t);
            let mut domain = vec![label("Domain")];
            domain.extend(inset(t, "example.com", 28, Some(if internet == Internet::Picking { "▴" } else { "▾" })));
            card.row(t, domain);
            if internet == Internet::Picking {
                let list = Style::new().fg(t.fg).bg(S.raised);
                for (domain, provider, current) in
                    [("example.com", "Porkbun", true), ("shop-example.org", "Porkbun", false), ("myshop.net", "Cloudflare", false)]
                {
                    let mark = if current { "✓" } else { " " };
                    card.row(t, vec![t.n(" ".repeat(17)), Span::styled(format!(" {mark} {} {} ", pad(domain, 16), pad(provider, 11)), list)]);
                }
                card.row(t, vec![t.n(" ".repeat(17)), Span::styled(format!(" {} ", pad("+ Add a provider…", 30)), Style::new().fg(t.accent).bg(S.raised))]);
            } else {
                card.blank(t);
                let mut address = vec![label("Address")];
                address.extend(inset(t, "pay", 10, None));
                address.push(t.n(" .example.com"));
                card.row(t, address);
                card.blank(t);
                card.row(t, vec![t.d("     Visitors will be able to see your home's internet address.")]);
            }
        }
        Internet::NoProvider => {
            card.blank(t);
            card.row(t, vec![t.n("     To use a web address you own, first add the company you bought it from.")]);
            card.row(t, vec![t.d("     You'll come back here when it's added.")]);
            card.blank(t);
            // The button belongs to the Internet choice, so it sits in that row's area, on its
            // indent, rather than at the end of the card.
            card.inline_button(t, "Add a provider", 5);
        }
    }
    card.blank(t);
    card.row(t, vec![t.a("Show details ›")]);
    card.blank(t);
    let (buttons, keys): (&[(&str, bool)], &str) = match internet {
        Internet::Off => (&[("Save", true)], "Keys: Tab move  Space tick  ↵ save  Esc back"),
        Internet::Chosen => (&[("Check and save", true)], "Keys: Tab move  Space tick  ↵ check and save  Esc back"),
        Internet::Picking => (&[("Check and save", true)], "Keys: ↑↓ choose  ↵ pick  Esc close the list"),
        Internet::NoProvider => (&[], "Keys: Tab move  Space tick  ↵ add a provider  Esc back"),
    };
    card.buttons(t, buttons);
    let items = vec![
        Item::Blank,
        trail(t, &["Packages", name, "Hosting"]),
        Item::Blank,
        line(vec![margin(t), t.b("Hosting")]),
        quiet(t, &format!("Choose who can open {name}. Any combination works.")),
        Item::Blank,
    ];
    fit_page(t, top(app, t, Tab::Packages, w), items, card, Vec::new(), keys, w, h)
}

fn v3_confirm(app: &App, t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    for check in ["Porkbun accepted the key", "pay.example.com points to your home", "The internet can reach your Nodo", "Secure connection ready"] {
        card.row(t, vec![t.o("✓  "), t.n(check.to_owned())]);
    }
    card.blank(t);
    card.row(t, vec![t.n("Anyone will be able to open "), t.b("https://pay.example.com")]);
    card.row(t, vec![t.d("Visitors can see your home's internet address. The Nodo keeps everything up to date itself.")]);
    card.blank(t);
    card.row(t, vec![t.a("Show details ›")]);
    card.blank(t);
    let mut items = vec![Item::Blank, trail(t, &["Packages", "mk-pay", "Hosting"]), Item::Blank];
    items.push(heading(t, "Ready to put mk-pay on the internet"));
    items.push(Item::Blank);
    card.buttons(t, &[("Cancel", false), ("Put mk-pay online", true)]);
    let after = Vec::new();
    fit_page(t, top(app, t, Tab::Packages, w), items, card, after, "Keys: ↵ put online  Esc cancel", w, h)
}

fn v3_online(app: &App, t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.o("✓  "), t.b("mk-pay is online")]);
    card.blank(t);
    card.row(t, vec![t.n("   https://pay.example.com")]);
    card.row(t, vec![t.n("   https://mk-pay.local")]);
    card.blank(t);
    card.row(t, vec![t.d("   The Nodo renews the secure connection and follows your home's address by itself.")]);
    card.blank(t);
    let items = vec![Item::Blank, trail(t, &["Packages", "mk-pay", "Hosting"]), Item::Blank, Item::Blank];
    card.buttons(t, &[("Done", true)]);
    let after = Vec::new();
    fit_page(t, top(app, t, Tab::Packages, w), items, card, after, "Keys: ↵ done", w, h)
}

fn v3_blocked(app: &App, t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.n("Your internet provider shares one address between many homes, so visitors stop at the")]);
    card.row(t, vec![t.n("provider and never reach you. Router settings can't change this.")]);
    card.blank(t);
    card.row(t, vec![t.d("A private Tor link still works on this connection.")]);
    card.blank(t);
    card.row(t, vec![t.a("Show details ›")]);
    card.blank(t);
    let mut items = vec![Item::Blank, trail(t, &["Packages", "mk-pay", "Hosting"]), Item::Blank];
    items.push(heading(t, "The internet can't reach your Nodo"));
    items.push(Item::Blank);
    card.buttons(t, &[("Check again", false), ("Use Tor instead", true)]);
    let after = Vec::new();
    fit_page(t, top(app, t, Tab::Packages, w), items, card, after, "Keys: ↵ use Tor  c check again  Esc back", w, h)
}

fn v3_providers(app: &App, t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    for (index, (name, domains)) in [("Porkbun", "example.com   shop-example.org"), ("Cloudflare", "myshop.net")].into_iter().enumerate() {
        if index > 0 {
            card.blank(t);
        }
        card.row(t, vec![t.b(pad(name, 14)), t.n(pad(domains, 40)), Span::styled("  Remove  ", Style::new().fg(t.fg).bg(S.raised))]);
    }
    card.blank(t);
    let mut items = vec![Item::Blank, trail(t, &["Web", "Domain providers"]), Item::Blank, heading(t, "Domain providers")];
    items.push(quiet(t, "The companies where you bought your web addresses. Add one once; then any app can use its domains."));
    items.push(Item::Blank);
    card.buttons(t, &[("Add a provider", true)]);
    let after = Vec::new();
    fit_page(t, top(app, t, Tab::Web, w), items, card, after, "Keys: ↑↓ choose  a add  x remove  Esc back", w, h)
}

fn v3_add_provider(app: &App, t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    for (index, (name, note)) in [
        ("Porkbun", ""),
        ("Cloudflare", ""),
        ("Namecheap", "Needs your home's address on an allow list; we'll show you how."),
        ("Gandi", ""),
        ("deSEC", ""),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            card.blank(t);
        }
        card.row(t, vec![t.b(pad(name, 14)), t.d(note.to_owned())]);
    }
    card.blank(t);
    let items = vec![Item::Blank, trail(t, &["Web", "Domain providers", "Add"]), Item::Blank, heading(t, "Where did you buy your web address?"), Item::Blank];
    let after = vec![Item::Blank, quiet(t, "Somewhere else? It can't be connected yet, but Tor works without a web address.")];
    fit_page(t, top(app, t, Tab::Web, w), items, card, after, "Click the company.   Keys: ↑↓ choose  ↵ next  Esc back", w, h)
}

fn v3_provider_key(app: &App, t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    let mut key = vec![t.d(pad("API key", 13))];
    key.extend(inset(t, "pk1_7f3a••••••••••••••••", 34, None));
    card.row(t, key);
    card.blank(t);
    let mut secret = vec![t.d(pad("Secret key", 13))];
    secret.extend(inset(t, "", 34, None));
    card.row(t, secret);
    card.blank(t);
    card.row(t, vec![t.d("The key stays on this Nodo, locked with your device passphrase.")]);
    card.blank(t);
    let mut items = vec![Item::Blank, trail(t, &["Web", "Domain providers", "Add", "Porkbun"]), Item::Blank, heading(t, "Connect Porkbun")];
    items.push(quiet(t, "Make a key at porkbun.com › Account › API Access, then paste both parts here."));
    items.push(Item::Blank);
    card.buttons(t, &[("Connect", true)]);
    let after = Vec::new();
    fit_page(t, top(app, t, Tab::Web, w), items, card, after, "Keys: Tab next box  ↵ connect  Esc back", w, h)
}

fn v3_provider_done(app: &App, t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.o("✓  "), t.b("Porkbun is connected")]);
    card.blank(t);
    card.row(t, vec![t.d("   This key can set up these web addresses:")]);
    card.row(t, vec![t.n("   example.com")]);
    card.row(t, vec![t.n("   shop-example.org")]);
    card.blank(t);
    card.row(t, vec![t.d("   Missing one? Turn on API access for it at Porkbun, and it appears here.")]);
    card.blank(t);
    let items = vec![Item::Blank, trail(t, &["Web", "Domain providers", "Add", "Porkbun"]), Item::Blank, Item::Blank];
    card.buttons(t, &[("Back to mk-pay", true)]);
    let after = Vec::new();
    fit_page(t, top(app, t, Tab::Web, w), items, card, after, "Keys: ↵ back to mk-pay", w, h)
}

/// The button styles side by side on the action bar's surface, for choosing.
fn v3_buttons(app: &App, t: &Theme, w: u16, h: u16) -> Page {
    let list: &[(&str, bool)] = &[("Cancel", false), ("Put mk-pay online", true)];
    let mut items = vec![Item::Blank, heading(t, "Buttons: flat fills with rounded ends")];
    for (name, note, style) in [
        ("Rounded (chosen)", "Powerline rounded caps in the button colour; needs a Nerd Font.", ButtonStyle::Rounded),
        ("Without a Nerd Font", "The automatic fallback: the same fill with plain ends.", ButtonStyle::Plain),
        ("Flat, three rows (first v3)", "For comparison.", ButtonStyle::Flat),
    ] {
        let mut card = Card::new(w);
        card.blank(t);
        card.row(t, vec![t.b(name.to_owned()), t.d(format!("   {note}"))]);
        let inner = card.width - 6;
        for row in button_bar(t, list, style, S.card) {
            if let Item::Row(_, right) = row {
                let spans: Vec<Span<'static>> = right.spans;
                let used = width_of(&spans);
                let mut out = vec![t.n(" ".repeat(inner.saturating_sub(used)))];
                out.extend(spans);
                card.row(t, out);
            }
        }
        items.push(Item::Blank);
        items.extend(card.items(t));
    }
    let _ = h;
    top(app, t, Tab::Packages, w).plain(items).footers(action_bar(t, "Rounded and plain buttons are one row tall; the rows above and below count for a click too."))
}

// ---------------------------------------------------------------------------------------------
// Simplified TUI (batch 1: navigation, Home, Apps). Four tabs instead of six:
//   Home    what needs you, and that everything else is fine
//   Apps    installed apps; each app's page holds its Hosting, Settings, Logs and updates;
//           "Get apps" finds new ones, and its Sources replace the Feeds tab
//   Web     as in web flow v3: sites online, Domain providers
//   System  updates for the Nodo itself, backups, network, device logs
// The Logs and Feeds tabs disappear (logs live on each app and under System; feeds are an app
// source). Updates leave their tab: app updates show on the app and on Home, system ones under
// System. Same visual system as v3.

const TABS2: [&str; 4] = ["Home", "Apps", "Web", "System"];

/// The four-tab strip, same look as v3's.
fn top2(t: &Theme, tab: &str, badge: Option<(&str, usize)>, w: u16) -> Page {
    let mut tabs = vec![t.n("  ")];
    for each in TABS2 {
        let extra = match badge {
            Some((name, n)) if name == each && n > 0 => format!(" {n}"),
            _ => String::new(),
        };
        if each == tab {
            tabs.push(Span::styled(format!("  {each}{extra}  "), Style::new().fg(t.accent).bg(S.raised).add_modifier(Modifier::BOLD)));
        } else {
            tabs.push(t.n(format!("  {each}")));
            if !extra.is_empty() {
                tabs.push(t.a(extra));
            }
            tabs.push(t.n("  "));
        }
        tabs.push(t.n(" "));
    }
    let row = if w >= 100 {
        Item::Row(Line::from(tabs), Line::from(vec![t.d("monokulo-nodo"), t.n(MARGIN)]))
    } else {
        Item::Text(Line::from(tabs))
    };
    Page::new(Line::default(), Line::default()).plain(vec![row, Item::Blank])
}

/// A page of several cards stacked with a blank row between them.
fn stack(t: &Theme, page: Page, before: Vec<Item>, cards: Vec<Card>, after: Vec<Item>, keys: &str) -> Page {
    let mut items = before;
    for (index, card) in cards.into_iter().enumerate() {
        if index > 0 {
            items.push(Item::Blank);
        }
        items.extend(card.items(t));
    }
    items.extend(after);
    page.plain(items).footers(action_bar(t, keys))
}

/// A progress bar of whole cells: done cells filled with the accent, the rest inset.
fn bar(t: &Theme, fraction: f32, width: usize) -> Vec<Span<'static>> {
    let done = ((fraction.clamp(0.0, 1.0)) * width as f32).round() as usize;
    vec![
        Span::styled(" ".repeat(done), Style::new().bg(t.accent)),
        Span::styled(" ".repeat(width - done), Style::new().bg(S.field)),
    ]
}

fn tui2_page(t: &Theme, scene: Scene, w: u16, h: u16) -> Page {
    match scene {
        Scene::T2Nav => t2_nav(t, w),
        Scene::T2Home => t2_home(t, w),
        Scene::T2Apps => t2_apps(t, w),
        Scene::T2GetApps => t2_get_apps(t, w),
        Scene::T2AppAbout => t2_app_about(t, w, h),
        Scene::T2Review => t2_review(t, w, h),
        Scene::T2Installing => t2_installing(t, w, h),
        Scene::T2AppPage => t2_app_page(t, w),
        Scene::T2Unlock => t2_unlock(t, w, h),
        Scene::T2Sources => t2_sources(t, w, h, false),
        Scene::T2SourcesDetails => t2_sources(t, w, h, true),
        Scene::T2SourceAdd => t2_source_add(t, w, h),
        Scene::T2SourceCheck => t2_source_check(t, w, h),
        Scene::T2SourceErrors => t2_source_errors(t, w),
        Scene::T2SourceKeyChanged => t2_source_key_changed(t, w, h),
        Scene::T2SourceRemoveBuiltin => t2_source_remove_builtin(t, w, h),
        Scene::T2AppRemove => t2_app_remove(t, w, h),
        Scene::T2Settings => t2_settings(t, w, h, false),
        Scene::T2SettingsPick => t2_settings(t, w, h, true),
        Scene::T2SettingsRolledBack => t2_settings_rolled_back(t, w, h),
        _ => Page::new(Line::default(), Line::default()),
    }
}

fn t2_nav(t: &Theme, w: u16) -> Page {
    let section = |title: &str, now: &str, holds: &[&str], from: &str| {
        let mut card = Card::new(w);
        card.blank(t);
        card.row(t, vec![t.b(pad(title, 9)), t.n(now.to_owned())]);
        for item in holds {
            card.row(t, vec![t.n(" ".repeat(9)), t.d(format!("· {item}"))]);
        }
        if !from.is_empty() {
            card.row(t, vec![t.n(" ".repeat(9)), t.a(from.to_owned())]);
        }
        card.blank(t);
        card
    };
    let cards = vec![
        section("Home", "What needs you, and that everything else is fine.", &[], "was Home: same tab, much less on it"),
        section(
            "Apps",
            "Your installed apps. Click one for its own page:",
            &["Hosting (who can open it)  ·  Settings  ·  Logs  ·  Update", "Get apps: search what you can install; Sources (the old Feeds)"],
            "merges Packages, Feeds, per-app Logs and per-app Updates",
        ),
        section("Web", "Sites online, and Domain providers. (Web flow v3, approved.)", &[], ""),
        section(
            "System",
            "The Nodo itself:",
            &["Updates for the Nodo (system base, automatic updates, rollbacks)", "Backups  ·  Network (Wi-Fi, Bluetooth)  ·  Device logs"],
            "takes the Updates and Logs tabs' Nodo-wide parts",
        ),
    ];
    let before = vec![
        Item::Blank,
        heading(t, "Four tabs instead of six"),
        quiet(t, "Logs and Feeds stop being tabs; an app's settings, hosting, logs and updates live on its own page."),
        Item::Blank,
    ];
    stack(t, top2(t, "Home", None, w), before, cards, Vec::new(), "Click a tab, or press 1 2 3 4.")
}

fn t2_home(t: &Theme, w: u16) -> Page {
    let mut needs = Card::new(w);
    needs.blank(t);
    needs.row(t, vec![t.b("2 app updates are ready")]);
    needs.row(t, vec![t.d("mk-pay and kringle. Each keeps its settings and can be rolled back.")]);
    needs.blank(t);
    needs.buttons(t, &[("Review updates", true)]);
    let mut fine = Card::new(w);
    fine.blank(t);
    fine.row(t, vec![t.o("✓  "), t.b("Everything else is running")]);
    fine.blank(t);
    for (name, state) in [
        ("cuprated", "Syncing the Monero chain · 63%"),
        ("mk-pay", "Running · open at https://pay.example.com"),
        ("mk-lws", "Running"),
        ("kringle", "Running"),
    ] {
        if name == "mk-pay" {
            fine.hover_row(t, vec![t.n(pad(name, 12)), t.d(state.to_owned())]);
        } else {
            fine.row(t, vec![t.n(pad(name, 12)), t.d(state.to_owned())]);
        }
    }
    fine.blank(t);
    fine.row(t, vec![t.d("Storage 27% used of 2 TB   ·   Temperature 48 °C   ·   Up 3 days")]);
    fine.blank(t);
    let before = vec![Item::Blank, heading(t, "Good afternoon"), Item::Blank];
    stack(t, top2(t, "Home", Some(("Apps", 2)), w), before, vec![needs, fine], Vec::new(), "Keys: ↵ review updates  Tab move  1-4 tabs")
}

fn t2_apps(t: &Theme, w: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    for (index, (name, what, state)) in [
        ("cuprated", "Monero node", "Syncing · 63%"),
        ("mk-pay", "Payments for your shop", "Update ready"),
        ("mk-lws", "Light wallet server", "Running"),
        ("mk-bell", "Payment alerts", "Running"),
        ("kringle", "Tip jar", "Update ready"),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            card.blank(t);
        }
        let status = if state == "Update ready" { t.a(pad(state, 14)) } else { t.d(pad(state, 14)) };
        let spans = vec![t.b(pad(name, 12)), t.n(pad(what, 28)), status, t.d("›")];
        if name == "mk-pay" {
            card.hover_row(t, spans);
        } else {
            card.row(t, spans);
        }
    }
    card.blank(t);
    let before = vec![
        Item::Blank,
        Item::Row(Line::from(vec![margin(t), t.b("Installed")]), Line::from(vec![t.a("Get apps ›"), t.n(MARGIN)])),
        Item::Blank,
    ];
    stack(t, top2(t, "Apps", Some(("Apps", 2)), w), before, vec![card], Vec::new(), "Click an app to open it.   Keys: ↑↓ choose  ↵ open  g get apps")
}

fn t2_get_apps(t: &Theme, w: u16) -> Page {
    let mut search = vec![t.d(pad("Search", 9))];
    search.extend(inset(t, "wallet", 40, None));
    let mut card = Card::new(w);
    card.blank(t);
    for (index, (name, what, tag)) in [
        ("mk-lws", "Light wallet server for Monero wallets", "Installed"),
        ("feather-ws", "Server for Feather Wallet", ""),
        ("mk-pay", "Accept Monero in your shop", "Installed"),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            card.blank(t);
        }
        let spans = vec![t.b(pad(name, 13)), t.n(pad(what, 42)), t.d(tag.to_owned())];
        if name == "feather-ws" {
            card.hover_row(t, spans);
        } else {
            card.row(t, spans);
        }
    }
    card.blank(t);
    let before = vec![
        Item::Blank,
        trail(t, &["Apps", "Get apps"]),
        Item::Blank,
        Item::Row(Line::from(vec![margin(t)].into_iter().chain(search).collect::<Vec<_>>()), Line::from(vec![t.a("Sources ›"), t.n(MARGIN)])),
        Item::Blank,
    ];
    let after = vec![Item::Blank, quiet(t, "3 of 14 apps from your sources match. Click one to see what it does before installing.")];
    stack(t, top2(t, "Apps", Some(("Apps", 2)), w), before, vec![card], after, "Type to search.   Keys: ↑↓ choose  ↵ open  s sources  Esc back")
}

fn t2_app_about(t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.b("feather-ws"), t.d("   Server for Feather Wallet · from monokulo · 18 MB")]);
    card.blank(t);
    card.row(t, vec![t.n("Lets Feather Wallet on your phone or computer use your own Monero node instead of a")]);
    card.row(t, vec![t.n("stranger's, so nobody else sees which wallet is yours.")]);
    card.blank(t);
    card.row(t, vec![t.d("It will:")]);
    for item in ["Read from your Monero node", "Use the internet to reach other Monero nodes", "Keep 2 GB of data on the drive"] {
        card.row(t, vec![t.n(format!("  ·  {item}"))]);
    }
    card.blank(t);
    card.row(t, vec![t.a("Show details ›")]);
    card.blank(t);
    card.buttons(t, &[("Install", true)]);
    let before = vec![Item::Blank, trail(t, &["Apps", "Get apps", "feather-ws"]), Item::Blank];
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), before, card, Vec::new(), "Keys: ↵ install  d details  Esc back", w, h)
}

fn t2_review(t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.b("Install feather-ws?")]);
    card.blank(t);
    card.row(t, vec![t.n("It will be able to:")]);
    for (item, note) in [
        ("Read from your Monero node", ""),
        ("Use the internet", "to reach other Monero nodes"),
        ("Keep 2 GB on the drive", "locked with your device passphrase"),
    ] {
        card.row(t, vec![t.o("✓  "), t.n(item.to_owned()), t.d(if note.is_empty() { String::new() } else { format!("   {note}") })]);
    }
    card.blank(t);
    card.row(t, vec![t.d("It can't see your other apps or their data. You can remove it at any time.")]);
    card.blank(t);
    let mut pass = vec![t.d(pad("Passphrase", 13))];
    pass.extend(inset(t, "••••••••", 30, None));
    card.row(t, pass);
    card.blank(t);
    card.row(t, vec![t.a("Show details ›")]);
    card.blank(t);
    card.buttons(t, &[("Cancel", false), ("Install", true)]);
    let before = vec![Item::Blank, trail(t, &["Apps", "Get apps", "feather-ws", "Install"]), Item::Blank];
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), before, card, Vec::new(), "Keys: type the passphrase  ↵ install  Esc cancel", w, h)
}

fn t2_installing(t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.b("Installing feather-ws")]);
    card.blank(t);
    let mut progress = bar(t, 0.6, 50);
    progress.push(t.d("   60%"));
    card.row(t, progress);
    card.blank(t);
    for (mark, text) in [("✓", "Downloaded and checked"), ("✓", "Space made on the drive"), ("›", "Starting it"), ("○", "Making sure it answers")] {
        let span = match mark {
            "✓" => t.o(format!("{mark}  ")),
            "›" => t.a(format!("{mark}  ")),
            _ => t.d(format!("{mark}  ")),
        };
        card.row(t, vec![span, if mark == "○" { t.d(text.to_owned()) } else { t.n(text.to_owned()) }]);
    }
    card.blank(t);
    card.row(t, vec![t.d("You can leave this page; it carries on.")]);
    card.blank(t);
    let before = vec![Item::Blank, trail(t, &["Apps", "feather-ws"]), Item::Blank];
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), before, card, Vec::new(), "Keys: Esc back (installing continues)", w, h)
}

fn t2_app_page(t: &Theme, w: u16) -> Page {
    let mut status = Card::new(w);
    status.blank(t);
    status.row(t, vec![t.b("mk-pay"), t.d("   Payments for your shop")]);
    status.row(t, vec![t.o("●  "), t.n("Running for 3 days   ·   open at https://pay.example.com")]);
    status.blank(t);
    status.row_with_button(t, vec![t.a("Version 0.2 is ready"), t.d("   fixes receipts in some currencies")], "Update", true);
    status.blank(t);
    status.buttons(t, &[("Restart", false), ("Stop", false), ("!Remove…", false)]);
    let mut sections = Card::new(w);
    sections.blank(t);
    for (index, (name, summary)) in [
        ("Hosting", "Home network and internet"),
        ("Settings", "Store name, currency, 4 more"),
        ("Logs", "Last problem: none"),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            sections.blank(t);
        }
        let spans = vec![t.b(pad(name, 12)), t.d(pad(summary, 40)), t.d("›")];
        if name == "Settings" {
            sections.hover_row(t, spans);
        } else {
            sections.row(t, spans);
        }
    }
    sections.blank(t);
    let before = vec![Item::Blank, trail(t, &["Apps", "mk-pay"]), Item::Blank];
    stack(t, top2(t, "Apps", Some(("Apps", 2)), w), before, vec![status, sections], Vec::new(), "Click a section.   Keys: ↑↓ choose  ↵ open  u update  r restart  Esc back")
}

fn t2_app_remove(t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.e("▲  "), t.b("Remove mk-pay?")]);
    card.blank(t);
    card.row(t, vec![t.n("Its data on the drive is deleted: orders, receipts and settings. Your shop stops taking")]);
    card.row(t, vec![t.n("payments. Make a backup first if you might want it back.")]);
    card.blank(t);
    card.buttons(t, &[("Cancel", false), ("!Remove mk-pay", true)]);
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), vec![Item::Blank, trail(t, &["Apps", "mk-pay", "Remove"]), Item::Blank], card, Vec::new(), "Keys: Esc cancel  (Remove asks for a click, not Enter)", w, h)
}

/// A settings field: label column, then the control, and a quiet hint or an error under it.
fn setting(card: &mut Card, t: &Theme, label: &str, control: Vec<Span<'static>>, hint: Option<Span<'static>>) {
    let mut spans = vec![t.n(pad(label, 24))];
    spans.extend(control);
    card.row(t, spans);
    if let Some(hint) = hint {
        card.row(t, vec![t.n(" ".repeat(24)), hint]);
    }
    card.blank(t);
}

fn t2_settings(t: &Theme, w: u16, h: u16, picking: bool) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    setting(&mut card, t, "Store name", inset(t, "Corner Café", 30, None), None);
    setting(&mut card, t, "Currency", inset(t, "EUR — euro", 28, Some("▾")), None);
    setting(
        &mut card,
        t,
        "Payment address",
        {
            let mut v = inset(t, "48ZqC7…x3", 30, None);
            v[0].style = v[0].style.fg(t.err);
            v
        },
        Some(t.e("✕ That isn't a Monero address: it should be 95 characters starting with 4 or 8")),
    );
    setting(&mut card, t, "View key", vec![Span::styled(format!(" {} ", pad("••••••••••••••••", 30)), Style::new().fg(t.fg).bg(S.field)), t.a("   Show")], Some(t.d("Lets mk-pay see payments arrive, but not spend them.")));
    setting(&mut card, t, "Confirmations to wait", vec![Span::styled("  10  ", Style::new().fg(t.fg).bg(S.field)), t.n("   "), t.a("−"), t.n("   "), t.a("+")], Some(t.d("0 to 30. More is safer but slower; 10 takes about 20 minutes.")));
    setting(&mut card, t, "Email receipts", toggle(t, true), None);
    let mut node = inset(t, "cuprated  · Monero node on this Nodo", 36, Some(if picking { "▴" } else { "▾" }));
    if picking {
        node[0].style = node[0].style.bg(S.field);
    }
    card.row(t, {
        let mut spans = vec![t.n(pad("Monero node", 24))];
        spans.extend(node);
        spans
    });
    if picking {
        let list = Style::new().fg(t.fg).bg(S.raised);
        let pad_left = " ".repeat(24);
        card.row(t, vec![t.n(pad_left.clone()), Span::styled(format!(" ✓ {} ", pad("cuprated  · on this Nodo (recommended)", 36)), list)]);
        card.row(t, vec![t.n(pad_left.clone()), Span::styled(format!("   {} ", pad("monerod   · on this Nodo, stopped", 36)), Style::new().fg(t.dim).bg(S.raised))]);
        card.row(t, vec![t.n(pad_left), Span::styled(format!("   {} ", pad("Another node… type its address", 36)), Style::new().fg(t.accent).bg(S.raised))]);
    } else {
        card.row(t, vec![t.n(" ".repeat(24)), t.d("Picked from your apps, so it follows cuprated if its address changes.")]);
    }
    card.blank(t);
    card.buttons(t, &[("Cancel", false), ("Save", true)]);
    let mut before = vec![Item::Blank, trail(t, &["Apps", "mk-pay", "Settings"]), Item::Blank];
    before.push(heading(t, "Settings"));
    before.push(quiet(t, "mk-pay restarts with the new settings. If it doesn't start, they're undone by themselves."));
    before.push(Item::Blank);
    let keys = if picking { "Keys: ↑↓ choose  ↵ pick  Esc close the list" } else { "Keys: Tab move  ↵ save  Esc cancel" };
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), before, card, Vec::new(), keys, w, h)
}

fn t2_settings_rolled_back(t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.e("▲  "), t.b("mk-pay didn't start with the new settings, so they were undone")]);
    card.blank(t);
    card.row(t, vec![t.n("It's running again with the settings it had before. What you changed:")]);
    card.row(t, vec![t.d(pad("  Confirmations to wait", 26)), t.n("10  →  0")]);
    card.row(t, vec![t.d(pad("  Email receipts", 26)), t.n("On  →  Off")]);
    card.blank(t);
    card.row(t, vec![t.d("mk-pay said: \"confirmations must be at least 1\"")]);
    card.blank(t);
    card.row(t, vec![t.a("Show log ›")]);
    card.blank(t);
    card.buttons(t, &[("Keep as it is", false), ("Edit again", true)]);
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), vec![Item::Blank, trail(t, &["Apps", "mk-pay", "Settings"]), Item::Blank], card, Vec::new(), "Keys: ↵ edit again  Esc keep as it is", w, h)
}

fn t2_unlock(t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.b("Unlock your Nodo")]);
    card.blank(t);
    card.row(t, vec![t.n("The Nodo restarted. Your apps' data is locked until you type the device passphrase.")]);
    card.row(t, vec![t.d("cuprated is already running; mk-pay, mk-lws and kringle start after you unlock.")]);
    card.blank(t);
    let mut pass = vec![t.d(pad("Passphrase", 13))];
    pass.extend(inset(t, "", 30, None));
    card.row(t, pass);
    card.blank(t);
    card.buttons(t, &[("Unlock", true)]);
    let before = vec![Item::Blank, Item::Blank];
    fit_page(t, top2(t, "Home", None, w), before, card, Vec::new(), "Keys: type the passphrase  ↵ unlock", w, h)
}

// Sources (the old Feeds), under Apps › Get apps › Sources. Adding your own is a main task, so it
// is the card's primary action. Keys are shown as four groups of four, to compare by eye.

fn fingerprint_groups(fp: &str) -> String {
    let parts: Vec<&str> = fp.split(':').collect();
    parts.chunks(4).map(|c| c.join("")).collect::<Vec<_>>().join("  ")
}

const MONOKULO_FP: &str = "62:54:a2:57:c2:57:9d:7e:11:8f:4c:30:a9:32:93:07";
const ALICE_FP: &str = "9f:03:6e:b1:44:d2:7a:10:c8:5b:e3:91:0d:76:2a:ff";
const ALICE_NEW_FP: &str = "17:c4:90:2e:aa:5f:63:08:b9:d1:4e:77:30:1c:e5:42";

fn sources_trail(t: &Theme, last: &[&str]) -> Vec<Item> {
    let mut parts = vec!["Apps", "Get apps", "Sources"];
    parts.extend_from_slice(last);
    vec![Item::Blank, trail(t, &parts), Item::Blank]
}

fn toggle(t: &Theme, on_: bool) -> Vec<Span<'static>> {
    if on_ {
        vec![t.a("[x]"), t.n(" On ")]
    } else {
        vec![t.d("[ ]"), t.d(" Off")]
    }
}

fn t2_sources(t: &Theme, w: u16, h: u16, details: bool) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    // name, address, apps, enabled, problem, key
    type SourceRow<'a> = (&'a str, &'a str, &'a str, bool, Option<&'a str>, &'a str);
    let rows: [SourceRow; 3] = [
        ("monokulo", "nodo.monokulo.com/feed", "5 apps · built in", true, None, MONOKULO_FP),
        ("Alice's apps", "apps.alice.example/feed", "2 apps", true, Some("Couldn't reach it since yesterday"), ALICE_FP),
        ("Test builds", "192.168.1.40:8080/feed", "1 app", false, None, "4a:11:0e:93:5c:28:b7:f0:62:d9:13:8e:a4:07:5b:c1"),
    ];
    for (index, (name, address, count, enabled, problem, fp)) in rows.into_iter().enumerate() {
        if index > 0 {
            card.blank(t);
        }
        let mut first = vec![t.b(pad(name, 15)), t.n(pad(address, 28)), t.d(pad(count, 18))];
        first.extend(toggle(t, enabled));
        first.push(t.n("   "));
        first.push(Span::styled("  Remove  ", Style::new().fg(t.fg).bg(S.raised)));
        if index == 1 {
            card.hover_row(t, first);
        } else {
            card.row(t, first);
        }
        if let Some(problem) = problem {
            card.row(t, vec![t.n(" ".repeat(15)), t.e(format!("✕ {problem}")), t.d("   · Try again")]);
        }
        if details {
            card.row(t, vec![t.n(" ".repeat(15)), t.d(format!("Key  {}", fingerprint_groups(fp)))]);
        }
    }
    card.blank(t);
    card.row(t, vec![t.a(if details { "Hide details ‹" } else { "Show details ›" })]);
    card.blank(t);
    card.buttons(t, &[("Add a source", true)]);
    let mut before = sources_trail(t, &[]);
    before.push(heading(t, "Sources"));
    before.push(quiet(t, "Where the Nodo finds apps. Each source signs its apps; the Nodo only installs what its key signed."));
    before.push(Item::Blank);
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), before, card, Vec::new(), "Keys: ↑↓ choose  Space on/off  a add  x remove  d details  Esc back", w, h)
}

fn t2_source_add(t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.b("Add a source")]);
    card.blank(t);
    card.row(t, vec![t.n("Paste the address the publisher gave you.")]);
    card.blank(t);
    let mut address = vec![t.d(pad("Address", 10))];
    address.extend(inset(t, "https://apps.bob.example/feed", 44, None));
    card.row(t, address);
    card.blank(t);
    card.row(t, vec![t.d("Nothing is installed yet: the Nodo first shows you what the source offers and its key.")]);
    card.blank(t);
    card.buttons(t, &[("Cancel", false), ("Check", true)]);
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), sources_trail(t, &["Add"]), card, Vec::new(), "Keys: paste the address  ↵ check  Esc cancel", w, h)
}

fn t2_source_check(t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.o("✓  "), t.b("Bob's apps"), t.d("   apps.bob.example/feed")]);
    card.blank(t);
    card.row(t, vec![t.n("It offers 3 apps: "), t.b("bob-notes"), t.n(", "), t.b("bob-photos"), t.n(", "), t.b("bob-backup")]);
    card.blank(t);
    card.row(t, vec![t.n("Its key")]);
    card.row(t, vec![Span::styled(format!("  {}  ", fingerprint_groups("3c:8e:51:0a:d7:26:f4:9b:e0:12:6d:a8:47:c3:b5:19")), Style::new().fg(t.accent).bg(S.field).add_modifier(Modifier::BOLD))]);
    card.blank(t);
    card.row(t, vec![t.n("Check this matches what the publisher gave you, by a way you trust (in person, their site,")]);
    card.row(t, vec![t.n("a message you know is from them). If it doesn't match, don't add it.")]);
    card.blank(t);
    card.buttons(t, &[("Cancel", false), ("Add source", true)]);
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), sources_trail(t, &["Add"]), card, Vec::new(), "Keys: ↵ add source  Esc cancel", w, h)
}

fn t2_source_errors(t: &Theme, w: u16) -> Page {
    let make = |title: &str, body: &[&str]| {
        let mut card = Card::new(w);
        card.blank(t);
        card.row(t, vec![t.e("✕  "), t.b(title.to_owned())]);
        for line_ in body {
            card.row(t, vec![t.n(format!("   {line_}"))]);
        }
        card.blank(t);
        card
    };
    let cards = vec![
        make("That address isn't a source", &["It answered, but with a web page, not a list of apps. Check you copied the whole address."]),
        make("Couldn't reach it", &["Nothing answered at apps.bob.example. It may be offline, or the address may be wrong.", "Try again later, or check the address with the publisher."]),
        make("Its signature doesn't check out", &["The list of apps wasn't signed by the key it claims. Someone may have tampered with it,", "so the Nodo won't use it. Tell the publisher."]),
    ];
    let mut before = sources_trail(t, &["Add"]);
    before.push(heading(t, "When a source can't be added (three cases, shown in the Add card)"));
    before.push(Item::Blank);
    stack(t, top2(t, "Apps", Some(("Apps", 2)), w), before, cards, Vec::new(), "Each case appears in place of the source's details; the address stays filled in to fix and Check again.")
}

fn t2_source_key_changed(t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.e("▲  "), t.b("Alice's apps is now signed with a different key")]);
    card.blank(t);
    card.row(t, vec![t.n("This happens when a publisher replaces a lost key, and also when someone else takes over their address. Until you decide, the Nodo installs nothing new from Alice's apps.")]);
    card.blank(t);
    card.row(t, vec![t.d(pad("Key you trust", 15)), t.n(fingerprint_groups(ALICE_FP))]);
    card.row(t, vec![t.d(pad("New key", 15)), Span::styled(format!(" {} ", fingerprint_groups(ALICE_NEW_FP)), Style::new().fg(t.accent).bg(S.field).add_modifier(Modifier::BOLD))]);
    card.blank(t);
    card.row(t, vec![t.n("Ask Alice, by a way you trust, whether the new key is hers. To trust it, type the source's name:")]);
    let mut typed = vec![t.d(pad("Name", 15))];
    typed.extend(inset(t, "", 30, None));
    typed.push(t.d("   Alice's apps"));
    card.row(t, typed);
    card.blank(t);
    card.buttons(t, &[("Trust the new key", false), ("Keep the old key", true)]);
    let mut before = sources_trail(t, &["Alice's apps"]);
    before.push(heading(t, "Check before you trust"));
    before.push(Item::Blank);
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), before, card, Vec::new(), "Keys: ↵ keep the old key  Tab move  Esc back", w, h)
}

fn t2_source_remove_builtin(t: &Theme, w: u16, h: u16) -> Page {
    let mut card = Card::new(w);
    card.blank(t);
    card.row(t, vec![t.b("Remove monokulo?")]);
    card.blank(t);
    card.row(t, vec![t.n("It came with the Nodo, but you don't have to keep it. Apps you installed from it keep")]);
    card.row(t, vec![t.n("running; they just won't get updates until you add it back.")]);
    card.blank(t);
    card.row(t, vec![t.d("To add it back later: Sources › Show details › Restore built-in sources.")]);
    card.blank(t);
    card.buttons(t, &[("Cancel", false), ("Remove", true)]);
    fit_page(t, top2(t, "Apps", Some(("Apps", 2)), w), sources_trail(t, &["monokulo"]), card, Vec::new(), "Keys: ↵ remove  Esc cancel", w, h)
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
                // Today's screens without a tab bar (package detail, review…) are drawn as they are.
                assert!(
                    scene.before().is_some() || text.contains("Web"),
                    "{} at {w}x{h} lost its header",
                    scene.slug()
                );
            }
        }
    }

    #[test]
    fn every_v3_screen_names_what_is_clickable_and_keeps_its_main_button_on_a_small_terminal() {
        // Buttons live in their card; when a small terminal can't show the whole card, the rows
        // above the buttons scroll and the buttons stay. So the main button is on screen at 80x24.
        let t = Theme::truecolor();
        let main = |scene: Scene| match scene {
            Scene::V3Hosting => Some("Save"),
            Scene::V3HostingInternet | Scene::V3PickDomain => Some("Check and save"),
            Scene::V3NoProvider | Scene::V3Providers => Some("Add a provider"),
            Scene::V3Confirm => Some("Put mk-pay online"),
            Scene::V3Online => Some("Done"),
            Scene::V3Blocked => Some("Use Tor instead"),
            Scene::V3ProviderKey => Some("Connect"),
            Scene::V3ProviderDone => Some("Back to mk-pay"),
            Scene::T2Home => Some("Review updates"),
            Scene::T2AppAbout | Scene::T2Review => Some("Install"),
            Scene::T2AppPage => Some("Update"),
            Scene::T2Unlock => Some("Unlock"),
            Scene::T2Sources | Scene::T2SourcesDetails => Some("Add a source"),
            Scene::T2SourceAdd => Some("Check"),
            Scene::T2SourceCheck => Some("Add source"),
            Scene::T2SourceKeyChanged => Some("Keep the old key"),
            Scene::T2SourceRemoveBuiltin => Some("Remove"),
            Scene::T2AppRemove => Some("Remove mk-pay"),
            Scene::T2Settings => Some("Save"),
            Scene::T2SettingsRolledBack => Some("Edit again"),
            _ => None,
        };
        for scene in Scene::ALL.into_iter().filter(|s| s.is_surface()) {
            assert!(!scene.clickable().is_empty(), "{}", scene.slug());
            let (app, _) = fixture(ScreenId::WebSites);
            let page = if scene.is_v3() { v3_page(&app, &t, scene, 120, 36) } else { tui2_page(&t, scene, 120, 36) };
            assert_eq!(page.footer.len(), 1, "{}: the footer is only the keys", scene.slug());
            if let Some(label) = main(scene) {
                for (w, h) in [(120u16, 36u16), (80, 24)] {
                    let b = render(scene, w, h, &t);
                    let text: String = b.content().iter().map(|c| c.symbol()).collect();
                    assert!(text.contains(label), "{} at {w}x{h}: \"{label}\" is off screen", scene.slug());
                }
            }
        }
    }

    #[test]
    fn v3_surfaces_are_rectangles_at_both_sizes() {
        // Every row of a card is filled from the same first to the same last column: no steps.
        let t = Theme::truecolor();
        for scene in Scene::ALL.into_iter().filter(|s| s.is_surface()) {
            for (w, h) in [(120u16, 36u16), (80, 24)] {
                let b = render(scene, w, h, &t);
                let mut edges = std::collections::BTreeSet::new();
                for y in 0..h {
                    let xs: Vec<u16> = (0..w).filter(|&x| b[(x, y)].bg == S.card).collect();
                    if let (Some(first), Some(last)) = (xs.first(), xs.last()) {
                        edges.insert((*first, *last));
                    }
                }
                assert!(edges.len() <= 1, "{} at {w}x{h}: card rows span different columns {edges:?}", scene.slug());
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
