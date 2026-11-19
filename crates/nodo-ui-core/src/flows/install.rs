//! Get apps and install one: search, an app's details, the gate before an install, the review
//! and its approval, the install itself, and the two ways the gate says no.
//!
//! The order is fixed: nothing installs without passing the gate (`check`), and a gate that
//! answers with a review installs only what that review showed, with its approval quoted back.
//!
//! [`Search`] and [`Review`] have state. The other pages show data the helper gave
//! (`PackageDetail`, `InstallSnapshot`, `RejectionData`, `CapabilityData`), so their actions are
//! functions that take it.

use super::{Dest, Notice, Step};
use crate::model::{
    Capabilities, CapabilityData, DocKind, InstallGate, InstallOutcome, InstallSnapshot, PackageDetail, PackageId, PackageSummary,
    RejectionData, ReviewData, Snapshot,
};
use crate::secret::Input;
use crate::{Effect, Outcome};

// Get apps

/// What narrows the list of apps on offer, besides the words typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    /// Only this source's apps are listed.
    pub feed_filter: Option<String>,
}

impl Search {
    /// The apps on offer that match the words typed, in the source chosen.
    pub fn results<'a>(&self, snapshot: &'a Snapshot, query: &str) -> Vec<&'a PackageSummary> {
        snapshot.search_results(query, self.feed_filter.as_deref())
    }

    /// Narrows to the next source: every source, then each one in turn, then every source again.
    pub fn next_source(&mut self, snapshot: &Snapshot) {
        let position = self.feed_filter.as_ref().and_then(|current| snapshot.feeds.iter().position(|feed| feed.name == *current));
        self.feed_filter = match position {
            None => snapshot.feeds.first().map(|feed| feed.name.clone()),
            Some(index) => snapshot.feeds.get(index + 1).map(|feed| feed.name.clone()),
        };
    }

    /// Lists every source's apps again.
    pub fn clear(&mut self) {
        self.feed_filter = None;
    }
}

/// An app's details page.
pub fn detail(id: &PackageId) -> Vec<Effect> {
    vec![Effect::LoadDetail(id.clone())]
}

/// The gate every install goes through: the helper answers with a review to approve, a
/// rejection, a missing capability or a bundle. See [`apply`].
pub fn check(id: &PackageId) -> Vec<Effect> {
    vec![Effect::CheckInstall(id.clone())]
}

// An app's details

/// Install, from the details page. An app that is installed is not installed again.
pub fn install(detail: &PackageDetail) -> Vec<Effect> {
    if detail.installed { Vec::new() } else { check(&detail.id) }
}

/// The installed app's own page. Nothing for an app that is not installed.
pub fn open_installed(detail: &PackageDetail) -> Step {
    if detail.installed { Step::Go(Dest::AppPage(detail.id.name.clone())) } else { Step::Stay }
}

/// The installed app's settings. Nothing for an app that is not installed.
pub fn settings(detail: &PackageDetail) -> Vec<Effect> {
    if detail.installed { vec![Effect::OpenSettings(detail.id.name.clone())] } else { Vec::new() }
}

/// The versions the sources offer, as a document.
pub fn versions(detail: &PackageDetail) -> Vec<Effect> {
    vec![Effect::ShowDocument(DocKind::Versions(detail.id.name.clone()))]
}

/// What changed between versions, as a document.
pub fn changelog(detail: &PackageDetail) -> Vec<Effect> {
    vec![Effect::ShowDocument(DocKind::Changelog(detail.id.name.clone()))]
}

// The review

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewPurpose {
    Install,
    /// One of a batch of updates that ask for something new. `pending_reviews` are the updates
    /// still to review after this one, and `approvals` gathers each confirmed review, which the
    /// helper requires for those updates.
    Update { batch: Vec<String>, pending_reviews: Vec<String>, approvals: Vec<(String, nodo_api::PermissionApproval)> },
}

/// What an app asks for, shown before it is installed or updated, and what approving leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub data: ReviewData,
    pub purpose: ReviewPurpose,
}

impl Review {
    pub fn new(data: ReviewData, purpose: ReviewPurpose) -> Self {
        Self { data, purpose }
    }

    /// The files the package installs, as a document.
    pub fn files(&self) -> Vec<Effect> {
        vec![Effect::ShowDocument(DocKind::FileList(self.data.id.name.clone()))]
    }

    /// Approving asks for the device passphrase first: the install creates encrypted storage.
    /// A frontend asks for it before calling [`Review::approve`].
    pub fn needs_passphrase(&self) -> bool {
        self.purpose == ReviewPurpose::Install && self.data.needs_passphrase
    }

    /// The owner approves what the review shows.
    ///
    /// An install starts at once and goes to its page, because with the helper it is one request
    /// that answers when it is over. When it needs the device passphrase and none was typed, it
    /// is refused with a remark and `passphrase` is left alone; otherwise the passphrase is taken
    /// out of the box.
    ///
    /// An update's approval is added to the batch's. The next update that needs a review is
    /// loaded, or with none left the batch goes on to be applied (`Dest::ApplyUpdates`).
    pub fn approve(&self, passphrase: Option<&mut Input>) -> Step {
        match &self.purpose {
            ReviewPurpose::Install => {
                let passphrase = match passphrase {
                    Some(input) if !input.is_empty() => Some(input.take_secret()),
                    _ if self.needs_passphrase() => {
                        return Step::Failed(Notice::info("Enter the device passphrase to create its encrypted storage."));
                    }
                    _ => None,
                };
                let (id, approval) = (self.data.id.clone(), self.data.approval.clone());
                Step::All(vec![
                    Step::Go(Dest::Installing(InstallSnapshot::starting(&id, &self.data.version))),
                    Step::Run(vec![Effect::StartInstall { id, approval, passphrase }]),
                ])
            }
            ReviewPurpose::Update { batch, pending_reviews, approvals } => {
                let mut approvals = approvals.clone();
                if let Some(approval) = self.data.approval.clone() {
                    approvals.push((self.data.id.name.clone(), approval));
                }
                match pending_reviews.split_first() {
                    None => Step::Go(Dest::ApplyUpdates { names: batch.clone(), approvals }),
                    Some((name, rest)) => Step::All(vec![
                        Step::Go(Dest::Back),
                        Step::Run(vec![Effect::ReviewUpdate {
                            name: name.clone(),
                            batch: batch.clone(),
                            pending_reviews: rest.to_vec(),
                            approvals,
                        }]),
                    ]),
                }
            }
        }
    }
}

// The install

/// Asks how the install is going, where the backend reports progress. The helper's install
/// answers once it is over, and polling it would only queue behind that answer.
pub fn poll(install: &InstallSnapshot, capabilities: &Capabilities) -> Vec<Effect> {
    if install.outcome == InstallOutcome::InProgress && capabilities.install_progress {
        vec![Effect::PollInstall]
    } else {
        Vec::new()
    }
}

/// The install's log, as a document.
pub fn log(install: &InstallSnapshot) -> Vec<Effect> {
    vec![Effect::ShowDocument(DocKind::InstallLog(install.id.name.clone()))]
}

/// Leaving the install's page. A running install that can be stopped is cancelled, and its page
/// stays until the cancel is reported. One that cannot be stopped keeps running, and how it ends
/// is said wherever the owner is by then. A finished one is left and the device read again.
pub fn leave(install: &InstallSnapshot) -> Step {
    match install.outcome {
        InstallOutcome::InProgress if install.can_cancel => Step::Run(vec![Effect::CancelInstall]),
        InstallOutcome::InProgress => Step::Go(Dest::Back),
        _ => Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])]),
    }
}

/// Going on from the install's page: to the new app's settings once it is installed, and away
/// from a failed install. Nothing while it runs.
pub fn carry_on(install: &InstallSnapshot) -> Step {
    match install.outcome {
        InstallOutcome::Succeeded => Step::Run(vec![Effect::OpenSettings(install.id.name.clone())]),
        InstallOutcome::InProgress => Step::Stay,
        _ => leave(install),
    }
}

/// What a report about the install means (`Outcome::InstallProgress`). `on_install_page` says
/// whether the owner is looking at the install's page.
///
/// There the page shows the new report (`Dest::Installing`), and a cancelled install leaves it.
/// Anywhere else only the end is said, as a notice. A cancel is offered only where the backend
/// says it can cancel at all, whatever one report claims.
pub fn progress(report: &InstallSnapshot, capabilities: &Capabilities, on_install_page: bool) -> Step {
    let mut report = report.clone();
    report.can_cancel &= capabilities.cancel_install;
    let name = report.id.name.clone();
    match &report.outcome {
        InstallOutcome::Cancelled => {
            let said = Step::Done(Notice::info("Install cancelled. What it added was removed."));
            if on_install_page { Step::All(vec![Step::Go(Dest::Back), said]) } else { said }
        }
        _ if on_install_page => Step::Go(Dest::Installing(report)),
        InstallOutcome::Succeeded => Step::Done(Notice::ok(format!("{name} is installed. Open it from Apps to set it up."))),
        InstallOutcome::Failed(reason) => Step::Failed(Notice::error(format!("{name} wasn't installed: {reason}"))),
        InstallOutcome::InProgress => Step::Stay,
    }
}

// The gate said no

/// A report of why a package was rejected, for whoever published it.
pub fn report_rejection(rejection: &RejectionData) -> Vec<Effect> {
    vec![Effect::CopyReport(format!("{} rejection", rejection.id.name))]
}

/// The source a rejected package came from, among the sources. There is deliberately nothing
/// on a rejection that installs anything.
pub fn rejected_source(rejection: &RejectionData) -> Step {
    Step::Go(Dest::Sources { select: Some(rejection.id.feed.clone()) })
}

/// Whether an older version would work with the node installed now.
pub fn compatible_version(missing: &CapabilityData) -> Step {
    Step::Done(Notice::info(format!("No compatible version: {}.", missing.compatible_note)))
}

/// Installs the node that has what the package needs. It goes through the gate like any other.
pub fn install_alternative(missing: &CapabilityData) -> Vec<Effect> {
    check(&PackageId::new("official", &missing.alternative_node))
}

/// What a result means: details that loaded, the gate's answer, and the review of an update.
/// `Outcome::InstallProgress` is [`progress`]'s, since it depends on where the owner is.
pub fn apply(outcome: &Outcome) -> Step {
    match outcome {
        Outcome::DetailLoaded(Some(detail)) => Step::Go(Dest::Detail(detail.clone())),
        Outcome::DetailLoaded(None) => Step::Failed(Notice::error("No details are available for that package.")),
        Outcome::GateLoaded(gate) => Step::Go(match gate {
            InstallGate::Review(data) => Dest::Review(Review::new(data.clone(), ReviewPurpose::Install)),
            InstallGate::Bundle(review) => Dest::Bundle(Box::new(review.clone())),
            InstallGate::Rejected(data) => Dest::Rejected(data.clone()),
            InstallGate::MissingCapability(data) => Dest::MissingCapability(data.clone()),
        }),
        Outcome::UpdateReviewLoaded { review: Some(data), batch, pending_reviews, approvals } => {
            let purpose =
                ReviewPurpose::Update { batch: batch.clone(), pending_reviews: pending_reviews.clone(), approvals: approvals.clone() };
            Step::Go(Dest::Review(Review::new(data.clone(), purpose)))
        }
        Outcome::UpdateReviewLoaded { review: None, .. } => {
            Step::Failed(Notice::error("That update could not be reviewed, so nothing was applied."))
        }
        _ => Step::Stay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::MockBackend;

    fn one(mock: &mut MockBackend, effects: Vec<Effect>) -> Outcome {
        let [effect] = <[Effect; 1]>::try_from(effects).expect("one effect");
        execute_effect(mock, effect).remove(0)
    }

    fn review_of(mock: &mut MockBackend, feed: &str, name: &str) -> Review {
        match apply(&one(mock, check(&PackageId::new(feed, name)))) {
            Step::Go(Dest::Review(review)) => review,
            other => panic!("{name} should reach a review, not {other:?}"),
        }
    }

    fn typed(text: &str) -> Input {
        let mut input = Input::secret();
        text.chars().for_each(|c| input.push(c));
        input
    }

    #[test]
    fn the_source_filter_goes_through_every_source_and_back_to_all() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let mut search = Search::default();
        let all = search.results(&snapshot, "").len();
        let mut seen = Vec::new();
        loop {
            search.next_source(&snapshot);
            let Some(source) = search.feed_filter.clone() else { break };
            let listed = search.results(&snapshot, "");
            assert!(listed.iter().all(|pkg| pkg.id.feed == source), "only {source}'s apps");
            assert!(listed.len() <= all);
            seen.push(source);
        }
        let names: Vec<String> = snapshot.feeds.iter().map(|feed| feed.name.clone()).collect();
        assert_eq!(seen, names);

        search.next_source(&snapshot);
        assert!(search.feed_filter.is_some());
        search.clear();
        assert_eq!(search.results(&snapshot, "").len(), all);
        assert!(search.results(&snapshot, "pay").iter().any(|pkg| pkg.id.name == "moneropay"));
    }

    #[test]
    fn details_load_and_an_installed_app_is_not_installed_again() {
        let mut mock = MockBackend::new();
        let id = PackageId::new("official", "moneropay");
        let Step::Go(Dest::Detail(mut shown)) = apply(&one(&mut mock, detail(&id))) else { panic!("details load") };
        assert_eq!(shown.id, id);
        assert_eq!(versions(&shown), [Effect::ShowDocument(DocKind::Versions("moneropay".into()))]);
        assert_eq!(changelog(&shown), [Effect::ShowDocument(DocKind::Changelog("moneropay".into()))]);

        shown.installed = false;
        assert_eq!(install(&shown), [Effect::CheckInstall(id.clone())]);
        assert_eq!(open_installed(&shown), Step::Stay);
        assert!(settings(&shown).is_empty());

        shown.installed = true;
        assert!(install(&shown).is_empty());
        assert_eq!(open_installed(&shown), Step::Go(Dest::AppPage("moneropay".into())));
        assert_eq!(settings(&shown), [Effect::OpenSettings("moneropay".into())]);

        let missing = Outcome::DetailLoaded(None);
        assert!(matches!(apply(&missing), Step::Failed(_)));
    }

    #[test]
    fn the_gate_sorts_packages_into_review_rejection_and_missing_capability() {
        let mut mock = MockBackend::new();
        let review = review_of(&mut mock, "community", "wallet-relay");
        assert_eq!(review.purpose, ReviewPurpose::Install);
        assert_eq!(review.files(), [Effect::ShowDocument(DocKind::FileList("wallet-relay".into()))]);

        let rejected = apply(&one(&mut mock, check(&PackageId::new("community", "fast-sync-helper"))));
        let Step::Go(Dest::Rejected(rejection)) = rejected else { panic!("{rejected:?}") };
        assert_eq!(report_rejection(&rejection), [Effect::CopyReport("fast-sync-helper rejection".into())]);
        assert_eq!(rejected_source(&rejection), Step::Go(Dest::Sources { select: Some("community".into()) }));

        let missing = apply(&one(&mut mock, check(&PackageId::new("community", "zmq-indexer"))));
        let Step::Go(Dest::MissingCapability(missing)) = missing else { panic!("{missing:?}") };
        assert!(matches!(compatible_version(&missing), Step::Done(notice) if notice.text.starts_with("No compatible version: ")));
        // The other node is not installed from here either: it goes through the gate.
        assert_eq!(
            install_alternative(&missing),
            [Effect::CheckInstall(PackageId::new("official", &missing.alternative_node))]
        );
    }

    #[test]
    fn approving_a_review_installs_exactly_what_it_showed() {
        let mut mock = MockBackend::new();
        let review = review_of(&mut mock, "community", "wallet-relay");
        assert!(!review.needs_passphrase());
        let step = review.approve(None);
        let Step::All(steps) = &step else { panic!("{step:?}") };
        let Step::Go(Dest::Installing(starting)) = &steps[0] else { panic!("the install page opens at once") };
        assert_eq!((starting.outcome.clone(), starting.can_cancel), (InstallOutcome::InProgress, false));
        let Step::Run(effects) = &steps[1] else { panic!("{step:?}") };
        let [Effect::StartInstall { id, approval, passphrase: None }] = &effects[..] else { panic!("{effects:?}") };
        assert_eq!((id, approval), (&review.data.id, &review.data.approval));
        assert!(approval.is_some(), "the helper installs only with the review's approval");

        // The helper's install answers once, when it is over.
        let caps = load_snapshot(&mut mock).expect("the mock answers").capabilities;
        let mut report = match one(&mut mock, effects.clone()) {
            Outcome::InstallProgress(Some(report)) => report,
            other => panic!("{other:?}"),
        };
        while report.outcome == InstallOutcome::InProgress {
            let polls = poll(&report, &caps);
            assert_eq!(polls, [Effect::PollInstall], "a running install is only seen where progress is reported");
            let Outcome::InstallProgress(Some(next)) = one(&mut mock, polls) else { panic!("it keeps reporting") };
            report = next;
        }
        assert_eq!(report.outcome, InstallOutcome::Succeeded);
        assert!(poll(&report, &caps).is_empty());
        assert_eq!(progress(&report, &caps, true), Step::Go(Dest::Installing(report.clone())));
        assert_eq!(carry_on(&report), Step::Run(vec![Effect::OpenSettings("wallet-relay".into())]));
        assert!(load_snapshot(&mut mock).expect("the mock answers").services.iter().any(|svc| svc.id == "wallet-relay"));
    }

    #[test]
    fn new_encrypted_storage_needs_the_device_passphrase_before_anything_starts() {
        let mut mock = MockBackend::new();
        let mut review = review_of(&mut mock, "community", "wallet-relay");
        review.data.needs_passphrase = true;
        assert!(review.needs_passphrase());

        let refused = Step::Failed(Notice::info("Enter the device passphrase to create its encrypted storage."));
        assert_eq!(review.approve(None), refused);
        assert_eq!(review.approve(Some(&mut Input::secret())), refused);

        let mut passphrase = typed("correct horse");
        let effects = review.approve(Some(&mut passphrase)).effects();
        let [Effect::StartInstall { passphrase: Some(sent), .. }] = &effects[..] else { panic!("{effects:?}") };
        assert_eq!(sent.expose(), "correct horse");
        assert!(passphrase.is_empty(), "the passphrase leaves the box");
    }

    #[test]
    fn an_updates_approval_joins_the_batch_and_the_next_review_follows() {
        let mut mock = MockBackend::new();
        let mut data = review_of(&mut mock, "community", "wallet-relay").data;
        let approval = nodo_api::PermissionApproval { pkgver: "wallet-relay-0.4.2_1".into(), permissions: vec!["x".into()] };
        data.approval = Some(approval.clone());
        data.needs_passphrase = true;
        let batch = vec!["wallet-relay".to_owned(), "moneropay".to_owned()];
        let earlier = ("cuprated".to_owned(), approval.clone());

        let purpose =
            ReviewPurpose::Update { batch: batch.clone(), pending_reviews: vec!["moneropay".into()], approvals: vec![earlier.clone()] };
        let review = Review::new(data.clone(), purpose);
        assert!(!review.needs_passphrase(), "the Updates page asks for it, for the whole batch");
        let approvals = vec![earlier.clone(), ("wallet-relay".to_owned(), approval.clone())];
        assert_eq!(
            review.approve(None),
            Step::All(vec![
                Step::Go(Dest::Back),
                Step::Run(vec![Effect::ReviewUpdate {
                    name: "moneropay".into(),
                    batch: batch.clone(),
                    pending_reviews: Vec::new(),
                    approvals: approvals.clone(),
                }]),
            ])
        );

        let last = Review::new(data, ReviewPurpose::Update { batch: batch.clone(), pending_reviews: Vec::new(), approvals: vec![earlier] });
        assert_eq!(last.approve(None), Step::Go(Dest::ApplyUpdates { names: batch.clone(), approvals }));

        // The review of an update arrives with the batch it belongs to, or not at all.
        let loaded = Outcome::UpdateReviewLoaded {
            review: Some(last.data.clone()),
            batch: batch.clone(),
            pending_reviews: Vec::new(),
            approvals: Vec::new(),
        };
        assert!(matches!(apply(&loaded), Step::Go(Dest::Review(review)) if matches!(review.purpose, ReviewPurpose::Update { .. })));
        let failed = Outcome::UpdateReviewLoaded { review: None, batch, pending_reviews: Vec::new(), approvals: Vec::new() };
        assert_eq!(apply(&failed), Step::Failed(Notice::error("That update could not be reviewed, so nothing was applied.")));
    }

    #[test]
    fn how_an_install_ended_is_said_wherever_the_owner_is() {
        let caps = Capabilities::default();
        let id = PackageId::new("official", "moneropay");
        let mut report = InstallSnapshot::starting(&id, "2.7.0");
        assert_eq!(progress(&report, &caps, false), Step::Stay);

        report.outcome = InstallOutcome::Succeeded;
        assert!(matches!(progress(&report, &caps, false), Step::Done(notice) if notice.text.starts_with("moneropay is installed.")));
        report.outcome = InstallOutcome::Failed("no space left".into());
        assert_eq!(progress(&report, &caps, false), Step::Failed(Notice::error("moneropay wasn't installed: no space left")));
        assert_eq!(carry_on(&report), Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])]));

        report.outcome = InstallOutcome::Cancelled;
        let said = Step::Done(Notice::info("Install cancelled. What it added was removed."));
        assert_eq!(progress(&report, &caps, false), said);
        assert_eq!(progress(&report, &caps, true), Step::All(vec![Step::Go(Dest::Back), said]));
    }

    #[test]
    fn a_cancel_is_offered_only_where_the_backend_can_cancel() {
        let id = PackageId::new("official", "moneropay");
        let mut claims = InstallSnapshot::starting(&id, "2.7.0");
        claims.can_cancel = true;

        let cannot = Capabilities::default();
        let Step::Go(Dest::Installing(shown)) = progress(&claims, &cannot, true) else { panic!("the page shows it") };
        assert!(!shown.can_cancel);
        // It cannot be stopped, so leaving only leaves the page.
        assert_eq!(leave(&shown), Step::Go(Dest::Back));
        assert_eq!(carry_on(&shown), Step::Stay);

        let can = Capabilities { cancel_install: true, ..Capabilities::default() };
        let Step::Go(Dest::Installing(shown)) = progress(&claims, &can, true) else { panic!("the page shows it") };
        assert!(shown.can_cancel);
        assert_eq!(leave(&shown), Step::Run(vec![Effect::CancelInstall]));
        assert_eq!(log(&shown), [Effect::ShowDocument(DocKind::InstallLog("moneropay".into()))]);
    }
}
