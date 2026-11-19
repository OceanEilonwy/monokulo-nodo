//! Scenarios every backend must pass the same way, so the mock the tests use cannot drift from the
//! device. nodo-tui runs them against `MockBackend`, and nodo-helper runs them against
//! `HelperBackend` talking to its real request handler over a socket (see
//! `nodo-helper/src/tui_contract_tests.rs`). Each one panics with what differed.
//!
//! They check behaviour against [`Capabilities`], and a test holds the mock's default capabilities
//! equal to `HelperBackend::CAPABILITIES`, so both backends do the same here.

use crate::model::*;

/// The longest a backend with stepped installs may take, in polls, before the install is over.
const MOST_POLLS: usize = 1000;

/// Installs `id`, which must reach a review, and returns how the install ended. Nothing installs
/// without the review's approval; the install answers when it is over unless the backend reports
/// progress; there is a cancel only where the backend can cancel.
pub fn install(backend: &mut impl ScreenData, id: &PackageId) -> InstallSnapshot {
    let caps = backend.capabilities();
    let InstallGate::Review(review) = backend.install_gate(id) else {
        panic!("{} should reach a review", id.name);
    };
    assert!(review.approval.is_some(), "a review carries the approval an install quotes");
    assert!(backend.start_install(id, None, None).is_err(), "an install without the review's approval is refused");

    let mut snapshot = backend.start_install(id, review.approval.as_ref(), None).expect("the reviewed install starts");
    assert_eq!(&snapshot.id, id);
    if caps.install_progress {
        assert_eq!(snapshot.outcome, InstallOutcome::InProgress, "with progress the install answers while it runs");
        assert_eq!(snapshot.can_cancel, caps.cancel_install, "a running install offers a cancel exactly where the backend can cancel");
        for _ in 0..MOST_POLLS {
            if snapshot.outcome != InstallOutcome::InProgress {
                break;
            }
            snapshot = backend.poll_install().expect("an install with progress keeps reporting");
        }
    } else {
        assert_ne!(snapshot.outcome, InstallOutcome::InProgress, "without progress the install answers when it is over");
        assert!(!snapshot.can_cancel, "a finished install has nothing to cancel");
        assert_eq!(backend.poll_install(), None, "without progress there is nothing to poll");
    }
    assert_ne!(snapshot.outcome, InstallOutcome::InProgress, "the install ended");
    if !caps.cancel_install {
        assert_eq!(backend.cancel_install(), None, "a backend that cannot cancel never reports a cancel");
    }
    snapshot
}

/// An update refused before anything changed comes back as not applied with the device's reason,
/// and with no rollback to explain: nothing was put back, nothing is held, and the same update is
/// still offered. The caller makes the update of `name` one the backend refuses.
pub fn refused_update_changes_nothing(backend: &mut impl ScreenData, name: &str) {
    let before = backend.updates().into_iter().find(|u| u.name == name).expect("the update is offered");
    let report = backend.apply_updates(&[name.to_owned()], &[]);
    assert_eq!(report.failure, None, "a refusal is not a rollback: {report:?}");
    let result = report.results.iter().find(|r| r.name == name).expect("the refused update has a result");
    assert!(!result.applied, "{result:?}");
    assert!(!result.note.trim().is_empty(), "a refusal says why: {result:?}");
    assert_eq!(result.version, format!("still on {}", before.from), "{result:?}");
    let after = backend.updates().into_iter().find(|u| u.name == name).expect("the update is still offered");
    assert!(!after.held, "a refusal holds nothing: {after:?}");
    assert_eq!(after.to, before.to);
}

/// Switching or removing a source that does not exist says so, instead of looking as if it worked.
pub fn missing_sources_fail(backend: &mut impl ScreenData) {
    let missing = "no-such-source";
    assert!(!backend.feeds().iter().any(|feed| feed.name == missing));
    assert!(backend.toggle_feed(missing).is_err(), "switching a source that is not there fails");
    assert!(backend.remove_feed(missing).is_err(), "removing a source that is not there fails");
}

/// What the backend says it can and cannot do is what it does: what is not built fails with a
/// reason rather than pretending, and what is claimed works.
pub fn features_match_capabilities(backend: &mut impl ScreenData, site: &str) {
    let caps = backend.capabilities();
    let copied = backend.copy_report("contract");
    assert_eq!(copied.is_ok(), caps.copy_report, "copying a report works exactly where it is claimed: {copied:?}");
    let password = backend.set_public_access(site, &PublicAccess { require_password: true });
    assert_eq!(password.is_ok(), caps.site_password, "a site password works exactly where it is claimed: {password:?}");
    assert_eq!(backend.set_public_access(site, &PublicAccess { require_password: false }), Ok(()));
}

/// A source that cannot be read does not fail Check now: the check goes on with the others, the
/// source is marked unreachable with the backend's reason, and the summary names it. The caller
/// makes `unreadable` a source that cannot be read first.
pub fn unreadable_source_is_named(backend: &mut impl ScreenData, unreadable: &str) {
    let report = backend.check_updates();
    assert_eq!(report.apps, Ok(()), "an unreadable source does not fail the check of the others");
    let feeds = backend.feeds();
    let feed = feeds.iter().find(|feed| feed.name == unreadable).expect("the unreadable source is still listed");
    let FeedStatus::Unreachable(problem) = &feed.status else {
        panic!("{unreadable} is marked unreachable after the check, not {:?}", feed.status);
    };
    assert!(!problem.trim().is_empty(), "the source carries why it could not be read");
    let summary = report.summary(backend.updates().len(), &feeds);
    assert!(summary.trouble, "{}", summary.text);
    assert!(summary.text.contains(&format!("Could not read {unreadable}: ")), "{}", summary.text);
    if !feed.installed_from.is_empty() {
        assert!(!summary.text.contains("Up to date"), "apps from an unread source are not up to date: {}", summary.text);
    }
}

/// A site's other names: saved and read back, removed, and refused for the same reasons in the
/// same words. `site` is not public and `domain` is the one connected domain; `other_host` is
/// another site's address.
pub fn site_aliases(backend: &mut impl ScreenData, site: &str, domain: &str, other_host: &str) {
    use nodo_api::{AliasMode, PublicAlias};
    let alias = |host: &str, mode| PublicAlias { host: host.to_owned(), mode };
    let primary = format!("www.{domain}");
    let saved = |backend: &mut dyn FnMut() -> WebOverview| {
        backend().sites.into_iter().find(|s| s.package == site).map(|s| s.public_aliases).expect("the site is listed")
    };
    let names = vec![alias(domain, AliasMode::Redirect), alias(&format!("pay.{domain}"), AliasMode::Serve)];
    assert_eq!(backend.set_public_host(site, &primary, &names, &[]), Ok(()), "an apex redirect and a serving name are saved");
    assert_eq!(saved(&mut || backend.web()), names, "and read back as saved");

    let refusals = [
        (vec![alias("elsewhere.invalid", AliasMode::Redirect)], "not under a domain of a connected provider"),
        (vec![alias(domain, AliasMode::Redirect), alias(domain, AliasMode::Serve)], "listed twice"),
        (vec![alias(&primary, AliasMode::Serve)], "already the site's main address"),
        (vec![alias(other_host, AliasMode::Redirect)], "is already an address of"),
        (vec![alias("Shop.Example", AliasMode::Redirect)], "not a full lowercase domain name"),
    ];
    for (aliases, why) in refusals {
        let result = backend.set_public_host(site, &primary, &aliases, &[]);
        assert!(result.as_ref().is_err_and(|text| text.contains(why)), "{aliases:?} is refused because {why}: {result:?}");
        assert_eq!(saved(&mut || backend.web()), names, "a refusal leaves the saved names");
    }

    assert_eq!(backend.set_public_host(site, &primary, &[], &[]), Ok(()), "the names are removed");
    assert_eq!(saved(&mut || backend.web()), Vec::new());
}

/// A public site's other names: a new one goes on only when it is confirmed, and is refused in
/// the same words otherwise; whether a name redirects changes without a confirmation. `site` is
/// not public and has no other names; `domain` is the one connected domain.
pub fn public_site_gains_confirmed_names(backend: &mut (impl ScreenData + nodo_api::Backend), site: &str, domain: &str) {
    use nodo_api::{AliasMode, Exposure, PublicAlias, PublicConfirmation, Request};
    let alias = |host: &str, mode| PublicAlias { host: host.to_owned(), mode };
    let primary = format!("www.{domain}");
    let pay = format!("pay.{domain}");
    let saved = |backend: &mut dyn FnMut() -> WebOverview| {
        backend().sites.into_iter().find(|s| s.package == site).map(|s| s.public_aliases).expect("the site is listed")
    };
    let apex = vec![alias(domain, AliasMode::Redirect)];
    assert_eq!(backend.set_public_host(site, &primary, &apex, &[]), Ok(()), "before going public no name needs confirming");
    let going_public = Request::SetExposure {
        site: backend.site_id(site),
        exposure: Exposure { local: false, onion: false, public: true },
        confirmation: Some(PublicConfirmation { typed_host: primary.clone() }),
    };
    backend.call(going_public).expect("the site goes public");

    let both = vec![apex[0].clone(), alias(&pay, AliasMode::Serve)];
    let wanted = format!("the site is public, so {pay} would go on the internet at once: confirm adding it");
    for confirmed in [Vec::new(), vec![domain.to_owned()]] {
        assert_eq!(backend.set_public_host(site, &primary, &both, &confirmed), Err(wanted.clone()), "confirmed: {confirmed:?}");
        assert_eq!(saved(&mut || backend.web()), apex, "a refusal leaves the saved names");
    }
    assert_eq!(backend.set_public_host(site, &primary, &both, std::slice::from_ref(&pay)), Ok(()), "the confirmed name is added");
    assert_eq!(saved(&mut || backend.web()), both);

    let served = vec![alias(domain, AliasMode::Serve), both[1].clone()];
    assert_eq!(backend.set_public_host(site, &primary, &served, &[]), Ok(()), "a name that is public already changes how it answers");
    assert_eq!(saved(&mut || backend.web()), served);
    assert_eq!(backend.set_public_host(site, &primary, &[], &[]), Ok(()), "and names are removed while public");
}
