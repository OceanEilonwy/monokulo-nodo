//! Putting a site on the internet: the reachability check, what its result means, the guidance
//! when the home has no public address of its own (CGNAT), and the one confirmation.
//!
//! The internet is only turned on after the checks and after the owner confirmed on a page that
//! shows the address, with the answer starting on Cancel. The helper wants that address quoted
//! back (`nodo_api::PublicConfirmation`), and checks it again.

use nodo_api::Exposure;

use super::super::{Dest, Step};
use super::certs::Certificate;
use crate::effect::ExposureAfter;
use crate::model::{Check, CheckState, DocKind, PublicAccess, PublicPlan, Reachability, RouterHelp, Snapshot};
use crate::Effect;

/// Runs the checks for a site. `going_public` when it is a step towards the internet, not just a
/// test.
pub fn check(site: &str, going_public: bool) -> Vec<Effect> {
    vec![Effect::CheckReachability { site: site.to_owned(), going_public }]
}

/// Tor on, every other choice as it is. Tor is the fallback that never reveals the home address.
/// Nothing when there is no such site.
pub fn also_on_tor(snapshot: &Snapshot, site: &str) -> Vec<Effect> {
    let Some(entry) = snapshot.web.sites.iter().find(|entry| entry.package == site) else {
        return Vec::new();
    };
    vec![Effect::SetExposure {
        site: site.to_owned(),
        exposure: Exposure { onion: true, ..entry.exposure },
        after: ExposureAfter::Stay,
        confirmation: None,
    }]
}

/// What a reachability check found, as the page to show for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// The home shares its internet address, so no port can be forwarded.
    Cgnat(Cgnat),
    /// A check failed, or something could not be verified, or it was only a test.
    Problems(PublicCheck),
    /// Every check passed on the way to public: the confirmation.
    Clear(PublicConfirm),
}

impl Found {
    /// Unknown results, such as untested ports, are shown before confirming, never skipped: only
    /// checks that all passed lead straight to the confirmation.
    pub fn of(site: &str, going_public: bool, result: &Reachability) -> Self {
        match result {
            Reachability::Cgnat { wan_address, wan_range, internet_address } => Found::Cgnat(Cgnat {
                site: site.to_owned(),
                wan_address: wan_address.clone(),
                wan_range: wan_range.clone(),
                internet_address: internet_address.clone(),
                going_public,
            }),
            Reachability::Checked { plan: Some(plan), checks, .. }
                if going_public && checks.iter().all(|check| check.state == CheckState::Passed) =>
            {
                Found::Clear(PublicConfirm::new(site, plan.clone()))
            }
            Reachability::Checked { checks, router, plan, manual } => Found::Problems(PublicCheck {
                site: site.to_owned(),
                checks: checks.clone(),
                router: router.clone(),
                plan: plan.clone(),
                going_public,
                manual: manual.clone(),
            }),
        }
    }
}

/// The checks that found a problem, or could not verify something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicCheck {
    pub site: String,
    pub checks: Vec<Check>,
    pub router: Option<RouterHelp>,
    pub plan: Option<PublicPlan>,
    /// True when this check is a step towards going public, not just a test from the overview.
    pub going_public: bool,
    pub manual: Vec<String>,
}

impl PublicCheck {
    pub fn check_again(&self) -> Vec<Effect> {
        check(&self.site, self.going_public)
    }

    /// Nothing failed, but something could not be verified: the owner may go on, having seen it.
    pub fn can_continue(&self) -> bool {
        self.going_public && self.plan.is_some() && !self.checks.iter().any(|check| check.state == CheckState::Failed)
    }

    /// Goes on to the confirmation despite what could not be verified. Nothing when a check
    /// failed.
    pub fn continue_anyway(&self) -> Step {
        match &self.plan {
            Some(plan) if self.can_continue() => Step::Go(Dest::PublicConfirm(PublicConfirm::new(&self.site, plan.clone()))),
            _ => Step::Stay,
        }
    }

    /// How to forward ports on common routers.
    pub fn router_guides(&self) -> Vec<Effect> {
        vec![Effect::ShowDocument(DocKind::RouterGuides)]
    }

    pub fn use_tor(&self, snapshot: &Snapshot) -> Vec<Effect> {
        also_on_tor(snapshot, &self.site)
    }
}

/// The ways on when the home has no public address of its own, in the order they are offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgnatWay {
    /// Use Tor instead, which needs no public address.
    Tor,
    /// Ask the internet provider for a public address.
    PublicIp,
    Ipv6,
    /// A relay or tunnel run by someone else.
    Relay,
}

impl CgnatWay {
    pub const ALL: [CgnatWay; 4] = [CgnatWay::Tor, CgnatWay::PublicIp, CgnatWay::Ipv6, CgnatWay::Relay];

    /// The way, as the owner is offered it.
    pub fn label(self) -> &'static str {
        match self {
            CgnatWay::Tor => "Use Tor instead",
            CgnatWay::PublicIp => "Ask your provider for a public address",
            CgnatWay::Ipv6 => "Use IPv6, if your provider has it",
            CgnatWay::Relay => "Use a relay that forwards visitors",
        }
    }
}

/// The home shares its internet address with others (CGNAT).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cgnat {
    pub site: String,
    pub wan_address: String,
    pub wan_range: String,
    pub internet_address: String,
    pub going_public: bool,
}

impl Cgnat {
    pub fn check_again(&self) -> Vec<Effect> {
        check(&self.site, self.going_public)
    }

    pub fn use_tor(&self, snapshot: &Snapshot) -> Vec<Effect> {
        also_on_tor(snapshot, &self.site)
    }

    /// Takes one of the ways on: Tor is turned on, the others are explained.
    pub fn choose(&self, way: CgnatWay, snapshot: &Snapshot) -> Vec<Effect> {
        match way {
            CgnatWay::Tor => self.use_tor(snapshot),
            CgnatWay::PublicIp => vec![Effect::ShowDocument(DocKind::PublicIpFromIsp)],
            CgnatWay::Ipv6 => vec![Effect::ShowDocument(DocKind::Ipv6)],
            CgnatWay::Relay => vec![Effect::ShowDocument(DocKind::RelayTunnel)],
        }
    }
}

/// The confirmation before a site goes public: the address it will have and what changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicConfirm {
    pub site: String,
    pub plan: PublicPlan,
    pub access: PublicAccess,
}

impl PublicConfirm {
    pub fn new(site: &str, plan: PublicPlan) -> Self {
        Self { site: site.to_owned(), plan, access: PublicAccess { require_password: false } }
    }

    /// The answer starts on Cancel, so a stray Enter or tap does not put a site online.
    pub fn cancel_is_default(&self) -> bool {
        true
    }

    /// Puts the site online. `typed` is the address quoted back: what the owner typed, or, for a
    /// frontend whose confirmation is a button under the address, the address that button was
    /// shown with. It must be this confirmation's host exactly; the error belongs beside the box.
    ///
    /// The protections go first, so the site is never public without them, and the batch stops
    /// there when they fail (`exec::run_batch`). Nothing when the site is gone.
    pub fn confirm(&self, typed: &str, snapshot: &Snapshot) -> Result<Vec<Effect>, String> {
        if typed != self.plan.host {
            return Err(format!("That isn't the address. Type {} exactly.", self.plan.host));
        }
        let Some(site) = snapshot.web.sites.iter().find(|site| site.package == self.site) else {
            return Ok(Vec::new());
        };
        Ok(vec![
            Effect::SetPublicAccess { site: self.site.clone(), access: self.access.clone() },
            Effect::SetExposure {
                site: self.site.clone(),
                exposure: Exposure { public: true, ..site.exposure },
                after: ExposureAfter::WentPublic,
                confirmation: Some(self.plan.host.clone()),
            },
        ])
    }

    /// The page that follows the secure connection of the address confirmed, shown once the site
    /// is public (`Dest::WentPublic`).
    pub fn certificate(&self) -> Certificate {
        Certificate { site: self.site.clone(), host: self.plan.host.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::flows::web;
    use crate::flows::Notice;
    use crate::mock::{reachability_checks, MockBackend};
    use crate::Outcome;

    fn start() -> (MockBackend, Snapshot) {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        (mock, snapshot)
    }

    fn found(mock: &mut MockBackend, site: &str, going_public: bool) -> Found {
        let outcomes = execute_effect(mock, check(site, going_public).remove(0));
        let [outcome] = &outcomes[..] else { panic!("{outcomes:?}") };
        match web::apply(outcome) {
            Step::Go(Dest::Reachability(found)) => found,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn checks_that_all_pass_on_the_way_to_public_lead_to_the_confirmation() {
        let (mut mock, _) = start();
        let Found::Clear(confirm) = found(&mut mock, "moneropay", true) else { panic!("clear") };
        assert_eq!(confirm.site, "moneropay");
        assert_eq!(confirm.plan.host, "moneropay.example.com");
        assert!(confirm.cancel_is_default());
        assert!(!confirm.access.require_password);
        // The same result as a test only is shown, never confirmed.
        let Found::Problems(shown) = found(&mut mock, "moneropay", false) else { panic!("shown") };
        assert!(!shown.can_continue());
        assert_eq!(shown.continue_anyway(), Step::Stay);
    }

    #[test]
    fn a_failed_check_is_shown_and_cannot_be_passed() {
        let (mut mock, snapshot) = start();
        // The mock's shop fails its ports the first time and passes on checking again.
        let Found::Problems(problems) = found(&mut mock, "shop", true) else { panic!("problems") };
        assert!(problems.router.is_some() && problems.plan.is_none());
        assert!(!problems.can_continue());
        assert_eq!(problems.continue_anyway(), Step::Stay);
        assert_eq!(problems.router_guides(), [Effect::ShowDocument(DocKind::RouterGuides)]);
        assert_eq!(problems.check_again(), [Effect::CheckReachability { site: "shop".into(), going_public: true }]);
        assert!(matches!(found(&mut mock, "shop", true), Found::Clear(_)));
        // Tor instead: every other choice stays.
        let [Effect::SetExposure { exposure, after: ExposureAfter::Stay, confirmation: None, .. }] = &problems.use_tor(&snapshot)[..] else {
            panic!("one exposure")
        };
        assert_eq!(*exposure, Exposure { local: true, onion: true, public: true });
        assert!(also_on_tor(&snapshot, "gone").is_empty());
    }

    #[test]
    fn what_could_not_be_verified_is_shown_and_the_owner_may_go_on() {
        let Reachability::Checked { mut checks, router, plan, .. } = reachability_checks("shop.example.com", true) else {
            panic!("checked")
        };
        checks[4].state = CheckState::Unknown;
        let result = Reachability::Checked { checks, router, plan, manual: vec!["Ask a friend to open it.".into()] };
        let Found::Problems(unverified) = Found::of("shop", true, &result) else { panic!("an unknown is never skipped") };
        assert!(unverified.can_continue());
        let Step::Go(Dest::PublicConfirm(confirm)) = unverified.continue_anyway() else { panic!("the confirmation") };
        assert_eq!(confirm.plan.host, "shop.example.com");
        assert_eq!(unverified.manual.len(), 1);
    }

    #[test]
    fn a_shared_address_leads_to_the_cgnat_guidance() {
        let (mut mock, snapshot) = start();
        let Found::Cgnat(cgnat) = found(&mut mock, "monokulo-pay", true) else { panic!("cgnat") };
        assert_eq!(cgnat.wan_range, "100.64.0.0/10");
        assert!(cgnat.going_public);
        assert_eq!(cgnat.check_again(), [Effect::CheckReachability { site: "monokulo-pay".into(), going_public: true }]);
        assert_eq!(CgnatWay::ALL[0], CgnatWay::Tor, "the way that always works is first");
        assert_eq!(cgnat.choose(CgnatWay::PublicIp, &snapshot), [Effect::ShowDocument(DocKind::PublicIpFromIsp)]);
        assert_eq!(cgnat.choose(CgnatWay::Ipv6, &snapshot), [Effect::ShowDocument(DocKind::Ipv6)]);
        assert_eq!(cgnat.choose(CgnatWay::Relay, &snapshot), [Effect::ShowDocument(DocKind::RelayTunnel)]);
        // monokulo-pay is on Tor already, so the effect keeps what is on.
        let effects = cgnat.choose(CgnatWay::Tor, &snapshot);
        assert_eq!(effects, cgnat.use_tor(&snapshot));
        let outcomes = execute_effect(&mut mock, effects.into_iter().next().expect("one effect"));
        assert_eq!(web::apply(&outcomes[0]), Step::Done(Notice::ok("Saved.")));
    }

    #[test]
    fn the_site_goes_public_only_with_its_address_quoted_back() {
        let (mut mock, snapshot) = start();
        let Found::Clear(confirm) = found(&mut mock, "moneropay", true) else { panic!("clear") };
        for wrong in ["", "moneropay.example.co", "Moneropay.example.com", " moneropay.example.com", "https://moneropay.example.com"] {
            assert_eq!(
                confirm.confirm(wrong, &snapshot),
                Err("That isn't the address. Type moneropay.example.com exactly.".to_owned()),
                "{wrong:?}"
            );
        }
        let effects = confirm.confirm("moneropay.example.com", &snapshot).expect("the address, exactly");
        let [Effect::SetPublicAccess { .. }, Effect::SetExposure { exposure, after: ExposureAfter::WentPublic, confirmation, .. }] = &effects[..]
        else {
            panic!("{effects:?}")
        };
        assert_eq!(*exposure, Exposure { local: true, onion: false, public: true }, "the other choices stay as saved");
        assert_eq!(confirmation.as_deref(), Some("moneropay.example.com"));

        let mut steps = Vec::new();
        for effect in effects {
            steps.extend(execute_effect(&mut mock, effect).iter().map(web::apply));
        }
        assert!(steps.contains(&Step::Go(Dest::WentPublic)), "{steps:?}");
        let after = load_snapshot(&mut mock).expect("the mock answers");
        assert!(after.web.sites.iter().find(|s| s.package == "moneropay").expect("the site").exposure.public);
        assert_eq!(confirm.certificate(), Certificate { site: "moneropay".into(), host: "moneropay.example.com".into() });

        // A site that is gone: nothing to do.
        let gone = PublicConfirm::new("gone", confirm.plan.clone());
        assert_eq!(gone.confirm("moneropay.example.com", &snapshot), Ok(Vec::new()));
    }

    #[test]
    fn the_helper_refuses_going_public_without_the_address() {
        let (mut mock, snapshot) = start();
        let site = snapshot.web.sites.iter().find(|s| s.package == "moneropay").expect("the site");
        let effect = Effect::SetExposure {
            site: "moneropay".into(),
            exposure: Exposure { public: true, ..site.exposure },
            after: ExposureAfter::WentPublic,
            confirmation: None,
        };
        let outcomes = execute_effect(&mut mock, effect);
        assert!(matches!(&outcomes[0], Outcome::ExposureSet { result: Err(_), .. }));
        let Step::Failed(notice) = web::apply(&outcomes[0]) else { panic!("refused") };
        assert!(notice.text.starts_with("Not changed: "), "{}", notice.text);
    }
}
