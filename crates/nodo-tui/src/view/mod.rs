//! Rendering. Each screen builds a `Page` from the state, and `layout` fits it to the terminal.

mod apps;
mod backup;
mod certs;
mod home;
mod more;
mod network;
pub mod layout;
mod settings;
mod setup;
mod sources;
mod system;
pub mod v3;
mod web;

pub use apps::empty_catalog_line;

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crossterm::event::KeyCode;

use crate::app::{App, Modal, NoticeKind, Overlay, Screen, Tab, MIN_HEIGHT, MIN_WIDTH};
use crate::hit::{self, Click, Target};
use crate::model::Area;
use crate::theme::Theme;
use layout::{draw_boxed, draw_page, FitReport, Item};

thread_local! {
    static FRAME_SIZE: std::cell::Cell<(u16, u16)> = const { std::cell::Cell::new((100, 32)) };
}

/// The size of the frame `render` is drawing.
pub(crate) fn frame_size() -> (u16, u16) {
    FRAME_SIZE.with(std::cell::Cell::get)
}

thread_local! {
    static PENDING: std::cell::RefCell<Option<(Click, u64)>> = const { std::cell::RefCell::new(None) };
}

/// The click of the button whose work is still running on the screen `render` is drawing, with
/// the tick count for its spinner (`App::pending`).
pub(crate) fn pending() -> Option<(Click, u64)> {
    PENDING.with(|pending| pending.borrow().clone())
}

pub fn render(frame: &mut Frame, app: &App, theme: &Theme) -> FitReport {
    let area = frame.area();
    FRAME_SIZE.with(|size| size.set((area.width, area.height)));
    let pending = app.pending.as_ref().filter(|p| p.screen == app.screen.id() && app.modal.is_none());
    PENDING.with(|cell| *cell.borrow_mut() = pending.map(|p| (p.click.clone(), app.ticks)));
    frame.buffer_mut().set_style(area, theme.base());
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area, theme);
        app.hits.borrow_mut().clear();
        return FitReport::default();
    }
    let t = theme;
    let mut page = match &app.screen {
        Screen::Home { selected } => home::home(app, t, *selected),
        Screen::NoDrive { choice } => home::no_drive(app, t, *choice),
        Screen::Apps { selected } => apps::apps(app, t, *selected),
        Screen::AppPage(state) => apps::app_page(app, t, state),
        Screen::AppRemove(state) => apps::app_remove(app, t, state),
        Screen::Search(state) => apps::search(app, t, state),
        Screen::Detail(detail) => apps::detail(app, t, detail),
        Screen::Rejected(data) => apps::rejected(app, t, data),
        Screen::Capability(state) => apps::capability(app, t, state),
        Screen::Review(state) => apps::review(app, t, state),
        Screen::Installing(snapshot) => apps::installing(app, t, snapshot),
        Screen::Unlock(state) => apps::unlock(app, t, state),
        Screen::Feeds { selected, keys } => sources::sources(app, t, *selected, *keys),
        Screen::AddFeed(state) => sources::add_feed(app, t, state),
        Screen::FeedKey(state) => sources::feed_key(app, t, state),
        Screen::Settings(state) => settings::settings(app, t, state),
        Screen::Reference(state) => settings::reference(app, t, state),
        Screen::ApplyFailed(state) => settings::apply_failed(app, t, state),
        Screen::System { selected } => system::system(app, t, *selected),
        Screen::Updates(state) => system::updates(app, t, state),
        Screen::AutoUpdate(state) => system::auto_update(app, t, state),
        Screen::UpdateResults(state) => system::results(app, t, state),
        Screen::Logs(state) => system::logs(app, t, state),
        Screen::Web { selected } => web::sites(app, t, *selected),
        Screen::WebExposure(state) => web::exposure(app, t, state),
        Screen::WebProviders(state) => web::providers(app, t, state),
        Screen::WebCheck(state) => web::check(app, t, state),
        Screen::WebConfirm(state) => web::confirm(app, t, state),
        Screen::WebCgnat(state) => web::cgnat(app, t, state),
        Screen::WebDomain(state) => v3::modern(app, t, certs::domain(app, t, state)),
        Screen::WebCertificate(state) => web::online(app, t, state),
        Screen::WebTrust => v3::modern(app, t, certs::trust(app, t)),
        Screen::Setup(state) => v3::modern(app, t, setup::setup(t, state)),
        Screen::Backup(state) => v3::modern(app, t, backup::backup(t, state)),
        Screen::Onion(state) => v3::modern(app, t, more::onion(t, state)),
        Screen::Restore(state) => v3::modern(app, t, more::restore(app, t, state)),
        Screen::Rollbacks(state) => v3::modern(app, t, more::rollbacks(t, state)),
        Screen::Network(state) => v3::modern(app, t, network::network(t, state)),
    };
    if let Some(notice) = &app.notice {
        let (symbol, span): (&str, fn(&Theme, String) -> Span<'static>) = match notice.kind {
            NoticeKind::Info => ("▸", |t, text| t.i(text)),
            NoticeKind::Ok => ("✓", |t, text| t.o(text)),
            NoticeKind::Error => ("✕", |t, text| t.e(text)),
        };
        page.footer.insert(0, Item::Text(Line::from(span(t, format!("{symbol} {}", notice.text)))));
    }
    // The busy mark goes in front of the header's right-hand text instead of taking a footer row:
    // requests run often (following a log polls constantly), and a row that comes and goes made
    // the whole page jump. The right-hand text is right-aligned, so what was there stays put.
    if app.working {
        let mut spans = vec![t.d("⧗ Working…"), Span::raw("   ")];
        spans.append(&mut page.right.spans);
        page.right = Line::from(spans);
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
    let v3_screen = true;
    let footer_rows = page.footer.len() as u16;
    let mut targets = std::mem::take(&mut page.targets);
    if !v3_screen {
        if let Some(tab) = app.screen.tab() {
            let _ = tab;
            let mut tabs: Vec<Target> = Tab::ALL.into_iter().map(|tab| Target::text(tab.label(), Click::Tab(tab))).collect();
            tabs.append(&mut targets);
            targets = tabs;
        }
    }
    let report = draw_page(frame, area, t, page);
    if v3_screen {
        v3::paint_strips(frame.buffer_mut(), area, t, footer_rows);
    } else if app.screen.tab().is_some() {
        v3::paint_strips(frame.buffer_mut(), Rect::new(area.x, area.y, area.width, 1), t, 0);
    }
    let mut hits = hit::resolve(frame.buffer_mut(), area, &targets);
    if let Some(overlay) = &app.overlay {
        draw_overlay(frame, area, t, overlay);
        hits.clear();
    }
    if let Some(modal) = &app.modal {
        draw_modal(frame, area, t, modal);
        let modal_targets = [
            Target::text(modal.title.clone(), Click::Key(KeyCode::Null)),
            Target::button("Cancel", Click::Pick { index: 0, then: Some(KeyCode::Enter) }),
            Target::button(modal.confirm_label.clone(), Click::Pick { index: 1, then: Some(KeyCode::Enter) }),
        ];
        hits = hit::resolve(frame.buffer_mut(), area, &modal_targets);
        hits.retain(|hit| hit.click != Click::Key(KeyCode::Null));
    }
    hit::paint_hover(frame.buffer_mut(), &hits, app.hover, t);
    *app.hits.borrow_mut() = hits;
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
