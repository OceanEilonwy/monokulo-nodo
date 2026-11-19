//! Sources (feeds): add one by probing its address and trusting its key, name one, turn one on or
//! off, remove one, read them all again, and decide about a source whose signing key changed.
//!
//! A source is known by its `name`, which the device makes from its address and which installed
//! apps and a changed key refer to. The owner may give it a name of their own (`label`), shown in
//! its place, when adding it or later.
//!
//! The feature has no state of its own: the sources are in the `Snapshot`, a new source being
//! added is the helper's `FeedProbe`, and a changed key is its `KeyChange`. So these are
//! functions, and a frontend keeps the probe or the key change it is showing.

use super::{Confirm, Dest, Notice, Question, Step};
use crate::model::{DocKind, Feed, FeedProbe, FeedStatus, KeyChange};
use crate::{Effect, Outcome};

/// Reads every source again.
pub fn refresh() -> Vec<Effect> {
    vec![Effect::RefreshFeeds]
}

/// Turns a source on or off.
pub fn toggle(name: &str) -> Vec<Effect> {
    vec![Effect::ToggleFeed(name.to_owned())]
}

/// Removing a source asks first. `from_key_change` when it is asked on the page of that source's
/// changed key, which confirming then leaves.
pub fn ask_remove(name: &str, from_key_change: bool) -> Question {
    Question::careful(
        format!("Remove {name}?"),
        vec!["Apps you installed from it keep running; they just won't get updates until you add it back.".to_owned()],
        "Remove",
        Confirm::RemoveSource { name: name.to_owned(), from_key_change },
    )
}

/// Opening a source: one that is blocked because its key changed leads to that decision, any
/// other to the apps it offers.
pub fn open(feed: &Feed) -> Step {
    if feed.status == FeedStatus::Blocked {
        Step::Run(vec![Effect::LoadKeyChange(feed.name.clone())])
    } else {
        Step::Go(Dest::GetApps { source: Some(feed.name.clone()) })
    }
}

/// The first step of adding a source: asks what is at the address. The error belongs beside the
/// address box.
pub fn probe(address: &str) -> Result<Vec<Effect>, String> {
    if address.is_empty() {
        return Err("Enter the feed's address.".to_owned());
    }
    Ok(vec![Effect::ProbeFeed(address.trim().to_owned())])
}

/// The most characters a source's own name may have, as the helper allows.
pub const MAX_LABEL: usize = 48;

/// The second step: adds the probed source, shown by `probe.label` if the owner gave it one, and
/// pins the key it showed.
pub fn trust(probe: &FeedProbe) -> Vec<Effect> {
    vec![Effect::TrustFeed(probe.clone())]
}

/// Gives source `name` the name the owner typed, or with a blank one goes back to the name the
/// device made. The helper also refuses a name another source has. The error belongs beside the
/// box.
pub fn rename(name: &str, typed: &str) -> Result<Vec<Effect>, String> {
    let label = typed.trim();
    if label.chars().count() > MAX_LABEL {
        return Err(format!("A source's name can be at most {MAX_LABEL} characters."));
    }
    if label.chars().any(char::is_control) {
        return Err("A source's name can't contain control characters.".to_owned());
    }
    let label = (!label.is_empty() && label != name).then(|| label.to_owned());
    Ok(vec![Effect::RenameFeed { name: name.to_owned(), label }])
}

/// The packages a probed source offers, as a document, before deciding to trust it.
pub fn packages(probe: &FeedProbe) -> Vec<Effect> {
    vec![Effect::ShowDocument(DocKind::FeedPackages(probe.name.clone()))]
}

/// Trusts the new key of a source. The owner must have typed the source's name exactly, so the
/// one choice that lets a new signer in is never a single stray key. The error belongs beside
/// the box.
pub fn trust_new_key(change: &KeyChange, typed: &str) -> Result<Vec<Effect>, String> {
    if typed != change.feed {
        return Err(format!("That isn't the source's name. Type {} exactly.", change.feed));
    }
    Ok(vec![Effect::TrustNewKey { change: change.clone(), typed: typed.to_owned() }])
}

/// What a result means.
///
/// A probe that worked leads to the confirmation (`Dest::ConfirmSource`). One that failed is
/// `Step::Failed`, and since it is about the address typed, a frontend with the address box on
/// screen shows the text beside it rather than as a passing notice.
pub fn apply(outcome: &Outcome) -> Step {
    match outcome {
        Outcome::FeedProbed(Ok(probe)) => Step::Go(Dest::ConfirmSource(probe.clone())),
        Outcome::FeedProbed(Err(text)) => Step::Failed(Notice::error(text.clone())),
        Outcome::FeedTrusted(Ok(name)) => {
            Step::All(vec![Step::Go(Dest::Back), Step::Done(Notice::ok(format!("Added {name} and pinned its key.")))])
        }
        Outcome::FeedTrusted(Err(text)) => Step::Failed(Notice::error(text.clone())),
        Outcome::FeedRenamed(Ok(name)) => Step::Done(Notice::ok(format!("It's called {name} now."))),
        Outcome::FeedRenamed(Err(text)) => Step::Failed(Notice::error(text.clone())),
        Outcome::KeyChangeLoaded(Some(change)) => Step::Go(Dest::KeyChange(change.clone())),
        Outcome::KeyChangeLoaded(None) => Step::Failed(Notice::error("That feed has no pending key change.")),
        Outcome::NewKeyTrusted(Ok(name)) => {
            Step::All(vec![Step::Go(Dest::Back), Step::Done(Notice::ok(format!("{name} now uses the new key.")))])
        }
        Outcome::NewKeyTrusted(Err(text)) => Step::Failed(Notice::error(text.clone())),
        _ => Step::Stay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::MockBackend;
    use crate::model::Snapshot;

    const NEW_FEED: &str = "https://github.com/example/monero-tools-feed";

    /// Runs effects and returns each outcome's step with the device's state after them.
    fn run(mock: &mut MockBackend, effects: Vec<Effect>) -> (Vec<Step>, Snapshot) {
        let outcomes: Vec<Outcome> = effects.into_iter().flat_map(|effect| execute_effect(mock, effect)).collect();
        let steps = outcomes.iter().map(apply).collect();
        (steps, load_snapshot(mock).expect("the mock answers"))
    }

    fn feed<'a>(snapshot: &'a Snapshot, name: &str) -> &'a Feed {
        snapshot.feeds.iter().find(|feed| feed.name == name).unwrap_or_else(|| panic!("no source {name}"))
    }

    #[test]
    fn adding_a_source_probes_then_confirms_then_trusts() {
        let mut mock = MockBackend::new();
        let before = load_snapshot(&mut mock).expect("the mock answers").feeds.len();
        assert_eq!(probe(""), Err("Enter the feed's address.".to_owned()));
        let effects = probe(&format!("  {NEW_FEED} ")).expect("an address was typed");
        assert_eq!(effects, [Effect::ProbeFeed(NEW_FEED.to_owned())]);

        let (steps, snapshot) = run(&mut mock, effects);
        let [Step::Go(Dest::ConfirmSource(probed))] = &steps[..] else { panic!("{steps:?}") };
        assert_eq!(snapshot.feeds.len(), before, "probing adds nothing");

        assert_eq!(packages(probed), [Effect::ShowDocument(DocKind::FeedPackages(probed.name.clone()))]);
        let (steps, snapshot) = run(&mut mock, trust(probed));
        let Step::All(all) = &steps[0] else { panic!("{steps:?}") };
        assert_eq!(all[0], Step::Go(Dest::Back));
        assert_eq!(all[1], Step::Done(Notice::ok(format!("Added {} and pinned its key.", probed.name))));
        assert_eq!(snapshot.feeds.len(), before + 1);
        assert_eq!(feed(&snapshot, &probed.name).fingerprint, probed.fingerprint);
    }

    #[test]
    fn a_source_added_with_a_name_is_shown_by_it() {
        let mut mock = MockBackend::new();
        let (steps, _) = run(&mut mock, probe(NEW_FEED).expect("an address"));
        let [Step::Go(Dest::ConfirmSource(probed))] = &steps[..] else { panic!("{steps:?}") };
        let mut named = probed.clone();
        named.label = "  Monero tools ".to_owned();
        assert_eq!(named.shown(), "Monero tools");
        let (steps, snapshot) = run(&mut mock, trust(&named));
        let Step::All(all) = &steps[0] else { panic!("{steps:?}") };
        assert_eq!(all[1], Step::Done(Notice::ok("Added Monero tools and pinned its key.")));
        let added = feed(&snapshot, &probed.name);
        assert_eq!((added.label.as_deref(), added.shown()), (Some("Monero tools"), "Monero tools"));
    }

    #[test]
    fn a_source_is_renamed_and_named_back() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let name = snapshot.feeds[0].name.clone();
        assert_eq!(feed(&snapshot, &name).shown(), name);

        let effects = rename(&name, " My feed ").expect("a short name");
        assert_eq!(effects, [Effect::RenameFeed { name: name.clone(), label: Some("My feed".into()) }]);
        let (steps, after) = run(&mut mock, effects);
        assert_eq!(steps[0], Step::Done(Notice::ok("It's called My feed now.")));
        assert_eq!(feed(&after, &name).shown(), "My feed");

        // A blank name, or its own, goes back to the one the device made.
        for back in ["", "   ", name.as_str()] {
            assert_eq!(rename(&name, back).unwrap(), [Effect::RenameFeed { name: name.clone(), label: None }]);
        }
        let (steps, after) = run(&mut mock, rename(&name, "").unwrap());
        assert_eq!(steps[0], Step::Done(Notice::ok(format!("It's called {name} now."))));
        assert_eq!(feed(&after, &name).label, None);

        assert!(rename(&name, &"x".repeat(MAX_LABEL + 1)).is_err());
        assert!(rename(&name, "a\tb").is_err());
        // Another source's name is refused, so two never look the same.
        let other = after.feeds[1].name.to_uppercase();
        let (steps, _) = run(&mut mock, rename(&name, &other).unwrap());
        assert!(matches!(&steps[0], Step::Failed(notice) if notice.text.contains("already called")), "{steps:?}");
    }

    #[test]
    fn an_address_with_no_feed_fails_in_the_helpers_words() {
        let mut mock = MockBackend::new();
        let (steps, _) = run(&mut mock, probe("not an address").expect("something was typed"));
        assert!(matches!(&steps[..], [Step::Failed(notice)] if !notice.text.is_empty()), "{steps:?}");
    }

    #[test]
    fn a_source_is_turned_off_and_on_by_name() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let name = snapshot.feeds.iter().find(|feed| feed.status == FeedStatus::Healthy).expect("a healthy source").name.clone();
        let (_, off) = run(&mut mock, toggle(&name));
        assert_eq!(feed(&off, &name).status, FeedStatus::Disabled);
        let (_, on) = run(&mut mock, toggle(&name));
        assert_ne!(feed(&on, &name).status, FeedStatus::Disabled);
        assert_eq!(refresh(), [Effect::RefreshFeeds]);
    }

    #[test]
    fn removing_a_source_asks_first() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let name = snapshot.feeds.last().expect("the mock has sources").name.clone();
        let question = ask_remove(&name, false);
        assert!(question.cancel_is_default);
        assert_eq!(question.title, format!("Remove {name}?"));
        assert!(question.body[0].contains("keep running"));
        let (_, after) = run(&mut mock, question.confirm.step(&snapshot).effects());
        assert!(after.feeds.iter().all(|feed| feed.name != name));
    }

    #[test]
    fn opening_a_blocked_source_leads_to_its_key_change() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let blocked = feed(&snapshot, "old-tools");
        assert_eq!(blocked.status, FeedStatus::Blocked);
        let Step::Run(effects) = open(blocked) else { panic!("a blocked source loads its key change") };
        let (steps, _) = run(&mut mock, effects);
        assert!(matches!(&steps[..], [Step::Go(Dest::KeyChange(change))] if change.feed == "old-tools"), "{steps:?}");

        let healthy = snapshot.feeds.iter().find(|feed| feed.status == FeedStatus::Healthy).expect("a healthy source");
        assert_eq!(open(healthy), Step::Go(Dest::GetApps { source: Some(healthy.name.clone()) }));

        // A source with no pending change says so.
        let (steps, _) = run(&mut mock, vec![Effect::LoadKeyChange(healthy.name.clone())]);
        assert!(matches!(&steps[..], [Step::Failed(_)]), "{steps:?}");
    }

    #[test]
    fn a_new_key_is_trusted_only_when_the_name_is_typed_exactly() {
        use crate::model::ScreenData;
        let mut mock = MockBackend::new();
        let change = mock.key_change("old-tools").expect("the mock has a changed key");
        for wrong in ["", "old-tool", "Old-Tools", " old-tools", "old-tools "] {
            assert_eq!(
                trust_new_key(&change, wrong),
                Err("That isn't the source's name. Type old-tools exactly.".to_owned()),
                "{wrong:?}"
            );
        }
        let effects = trust_new_key(&change, "old-tools").expect("the name, exactly");
        let (steps, snapshot) = run(&mut mock, effects);
        let Step::All(all) = &steps[0] else { panic!("{steps:?}") };
        assert_eq!(all[..], [Step::Go(Dest::Back), Step::Done(Notice::ok("old-tools now uses the new key."))]);
        assert_ne!(feed(&snapshot, "old-tools").status, FeedStatus::Blocked);
    }

    #[test]
    fn removing_from_the_key_change_page_leaves_it() {
        let question = ask_remove("old-tools", true);
        let step = question.confirm.step(&Snapshot::default());
        assert_eq!(step, Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::RemoveFeed("old-tools".into())])]));
    }
}
