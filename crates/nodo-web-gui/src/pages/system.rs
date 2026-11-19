//! System: the Nodo itself. One page lists its parts, as the terminal interface does, with the
//! web GUI's own state shown and not changeable. Also here: About.

use axum::response::Response;
use maud::{html, Markup};
use nodo_api::WebGuiStatus;
use nodo_ui_core::flows::documents;
use nodo_ui_core::model::{Snapshot, UpdateNote};
use nodo_ui_core::screen::Tab;

use super::document_page;
use nodo_ui_core::words::nodo_update_note;
use nodo_ui_core::flows::web_gui::{ABOUT, SECURITY};
use nodo_ui_core::words::{nodo_update, system_row_summary, SYSTEM_ROWS};
use crate::ctx::{Ctx, Out};
use crate::dest;
use crate::layout::{self, Page};

/// The web GUI's own card. Turning it on or off stays an act over SSH: the helper refuses
/// `SetWebGui` from this socket, so the page shows the state and offers no switch.
fn web_gui_card(status: Option<&WebGuiStatus>) -> Markup {
    html! {
        section.card {
            h3 { "Web GUI" }
            @match status {
                Some(status) => dl.facts {
                    dt { "State" }
                    dd {
                        @if status.enabled { span.ok { "● " } "On" } @else { span.dim { "○ " } "Off" }
                        @if status.waiting { span.dim { " · waiting for the Nodo's address on the home network" } }
                    }
                    @if let Some(url) = &status.url { dt { "Address" } dd.mono { (url) } }
                },
                None => p.dim { "The Nodo did not say whether it is on." },
            }
            @for line in ABOUT { p.dim { (line) } }
            @for line in SECURITY { p.dim { span.warn aria-hidden="true" { "▲ " } (line) } }
            p.dim.small { "These pages are turned on and off over SSH: in nodo-tui under System › Web GUI, or with " span.mono { "nodo-tui --ctl web-gui 0|1" } "." }
        }
    }
}

fn page(snapshot: &Snapshot, web_gui: Option<&WebGuiStatus>) -> Page {
    let body = html! {
        h1 { "This Nodo" }
        @if let Some(update) = nodo_update(snapshot) {
            section.card.attn {
                h2 { "A Nodo update is ready" span.dim { "   " (update.to) "   " (nodo_update_note(update)) } }
                @if let UpdateNote::Withdrawal(withdrawn) = &update.note {
                    p { span.warn aria-hidden="true" { "▲ " } "The publisher withdrew " (withdrawn) ", which this Nodo runs." }
                }
                div.actions.end { a.btn.primary href=(dest::UPDATES) { "Review" } }
            }
        }
        section.card.flush {
            div.rows {
                @for name in SYSTEM_ROWS {
                    @let summary = system_row_summary(snapshot, name);
                    @match name {
                        "Updates" => (layout::row(dest::UPDATES, name, &summary, html! {})),
                        "Backups" => (layout::row(dest::BACKUPS, name, &summary, html! {})),
                        "Network" => (layout::row(dest::NETWORK, name, &summary, html! {})),
                        // Its card is below: the state, and no switch.
                        "Web GUI" => {},
                        "Device logs" => (layout::row(dest::LOGS, name, &summary, html! {})),
                        "About this Nodo" => (layout::row(dest::ABOUT, name, &summary, html! {})),
                        // A row the core adds later says so until it has a page here.
                        _ => (layout::coming(name, &summary)),
                    }
                }
                (layout::row(dest::ONION, "Onion addresses", "Tor links for the node and the sites", html! {}))
                (layout::row(dest::KEYS, "SSH keys", "Who may log in over SSH", html! {}))
                (layout::row(dest::DRIVE, "Data drive", if snapshot.overview.drive.is_some() { "Not connected" } else { "Connected" }, html! {}))
            }
        }
        (web_gui_card(web_gui))
    };
    Page::new(Tab::System, "System", body)
}

/// `GET /system`.
pub async fn show(ctx: Ctx) -> Response {
    ctx.with(dest::HOME, |work| Out::Page(page(&work.snapshot, work.web_gui.as_ref()))).await
}

/// `GET /system/about`: what the device reports about itself, as the core's document.
pub async fn about(ctx: Ctx) -> Response {
    ctx.with(dest::SYSTEM, |work| {
        let document = documents::about(&work.snapshot);
        let page = document_page(Some(Tab::System), &document, dest::SYSTEM);
        Out::Page(page.trail(&[("System", dest::SYSTEM), ("About this Nodo", dest::ABOUT)]))
    })
    .await
}
