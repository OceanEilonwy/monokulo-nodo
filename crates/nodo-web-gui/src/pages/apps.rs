//! Apps: the installed apps, and one app's page with start, stop, restart, update and removal
//! (`flows::services`).

use axum::extract::Path;
use axum::response::Response;
use maud::html;
use nodo_api::ServiceState;
use nodo_ui_core::flows::services::{installed_apps, AppPage};
use nodo_ui_core::flows::Step;
use nodo_ui_core::model::Snapshot;
use nodo_ui_core::queries::APP_SECTIONS;
use nodo_ui_core::screen::Tab;

use nodo_ui_core::words::open_url;
use nodo_ui_core::words::{app_state, hosting_summary, log_summary, tagline};
use crate::ctx::{Ctx, Out};
use crate::dest;
use crate::layout::{self, Page};
use crate::runner::{no_flow, Reply};

fn list_page(snapshot: &Snapshot) -> Page {
    let apps = installed_apps(snapshot);
    let body = html! {
        div.head {
            h1 { "Installed" }
            div.actions { a.btn href=(dest::GET_APPS) { "Get apps ›" } }
        }
        section.card.flush {
            @if apps.is_empty() {
                p.pad.dim { "No apps yet. Get apps finds them in your sources." }
            }
            div.rows {
                @for name in &apps {
                    @let page = AppPage::new(name);
                    @let (state, bad) = app_state(snapshot, name);
                    @let end = if page.has_update(snapshot) {
                        html! { span.warn { "Update ready" } }
                    } else {
                        html! { span class=[bad.then_some("err")] { (state) } }
                    };
                    (layout::row(&dest::app(name), name, &tagline(snapshot, name), end))
                }
            }
        }
    };
    Page::new(Tab::Apps, "Apps", body)
}

fn app_page(snapshot: &Snapshot, name: &str) -> Page {
    let page = AppPage::new(name);
    let here = dest::app(name);
    let Some(state) = page.state(snapshot) else {
        return super::missing(Tab::Apps, &format!("{name} is not installed"), "It may have been removed. Get apps can install it again.", ("Apps", dest::APPS));
    };
    let running = state == ServiceState::Running;
    let (words, bad) = app_state(snapshot, name);
    let site = snapshot.web.sites.iter().find(|site| site.package == name);
    let update = snapshot.updates.iter().find(|item| item.name == name && !item.held);
    let sections = snapshot.app_sections(name);
    let action = |what: &str| format!("{here}/{what}");
    let body = html! {
        div.head {
            h1 { (name) }
            div.actions {
                (layout::post(&action("restart"), "Restart", "btn"))
                (layout::post(&action("toggle"), if running { "Stop" } else { "Start" }, "btn"))
                (layout::ask(&action("remove"), "Remove…", "btn danger"))
            }
        }
        section.card {
            p.dim { (tagline(snapshot, name)) }
            p.state {
                span class=(if bad { "dot bad" } else if running { "dot on" } else { "dot" }) {}
                span class=[bad.then_some("err")] { (words) }
            }
            @if let Some(url) = site.and_then(open_url) {
                p { "Open at " a href=(url) rel="noreferrer" { (url) } }
            }
            @match update {
                Some(update) => div.head {
                    p { span.warn { "Version " (update.to) " is ready" } span.dim { "   from " (update.from) } }
                    (layout::post(&action("update"), "Update", "btn primary"))
                },
                None => p.dim { "Up to date" },
            }
        }
        section.card.flush {
            div.rows {
                @for section in &sections {
                    @match section.as_str() {
                        "Logs" => (layout::row(&dest::log(name), section, &log_summary(snapshot, name), html! {})),
                        // An app without a web page has nothing to host, and the row says so.
                        "Hosting" if !page.has_site(snapshot) => div.row.off {
                            span.name { (section) }
                            span.what { "This app has no web page to put online" }
                            span.end {}
                            span.go {}
                        },
                        "Hosting" => (layout::row(&dest::hosting(name), section, &hosting_summary(snapshot, name), html! {})),
                        "Settings" => (layout::row(&dest::settings(name), section, "Change how it works", html! {})),
                        // The app's setup page for its owner. Its row is named by the app, so it
                        // is whatever is not one of the fixed three.
                        _ => (layout::row(&format!("{here}/owner"), section, "The steps to set it up, and whether each is done", html! {})),
                    }
                }
            }
        }
    };
    debug_assert!(APP_SECTIONS.iter().all(|fixed| sections.iter().any(|section| section == fixed)));
    Page::new(Tab::Apps, name, body).trail(&[("Apps", dest::APPS), (name, &here)])
}

/// `GET /apps`.
pub async fn list(ctx: Ctx) -> Response {
    ctx.with(dest::HOME, |work| Out::Page(list_page(&work.snapshot))).await
}

/// `GET /apps/{name}`.
pub async fn show(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::APPS, move |work| Out::Page(app_page(&work.snapshot, &name))).await
}

/// `POST /apps/{name}/restart`.
pub async fn restart(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::app(&name), move |work| Out::Reply(work.run(Step::run(AppPage::new(&name).restart()), &mut no_flow))).await
}

/// `POST /apps/{name}/toggle`: the one start-or-stop button.
pub async fn toggle(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::app(&name), move |work| {
        let step = AppPage::new(&name).start_or_stop(&work.snapshot);
        Out::Reply(work.run(step, &mut no_flow))
    })
    .await
}

/// `POST /apps/{name}/update`: leads to the Updates page with this app chosen, or says why not.
pub async fn update(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.with(dest::app(&name), move |work| {
        let step = AppPage::new(&name).update(&work.snapshot);
        Out::Reply(work.run(step, &mut no_flow))
    })
    .await
}

/// `POST /apps/{name}/remove`: asks first. Confirming removes the app (`pages::flow::confirm`).
pub async fn remove(ctx: Ctx, Path(name): Path<String>) -> Response {
    ctx.respond(Reply::ask(AppPage::new(&name).ask_remove()), &dest::app(&name))
}
