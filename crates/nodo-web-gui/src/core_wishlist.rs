//! CORE WISHLIST: what these pages need that `nodo-ui-core` does not offer yet.
//!
//! Everything here is a copy of words or of a small rule that the terminal interface keeps in its
//! own drawing code (`crates/nodo-tui/src/view`), or something the core has no call for. None of
//! it should stay: each item belongs in the core, where both interfaces would read it, and this
//! module is deleted item by item as that happens. Nothing here decides what an action does.
//!
//! The list, for whoever moves it:
//!
//! 1. `login_error`: the words for a refused web login (a `flows::web_login`).
//! 2. `says_login_required`: effects answer with text, so "log in again" has to be found in it.
//!    An `Outcome` that keeps the typed `ApiError`, or a check in `exec`, would replace this.
//! 3. `typed_secret`: an `Input` from text typed elsewhere (`Input::from_secret`).
//! 4. `app_state`, `uptime`, `tagline`, `open_url`, `hosting_summary`, `log_summary`: an app in
//!    words, from `view/home.rs` and `view/apps.rs`.
//! 5. `home_card`, `unlock_lines`, `health_line`: Home's cards, from `view/home.rs` and the
//!    unlock page in `view/apps.rs`.
//! 6. `tab_badge`: the counts on the tabs, from `view/v3.rs`.
//! 7. `SYSTEM_ROWS`, `system_row_summary`, `nodo_update`: the System page, from `app.rs` and
//!    `view/system.rs`.
//! 8. `empty_catalog_line`, `status_word`: Get apps, from `view/apps.rs`.
//! 9. `fingerprint_groups`, `is_plain_http`: sources, from `view/v3.rs` and `view/sources.rs`.
//! 10. `MockBackend` does not answer `WebGuiLogin`, `WebGuiLogout` and `WebGuiStatus`, so
//!     `backends::MockBackends` plays the gate.
//! 11. `AppPage::start_or_stop` words a locked app as "Press u to unlock this device.", which is
//!     a key of the terminal.
//! 12. Which calls take long enough to be a job (`long_confirm`) is known only by habit.
//! 13. `MockBackend` has no `RemovePackage`, so removing an app in `--mock` and in the tests ends
//!     with the mock's "not available yet".
//! 15. `chip_label`, `picker_hint`, and the settings pages' own sentences (the grant question,
//!     the save confirmation's heading, the rolled-back page), from `view/settings.rs`.
//! 16. `update_note`, and the sentences of the Updates, results, automatic and undo pages, from
//!     `view/system.rs` and `view/more.rs`; `day` for a rollback's date.
//! 17. `applies_updates`: whether a step is long is read off its effects.
//! 18. A frontend without a text cursor has `insert_reference`, but `Settings::pick` and
//!     `Picker` know only `PickMode::Insert` and `Replace`; "after the text" is remembered here
//!     (`session::Picking::append`).
//! 19. `Confirm::DiscardSettings` does not say whose form; the session keeps the app's name
//!     beside the question (`Pending::subject`).
//! 20. `Rollbacks::apply` gives no notice for a rollback that the helper refused or took back;
//!     the result is kept for the page and worded here (`pages::updates::rolled_back_line`).
//! 14. `Review::approve` takes the passphrase as `Option<&mut Input>`, and `Unlock::submit` as
//!     `&mut Input`: a frontend that gets text from a form builds an `Input` only to hand it over.

use nodo_api::{ApiError, ServiceState};
use nodo_ui_core::flows::{Confirm, Notice};
use nodo_ui_core::model::{FeedStatus, PackageStatus, Site, Snapshot, UpdateItem, UpdateNote, SYSTEM_BASE};
use nodo_ui_core::queries::HomeItem;
use nodo_ui_core::screen::Tab;
use nodo_ui_core::secret::Input;
use nodo_ui_core::Outcome;

/// Why a login was refused, for the login page.
pub fn login_error(error: &ApiError) -> String {
    match error {
        ApiError::BadPassphrase => "That passphrase did not unlock this device. Try again.".to_owned(),
        ApiError::NotSetUp => {
            "This Nodo has no device passphrase yet. Set it up on the device or over SSH first.".to_owned()
        }
        ApiError::Failed(text) if text == "the web GUI is turned off" => {
            "The web GUI is turned off. Turn it on over SSH with: nodo-tui --ctl web-gui 1".to_owned()
        }
        ApiError::RateLimited { retry_after_secs } => {
            format!("Too many wrong tries. Try again in {}.", nodo_ui_core::model::counted(*retry_after_secs, "second"))
        }
        other => format!("Could not log in: {other}"),
    }
}

/// The helper refused because nobody is logged in any more: after it restarted, after its own
/// idle time, or once the web GUI was turned off. Typed errors are checked as such; the rest
/// reach an interface as text, which is searched for the refusal's exact words.
pub fn says_login_required(outcome: &Outcome) -> bool {
    let refusal = ApiError::web_login_required();
    match outcome {
        Outcome::UnlockResult(Err(error))
        | Outcome::SettingsApplied(Err(error))
        | Outcome::SetupDone(Err(error))
        | Outcome::Granted { result: Err(error), .. }
        | Outcome::ExposureSet { result: Err(error), .. }
        | Outcome::OwnerPageLoaded { result: Err(error), .. } => error.is_web_login_required(),
        Outcome::Failed(text) => text.contains(&refusal.to_string()),
        _ => false,
    }
}

/// The box a passphrase typed into a web form goes in, for the flows that take it out of one.
pub fn typed_secret(text: &str) -> Input {
    let mut input = Input::secret();
    text.chars().for_each(|c| input.push(c));
    input
}

/// An app's state in plain words, and whether it is a problem.
pub fn app_state(snapshot: &Snapshot, id: &str) -> (String, bool) {
    let Some(svc) = snapshot.services.iter().find(|svc| svc.id == id) else {
        return ("Not running".to_owned(), false);
    };
    let words = match svc.state {
        ServiceState::Running => {
            let mut parts = vec!["Running".to_owned()];
            parts.extend(svc.uptime_secs.map(|secs| format!("up {}", uptime(secs))));
            parts.extend((!svc.detail.is_empty()).then(|| svc.detail.clone()));
            parts.join(" · ")
        }
        ServiceState::Stopped => "Stopped".to_owned(),
        ServiceState::Failed => "Keeps stopping".to_owned(),
        ServiceState::Locked => "Locked until you unlock".to_owned(),
        ServiceState::Waiting => "Waiting for another app".to_owned(),
    };
    (words, svc.state == ServiceState::Failed)
}

/// How long an app has been up, in its two largest units, such as `3h 12m` or `2d 5h`.
pub fn uptime(secs: u64) -> String {
    let (days, hours, minutes) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60);
    let pair = |big: u64, big_unit: &str, small: u64, small_unit: &str| match small {
        0 => format!("{big}{big_unit}"),
        _ => format!("{big}{big_unit} {small}{small_unit}"),
    };
    match secs {
        s if s < 60 => "under a minute".to_owned(),
        s if s < 3600 => format!("{minutes}m"),
        s if s < 86400 => pair(hours, "h", minutes, "m"),
        _ => pair(days, "d", hours, "h"),
    }
}

/// What an app is for, from its source's description.
pub fn tagline(snapshot: &Snapshot, id: &str) -> String {
    snapshot.catalog.iter().find(|pkg| pkg.id.name == id).map(|pkg| pkg.tagline.clone()).unwrap_or_default()
}

/// Where an app's page opens: its public name, or its address on the home network.
pub fn open_url(site: &Site) -> Option<String> {
    site.public_host
        .clone()
        .map(|host| format!("https://{host}"))
        .or_else(|| (!site.local_url.is_empty() && site.exposure.local).then(|| site.local_url.clone()))
}

/// Where an app's web page can be reached, in a few words.
pub fn hosting_summary(snapshot: &Snapshot, name: &str) -> String {
    let Some(site) = snapshot.web.sites.iter().find(|s| s.package == name) else {
        return "No web page".to_owned();
    };
    let mut parts = Vec::new();
    if site.exposure.local {
        parts.push("Home network");
    }
    if site.exposure.onion {
        parts.push("Tor");
    }
    if site.exposure.public {
        parts.push("internet");
    }
    if parts.is_empty() {
        return "Not online".to_owned();
    }
    let joined = parts.join(", ");
    let mut chars = joined.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

pub fn log_summary(snapshot: &Snapshot, name: &str) -> &'static str {
    match snapshot.services.iter().find(|svc| svc.id == name).map(|svc| svc.state) {
        Some(ServiceState::Failed) => "It keeps stopping; the log says why",
        _ => "What it has been doing",
    }
}

/// One of Home's cards about something that needs the owner: its heading, what it means, and
/// the words on its one button. `None` for an app's row.
pub fn home_card(snapshot: &Snapshot, item: &HomeItem) -> Option<(String, String, &'static str)> {
    Some(match item {
        HomeItem::Unlock => (
            "Your Nodo is locked".to_owned(),
            if snapshot.services.iter().any(|svc| svc.state == ServiceState::Locked) {
                "Apps' data waits for the device passphrase. They start once you unlock.".to_owned()
            } else {
                "Saved keys and secret settings, such as a domain provider's, need the device passphrase.".to_owned()
            },
            "Unlock",
        ),
        HomeItem::Failing(id) => (
            format!("{id} keeps stopping"),
            "Its log usually says why. Settings or an update can be undone from its page.".to_owned(),
            "Show log",
        ),
        HomeItem::Updates => {
            let names: Vec<String> = snapshot
                .update_rows()
                .iter()
                .filter(|u| !u.held)
                .map(|u| if u.name == SYSTEM_BASE { "the Nodo".to_owned() } else { u.name.clone() })
                .collect();
            let count = names.len();
            (
                if count == 1 { "1 update is ready".to_owned() } else { format!("{count} updates are ready") },
                format!("{}. Each keeps its settings and can be undone.", names.join(", ")),
                "Review updates",
            )
        }
        HomeItem::App(_) => return None,
    })
}

/// What the unlock card says under its heading: when the Nodo restarted and what waits.
pub fn unlock_lines(snapshot: &Snapshot) -> Vec<String> {
    let overview = &snapshot.overview;
    let waiting: Vec<&str> = overview.unlock_targets.iter().map(|target| target.name.as_str()).collect();
    let restarted = if overview.restarted.is_empty() {
        "The Nodo restarted.".to_owned()
    } else {
        format!("{}.", overview.restarted.trim_end_matches('.'))
    };
    let what = if waiting.is_empty() { "Saved keys and secret settings stay" } else { "Your apps' data is" };
    let mut lines = vec![format!("{restarted} {what} locked until you type the device passphrase.")];
    if !waiting.is_empty() {
        lines.push(format!("{} start after you unlock.", waiting.join(", ")));
    }
    lines
}

/// The device's health in a few pairs: storage, temperature, uptime.
pub fn health(snapshot: &Snapshot) -> Vec<(String, String)> {
    snapshot.overview.device.iter().take(3).cloned().collect()
}

/// The count beside a tab's name: updates for apps on Apps, for the Nodo itself on System.
pub fn tab_badge(snapshot: &Snapshot, tab: Tab) -> usize {
    let count = |nodo: bool| snapshot.updates.iter().filter(|u| !u.held && (u.name == SYSTEM_BASE) == nodo).count();
    match tab {
        Tab::Apps => count(false),
        Tab::System => count(true),
        Tab::Home | Tab::Web => 0,
    }
}

/// The System page's rows, in the terminal interface's order.
pub const SYSTEM_ROWS: [&str; 5] = ["Updates", "Backups", "Network", "Device logs", "About this Nodo"];

/// The Nodo's own update, when one is offered and not held back.
pub fn nodo_update(snapshot: &Snapshot) -> Option<&UpdateItem> {
    snapshot.updates.iter().find(|item| item.name == SYSTEM_BASE && !item.held)
}

/// What the Nodo's own update needs, beside its version.
pub fn nodo_update_note(update: &UpdateItem) -> &'static str {
    if update.note == UpdateNote::RestartNeeded { "needs a restart" } else { "no restart needed" }
}

pub fn system_row_summary(snapshot: &Snapshot, row: &str) -> String {
    match row {
        "Updates" => {
            let apps = snapshot.updates.iter().filter(|u| u.name != SYSTEM_BASE && !u.held).count();
            let mut parts = Vec::new();
            if apps > 0 {
                parts.push(if apps == 1 { "1 for an app".to_owned() } else { format!("{apps} for apps") });
            }
            if nodo_update(snapshot).is_some() {
                parts.push("1 for the Nodo".to_owned());
            }
            let auto = snapshot.auto_update.enabled_count();
            let automatic = if auto == 0 { "automatic updates off".to_owned() } else { format!("automatic for {auto}") };
            if parts.is_empty() { format!("Up to date · {automatic}") } else { format!("{} · {automatic}", parts.join(", ")) }
        }
        "Backups" => "Make one now, or on a schedule".to_owned(),
        "Network" => "Cable, Wi-Fi and Bluetooth".to_owned(),
        "Device logs" => "What the Nodo and its apps have been doing".to_owned(),
        _ => snapshot.overview.device.iter().take(2).map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(" · "),
    }
}

/// What Get apps says when it lists nothing.
pub fn empty_catalog_line(snapshot: &Snapshot, query: &str) -> String {
    if !snapshot.catalog.is_empty() || !query.is_empty() {
        return "Nothing matches that search.".to_owned();
    }
    let enabled: Vec<_> = snapshot.feeds.iter().filter(|f| f.status != FeedStatus::Disabled).collect();
    if let Some((name, problem)) = enabled.iter().find_map(|f| match &f.status {
        FeedStatus::Unreachable(problem) => Some((&f.name, problem)),
        _ => None,
    }) {
        return format!("Could not read the source {name}: {problem}");
    }
    if enabled.iter().any(|f| f.checked.is_empty()) {
        return "Checking sources…".to_owned();
    }
    if enabled.is_empty() {
        return "No sources are on. Add or turn one on under Sources.".to_owned();
    }
    "The sources offer no apps yet.".to_owned()
}

/// An app on offer, in a word: installed already, or with an update.
pub fn status_word(status: PackageStatus) -> &'static str {
    match status {
        PackageStatus::Installed => "Installed",
        PackageStatus::UpdateReady => "Update ready",
        PackageStatus::Available => "",
    }
}

/// A key fingerprint as groups of eight, to compare by eye.
pub fn fingerprint_groups(fp: &str) -> Vec<String> {
    // A "SHA256:" or similar label is not part of the key; its digits must not join the groups.
    let body = fp.rsplit_once(": ").map_or(fp, |(_, rest)| rest);
    let body = body.split_once("256:").map_or(body, |(_, rest)| rest);
    let hex: String = body.chars().filter(char::is_ascii_hexdigit).collect();
    hex.as_bytes().chunks(8).map(|c| String::from_utf8_lossy(c).into_owned()).collect()
}

pub fn is_plain_http(address: &str) -> bool {
    address.starts_with("http://")
}

/// A confirmed action that can take minutes, with the words for it while it runs. Everything
/// else is quick enough to answer within the request.
pub fn long_confirm(confirm: &Confirm) -> Option<&'static str> {
    match confirm {
        Confirm::RepairDrive => Some("Repairing the filesystem"),
        Confirm::RollBack { .. } => Some("Rolling back to the version before"),
        _ => None,
    }
}

/// A reference as a field shows it, from `view/settings.rs`.
pub fn chip_label(app: &str, setting: &str) -> String {
    format!("{app}·{setting}")
}

/// What the picker says above its list, from `view/settings.rs`.
pub fn picker_hint(kind: &nodo_ui_core::model::FieldKind) -> &'static str {
    use nodo_ui_core::model::FieldKind;
    match kind {
        FieldKind::Number { .. } | FieldKind::Port => "Numbers first; the others are dimmed because a number field can't hold them.",
        _ => "Those that fit first; the others are dimmed with why.",
    }
}

/// What an update's row says after its versions, from `view/system.rs`. The bool marks one that
/// wants the owner's eye.
pub fn update_note(item: &UpdateItem) -> (String, bool) {
    match (&item.note, item.held) {
        (UpdateNote::Skipped, _) => ("you skipped this version".to_owned(), false),
        (_, true) => ("held back".to_owned(), false),
        (UpdateNote::Ready, _) if item.grows.is_some() => ("needs the device passphrase".to_owned(), true),
        (UpdateNote::Ready, _) => (String::new(), false),
        (UpdateNote::RestartNeeded, _) => ("needs a restart".to_owned(), false),
        (UpdateNote::NewPermission(_), _) => ("▲ asks for something new; you'll review it first".to_owned(), true),
        (UpdateNote::Blocked(reason), _) => (format!("▲ {reason}"), true),
        (UpdateNote::Withdrawal(_), _) => ("▲ replaces a withdrawn version".to_owned(), true),
    }
}

/// A day as `2026-11-07`, from seconds since 1970 (Howard Hinnant's days-to-civil), from
/// `view/more.rs`.
pub fn day(at: u64) -> String {
    let z = (at / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02}")
}

/// Whether following a step asks the helper to apply updates, which takes minutes. Loading a
/// review does not.
pub fn applies_updates(step: &nodo_ui_core::flows::Step) -> bool {
    step.effects().iter().any(|effect| {
        matches!(effect, nodo_ui_core::Effect::ApplyUpdates { .. } | nodo_ui_core::Effect::ApplyUpdatesWithPassphrase { .. })
    })
}

/// A job that ended without a report, as a notice.
pub fn job_failed(what: &str, reason: &str) -> Notice {
    Notice::error(format!("{what} did not finish: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_refusal_is_found_in_typed_and_in_worded_failures() {
        let refusal = ApiError::web_login_required();
        assert!(says_login_required(&Outcome::UnlockResult(Err(refusal.clone()))));
        assert!(says_login_required(&Outcome::Failed(format!("Could not read device state: {refusal}"))));
        assert!(!says_login_required(&Outcome::Failed("locked: unlock the device to use the DNS provider".into())));
        assert!(!says_login_required(&Outcome::UnlockResult(Err(ApiError::BadPassphrase))));
    }

    #[test]
    fn fingerprints_are_grouped_without_their_label() {
        assert_eq!(fingerprint_groups("SHA256:c41e8d072fa966b3"), ["c41e8d07", "2fa966b3"]);
    }

    #[test]
    fn login_refusals_have_their_own_words() {
        assert!(login_error(&ApiError::NotSetUp).contains("no device passphrase yet"));
        assert!(login_error(&ApiError::Failed("the web GUI is turned off".into())).contains("turned off"));
        assert!(login_error(&ApiError::BadPassphrase).contains("did not unlock"));
    }
}
