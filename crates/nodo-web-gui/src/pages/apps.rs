//! Apps: the installed apps, and one app's page with start, stop, restart, update and removal
//! (`flows::services`).

use axum::extract::Path;
use axum::response::Response;
use maud::{html, Markup};
use nodo_api::ServiceState;
use nodo_ui_core::flows::services::{installed_apps, AppPage};
use nodo_ui_core::flows::Step;
use nodo_ui_core::model::Snapshot;
use nodo_ui_core::queries::APP_SECTIONS;
use nodo_ui_core::screen::Tab;

use nodo_ui_core::words::open_url;
use nodo_ui_core::words::{app_state, counted, hosting_summary, log_summary, tagline};
use crate::ctx::{Ctx, Out};
use crate::dest;
use crate::layout::{self, Page, Sub};
use crate::runner::{no_flow, Reply};

/// The colour of an app's state: running is fine, stopped is off, failing is bad, and waiting
/// for the passphrase or another app wants a look.
fn state_kind(snapshot: &Snapshot, name: &str) -> &'static str {
    match snapshot.services.iter().find(|svc| svc.id == name).map(|svc| svc.state) {
        Some(ServiceState::Running) => "ok",
        Some(ServiceState::Failed) => "bad",
        Some(ServiceState::Locked | ServiceState::Waiting) => "warn",
        Some(ServiceState::Stopped) | None => "off",
    }
}

/// An app's state as a label, in the core's words.
pub fn state_label(snapshot: &Snapshot, name: &str) -> Markup {
    let (words, _) = app_state(snapshot, name);
    layout::label(state_kind(snapshot, name), &words)
}

/// The top of every page of an app: its name, its state, where it opens, whether an update is
/// ready, and the buttons that act on the whole app.
pub fn app_head(snapshot: &Snapshot, name: &str, desc: Option<&str>) -> Markup {
    let page = AppPage::new(name);
    let here = dest::app(name);
    let running = page.state(snapshot) == Some(ServiceState::Running);
    let site = snapshot.web.sites.iter().find(|site| site.package == name);
    let action = |what: &str| format!("{here}/{what}");
    html! {
        h1 { (name) }
        div.status {
            div.grow {
                (state_label(snapshot, name))
                @if let Some(url) = site.and_then(open_url) {
                    span.sub { "Open at " a.mono href=(url) rel="noreferrer" { (url) } }
                }
                @if page.has_update(snapshot) { (layout::label("info", "Update ready")) }
            }
            div.btns {
                @if page.has_update(snapshot) { (layout::post(&action("update"), "Update", "btn pos")) }
                (layout::post(&action("restart"), "Restart", "btn"))
                (layout::post(&action("toggle"), if running { "Stop" } else { "Start" }, "btn"))
                (layout::ask(&action("remove"), "Remove…", "btn danger"))
            }
        }
        @if let Some(desc) = desc { p.desc { (desc) } }
    }
}

/// The tabs of an app's pages: its overview, then the sections the core lists for it. Hosting
/// is left out for an app without a web page.
pub fn app_tabs(snapshot: &Snapshot, name: &str, on: &str) -> Markup {
    let here = dest::app(name);
    let page = AppPage::new(name);
    let mut entries = vec![("Overview".to_owned(), here.clone(), on == "Overview")];
    for section in snapshot.app_sections(name) {
        let url = match section.as_str() {
            "Logs" => dest::log(name),
            "Hosting" if !page.has_site(snapshot) => continue,
            "Hosting" => dest::hosting(name),
            "Settings" => dest::settings(name),
            _ => format!("{here}/owner"),
        };
        let current = section == on || (on == "Setup" && !APP_SECTIONS.contains(&section.as_str()));
        entries.push((section, url, current));
    }
    layout::strip(name, &entries)
}

/// A page of an app: its head and tabs above `body`, with `on` the current tab.
pub fn app_frame(snapshot: &Snapshot, name: &str, on: &str, desc: Option<&str>, title: impl Into<String>, body: Markup) -> Page {
    Page::new(Tab::Apps, title, body).head(app_head(snapshot, name, desc)).strip(app_tabs(snapshot, name, on))
}

fn list_page(snapshot: &Snapshot) -> Page {
    let apps = installed_apps(snapshot);
    let updates = apps.iter().filter(|name| AppPage::new(name).has_update(snapshot)).count();
    let body = html! {
        section.sec {
            h2 {
                span { "Installed apps" }
                span.sub { (counted(apps.len() as u64, "app")) @if updates > 0 { " · " (counted(updates as u64, "update")) " ready" } }
            }
            @if apps.is_empty() {
                p.dim { "No apps yet. Get apps finds them in your sources." }
            } @else {
                div.tbox {
                    table {
                        thead { tr { th { "Name" } th { "State" } th { "Update" } th.end { "Actions" } } }
                        tbody {
                            @for name in &apps {
                                @let page = AppPage::new(name);
                                @let here = dest::app(name);
                                @let running = page.state(snapshot) == Some(ServiceState::Running);
                                tr {
                                    td { a href=(here) { strong { (name) } } div.sub { (tagline(snapshot, name)) } }
                                    td { (state_label(snapshot, name)) }
                                    td { @if page.has_update(snapshot) { (layout::label("info", "Update ready")) } @else { span.sub { "Up to date" } } }
                                    td {
                                        div.btns {
                                            @if page.has_update(snapshot) { (layout::post(&format!("{here}/update"), "Update", "btn pos")) }
                                            a.btn href=(dest::log(name)) { "Logs" }
                                            (layout::post(&format!("{here}/restart"), "Restart", "btn"))
                                            (layout::post(&format!("{here}/toggle"), if running { "Stop" } else { "Start" }, if running { "btn" } else { "btn pos" }))
                                            (layout::ask(&format!("{here}/remove"), "Remove…", "btn danger"))
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    let head = html! {
        (layout::heading("Installed", Some("Start, stop and update the apps on this Nodo. Open a name for its hosting, settings and logs.")))
    };
    Page::new(Tab::Apps, "Apps", body).head(head).sub(Sub::Installed)
}

fn app_page(snapshot: &Snapshot, name: &str) -> Page {
    let page = AppPage::new(name);
    let here = dest::app(name);
    if page.state(snapshot).is_none() {
        return super::missing(Tab::Apps, &format!("{name} is not installed"), "It may have been removed. Get apps can install it again.", ("Apps", dest::APPS));
    }
    let update = snapshot.updates.iter().find(|item| item.name == name && !item.held);
    let sections = snapshot.app_sections(name);
    let body = html! {
        section.sec {
            h2 { "About" }
            table.kv {
                tbody {
                    tr { td { "What it is" } td { (tagline(snapshot, name)) } }
                    tr { td { "State" } td { (state_label(snapshot, name)) } }
                    tr {
                        td { "Version" }
                        td {
                            @match update {
                                Some(update) => {
                                    span.warn { "Version " (update.to) " is ready" } span.sub { "   from " (update.from) }
                                },
                                None => span.sub { "Up to date" },
                            }
                        }
                    }
                }
            }
        }
        section.sec {
            h2 { "Its pages" }
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
    app_frame(snapshot, name, "Overview", None, name, body).trail(&[("Apps", dest::APPS), (name, &here)])
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
