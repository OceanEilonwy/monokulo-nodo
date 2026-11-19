//! Where each page lives: the paths, and the one place a `Dest` of the core becomes a URL.

use nodo_ui_core::flows::install::ReviewPurpose;
use nodo_ui_core::flows::web::public::Found;
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
pub const PROVIDERS: &str = "/web/providers";
pub const DOMAIN: &str = "/web/domain";
pub const CERTIFICATES: &str = "/web/certificates";
pub const TRUST: &str = "/web/trust";
pub const ONION: &str = "/system/onion";
pub const SYSTEM: &str = "/system";
pub const LOGS: &str = "/system/logs";
pub const KEYS: &str = "/system/keys";
pub const DRIVE: &str = "/system/drive";
pub const ABOUT: &str = "/system/about";
pub const UPDATES: &str = "/system/updates";
pub const UPDATE_REVIEW: &str = "/system/updates/review";
pub const UPDATE_PASSPHRASE: &str = "/system/updates/passphrase";
pub const UPDATE_RESULTS: &str = "/system/updates/results";
pub const AUTO_UPDATE: &str = "/system/auto-update";
pub const UNDO: &str = "/system/undo";
pub const BACKUPS: &str = "/system/backups";
pub const NETWORK: &str = "/system/network";
pub const BUNDLE: &str = "/apps/get/bundle";
pub const DOCUMENT: &str = "/document";
pub const WORKING: &str = "/working";
pub const CONFIRM: &str = "/confirm";
pub const UNLOCK: &str = "/unlock";
/// The changes kept with Save and not applied yet (`flows::staged`).
pub const CHANGES: &str = "/changes";

/// An installed app's own page.
pub fn app(name: &str) -> String {
    format!("{APPS}/{}", seg(name))
}

/// An installed app's settings.
pub fn settings(name: &str) -> String {
    format!("{}/settings", app(name))
}

/// A site's Hosting page. A site is named by its app.
pub fn hosting(site: &str) -> String {
    format!("{}/hosting", app(site))
}

/// The Updates page, with only one app's update ticked when it is named.
pub fn updates(only: Option<&str>) -> String {
    match only {
        Some(name) => format!("{UPDATES}?only={}", seg(name)),
        None => UPDATES.to_owned(),
    }
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
        Dest::Updates { only } => updates(only.as_deref()),
        // Logging in unlocks the device, so this page is only met when something is still
        // locked afterwards. Home has the same form on a card.
        Dest::Unlock { resume: _ } => UNLOCK.to_owned(),
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
            ReviewPurpose::Update { .. } => UPDATE_REVIEW.to_owned(),
        },
        Dest::Bundle(_) => BUNDLE.to_owned(),
        Dest::OwnerPage(page) => format!("{}/owner", app(&page.package)),
        // Only a device that is not set up reaches this, and the web GUI cannot be on before
        // setup. The Backups page is where a restore is on a device that is.
        Dest::SetUpFromBackup => BACKUPS.to_owned(),
        Dest::Rejected(data) => format!("{}/rejected", offer(&data.id)),
        Dest::MissingCapability(data) => format!("{}/capability", offer(&data.id)),
        Dest::Installing(report) => format!("{}/installing", offer(&report.id)),
        Dest::UpdatePassphrase(_) => UPDATE_PASSPHRASE.to_owned(),
        Dest::UpdateResults(_) => UPDATE_RESULTS.to_owned(),
        Dest::Settings(form) => settings(&form.package),
        Dest::ApplyFailed(failed) => format!("{}/failed", settings(&failed.package)),
        // Saved: the list of sites shows who can reach it now.
        Dest::LeaveHosting { site: _ } => WEB.to_owned(),
        Dest::Reachability(Found::Problems(check)) => format!("{}/check", hosting(&check.site)),
        Dest::Reachability(Found::Cgnat(cgnat)) => format!("{}/cgnat", hosting(&cgnat.site)),
        Dest::Reachability(Found::Clear(confirm)) | Dest::PublicConfirm(confirm) => format!("{}/confirm", hosting(&confirm.site)),
        // It does not name its site, so, like `Back`, it leads where the page that can get it
        // says: the Hosting page that sent the owner to add a provider.
        Dest::HostingWithProvider { domains: _ } => back.to_owned(),
        // The page following the secure connection of the site that was just confirmed.
        Dest::WentPublic { site } => format!("{}/certificate", hosting(site)),
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
