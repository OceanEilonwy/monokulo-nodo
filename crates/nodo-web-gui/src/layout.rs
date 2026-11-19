//! The frame every page is drawn in, and the pieces pages are made of.
//!
//! Everything is plain HTML that works without scripts: real links, forms and buttons. htmx adds
//! three things where it is loaded, and nothing depends on them: a question appears as a dialog
//! over the page instead of on a page of its own, a page that waits asks again by itself, and a
//! search narrows as it is typed. `assets/app.js` gives that dialog the keyboard's focus.
//!
//! The content security policy allows no inline script or style, so there is no `style=`
//! attribute and no `on…=` attribute anywhere; every look is a class of `assets/app.css`.

use axum::http::StatusCode;
use maud::{html, Markup, DOCTYPE};
use nodo_ui_core::flows::{Notice, NoticeKind};
use nodo_ui_core::model::Document;
use nodo_ui_core::screen::Tab;

use crate::assets;
use nodo_ui_core::words::fingerprint_groups;
use crate::dest;
use crate::session::{JobView, Pending};

/// One page: what goes inside the frame.
pub struct Page {
    /// For the browser's tab and history.
    pub title: String,
    /// The tab it sits under, shown as the current one. `None` for pages that belong to none.
    pub tab: Option<Tab>,
    /// The way here from the tab: each a name and its URL, the last one this page.
    pub trail: Vec<(String, String)>,
    pub body: Markup,
    pub status: StatusCode,
    /// The page waits for something and, without scripts, loads itself again.
    pub waiting: bool,
    /// The page's title and what it is for, drawn above its tab strip. A page without one has
    /// its title at the top of `body`.
    pub head: Option<Markup>,
    /// The tab strip under the title: the pages of a tab (`Page::sub`) or of an app.
    pub strip: Option<Markup>,
}

impl Page {
    pub fn new(tab: Tab, title: impl Into<String>, body: Markup) -> Self {
        Self { title: title.into(), tab: Some(tab), trail: Vec::new(), body, status: StatusCode::OK, waiting: false, head: None, strip: None }
    }

    /// A page under no tab.
    pub fn plain(title: impl Into<String>, body: Markup) -> Self {
        Self { title: title.into(), tab: None, trail: Vec::new(), body, status: StatusCode::OK, waiting: false, head: None, strip: None }
    }

    /// The title and what the page is for, above the tab strip.
    pub fn head(mut self, head: Markup) -> Self {
        self.head = Some(head);
        self
    }

    /// One of its tab's own pages: the tab strip of its tab is shown, with this one current.
    pub fn sub(mut self, sub: Sub) -> Self {
        self.strip = self.tab.map(|tab| section_strip(tab, sub));
        self
    }

    /// A tab strip of the page's own, such as an app's.
    pub fn strip(mut self, strip: Markup) -> Self {
        self.strip = Some(strip);
        self
    }

    /// The breadcrumbs: names and URLs from the tab down to this page.
    pub fn trail(mut self, crumbs: &[(&str, &str)]) -> Self {
        self.trail = crumbs.iter().map(|(name, url)| ((*name).to_owned(), (*url).to_owned())).collect();
        self
    }

    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    pub fn waiting(mut self) -> Self {
        self.waiting = true;
        self
    }
}

/// What the frame shows around a page.
pub struct Chrome {
    /// Tabs and the way out are shown only to someone logged in.
    pub signed_in: bool,
    /// The counts beside Apps and System.
    pub badges: (usize, usize),
    /// Changes kept with Save and not applied yet, linked to the list of them.
    pub unsaved: usize,
    pub notices: Vec<Notice>,
}

impl Chrome {
    /// The frame of the login page: no tabs, nothing about the device.
    pub fn signed_out() -> Self {
        Self { signed_in: false, badges: (0, 0), unsaved: 0, notices: Vec::new() }
    }
}

/// A page of a tab's own: an entry of the strip under the title of every page of that tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sub {
    Installed,
    GetApps,
    Sources,
    Sites,
    Providers,
    Domain,
    Certificates,
    Trust,
    Onion,
    ThisNodo,
    Updates,
    Backups,
    Network,
    Keys,
    Drive,
    Logs,
    About,
}

impl Sub {
    pub fn label(self) -> &'static str {
        match self {
            Sub::Installed => "Installed",
            Sub::GetApps => "Get apps",
            Sub::Sources => "Sources",
            Sub::Sites => "Sites online",
            Sub::Providers => "Domain providers",
            Sub::Domain => "Certificates and home names",
            Sub::Certificates => "Certificates",
            Sub::Trust => "Local CA",
            Sub::Onion => "Onion addresses",
            Sub::ThisNodo => "This Nodo",
            Sub::Updates => "Updates",
            Sub::Backups => "Backups",
            Sub::Network => "Network",
            Sub::Keys => "SSH keys",
            Sub::Drive => "Data drive",
            Sub::Logs => "Device logs",
            Sub::About => "About",
        }
    }

    pub fn url(self) -> &'static str {
        match self {
            Sub::Installed => dest::APPS,
            Sub::GetApps => dest::GET_APPS,
            Sub::Sources => dest::SOURCES,
            Sub::Sites => dest::WEB,
            Sub::Providers => dest::PROVIDERS,
            Sub::Domain => dest::DOMAIN,
            Sub::Certificates => dest::CERTIFICATES,
            Sub::Trust => dest::TRUST,
            Sub::Onion => dest::ONION,
            Sub::ThisNodo => dest::SYSTEM,
            Sub::Updates => dest::UPDATES,
            Sub::Backups => dest::BACKUPS,
            Sub::Network => dest::NETWORK,
            Sub::Keys => dest::KEYS,
            Sub::Drive => dest::DRIVE,
            Sub::Logs => dest::LOGS,
            Sub::About => dest::ABOUT,
        }
    }
}

/// The pages of each tab, in the order of its strip. Onion addresses are the Web tab's page,
/// and System lists them too, as the terminal does.
fn subs_of(tab: Tab) -> &'static [Sub] {
    match tab {
        Tab::Home => &[],
        Tab::Apps => &[Sub::Installed, Sub::GetApps, Sub::Sources],
        Tab::Web => &[Sub::Sites, Sub::Providers, Sub::Domain, Sub::Certificates, Sub::Trust, Sub::Onion],
        Tab::System => &[Sub::ThisNodo, Sub::Updates, Sub::Backups, Sub::Network, Sub::Keys, Sub::Drive, Sub::Onion, Sub::Logs, Sub::About],
    }
}

/// A strip of tabs under a page's title: each a name, its URL and whether it is this page.
pub fn strip(label: &str, entries: &[(String, String, bool)]) -> Markup {
    html! {
        nav.tabs aria-label=(label) {
            @for (name, url, on) in entries {
                a href=(url) aria-current=[on.then_some("page")] { (name) }
            }
        }
    }
}

/// The strip of a tab's own pages, with `on` current.
fn section_strip(tab: Tab, on: Sub) -> Markup {
    let entries: Vec<(String, String, bool)> = subs_of(tab).iter().map(|sub| (sub.label().to_owned(), sub.url().to_owned(), *sub == on)).collect();
    strip(tab.label(), &entries)
}

/// The title of a page and the line that says what it is for.
pub fn heading(title: &str, desc: Option<&str>) -> Markup {
    html! {
        h1 { (title) }
        @if let Some(desc) = desc { p.desc { (desc) } }
    }
}

/// A coloured label for a state: `kind` is `ok`, `warn`, `bad`, `off` or `info`.
pub fn label(kind: &str, text: &str) -> Markup {
    html! { span class=(format!("lbl {kind}")) { (text) } }
}

fn tab_url(tab: Tab) -> &'static str {
    match tab {
        Tab::Home => dest::HOME,
        Tab::Apps => dest::APPS,
        Tab::Web => dest::WEB,
        Tab::System => dest::SYSTEM,
    }
}

fn head(title: &str, waiting: bool) -> Markup {
    html! {
        head {
            meta charset="utf-8";
            meta name="viewport" content="width=device-width, initial-scale=1";
            meta name="color-scheme" content="light dark";
            // htmx would add its indicator styles through a stylesheet of its own making. The
            // classes are in app.css instead, so nothing about the look comes from a script.
            meta name="htmx-config" content=r#"{"includeIndicatorCSS":false}"#;
            title { (title) " · Nodo" }
            // The logo, so browsers do not ask for /favicon.ico on every page.
            link rel="icon" href=(assets::logo_path()) type="image/svg+xml";
            link rel="stylesheet" href=(assets::css_path());
            script src=(assets::htmx_path()) defer {}
            script src=(assets::script_path()) defer {}
            @if waiting {
                // Without scripts the page loads itself again. With htmx the fragment asks.
                noscript { meta http-equiv="refresh" content="2"; }
            }
        }
    }
}

/// A whole HTML document: the frame with `page` inside.
pub fn document(page: &Page, chrome: &Chrome) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            (head(&page.title, page.waiting))
            body {
                @if chrome.signed_in { a.skip href="#main" { "Skip to the page" } }
                header.hdr {
                    a.brand href=(dest::HOME) { img src=(assets::logo_path()) alt="" width="24" height="24"; "Nodo" }
                    @if chrome.signed_in {
                        nav.nav aria-label="Sections" {
                            @for tab in Tab::ALL {
                                @let count = match tab { Tab::Apps => chrome.badges.0, Tab::System => chrome.badges.1, _ => 0 };
                                a href=(tab_url(tab)) aria-current=[(page.tab == Some(tab)).then_some("page")] {
                                    (tab.label())
                                    @if count > 0 { span.cnt title="Updates ready" { (count) } }
                                }
                            }
                        }
                        @if chrome.unsaved > 0 {
                            a.unsaved href=(dest::CHANGES) {
                                "Unsaved changes " span.cnt { (chrome.unsaved) }
                            }
                        }
                        form.out method="post" action=(dest::LOGOUT) {
                            button type="submit" { "Log out" }
                        }
                    }
                }
                main #main class=[(!chrome.signed_in).then_some("narrow")] {
                    @if !page.trail.is_empty() {
                        nav.crumb aria-label="Where this page is" {
                            @for (index, (name, url)) in page.trail.iter().enumerate() {
                                @if index > 0 { span.sep aria-hidden="true" { "›" } }
                                @if index + 1 == page.trail.len() || url.is_empty() {
                                    span aria-current=[(index + 1 == page.trail.len()).then_some("page")] { (name) }
                                } @else {
                                    a href=(url) { (name) }
                                }
                            }
                        }
                    } @else if let (true, Some(tab), Some(_)) = (chrome.signed_in, page.tab, &page.head) {
                        div.crumb { (tab.label()) }
                    }
                    @if let Some(head) = &page.head { div.pagehead { (head) } }
                    @if let Some(strip) = &page.strip { (strip) }
                    @if !chrome.notices.is_empty() {
                        div.notices role="status" {
                            @for each in &chrome.notices { (notice(each)) }
                        }
                    }
                    (page.body)
                }
                @if chrome.signed_in { footer.ftr { "Nodo web GUI" } }
                // Where a question is put by htmx, over the page.
                div #dialog {}
            }
        }
    }
}

/// One line about what just happened.
pub fn notice(notice: &Notice) -> Markup {
    let (class, sign) = match notice.kind {
        NoticeKind::Info => ("notice", "▸"),
        NoticeKind::Ok => ("notice ok", "✓"),
        NoticeKind::Error => ("notice error", "✕"),
    };
    html! { p class=(class) { span.sign aria-hidden="true" { (sign) } span { (notice.text) (crate::hints::linked(notice.hint)) } } }
}

/// A form of one button that posts to `action`. `class` is the button's look: `"btn"`,
/// `"btn primary"`, `"btn danger"`, `"btn quiet"`.
pub fn post(action: &str, label: &str, class: &str) -> Markup {
    html! { form method="post" action=(action) { button class=(class) type="submit" { (label) } } }
}

/// The count of changes inside a button, hidden when there are none. The page says the count
/// in words elsewhere, so it is not read out.
pub fn count_badge(changes: usize) -> Markup {
    html! { span.bcnt aria-hidden="true" hidden[changes == 0] { (changes) } }
}

/// The three buttons of a page whose draft can be kept aside (`flows::staged`), inside its
/// form: Reset (`do=reset`) drops the draft, Save (`do=keep`) keeps it here without touching
/// the Nodo, and the apply button (`do=save`) is the page's own save, which checks the draft
/// and puts it live. `keep_count` and `apply_count` go inside Save and the apply button.
pub fn stage_buttons(apply: &str, keep_count: Markup, apply_count: Markup) -> Markup {
    html! {
        button.btn type="submit" name="do" value="reset" { "Reset" }
        button.btn type="submit" name="do" value="keep" { "Save" (keep_count) }
        button.btn.primary type="submit" name="do" value="save" { (apply) (apply_count) }
    }
}

/// What the three buttons of [`stage_buttons`] do, in a line under them.
pub fn stage_hint() -> Markup {
    html! { p.hint.small { "Save keeps your changes here, without touching the Nodo. Save & Apply checks them and puts them live." } }
}

/// A button whose action asks first. With htmx the question appears as a dialog over the page;
/// without it the same post leads to the question's own page.
pub fn ask(action: &str, label: &str, class: &str) -> Markup {
    html! {
        form method="post" action=(action) hx-post=(action) hx-target="#dialog" hx-swap="innerHTML" {
            button class=(class) type="submit" { (label) }
        }
    }
}

/// A hidden field of a form.
pub fn hidden(name: &str, value: &str) -> Markup {
    html! { input type="hidden" name=(name) value=(value); }
}

/// A row of a list that opens a page: its name, a few words, and something at the end.
pub fn row(href: &str, name: &str, what: &str, end: Markup) -> Markup {
    html! {
        a.row href=(href) {
            span.name { (name) }
            span.what { (what) }
            span.end { (end) }
            span.go aria-hidden="true" { "›" }
        }
    }
}

/// A row for a page that is not built yet. It says so and leads nowhere.
pub fn coming(name: &str, what: &str) -> Markup {
    html! {
        div.row.off {
            span.name { (name) }
            span.what { (what) }
            span.end { span.tag { "From the terminal for now" } }
            span.go {}
        }
    }
}

/// A key's fingerprint in groups. `check` marks the one the owner is asked to compare.
pub fn fingerprint(fp: &str, check: bool) -> Markup {
    html! {
        span class=(if check { "fp check" } else { "fp" }) {
            @for group in fingerprint_groups(fp).split_whitespace() { span { (group) } }
        }
    }
}

/// The passphrase box. `current-password` lets a password manager fill the device passphrase.
pub fn passphrase(id: &str, label: &str, problem: Option<&str>, focus: bool) -> Markup {
    html! {
        div.field {
            label for=(id) { (label) }
            input type="password" id=(id) name="passphrase" autocomplete="current-password" required
                autofocus[focus] aria-invalid=[problem.map(|_| "true")] aria-describedby=[problem.map(|_| format!("{id}-problem"))];
            @if let Some(problem) = problem {
                p.problem id=(format!("{id}-problem")) { "✕ " (problem) }
            }
        }
    }
}

/// The question before something that cannot be taken back, with its two answers. Cancel comes
/// first, so it is what Tab reaches first and a stray Enter in the page confirms nothing.
pub fn question(pending: &Pending) -> Markup {
    let question = &pending.question;
    html! {
        h2 #question-title { (question.title) }
        @for (index, line) in question.body.iter().enumerate() {
            p class=[(index > 0).then_some("dim")] { (line) }
        }
        div.actions.end {
            a.btn href=(pending.cancel) autofocus[question.cancel_is_default] { "Cancel" }
            form method="post" action=(dest::CONFIRM) {
                (hidden("id", &pending.id.to_string()))
                button.btn.danger type="submit" autofocus[!question.cancel_is_default] { (question.confirm_label) }
            }
        }
    }
}

/// The question as a dialog over the page, for htmx to put into `#dialog`.
pub fn dialog(pending: &Pending) -> Markup {
    html! {
        div.backdrop {
            div.dialog role="alertdialog" aria-modal="true" aria-labelledby="question-title" {
                (question(pending))
            }
        }
    }
}

/// What a page shows while a job runs: what, for how long, and that it will go on by itself.
/// With htmx it asks `/working` again every second and is replaced by the answer; `to` is the
/// page to return to when the job is over, for a page that was only waiting for the backend.
pub fn working(job: &JobView, to: Option<&str>) -> Markup {
    let poll = match to {
        Some(to) => format!("{}?to={}", dest::WORKING, dest::seg(to)),
        None => dest::WORKING.to_owned(),
    };
    html! {
        section.card.working #working hx-get=(poll) hx-trigger="every 1s" hx-swap="outerHTML" {
            div.spinner aria-hidden="true" {}
            h1 { (job.what) }
            p role="status" { "Working for " (nodo_ui_core::model::counted(job.seconds, "second")) "." }
            @for line in &job.note { p.dim { (line) } }
            p.dim.small { "This page moves on by itself when it is done." }
        }
    }
}

/// A text the device wrote, line for line.
pub fn document_text(document: &Document) -> Markup {
    html! { pre.text { (document.lines.join("\n")) } }
}
