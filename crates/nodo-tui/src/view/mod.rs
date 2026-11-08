//! Rendering. Each screen builds a `Page` from the state, and `layout` fits it to the terminal.

mod backup;
mod certs;
mod home;
mod install;
pub mod layout;
mod packages;
mod settings;
mod setup;
mod feeds;
mod updates;
mod web;

use nodo_api::ServiceState;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::app::{App, Modal, NoticeKind, Overlay, Screen, Tab, MIN_HEIGHT, MIN_WIDTH};
use crate::model::Area;
use crate::theme::Theme;
use layout::{draw_boxed, draw_page, FitReport, Item};

pub fn render(frame: &mut Frame, app: &App, theme: &Theme) -> FitReport {
    let area = frame.area();
    frame.buffer_mut().set_style(area, theme.base());
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area, theme);
        return FitReport::default();
    }
    let t = theme;
    let mut page = match &app.screen {
        Screen::Home { selected } => home::home(app, t, *selected),
        Screen::NoDrive { choice } => home::no_drive(app, t, *choice),
        Screen::Search(state) => packages::search(app, t, state),
        Screen::Detail(detail) => packages::detail(t, detail),
        Screen::Rejected(data) => packages::rejected(t, data),
        Screen::Capability(state) => packages::capability(t, state),
        Screen::Review(state) => install::review(t, state),
        Screen::Installing(snapshot) => install::installing(t, snapshot),
        Screen::Unlock(state) => install::unlock(app, t, state),
        Screen::Feeds { selected } => feeds::feeds(app, t, *selected),
        Screen::AddFeed(state) => feeds::add_feed(t, state),
        Screen::FeedKey(state) => feeds::feed_key(t, state),
        Screen::Settings(state) => settings::settings(t, state),
        Screen::Reference(state) => settings::reference(t, state),
        Screen::ApplyFailed(state) => settings::apply_failed(t, state),
        Screen::Updates(state) => updates::updates(app, t, state),
        Screen::AutoUpdate(state) => updates::auto_update(t, state),
        Screen::UpdateResults(state) => updates::results(t, state),
        Screen::Logs(state) => updates::logs(app, t, state),
        Screen::Web { selected } => web::sites(app, t, *selected),
        Screen::WebExposure(state) => web::exposure(app, t, state),
        Screen::WebProviders(state) => web::providers(app, t, state),
        Screen::WebCheck(state) => web::check(t, state),
        Screen::WebConfirm(state) => web::confirm(app, t, state),
        Screen::WebCgnat(state) => web::cgnat(t, state),
        Screen::WebDomain(state) => certs::domain(app, t, state),
        Screen::WebCertificate(state) => certs::certificate(app, t, state),
        Screen::WebTrust => certs::trust(app, t),
        Screen::Setup(state) => setup::setup(t, state),
        Screen::Backup(state) => backup::backup(t, state),
    };
    if let Some(notice) = &app.notice {
        let (symbol, span): (&str, fn(&Theme, String) -> Span<'static>) = match notice.kind {
            NoticeKind::Info => ("▸", |t, text| t.i(text)),
            NoticeKind::Ok => ("✓", |t, text| t.o(text)),
            NoticeKind::Error => ("✕", |t, text| t.e(text)),
        };
        page.footer.insert(0, Item::Text(Line::from(span(t, format!("{symbol} {}", notice.text)))));
    }
    if app.working {
        page.footer.insert(0, Item::Text(Line::from(t.d("⧗ Working…"))));
    }
    let area_shown = match &app.screen {
        Screen::Search(_) => Some(Area::Catalog),
        Screen::Feeds { .. } | Screen::AddFeed(_) => Some(Area::Feeds),
        Screen::Updates(_) => Some(Area::Updates),
        Screen::AutoUpdate(_) => Some(Area::AutoUpdate),
        Screen::WebProviders(_) => Some(Area::Providers),
        Screen::WebCheck(_) => Some(Area::Reachability),
        _ => None,
    };
    if let Some(reason) = area_shown.and_then(|area| app.data.unavailable(area)) {
        page.footer.insert(0, Item::Text(Line::from(t.e(format!("▲ Not available yet: {reason}")))));
    }
    let report = draw_page(frame, area, t, page);
    if let Some(overlay) = &app.overlay {
        draw_overlay(frame, area, t, overlay);
    }
    if let Some(modal) = &app.modal {
        draw_modal(frame, area, t, modal);
    }
    report
}

fn draw_too_small(frame: &mut Frame, area: Rect, t: &Theme) {
    let lines = [
        format!("Terminal too small: {}x{}.", area.width, area.height),
        format!("Needs at least {MIN_WIDTH}x{MIN_HEIGHT}."),
        "Keys are off until it is larger. q quits.".to_owned(),
    ];
    for (index, line) in lines.iter().enumerate() {
        if (index as u16) < area.height {
            frame.buffer_mut().set_stringn(
                area.x,
                area.y + index as u16,
                line,
                area.width as usize,
                t.base(),
            );
        }
    }
}

fn draw_overlay(frame: &mut Frame, area: Rect, t: &Theme, overlay: &Overlay) {
    let mut items: Vec<Item> = overlay
        .doc
        .lines
        .iter()
        .skip(overlay.scroll)
        .map(|line| Item::Text(Line::from(t.n(line.clone()))))
        .collect();
    items.push(Item::Blank);
    items.push(Item::Text(hints(t, &[("↑↓", "scroll"), ("Esc", "close")])));
    draw_boxed(frame, area, t, &overlay.doc.title, false, items);
}

fn draw_modal(frame: &mut Frame, area: Rect, t: &Theme, modal: &Modal) {
    let mut items: Vec<Item> =
        modal.body.iter().map(|line| Item::Text(Line::from(t.n(line.clone())))).collect();
    items.push(Item::Blank);
    let buttons = button_line(t, "Cancel", &modal.confirm_label, modal.confirm_focused);
    items.push(Item::Text(buttons));
    items.push(Item::Text(hints(t, &[("←→", "choose"), ("↵", "confirm"), ("Esc", "cancel")])));
    draw_boxed(frame, area, t, &modal.title, true, items);
}

/// A row's text with its leading space turned into `▸` when selected, so selection survives on a
/// terminal without colour.
pub(crate) fn mark(selected: bool, text: &str) -> String {
    match (selected, text.strip_prefix(' ')) {
        (true, Some(rest)) => format!("▸{rest}"),
        _ => text.to_owned(),
    }
}

/// The two-button choice. The safe button comes first and keeps focus until the user moves.
pub(crate) fn button_line(t: &Theme, safe: &str, risky: &str, risky_focused: bool) -> Line<'static> {
    let button = |label: &str, focused: bool| {
        if focused {
            t.s(format!("▸{label} "))
        } else {
            t.n(format!(" {label} "))
        }
    };
    Line::from(vec![button(safe, !risky_focused), t.n("  "), button(risky, risky_focused)])
}

pub(crate) fn hints(t: &Theme, keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(keys.len() * 3);
    for (index, (key, label)) in keys.iter().enumerate() {
        if index > 0 {
            spans.push(t.d("  "));
        }
        spans.push(t.a((*key).to_owned()));
        spans.push(t.d(format!(" {label}")));
    }
    Line::from(spans)
}

/// Text with `backtick`-quoted keys in the accent colour. The data layer uses this to write
/// suggestions without knowing about styles.
pub(crate) fn keyed(t: &Theme, text: &str) -> Line<'static> {
    let spans = text
        .split('`')
        .enumerate()
        .map(|(index, part)| {
            if index % 2 == 1 {
                t.a(part.to_owned())
            } else {
                t.n(part.to_owned())
            }
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

pub(crate) fn tab_bar(app: &App, t: &Theme, active: Tab) -> Line<'static> {
    let mut spans = Vec::new();
    let pending = app.pending_updates();
    for (index, tab) in Tab::ALL.into_iter().enumerate() {
        if index > 0 {
            spans.push(t.n("  "));
        }
        let count = (tab == Tab::Updates && pending > 0).then(|| format!("({pending})"));
        if tab == active {
            let label = match &count {
                Some(count) => format!(" {} {count} ", tab.label()),
                None => format!(" {} ", tab.label()),
            };
            spans.push(t.s(label));
        } else {
            spans.push(t.n(tab.label()));
            if let Some(count) = count {
                spans.push(t.n(" "));
                spans.push(t.a(count));
            }
        }
    }
    Line::from(spans)
}

pub(crate) fn service_symbol(t: &Theme, state: ServiceState) -> (Span<'static>, Span<'static>) {
    match state {
        ServiceState::Running => (t.o("●"), t.n("running")),
        ServiceState::Stopped => (t.d("○"), t.d("stopped")),
        ServiceState::Failed => (t.e("✕"), t.e("failed")),
        ServiceState::Locked => (t.a("◆"), t.a("locked")),
        ServiceState::Waiting => (t.d("◌"), t.d("waiting")),
    }
}

/// Pads to a display width, for the columns that the design aligns with spaces.
pub(crate) fn pad(text: &str, width: usize) -> String {
    let used = unicode_width::UnicodeWidthStr::width(text);
    format!("{text}{}", " ".repeat(width.saturating_sub(used)))
}

/// Checkbox text, so a tick is visible without colour.
pub(crate) fn checkbox(on: bool) -> &'static str {
    if on {
        "[x]"
    } else {
        "[ ]"
    }
}
