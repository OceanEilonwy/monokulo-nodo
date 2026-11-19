//! Installed apps and one app's page: start, stop, restart, update and remove.
//!
//! The page has no state beyond the app's name, so [`AppPage`] is that name with the actions on
//! it; everything else is read from the device's `Snapshot`. What the helper answers to a start,
//! stop, restart or removal comes back as the outcomes every page shares (`flows::apply`).

use nodo_api::ServiceState;

use super::{Confirm, Dest, Hint, Notice, Question, Step};
use crate::model::Snapshot;
use crate::Effect;

/// The installed apps, in the order the device lists them.
pub fn installed_apps(snapshot: &Snapshot) -> Vec<String> {
    snapshot.services.iter().map(|svc| svc.id.clone()).collect()
}

/// One installed app's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPage {
    pub name: String,
}

impl AppPage {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_owned() }
    }

    /// The app's state, `None` when it is not installed (any more).
    pub fn state(&self, snapshot: &Snapshot) -> Option<ServiceState> {
        snapshot.services.iter().find(|svc| svc.id == self.name).map(|svc| svc.state)
    }

    /// An update is offered for the app and is not held back.
    pub fn has_update(&self, snapshot: &Snapshot) -> bool {
        snapshot.updates.iter().any(|item| item.name == self.name && !item.held)
    }

    /// The app has a web page, so it has a Hosting page.
    pub fn has_site(&self, snapshot: &Snapshot) -> bool {
        snapshot.web.sites.iter().any(|site| site.package == self.name)
    }

    pub fn restart(&self) -> Vec<Effect> {
        vec![Effect::Restart(self.name.clone())]
    }

    pub fn stop(&self) -> Vec<Effect> {
        vec![Effect::Stop(self.name.clone())]
    }

    /// The one start-or-stop button: stops a running app and starts any other, except one that
    /// cannot start yet, which says why instead.
    pub fn start_or_stop(&self, snapshot: &Snapshot) -> Step {
        let name = &self.name;
        match self.state(snapshot) {
            Some(ServiceState::Running) => Step::Run(self.stop()),
            Some(ServiceState::Locked) => Step::Failed(Notice::error(format!("{name} is locked.")).hinting(Hint::Unlock)),
            Some(ServiceState::Waiting) => {
                Step::Failed(Notice::error(format!("{name} is waiting for another app it depends on.")))
            }
            _ => Step::Run(vec![Effect::Start(name.clone())]),
        }
    }

    /// The app's update: the Updates page with only this app chosen. Without one, a locked
    /// device is sent to unlock, since it cannot tell what is new, and an unlocked one says the
    /// app is up to date.
    pub fn update(&self, snapshot: &Snapshot) -> Step {
        if self.has_update(snapshot) {
            Step::Go(Dest::Updates { only: Some(self.name.clone()) })
        } else if snapshot.is_locked() {
            Step::Go(Dest::Unlock { resume: None })
        } else {
            Step::Done(Notice::info(format!("{} is up to date.", self.name)))
        }
    }

    /// Removing asks first. Confirming removes the app and goes to the list of apps.
    pub fn ask_remove(&self) -> Question {
        ask_remove(&self.name)
    }
}

/// The question before an app is removed: what goes, what stays, and what blocks it.
pub fn ask_remove(name: &str) -> Question {
    Question::careful(
        format!("Remove {name}?"),
        vec![
            format!("{name} stops and its program is removed. Anything that relies on it stops working."),
            "Its data stays on the drive, so installing it again brings everything back. A site still online has to be taken offline first, in Hosting.".to_owned(),
        ],
        format!("Remove {name}"),
        Confirm::RemoveApp(name.to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::flows;
    use crate::mock::MockBackend;
    use crate::Outcome;

    fn snapshot(mock: &mut MockBackend) -> Snapshot {
        load_snapshot(mock).expect("the mock answers")
    }

    fn with_state(snapshot: &Snapshot, name: &str, state: ServiceState) -> Snapshot {
        let mut changed = snapshot.clone();
        changed.services.iter_mut().find(|svc| svc.id == name).expect("installed").state = state;
        changed
    }

    #[test]
    fn the_start_or_stop_button_follows_the_apps_state() {
        let mut mock = MockBackend::new();
        let snapshot = snapshot(&mut mock);
        let name = installed_apps(&snapshot).into_iter().next().expect("the mock has apps");
        let page = AppPage::new(&name);

        let running = with_state(&snapshot, &name, ServiceState::Running);
        assert_eq!(page.start_or_stop(&running), Step::Run(vec![Effect::Stop(name.clone())]));
        for stopped in [ServiceState::Stopped, ServiceState::Failed] {
            let snapshot = with_state(&snapshot, &name, stopped);
            assert_eq!(page.start_or_stop(&snapshot), Step::Run(vec![Effect::Start(name.clone())]));
        }
        // One that cannot start yet says why and asks nothing of the helper.
        // What a locked one points at is said beside the words, which name no key.
        for (state, word, hint) in [(ServiceState::Locked, "locked.", Some(Hint::Unlock)), (ServiceState::Waiting, "waiting", None)] {
            let Step::Failed(notice) = page.start_or_stop(&with_state(&snapshot, &name, state)) else {
                panic!("{state:?} is refused");
            };
            assert!(notice.text.contains(word), "{}", notice.text);
            assert!(!notice.text.contains("Press"), "{}", notice.text);
            assert_eq!(notice.hint, hint);
        }
    }

    #[test]
    fn stopping_and_starting_reach_the_device() {
        let mut mock = MockBackend::new();
        let before = snapshot(&mut mock);
        let page = AppPage::new("cuprated");
        assert_eq!(page.state(&before), Some(ServiceState::Running));
        let Step::Run(effects) = page.start_or_stop(&before) else { panic!("a running app stops") };
        let mut after = before;
        for effect in effects {
            for outcome in execute_effect(&mut mock, effect) {
                if let Outcome::Refreshed(fresh) = outcome {
                    after = *fresh;
                }
            }
        }
        assert_eq!(page.state(&after), Some(ServiceState::Stopped));
        assert_eq!(page.start_or_stop(&after), Step::Run(vec![Effect::Start("cuprated".into())]));
    }

    #[test]
    fn the_update_button_goes_to_the_update_or_says_why_not() {
        let mut mock = MockBackend::new();
        let snapshot = snapshot(&mut mock);
        let with_update = snapshot.updates.iter().find(|item| !item.held).expect("the mock offers an update").name.clone();
        assert_eq!(
            AppPage::new(&with_update).update(&snapshot),
            Step::Go(Dest::Updates { only: Some(with_update.clone()) })
        );

        let mut none = snapshot.clone();
        none.updates.clear();
        none.overview.locked = false;
        none.services.iter_mut().filter(|svc| svc.state == ServiceState::Locked).for_each(|svc| svc.state = ServiceState::Running);
        let Step::Done(notice) = AppPage::new(&with_update).update(&none) else { panic!("up to date") };
        assert_eq!(notice.text, format!("{with_update} is up to date."));

        none.overview.locked = true;
        assert_eq!(AppPage::new(&with_update).update(&none), Step::Go(Dest::Unlock { resume: None }));

        // A held version is not offered.
        let mut held = snapshot.clone();
        held.updates.iter_mut().for_each(|item| item.held = true);
        assert!(!AppPage::new(&with_update).has_update(&held));
    }

    #[test]
    fn removing_asks_first_then_removes_and_goes_to_the_apps() {
        let mut mock = MockBackend::new();
        let before = snapshot(&mut mock);
        // An app with no site online, so nothing blocks its removal.
        let name = installed_apps(&before)
            .into_iter()
            .find(|name| !before.active_sites().iter().any(|site| &site.package == name))
            .expect("an app without a site online");
        let question = AppPage::new(&name).ask_remove();
        assert!(question.cancel_is_default, "a stray Enter must not remove an app");
        assert_eq!(question.title, format!("Remove {name}?"));
        assert!(question.body[1].contains("Its data stays on the drive"));
        assert!(question.body[1].contains("taken offline first"));

        let step = question.confirm.step(&before);
        assert_eq!(step, Step::All(vec![Step::Go(Dest::Apps), Step::Run(vec![Effect::RemovePackage(name.clone())])]));
        // The helper's answer is one of the outcomes every page shares, and the device is read
        // again after it either way.
        let outcomes: Vec<Outcome> = step.effects().into_iter().flat_map(|effect| execute_effect(&mut mock, effect)).collect();
        assert!(matches!(flows::apply(&outcomes[0]), Step::Done(_) | Step::Failed(_)), "{:?}", outcomes[0]);
        assert!(matches!(outcomes.last(), Some(Outcome::Refreshed(_))), "the device is read again");
        assert_eq!(flows::apply(outcomes.last().expect("checked above")), Step::Stay, "the frontend keeps the snapshot");
    }
}
