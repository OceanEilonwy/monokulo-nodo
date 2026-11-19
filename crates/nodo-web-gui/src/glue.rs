//! What is this interface's own between a request and the core: no rule and no shared wording,
//! only what a web page needs that a terminal or a touchscreen does not.
//!
//! Everything a second interface would have to say or decide the same way is in
//! `nodo_ui_core` (`flows`, `words`, `Effect::is_long`). Each item here says why it is local.

use nodo_api::ApiError;
use nodo_ui_core::flows::{Confirm, Notice};
use nodo_ui_core::secret::Input;
use nodo_ui_core::words::Tone;
use nodo_ui_core::Outcome;

/// The helper refused because nobody is logged in any more. Local because only the web GUI's
/// socket gives this refusal; effects report failures as text, so beside the typed errors the
/// text is searched for the refusal's exact words. The typed check (`WebGuiStatus.open`,
/// `load_snapshot`) runs on every request.
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

/// The box a passphrase typed into a web form goes in. Local because a form posts text, while
/// the flows take a secret out of an `Input`, which the other interfaces type into key by key.
pub fn typed_secret(text: &str) -> Input {
    let mut input = Input::secret();
    text.chars().for_each(|c| input.push(c));
    input
}

/// The sign and the CSS class for a `Tone` of the core. Local because how a standing looks is
/// each interface's: a colour in the terminal, a class here.
pub fn tone(tone: Tone) -> (&'static str, &'static str) {
    match tone {
        Tone::Good => ("✓", "ok"),
        Tone::Working => ("◆", "warn"),
        Tone::Bad => ("✕", "err"),
    }
}

/// The heading of the page that waits while a confirmed action runs. Local because only this
/// interface has such a page; whether the action is long is the core's (`Confirm::is_long`).
pub fn waiting_for(confirm: &Confirm) -> &'static str {
    match confirm {
        Confirm::RepairDrive => "Repairing the filesystem",
        Confirm::RollBack { .. } => "Rolling back to the version before",
        _ => "Working",
    }
}

/// A job that ended without a report, as a notice. Local because jobs are this interface's way
/// of running a long call; the others wait for the answer.
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
    fn every_confirmation_that_takes_long_has_words_for_the_wait() {
        for confirm in [Confirm::RepairDrive, Confirm::RollBack { package: "tor".into(), to: "1".into() }] {
            assert!(confirm.is_long());
            assert_ne!(waiting_for(&confirm), "Working");
        }
    }
}
