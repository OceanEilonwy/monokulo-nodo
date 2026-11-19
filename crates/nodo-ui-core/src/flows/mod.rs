//! The rules and the sequences of helper calls of each feature, once, for every interface.
//!
//! A frontend (the terminal interface, the built-in screen, a web page) keeps only what is its
//! own: which key, tap or request means which action, where the focus is, scrolling, its way of
//! going from page to page, and drawing. Everything that would have to be the same in a second
//! frontend is here: what an action does, what must be asked first, what a result means,
//! validation, the order of steps, and the wording of consequences and errors.
//!
//! # The conventions every flow follows
//!
//! Each feature is a module, `flows::<feature>`.
//!
//! **State.** A feature that has state of its own is a type (`unlock::Unlock`, `logs::Logs`).
//! It holds no cursor, no selected row and no scroll position. A frontend's screen state is that
//! type plus its own cursor fields. A feature whose only state is data the helper gave (a
//! `FeedProbe`, a `KeyChange`, an `InstallSnapshot`) or the device's `Snapshot` is a set of
//! functions that take that data.
//!
//! **Actions** are named for what the owner means: `restart`, `toggle`, `trust`, `submit`.
//! They take names and typed values (an app's name, a source's name, a fingerprint, the text
//! typed), never a key code and never an index into a list a frontend drew. They return one of:
//!
//! - `Vec<Effect>`: the helper calls to make, when the action can only do that. An empty list
//!   means there was nothing to do.
//! - [`Step`]: when the action can also refuse, ask first or lead to another page.
//! - `Result<Vec<Effect>, String>`: when the action checks what was typed. The error is worded
//!   for the owner and belongs beside the field it is about.
//!
//! An action that takes typed text takes it as `&str`, or as a `&mut Input` when it is a secret
//! the action must take out of the box (`Input::take_secret`) only once it goes ahead.
//!
//! **Results.** A frontend performs effects with `exec::execute_effect` (or an `EffectWorker`)
//! and hands every [`Outcome`] to `apply`: the flow type's `apply(&mut self, &Outcome) -> Step`,
//! or the module's `apply(&Outcome) -> Step` where the feature has no type. `apply` says what the
//! result means. **An outcome that is not the flow's own gives `Step::Stay` and changes nothing**,
//! so a frontend may hand an outcome to the flow of the page it shows without knowing whose it
//! is. Outcomes of features not extracted yet are still handled by the frontend.
//!
//! **Following a step.** A frontend has one function that follows a `Step`: it shows the notice,
//! shows the question, goes to the destination, and performs the effects of `Step::Run` and
//! hands their outcomes to `apply` again, until a step carries no effects. `Step::All` is
//! followed in order. The terminal interface returns the effects to its event loop, which is that
//! loop; a web request runs them before it answers.
//!
//! **Questions.** An action that must be confirmed returns `Step::Ask(Question)` (or a
//! `Question`) and does nothing else. The frontend shows it its own way, starting on Cancel when
//! `cancel_is_default` says so. When the owner confirms, the frontend first tells the flow that
//! asked, where that flow has a `confirmed(&mut self, &Confirm)` method (it marks the flow busy),
//! and then follows `question.confirm.step(&snapshot)`. Nothing happens when the owner cancels.
//!
//! **Destinations.** `Step::Go(Dest)` names a page by meaning and carries what that page starts
//! with. Each frontend maps a `Dest` to its own screen or URL. `Dest::Back` is the one that is
//! relative: the flow's page has nothing more to show, so the owner returns to the page that
//! opened it.
//!
//! **Queries** for drawing are plain methods and functions that read: rows, whether a button is
//! offered, the words of a consequence. They never change state.
//!
//! **Time** is in whole seconds, never in ticks or instants. A flow says "wait this long"; the
//! frontend keeps the clock.
//!
//! **Tests.** Each flow has unit tests here against `MockBackend`: actions are run with
//! `execute_effect`, their outcomes are fed to `apply`, and the steps and effects are asserted.

pub mod documents;
pub mod drive;
pub mod install;
pub mod logs;
pub mod services;
pub mod settings;
pub mod setup;
pub mod sources;
pub mod unlock;
pub mod updates;

use nodo_api::Exposure;

use crate::effect::ExposureAfter;
use crate::model::{
    CapabilityData, Document, DriveAction, FeedProbe, InstallSnapshot, KeyChange, PackageDetail, RejectionData, Snapshot,
};
use crate::{Effect, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Ok,
    Error,
}

/// One line said to the owner about what just happened. A frontend shows the latest one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
}

impl Notice {
    pub fn info(text: impl Into<String>) -> Self {
        Self { kind: NoticeKind::Info, text: text.into() }
    }

    pub fn ok(text: impl Into<String>) -> Self {
        Self { kind: NoticeKind::Ok, text: text.into() }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { kind: NoticeKind::Error, text: text.into() }
    }
}

/// What a frontend does next, after an action or a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing more than drawing again. The flow's state may have changed (an error beside a
    /// field, new rows), or the action or outcome was not this flow's.
    Stay,
    /// It worked, or there is something to tell: show the notice and stay on the page.
    Done(Notice),
    /// It was refused or it failed: show the notice and stay on the page. On a page whose flow
    /// keeps an `error` of its own the flow sets that instead and returns `Stay`.
    Failed(Notice),
    /// The owner must confirm first. Nothing has been done yet. See "Questions" in the module
    /// documentation.
    Ask(Question),
    /// Go to another page.
    Go(Dest),
    /// Perform these effects, in order, and hand their outcomes to `apply`.
    Run(Vec<Effect>),
    /// Several of the above, followed in order: a notice and a page to go to, a page to go to
    /// and effects to run there.
    All(Vec<Step>),
}

impl Step {
    /// The effects alone, or `Stay` when there are none.
    pub fn run(effects: Vec<Effect>) -> Self {
        if effects.is_empty() { Step::Stay } else { Step::Run(effects) }
    }

    /// Every effect the step carries, in order, for a frontend or a test that only needs those.
    pub fn effects(&self) -> Vec<Effect> {
        match self {
            Step::Run(effects) => effects.clone(),
            Step::All(steps) => steps.iter().flat_map(Step::effects).collect(),
            Step::Stay | Step::Done(_) | Step::Failed(_) | Step::Ask(_) | Step::Go(_) => Vec::new(),
        }
    }
}

/// A page, named by what it is for. Each frontend maps one to its own screen or URL. The list
/// grows with each feature that moves here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest {
    /// The page that opened this one: the flow here is over. A frontend without a stack of pages
    /// knows each page's parent.
    Back,
    /// The list of installed apps.
    Apps,
    /// The Updates page. `only` names the one app whose update is chosen there, when the owner
    /// came from that app's page.
    Updates { only: Option<String> },
    /// The page that asks for the device passphrase. `resume` says, in words for the owner, what
    /// sent them there; once unlocked that carries on (`Dest::Resume`).
    Unlock { resume: Option<String> },
    /// Back to the page that sent the owner to unlock, which now carries on with what it was
    /// asked: the terminal presses Enter there again, a web page sends its request again.
    Resume,
    /// Get apps: what the sources offer, only one source's when `source` names it.
    GetApps { source: Option<String> },
    /// The second step of adding a source: what was found at the address, to trust or not.
    ConfirmSource(FeedProbe),
    /// The decision about a source whose signing key changed.
    KeyChange(KeyChange),
    /// The sources, with the one named chosen.
    Sources { select: Option<String> },
    /// An installed app's own page, by name.
    AppPage(String),
    /// An app's details, among the apps on offer.
    Detail(PackageDetail),
    /// What an app asks for, to approve or not.
    Review(install::Review),
    /// A suite of apps installed together, to review.
    Bundle(Box<nodo_api::BundleReview>),
    /// Why a package cannot be installed at all.
    Rejected(RejectionData),
    /// What a package needs that the node installed now lacks.
    MissingCapability(CapabilityData),
    /// The page of the install under way, showing this report. A frontend already on that page
    /// shows the report in place.
    Installing(InstallSnapshot),
    /// The Updates page, asking for the device passphrase before this batch is applied, because
    /// one of its updates grows encrypted storage. A review or a report of results that led here
    /// is left first.
    UpdatePassphrase(updates::Batch),
    /// What a batch of updates came to. It takes the place of the review or the earlier report
    /// that led to the batch.
    UpdateResults(crate::model::UpdateReport),
    /// An app's settings, with the form the helper gave: the page of a new `settings::Settings`.
    Settings(crate::model::SettingsForm),
    /// The page of a save the helper took back, on top of the form, which keeps its draft.
    ApplyFailed(settings::ApplyFailed),
    /// A text to read, shown on top of the page the owner is on.
    Document(Document),
}

/// A yes-or-no question the owner must answer before something that cannot be taken back, or
/// that costs something, is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub title: String,
    /// What confirming leads to, a line each.
    pub body: Vec<String>,
    /// The words on the confirming button. The other button is always Cancel.
    pub confirm_label: String,
    /// The focus, or the default answer, starts on Cancel, so a stray Enter does nothing.
    pub cancel_is_default: bool,
    /// What confirming does.
    pub confirm: Confirm,
}

impl Question {
    /// A question that starts on Cancel, as every one about a loss does.
    pub fn careful(title: impl Into<String>, body: Vec<String>, confirm_label: impl Into<String>, confirm: Confirm) -> Self {
        Self { title: title.into(), body, confirm_label: confirm_label.into(), cancel_is_default: true, confirm }
    }
}

/// What the owner confirmed. `step` carries it out.
///
/// The first group belongs to features whose flows are here. The second group is still asked by
/// the terminal interface, with its own wording, until the slice of its feature moves the
/// question here; what confirming does is already in `step`, so there is one copy of that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// Removes an installed app, by name.
    RemoveApp(String),
    /// Removes a source. `from_key_change` when it was asked on the page of that source's changed
    /// key, which then has nothing more to show.
    RemoveSource { name: String, from_key_change: bool },
    /// Repairs the data drive's filesystem.
    RepairDrive,
    /// Removes an SSH key, by fingerprint.
    RemoveSshKey(String),
    /// Leaves an app's settings without saving.
    DiscardSettings,
    /// Leaves the auto-update choices without saving.
    DiscardAutoUpdate,
    /// Puts a package back to the version given.
    RollBack { package: String, to: String },

    /// Takes every site offline.
    AllSitesOffline,
    /// Joins an open network, whose traffic anyone nearby can read.
    JoinOpenWifi(String),
    ForgetWifi(String),
    /// Turns Wi-Fi off while it may be the connection in use.
    WifiOff,
    RemoveBluetooth(String),
    /// Disconnects a domain provider, by id.
    RemoveProvider(String),
}

impl Confirm {
    /// What confirming does: the effects to run and where to go. `snapshot` is the device's
    /// state as the frontend last read it.
    pub fn step(&self, snapshot: &Snapshot) -> Step {
        match self {
            Confirm::RemoveApp(name) => Step::All(vec![Step::Go(Dest::Apps), Step::Run(vec![Effect::RemovePackage(name.clone())])]),
            Confirm::RemoveSource { name, from_key_change } => {
                let remove = Step::Run(vec![Effect::RemoveFeed(name.clone())]);
                if *from_key_change { Step::All(vec![Step::Go(Dest::Back), remove]) } else { remove }
            }
            Confirm::RepairDrive => Step::Run(vec![Effect::Drive(DriveAction::Repair)]),
            Confirm::RemoveSshKey(fingerprint) => Step::Run(vec![Effect::RemoveSshKey(fingerprint.clone())]),
            Confirm::AllSitesOffline => Step::run(
                snapshot
                    .web
                    .sites
                    .iter()
                    .filter(|site| site.exposure != Exposure::default())
                    .map(|site| Effect::SetExposure {
                        site: site.package.clone(),
                        exposure: Exposure::default(),
                        after: ExposureAfter::Stay,
                        confirmation: None,
                    })
                    .collect(),
            ),
            Confirm::DiscardSettings | Confirm::DiscardAutoUpdate => Step::Go(Dest::Back),
            Confirm::RollBack { package, to } => Step::Run(vec![Effect::RollBack { package: package.clone(), to: to.clone() }]),
            Confirm::JoinOpenWifi(ssid) => Step::Run(vec![Effect::JoinWifi {
                ssid: ssid.clone(),
                security: nodo_api::WifiSecurity::Open,
                passphrase: None,
                accept_open: true,
            }]),
            Confirm::ForgetWifi(ssid) => Step::Run(vec![Effect::ForgetWifi(ssid.clone())]),
            Confirm::WifiOff => Step::Run(vec![Effect::SetWifi(false)]),
            Confirm::RemoveBluetooth(address) => Step::Run(vec![Effect::RemoveBluetooth(address.clone())]),
            Confirm::RemoveProvider(id) => Step::Run(vec![Effect::RemoveProvider(id.clone())]),
        }
    }
}

/// The outcomes that belong to no feature: a failure or a remark about whatever was last asked
/// for. `Outcome::Refreshed` is not here: the frontend keeps the `Snapshot`, so it replaces its
/// own. Every other outcome gives `Step::Stay`.
pub fn apply(outcome: &Outcome) -> Step {
    match outcome {
        Outcome::Failed(text) => Step::Failed(Notice::error(text.clone())),
        Outcome::Info(text) => Step::Done(Notice::info(text.clone())),
        _ => Step::Stay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::load_snapshot;
    use crate::mock::MockBackend;

    #[test]
    fn a_step_lists_the_effects_it_carries_in_order() {
        let step = Step::All(vec![
            Step::Done(Notice::ok("done")),
            Step::Run(vec![Effect::Refresh]),
            Step::Go(Dest::Back),
            Step::All(vec![Step::Run(vec![Effect::RefreshFeeds])]),
        ]);
        assert_eq!(step.effects(), [Effect::Refresh, Effect::RefreshFeeds]);
        assert_eq!(Step::run(Vec::new()), Step::Stay);
    }

    #[test]
    fn taking_every_site_offline_changes_only_the_sites_that_are_online() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let online: Vec<String> =
            snapshot.web.sites.iter().filter(|s| s.exposure != Exposure::default()).map(|s| s.package.clone()).collect();
        assert!(!online.is_empty() && online.len() < snapshot.web.sites.len(), "the mock has sites of both kinds");
        let effects = Confirm::AllSitesOffline.step(&snapshot).effects();
        let named: Vec<String> = effects
            .iter()
            .map(|effect| match effect {
                Effect::SetExposure { site, exposure, .. } if *exposure == Exposure::default() => site.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(named, online);
    }

    #[test]
    fn removing_a_source_from_its_key_change_page_leaves_that_page() {
        let snapshot = Snapshot::default();
        let remove = Effect::RemoveFeed("old-tools".into());
        let plain = Confirm::RemoveSource { name: "old-tools".into(), from_key_change: false }.step(&snapshot);
        assert_eq!(plain, Step::Run(vec![remove.clone()]));
        let from_page = Confirm::RemoveSource { name: "old-tools".into(), from_key_change: true }.step(&snapshot);
        assert_eq!(from_page, Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![remove])]));
    }
}
