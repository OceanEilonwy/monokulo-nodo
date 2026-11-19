//! Home: what needs the owner, with one button each, then the apps and the device's health.

use std::time::Duration;

use axum::response::Response;
use axum::Form;
use maud::{html, Markup};
use nodo_ui_core::flows::unlock::Unlock;
use nodo_ui_core::flows::{unlock as unlock_flow, Dest, Step};
use nodo_ui_core::model::{AttentionLevel, Snapshot};
use nodo_ui_core::queries::{HomeItem, Landing};
use nodo_ui_core::screen::Tab;

use super::{apps, drive};
use crate::glue::typed_secret;
use nodo_ui_core::words::{home_card, unlock_lines};
use nodo_ui_core::words::{app_state, cpu_busy_permille, cpu_summary, open_url, reach_words, site_address};
use crate::ctx::{Ctx, Out, Work};
use crate::runner::follow_plain;
use crate::dest;
use crate::layout::{self, Page};

/// Whole seconds left until `at`, rounded up.
fn seconds_until(at: Duration, now: Duration) -> u64 {
    let left = at.saturating_sub(now);
    left.as_secs() + u64::from(left.subsec_nanos() > 0)
}

/// The unlock strip: after a login the device can still be partly locked (a helper that
/// restarted since, storage that failed to open), and this is where it is unlocked.
fn unlock_card(snapshot: &Snapshot, head: &str, body: &str, problem: Option<&str>) -> Markup {
    html! {
        section.alert.warn #unlock {
            (layout::label("warn", "Locked"))
            div.grow {
                p { strong { (head) "." } " " (body) }
                @for line in unlock_lines(snapshot, None) { p.sub { (line) } }
                p.sub { "A lost passphrase can't be recovered; a backup brings the data back." }
            }
            form method="post" action=(dest::UNLOCK) {
                (layout::passphrase("unlock-passphrase", "Device passphrase", problem, false))
                button.btn.primary type="submit" { "Unlock" }
            }
        }
    }
}

/// How much of a disk or of memory is in use, from the device's own words for it: "412 GB of
/// 1.9 TB" is used of the whole, "3.1 GB free of 7.6 GB" or "… available of …" what is left.
/// `None` for anything else, which is shown as words without a bar.
fn usage(value: &str) -> Option<f64> {
    fn size(number: &str, unit: &str) -> Option<f64> {
        let scale = match unit {
            "KB" | "kB" => 1e3,
            "MB" => 1e6,
            "GB" => 1e9,
            "TB" => 1e12,
            _ => return None,
        };
        number.replace(',', "").parse::<f64>().ok().map(|n| n * scale)
    }
    let words: Vec<&str> = value.split_whitespace().collect();
    let (part, left, total) = match words.as_slice() {
        [n, u, "of", tn, tu] => (size(n, u)?, false, size(tn, tu)?),
        [n, u, "free" | "available", "of", tn, tu] => (size(n, u)?, true, size(tn, tu)?),
        _ => return None,
    };
    if total <= 0.0 || part > total {
        return None;
    }
    Some(if left { (total - part) / total } else { part / total })
}

/// The device's own lines: what it is in one table, and the disks and memory with their bars
/// in another beside it.
fn device_sections(snapshot: &Snapshot) -> Markup {
    let rows = &snapshot.overview.device;
    let (meters, facts): (Vec<_>, Vec<_>) = rows.iter().partition(|(_, value)| usage(value).is_some());
    let cpu = snapshot.overview.cpu.as_ref();
    html! {
        div.grid2 {
            section.sec {
                h2 { "This Nodo" }
                table.kv {
                    tbody {
                        @for (label, value) in &facts { tr { td { (label) } td { (value) } } }
                        @if let Some(status) = &snapshot.web_gui {
                            tr { td { "Web GUI" } td { (layout::label(if status.enabled { "ok" } else { "off" }, if status.enabled { "On" } else { "Off" })) @if let Some(url) = &status.url { " " span.mono { (url) } } } }
                        }
                        tr {
                            td { "Data drive" }
                            td { @if snapshot.overview.drive.is_some() { (layout::label("bad", "Not connected")) } @else { (layout::label("ok", "Connected")) } }
                        }
                    }
                }
            }
            @if cpu.is_some() || !meters.is_empty() {
                section.sec {
                    h2 { @if cpu.is_some() { "CPU, memory and storage" } @else { "Memory and storage" } }
                    table.kv {
                        tbody {
                            @if let Some(cpu) = cpu {
                                @let busy = f64::from(cpu_busy_permille(cpu)) / 1000.0;
                                @let summary = cpu_summary(cpu);
                                tr {
                                    td { "CPU" }
                                    td { div.meter { meter min="0" max="1" value=(format!("{busy:.3}")) aria-label=(format!("CPU: {summary}")) {} span.sub { (summary) } } }
                                }
                            }
                            @for (label, value) in &meters {
                                @let used = usage(value).unwrap_or_default();
                                tr {
                                    td { (label) }
                                    td { div.meter { meter min="0" max="1" value=(format!("{used:.3}")) aria-label=(format!("{label}: {value}")) {} span.sub { (value) } } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn page(work: &mut Work<'_>) -> Page {
    let snapshot = &work.snapshot;
    let items = snapshot.home_items();
    let needs: Vec<&HomeItem> = items.iter().filter(|item| !matches!(item, HomeItem::App(_))).collect();
    let notes = &snapshot.overview.attention;
    let title = if needs.is_empty() && notes.is_empty() { "Everything is running" } else { "Your Nodo" };
    // Said once: the reason the last try was refused. The count of wrong tries stays.
    let unlock_problem = work.session.inner().unlock.error.take();
    let apps: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            HomeItem::App(id) => Some(id.as_str()),
            _ => None,
        })
        .collect();
    let problems = needs.iter().any(|item| matches!(item, HomeItem::Failing(_) | HomeItem::Unlock));
    let summary = if apps.is_empty() {
        "No apps yet"
    } else if problems {
        "The other apps"
    } else {
        "Everything else is running"
    };
    let sites = snapshot.active_sites();
    let body = html! {
        // Where the terminal shows the drive's page in Home's place, the drive's card takes the
        // top here. First setup cannot be due: the web GUI is only on once it is done.
        @match (snapshot.landing(), &snapshot.overview.drive) {
            (Landing::NoDrive, Some(failure)) => (drive::card(failure)),
            (Landing::Setup, _) => p.notice.error { "✕ This Nodo is not set up yet. Set it up on the device or over SSH." },
            _ => {},
        }
        @if !needs.is_empty() || !notes.is_empty() {
            div.alerts {
                @for item in &needs {
                    @if let Some((head, body, label)) = home_card(snapshot, item) {
                        @match item {
                            HomeItem::Unlock => (unlock_card(snapshot, &head, &body, unlock_problem.as_deref())),
                            HomeItem::Failing(id) => section.alert.bad {
                                (layout::label("bad", &app_state(snapshot, id).0))
                                p { strong { (head) "." } " " (body) }
                                a.btn href=(dest::log(id)) { (label) }
                            },
                            HomeItem::Updates => section.alert.info {
                                (layout::label("info", "Updates"))
                                p { strong { (head) } " — " (body) }
                                a.btn.pos href=(dest::url(&Dest::Updates { only: None }, dest::HOME)) { (label) }
                            },
                            HomeItem::App(_) => {},
                        }
                    }
                }
                @for note in notes {
                    @let (kind, word) = match note.level {
                        AttentionLevel::Critical => ("bad", "Critical"),
                        AttentionLevel::Warning => ("warn", "Warning"),
                        AttentionLevel::Info => ("info", "Note"),
                    };
                    div class=(format!("alert {kind}")) {
                        (layout::label(kind, word))
                        p { (note.text.replace('`', "")) }
                    }
                }
            }
        }
        @if !snapshot.overview.device.is_empty() { (device_sections(snapshot)) }
        section.sec {
            h2 {
                span { @if !problems && !apps.is_empty() { span.ok aria-hidden="true" { "✓ " } } (summary) }
                a href=(dest::APPS) { "All apps" }
            }
            @if apps.is_empty() {
                p.dim { "Get apps from " a href=(dest::GET_APPS) { "Apps › Get apps" } "." }
            } @else {
                div.tbox {
                    table {
                        thead { tr { th { "App" } th { "State" } th.end { "Actions" } } }
                        tbody {
                            @for id in &apps {
                                @let site = snapshot.web.sites.iter().find(|site| site.package == *id);
                                tr {
                                    td { a href=(dest::app(id)) { strong { (id) } } }
                                    td { (apps::state_label(snapshot, id)) }
                                    td {
                                        div.btns {
                                            @if let Some(url) = site.and_then(open_url) { a.btn href=(url) rel="noreferrer" { "Open" } }
                                            a.btn href=(dest::log(id)) { "Logs" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        @if !sites.is_empty() {
            section.sec {
                h2 { "Sites online" a href=(dest::WEB) { "Web settings" } }
                div.tbox {
                    table {
                        thead { tr { th { "Address" } th { "App" } th { "Reach" } } }
                        tbody {
                            @for site in &sites {
                                tr {
                                    td.mono { (site_address(site)) }
                                    td { a href=(dest::hosting(&site.package)) { (site.package) } }
                                    td { (layout::label(if site.exposure.public { "ok" } else { "info" }, reach_words(site))) }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    Page::new(Tab::Home, "Home", body).head(layout::heading(title, Some("What needs you first, then everything else.")))
}

/// `GET /`.
pub async fn show(ctx: Ctx) -> Response {
    ctx.with(dest::HOME, |work| Out::Page(page(work))).await
}

/// `GET /unlock`: the page a flow sends the owner to when what was asked needs the device
/// unlocked (`Dest::Unlock`). It says why, and once unlocked the owner is back on the page
/// that sent them, to press its button again. Home's card is the same form without a reason.
pub async fn unlock_page(ctx: Ctx) -> Response {
    ctx.with(dest::HOME, |work| {
        let (sent, problem, wait) = {
            let mut inner = work.session.inner();
            let wait = inner.unlock_retry_at.map_or(0, |at| seconds_until(at, work.now));
            (inner.stash.unlock_return.clone(), inner.unlock.error.take(), wait)
        };
        // The flow says whether there is anything to unlock.
        let step = unlock_flow::open(&work.snapshot);
        if !matches!(step, Step::Go(Dest::Unlock { .. })) {
            let mut inner = work.session.inner();
            inner.stash.unlock_return = None;
            inner.notices.extend(follow_plain(step).notices);
            return Out::Go(sent.map_or_else(|| dest::HOME.to_owned(), |(_, back)| back));
        }
        let body = html! {
            h1 { "Unlock your Nodo" }
            section.card.attn {
                @for line in unlock_lines(&work.snapshot, sent.as_ref().map(|(reason, _)| reason.as_str())) { p { (line) } }
                // The wait after a wrong try, counted by this process's clock.
                @if wait > 0 { p.warn role="status" { "You can try again in " (nodo_ui_core::words::counted(wait, "second")) "." } }
                form.stack method="post" action=(dest::UNLOCK) {
                    (layout::hidden("from", "page"))
                    (layout::passphrase("unlock-passphrase", "Device passphrase", problem.as_deref(), true))
                    div.actions.end {
                        a.btn href=(sent.as_ref().map_or(dest::HOME, |(_, back)| back.as_str())) { "Leave it locked" }
                        button.btn.primary type="submit" { "Unlock" }
                    }
                }
                p.dim.small { "A lost passphrase can't be recovered; a backup brings the data back." }
            }
        };
        let page = Page::plain("Unlock", body);
        Out::Page(if wait > 0 { page.waiting() } else { page })
    })
    .await
}

/// What the unlock form posts.
#[derive(Debug, Default, serde::Deserialize)]
pub struct UnlockForm {
    #[serde(default)]
    passphrase: super::Typed,
    /// `page` when it came from `/unlock` and not from Home's card.
    #[serde(default)]
    from: String,
}

/// `POST /unlock`, from the card on Home or from the unlock page. The count of wrong tries and
/// the wait after them are the unlock flow's; the session keeps them between requests and the
/// clock is this process's. Unlocked, the owner returns to the page that sent them.
pub async fn unlock(ctx: Ctx, Form(form): Form<UnlockForm>) -> Response {
    let sent = ctx.session.inner().stash.unlock_return.clone();
    let from_page = form.from == "page";
    let back = sent.as_ref().map_or_else(|| dest::HOME.to_owned(), |(_, back)| back.clone());
    ctx.with(back, move |work| {
        let (mut flow, wait) = {
            let inner = work.session.inner();
            (inner.unlock.clone(), inner.unlock_retry_at.map_or(0, |at| seconds_until(at, work.now)))
        };
        // What sent the owner here, in words: with it the flow answers "carry on" (`Resume`).
        flow.resume = sent.map(|(reason, _)| reason);
        let failures = flow.failures;
        let mut typed = typed_secret(form.passphrase.expose());
        let effects = flow.submit(&mut typed, wait);
        let reply = work.run(Step::run(effects), &mut |outcome, snapshot| flow.apply(outcome, snapshot));
        let mut inner = work.session.inner();
        if flow.failures > failures {
            inner.unlock_retry_at = Some(work.now + Duration::from_secs(flow.retry_delay_seconds()));
        }
        if reply.go.is_some() {
            // Unlocked: the next lock starts from nothing.
            inner.unlock = Unlock::default();
            inner.unlock_retry_at = None;
            inner.stash.unlock_return = None;
            return Out::Reply(reply);
        }
        inner.unlock = flow;
        inner.notices.extend(reply.notices);
        // Refused: the form again, where it was sent from.
        Out::Go(if from_page { dest::UNLOCK } else { dest::HOME }.to_owned())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::usage;

    #[test]
    fn usage_is_read_from_the_devices_words_or_not_at_all() {
        assert_eq!(usage("412 GB of 1.9 TB").map(|used| (used * 1000.0).round()), Some(217.0));
        assert_eq!(usage("3.1 GB free of 7.6 GB").map(|used| (used * 100.0).round()), Some(59.0));
        assert_eq!(usage("6.1 GB available of 31.0 GB").map(|used| (used * 100.0).round()), Some(80.0));
        assert_eq!(usage("3 days"), None);
        assert_eq!(usage("52 °C"), None);
        assert_eq!(usage("9 GB of 2 GB"), None);
    }
}
