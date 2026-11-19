//! The Web tab: which sites are online and where, and everything around a web address.
//!
//! - [`hosting`]: one site's draft (home network, Tor, internet, its address and other names)
//!   and what Save does.
//! - [`public`]: the reachability check, what its result means, CGNAT, and the confirmation
//!   before a site goes on the internet.
//! - [`providers`]: connecting the companies web addresses were bought from.
//! - [`domain`]: the certificate settings and how local sites are named.
//! - [`certs`]: following a certificate, and the local CA.
//! - [`onion`]: the node's onion address and each site's Tor switch.
//!
//! Safety rules: exposures are independent flags; turning anything off never needs a
//! confirmation; the internet is only turned on after the checks and after the owner confirmed on
//! a page showing the address, with the answer starting on Cancel.

pub mod certs;
pub mod domain;
pub mod hosting;
pub mod onion;
pub mod providers;
pub mod public;

use super::{Confirm, Dest, Notice, Question, Step};
use crate::effect::ExposureAfter;
use crate::model::Snapshot;
use crate::Outcome;

/// Taking every site offline asks first. `None` when no site is online anywhere.
pub fn ask_all_offline(snapshot: &Snapshot) -> Option<Question> {
    if snapshot.active_sites().is_empty() {
        return None;
    }
    Some(Question::careful(
        "Take all sites offline?",
        vec![
            "Home network, Tor and internet addresses all stop, for every site.".to_owned(),
            "Records of your home's address can stay in public DNS history after this.".to_owned(),
        ],
        "Take offline",
        Confirm::AllSitesOffline,
    ))
}

/// What a result about a site's exposure, its names or its reachability means, on whichever page
/// it arrives.
///
/// - The names were refused: `Step::Failed`, and the Hosting page stays as it was, so the owner
///   can fix the name. Saved with nothing else to do: the Hosting page is left. Saved with more
///   to do: those effects, which is how a refused name never reaches the checks.
/// - An exposure that was set says so, and leaves the Hosting page when Save asked for that.
///   One that went public leads to `Dest::WentPublic`.
/// - A reachability result leads to the page for what it found (`Dest::Reachability`).
pub fn apply(outcome: &Outcome) -> Step {
    match outcome {
        Outcome::PublicHostSet { result: Err(text), .. } => Step::Failed(Notice::error(format!("Not saved: {text}"))),
        Outcome::PublicHostSet { site, result: Ok(()), then } if then.is_empty() => saved_and_left(site),
        Outcome::PublicHostSet { result: Ok(()), then, .. } => Step::Run(then.clone()),
        Outcome::ExposureSet { result: Err(error), .. } => Step::Failed(Notice::error(format!("Not changed: {error}"))),
        Outcome::ExposureSet { site, after: ExposureAfter::Back, .. } => saved_and_left(site),
        Outcome::ExposureSet { after: ExposureAfter::Stay, .. } => Step::Done(Notice::ok("Saved.")),
        Outcome::ExposureSet { site, after: ExposureAfter::WentPublic, .. } => Step::Go(Dest::WentPublic { site: site.clone() }),
        Outcome::Reachability { site, going_public, result } => {
            Step::Go(Dest::Reachability(public::Found::of(site, *going_public, result)))
        }
        _ => Step::Stay,
    }
}

fn saved_and_left(site: &str) -> Step {
    Step::All(vec![Step::Go(Dest::LeaveHosting { site: site.to_owned() }), Step::Done(Notice::ok("Saved."))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::MockBackend;
    use nodo_api::Exposure;

    #[test]
    fn taking_every_site_offline_asks_first_and_only_when_one_is_online() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let question = ask_all_offline(&snapshot).expect("sites are online");
        assert!(question.cancel_is_default);
        assert_eq!(question.title, "Take all sites offline?");
        assert_eq!(question.confirm_label, "Take offline");
        assert!(question.body[1].contains("DNS history"));
        let mut steps = Vec::new();
        for effect in question.confirm.step(&snapshot).effects() {
            steps.extend(execute_effect(&mut mock, effect).iter().map(apply).filter(|step| *step != Step::Stay));
        }
        assert!(!steps.is_empty() && steps.iter().all(|step| *step == Step::Done(Notice::ok("Saved."))), "{steps:?}");
        let after = load_snapshot(&mut mock).expect("the mock answers");
        assert!(after.web.sites.iter().all(|site| site.exposure == Exposure::default()));
        assert_eq!(ask_all_offline(&after), None, "nothing is online, so there is nothing to ask");
    }

    #[test]
    fn an_outcome_of_another_feature_changes_nothing() {
        assert_eq!(apply(&Outcome::Info("hello".into())), Step::Stay);
        assert_eq!(apply(&Outcome::ProviderRemoved(Ok(()))), Step::Stay);
    }
}
