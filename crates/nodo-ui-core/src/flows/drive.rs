//! The data drive is missing or cannot be used: look again, check it, repair it, or read what
//! the device knows about it.
//!
//! There is no state: the page is shown while `Snapshot::overview.drive` says why the drive
//! cannot be used, and every action answers with a document to read or a failure.

use super::{Confirm, Dest, Notice, Question, Step};
use crate::model::DriveAction;
use crate::{Effect, Outcome};

/// Looks for the drive again and mounts it if it is there now.
pub fn retry() -> Vec<Effect> {
    vec![Effect::Drive(DriveAction::Retry)]
}

/// Checks the filesystem without changing anything on it.
pub fn check() -> Vec<Effect> {
    vec![Effect::Drive(DriveAction::ReadOnlyCheck)]
}

/// Repairing can lose data, so it asks first.
pub fn ask_repair() -> Question {
    Question::careful(
        "Repair the filesystem?",
        vec![
            "Repairing rewrites damaged parts of the filesystem and can lose data.".to_owned(),
            "Run the read-only check first if you have not. Chain data can be synced again.".to_owned(),
        ],
        "Repair",
        Confirm::RepairDrive,
    )
}

/// What the device knows about its drives.
pub fn diagnostics() -> Vec<Effect> {
    vec![Effect::Drive(DriveAction::Diagnostics)]
}

/// A shell on the device, for whoever knows what to do there.
pub fn shell() -> Vec<Effect> {
    vec![Effect::Shell]
}

/// What a drive action came to: a report to read, or why it could not be done.
pub fn apply(outcome: &Outcome) -> Step {
    match outcome {
        Outcome::DriveResult(Ok(document)) => Step::Go(Dest::Document(document.clone())),
        Outcome::DriveResult(Err(text)) => Step::Failed(Notice::error(text.clone())),
        _ => Step::Stay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::execute_effect;
    use crate::mock::MockBackend;
    use crate::model::Snapshot;

    fn step_of(mock: &mut MockBackend, effects: Vec<Effect>) -> Step {
        let [effect] = <[Effect; 1]>::try_from(effects).expect("one effect");
        let outcomes = execute_effect(mock, effect);
        assert!(matches!(outcomes.last(), Some(Outcome::Refreshed(_))), "the device is read again after a drive action");
        apply(&outcomes[0])
    }

    #[test]
    fn each_action_answers_with_a_report_or_a_reason() {
        let mut mock = MockBackend::new();
        for effects in [retry(), check(), diagnostics()] {
            let step = step_of(&mut mock, effects.clone());
            assert!(matches!(step, Step::Go(Dest::Document(_)) | Step::Failed(_)), "{effects:?}: {step:?}");
        }
        assert!(matches!(step_of(&mut mock, check()), Step::Go(Dest::Document(doc)) if !doc.lines.is_empty()));
    }

    #[test]
    fn repairing_asks_first_and_starts_on_cancel() {
        let question = ask_repair();
        assert!(question.cancel_is_default);
        assert!(question.body[0].contains("can lose data"));
        assert_eq!(question.confirm, Confirm::RepairDrive);
        assert_eq!(question.confirm.step(&Snapshot::default()), Step::Run(vec![Effect::Drive(DriveAction::Repair)]));
    }

    #[test]
    fn a_failed_action_says_why() {
        let failed = Outcome::DriveResult(Err("no drive is attached".into()));
        assert_eq!(apply(&failed), Step::Failed(Notice::error("no drive is attached")));
        assert_eq!(apply(&Outcome::Info("something else".into())), Step::Stay);
    }
}
