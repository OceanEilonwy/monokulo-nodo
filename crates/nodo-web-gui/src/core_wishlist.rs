//! CORE WISHLIST: what these pages still need that `nodo-ui-core` does not offer.
//!
//! The words every interface shares are in `nodo_ui_core::words` now, and the pages take them
//! from there. What is left here is a rule or a set of words the core has no call for yet. Each
//! item says why it is still local; none of it decides what an action does.
//!
//! 1. `login_error`: a refused web login has no flow of its own, so its words are here.
//! 2. `says_login_required`: effects answer failures as text, so "log in again" is searched for
//!    in it. The typed check (`WebGuiStatus.open`, `load_snapshot`) runs on every request.
//! 3. `typed_secret`: flows take a passphrase out of an `Input`; a form gives text, so a box is
//!    made only to hand it over.
//! 4. `open_url`: where an app's page opens (public name, else its home address) is worded only
//!    in the terminal's app page.
//! 5. `home_card`, `unlock_lines`: Home's cards and the unlock page's lines are the terminal's
//!    drawing code, not in `words`.
//! 6. `tab_badge`: the counts beside the tabs are computed in the terminal's tab strip.
//! 7. `nodo_update_note`, `update_note`, `status_word`, `chip_label`, `picker_hint`: short
//!    labels of the Updates, Get apps and settings pages, still in the terminal's views. So are
//!    the sentences of most pages here, copied where the page is drawn.
//! 8. `tone`: `words::cert_status` gives a `Tone`; the sign and the look for it are each
//!    interface's, which is as it should be, and listed only so the copy is known.
//! 9. `long_confirm`, `applies_updates`, `checks_or_goes_public`: which calls take minutes is
//!    read off a step's effects or known by habit. A flow could say "this is long".
//! 10. `job_failed`: a job that ended without a report is this interface's own case.
//! 11. `MockBackend` does not answer `WebGuiLogin`, `WebGuiLogout`, `WebGuiStatus`, and has no
//!     `RemovePackage`; `backends::MockBackends` plays the gate.
//! 12. Several notices of the core name keys of the terminal ("Press u to unlock", "Turn it on
//!     first: press o", "Press s to look for networks"). They are shown as they are.
//! 13. `Dest::WentPublic`, `LeaveHosting`, `HostingWithProvider` and `Confirm::DiscardSettings`
//!     do not name their site or app; `dest::url` and `Pending::subject` carry it instead.
//! 14. `Settings::pick` knows `Insert` and `Replace` only; "after the text" is remembered in
//!     `session::Picking::append` and placed with `insert_reference`.
//! 15. `Rollbacks::apply` gives no notice for a rollback the helper refused or took back; the
//!     line is worded in `pages::updates`.
//! 16. The certificate page after going public has no flow function for its heading.

use nodo_api::{ApiError, ServiceState};
use nodo_ui_core::flows::{Confirm, Notice};
use nodo_ui_core::model::{FieldKind, PackageStatus, Site, Snapshot, UpdateItem, UpdateNote, SYSTEM_BASE};
use nodo_ui_core::queries::HomeItem;
use nodo_ui_core::screen::Tab;
use nodo_ui_core::secret::Input;
use nodo_ui_core::words::{counted, Tone};
use nodo_ui_core::Outcome;

/// Why a login was refused, for the login page.
pub fn login_error(error: &ApiError) -> String {
    match error {
        ApiError::BadPassphrase => "That passphrase did not unlock this device. Try again.".to_owned(),
        ApiError::NotSetUp => "This Nodo has no device passphrase yet. Set it up on the device or over SSH first.".to_owned(),
        ApiError::Failed(text) if text == "the web GUI is turned off" => {
            "The web GUI is turned off. Turn it on over SSH with: nodo-tui --ctl web-gui 1".to_owned()
        }
        ApiError::RateLimited { retry_after_secs } => format!("Too many wrong tries. Try again in {}.", counted(*retry_after_secs, "second")),
        other => format!("Could not log in: {other}"),
    }
}

/// The helper refused because nobody is logged in any more. Typed errors are checked as such;
/// the rest reach an interface as text, which is searched for the refusal's exact words.
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

/// Where an app's page opens: its public name, or its address on the home network.
pub fn open_url(site: &Site) -> Option<String> {
    site.public_host
        .clone()
        .map(|host| format!("https://{host}"))
        .or_else(|| (!site.local_url.is_empty() && site.exposure.local).then(|| site.local_url.clone()))
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
    let restarted = if overview.restarted.is_empty() { "The Nodo restarted.".to_owned() } else { format!("{}.", overview.restarted.trim_end_matches('.')) };
    let what = if waiting.is_empty() { "Saved keys and secret settings stay" } else { "Your apps' data is" };
    let mut lines = vec![format!("{restarted} {what} locked until you type the device passphrase.")];
    if !waiting.is_empty() {
        lines.push(format!("{} start after you unlock.", waiting.join(", ")));
    }
    lines
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

/// What the Nodo's own update needs, beside its version.
pub fn nodo_update_note(update: &UpdateItem) -> &'static str {
    if update.note == UpdateNote::RestartNeeded { "needs a restart" } else { "no restart needed" }
}

/// What an update's row says after its versions. The bool marks one that wants the owner's eye.
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

/// An app on offer, in a word: installed already, or with an update.
pub fn status_word(status: PackageStatus) -> &'static str {
    match status {
        PackageStatus::Installed => "Installed",
        PackageStatus::UpdateReady => "Update ready",
        PackageStatus::Available => "",
    }
}

/// A reference as a field shows it.
pub fn chip_label(app: &str, setting: &str) -> String {
    format!("{app}·{setting}")
}

/// What the picker says above its list.
pub fn picker_hint(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::Number { .. } | FieldKind::Port => "Numbers first; the others are dimmed because a number field can't hold them.",
        _ => "Those that fit first; the others are dimmed with why.",
    }
}

/// The sign and the class these pages show a `Tone` of the core with.
pub fn tone(tone: Tone) -> (&'static str, &'static str) {
    match tone {
        Tone::Good => ("✓", "ok"),
        Tone::Working => ("◆", "warn"),
        Tone::Bad => ("✕", "err"),
    }
}

/// A confirmed action that can take minutes, with the words for it while it runs.
pub fn long_confirm(confirm: &Confirm) -> Option<&'static str> {
    match confirm {
        Confirm::RepairDrive => Some("Repairing the filesystem"),
        Confirm::RollBack { .. } => Some("Rolling back to the version before"),
        _ => None,
    }
}

/// Whether following a step asks the helper to apply updates, which takes minutes.
pub fn applies_updates(step: &nodo_ui_core::flows::Step) -> bool {
    step.effects().iter().any(|effect| {
        matches!(effect, nodo_ui_core::Effect::ApplyUpdates { .. } | nodo_ui_core::Effect::ApplyUpdatesWithPassphrase { .. })
    })
}

/// Whether following a step runs the reachability checks or puts a site on the internet, both
/// of which take long. A check may wait inside the saving of the names.
pub fn checks_or_goes_public(step: &nodo_ui_core::flows::Step) -> bool {
    use nodo_ui_core::effect::ExposureAfter;
    use nodo_ui_core::Effect;
    fn long(effect: &Effect) -> bool {
        match effect {
            Effect::CheckReachability { .. } | Effect::SetExposure { after: ExposureAfter::WentPublic, .. } => true,
            Effect::SetPublicHost { then, .. } => then.iter().any(long),
            _ => false,
        }
    }
    step.effects().iter().any(long)
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
    fn login_refusals_have_their_own_words() {
        assert!(login_error(&ApiError::NotSetUp).contains("no device passphrase yet"));
        assert!(login_error(&ApiError::Failed("the web GUI is turned off".into())).contains("turned off"));
        assert!(login_error(&ApiError::BadPassphrase).contains("did not unlock"));
    }
}
