//! The step runner: this interface's one function that follows a `Step` of the core
//! (`nodo_ui_core::flows`, "Following a step").
//!
//! A web request runs the effects before it answers: each batch goes through
//! `exec::run_batch`, every `Outcome` is handed to the page's flow and to the flows that keep no
//! state, and the steps they answer with are followed in turn until none carries effects. What
//! is left is a [`Reply`]: the notices to show, a question to ask, and the page to go to.

use std::collections::VecDeque;

use nodo_ui_core::exec::{run_batch, Data};
use nodo_ui_core::flows::{self, Dest, Notice, Question, Step};
use nodo_ui_core::model::Snapshot;
use nodo_ui_core::{Effect, Outcome};

use crate::glue::says_login_required;

/// More rounds of effects than any flow asks for. A flow that answers its own outcome with the
/// same effects for ever is stopped here and said, instead of holding the request.
const MAX_ROUNDS: usize = 32;

/// What a followed step came to. A handler turns it into a response (`ctx::Ctx::respond`).
#[derive(Debug, Default)]
pub struct Reply {
    /// Every notice the steps carried, in order.
    pub notices: Vec<Notice>,
    /// The owner must confirm first. Nothing has been done yet.
    pub question: Option<Question>,
    /// The page to go to: the last one named, as the steps are followed in order.
    pub go: Option<Dest>,
    /// The helper refused because nobody is logged in any more. Every session is then dropped.
    pub login_required: bool,
    /// Where Cancel on the question leads, when that is not where the reply goes back to.
    pub cancel: Option<String>,
}

impl Reply {
    /// A reply that only says something.
    pub fn notice(notice: Notice) -> Self {
        Self { notices: vec![notice], ..Self::default() }
    }

    /// A reply that asks.
    pub fn ask(question: Question) -> Self {
        Self { question: Some(question), ..Self::default() }
    }

    /// Cancel on this reply's question returns to `url`.
    pub fn cancel_to(mut self, url: impl Into<String>) -> Self {
        self.cancel = Some(url.into());
        self
    }

    /// The first error among the notices, for a page that shows it beside a field.
    pub fn error(&self) -> Option<&str> {
        self.notices.iter().find(|n| n.kind == flows::NoticeKind::Error).map(|n| n.text.as_str())
    }
}

/// What a page's own flow makes of an outcome. It may change the flow's state and the snapshot
/// (unlocking marks it unlocked). Pages without a flow of their own pass [`no_flow`].
pub type PageApply<'a> = &'a mut dyn FnMut(&Outcome, &mut Snapshot) -> Step;

/// For a page whose results are all understood by the flows without state.
pub fn no_flow(_: &Outcome, _: &mut Snapshot) -> Step {
    Step::Stay
}

/// The flows that keep no state understand their outcomes wherever the owner is, so every page
/// hands them every outcome that its own flow had nothing to say about. Each answers
/// `Step::Stay` to an outcome that is not its own. `snapshot` is the device as read after the
/// outcome. A feature whose flow has no state adds its `apply` here.
pub fn shared_apply(outcome: &Outcome, snapshot: &Snapshot) -> Step {
    let steps: Vec<Step> = [
        flows::apply(outcome),
        flows::sources::apply(outcome),
        flows::install::apply(outcome),
        flows::drive::apply(outcome),
        flows::documents::apply(outcome),
        flows::settings::apply(outcome),
        flows::updates::apply(outcome, snapshot),
        flows::web::apply(outcome),
        flows::web::providers::apply(outcome),
        flows::web::domain::apply(outcome),
    ]
    .into_iter()
    .filter(|step| *step != Step::Stay)
    .collect();
    if steps.is_empty() { Step::Stay } else { Step::All(steps) }
}

/// Follows `first` to its end against the backend. `snapshot` is replaced whenever the device is
/// read again, so it is current when the function returns.
pub fn run_steps(backend: &mut dyn Data, snapshot: &mut Snapshot, first: Step, page: PageApply<'_>) -> Reply {
    follow(snapshot, first, page, &mut |effects| {
        let mut outcomes = Vec::new();
        run_batch(backend, effects, |more| outcomes.extend(more));
        outcomes
    })
}

/// Follows a step that carries no effects, with no backend: a refusal, a question, a page to go
/// to. Effects, if it has any after all, are not performed and that is said.
pub fn follow_plain(first: Step) -> Reply {
    let mut skipped = false;
    let mut reply = follow(&mut Snapshot::default(), first, &mut no_flow, &mut |_| {
        skipped = true;
        Vec::new()
    });
    if skipped {
        reply.notices.push(Notice::error("That could not be done from here."));
    }
    reply
}

fn follow(
    snapshot: &mut Snapshot,
    first: Step,
    page: PageApply<'_>,
    perform: &mut dyn FnMut(Vec<Effect>) -> Vec<Outcome>,
) -> Reply {
    let mut reply = Reply::default();
    let mut queue = VecDeque::from([first]);
    let mut rounds = 0;
    while let Some(step) = queue.pop_front() {
        match step {
            Step::Stay => {}
            Step::Done(notice) | Step::Failed(notice) => reply.notices.push(notice),
            Step::Ask(question) => reply.question = Some(question),
            Step::Go(dest) => reply.go = Some(dest),
            // In order, ahead of whatever was already waiting.
            Step::All(steps) => steps.into_iter().rev().for_each(|step| queue.push_front(step)),
            Step::Run(effects) => {
                rounds += 1;
                if rounds > MAX_ROUNDS {
                    reply.notices.push(Notice::error("That did not come to an end, so it was stopped."));
                    break;
                }
                for outcome in perform(effects) {
                    if says_login_required(&outcome) {
                        reply.login_required = true;
                    }
                    if let Outcome::Refreshed(fresh) = &outcome {
                        *snapshot = (**fresh).clone();
                    }
                    // The results are followed after what the step itself still holds, as the
                    // terminal interface does when its effects come back from the worker. The
                    // page's own flow comes first; an outcome it answers is its own, and the
                    // flows without state are not asked about it a second time (an approval in
                    // the reference picker, automatic-update choices saved on their page).
                    let own = page(&outcome, snapshot);
                    if own == Step::Stay {
                        queue.push_back(shared_apply(&outcome, snapshot));
                    } else {
                        queue.push_back(own);
                    }
                }
            }
        }
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodo_ui_core::exec::load_snapshot;
    use nodo_ui_core::flows::{services, sources, Confirm};
    use nodo_ui_core::mock::MockBackend;

    fn mock() -> (MockBackend, Snapshot) {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        (mock, snapshot)
    }

    #[test]
    fn effects_are_run_and_the_snapshot_is_the_device_as_it_is_afterwards() {
        let (mut mock, mut snapshot) = mock();
        let name = snapshot.services[0].id.clone();
        let reply = run_steps(&mut mock, &mut snapshot, Step::Run(services::AppPage::new(&name).stop()), &mut no_flow);
        assert!(reply.notices.is_empty() && reply.go.is_none() && reply.question.is_none());
        assert_eq!(services::AppPage::new(&name).state(&snapshot), Some(nodo_api::ServiceState::Stopped));
    }

    #[test]
    fn a_result_that_calls_for_more_is_followed_to_the_page_it_ends_on() {
        // Probing answers with the confirmation page; nothing else is said.
        let (mut mock, mut snapshot) = mock();
        let effects = sources::probe("https://github.com/example/monero-tools-feed").expect("an address");
        let reply = run_steps(&mut mock, &mut snapshot, Step::Run(effects), &mut no_flow);
        assert!(matches!(reply.go, Some(Dest::ConfirmSource(_))), "{reply:?}");
    }

    #[test]
    fn all_is_followed_in_order_and_the_last_page_named_wins() {
        let (mut mock, mut snapshot) = mock();
        let name = snapshot.services[0].id.clone();
        // Removing goes to Apps and then removes; what the helper said about it arrives after.
        let step = Confirm::RemoveApp(name.clone()).step(&snapshot);
        let reply = run_steps(&mut mock, &mut snapshot, step, &mut no_flow);
        assert_eq!(reply.go, Some(Dest::Apps));
        assert_eq!(reply.notices.len(), 1, "{reply:?}");
        assert!(mock.calls.contains(&nodo_api::Request::RemovePackage { package: name }));

        let step = Step::All(vec![Step::Go(Dest::Apps), Step::Done(Notice::ok("one")), Step::Go(Dest::Back), Step::Done(Notice::ok("two"))]);
        let reply = follow_plain(step);
        assert_eq!(reply.go, Some(Dest::Back));
        assert_eq!(reply.notices.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), ["one", "two"]);
    }

    #[test]
    fn a_question_is_kept_and_nothing_is_done() {
        let (mock, _) = mock();
        let reply = follow_plain(Step::Ask(services::ask_remove("tor")));
        assert_eq!(reply.question.map(|q| q.confirm), Some(Confirm::RemoveApp("tor".into())));
        assert!(mock.calls.iter().all(|call| !matches!(call, nodo_api::Request::RemovePackage { .. })));
    }

    #[test]
    fn the_pages_own_flow_sees_every_outcome_and_its_steps_are_followed() {
        let (mut mock, mut snapshot) = mock();
        let mut seen = 0;
        let mut page = |outcome: &Outcome, _: &mut Snapshot| {
            seen += 1;
            match outcome {
                // One more round, asked for by the page's flow.
                Outcome::Refreshed(_) if seen == 1 => Step::Run(vec![Effect::LoadSshKeys]),
                Outcome::SshKeysLoaded(_) => Step::Done(Notice::ok("keys read")),
                _ => Step::Stay,
            }
        };
        let reply = run_steps(&mut mock, &mut snapshot, Step::Run(vec![Effect::Refresh]), &mut page);
        assert_eq!(seen, 2);
        assert_eq!(reply.notices, [Notice::ok("keys read")]);
    }

    #[test]
    fn a_flow_that_never_ends_is_stopped() {
        let (mut mock, mut snapshot) = mock();
        let mut page = |_: &Outcome, _: &mut Snapshot| Step::Run(vec![Effect::LoadSshKeys]);
        let reply = run_steps(&mut mock, &mut snapshot, Step::Run(vec![Effect::LoadSshKeys]), &mut page);
        assert!(reply.error().is_some_and(|text| text.contains("stopped")));
    }

    #[test]
    fn a_login_refusal_among_the_results_is_flagged() {
        let mut snapshot = Snapshot::default();
        let refusal = nodo_api::ApiError::web_login_required();
        let reply = follow(&mut snapshot, Step::Run(vec![Effect::Refresh]), &mut no_flow, &mut |_| {
            vec![Outcome::Failed(format!("Could not read device state: {refusal}"))]
        });
        assert!(reply.login_required);
    }
}
