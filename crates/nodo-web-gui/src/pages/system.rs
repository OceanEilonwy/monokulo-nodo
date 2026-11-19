//! System: the Nodo itself. One page lists its parts, as the terminal interface does, with the
//! web GUI's own state shown and not changeable. Also here: About.

use axum::response::Response;
use maud::{html, Markup};
use nodo_api::WebGuiStatus;
use nodo_ui_core::flows::documents;
use nodo_ui_core::model::{Snapshot, UpdateNote};
use nodo_ui_core::screen::Tab;

use nodo_ui_core::words::nodo_update_note;
use nodo_ui_core::flows::web_gui::{ABOUT, SECURITY};
use nodo_ui_core::words::{nodo_update, system_row_summary, SYSTEM_ROWS};
use crate::ctx::{Ctx, Out};
use crate::dest;
use crate::layout::{self, Page, Sub};

/// The web GUI's own section. Turning it on or off stays an act over SSH: the helper refuses
/// `SetWebGui` from this socket, so the page shows the state and offers no switch.
fn web_gui_card(status: Option<&WebGuiStatus>) -> Markup {
    html! {
        section.sec {
            h2 { "Web GUI" }
            @match status {
                Some(status) => table.kv {
                    tbody {
                        tr {
                            td { "State" }
                            td {
                                @if status.enabled { (layout::label("ok", "On")) } @else { (layout::label("off", "Off")) }
                                @if status.waiting { span.sub { " waiting for the Nodo's address on the home network" } }
                            }
                        }
                        @if let Some(url) = &status.url { tr { td { "Address" } td.mono { (url) } } }
                    }
                },
                None => p.dim { "The Nodo did not say whether it is on." },
            }
            div.body.pad {
                div.stack {
                    @for line in ABOUT { p.dim { (line) } }
                    @for line in SECURITY { p.dim { span.warn aria-hidden="true" { "▲ " } (line) } }
                    p.dim.small { "These pages are turned on and off over SSH: in nodo-tui under System › Web GUI, or with " span.mono { "nodo-tui --ctl web-gui 0|1" } "." }
                }
            }
        }
    }
}

fn page(snapshot: &Snapshot, web_gui: Option<&WebGuiStatus>) -> Page {
    let body = html! {
        @if let Some(update) = nodo_update(snapshot) {
            div.alert.info {
                (layout::label("info", "Update ready"))
                div.grow {
                    p { strong { "A Nodo update is ready" } span.dim { "   " (update.to) "   " (nodo_update_note(update)) } }
                    @if let UpdateNote::Withdrawal(withdrawn) = &update.note {
                        p { span.warn aria-hidden="true" { "▲ " } "The publisher withdrew " (withdrawn) ", which this Nodo runs." }
                    }
                }
                a.btn.primary href=(dest::UPDATES) { "Review" }
            }
        }
        section.sec {
            div.rows {
                @for name in SYSTEM_ROWS {
                    @let summary = system_row_summary(snapshot, name);
                    @match name {
                        "Updates" => (layout::row(dest::UPDATES, name, &summary, html! {})),
                        "Backups" => (layout::row(dest::BACKUPS, name, &summary, html! {})),
                        "Network" => (layout::row(dest::NETWORK, name, &summary, html! {})),
                        // Its section is below: the state, and no switch.
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
    Page::new(Tab::System, "System", body).head(layout::heading("This Nodo", None)).sub(Sub::ThisNodo)
}

/// `GET /system`.
pub async fn show(ctx: Ctx) -> Response {
    ctx.with(dest::HOME, |work| Out::Page(page(&work.snapshot, work.web_gui.as_ref()))).await
}

/// `GET /system/about`: what the device reports about itself, as the core's document.
pub async fn about(ctx: Ctx) -> Response {
    ctx.with(dest::SYSTEM, |work| {
        let document = documents::about(&work.snapshot);
        let page = Page::new(Tab::System, document.title.clone(), layout::document_text(&document));
        Out::Page(page.trail(&[("System", dest::SYSTEM), ("About this Nodo", dest::ABOUT)]).head(layout::heading(&document.title, None)).sub(Sub::About))
    })
    .await
}
