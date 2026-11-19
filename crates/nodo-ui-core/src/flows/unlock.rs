//! Unlocking the device after a restart: the passphrase, and the wait after a wrong one.
//!
//! The wait is in seconds. A frontend keeps the clock: after a failure it notes that the next
//! try may come [`Unlock::retry_delay_seconds`] from now, and it tells [`Unlock::submit`] how
//! many of those seconds are left.

use nodo_api::{ApiError, ServiceState};

use super::{Dest, Notice, Step};
use crate::model::{counted, Snapshot};
use crate::secret::Input;
use crate::{Effect, Outcome};

/// Upper bound of the unlock retry delay. The helper must enforce its own limit as well, because
/// this one only protects against an impatient user at this screen.
pub const MAX_UNLOCK_DELAY_SECONDS: u64 = 30;

/// The owner asks to unlock: the unlock page while the device is locked, and a remark otherwise.
pub fn open(snapshot: &Snapshot) -> Step {
    if snapshot.is_locked() {
        Step::Go(Dest::Unlock { resume: None })
    } else {
        Step::Done(Notice::info("This device is already unlocked."))
    }
}

/// The unlock page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unlock {
    /// Why the last try was refused, shown beside the passphrase.
    pub error: Option<String>,
    /// Wrong tries in a row on this page.
    pub failures: u32,
    /// Why a page sent the owner here, in words for them. Once unlocked, that page carries on
    /// with what was asked, so it is not asked twice.
    pub resume: Option<String>,
}

impl Unlock {
    pub fn new(resume: Option<String>) -> Self {
        Self { resume, ..Self::default() }
    }

    /// Tries the passphrase typed. `wait_seconds` is what is left of the delay after the last
    /// failure, 0 when a try is allowed. A refused try sets `error` and leaves the box as it is;
    /// a try that goes ahead takes the passphrase out of the box.
    pub fn submit(&mut self, passphrase: &mut Input, wait_seconds: u64) -> Vec<Effect> {
        if wait_seconds > 0 {
            self.error = Some(format!("Please wait {} before trying again.", counted(wait_seconds, "second")));
            Vec::new()
        } else if passphrase.is_empty() {
            self.error = Some("Enter the device passphrase.".to_owned());
            Vec::new()
        } else {
            vec![Effect::Unlock(passphrase.take_secret())]
        }
    }

    /// How long the next try must wait after the failures so far: it doubles with each one, up
    /// to [`MAX_UNLOCK_DELAY_SECONDS`]. 0 before any failure.
    pub fn retry_delay_seconds(&self) -> u64 {
        if self.failures == 0 {
            return 0;
        }
        (1u64 << self.failures.min(16)).min(MAX_UNLOCK_DELAY_SECONDS)
    }

    /// What the helper answered. A failure counts, sets `error` and stays; the frontend then
    /// starts the wait of `retry_delay_seconds`. Success marks `snapshot` unlocked until it is
    /// read again, and leaves the page: back, or on with what `resume` named.
    pub fn apply(&mut self, outcome: &Outcome, snapshot: &mut Snapshot) -> Step {
        let Outcome::UnlockResult(result) = outcome else {
            return Step::Stay;
        };
        match result {
            Ok(_) => {
                mark_unlocked(snapshot);
                let (notice, dest) = if self.resume.is_some() {
                    ("Unlocked.", Dest::Resume)
                } else {
                    ("Unlocked. Services are starting in dependency order.", Dest::Back)
                };
                Step::All(vec![Step::Done(Notice::ok(notice)), Step::Go(dest), Step::Run(vec![Effect::Refresh])])
            }
            Err(error) => {
                self.failures += 1;
                self.error = Some(match error {
                    ApiError::BadPassphrase => "That passphrase did not unlock this device. Try again.".to_owned(),
                    other => format!("Could not unlock: {other}"),
                });
                Step::Stay
            }
        }
    }
}

/// Until the refresh arrives, the snapshot still says locked; the helper has the key now.
pub fn mark_unlocked(snapshot: &mut Snapshot) {
    snapshot.overview.locked = false;
    snapshot.overview.unlock_targets.clear();
    for svc in snapshot.services.iter_mut().filter(|svc| svc.state == ServiceState::Locked) {
        svc.state = ServiceState::Waiting;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::{MockBackend, MOCK_PASSPHRASE};

    fn typed(text: &str) -> Input {
        let mut input = Input::secret();
        text.chars().for_each(|c| input.push(c));
        input
    }

    fn outcome(mock: &mut MockBackend, effects: Vec<Effect>) -> Outcome {
        let [effect] = <[Effect; 1]>::try_from(effects).expect("one effect");
        execute_effect(mock, effect).remove(0)
    }

    #[test]
    fn opening_unlock_only_makes_sense_on_a_locked_device() {
        let mut mock = MockBackend::new();
        let mut snapshot = load_snapshot(&mut mock).expect("the mock answers");
        assert!(snapshot.is_locked(), "the mock starts locked");
        assert_eq!(open(&snapshot), Step::Go(Dest::Unlock { resume: None }));
        mark_unlocked(&mut snapshot);
        assert!(!snapshot.is_locked());
        assert!(matches!(open(&snapshot), Step::Done(notice) if notice.text.contains("already unlocked")));
    }

    #[test]
    fn an_empty_passphrase_is_not_sent() {
        let mut unlock = Unlock::default();
        let mut input = Input::secret();
        assert!(unlock.submit(&mut input, 0).is_empty());
        assert_eq!(unlock.error.as_deref(), Some("Enter the device passphrase."));
    }

    #[test]
    fn each_wrong_passphrase_doubles_the_wait_up_to_the_limit() {
        let mut mock = MockBackend::new();
        let mut snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let mut unlock = Unlock::default();
        assert_eq!(unlock.retry_delay_seconds(), 0);
        let mut delays = Vec::new();
        for _ in 0..6 {
            let mut input = typed("not the passphrase");
            let result = outcome(&mut mock, unlock.submit(&mut input, 0));
            assert!(input.is_empty(), "a tried passphrase is not kept");
            assert_eq!(unlock.apply(&result, &mut snapshot), Step::Stay);
            delays.push(unlock.retry_delay_seconds());
        }
        assert_eq!(delays, [2, 4, 8, 16, 30, 30]);
        assert_eq!(unlock.error.as_deref(), Some("That passphrase did not unlock this device. Try again."));
        assert!(snapshot.is_locked());
    }

    #[test]
    fn a_try_during_the_wait_is_refused_and_keeps_what_was_typed() {
        let mut unlock = Unlock { failures: 1, ..Unlock::default() };
        let mut input = typed(MOCK_PASSPHRASE);
        assert!(unlock.submit(&mut input, 2).is_empty());
        assert_eq!(unlock.error.as_deref(), Some("Please wait 2 seconds before trying again."));
        assert!(unlock.submit(&mut input, 1).is_empty());
        assert_eq!(unlock.error.as_deref(), Some("Please wait 1 second before trying again."));
        assert!(!input.is_empty());
        assert_eq!(unlock.submit(&mut input, 0).len(), 1);
    }

    #[test]
    fn the_right_passphrase_unlocks_and_leaves_the_page() {
        let mut mock = MockBackend::new();
        let mut snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let mut unlock = Unlock::default();
        let result = outcome(&mut mock, unlock.submit(&mut typed(MOCK_PASSPHRASE), 0));
        let step = unlock.apply(&result, &mut snapshot);
        let Step::All(steps) = &step else { panic!("{step:?}") };
        assert!(matches!(&steps[0], Step::Done(notice) if notice.text.starts_with("Unlocked. Services are starting")));
        assert_eq!(steps[1..], [Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])]);
        // The snapshot says unlocked before the refresh, and the refresh agrees.
        assert!(!snapshot.is_locked());
        assert!(snapshot.services.iter().all(|svc| svc.state != ServiceState::Locked));
        let fresh = load_snapshot(&mut mock).expect("the mock answers");
        assert!(!fresh.overview.locked);
    }

    #[test]
    fn unlocking_for_a_page_that_asked_carries_on_there() {
        let mut mock = MockBackend::new();
        let mut snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let mut unlock = Unlock::new(Some("The DNS key is stored encrypted.".to_owned()));
        let result = outcome(&mut mock, unlock.submit(&mut typed(MOCK_PASSPHRASE), 0));
        let Step::All(steps) = unlock.apply(&result, &mut snapshot) else { panic!("unlocked") };
        assert_eq!(steps[0], Step::Done(Notice::ok("Unlocked.")));
        assert_eq!(steps[1], Step::Go(Dest::Resume));
    }

    #[test]
    fn other_refusals_are_shown_in_the_helpers_words_and_other_outcomes_are_ignored() {
        let mut snapshot = Snapshot::default();
        let mut unlock = Unlock::default();
        let refused = Outcome::UnlockResult(Err(ApiError::Failed("the key file is missing".into())));
        assert_eq!(unlock.apply(&refused, &mut snapshot), Step::Stay);
        assert!(unlock.error.as_deref().is_some_and(|e| e.starts_with("Could not unlock: ") && e.contains("the key file is missing")));
        assert_eq!(unlock.failures, 1);

        let before = unlock.clone();
        assert_eq!(unlock.apply(&Outcome::Failed("something else".into()), &mut snapshot), Step::Stay);
        assert_eq!(unlock, before);
    }
}
