//! First setup: the device passphrase, then the SSH keys that may log in.
//!
//! The SSH keys page is also opened on its own later, from Home or System, as [`Setup::keys`].

use super::{Confirm, Dest, Notice, Question, Step};
use crate::secret::{Input, Secret};
use crate::{Effect, Outcome};

/// The helper enforces this too. Checking here saves a round trip and a KDF run.
pub const MIN_PASSPHRASE_CHARS: usize = 8;

/// The pages of first setup, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStage {
    /// Choosing the device passphrase, typed twice.
    Passphrase,
    /// The SSH keys that may log in.
    Keys,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    pub stage: SetupStage,
    /// The keys that may log in, once read.
    pub keys: Vec<nodo_api::SshKey>,
    /// What is wrong with what was typed, or why the helper refused.
    pub error: Option<String>,
    /// The helper is working on this page's request. Actions are refused meanwhile.
    pub busy: bool,
}

/// Why a new device passphrase cannot be used, if it cannot.
pub fn passphrase_problem(passphrase: &Secret) -> Option<String> {
    (passphrase.expose().chars().count() < MIN_PASSPHRASE_CHARS).then(|| format!("Use at least {MIN_PASSPHRASE_CHARS} characters."))
}

impl Setup {
    /// First setup, at its first page.
    pub fn passphrase() -> Self {
        Self { stage: SetupStage::Passphrase, keys: Vec::new(), error: None, busy: false }
    }

    /// The SSH keys page, waiting for the keys. Whoever opens it runs [`Setup::load_keys`].
    pub fn keys() -> Self {
        Self { stage: SetupStage::Keys, keys: Vec::new(), error: None, busy: true }
    }

    pub fn load_keys() -> Vec<Effect> {
        vec![Effect::LoadSshKeys]
    }

    /// Checks the passphrase when it is typed the first time, for a frontend that asks for the
    /// repeat on a second step. False, with `error` set, when it cannot be used.
    pub fn first_entered(&mut self, first: &Secret) -> bool {
        self.error = passphrase_problem(first);
        self.error.is_none()
    }

    /// Sets the device passphrase, typed twice. When the two differ, or it is too short, both
    /// boxes are emptied, `error` says why and nothing is asked of the helper.
    pub fn choose_passphrase(&mut self, first: &mut Input, second: &mut Input) -> Vec<Effect> {
        if self.busy || self.stage != SetupStage::Passphrase {
            return Vec::new();
        }
        let problem = passphrase_problem(&first.secret_value()).or_else(|| {
            (first.secret_value() != second.secret_value()).then(|| "The two passphrases differ. Type them again.".to_owned())
        });
        if let Some(problem) = problem {
            self.error = Some(problem);
            first.clear();
            second.clear();
            return Vec::new();
        }
        self.error = None;
        self.busy = true;
        second.clear();
        vec![Effect::InitEncryption(first.take_secret())]
    }

    /// Adds a public key, given as its line of an `authorized_keys` file. An empty line adds
    /// nothing.
    pub fn add_key(&mut self, line: &str) -> Vec<Effect> {
        let line = line.trim();
        if self.busy || self.stage != SetupStage::Keys || line.is_empty() {
            return Vec::new();
        }
        self.busy = true;
        vec![Effect::AddSshKey(line.to_owned())]
    }

    /// Removing a key asks first. `None` when no such key is listed.
    pub fn ask_remove_key(&self, fingerprint: &str) -> Option<Question> {
        let key = self.keys.iter().find(|key| key.fingerprint == fingerprint)?;
        Some(Question::careful(
            "Remove this SSH key?",
            vec![
                format!("{} {}", key.algorithm, key.comment),
                key.fingerprint.clone(),
                "It will no longer be able to log in.".to_owned(),
            ],
            "Remove",
            Confirm::RemoveSshKey(key.fingerprint.clone()),
        ))
    }

    /// The owner confirmed a question this page asked: the removal is under way.
    pub fn confirmed(&mut self, confirm: &Confirm) {
        if matches!(confirm, Confirm::RemoveSshKey(_)) {
            self.busy = true;
        }
    }

    /// Leaves the keys page. The device is read again, since setup changed what Home shows.
    pub fn finish(&self) -> Step {
        Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])])
    }

    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::SetupDone(Ok(())) => {
                *self = Self::keys();
                Step::All(vec![
                    Step::Done(Notice::ok("Device passphrase set. Keep it somewhere safe.")),
                    Step::Run(Self::load_keys()),
                ])
            }
            // The passphrase is typed again from the start, with the helper's reason shown.
            Outcome::SetupDone(Err(error)) => {
                *self = Self::passphrase();
                self.error = Some(error.to_string());
                Step::Stay
            }
            Outcome::SshKeysLoaded(result) => {
                self.busy = false;
                match result {
                    Ok(list) if self.stage == SetupStage::Keys => self.keys = list.clone(),
                    Ok(_) => {}
                    Err(error) => self.error = Some(error.clone()),
                }
                Step::Stay
            }
            Outcome::SshKeyChanged(result) => {
                self.error = result.clone().err();
                Step::Stay
            }
            _ => Step::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::MockBackend;
    use crate::model::ScreenData;

    fn typed(text: &str) -> Input {
        let mut input = Input::secret();
        text.chars().for_each(|c| input.push(c));
        input
    }

    /// Runs the effects and hands every outcome to the page, following the effects its steps
    /// carry, as a frontend does. Returns the steps that were not plain `Stay`.
    fn run(mock: &mut MockBackend, setup: &mut Setup, effects: Vec<Effect>) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut queue = effects;
        while !queue.is_empty() {
            let mut next = Vec::new();
            for effect in queue {
                for outcome in execute_effect(mock, effect) {
                    let step = setup.apply(&outcome);
                    next.extend(step.effects());
                    if step != Step::Stay {
                        steps.push(step);
                    }
                }
            }
            queue = next;
        }
        steps
    }

    fn fresh() -> MockBackend {
        let mut mock = MockBackend::new();
        mock.setup_needed = true;
        mock
    }

    #[test]
    fn a_short_passphrase_is_refused_before_the_repeat() {
        let mut setup = Setup::passphrase();
        assert!(!setup.first_entered(&typed("short").secret_value()));
        assert_eq!(setup.error.as_deref(), Some("Use at least 8 characters."));
        assert!(setup.first_entered(&typed("long enough").secret_value()));
        assert_eq!(setup.error, None);
    }

    #[test]
    fn two_different_passphrases_start_over_and_ask_nothing_of_the_helper() {
        let mut setup = Setup::passphrase();
        let (mut first, mut second) = (typed("correct horse"), typed("correct house"));
        assert!(setup.choose_passphrase(&mut first, &mut second).is_empty());
        assert_eq!(setup.error.as_deref(), Some("The two passphrases differ. Type them again."));
        assert!(first.is_empty() && second.is_empty());
        assert!(!setup.busy);

        // A frontend that sends both at once gets the length rule too.
        let (mut first, mut second) = (typed("short"), typed("short"));
        assert!(setup.choose_passphrase(&mut first, &mut second).is_empty());
        assert_eq!(setup.error.as_deref(), Some("Use at least 8 characters."));
    }

    #[test]
    fn the_passphrase_sets_up_the_device_and_the_keys_page_follows() {
        let mut mock = fresh();
        let mut setup = Setup::passphrase();
        let (mut first, mut second) = (typed("correct horse"), typed("correct horse"));
        let effects = setup.choose_passphrase(&mut first, &mut second);
        assert!(matches!(effects[..], [Effect::InitEncryption(_)]));
        assert!(first.is_empty() && second.is_empty(), "the passphrase is not kept in the boxes");
        assert!(setup.busy);
        // While it runs, nothing else is started.
        assert!(setup.choose_passphrase(&mut typed("correct horse"), &mut typed("correct horse")).is_empty());

        let steps = run(&mut mock, &mut setup, effects);
        assert!(!mock.setup_needed);
        assert!(matches!(&steps[0], Step::All(all) if matches!(&all[0], Step::Done(notice) if notice.text.starts_with("Device passphrase set"))));
        // The keys were asked for and have arrived.
        assert_eq!(setup.stage, SetupStage::Keys);
        assert!(!setup.busy);
        assert_eq!(setup.keys, mock.ssh_keys().expect("the mock lists keys"));
        assert!(!load_snapshot(&mut mock).expect("the mock answers").overview.setup_needed);
    }

    #[test]
    fn a_refused_setup_returns_to_the_passphrase_with_the_reason() {
        let mut setup = Setup::passphrase();
        setup.busy = true;
        let refused = Outcome::SetupDone(Err(nodo_api::ApiError::Failed("no data drive".into())));
        assert_eq!(setup.apply(&refused), Step::Stay);
        assert_eq!(setup.stage, SetupStage::Passphrase);
        assert!(!setup.busy);
        assert!(setup.error.as_deref().is_some_and(|e| e.contains("no data drive")));
    }

    #[test]
    fn keys_are_added_and_removed_only_after_the_question() {
        let mut mock = MockBackend::new();
        let mut setup = Setup::keys();
        assert!(setup.add_key("ssh-ed25519 AAAA new@laptop").is_empty(), "nothing starts while the keys load");
        run(&mut mock, &mut setup, Setup::load_keys());
        let before = setup.keys.len();

        assert!(setup.add_key("   ").is_empty(), "an empty line adds nothing");
        let effects = setup.add_key("  ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGJ2 new@laptop ");
        assert_eq!(effects, [Effect::AddSshKey("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGJ2 new@laptop".into())]);
        assert!(setup.busy);
        run(&mut mock, &mut setup, effects);
        assert!(!setup.busy);
        assert_eq!(setup.keys.len(), before + 1, "{:?}", setup.error);

        let fingerprint = setup.keys.last().expect("just added").fingerprint.clone();
        assert_eq!(setup.ask_remove_key("no such fingerprint"), None);
        let question = setup.ask_remove_key(&fingerprint).expect("a listed key");
        assert!(question.cancel_is_default);
        assert_eq!(question.body[1], fingerprint);
        assert_eq!(mock.ssh_keys().expect("keys").len(), before + 1, "asking removes nothing");

        setup.confirmed(&question.confirm);
        assert!(setup.busy);
        let step = question.confirm.step(&load_snapshot(&mut mock).expect("the mock answers"));
        assert_eq!(step, Step::Run(vec![Effect::RemoveSshKey(fingerprint)]));
        run(&mut mock, &mut setup, step.effects());
        assert!(!setup.busy);
        assert_eq!(setup.keys.len(), before);
    }

    #[test]
    fn a_key_the_helper_refuses_shows_why_and_keeps_the_list() {
        let mut mock = MockBackend::new();
        let mut setup = Setup::keys();
        run(&mut mock, &mut setup, Setup::load_keys());
        let before = setup.keys.clone();
        let effects = setup.add_key("this is not a public key");
        run(&mut mock, &mut setup, effects);
        assert!(setup.error.is_some(), "the helper's reason is shown");
        assert_eq!(setup.keys, before);
        assert!(!setup.busy);
    }

    #[test]
    fn leaving_the_keys_reads_the_device_again() {
        assert_eq!(Setup::keys().finish(), Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])]));
    }
}
