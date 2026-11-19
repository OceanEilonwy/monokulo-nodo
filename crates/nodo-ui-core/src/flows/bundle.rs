//! Installing a suite of apps together, from the review of the head that declares it.
//!
//! The steps, in order: the review of every app with its permissions, the grants between them and
//! which apps keep their data on the data drive; the settings the bundle asks for once, when it
//! asks for any; the device passphrase, when new encrypted storage or a secret answer needs it;
//! the install; and what it came to. Nothing is installed before the last step is taken.

use nodo_api::{ApiError, BundleAnswer, BundleReport, BundleReview};

use crate::flows::{owner_page, Dest, Notice, Step};
use crate::model::Snapshot;
use crate::secret::Input;
use crate::{Effect, Outcome};

/// Where installing a bundle stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleStep {
    /// The apps it installs with their permissions, the grants and the data drive choices.
    Review,
    /// The settings the bundle asks for once, before anything installs.
    Answers,
    /// The device passphrase, for new encrypted storage or a secret answer while locked.
    Passphrase,
    /// The helper is installing.
    Installing,
    Done(BundleReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub review: BundleReview,
    pub step: BundleStep,
    /// Per `review.data_drive`: keep that app's data on the data drive. On by default when one
    /// is present.
    pub drive: Vec<bool>,
    pub error: Option<String>,
    /// The answers given, as the helper takes them, kept until the install is asked for.
    answers: Vec<BundleAnswer>,
}

impl Bundle {
    pub fn new(review: BundleReview) -> Self {
        let drive = vec![review.data_drive_present; review.data_drive.len()];
        Self { review, step: BundleStep::Review, drive, error: None, answers: Vec::new() }
    }

    /// The asks the owner answers, as indices into `review.asks`: those for apps not installed
    /// yet.
    pub fn open_asks(&self) -> Vec<usize> {
        (0..self.review.asks.len()).filter(|&i| !self.review.asks[i].skipped).collect()
    }

    /// An empty box per ask of the review, in order. A secret ask is a masked box.
    pub fn answer_boxes(&self) -> Vec<Input> {
        self.review.asks.iter().map(|ask| if ask.secret { Input::secret() } else { Input::plain("") }).collect()
    }

    /// The apps this install puts their data on the data drive for.
    pub fn chosen_drive(&self) -> Vec<String> {
        self.review.data_drive.iter().zip(&self.drive).filter(|(_, on)| **on).map(|(id, _)| id.clone()).collect()
    }

    /// Switches whether an app's data goes on the data drive, by the app's name, in the review.
    pub fn toggle_drive(&mut self, app: &str) {
        if self.step != BundleStep::Review {
            return;
        }
        if let Some(index) = self.review.data_drive.iter().position(|id| id == app) {
            self.drive[index] = !self.drive[index];
        }
    }

    /// Continues from the review: to the questions when there are any, else to the passphrase
    /// when one is needed, else the install starts.
    pub fn proceed(&mut self) -> Vec<Effect> {
        if self.step != BundleStep::Review {
            return Vec::new();
        }
        self.error = None;
        if !self.open_asks().is_empty() {
            self.step = BundleStep::Answers;
            return Vec::new();
        }
        self.answers.clear();
        self.after_answers()
    }

    /// Takes the answers: one box per ask of the review, in order, as [`Bundle::answer_boxes`]
    /// makes them. Every open ask needs an answer; `Err` is the index of the first ask without
    /// one, which `error` names. The boxes are left as they are, so going back shows them again.
    pub fn answer(&mut self, answers: &[Input]) -> Result<Vec<Effect>, usize> {
        if self.step != BundleStep::Answers {
            return Ok(Vec::new());
        }
        let open = self.open_asks();
        if let Some(&empty) = open.iter().find(|&&i| answers.get(i).is_none_or(Input::is_empty)) {
            self.error = Some(format!("Fill in {} first.", self.review.asks[empty].label));
            return Err(empty);
        }
        self.error = None;
        // Every value travels as a secret, whether its ask is one or not.
        self.answers = open
            .into_iter()
            .map(|i| {
                let ask = &self.review.asks[i];
                BundleAnswer {
                    package: ask.package.clone(),
                    setting: ask.setting.clone(),
                    value: nodo_api::Secret::new(answers[i].secret_value().expose()),
                }
            })
            .collect();
        Ok(self.after_answers())
    }

    fn after_answers(&mut self) -> Vec<Effect> {
        if self.review.needs_passphrase {
            self.step = BundleStep::Passphrase;
            return Vec::new();
        }
        self.start(None)
    }

    /// Starts the install with the device passphrase typed, which is taken out of the box. An
    /// empty box is refused.
    pub fn submit_passphrase(&mut self, passphrase: &mut Input) -> Vec<Effect> {
        if self.step != BundleStep::Passphrase {
            return Vec::new();
        }
        if passphrase.is_empty() {
            self.error = Some("Type the device passphrase.".to_owned());
            return Vec::new();
        }
        self.start(Some(passphrase))
    }

    fn start(&mut self, passphrase: Option<&mut Input>) -> Vec<Effect> {
        let passphrase = passphrase.filter(|_| self.review.needs_passphrase).map(Input::take_secret);
        self.step = BundleStep::Installing;
        self.error = None;
        vec![Effect::InstallBundle {
            review: Box::new(self.review.clone()),
            answers: self.answers.clone(),
            data_drive: self.chosen_drive(),
            passphrase,
        }]
    }

    /// One step back: the passphrase to the questions (or the review when nothing is asked),
    /// the questions to the review. From the review and from the result it leaves the page;
    /// nothing was installed in the first case. While the helper installs it does nothing.
    pub fn back(&mut self) -> Step {
        match self.step {
            BundleStep::Review | BundleStep::Done(_) => return Step::Go(Dest::Back),
            BundleStep::Answers => self.step = BundleStep::Review,
            BundleStep::Passphrase => {
                self.step = if self.open_asks().is_empty() { BundleStep::Review } else { BundleStep::Answers };
            }
            BundleStep::Installing => return Step::Stay,
        }
        self.error = None;
        Step::Stay
    }

    /// The owner has read the result. An install without a problem carries on to the head's
    /// setup page, when it has one; any other is left.
    pub fn finish(&self, snapshot: &Snapshot) -> Step {
        match &self.step {
            BundleStep::Done(report) if report.problem.is_none() => {
                Step::All(vec![Step::Go(Dest::Back), owner_page::open(snapshot, &report.package)])
            }
            BundleStep::Done(_) => Step::Go(Dest::Back),
            _ => Step::Stay,
        }
    }

    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        let Outcome::BundleInstalled(result) = outcome else {
            return Step::Stay;
        };
        match result {
            Ok(report) => {
                self.answers.clear();
                self.step = BundleStep::Done(report.clone());
            }
            Err(text) if *text == ApiError::BadPassphrase.to_string() => {
                self.step = BundleStep::Passphrase;
                self.error = Some("That passphrase did not unlock this Nodo. Nothing was installed.".to_owned());
            }
            Err(text) => {
                self.step = BundleStep::Review;
                self.error = Some(text.clone());
            }
        }
        Step::Stay
    }
}

/// What a bundle's install came to, for a frontend whose owner has left the bundle's page.
pub fn apply(outcome: &Outcome) -> Step {
    match outcome {
        Outcome::BundleInstalled(Ok(report)) => match &report.problem {
            None => Step::Done(Notice::ok(format!("{} and its apps are installed.", report.package))),
            Some(problem) => Step::Failed(Notice::error(problem.clone())),
        },
        Outcome::BundleInstalled(Err(text)) => Step::Failed(Notice::error(text.clone())),
        _ => Step::Stay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::{sample_bundle_review, sample_owner_page, MockBackend, MOCK_PASSPHRASE};

    fn typed(mut input: Input, text: &str) -> Input {
        text.chars().for_each(|c| input.push(c));
        input
    }

    fn run(mock: &mut MockBackend, page: &mut Bundle, effects: Vec<Effect>) {
        let outcomes: Vec<Outcome> = effects.into_iter().flat_map(|effect| execute_effect(mock, effect)).collect();
        assert!(matches!(outcomes.as_slice(), [Outcome::BundleInstalled(_), Outcome::Refreshed(_)]), "{outcomes:?}");
        for outcome in &outcomes {
            assert_eq!(page.apply(outcome), Step::Stay);
        }
    }

    fn answered(page: &Bundle) -> Vec<Input> {
        let mut boxes = page.answer_boxes();
        boxes[0] = typed(boxes[0].clone(), "example.com");
        boxes[1] = typed(boxes[1].clone(), "{\"type\":\"service_account\"}");
        boxes
    }

    #[test]
    fn the_review_offers_the_data_drive_and_asks_only_for_apps_not_installed() {
        let mut review = sample_bundle_review();
        let page = Bundle::new(review.clone());
        assert_eq!((page.step.clone(), page.drive.clone()), (BundleStep::Review, vec![true]));
        assert_eq!(page.chosen_drive(), ["pingers-map-host"]);
        assert_eq!(page.open_asks(), [0, 1]);
        let boxes = page.answer_boxes();
        assert!(!boxes[0].is_secret() && boxes[1].is_secret(), "a secret ask is a masked box");

        review.data_drive_present = false;
        review.asks[0].skipped = true;
        let mut page = Bundle::new(review);
        assert_eq!(page.drive, [false], "off without a data drive");
        assert_eq!(page.open_asks(), [1]);
        page.toggle_drive("pingers-map-host");
        page.toggle_drive("not in the bundle");
        assert_eq!(page.chosen_drive(), ["pingers-map-host"]);
    }

    #[test]
    fn a_bundle_goes_from_the_review_through_the_answers_and_the_passphrase_to_its_setup_page() {
        let mut mock = MockBackend::new();
        mock.owner_views.insert("pingers".to_owned(), ("Setup".to_owned(), sample_owner_page()));
        let mut page = Bundle::new(sample_bundle_review());
        page.toggle_drive("pingers-map-host");
        assert!(page.proceed().is_empty());
        assert_eq!(page.step, BundleStep::Answers);
        page.toggle_drive("pingers-map-host");
        assert_eq!(page.drive, [false], "the choices are made in the review");

        // An empty answer is refused, with the ask named.
        let mut boxes = page.answer_boxes();
        boxes[1] = typed(boxes[1].clone(), "key");
        assert_eq!(page.answer(&boxes), Err(0));
        assert_eq!(page.error.as_deref(), Some("Fill in Your domain first."));
        assert_eq!(page.answer(&[]), Err(0));

        let boxes = answered(&page);
        assert_eq!(page.answer(&boxes), Ok(Vec::new()));
        assert_eq!((page.step.clone(), page.error.clone()), (BundleStep::Passphrase, None));
        assert!(!boxes[0].is_empty(), "the boxes keep what was typed");

        assert!(page.submit_passphrase(&mut Input::secret()).is_empty());
        assert_eq!(page.error.as_deref(), Some("Type the device passphrase."));
        let mut wrong = typed(Input::secret(), "wrong one");
        let effects = page.submit_passphrase(&mut wrong);
        assert!(wrong.is_empty() && page.step == BundleStep::Installing);
        assert_eq!(page.back(), Step::Stay, "nothing is left while the helper installs");
        run(&mut mock, &mut page, effects);
        assert_eq!(page.step, BundleStep::Passphrase, "a wrong passphrase asks again");
        assert_eq!(page.error.as_deref(), Some("That passphrase did not unlock this Nodo. Nothing was installed."));
        assert!(mock.bundle_installs.is_empty());

        let effects = page.submit_passphrase(&mut typed(Input::secret(), MOCK_PASSPHRASE));
        let [Effect::InstallBundle { answers, data_drive, passphrase: Some(_), .. }] = effects.as_slice() else {
            panic!("{effects:?}")
        };
        assert_eq!(answers.iter().map(|a| (a.setting.as_str(), a.value.expose())).collect::<Vec<_>>(), [
            ("domain", "example.com"),
            ("fcm_service_account", "{\"type\":\"service_account\"}"),
        ]);
        assert!(data_drive.is_empty());
        assert!(!format!("{effects:?}").contains("service_account\\\""), "answers never print");
        run(&mut mock, &mut page, effects);
        let BundleStep::Done(report) = &page.step else { panic!("{:?}", page.step) };
        assert_eq!(report.problem, None);

        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let Step::All(steps) = page.finish(&snapshot) else { panic!("installed") };
        assert_eq!(steps[0], Step::Go(Dest::Back));
        assert!(matches!(&steps[1], Step::Go(Dest::OwnerPage(setup)) if setup.package == "pingers" && setup.title == "Setup"));
    }

    #[test]
    fn a_bundle_that_asks_nothing_and_needs_no_passphrase_installs_from_the_review() {
        let mut mock = MockBackend::new();
        let mut review = sample_bundle_review();
        review.asks.iter_mut().for_each(|ask| ask.skipped = true);
        review.needs_passphrase = false;
        let mut page = Bundle::new(review.clone());
        let effects = page.proceed();
        let [Effect::InstallBundle { answers, data_drive, passphrase: None, .. }] = effects.as_slice() else { panic!("{effects:?}") };
        assert!(answers.is_empty(), "an ask for an installed app is not answered");
        assert_eq!(data_drive, &["pingers-map-host"]);
        run(&mut mock, &mut page, effects);
        assert!(matches!(page.step, BundleStep::Done(_)));
        // Without a setup page the result is left with a word about it.
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let Step::All(steps) = page.finish(&snapshot) else { panic!("installed") };
        assert_eq!(steps[1], Step::Done(Notice::info("pingers has no setup page.")));

        // With only the passphrase to ask, going back from it returns to the review.
        review.needs_passphrase = true;
        let mut page = Bundle::new(review);
        assert!(page.proceed().is_empty());
        assert_eq!(page.step, BundleStep::Passphrase);
        assert_eq!(page.back(), Step::Stay);
        assert_eq!(page.step, BundleStep::Review);
        assert_eq!(page.back(), Step::Go(Dest::Back));
    }

    #[test]
    fn going_back_keeps_the_choices_and_a_refusal_returns_to_the_review() {
        let mut page = Bundle::new(sample_bundle_review());
        assert!(page.proceed().is_empty());
        let boxes = answered(&page);
        assert_eq!(page.answer(&boxes), Ok(Vec::new()));
        assert_eq!(page.back(), Step::Stay);
        assert_eq!(page.step, BundleStep::Answers);
        assert_eq!(page.back(), Step::Stay);
        assert_eq!(page.step, BundleStep::Review);

        assert_eq!(page.apply(&Outcome::BundleInstalled(Err("pingers-server is not signed".to_owned()))), Step::Stay);
        assert_eq!((page.step.clone(), page.error.as_deref()), (BundleStep::Review, Some("pingers-server is not signed")));

        let mut report = nodo_api::BundleReport { package: "pingers".to_owned(), members: Vec::new(), problem: None };
        report.problem = Some("pingers-server failed to start".to_owned());
        assert_eq!(page.apply(&Outcome::BundleInstalled(Ok(report.clone()))), Step::Stay);
        assert_eq!(page.finish(&Snapshot::default()), Step::Go(Dest::Back), "a problem does not lead to setup");

        // Off the page the same results are a line each.
        assert_eq!(apply(&Outcome::BundleInstalled(Ok(report.clone()))), Step::Failed(Notice::error("pingers-server failed to start")));
        report.problem = None;
        assert_eq!(apply(&Outcome::BundleInstalled(Ok(report))), Step::Done(Notice::ok("pingers and its apps are installed.")));
        assert_eq!(apply(&Outcome::BundleInstalled(Err("no".to_owned()))), Step::Failed(Notice::error("no")));
        assert_eq!(apply(&Outcome::Info("something else".to_owned())), Step::Stay);
    }
}
