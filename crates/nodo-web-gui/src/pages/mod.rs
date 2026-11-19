//! The pages, one module per group. Each handler is thin: it names the flow's action, lets
//! `Ctx` run it, and draws what comes back.

pub mod apps;
pub mod drive;
pub mod flow;
pub mod home;
pub mod install;
pub mod keys;
pub mod login;
pub mod logs;
pub mod settings;
pub mod sources;
pub mod system;
pub mod updates;

use axum::http::StatusCode;
use maud::html;
use nodo_ui_core::model::Document;
use nodo_ui_core::screen::Tab;
use serde::{Deserialize, Deserializer};
use zeroize::Zeroizing;

use crate::layout::{self, Page};

/// A passphrase as it arrives in a form: wiped when dropped, and never printed.
///
/// The bytes of the request body it was read from are hyper's and axum's and are freed without
/// being wiped; this is what the page's own code holds.
#[derive(Default)]
pub struct Typed(Zeroizing<String>);

impl Typed {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Typed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Typed(<redacted>)")
    }
}

impl<'de> Deserialize<'de> for Typed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|text| Typed(Zeroizing::new(text)))
    }
}

/// A form that carries the device passphrase and nothing else.
#[derive(Debug, Default, Deserialize)]
pub struct PassForm {
    #[serde(default)]
    pub passphrase: Typed,
}

/// A text to read, as a page of its own with the way back.
pub fn document_page(tab: Option<Tab>, document: &Document, back: &str) -> Page {
    let body = html! {
        div.head {
            h1 { (document.title) }
            div.actions { a.btn href=(back) { "Back" } }
        }
        (layout::document_text(document))
    };
    Page { tab, ..Page::plain(document.title.clone(), body) }
}

/// A page for something that is not there (any more).
pub fn missing(tab: Tab, title: &str, what: &str, back: (&str, &str)) -> Page {
    let body = html! {
        section.card {
            h1 { (title) }
            p.dim { (what) }
            div.actions { a.btn href=(back.1) { (back.0) } }
        }
    };
    Page::new(tab, title, body).status(StatusCode::NOT_FOUND)
}
