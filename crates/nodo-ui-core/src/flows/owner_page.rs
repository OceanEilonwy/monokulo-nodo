//! An app's setup page for the owner: the device passphrase first, then the checklist the app
//! gives, with its values, QR codes and DNS records.
//!
//! The page holds secrets of the app, so the helper asks for the device passphrase each time it
//! is read. The flow keeps that passphrase in memory only while the page is open, so the page
//! can be read again; dropping the flow drops it. A frontend never shows it and never sends it
//! anywhere but to the helper.

use nodo_api::{ApiError, OwnerPageView};

use crate::flows::{documents, Dest, Notice, Step};
use crate::model::Snapshot;
use crate::secret::{Input, Secret};
use crate::{Effect, Outcome};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerPage {
    pub package: String,
    /// The row's name on the app's page, such as "Setup".
    pub title: String,
    /// The device passphrase the page was opened with, kept to read it again.
    pub held: Option<Secret>,
    /// The page, once the passphrase opened it. `None` is the passphrase gate.
    pub view: Option<OwnerPageView>,
    pub error: Option<String>,
    /// A read is under way.
    pub loading: bool,
}

/// Opens an app's setup page, at its passphrase gate. An app without one says so.
pub fn open(snapshot: &Snapshot, package: &str) -> Step {
    match snapshot.owner_pages.iter().find(|(name, _)| name == package) {
        Some((_, title)) => Step::Go(Dest::OwnerPage(OwnerPage::new(package, title))),
        None => Step::Done(Notice::info(format!("{package} has no setup page."))),
    }
}

impl OwnerPage {
    pub fn new(package: &str, title: &str) -> Self {
        Self { package: package.to_owned(), title: title.to_owned(), held: None, view: None, error: None, loading: false }
    }

    /// Opens the page with the device passphrase typed, which is taken out of the box. An empty
    /// box is refused, and nothing is asked twice while a read is under way.
    pub fn unlock(&mut self, passphrase: &mut Input) -> Vec<Effect> {
        if self.loading || self.view.is_some() {
            return Vec::new();
        }
        if passphrase.is_empty() {
            self.error = Some("Type the device passphrase.".to_owned());
            return Vec::new();
        }
        let passphrase = passphrase.take_secret();
        self.held = Some(passphrase.clone());
        self.loading = true;
        self.error = None;
        vec![Effect::LoadOwnerPage { package: self.package.clone(), passphrase }]
    }

    /// Reads the open page again, with the passphrase it was opened with.
    pub fn refresh(&mut self) -> Vec<Effect> {
        if self.loading || self.view.is_none() {
            return Vec::new();
        }
        let Some(passphrase) = self.held.clone() else {
            return Vec::new();
        };
        self.loading = true;
        vec![Effect::LoadOwnerPage { package: self.package.clone(), passphrase }]
    }

    /// Everything on the open page that can be copied, in the order drawn, each with its label.
    pub fn copy_targets(&self) -> Vec<(String, String)> {
        self.view.as_ref().map(documents::copy_targets).unwrap_or_default()
    }

    /// A page read for another app is not this one's. A wrong passphrase closes the page to its
    /// gate; any other failure keeps what is shown, with the reason.
    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        let Outcome::OwnerPageLoaded { package, result } = outcome else {
            return Step::Stay;
        };
        if *package != self.package {
            return Step::Stay;
        }
        self.loading = false;
        match result {
            Ok(view) => {
                self.view = Some(view.clone());
                self.error = None;
            }
            Err(ApiError::BadPassphrase) => {
                self.held = None;
                self.view = None;
                self.error = Some("That passphrase did not unlock this Nodo.".to_owned());
            }
            Err(e) => {
                if self.view.is_none() {
                    self.held = None;
                }
                self.error = Some(e.to_string());
            }
        }
        Step::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::{sample_owner_page, MockBackend, MOCK_PASSPHRASE};

    fn secret(text: &str) -> Input {
        let mut input = Input::secret();
        text.chars().for_each(|c| input.push(c));
        input
    }

    fn run(mock: &mut MockBackend, page: &mut OwnerPage, effects: Vec<Effect>) {
        for outcome in effects.into_iter().flat_map(|effect| execute_effect(mock, effect)) {
            assert_eq!(page.apply(&outcome), Step::Stay);
        }
    }

    fn with_setup() -> (MockBackend, OwnerPage) {
        let mut mock = MockBackend::new();
        mock.owner_views.insert("pingers".to_owned(), ("Setup".to_owned(), sample_owner_page()));
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let Step::Go(Dest::OwnerPage(page)) = open(&snapshot, "pingers") else { panic!("pingers has a setup page") };
        (mock, page)
    }

    #[test]
    fn only_an_app_with_a_setup_page_opens_one() {
        let (mut mock, page) = with_setup();
        assert_eq!((page.package.as_str(), page.title.as_str()), ("pingers", "Setup"));
        assert!(page.view.is_none() && page.held.is_none() && !page.loading, "it opens at the gate");
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        assert_eq!(open(&snapshot, "tor"), Step::Done(Notice::info("tor has no setup page.")));
    }

    #[test]
    fn the_gate_takes_the_passphrase_and_a_wrong_one_asks_again() {
        let (mut mock, mut page) = with_setup();
        assert!(page.unlock(&mut Input::secret()).is_empty());
        assert_eq!(page.error.as_deref(), Some("Type the device passphrase."));
        assert!(page.refresh().is_empty(), "nothing is read before the gate is passed");

        let mut wrong = secret("not the passphrase");
        let effects = page.unlock(&mut wrong);
        assert!(wrong.is_empty() && page.loading);
        assert!(page.unlock(&mut secret("again")).is_empty(), "nothing is asked twice");
        run(&mut mock, &mut page, effects);
        assert_eq!(page.error.as_deref(), Some("That passphrase did not unlock this Nodo."));
        assert!(page.view.is_none() && page.held.is_none() && !page.loading);

        let effects = page.unlock(&mut secret(MOCK_PASSPHRASE));
        assert!(!format!("{effects:?}").contains(MOCK_PASSPHRASE), "secrets never print");
        run(&mut mock, &mut page, effects);
        assert!(page.view.is_some() && page.error.is_none());
        assert!(!page.copy_targets().is_empty());
        assert!(page.unlock(&mut secret(MOCK_PASSPHRASE)).is_empty(), "an open page has no gate");
    }

    #[test]
    fn the_open_page_is_read_again_with_the_held_passphrase() {
        let (mut mock, mut page) = with_setup();
        let effects = page.unlock(&mut secret(MOCK_PASSPHRASE));
        run(&mut mock, &mut page, effects);
        let effects = page.refresh();
        let [Effect::LoadOwnerPage { package, passphrase }] = effects.as_slice() else { panic!("{effects:?}") };
        assert_eq!((package.as_str(), passphrase.expose()), ("pingers", MOCK_PASSPHRASE));
        assert!(page.loading && page.refresh().is_empty());

        // A page read for another app is not this one's.
        let other = Outcome::OwnerPageLoaded { package: "shop".to_owned(), result: Err(ApiError::BadPassphrase) };
        assert_eq!(page.apply(&other), Step::Stay);
        assert!(page.loading && page.view.is_some());

        // A failure that is not the passphrase keeps the page and what opens it.
        let failed = Outcome::OwnerPageLoaded { package: "pingers".to_owned(), result: Err(ApiError::Failed("busy".to_owned())) };
        assert_eq!(page.apply(&failed), Step::Stay);
        assert!(page.view.is_some() && page.held.is_some() && page.error.as_deref().is_some_and(|e| e.contains("busy")));
        run(&mut mock, &mut page, vec![Effect::LoadOwnerPage { package: "pingers".to_owned(), passphrase: Secret::new(MOCK_PASSPHRASE) }]);
        assert!(page.error.is_none());

        // The passphrase changed meanwhile: the page closes to its gate and nothing is held.
        let refused = Outcome::OwnerPageLoaded { package: "pingers".to_owned(), result: Err(ApiError::BadPassphrase) };
        assert_eq!(page.apply(&refused), Step::Stay);
        assert!(page.view.is_none() && page.held.is_none() && page.copy_targets().is_empty());

        // At the gate, a failure that is not the passphrase holds nothing either.
        let effects = page.unlock(&mut secret(MOCK_PASSPHRASE));
        assert_eq!(effects.len(), 1);
        assert_eq!(page.apply(&failed), Step::Stay);
        assert!(page.held.is_none() && page.view.is_none());
    }
}
