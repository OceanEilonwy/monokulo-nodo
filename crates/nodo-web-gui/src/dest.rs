//! Where each page lives: the paths, and the one place a `Dest` of the core becomes a URL.

use nodo_ui_core::flows::install::ReviewPurpose;
use nodo_ui_core::flows::Dest;
use nodo_ui_core::model::PackageId;

use crate::session::Stash;

/// One path segment, percent-encoded: names of apps and sources come from outside.
pub fn seg(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(char::from(byte)),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

pub const HOME: &str = "/";
pub const LOGIN: &str = "/login";
pub const LOGOUT: &str = "/logout";
pub const APPS: &str = "/apps";
pub const GET_APPS: &str = "/apps/get";
pub const SOURCES: &str = "/sources";
pub const ADD_SOURCE: &str = "/sources/add";
pub const CONFIRM_SOURCE: &str = "/sources/add/confirm";
pub const WEB: &str = "/web";
pub const SYSTEM: &str = "/system";
pub const LOGS: &str = "/system/logs";
pub const KEYS: &str = "/system/keys";
pub const DRIVE: &str = "/system/drive";
pub const ABOUT: &str = "/system/about";
pub const DOCUMENT: &str = "/document";
pub const WORKING: &str = "/working";
pub const CONFIRM: &str = "/confirm";
pub const UNLOCK: &str = "/unlock";

/// An installed app's own page.
pub fn app(name: &str) -> String {
    format!("{APPS}/{}", seg(name))
}

/// An app on offer: its details, and the pages of its install under it.
pub fn offer(id: &PackageId) -> String {
    format!("{GET_APPS}/{}/{}", seg(&id.feed), seg(&id.name))
}

/// Get apps, with only one source's apps when it is named.
pub fn get_apps(source: Option<&str>) -> String {
    match source {
        Some(name) => format!("{GET_APPS}?source={}", seg(name)),
        None => GET_APPS.to_owned(),
    }
}

/// A source's own pages. They sit under `of/` so that no source's name can be taken for `add`.
pub fn source(name: &str) -> String {
    format!("{SOURCES}/of/{}", seg(name))
}

pub fn log(source: &str) -> String {
    format!("{LOGS}/{}", seg(source))
}

/// A path of this origin and nothing else: what a `to` or `back` taken from a request may be.
pub fn local_path(candidate: &str) -> Option<&str> {
    (candidate.starts_with('/') && !candidate.starts_with("//") && !candidate.contains('\\')).then_some(candidate)
}

/// The URL of a destination. `back` is the page that `Dest::Back` means for whoever asks: this
/// interface keeps no stack of pages, so each handler names its page's parent.
///
/// The match is exhaustive on purpose: a `Dest` the core gains does not compile until it has a
/// page here.
pub fn url(dest: &Dest, back: &str) -> String {
    match dest {
        Dest::Back => back.to_owned(),
        Dest::Apps => APPS.to_owned(),
        // TODO(page): the Updates page, with `only` chosen. Until then System, which lists it.
        Dest::Updates { only: _ } => SYSTEM.to_owned(),
        // Logging in unlocks the device, so there is no unlock page: what is still locked
        // afterwards is unlocked from the card on Home.
        Dest::Unlock { resume: _ } => HOME.to_owned(),
        // The page that sent the owner to unlock; they press its button again.
        Dest::Resume => back.to_owned(),
        Dest::GetApps { source } => get_apps(source.as_deref()),
        Dest::ConfirmSource(_) => CONFIRM_SOURCE.to_owned(),
        Dest::KeyChange(change) => format!("{}/key", source(&change.feed)),
        Dest::Sources { select: _ } => SOURCES.to_owned(),
        Dest::AppPage(name) => app(name),
        Dest::Detail(detail) => offer(&detail.id),
        Dest::Review(review) => match review.purpose {
            ReviewPurpose::Install => format!("{}/review", offer(&review.data.id)),
            // TODO(page): the review of an update, under the Updates page.
            ReviewPurpose::Update { .. } => SYSTEM.to_owned(),
        },
        // TODO(page): installing a suite of apps together. Until then Get apps.
        Dest::Bundle(_) => GET_APPS.to_owned(),
        Dest::Rejected(data) => format!("{}/rejected", offer(&data.id)),
        Dest::MissingCapability(data) => format!("{}/capability", offer(&data.id)),
        Dest::Installing(report) => format!("{}/installing", offer(&report.id)),
        // TODO(page): applying a batch of updates, as a job. Until then System.
        Dest::ApplyUpdates { .. } => SYSTEM.to_owned(),
        Dest::Document(_) => DOCUMENT.to_owned(),
    }
}

/// Goes to a destination: keeps what it carries for the page it leads to, and gives its URL.
pub fn land(dest: &Dest, back: &str, stash: &mut Stash) -> String {
    stash.keep(dest, back);
    url(dest, back)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_keep_plain_names_and_encode_the_rest() {
        assert_eq!(seg("wallet-relay"), "wallet-relay");
        assert_eq!(seg("a/b c?"), "a%2Fb%20c%3F");
        assert_eq!(seg("é"), "%C3%A9");
    }

    #[test]
    fn only_paths_of_this_origin_are_followed() {
        assert_eq!(local_path("/apps?x=1"), Some("/apps?x=1"));
        assert_eq!(local_path("//evil.example"), None);
        assert_eq!(local_path("https://evil.example"), None);
        assert_eq!(local_path("/\\evil.example"), None);
    }
}
