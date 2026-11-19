//! Onion (Tor) addresses: the node's, and each site's switch.
//!
//! The node's onion address only ever serves the restricted RPC. A site that works from a single
//! address cannot add an onion one beside another. The helper checks both again.

use nodo_api::{OnionStatus, SiteId};

use super::super::{Dest, Notice, Step};
use crate::{Effect, Outcome};

/// A row of the page, for drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Node,
    /// An index into `OnionStatus::sites`. Actions take the site's id, not this index.
    Site(usize),
}

/// The onion page: what Tor reports, and what is being asked of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Onion {
    pub status: Option<OnionStatus>,
    /// Why the last switch was refused or the status could not be read.
    pub error: Option<String>,
    /// The helper is asked; nothing else is taken until it answers.
    pub busy: bool,
}

impl Default for Onion {
    fn default() -> Self {
        Self::new()
    }
}

impl Onion {
    /// The page as it opens, waiting for the status. A frontend opening it runs [`Onion::load`].
    pub fn new() -> Self {
        Self { status: None, error: None, busy: true }
    }

    pub fn load() -> Vec<Effect> {
        vec![Effect::LoadOnion]
    }

    /// The rows in order: the node, when one is installed, then the sites.
    pub fn rows(&self) -> Vec<Row> {
        let Some(status) = &self.status else { return Vec::new() };
        let node = status.node.iter().map(|_| Row::Node);
        node.chain((0..status.sites.len()).map(Row::Site)).collect()
    }

    /// Asks Tor again.
    pub fn reload(&mut self) -> Vec<Effect> {
        if self.busy {
            return Vec::new();
        }
        self.busy = true;
        self.error = None;
        Self::load()
    }

    /// Leaving reads the device's state again, since the switches change what the sites show.
    pub fn leave(&self) -> Step {
        Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])])
    }

    /// The settings of the app that provides the node, where its ports are chosen.
    pub fn node_settings(&self) -> Vec<Effect> {
        match self.status.as_ref().and_then(|status| status.node.as_ref()) {
            Some(node) if !self.busy => vec![Effect::OpenSettings(node.provider.clone())],
            _ => Vec::new(),
        }
    }

    /// Turns the node's onion address on or off. A node that does not say which port is its
    /// restricted RPC cannot go on Tor, and no setting changes that; `error` says so.
    pub fn toggle_node(&mut self) -> Vec<Effect> {
        if self.busy {
            return Vec::new();
        }
        let Some(node) = self.status.as_ref().and_then(|status| status.node.as_ref()) else {
            return Vec::new();
        };
        if !node.enabled && !node.restricted_rpc {
            self.error = Some(format!(
                "{} does not say which port serves its restricted RPC, so it cannot have \
                 an onion address. The onion address only ever serves the restricted RPC.",
                node.provider
            ));
            return Vec::new();
        }
        let enable = !node.enabled;
        self.busy = true;
        self.error = None;
        vec![Effect::SetNodeOnion(enable)]
    }

    /// Turns one site's onion address on or off, every other choice as it is. Turning it on is
    /// refused, with the reason in `error`, for a locked site and for a site that works from a
    /// single address and already has another.
    pub fn toggle_site(&mut self, id: &SiteId) -> Vec<Effect> {
        if self.busy {
            return Vec::new();
        }
        let Some(site) = self.status.as_ref().and_then(|status| status.sites.iter().find(|site| site.id == *id)) else {
            return Vec::new();
        };
        if site.locked && !site.exposure.onion {
            self.error = Some(format!("{} is locked. Unlock the device first.", site.id.package));
            return Vec::new();
        }
        let mut exposure = site.exposure;
        exposure.onion = !exposure.onion;
        if exposure.onion && !site.multi_address && (exposure.local || exposure.public) {
            self.error =
                Some(format!("{} works from a single address. Turn its other address off under Web first.", site.id.package));
            return Vec::new();
        }
        let effect = Effect::SetSiteExposure { site: site.id.clone(), exposure };
        self.busy = true;
        self.error = None;
        vec![effect]
    }

    /// What a result means. The status that follows every switch ends the wait.
    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::OnionLoaded(result) => {
                self.busy = false;
                match result {
                    Ok(status) => self.status = Some(status.clone()),
                    Err(error) => self.error = Some(error.clone()),
                }
                Step::Stay
            }
            Outcome::OnionChanged(Ok(())) => Step::Done(Notice::ok("Onion exposure changed.")),
            Outcome::OnionChanged(Err(error)) => {
                self.error = Some(error.clone());
                Step::Stay
            }
            _ => Step::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::execute_effect;
    use crate::mock::MockBackend;

    /// Runs effects and hands every outcome to the page.
    fn run(mock: &mut MockBackend, page: &mut Onion, effects: Vec<Effect>) -> Vec<Step> {
        let mut steps = Vec::new();
        for effect in effects {
            for outcome in execute_effect(mock, effect) {
                steps.push(page.apply(&outcome));
            }
        }
        steps.into_iter().filter(|step| *step != Step::Stay).collect()
    }

    fn open(mock: &mut MockBackend) -> Onion {
        let mut page = Onion::new();
        assert!(page.busy && page.rows().is_empty());
        run(mock, &mut page, Onion::load());
        assert!(!page.busy);
        page
    }

    fn id(package: &str) -> SiteId {
        SiteId { package: package.to_owned(), site: "web".to_owned() }
    }

    fn onion_of(page: &Onion, package: &str) -> bool {
        let status = page.status.as_ref().expect("a status");
        status.sites.iter().find(|site| site.id.package == package).expect("the site").exposure.onion
    }

    #[test]
    fn the_node_comes_first_and_then_every_site() {
        let mut mock = MockBackend::new();
        let page = open(&mut mock);
        let sites = page.status.as_ref().expect("a status").sites.len();
        assert_eq!(page.rows()[0], Row::Node);
        assert_eq!(page.rows().len(), sites + 1);
        let mut without = page.clone();
        without.status.as_mut().expect("a status").node = None;
        assert_eq!(without.rows()[0], Row::Site(0));
        assert!(without.node_settings().is_empty() && without.toggle_node().is_empty());
        let provider = page.status.as_ref().and_then(|s| s.node.as_ref()).expect("a node").provider.clone();
        assert_eq!(page.node_settings(), [Effect::OpenSettings(provider)]);
        assert_eq!(page.leave(), Step::All(vec![Step::Go(Dest::Back), Step::Run(vec![Effect::Refresh])]));
    }

    #[test]
    fn the_nodes_address_is_switched_and_the_status_read_again() {
        let mut mock = MockBackend::new();
        let mut page = open(&mut mock);
        let was = mock.node_onion.enabled;
        let effects = page.toggle_node();
        assert_eq!(effects, [Effect::SetNodeOnion(!was)]);
        assert!(page.busy);
        assert!(page.toggle_node().is_empty() && page.reload().is_empty(), "nothing more is asked while it waits");
        assert!(page.toggle_site(&id("shop")).is_empty() && page.node_settings().is_empty());
        let steps = run(&mut mock, &mut page, effects);
        assert_eq!(steps, [Step::Done(Notice::ok("Onion exposure changed."))]);
        assert!(!page.busy);
        assert_eq!(page.status.as_ref().and_then(|s| s.node.as_ref()).map(|n| n.enabled), Some(!was));
    }

    #[test]
    fn a_node_without_a_restricted_port_cannot_go_on_tor() {
        let mut mock = MockBackend::new();
        mock.node_onion.enabled = false;
        mock.node_onion.restricted_rpc = false;
        let mut page = open(&mut mock);
        assert!(page.toggle_node().is_empty());
        let error = page.error.clone().expect("a reason");
        assert!(error.contains("restricted RPC") && error.contains("cannot have"), "{error}");
        assert!(!page.busy);
        // One that is on can always be turned off.
        mock.node_onion.enabled = true;
        let mut page = open(&mut mock);
        assert_eq!(page.toggle_node(), [Effect::SetNodeOnion(false)]);
    }

    #[test]
    fn a_site_is_switched_with_its_other_choices_kept() {
        let mut mock = MockBackend::new();
        let mut page = open(&mut mock);
        assert!(onion_of(&page, "shop"));
        let effects = page.toggle_site(&id("shop"));
        let [Effect::SetSiteExposure { site, exposure }] = &effects[..] else { panic!("{effects:?}") };
        assert_eq!(site, &id("shop"));
        assert!(!exposure.onion && exposure.local && exposure.public);
        let steps = run(&mut mock, &mut page, effects);
        assert_eq!(steps, [Step::Done(Notice::ok("Onion exposure changed."))]);
        assert!(!onion_of(&page, "shop"));
        assert!(page.toggle_site(&id("nowhere")).is_empty());
        assert_eq!(page.error, None);
    }

    #[test]
    fn a_single_address_site_and_a_locked_site_are_refused_with_the_reason() {
        let mut mock = MockBackend::new();
        let mut page = open(&mut mock);
        // moneropay works from one address and is on the home network.
        assert!(page.toggle_site(&id("moneropay")).is_empty());
        assert_eq!(page.error.as_deref(), Some("moneropay works from a single address. Turn its other address off under Web first."));
        // wallet-relay is locked and off.
        assert!(page.toggle_site(&id("wallet-relay")).is_empty());
        assert_eq!(page.error.as_deref(), Some("wallet-relay is locked. Unlock the device first."));
        assert!(!page.busy);
        // Asking again clears what was said.
        assert_eq!(page.reload(), [Effect::LoadOnion]);
        assert!(page.busy && page.error.is_none());
    }

    #[test]
    fn a_failure_is_kept_on_the_page() {
        let mut page = Onion::new();
        assert_eq!(page.apply(&Outcome::OnionLoaded(Err("Tor did not answer.".into()))), Step::Stay);
        assert!(!page.busy);
        assert_eq!(page.error.as_deref(), Some("Tor did not answer."));
        assert_eq!(page.apply(&Outcome::OnionChanged(Err("Refused.".into()))), Step::Stay);
        assert_eq!(page.error.as_deref(), Some("Refused."));
        assert_eq!(page.apply(&Outcome::Info("other".into())), Step::Stay);
    }
}
