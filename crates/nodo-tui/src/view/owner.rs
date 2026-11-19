//! Installing a bundle of apps, and an app's setup page for the owner. Both are long, so their
//! content scrolls inside the page while the buttons and keys stay put.

use crossterm::event::KeyCode;
use nodo_api::{OwnerCheck, OwnerCheckState, OwnerItem, OwnerStepState, StepState};
use ratatui::style::{Color, Style};
use ratatui::text::Span;

use super::layout::{Item, Page};
use super::pad;
use super::v3::{focused, inset, keys_line, line, margin, rows_of, tick, trail, Card, Screen3, STRIP_ROWS};
use crate::app::{App, BundleState, BundleStep, OwnerPageState};
use nodo_ui_core::flows::documents::copy_targets;
use crate::hit::{Click, Target};
use crate::theme::Theme;
use nodo_ui_core::screen::Tab;

fn pick(index: usize, then: KeyCode) -> Click {
    Click::Pick { index, then: Some(then) }
}

/// Card rows built a piece at a time, remembering which rows each click target is drawn on, so a
/// window of them declares only the targets it shows.
struct Rows<'a> {
    t: &'a Theme,
    w: u16,
    items: Vec<Item>,
    /// First row, row after the last, and the targets drawn on those rows.
    pieces: Vec<(usize, usize, Vec<Target>)>,
    /// The row the keyboard focus is on.
    focus: Option<usize>,
}

impl<'a> Rows<'a> {
    fn new(t: &'a Theme, w: u16) -> Self {
        Self { t, w, items: Vec::new(), pieces: Vec::new(), focus: None }
    }

    /// Rows drawn on a card of their own, with the targets tied to them (`Card::row_with`), so
    /// each is found only on its own row's text.
    fn piece(&mut self, build: impl FnOnce(&mut Card, &Theme)) {
        let mut card = Card::new(self.t, self.w);
        build(&mut card, self.t);
        let (items, targets) = card.into_parts();
        let start = self.items.len();
        self.items.extend(items);
        self.pieces.push((start, self.items.len(), targets.into_iter().map(|(_, target)| target).collect()));
    }

    fn row(&mut self, spans: Vec<Span<'static>>) {
        self.piece(|card, t| card.row(t, spans));
    }

    fn blank(&mut self) {
        self.row(Vec::new());
    }

    fn focus_here(&mut self) {
        self.focus = Some(self.items.len());
    }

    fn inner(&self) -> usize {
        Card::new(self.t, self.w).inner()
    }
}

/// The page: what comes before, a window of `rows` from `start` (kept on the focus when
/// `follow`), a dim line when there is more, what comes after, and the keys.
#[allow(clippy::too_many_arguments)]
fn scrolled(t: &Theme, mut screen: Screen3, rows: Rows, start: usize, follow: bool, after: Vec<Item>, after_targets: Vec<Target>, keys: &str, w: u16, h: u16) -> Page {
    let fixed = usize::from(STRIP_ROWS) + screen.bottom_rows(t, keys, w) + rows_of(&screen.before, w) + rows_of(&after, w);
    let total = rows.items.len();
    let mut avail = usize::from(h).saturating_sub(fixed);
    let mut start = start;
    let mut hint = None;
    if total > avail {
        avail = avail.saturating_sub(2).max(1);
        if follow {
            if let Some(focus) = rows.focus {
                start = start.min(focus).max((focus + 2).saturating_sub(avail));
            }
        }
        start = start.min(total - avail);
        let above = start > 0;
        let below = start + avail < total;
        let arrow = match (above, below) {
            (true, true) => "↕",
            (true, false) => "↑",
            _ => "↓",
        };
        hint = Some(line(vec![margin(t), t.d(format!("{arrow} more — scroll with ↑↓, PgUp and PgDn, or the wheel"))]));
    } else {
        start = 0;
        avail = total;
    }
    let end = start + avail;
    for (first, last, targets) in rows.pieces {
        if first >= start && last <= end {
            screen.targets.extend(targets);
        }
    }
    let mut items = std::mem::take(&mut screen.before);
    items.extend(rows.items.into_iter().skip(start).take(avail));
    if let Some(hint) = hint {
        items.push(hint);
        items.push(Item::Blank);
    }
    items.extend(after);
    let mut page = screen.page.plain(items).footer(keys_line(t, keys));
    page.targets = screen.targets;
    page.targets.extend(after_targets);
    page
}

fn step_mark(t: &Theme, state: StepState) -> Span<'static> {
    match state {
        StepState::Done => t.o("✓  "),
        StepState::Skipped => t.d("–  "),
        StepState::Failed => t.e("✕  "),
        StepState::Pending => t.d("○  "),
    }
}

/// A bundle: the apps it installs, the questions, the passphrase, the progress and the result.
pub fn bundle(app: &App, t: &Theme, state: &BundleState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let review = &state.review;
    let head = review.package.as_str();
    let mut before_targets = Vec::new();
    screen.before = vec![Item::Blank, trail(t, &["Apps", "Get apps", head, "Install"], &mut before_targets), Item::Blank];
    screen.targets.extend(before_targets);
    let mut rows = Rows::new(t, w);
    let mut after = Card::new(t, w);
    let installing = review.members.iter().filter(|m| m.installed.is_none()).count();
    let keys;
    match &state.step {
        BundleStep::Review => {
            rows.blank();
            rows.row(vec![t.b(format!("Install {head}?")), t.d(format!("   Installs {} apps", review.members.len()))]);
            rows.blank();
            rows.row(vec![t.a("▲  "), t.n(format!("They come from {}, a source you added. Nobody has reviewed their code for you.", review.feed))]);
            rows.blank();
            for member in &review.members {
                let name = member.review.package.clone();
                let version = member.review.pkgver.strip_prefix(&format!("{name}-")).unwrap_or(&member.review.pkgver).to_owned();
                match &member.installed {
                    Some(have) => rows.row(vec![t.d("–  "), t.b(name), t.d(format!("   {have} is installed already; it is kept as it is"))]),
                    None => {
                        rows.row(vec![t.o("●  "), t.b(name), t.d(format!("   version {version}"))]);
                        for permission in &member.review.permissions {
                            let mut text = permission.clone();
                            if let Some(first) = text.get(..1) {
                                text = first.to_uppercase() + &text[1..];
                            }
                            rows.row(vec![t.n("     "), t.d("·  "), t.n(text)]);
                        }
                        if member.review.encrypted_storage {
                            rows.row(vec![t.n("     "), t.a("◆  "), t.n("Keeps its data in encrypted storage"), t.d("   Locks at reboot")]);
                        }
                    }
                }
            }
            if !review.grants.is_empty() {
                rows.blank();
                rows.row(vec![t.n("They read each other's settings, which you approve now:")]);
                for grant in &review.grants {
                    rows.row(vec![t.a("▲  "), t.n(grant.text.clone())]);
                }
            }
            if !review.data_drive.is_empty() {
                rows.blank();
                let present = if review.data_drive_present { "" } else { "   The data drive is not connected" };
                rows.row(vec![t.n("Keep their data on the data drive:"), t.d(present)]);
                for (index, (id, on)) in review.data_drive.iter().zip(&state.drive).enumerate() {
                    let selected = state.focus == index;
                    if selected {
                        rows.focus_here();
                    }
                    let lead = if selected { t.a("▸ ") } else { t.n("  ") };
                    let label = id.clone();
                    rows.piece(|card, t| {
                        let target = Target::card_row(label.clone(), pick(index, KeyCode::Char(' ')));
                        card.row_with(t, vec![lead, tick(t, *on), t.n(format!(" {label}"))], [target]);
                    });
                }
            }
            rows.blank();
            for guarantee in ["Cannot read other packages' data or secrets", "Cannot run setup scripts as root or replace system packages"] {
                rows.row(vec![t.d(guarantee)]);
            }
            rows.blank();
            after.blank(t);
            if let Some(error) = &state.error {
                after.row(t, vec![t.e(format!("✕ {error}"))]);
                after.blank(t);
            }
            let cancel = state.cancel_index();
            let next = if state.open_asks().is_empty() { "Install" } else { "Continue" };
            let cancel_label = focused("Cancel", state.focus == cancel);
            let next_label = focused(next, state.focus == cancel + 1);
            after.buttons(t, &[(cancel_label.as_str(), false, pick(cancel, KeyCode::Enter)), (next_label.as_str(), true, pick(cancel + 1, KeyCode::Enter))]);
            keys = if state.focus == cancel + 1 {
                "Keys: ↵ continue  ↑↓ choose  space switch  PgUp PgDn scroll  Esc cancel"
            } else {
                "Keys: ↑↓ choose  space switch  ↵ on a button  PgUp PgDn scroll  Esc cancel"
            };
        }
        BundleStep::Answers => {
            rows.blank();
            rows.row(vec![t.b(format!("Set up {head}"))]);
            rows.blank();
            rows.row(vec![t.n("These are asked once, before anything installs. You can change them later in each app's Settings.")]);
            rows.blank();
            let open = state.open_asks();
            let room = rows.inner().saturating_sub(4).clamp(20, 60);
            for (position, &index) in open.iter().enumerate() {
                let ask = &review.asks[index];
                let input = &state.answers[index];
                let selected = state.focus == position;
                if selected {
                    rows.focus_here();
                }
                let lead = if selected { t.a("▸ ") } else { t.n("  ") };
                // The question's label picks it, as it did when found by its text alone.
                let (label, package) = (ask.label.clone(), ask.package.clone());
                rows.piece(|card, t| {
                    let target = Target::card_row(label.clone(), Click::Pick { index: position, then: None });
                    card.row_with(t, vec![lead, t.b(label), t.d(format!("   for {package}"))], [target]);
                });
                // A long paste, such as a key file, is shown by its length rather than overflowing.
                let count = input.secret_value().expose().chars().count();
                let shown = if ask.secret {
                    "•".repeat(count.min(room.saturating_sub(1)))
                } else {
                    let text = input.text();
                    let skip = text.chars().count().saturating_sub(room.saturating_sub(1));
                    text.chars().skip(skip).collect()
                };
                let mut field = vec![t.n("  ")];
                field.extend(inset(t, &shown, room, None, selected));
                if ask.secret && count > 0 {
                    field.push(t.d(format!("  {count} characters")));
                }
                rows.row(field);
                if !ask.help.is_empty() {
                    rows.row(vec![t.n("  "), t.d(ask.help.clone())]);
                }
                if ask.secret {
                    rows.row(vec![t.n("  "), t.d("Paste it whole; it is never shown, and line breaks in a pasted file are dropped.")]);
                }
                rows.blank();
            }
            after.blank(t);
            if let Some(error) = &state.error {
                after.row(t, vec![t.e(format!("✕ {error}"))]);
                after.blank(t);
            }
            let next = if review.needs_passphrase { "Continue" } else { "Install" };
            after.buttons(t, &[("Back", false, Click::Key(KeyCode::Esc)), (next, true, pick(open.len().saturating_sub(1), KeyCode::Enter))]);
            keys = "Keys: type or paste  Tab next  ↵ continue  Esc back";
        }
        BundleStep::Passphrase => {
            rows.blank();
            rows.row(vec![t.b(format!("Install {head} and {} more?", installing.saturating_sub(1)))]);
            rows.blank();
            rows.row(vec![t.n("Their locked storage is made now, and secret settings are sealed, with your device passphrase.")]);
            rows.blank();
            let mut pass = vec![t.d(pad("Passphrase", 13))];
            pass.extend(inset(t, &state.passphrase.display(), 30, None, true));
            rows.row(pass);
            if let Some(error) = &state.error {
                rows.row(vec![t.n(" ".repeat(13)), t.e(format!("✕ {error}"))]);
            }
            rows.blank();
            after.blank(t);
            after.buttons(t, &[("Back", false, Click::Key(KeyCode::Esc)), ("Install", true, Click::Key(KeyCode::Enter))]);
            keys = "Keys: type the passphrase  ↵ install  Esc back";
        }
        BundleStep::Installing => {
            rows.blank();
            rows.row(vec![t.b(format!("Installing {head}")), t.d(format!("   {} apps", installing))]);
            rows.blank();
            for member in &review.members {
                let mark = if member.installed.is_some() { step_mark(t, StepState::Skipped) } else { t.a("›  ") };
                rows.row(vec![mark, t.n(member.review.package.clone())]);
            }
            rows.blank();
            rows.row(vec![t.d("Each app installs in turn, then the next. A failed one stops the rest; what installed before it stays.")]);
            rows.blank();
            keys = "Keys: wait while it installs";
        }
        BundleStep::Done(report) => {
            rows.blank();
            let title = match &report.problem {
                None => format!("{head} is installed"),
                Some(_) => format!("{head} wasn't fully installed"),
            };
            rows.row(vec![t.b(title)]);
            rows.blank();
            for member in &report.members {
                rows.row(vec![step_mark(t, member.state), t.b(member.package.clone()), t.d(format!("   {}", member.note))]);
                if let Some(inner) = &member.report {
                    for detail in &inner.details {
                        rows.row(vec![t.n("     "), t.d(detail.clone())]);
                    }
                }
            }
            rows.blank();
            if let Some(problem) = &report.problem {
                rows.row(vec![t.e(format!("✕ {problem}"))]);
                rows.row(vec![t.d("The apps installed before it stay. Installing again skips them and carries on.")]);
                rows.blank();
            }
            after.blank(t);
            let has_setup = report.problem.is_none() && app.data.owner_pages.iter().any(|(p, _)| p == head);
            if has_setup {
                after.buttons(t, &[("Done", false, Click::Key(KeyCode::Esc)), ("Open setup", true, Click::Key(KeyCode::Enter))]);
                keys = "Keys: ↵ open setup  ↑↓ scroll  Esc done";
            } else {
                after.buttons(t, &[("Done", true, Click::Key(KeyCode::Esc))]);
                keys = "Keys: ↑↓ scroll  Esc done";
            }
        }
    }
    // The card under the window keeps its buttons' targets; while installing it is not shown.
    let (after, after_targets) = match after.into_parts() {
        (items, targets) if !matches!(state.step, BundleStep::Installing) => (items, targets.into_iter().map(|(_, target)| target).collect()),
        _ => (Vec::new(), Vec::new()),
    };
    let follow = !matches!(state.step, BundleStep::Done(_));
    scrolled(t, screen, rows, state.scroll, follow, after, after_targets, keys, w, h)
}

fn step_state_mark(t: &Theme, state: OwnerStepState) -> Span<'static> {
    match state {
        OwnerStepState::Done => t.o("✓"),
        OwnerStepState::Todo => t.a("○"),
        OwnerStepState::Problem => t.e("✕"),
        OwnerStepState::Unknown => t.d("?"),
    }
}

fn check_mark(t: &Theme, state: OwnerCheckState) -> Span<'static> {
    match state {
        OwnerCheckState::Ok => t.o("✓  "),
        OwnerCheckState::Failed => t.e("✕  "),
        OwnerCheckState::Waiting => t.a("…  "),
        OwnerCheckState::Unknown => t.d("?  "),
    }
}

fn check_row(rows: &mut Rows, t: &Theme, check: &OwnerCheck) {
    let mut spans = vec![t.n("     "), check_mark(t, check.state), t.n(check.label.clone())];
    if let Some(detail) = &check.detail {
        spans.push(t.d(format!("   {detail}")));
    }
    rows.row(spans);
}

/// Text from a package, as rows: each of its lines wrapped by the card.
fn text_rows(rows: &mut Rows, t: &Theme, text: &str, indent: &str) {
    for paragraph in text.split('\n') {
        rows.row(vec![t.n(indent.to_owned()), t.n(paragraph.to_owned())]);
    }
}

/// A value on its own line in an inset field, with a Copy button, and kept as plain text so it
/// can also be selected by hand.
fn value_rows(rows: &mut Rows, t: &Theme, label: &str, value: &str, index: usize, state: &OwnerPageState) {
    let selected = state.focus == index;
    if selected {
        rows.focus_here();
    }
    let lead = if selected { t.a("▸ ") } else { t.n("  ") };
    let copied = if state.copied == Some(index) { "   Copied" } else { "" };
    let label = label.to_owned();
    rows.piece(|card, t| {
        card.row_with_button(t, vec![t.n("   "), lead, t.b(label), t.o(copied)], "Copy", selected, pick(index, KeyCode::Char('c')));
    });
    let room = rows.inner().saturating_sub(10);
    for part in value.split('\n') {
        let chars: Vec<char> = part.chars().collect();
        for chunk in chars.chunks(room.max(20)) {
            let text: String = chunk.iter().collect();
            let style = Style::new().fg(t.fg).bg(t.field);
            rows.row(vec![t.n("     "), Span::styled(format!(" {text} "), style)]);
        }
    }
}

/// The QR code's rows in half blocks: two modules a row, dark modules black on a white quiet zone
/// of four modules, so a phone reads it in a dark terminal too.
pub fn qr_lines(data: &str) -> Option<Vec<Vec<Span<'static>>>> {
    let code = qrcodegen::QrCode::encode_text(data, qrcodegen::QrCodeEcc::Low).ok()?;
    let size = code.size();
    let quiet = 4;
    let dark = |x: i32, y: i32| x >= 0 && y >= 0 && x < size && y < size && code.get_module(x, y);
    let colour = |is_dark: bool| if is_dark { Color::Rgb(0, 0, 0) } else { Color::Rgb(255, 255, 255) };
    let mut lines = Vec::new();
    let mut y = -quiet;
    while y < size + quiet {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut current: Option<(bool, bool)> = None;
        for x in -quiet..size + quiet {
            let pair = (dark(x, y), dark(x, y + 1) && y + 1 < size + quiet);
            if current.is_some_and(|c| c != pair) {
                let (top, bottom) = current.expect("checked");
                spans.push(Span::styled(std::mem::take(&mut run), Style::new().fg(colour(top)).bg(colour(bottom))));
            }
            current = Some(pair);
            run.push('▀');
        }
        if let Some((top, bottom)) = current {
            spans.push(Span::styled(run, Style::new().fg(colour(top)).bg(colour(bottom))));
        }
        lines.push(spans);
        y += 2;
    }
    Some(lines)
}

/// An app's setup page: the passphrase, then the checklist.
pub fn owner_page(app: &App, t: &Theme, state: &OwnerPageState) -> Page {
    let (w, h) = super::frame_size();
    let mut screen = Screen3::new(app, t, Tab::Apps, w);
    let package = state.package.as_str();
    let mut before_targets = Vec::new();
    screen.before = vec![Item::Blank, trail(t, &["Apps", package, &state.title], &mut before_targets), Item::Blank];
    screen.targets.extend(before_targets);
    let Some(view) = &state.view else {
        let mut card = Card::new(t, w);
        card.blank(t);
        card.row(t, vec![t.b(format!("{} for {package}", state.title))]);
        card.blank(t);
        card.row(t, vec![t.n("The setup page shows addresses and codes that let others in, so it needs your device passphrase.")]);
        card.blank(t);
        let mut pass = vec![t.d(pad("Passphrase", 13))];
        pass.extend(inset(t, &state.passphrase.display(), 30, None, true));
        card.row(t, pass);
        if let Some(error) = &state.error {
            card.row(t, vec![t.n(" ".repeat(13)), t.e(format!("✕ {error}"))]);
        }
        if state.loading {
            card.row(t, vec![t.n(" ".repeat(13)), t.d(format!("Asking {package}… it may check its sites first, up to half a minute."))]);
        }
        card.blank(t);
        card.buttons(t, &[("Open", true, Click::Key(KeyCode::Enter))]);
        return screen.card(t, card, "Keys: type the passphrase  ↵ open  Esc back", w, h);
    };
    let mut rows = Rows::new(t, w);
    rows.blank();
    let mut heading = vec![t.b(view.title.clone())];
    if state.loading {
        heading.push(t.d("   Checking again…"));
    }
    rows.row(heading);
    if let Some(intro) = &view.intro {
        rows.blank();
        text_rows(&mut rows, t, intro, "");
    }
    if let Some(error) = &state.error {
        rows.blank();
        rows.row(vec![t.e(format!("✕ {error}"))]);
    }
    rows.blank();
    let mut copy = 0;
    for (number, step) in view.steps.iter().enumerate() {
        rows.row(vec![step_state_mark(t, step.state), t.n(format!("  {}.  ", number + 1)), t.b(step.title.clone())]);
        for item in &step.items {
            match item {
                OwnerItem::Text(text) => text_rows(&mut rows, t, text, "     "),
                OwnerItem::Value { label, value } => {
                    value_rows(&mut rows, t, label, value, copy, state);
                    copy += 1;
                }
                OwnerItem::Check(check) => check_row(&mut rows, t, check),
                OwnerItem::Qr { label, data } => {
                    rows.row(vec![t.n("     "), t.n(label.clone())]);
                    let room = rows.inner().saturating_sub(5);
                    match qr_lines(data) {
                        Some(lines) if lines.first().is_some_and(|l| super::v3::width_of(l) <= room) => {
                            for spans in lines {
                                let mut row = vec![t.n("     ")];
                                row.extend(spans);
                                rows.row(row);
                            }
                        }
                        Some(lines) => rows.row(vec![
                            t.n("     "),
                            t.a(format!("Make the window {} columns wider to show the code.", lines[0].iter().map(|s| s.content.chars().count()).sum::<usize>().saturating_sub(room))),
                        ]),
                        None => rows.row(vec![t.n("     "), t.e("This code is too long to draw.")]),
                    }
                    value_rows(&mut rows, t, "The same, as text", data, copy, state);
                    copy += 1;
                }
                OwnerItem::Site(site) => {
                    let name = format!("{} · {}", site.package, site.site);
                    rows.row(vec![t.n("     "), t.b(name)]);
                    if let Some(host) = &site.host {
                        let label = if site.suggested { "Suggested address" } else { "Address" };
                        value_rows(&mut rows, t, label, host, copy, state);
                        copy += 1;
                    }
                    for record in &site.records {
                        let label = format!("{} record {} in {}", record.kind, record.name, record.zone);
                        value_rows(&mut rows, t, &label, &record.value, copy, state);
                        copy += 1;
                    }
                    match &site.automatic_dns {
                        Some(provider) => rows.row(vec![t.n("     "), t.d(format!("{provider} publishes this record by itself."))]),
                        None if !site.records.is_empty() => {
                            rows.row(vec![t.n("     "), t.d("Create this record at your domain's DNS provider, or connect it in Web › Domain providers.")])
                        }
                        None => {}
                    }
                    for check in &site.checks {
                        check_row(&mut rows, t, check);
                    }
                }
                OwnerItem::Service { check, .. } | OwnerItem::Backup { check } => check_row(&mut rows, t, check),
                OwnerItem::Domain { check, .. } => check_row(&mut rows, t, check),
            }
        }
        rows.blank();
    }
    debug_assert_eq!(copy, copy_targets(view).len());
    let keys = if copy > 0 {
        "Keys: ↑↓ scroll  Tab next value  c copy  r check again  Esc back"
    } else {
        "Keys: ↑↓ scroll  r check again  Esc back"
    };
    scrolled(t, screen, rows, state.scroll, state.follow, Vec::new(), Vec::new(), keys, w, h)
}
