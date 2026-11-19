//! Updates: which are ticked, checking for more, applying a batch, what the batch came to,
//! the automatic-update choices, and putting an app back to the version before its last update.
//!
//! The Updates page has no state but the names the owner ticked, so its rules are functions over
//! that list and the device's `Snapshot`. A batch goes through [`update_apps`] (or
//! [`update_system`] for the Nodo's own update), which sees to the order: an update that asks
//! for something new is reviewed and approved first (`flows::install`'s review, with
//! `ReviewPurpose::Update`), and a batch in which an update grows encrypted storage asks for the
//! device passphrase ([`Batch`]) before anything is sent.
//!
//! [`AutoUpdate`] is the page of automatic-update choices and [`Rollbacks`] the page that undoes
//! an update. What a batch came to is an `UpdateReport`; the functions about it take that.

use nodo_api::UpdateOutcome;

use super::{Confirm, Dest, Notice, Question, Step};
use crate::effect::RollbackView;
use crate::model::{AutoUpdatePrefs, DocKind, Snapshot, UpdateNote, UpdateReport, SYSTEM_BASE};
use crate::secret::Input;
use crate::{Effect, Outcome};

// The Updates page

/// The updates ticked when the page opens: those the helper marks as safe to take, never one
/// that is held. `only` names the one app whose update the owner came for, which is then the
/// only one ticked.
pub fn ticked_at_open(snapshot: &Snapshot, only: Option<&str>) -> Vec<String> {
    match only {
        Some(name) => vec![name.to_owned()],
        None => snapshot.update_rows().iter().filter(|item| item.preselected && !item.held).map(|item| item.name.clone()).collect(),
    }
}

/// Carries the ticks over when the device was read again, which a check can change: updates
/// that are gone are unticked, and new ones are ticked as the page ticks them when it opens.
/// `shown_before` names the updates the page listed until now.
pub fn carry_ticks(checked: &mut Vec<String>, shown_before: &[String], snapshot: &Snapshot) {
    let rows = snapshot.update_rows();
    checked.retain(|name| rows.iter().any(|item| item.name == *name));
    for item in rows {
        if item.preselected && !item.held && !shown_before.contains(&item.name) && !checked.contains(&item.name) {
            checked.push(item.name.clone());
        }
    }
}

/// Ticks or unticks one update. Ticking a held one releases it, since a held update is never
/// applied.
pub fn tick(checked: &mut Vec<String>, snapshot: &Snapshot, name: &str) -> Vec<Effect> {
    let Some(item) = snapshot.updates.iter().find(|item| item.name == name) else {
        return Vec::new();
    };
    if let Some(position) = checked.iter().position(|ticked| ticked == name) {
        checked.remove(position);
        return Vec::new();
    }
    checked.push(name.to_owned());
    if item.held { vec![Effect::SetHeld { name: name.to_owned(), held: false }] } else { Vec::new() }
}

/// Holds an update back, or releases it. Either way it is unticked: a held one is not applied,
/// and a released one is the owner's to tick.
pub fn set_held(checked: &mut Vec<String>, name: &str, held: bool) -> Vec<Effect> {
    checked.retain(|ticked| ticked != name);
    vec![Effect::SetHeld { name: name.to_owned(), held }]
}

/// Check now: the sources and the system snapshot are asked what is new. See [`apply`].
pub fn check_now() -> Vec<Effect> {
    vec![Effect::CheckUpdates]
}

/// What an update changes, as a document.
pub fn changelog(name: &str) -> Vec<Effect> {
    vec![Effect::ShowDocument(DocKind::Changelog(name.to_owned()))]
}

/// The Nodo's own update, which is never part of the apps' batch.
pub fn update_system(snapshot: &Snapshot) -> Step {
    if snapshot.updates.iter().any(|item| item.name == SYSTEM_BASE && !item.held) {
        apply_batch(snapshot, vec![SYSTEM_BASE.to_owned()], Vec::new())
    } else {
        Step::Done(Notice::info("The Nodo is up to date."))
    }
}

/// Updates the apps ticked. Those that ask for a new permission are reviewed first, one after
/// the other, even though their box is ticked; each approval is gathered and the batch goes on
/// once the last is given ([`apply_batch`]).
pub fn update_apps(snapshot: &Snapshot, checked: &[String]) -> Step {
    let rows = snapshot.update_rows();
    // The order of the list, not the order the boxes were ticked in.
    let batch: Vec<String> =
        rows.iter().filter(|item| item.name != SYSTEM_BASE && checked.contains(&item.name)).map(|item| item.name.clone()).collect();
    if batch.is_empty() {
        return Step::Done(Notice::info("Tick at least one app first."));
    }
    let mut pending_reviews: Vec<String> = rows
        .iter()
        .filter(|item| batch.contains(&item.name) && matches!(item.note, UpdateNote::NewPermission(_)))
        .map(|item| item.name.clone())
        .collect();
    if pending_reviews.is_empty() {
        return apply_batch(snapshot, batch, Vec::new());
    }
    let name = pending_reviews.remove(0);
    Step::Run(vec![Effect::ReviewUpdate { name, batch, pending_reviews, approvals: Vec::new() }])
}

/// Applies a batch of updates with the approvals gathered for it. When one of them grows
/// encrypted storage, the device passphrase is asked for first (`Dest::UpdatePassphrase`),
/// which LUKS needs to resize the volume.
pub fn apply_batch(snapshot: &Snapshot, names: Vec<String>, approvals: Vec<(String, nodo_api::PermissionApproval)>) -> Step {
    let growing: Vec<String> = snapshot
        .updates
        .iter()
        .filter(|item| names.contains(&item.name))
        .filter_map(|item| item.grows.as_ref().map(|grows| format!("{}: {grows}", item.name)))
        .collect();
    let batch = Batch { names, approvals, growing };
    if batch.growing.is_empty() { Step::Run(batch.without_passphrase()) } else { Step::Go(Dest::UpdatePassphrase(batch)) }
}

/// A batch of updates that waits for the device passphrase, because some of them grow encrypted
/// storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub names: Vec<String>,
    pub approvals: Vec<(String, nodo_api::PermissionApproval)>,
    /// Each growing update in words, such as "vault: 256 MB to 512 MB".
    pub growing: Vec<String>,
}

impl Batch {
    /// Applies the batch with the passphrase typed, which is taken out of the box. An empty box
    /// is refused with a remark and nothing is sent.
    pub fn submit(&self, passphrase: &mut Input) -> Step {
        if passphrase.is_empty() {
            return Step::Done(Notice::info("Enter the device passphrase to grow the encrypted storage."));
        }
        Step::Run(vec![Effect::ApplyUpdatesWithPassphrase {
            names: self.names.clone(),
            approvals: self.approvals.clone(),
            passphrase: passphrase.take_secret(),
        }])
    }

    /// Applies the batch as it is. For a batch that grows storage this is only for a frontend
    /// that has nowhere to ask: the helper refuses a growing update without the passphrase, and
    /// says why.
    pub fn without_passphrase(&self) -> Vec<Effect> {
        vec![Effect::ApplyUpdates { names: self.names.clone(), approvals: self.approvals.clone() }]
    }
}

/// What a result means on the Updates page and wherever a batch was started: how a check went,
/// in words; what a batch came to, on its own page (`Dest::UpdateResults`); and automatic-update
/// choices saved away from their page. `snapshot` is the device as read after the result.
pub fn apply(outcome: &Outcome, snapshot: &Snapshot) -> Step {
    match outcome {
        Outcome::UpdatesChecked(report) => {
            let summary = report.summary(snapshot.update_rows().len(), &snapshot.feeds);
            match (summary.trouble, summary.found) {
                (true, _) => Step::Failed(Notice::error(summary.text)),
                (false, 0) => Step::Done(Notice::ok(summary.text)),
                (false, _) => Step::Done(Notice::info(summary.text)),
            }
        }
        Outcome::UpdatesApplied(report) => Step::Go(Dest::UpdateResults(report.clone())),
        Outcome::AutoUpdateSaved(Ok(())) => Step::Done(saved_notice()),
        Outcome::AutoUpdateSaved(Err(text)) => Step::Failed(Notice::error(text.clone())),
        _ => Step::Stay,
    }
}

// What a batch came to

/// Leaving the results: back, and the device is read again.
pub fn leave_results() -> Step {
    Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])])
}

/// Holds the update that failed, so it is not tried again until the owner releases it. Leaves
/// the results as [`leave_results`] does when nothing failed.
pub fn hold_failed(report: &UpdateReport) -> Step {
    match &report.failure {
        Some(failure) => {
            Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::SetHeld { name: failure.name.clone(), held: true }])])
        }
        None => leave_results(),
    }
}

/// Tries the update that failed again. A failed version is held, and the helper only retries it
/// once released.
pub fn retry_failed(report: &UpdateReport, snapshot: &Snapshot) -> Step {
    let Some(failure) = &report.failure else {
        return Step::Stay;
    };
    let name = failure.name.clone();
    Step::All(vec![Step::Run(vec![Effect::SetHeld { name: name.clone(), held: false }]), apply_batch(snapshot, vec![name], Vec::new())])
}

/// A report of the failed update, for whoever published it.
pub fn report_failed(report: &UpdateReport) -> Vec<Effect> {
    report.failure.iter().map(|failure| Effect::CopyReport(format!("{} update", failure.name))).collect()
}

// Automatic updates

/// Parses `HH:MM to HH:MM`. A window may cross midnight, so only an empty one is refused.
pub fn parse_window(text: &str) -> Option<(String, String)> {
    fn time(text: &str) -> Option<String> {
        let (hours, minutes) = text.trim().split_once(':')?;
        if hours.len() != 2 || minutes.len() != 2 {
            return None;
        }
        let (hours, minutes) = (hours.parse::<u8>().ok()?, minutes.parse::<u8>().ok()?);
        (hours < 24 && minutes < 60).then(|| format!("{hours:02}:{minutes:02}"))
    }
    let (start, end) = text.split_once(" to ")?;
    let (start, end) = (time(start)?, time(end)?);
    (start != end).then_some((start, end))
}

fn saved_notice() -> Notice {
    Notice::ok("Auto-update choices saved.")
}

/// The automatic-update choices: how often, when, and for which apps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoUpdate {
    /// What is stored.
    pub saved: AutoUpdatePrefs,
    /// What Save would store.
    pub draft: AutoUpdatePrefs,
    /// Why the window typed was refused, shown beside it.
    pub error: Option<String>,
}

impl AutoUpdate {
    pub fn new(prefs: AutoUpdatePrefs) -> Self {
        Self { saved: prefs.clone(), draft: prefs, error: None }
    }

    /// The next of the frequencies on offer, going round.
    pub fn next_frequency(&mut self) {
        let prefs = &mut self.draft;
        let position = prefs.frequencies.iter().position(|option| option == &prefs.frequency).unwrap_or(0);
        if let Some(next) = prefs.frequencies.get((position + 1) % prefs.frequencies.len().max(1)) {
            prefs.frequency = next.clone();
        }
    }

    /// One of the frequencies on offer, by name. Any other name changes nothing.
    pub fn set_frequency(&mut self, frequency: &str) {
        if self.draft.frequencies.iter().any(|option| option == frequency) {
            self.draft.frequency = frequency.to_owned();
        }
    }

    /// Turns automatic updates on or off for one app, by name.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) {
        if let Some(entry) = self.draft.packages.iter_mut().find(|entry| entry.name == name) {
            entry.enabled = enabled;
        }
    }

    /// Flips automatic updates for one app, by name.
    pub fn toggle(&mut self, name: &str) {
        if let Some(entry) = self.draft.packages.iter_mut().find(|entry| entry.name == name) {
            entry.enabled ^= true;
        }
    }

    /// The window as it is typed: `03:00 to 05:00`.
    pub fn window(&self) -> String {
        format!("{} to {}", self.draft.window_start, self.draft.window_end)
    }

    /// Takes the window typed. Refused, with `error` set, unless it is two different times of
    /// day. Returns whether it was taken.
    pub fn set_window(&mut self, text: &str) -> bool {
        match parse_window(text) {
            Some((start, end)) => {
                self.draft.window_start = start;
                self.draft.window_end = end;
                self.error = None;
                true
            }
            None => {
                self.error = Some("Use two different device times, like 03:00 to 05:00.".to_owned());
                false
            }
        }
    }

    /// What the automatic updates did, as a document.
    pub fn history() -> Vec<Effect> {
        vec![Effect::ShowDocument(DocKind::AutoUpdateHistory)]
    }

    pub fn changed(&self) -> bool {
        self.draft != self.saved
    }

    /// Stores the choices.
    pub fn save(&self) -> Vec<Effect> {
        vec![Effect::SaveAutoUpdate(self.draft.clone())]
    }

    /// Leaving the page: at once when nothing was changed, after a question otherwise.
    pub fn leave(&self) -> Step {
        if self.changed() { Step::Ask(ask_discard_auto_update()) } else { Step::Go(Dest::Back) }
    }

    /// What saving came to: stored, and the page is left; or refused, with the reason.
    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::AutoUpdateSaved(Ok(())) => Step::All(vec![Step::Go(Dest::Back), Step::Done(saved_notice())]),
            Outcome::AutoUpdateSaved(Err(text)) => Step::Failed(Notice::error(text.clone())),
            _ => Step::Stay,
        }
    }
}

/// The question before leaving the automatic-update choices unsaved.
pub fn ask_discard_auto_update() -> Question {
    Question::careful(
        "Discard your changes?",
        vec!["Your auto-update choices will not be saved.".to_owned()],
        "Discard",
        Confirm::DiscardAutoUpdate,
    )
}

// Undoing an update

/// The question before an app is put back to the version from before its last update.
pub fn ask_roll_back(option: &nodo_api::RollbackOption) -> Question {
    Question::careful(
        format!("Roll {} back?", option.package),
        vec![
            format!("{} goes from {} back to {}.", option.package, option.current, option.previous),
            "Its data is copied first and put back if the old version fails.".to_owned(),
            format!("{} is held, so automatic updates leave it alone.", option.current),
        ],
        "Roll back",
        Confirm::RollBack { package: option.package.clone(), to: option.previous.clone() },
    )
}

/// The page that undoes an update: the versions that can be put back, and the rollbacks that
/// happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rollbacks {
    pub view: Option<RollbackView>,
    /// Why the list could not be read, or why the last thing asked was refused.
    pub error: Option<String>,
    /// The list is being read or a rollback is running: nothing else is taken meanwhile.
    pub busy: bool,
    /// The last rollback's outcome, shown until the next action.
    pub result: Option<nodo_api::UpdateResult>,
}

impl Rollbacks {
    /// The page as it opens, waiting for what [`Rollbacks::load`] reads.
    pub fn new() -> Self {
        Self { view: None, error: None, busy: true, result: None }
    }

    /// Reads what can be rolled back, for a page that opens.
    pub fn load() -> Vec<Effect> {
        vec![Effect::LoadRollbacks]
    }

    /// Reads the list again. Nothing while the page is busy.
    pub fn reload(&mut self) -> Vec<Effect> {
        if self.busy {
            return Vec::new();
        }
        self.busy = true;
        self.error = None;
        Self::load()
    }

    /// The versions that can be put back, as far as they are known.
    pub fn options(&self) -> &[nodo_api::RollbackOption] {
        self.view.as_ref().map(|view| view.options.as_slice()).unwrap_or_default()
    }

    /// Rolls an app back, by name, once the owner confirms. Refused, with `error` set, for an
    /// app that has no version kept or whose rollback is blocked now. Nothing while the page is
    /// busy.
    pub fn roll_back(&mut self, package: &str) -> Step {
        if self.busy {
            return Step::Stay;
        }
        let Some(option) = self.options().iter().find(|option| option.package == package).cloned() else {
            self.error = Some("Nothing can be rolled back now.".to_owned());
            return Step::Stay;
        };
        if let Some(reason) = &option.blocked {
            self.error = Some(format!("{} cannot roll back now: {reason}.", option.package));
            return Step::Stay;
        }
        Step::Ask(ask_roll_back(&option))
    }

    /// The owner confirmed a rollback asked here: it shows as running.
    pub fn confirmed(&mut self, confirm: &Confirm) {
        if matches!(confirm, Confirm::RollBack { .. }) {
            self.busy = true;
            self.error = None;
            self.result = None;
        }
    }

    /// Leaving the page: back, and the device is read again, since a rollback changes it.
    pub fn leave(&self) -> Step {
        Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])])
    }

    /// What a result means: the list that was read, and how a rollback went. After one that the
    /// helper carried out the list and the device are read again.
    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::RollbacksLoaded(result) => {
                self.busy = false;
                match result {
                    Ok(view) => self.view = Some(view.clone()),
                    Err(error) => self.error = Some(error.clone()),
                }
                Step::Stay
            }
            Outcome::RolledBack(Ok(outcome)) => {
                self.result = Some(outcome.clone());
                self.busy = true;
                let read_again = Step::Run(vec![Effect::LoadRollbacks, Effect::Refresh]);
                match outcome.outcome {
                    UpdateOutcome::Updated => Step::All(vec![
                        Step::Done(Notice::ok(format!("{} is back on {}.", outcome.package, outcome.to))),
                        read_again,
                    ]),
                    _ => read_again,
                }
            }
            Outcome::RolledBack(Err(error)) => {
                self.busy = false;
                Step::Failed(Notice::error(error.clone()))
            }
            _ => Step::Stay,
        }
    }
}

impl Default for Rollbacks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::flows::install::{self, ReviewPurpose};
    use crate::mock::{MockBackend, MOCK_PASSPHRASE};

    fn device() -> (MockBackend, Snapshot) {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        (mock, snapshot)
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    /// Performs one effect and reads the device again, as a frontend's refresh does.
    fn perform(mock: &mut MockBackend, snapshot: &mut Snapshot, effect: Effect) -> Vec<Outcome> {
        let outcomes = execute_effect(mock, effect);
        *snapshot = load_snapshot(mock).expect("the mock answers");
        outcomes.into_iter().filter(|outcome| !matches!(outcome, Outcome::Refreshed(_))).collect()
    }

    fn typed(text: &str) -> Input {
        let mut input = Input::secret();
        text.chars().for_each(|c| input.push(c));
        input
    }

    #[test]
    fn the_page_opens_with_the_safe_updates_ticked_and_never_a_held_one() {
        let (_, mut snapshot) = device();
        assert_eq!(ticked_at_open(&snapshot, None), names(&["moneropay", SYSTEM_BASE]));
        assert_eq!(ticked_at_open(&snapshot, Some("wallet-relay")), names(&["wallet-relay"]));
        for item in &mut snapshot.updates {
            item.preselected = true;
        }
        assert!(!ticked_at_open(&snapshot, None).contains(&"cuprated".to_owned()), "cuprated is held");
    }

    #[test]
    fn ticks_follow_the_updates_when_the_device_is_read_again() {
        let (_, mut snapshot) = device();
        let shown: Vec<String> = snapshot.update_rows().iter().map(|item| item.name.clone()).collect();
        let mut checked = names(&["moneropay", "wallet-relay"]);
        snapshot.updates.retain(|item| item.name != "moneropay");
        let mut found = snapshot.updates[0].clone();
        found.name = "fast-sync".to_owned();
        found.preselected = true;
        snapshot.updates.push(found);
        carry_ticks(&mut checked, &shown, &snapshot);
        assert_eq!(checked, names(&["wallet-relay", "fast-sync"]), "the gone one is unticked, the new one ticked");
        // An update the owner unticked stays unticked: it was shown before.
        let mut unticked = Vec::new();
        carry_ticks(&mut unticked, &names(&[SYSTEM_BASE, "wallet-relay", "cuprated", "fast-sync"]), &snapshot);
        assert!(unticked.is_empty());
    }

    #[test]
    fn ticking_a_held_update_releases_it_and_holding_one_unticks_it() {
        let (mut mock, mut snapshot) = device();
        let mut checked = ticked_at_open(&snapshot, None);
        assert!(tick(&mut checked, &snapshot, "moneropay").is_empty());
        assert_eq!(checked, names(&[SYSTEM_BASE]));
        assert!(tick(&mut checked, &snapshot, "moneropay").is_empty());
        assert!(tick(&mut checked, &snapshot, "nobody").is_empty());
        assert_eq!(checked, names(&[SYSTEM_BASE, "moneropay"]));

        let release = tick(&mut checked, &snapshot, "cuprated");
        assert_eq!(release, [Effect::SetHeld { name: "cuprated".to_owned(), held: false }]);
        assert!(checked.contains(&"cuprated".to_owned()));
        assert!(perform(&mut mock, &mut snapshot, release[0].clone()).is_empty());
        assert!(!snapshot.updates.iter().any(|item| item.held));

        let hold = set_held(&mut checked, "moneropay", true);
        assert_eq!(hold, [Effect::SetHeld { name: "moneropay".to_owned(), held: true }]);
        assert!(!checked.contains(&"moneropay".to_owned()), "a held update is not applied");
        assert!(perform(&mut mock, &mut snapshot, hold[0].clone()).is_empty());
        assert_eq!(snapshot.update_rows().last().map(|item| item.name.as_str()), Some("moneropay"));
        assert_eq!(changelog("moneropay"), [Effect::ShowDocument(DocKind::Changelog("moneropay".to_owned()))]);
    }

    #[test]
    fn check_now_says_what_was_found_and_what_could_not_be_checked() {
        let (mut mock, mut snapshot) = device();
        // Every source can be read, so that nothing but the count is said.
        mock.feeds.retain(|feed| !matches!(feed.status, crate::model::FeedStatus::Unreachable(_)));
        let outcomes = perform(&mut mock, &mut snapshot, check_now().remove(0));
        let [checked] = <[Outcome; 1]>::try_from(outcomes).expect("one result besides the refresh");
        let Step::Done(notice) = apply(&checked, &snapshot) else { panic!("a check that worked") };
        assert_eq!(notice, Notice::info(format!("{} updates available.", snapshot.update_rows().len())));

        mock.updates.clear();
        let outcomes = perform(&mut mock, &mut snapshot, check_now().remove(0));
        assert_eq!(apply(&outcomes[0], &snapshot), Step::Done(Notice::ok("Up to date.")));

        mock.knobs.system_check_failure = Some("no route to the snapshot server".to_owned());
        let outcomes = perform(&mut mock, &mut snapshot, check_now().remove(0));
        let Step::Failed(notice) = apply(&outcomes[0], &snapshot) else { panic!("a check with trouble") };
        assert!(notice.text.contains("The system check failed: no route to the snapshot server."), "{}", notice.text);
    }

    #[test]
    fn the_nodo_updates_on_its_own_and_the_apps_without_it() {
        let (_, mut snapshot) = device();
        assert_eq!(
            update_system(&snapshot),
            Step::Run(vec![Effect::ApplyUpdates { names: names(&[SYSTEM_BASE]), approvals: Vec::new() }])
        );
        // Ticked in another order than listed, with the Nodo's own update ticked too.
        let checked = names(&[SYSTEM_BASE, "moneropay"]);
        assert_eq!(
            update_apps(&snapshot, &checked),
            Step::Run(vec![Effect::ApplyUpdates { names: names(&["moneropay"]), approvals: Vec::new() }])
        );
        assert_eq!(update_apps(&snapshot, &names(&[SYSTEM_BASE])), Step::Done(Notice::info("Tick at least one app first.")));
        snapshot.updates.retain(|item| item.name != SYSTEM_BASE);
        assert_eq!(update_system(&snapshot), Step::Done(Notice::info("The Nodo is up to date.")));
    }

    #[test]
    fn an_update_that_asks_for_more_is_reviewed_and_its_approval_goes_with_the_batch() {
        let (mut mock, mut snapshot) = device();
        let checked = names(&["wallet-relay", "moneropay"]);
        let Step::Run(effects) = update_apps(&snapshot, &checked) else { panic!("a review comes first") };
        assert_eq!(
            effects,
            [Effect::ReviewUpdate {
                name: "wallet-relay".to_owned(),
                batch: names(&["moneropay", "wallet-relay"]),
                pending_reviews: Vec::new(),
                approvals: Vec::new(),
            }]
        );
        let loaded = perform(&mut mock, &mut snapshot, effects[0].clone());
        let Step::Go(Dest::Review(review)) = install::apply(&loaded[0]) else { panic!("the review's page") };
        assert!(matches!(review.purpose, ReviewPurpose::Update { .. }));
        let Step::Run(effects) = review.approve(None, &snapshot) else { panic!("the last review applies the batch") };
        let [Effect::ApplyUpdates { names: applied, approvals }] = effects.as_slice() else { panic!("{effects:?}") };
        assert_eq!(*applied, names(&["moneropay", "wallet-relay"]));
        assert_eq!(approvals.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["wallet-relay"]);

        let outcomes = perform(&mut mock, &mut snapshot, effects[0].clone());
        let Step::Go(Dest::UpdateResults(report)) = apply(&outcomes[0], &snapshot) else { panic!("the results' page") };
        assert!(report.results.iter().any(|result| result.name == "wallet-relay" && result.applied), "{report:?}");
        assert!(!snapshot.updates.iter().any(|item| item.name == "wallet-relay"));
    }

    #[test]
    fn a_batch_that_grows_encrypted_storage_asks_for_the_device_passphrase() {
        let mut mock = MockBackend::new();
        mock.updates[2].grows = Some("256 MB to 320 MB of encrypted storage".to_owned());
        let mut snapshot = load_snapshot(&mut mock).expect("the mock answers");

        // Even after its review: the approval is kept with the batch that waits.
        let Step::Run(effects) = update_apps(&snapshot, &names(&["wallet-relay"])) else { panic!("a review comes first") };
        let loaded = perform(&mut mock, &mut snapshot, effects[0].clone());
        let Step::Go(Dest::Review(review)) = install::apply(&loaded[0]) else { panic!("the review's page") };
        let Step::Go(Dest::UpdatePassphrase(batch)) = review.approve(None, &snapshot) else { panic!("the passphrase is asked") };
        assert_eq!(batch.names, names(&["wallet-relay"]));
        assert_eq!(batch.growing, ["wallet-relay: 256 MB to 320 MB of encrypted storage"]);
        assert_eq!(batch.approvals.len(), 1);

        let mut empty = Input::secret();
        assert_eq!(
            batch.submit(&mut empty),
            Step::Done(Notice::info("Enter the device passphrase to grow the encrypted storage."))
        );
        let mut passphrase = typed(MOCK_PASSPHRASE);
        let Step::Run(effects) = batch.submit(&mut passphrase) else { panic!("sent with the passphrase") };
        assert!(passphrase.is_empty(), "the passphrase was taken out of the box");
        let [Effect::ApplyUpdatesWithPassphrase { names: sent, approvals, passphrase }] = effects.as_slice() else {
            panic!("{effects:?}")
        };
        assert_eq!((sent, approvals, passphrase.expose()), (&batch.names, &batch.approvals, MOCK_PASSPHRASE));
        let outcomes = perform(&mut mock, &mut snapshot, effects[0].clone());
        assert!(matches!(apply(&outcomes[0], &snapshot), Step::Go(Dest::UpdateResults(_))));
        assert!(!snapshot.updates.iter().any(|item| item.name == "wallet-relay"));

        // A batch without such an update is sent as it is.
        assert!(matches!(apply_batch(&snapshot, names(&["moneropay"]), Vec::new()), Step::Run(_)));
        assert_eq!(batch.without_passphrase(), [Effect::ApplyUpdates { names: batch.names.clone(), approvals: batch.approvals.clone() }]);
    }

    #[test]
    fn a_failed_update_can_be_held_tried_again_or_reported() {
        let (mut mock, mut snapshot) = device();
        let Step::Run(effects) = update_apps(&snapshot, &names(&["moneropay"])) else { panic!("nothing to review") };
        let outcomes = perform(&mut mock, &mut snapshot, effects[0].clone());
        let Step::Go(Dest::UpdateResults(report)) = apply(&outcomes[0], &snapshot) else { panic!("the results' page") };
        assert_eq!(report.failure.as_ref().map(|failure| failure.name.as_str()), Some("moneropay"));

        let hold = Effect::SetHeld { name: "moneropay".to_owned(), held: true };
        assert_eq!(hold_failed(&report), Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![hold])]));
        assert_eq!(
            retry_failed(&report, &snapshot).effects(),
            [
                Effect::SetHeld { name: "moneropay".to_owned(), held: false },
                Effect::ApplyUpdates { names: names(&["moneropay"]), approvals: Vec::new() },
            ]
        );
        assert_eq!(report_failed(&report), [Effect::CopyReport("moneropay update".to_owned())]);
        assert_eq!(leave_results(), Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])]));

        // A retry of an update that grows storage asks for the passphrase like any batch.
        snapshot.updates[0].grows = Some("256 MB to 512 MB".to_owned());
        let Step::All(steps) = retry_failed(&report, &snapshot) else { panic!("released, then applied") };
        assert!(matches!(&steps[1], Step::Go(Dest::UpdatePassphrase(batch)) if batch.growing == ["moneropay: 256 MB to 512 MB"]));

        let fine = UpdateReport { results: report.results.clone(), failure: None };
        assert_eq!(hold_failed(&fine), leave_results());
        assert_eq!(retry_failed(&fine, &snapshot), Step::Stay);
        assert!(report_failed(&fine).is_empty());
    }

    #[test]
    fn window_parsing_accepts_only_two_different_valid_times() {
        assert_eq!(parse_window("03:00 to 05:00"), Some(("03:00".to_owned(), "05:00".to_owned())));
        assert_eq!(parse_window("23:00 to 01:00"), Some(("23:00".to_owned(), "01:00".to_owned())));
        assert_eq!(parse_window("03:00 to 03:00"), None);
        assert_eq!(parse_window("25:00 to 05:00"), None);
        assert_eq!(parse_window("3am to 5am"), None);
        assert_eq!(parse_window("3:00 to 5:00"), None);
        assert_eq!(parse_window("03:00-05:00"), None);
    }

    #[test]
    fn automatic_update_choices_are_checked_saved_and_asked_about_when_left_unsaved() {
        let (mut mock, mut snapshot) = device();
        let mut auto = AutoUpdate::new(snapshot.auto_update.clone());
        assert_eq!(auto.leave(), Step::Go(Dest::Back));

        let first = auto.draft.frequency.clone();
        for _ in 0..auto.draft.frequencies.len() {
            auto.next_frequency();
        }
        assert_eq!(auto.draft.frequency, first, "the frequencies go round");
        auto.set_frequency("whenever");
        assert_eq!(auto.draft.frequency, first, "only a frequency on offer is taken");
        let other = auto.draft.frequencies.iter().find(|option| **option != first).expect("more than one").clone();
        auto.set_frequency(&other);
        assert_eq!(auto.draft.frequency, other);

        let name = auto.draft.packages[0].name.clone();
        let was = auto.draft.packages[0].enabled;
        auto.toggle(&name);
        assert_eq!(auto.draft.packages[0].enabled, !was);
        auto.set_enabled(&name, was);
        auto.toggle("nobody");
        assert_eq!(auto.draft.packages, auto.saved.packages);

        assert!(!auto.set_window("03:00 to 03:00"));
        assert_eq!(auto.error.as_deref(), Some("Use two different device times, like 03:00 to 05:00."));
        assert_eq!(auto.window(), format!("{} to {}", auto.saved.window_start, auto.saved.window_end), "a refused window changes nothing");
        assert!(auto.set_window("01:30 to 02:45"));
        assert_eq!((auto.error.clone(), auto.window().as_str()), (None, "01:30 to 02:45"));

        assert!(auto.changed());
        assert!(matches!(auto.leave(), Step::Ask(question) if question.confirm == Confirm::DiscardAutoUpdate && question.cancel_is_default));
        assert_eq!(Confirm::DiscardAutoUpdate.step(&snapshot), Step::Go(Dest::Back));

        let outcomes = perform(&mut mock, &mut snapshot, auto.save().remove(0));
        let saved = Notice::ok("Auto-update choices saved.");
        assert_eq!(auto.apply(&outcomes[0]), Step::All(vec![Step::Go(Dest::Back), Step::Done(saved.clone())]));
        assert_eq!(apply(&outcomes[0], &snapshot), Step::Done(saved), "away from the page it is only said");
        assert_eq!((snapshot.auto_update.window_start.as_str(), snapshot.auto_update.frequency.as_str()), ("01:30", other.as_str()));

        let refused = Outcome::AutoUpdateSaved(Err("the drive is full".to_owned()));
        assert_eq!(auto.apply(&refused), Step::Failed(Notice::error("the drive is full")));
        assert_eq!(AutoUpdate::history(), [Effect::ShowDocument(DocKind::AutoUpdateHistory)]);
    }

    /// The Rollbacks page with its list read.
    fn rollbacks(mock: &mut MockBackend) -> Rollbacks {
        let mut page = Rollbacks::new();
        assert!(page.busy && page.reload().is_empty(), "nothing is asked twice while the list is read");
        for outcome in execute_effect(mock, Rollbacks::load().remove(0)) {
            assert_eq!(page.apply(&outcome), Step::Stay);
        }
        assert!(!page.busy);
        page
    }

    #[test]
    fn rolling_back_is_asked_first_and_says_what_it_does() {
        let (mut mock, snapshot) = device();
        let mut page = rollbacks(&mut mock);
        assert_eq!(page.options().len(), 2);

        assert_eq!(page.roll_back("wallet-relay"), Step::Stay);
        assert_eq!(page.error.as_deref(), Some("wallet-relay cannot roll back now: its data is in layout 2."));
        assert_eq!(page.roll_back("tor"), Step::Stay);
        assert_eq!(page.error.as_deref(), Some("Nothing can be rolled back now."));

        let Step::Ask(question) = page.roll_back("moneropay") else { panic!("asked first") };
        assert_eq!(question.title, "Roll moneropay back?");
        assert_eq!(
            question.body,
            [
                "moneropay goes from 2.7.0_1 back to 2.6.1_1.",
                "Its data is copied first and put back if the old version fails.",
                "2.7.0_1 is held, so automatic updates leave it alone.",
            ]
        );
        assert_eq!((question.confirm_label.as_str(), question.cancel_is_default), ("Roll back", true));
        assert_eq!(question.confirm, Confirm::RollBack { package: "moneropay".to_owned(), to: "2.6.1_1".to_owned() });
        assert!(!page.busy, "nothing runs before the owner confirms");

        page.confirmed(&question.confirm);
        assert!(page.busy && page.error.is_none());
        assert_eq!(page.roll_back("moneropay"), Step::Stay, "nothing else is taken while it runs");
        let effects = question.confirm.step(&snapshot).effects();
        assert_eq!(effects, [Effect::RollBack { package: "moneropay".to_owned(), to: "2.6.1_1".to_owned() }]);
        let outcome = execute_effect(&mut mock, effects[0].clone()).remove(0);
        let step = page.apply(&outcome);
        assert_eq!(
            step,
            Step::All(vec![
                Step::Done(Notice::ok("moneropay is back on 2.6.1_1.")),
                Step::Run(vec![Effect::LoadRollbacks, Effect::Refresh]),
            ])
        );
        assert!(page.busy && page.result.is_some(), "the list is read again before anything else");
        for outcome in execute_effect(&mut mock, Effect::LoadRollbacks) {
            assert_eq!(page.apply(&outcome), Step::Stay);
        }
        assert_eq!(page.options().iter().map(|option| option.package.as_str()).collect::<Vec<_>>(), ["wallet-relay"]);
        assert_eq!(page.leave(), Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])]));
    }

    #[test]
    fn a_rollback_the_helper_refuses_is_said_and_the_page_can_be_used_again() {
        let (mut mock, _) = device();
        let mut page = rollbacks(&mut mock);
        let Step::Ask(question) = page.roll_back("moneropay") else { panic!("asked first") };
        page.confirmed(&question.confirm);
        mock.rollbacks.clear();
        let outcome = execute_effect(&mut mock, Effect::RollBack { package: "moneropay".to_owned(), to: "2.6.1_1".to_owned() }).remove(0);
        let Step::Failed(notice) = page.apply(&outcome) else { panic!("refused") };
        assert!(notice.text.contains("moneropay has no earlier version kept"), "{}", notice.text);
        assert!(!page.busy);
        assert_eq!(page.reload(), [Effect::LoadRollbacks]);
        assert!(page.busy);

        let unread = Outcome::RollbacksLoaded(Err("the helper did not answer".to_owned()));
        assert_eq!(page.apply(&unread), Step::Stay);
        assert_eq!((page.busy, page.error.as_deref()), (false, Some("the helper did not answer")));
        assert_eq!(page.apply(&Outcome::Info("hello".to_owned())), Step::Stay);
    }
}
