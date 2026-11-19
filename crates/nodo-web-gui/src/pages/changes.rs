//! Unsaved changes (`flows::staged`): the drafts this session kept with Save and has not applied,
//! each with the way to its page, where Save & Apply puts it live, and the way to drop it.
//!
//! Applying happens on each draft's own page, since each asks its own questions first (what a
//! changed reference allows, the address typed before a site goes public). This page only
//! lists and resets.

use axum::response::Response;
use axum::Form;
use maud::html;
use nodo_ui_core::flows::staged::{self, StageKey, StagedChange};
use serde::Deserialize;

use crate::ctx::{Ctx, Out};
use crate::dest;
use crate::layout::Page;

/// A staged draft's key as a form carries it: `auto-update`, `settings/{app}` or
/// `hosting/{site}`.
fn key_text(key: &StageKey) -> String {
    match key {
        StageKey::AutoUpdate => "auto-update".to_owned(),
        StageKey::Settings(package) => format!("settings/{package}"),
        StageKey::Hosting(site) => format!("hosting/{site}"),
    }
}

/// The key a form posted, or `None` for anything else.
fn parse_key(text: &str) -> Option<StageKey> {
    match text.split_once('/') {
        None if text == "auto-update" => Some(StageKey::AutoUpdate),
        Some(("settings", package)) if !package.is_empty() => Some(StageKey::Settings(package.to_owned())),
        Some(("hosting", site)) if !site.is_empty() => Some(StageKey::Hosting(site.to_owned())),
        _ => None,
    }
}

/// The page where a staged draft is changed and applied.
fn page_of(key: &StageKey) -> String {
    match key {
        StageKey::AutoUpdate => dest::AUTO_UPDATE.to_owned(),
        StageKey::Settings(package) => dest::settings(package),
        StageKey::Hosting(site) => dest::hosting(site),
    }
}

fn page(list: &[StagedChange]) -> Page {
    let total = staged::total(list);
    let body = html! {
        div.head { h1 { "Unsaved changes" } }
        @if list.is_empty() {
            section.card { p.dim { "Nothing is waiting. Every change you saved is on the Nodo." } }
        } @else {
            p.lead {
                (total) @if total == 1 { " change is" } @else { " changes are" }
                " kept here and not on the Nodo yet. Open a page to apply its changes; each checks them first."
            }
            section.sec {
                table.tbl {
                    thead { tr { th { "Page" } th { "Changes" } th.right { "Actions" } } }
                    tbody {
                        @for change in list {
                            tr {
                                td { a href=(page_of(&change.key)) { (change.title) } }
                                td { (change.changes) }
                                td.right {
                                    div.btns {
                                        a.btn.pos href=(page_of(&change.key)) { "Review and apply" }
                                        form method="post" action=(dest::CHANGES) {
                                            input type="hidden" name="key" value=(key_text(&change.key));
                                            button.btn type="submit" name="do" value="reset" { "Reset" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            form.applybar method="post" action=(dest::CHANGES) {
                button.btn.danger type="submit" name="do" value="reset-all" { "Reset all" }
            }
        }
    };
    Page::plain("Unsaved changes".to_owned(), body)
}

/// `GET /changes`.
pub async fn show(ctx: Ctx) -> Response {
    ctx.with(dest::HOME, |work| {
        let list = work.session.inner().stash.staged(&work.snapshot);
        Out::Page(page(&list))
    })
    .await
}

/// What the page posts: the button as `do`, and the draft it is about.
#[derive(Debug, Default, Deserialize)]
pub struct ChangesForm {
    #[serde(default, rename = "do")]
    action: String,
    #[serde(default)]
    key: String,
}

/// `POST /changes`: Reset one draft, or all of them.
pub async fn act(ctx: Ctx, Form(form): Form<ChangesForm>) -> Response {
    ctx.with(dest::CHANGES, move |work| {
        let mut inner = work.session.inner();
        let keys: Vec<StageKey> = match form.action.as_str() {
            "reset" => parse_key(&form.key).into_iter().collect(),
            "reset-all" => inner.stash.staged(&work.snapshot).into_iter().map(|change| change.key).collect(),
            _ => Vec::new(),
        };
        for key in &keys {
            inner.stash.drop_staged(key);
            inner.notices.push(staged::reset_notice(key));
        }
        Out::Go(dest::CHANGES.to_owned())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_goes_into_a_form_and_comes_back() {
        for key in [StageKey::AutoUpdate, StageKey::Settings("moneropay".to_owned()), StageKey::Hosting("monokulo-pay".to_owned())] {
            assert_eq!(parse_key(&key_text(&key)), Some(key));
        }
        assert_eq!(parse_key("settings/"), None);
        assert_eq!(parse_key("other/x"), None);
        assert_eq!(parse_key(""), None);
    }
}
