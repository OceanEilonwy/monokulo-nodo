//! Domain providers: the companies web addresses were bought from. Connecting one gives the
//! device a key to change that account's DNS records, which is what puts a site at an address.
//!
//! Adding one has three steps: choose the company, give its key (and the domain, for a company
//! that cannot list them), and see which domains the key controls. A key is only ever sent to
//! the provider it was typed for, is stored sealed with the device passphrase, and is never shown
//! again.
//!
//! A provider lists only the domains its key may change. So that a domain the owner just allowed
//! in the provider's account shows up without knowing to ask, opening the list asks every
//! provider again ([`open_list`]); and each connection has a page of its details: which key it
//! holds (a masked hint), when its domains were last read, and the domains in the account the key
//! may not change yet, with what to switch on.

use super::super::{Confirm, Dest, Hint, Notice, Question, Step};
use crate::model::{ConnectedProvider, DocKind, Provider, Snapshot};
use crate::secret::Input;
use crate::{Effect, Outcome};

/// Where the provider pages are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderStage {
    /// The providers added, with a way to add one.
    List,
    /// Choosing the company.
    Pick,
    /// The domain and key for the chosen company.
    Key,
    /// Connected. Holds the provider's name, the domains its key controls that sites can use, the
    /// providers already holding the rest (the same account connected twice, most often), and the
    /// account's domains the key may not change yet with what to switch on.
    Done { provider: String, domains: Vec<String>, held_by: Vec<String>, blocked: Vec<(String, String)> },
    /// One connected provider: which key it holds, when its domains were last looked up, each
    /// domain and who uses it, and the domains it cannot change with the reason. Which provider
    /// is the frontend's to know (its chosen row in the list, its URL).
    Details,
}

/// A box of the key step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyField {
    /// The web address bought, only for a provider that cannot list its domains.
    Domain,
    ApiKey,
    /// The second credential, for a provider that has two.
    SecretKey,
}

/// The boxes of the key step for a company, in order. Without a company, the key alone.
pub fn fields(provider: Option<&Provider>) -> Vec<KeyField> {
    let mut order = Vec::new();
    if provider.is_some_and(|p| !p.lists_domains) {
        order.push(KeyField::Domain);
    }
    order.push(KeyField::ApiKey);
    if provider.is_some_and(|p| p.fields.len() > 1) {
        order.push(KeyField::SecretKey);
    }
    order
}

/// Reads the catalog of companies again, as the add flow opens.
pub fn load() -> Vec<Effect> {
    vec![Effect::LoadProviders]
}

/// Asks a connected provider again which domains its key controls.
pub fn refresh(id: &str) -> Vec<Effect> {
    vec![Effect::RefreshProvider(id.to_owned())]
}

/// What opening the list of connected providers does: each one that can list its domains is asked
/// again, so a domain the owner just allowed in the provider's account shows up without knowing
/// to ask. A locked device cannot open the keys, so there the list shows what was last known and
/// nothing is asked.
pub fn open_list(snapshot: &Snapshot) -> Vec<Effect> {
    if snapshot.is_locked() {
        return Vec::new();
    }
    snapshot.connected.iter().filter(|p| p.lists_domains).flat_map(|p| refresh(&p.id)).collect()
}

/// What looking at a provider's domains again found, in a sentence. A problem is an error. Domains
/// the key cannot change yet are not, but the owner looking for one needs to know it was found,
/// so they are named and the notice points at the provider's details.
pub fn refreshed_notice(provider: &ConnectedProvider) -> Notice {
    match &provider.problem {
        Some(problem) => Notice::error(problem.clone()),
        None if provider.blocked.is_empty() => Notice::ok(format!("{} lists {} domains.", provider.name, provider.domains.len())),
        None => Notice::info(format!(
            "{} lists {} domains, and {} more its key can't change yet: {}.",
            provider.name,
            provider.domains.len(),
            provider.blocked.len(),
            provider.blocked.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(", ")
        ))
        .hinting(Hint::ProviderDetails),
    }
}

/// How to make a key at that company, as a document.
pub fn key_help(provider: &Provider) -> Vec<Effect> {
    vec![Effect::ShowDocument(DocKind::ProviderKeys(provider.name.clone()))]
}

/// Removing a connected provider asks first, and says which apps still use its domains.
pub fn ask_remove(provider: &ConnectedProvider) -> Question {
    let used: Vec<String> = provider.domains.iter().flat_map(|d| d.used_by.clone()).collect();
    let mut body = vec![format!("The Nodo forgets the {} key. Its domains can't be picked for apps any more.", provider.name)];
    if !used.is_empty() {
        body.push(format!("{} still use it, so take them off the internet first.", used.join(", ")));
    }
    Question::careful(format!("Remove {}?", provider.name), body, "Remove", Confirm::RemoveProvider(provider.id.clone()))
}

/// The catalog as it was read when the add flow opened. The snapshot's list is only replaced
/// while the company is being chosen (or when there is none yet), since a frontend at a later
/// step may hold a place in it. `flow` is the provider page being shown, if one is.
pub fn catalog_loaded(snapshot: &mut Snapshot, flow: Option<&Providers>, result: &Result<Vec<Provider>, String>) {
    let picking = flow.is_some_and(|flow| flow.stage == ProviderStage::Pick);
    match result {
        Ok(list) if picking || snapshot.providers.is_empty() => {
            snapshot.providers = list.clone();
            snapshot.providers_error = None;
        }
        Ok(_) => {}
        Err(error) => snapshot.providers_error = Some(error.clone()),
    }
}

/// What a result about a connected provider means, on whichever page it arrives. Both read the
/// device's state again when they worked.
pub fn apply(outcome: &Outcome) -> Step {
    let again = || Step::Run(vec![Effect::Refresh]);
    match outcome {
        Outcome::ProviderRemoved(Ok(())) => Step::All(vec![Step::Done(Notice::ok("The provider was removed.")), again()]),
        Outcome::ProviderRemoved(Err(text)) => Step::Failed(Notice::error(text.clone())),
        Outcome::ProviderRefreshed(Ok(provider)) => {
            let notice = refreshed_notice(provider);
            let said = if provider.problem.is_some() { Step::Failed(notice) } else { Step::Done(notice) };
            Step::All(vec![said, again()])
        }
        Outcome::ProviderRefreshed(Err(text)) => Step::Failed(Notice::error(text.clone())),
        _ => Step::Stay,
    }
}

/// The provider pages: the list of those connected, and the steps of adding one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Providers {
    pub stage: ProviderStage,
    /// What the last try came to, shown above the boxes: why a company cannot be connected, what
    /// is missing, or the helper's refusal of a key.
    pub test: Option<Result<String, String>>,
    /// The site whose Hosting page opened this, to go back to once a provider is added.
    pub return_to: Option<String>,
}

impl Providers {
    /// The add flow, at choosing the company. A frontend opening it also runs [`load`].
    pub fn adding(return_to: Option<String>) -> Self {
        Self { stage: ProviderStage::Pick, test: None, return_to }
    }

    /// The list of providers connected.
    pub fn list() -> Self {
        Self { stage: ProviderStage::List, test: None, return_to: None }
    }

    /// From the list: starts adding one.
    pub fn add(&mut self) -> Vec<Effect> {
        self.stage = ProviderStage::Pick;
        self.test = None;
        load()
    }

    /// From the list: opens the details of one connected provider. Which one is the frontend's to
    /// know.
    pub fn open_details(&mut self) {
        self.stage = ProviderStage::Details;
        self.test = None;
    }

    /// Another company is looked at, so what was said about the last one no longer stands.
    pub fn clear_test(&mut self) {
        self.test = None;
    }

    /// Chooses the company. False when it cannot be connected; `test` then says why. When true,
    /// the key step starts, and a frontend must start it with empty boxes: keys typed for one
    /// provider must never be sent to another.
    pub fn choose(&mut self, provider: &Provider) -> bool {
        if !provider.is_supported() {
            self.test = Some(Err(format!(
                "{} can't be connected: it has no way for the Nodo to change your records. Tor works without a web address.",
                provider.name
            )));
            return false;
        }
        self.stage = ProviderStage::Key;
        self.test = None;
        true
    }

    /// Connects the chosen company with what was typed.
    ///
    /// `Err` names the box to fix, and `test` says what is missing; nothing was taken out of the
    /// boxes. On a locked device the owner is sent to unlock first, since the key is stored
    /// sealed, and the keys stay in their boxes for when they come back. Only when the helper is
    /// asked are the keys taken out of the boxes.
    pub fn submit(
        &mut self,
        provider: &Provider,
        domain: &str,
        api_key: &mut Input,
        secret_key: &mut Input,
        snapshot: &Snapshot,
    ) -> Result<Step, KeyField> {
        let domain = domain.trim().to_owned();
        if !provider.lists_domains && domain.is_empty() {
            self.test = Some(Err(format!(
                "{} can't list your domains, so type the web address you bought, such as example.com.",
                provider.name
            )));
            return Err(KeyField::Domain);
        }
        let needs_second = provider.fields.len() > 1;
        if api_key.is_empty() || (needs_second && secret_key.is_empty()) {
            self.test = Some(Err(format!("Paste the key from {}.", provider.name)));
            return Err(if api_key.is_empty() { KeyField::ApiKey } else { KeyField::SecretKey });
        }
        self.test = None;
        if snapshot.is_locked() {
            let reason = format!("Connecting {} stores its key locked with the device passphrase.", provider.name);
            return Ok(Step::Go(Dest::Unlock { resume: Some(reason) }));
        }
        Ok(Step::Run(vec![Effect::AddProvider {
            provider: provider.name.clone(),
            domain: (!provider.lists_domains).then_some(domain),
            api_key: api_key.take_secret(),
            secret_key: secret_key.take_secret(),
        }]))
    }

    /// One step back: from the key to the companies, from the companies to the list (or, when a
    /// Hosting page opened this, back to it), from a provider's details to the list, and from the
    /// list to the page that opened it. A frontend empties its key boxes when the key step is left.
    pub fn back(&mut self) -> Step {
        match self.stage {
            ProviderStage::List => Step::Go(Dest::Back),
            ProviderStage::Pick if self.return_to.is_some() => Step::Go(Dest::Back),
            ProviderStage::Pick => {
                self.stage = ProviderStage::List;
                self.test = None;
                Step::Stay
            }
            ProviderStage::Key => {
                self.stage = ProviderStage::Pick;
                self.test = None;
                Step::Stay
            }
            ProviderStage::Details => {
                self.stage = ProviderStage::List;
                Step::Stay
            }
            ProviderStage::Done { .. } => self.finish(),
        }
    }

    /// Leaves the page that says the provider is connected: back to the Hosting page that sent
    /// the owner here, which now has a provider (`Dest::HostingWithProvider`), or else to the
    /// list. Either way the device's state is read again. Nothing at any other step.
    pub fn finish(&mut self) -> Step {
        let ProviderStage::Done { domains, .. } = &self.stage else {
            return Step::Stay;
        };
        let again = Step::Run(vec![Effect::Refresh]);
        if self.return_to.is_some() {
            Step::All(vec![Step::Go(Dest::HostingWithProvider { domains: domains.clone() }), again])
        } else {
            *self = Self::list();
            again
        }
    }

    /// What the helper said about the key. Connected: the last step, with the domains the key
    /// controls and the ones it cannot change yet. Refused: its words in `test`, on the key step,
    /// whose boxes a frontend has emptied either way, since nothing typed is kept. A provider
    /// removed while its details are shown leaves them for the list: they have nothing left to
    /// show (the notice about the removal is the module's [`apply`]).
    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::ProviderAdded(Ok(provider)) => {
                let domains = provider.domains.iter().filter(|d| d.held_by.is_none()).map(|d| d.name.clone()).collect();
                let mut held_by: Vec<String> = provider.domains.iter().filter_map(|d| d.held_by.clone()).collect();
                held_by.sort();
                held_by.dedup();
                self.stage =
                    ProviderStage::Done { provider: provider.name.clone(), domains, held_by, blocked: provider.blocked.clone() };
                Step::Run(vec![Effect::Refresh])
            }
            Outcome::ProviderRemoved(Ok(())) if self.stage == ProviderStage::Details => {
                self.stage = ProviderStage::List;
                Step::Stay
            }
            Outcome::ProviderAdded(Err(text)) => {
                self.test = Some(Err(text.clone()));
                Step::Stay
            }
            _ => Step::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::{MockBackend, MOCK_PASSPHRASE};
    use crate::model::ScreenData;
    use crate::secret::Secret;

    /// An unlocked device with the catalog read, as after opening the add flow.
    fn start() -> (MockBackend, Snapshot) {
        let mut mock = MockBackend::new();
        let unlock = nodo_api::Request::Unlock { passphrase: nodo_api::Secret::new(MOCK_PASSPHRASE) };
        nodo_api::Backend::call(&mut mock, unlock).expect("the mock unlocks");
        let mut snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let loaded = execute_effect(&mut mock, load().remove(0));
        let [Outcome::ProvidersLoaded(result)] = &loaded[..] else { panic!("{loaded:?}") };
        catalog_loaded(&mut snapshot, Some(&Providers::adding(None)), result);
        (mock, snapshot)
    }

    fn company<'a>(snapshot: &'a Snapshot, name: &str) -> &'a Provider {
        snapshot.providers.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("no provider {name}"))
    }

    fn typed(text: &str) -> Input {
        let mut input = Input::secret();
        for c in text.chars() {
            input.push(c);
        }
        input
    }

    /// Runs a step's effects and hands the outcomes to the flow.
    fn run(mock: &mut MockBackend, flow: &mut Providers, step: Step) -> Vec<Step> {
        let mut steps = Vec::new();
        for effect in step.effects() {
            for outcome in execute_effect(mock, effect) {
                steps.push(flow.apply(&outcome));
            }
        }
        steps
    }

    #[test]
    fn the_boxes_follow_the_company() {
        let (_, snapshot) = start();
        let lists = company(&snapshot, "Porkbun");
        assert!(lists.lists_domains && lists.fields.len() > 1);
        assert_eq!(fields(Some(lists)), [KeyField::ApiKey, KeyField::SecretKey]);
        let typed_domain = company(&snapshot, "GoDaddy");
        assert_eq!(fields(Some(typed_domain)), [KeyField::Domain, KeyField::ApiKey, KeyField::SecretKey]);
        let mut one_key = lists.clone();
        one_key.fields.truncate(1);
        assert_eq!(fields(Some(&one_key)), [KeyField::ApiKey]);
        assert_eq!(fields(None), [KeyField::ApiKey]);
    }

    #[test]
    fn a_company_that_lists_its_domains_is_connected_with_its_keys_alone() {
        let (mut mock, snapshot) = start();
        let before = snapshot.connected.len();
        let mut flow = Providers::adding(None);
        assert_eq!(flow.stage, ProviderStage::Pick);
        let gandi = company(&snapshot, "Gandi");
        assert!(flow.choose(gandi));
        assert_eq!(flow.stage, ProviderStage::Key);

        // Nothing typed, then one key of two: refused beside the box, nothing sent.
        let (mut api, mut secret) = (Input::secret(), Input::secret());
        assert_eq!(flow.submit(gandi, "", &mut api, &mut secret, &snapshot), Err(KeyField::ApiKey));
        assert_eq!(flow.test, Some(Err("Paste the key from Gandi.".to_owned())));
        let mut api = typed("key-one");
        assert_eq!(flow.submit(gandi, "", &mut api, &mut secret, &snapshot), Err(KeyField::SecretKey));
        assert!(!api.is_empty(), "a refused try leaves the key in its box");

        let mut secret = typed("key-two");
        let step = flow.submit(gandi, "ignored.example", &mut api, &mut secret, &snapshot).expect("both keys are typed");
        assert!(api.is_empty() && secret.is_empty(), "the keys were taken out of the boxes");
        assert_eq!(flow.test, None);
        let [Effect::AddProvider { provider, domain, api_key, secret_key }] = &step.effects()[..] else { panic!("{step:?}") };
        assert_eq!((provider.as_str(), domain), ("Gandi", &None), "the domain is only sent for a company that cannot list them");
        assert_eq!((api_key.expose(), secret_key.expose()), ("key-one", "key-two"));

        let steps = run(&mut mock, &mut flow, step);
        assert_eq!(steps[0], Step::Run(vec![Effect::Refresh]));
        let ProviderStage::Done { provider, domains, held_by, blocked } = &flow.stage else { panic!("{:?}", flow.stage) };
        assert_eq!(provider, "Gandi");
        assert_eq!(domains, &["example.org".to_owned(), "myshop.net".to_owned()]);
        assert!(held_by.is_empty() && blocked.is_empty());
        assert_eq!(load_snapshot(&mut mock).expect("the mock answers").connected.len(), before + 1);

        // Leaving the last step goes to the list and reads the state again.
        assert_eq!(flow.finish(), Step::Run(vec![Effect::Refresh]));
        assert_eq!(flow, Providers::list());
    }

    #[test]
    fn a_company_that_cannot_list_domains_needs_the_domain_typed() {
        let (mut mock, snapshot) = start();
        let mut flow = Providers::adding(None);
        let godaddy = company(&snapshot, "GoDaddy");
        assert!(flow.choose(godaddy));
        let (mut api, mut secret) = (typed("k"), typed("s"));
        assert_eq!(flow.submit(godaddy, "   ", &mut api, &mut secret, &snapshot), Err(KeyField::Domain));
        assert_eq!(
            flow.test,
            Some(Err("GoDaddy can't list your domains, so type the web address you bought, such as example.com.".to_owned()))
        );
        assert!(!api.is_empty() && !secret.is_empty());
        let step = flow.submit(godaddy, " mine.example ", &mut api, &mut secret, &snapshot).expect("everything is typed");
        assert!(matches!(&step.effects()[..], [Effect::AddProvider { domain: Some(domain), .. }] if domain == "mine.example"));
        run(&mut mock, &mut flow, step);
        assert!(matches!(&flow.stage, ProviderStage::Done { domains, .. } if domains == &["mine.example".to_owned()]), "{:?}", flow.stage);
    }

    #[test]
    fn a_company_without_an_api_cannot_be_chosen() {
        let (_, snapshot) = start();
        let unsupported = snapshot.providers.iter().find(|p| !p.is_supported()).expect("the catalog has one without an API");
        let mut flow = Providers::adding(None);
        assert!(!flow.choose(unsupported));
        assert_eq!(flow.stage, ProviderStage::Pick);
        let Some(Err(why)) = &flow.test else { panic!("a reason") };
        assert!(why.starts_with(&format!("{} can't be connected", unsupported.name)) && why.contains("Tor works"), "{why}");
        flow.clear_test();
        assert_eq!(flow.test, None);
    }

    #[test]
    fn a_locked_device_asks_for_the_passphrase_and_keeps_the_keys() {
        // The mock starts locked, as the device does after a restart.
        let mut mock = MockBackend::new();
        let mut snapshot = load_snapshot(&mut mock).expect("the mock answers");
        snapshot.providers = mock.providers().expect("the catalog");
        assert!(snapshot.is_locked());
        let mut flow = Providers::adding(None);
        let porkbun = company(&snapshot, "Porkbun");
        assert!(flow.choose(porkbun));
        let (mut api, mut secret) = (typed("k"), typed("s"));
        let step = flow.submit(porkbun, "", &mut api, &mut secret, &snapshot).expect("everything is typed");
        let Step::Go(Dest::Unlock { resume: Some(reason) }) = step else { panic!("unlock first") };
        assert_eq!(reason, "Connecting Porkbun stores its key locked with the device passphrase.");
        assert!(!api.is_empty() && !secret.is_empty(), "the keys wait in their boxes");
    }

    #[test]
    fn a_refused_key_is_said_on_the_key_step() {
        let (mut mock, snapshot) = start();
        let mut flow = Providers::adding(None);
        flow.stage = ProviderStage::Key;
        let refused = Outcome::ProviderAdded(Err("The key was refused.".to_owned()));
        assert_eq!(flow.apply(&refused), Step::Stay);
        assert_eq!(flow.stage, ProviderStage::Key);
        assert_eq!(flow.test, Some(Err("The key was refused.".to_owned())));
        // The helper's own refusal arrives the same way.
        let outcomes = execute_effect(
            &mut mock,
            Effect::AddProvider { provider: "Nobody".into(), domain: None, api_key: Secret::new("k"), secret_key: Secret::new("s") },
        );
        assert_eq!(flow.apply(&outcomes[0]), Step::Stay);
        assert!(matches!(&flow.test, Some(Err(text)) if text.contains("Nobody")));
        assert_eq!(flow.apply(&Outcome::Info("other".into())), Step::Stay);
        drop(snapshot);
    }

    #[test]
    fn domains_another_provider_holds_are_named_apart() {
        let (mut mock, _) = start();
        let key = Secret::new("k");
        mock.add_provider("Gandi", None, &key, &key).expect("the first account");
        // The same account again: every domain is already held.
        let again = mock.add_provider("Gandi", None, &key, &key).expect("the second account");
        let mut flow = Providers::adding(None);
        flow.stage = ProviderStage::Key;
        assert_eq!(flow.apply(&Outcome::ProviderAdded(Ok(again))), Step::Run(vec![Effect::Refresh]));
        let ProviderStage::Done { domains, held_by, .. } = &flow.stage else { panic!("done") };
        assert!(domains.is_empty());
        assert_eq!(held_by, &["Gandi".to_owned()], "named once");
    }

    #[test]
    fn going_back_follows_the_steps_and_where_the_owner_came_from() {
        let mut flow = Providers::list();
        assert_eq!(flow.back(), Step::Go(Dest::Back));
        assert_eq!(flow.add(), [Effect::LoadProviders]);
        assert_eq!(flow.stage, ProviderStage::Pick);
        flow.stage = ProviderStage::Key;
        flow.test = Some(Err("refused".into()));
        assert_eq!(flow.back(), Step::Stay);
        assert_eq!((&flow.stage, &flow.test), (&ProviderStage::Pick, &None));
        assert_eq!(flow.back(), Step::Stay);
        assert_eq!(flow.stage, ProviderStage::List);

        // Opened from a site's Hosting page: the companies are the first page, so back leaves.
        let mut from_hosting = Providers::adding(Some("shop".into()));
        assert_eq!(from_hosting.back(), Step::Go(Dest::Back));
        assert_eq!(from_hosting.finish(), Step::Stay, "nothing is connected yet");
        from_hosting.stage =
            ProviderStage::Done { provider: "Gandi".into(), domains: vec!["myshop.net".into()], held_by: Vec::new(), blocked: Vec::new() };
        let expected = Step::All(vec![
            Step::Go(Dest::HostingWithProvider { domains: vec!["myshop.net".into()] }),
            Step::Run(vec![Effect::Refresh]),
        ]);
        assert_eq!(from_hosting.clone().finish(), expected);
        assert_eq!(from_hosting.back(), expected, "Esc on the last page is the same as Enter");
    }

    #[test]
    fn removing_a_provider_asks_first_and_names_the_apps_that_use_it() {
        let (mut mock, snapshot) = start();
        let used = snapshot.connected.iter().find(|p| p.domains.iter().any(|d| !d.used_by.is_empty())).expect("one in use");
        let question = ask_remove(used);
        assert!(question.cancel_is_default);
        assert_eq!(question.title, format!("Remove {}?", used.name));
        assert_eq!(question.confirm_label, "Remove");
        assert_eq!(question.confirm, Confirm::RemoveProvider(used.id.clone()));
        assert!(question.body[0].contains(&format!("forgets the {} key", used.name)));
        assert!(question.body[1].contains("shop") && question.body[1].ends_with("take them off the internet first."), "{:?}", question.body);

        let key = Secret::new("k");
        let unused = mock.add_provider("Gandi", None, &key, &key).expect("another account");
        let question = ask_remove(&unused);
        assert_eq!(question.body.len(), 1, "no app uses it");
        let step = question.confirm.step(&snapshot);
        assert_eq!(step, Step::Run(vec![Effect::RemoveProvider(unused.id.clone())]));
        let outcomes = execute_effect(&mut mock, step.effects().remove(0));
        assert_eq!(
            apply(&outcomes[0]),
            Step::All(vec![Step::Done(Notice::ok("The provider was removed.")), Step::Run(vec![Effect::Refresh])])
        );
        assert!(mock.connected.iter().all(|p| p.id != unused.id));
        assert!(matches!(apply(&Outcome::ProviderRemoved(Err("In use.".into()))), Step::Failed(notice) if notice.text == "In use."));
    }

    #[test]
    fn asking_a_provider_again_says_what_it_lists_or_what_is_wrong() {
        let (mut mock, snapshot) = start();
        let id = snapshot.connected.first().expect("a connected provider").id.clone();
        let outcomes = execute_effect(&mut mock, refresh(&id).remove(0));
        let Step::All(steps) = apply(&outcomes[0]) else { panic!("{outcomes:?}") };
        assert!(matches!(&steps[0], Step::Done(notice) if notice.text.contains(" lists ") && notice.text.ends_with(" domains.")), "{steps:?}");
        assert_eq!(steps[1], Step::Run(vec![Effect::Refresh]));

        let mut troubled = snapshot.connected[0].clone();
        troubled.problem = Some("The key no longer works.".into());
        let Step::All(steps) = apply(&Outcome::ProviderRefreshed(Ok(troubled))) else { panic!("all") };
        assert_eq!(steps[0], Step::Failed(Notice::error("The key no longer works.")));
        assert_eq!(steps[1], Step::Run(vec![Effect::Refresh]), "the state is read again either way");
        assert!(matches!(apply(&Outcome::ProviderRefreshed(Err("No answer.".into()))), Step::Failed(_)));
    }

    const API_ACCESS_OFF: &str = "API Access is off for this domain. In your Porkbun account, open the domain's details and switch API Access on";

    #[test]
    fn opening_the_list_asks_every_provider_that_can_list_its_domains_again() {
        let (mut mock, snapshot) = start();
        let key = Secret::new("k");
        mock.add_provider("GoDaddy", Some("mine.example"), &key, &key).expect("one that cannot list its domains");
        let snapshot_now = load_snapshot(&mut mock).expect("the mock answers");
        let listing: Vec<Effect> =
            snapshot_now.connected.iter().filter(|p| p.lists_domains).map(|p| Effect::RefreshProvider(p.id.clone())).collect();
        assert!(!listing.is_empty() && listing.len() < snapshot_now.connected.len(), "the mock has both kinds");
        assert_eq!(open_list(&snapshot_now), listing);
        drop(snapshot);

        // A locked device cannot open the keys: nothing is asked, and asking by hand says why.
        let mut locked = MockBackend::new();
        let snapshot = load_snapshot(&mut locked).expect("the mock answers");
        assert!(snapshot.is_locked() && !snapshot.connected.is_empty());
        assert_eq!(open_list(&snapshot), Vec::new());
        locked.key_locked = true;
        let outcomes = execute_effect(&mut locked, refresh(&snapshot.connected[0].id).remove(0));
        assert!(matches!(apply(&outcomes[0]), Step::Failed(notice) if notice.text.contains("unlock the device")), "{outcomes:?}");
    }

    #[test]
    fn looking_again_names_the_domains_the_key_cannot_change_and_points_at_the_details() {
        let (mut mock, snapshot) = start();
        mock.connected[0].blocked = vec![("example.net".to_owned(), API_ACCESS_OFF.to_owned())];
        let id = snapshot.connected[0].id.clone();
        let outcomes = execute_effect(&mut mock, refresh(&id).remove(0));
        let Step::All(steps) = apply(&outcomes[0]) else { panic!("{outcomes:?}") };
        let Step::Done(notice) = &steps[0] else { panic!("{steps:?}") };
        assert_eq!(notice.kind, crate::flows::NoticeKind::Info, "not an error, but worth saying");
        assert_eq!(notice.text, "Porkbun lists 1 domains, and 1 more its key can't change yet: example.net.");
        assert_eq!(notice.hint, Some(Hint::ProviderDetails), "the words name no key");
        assert_eq!(steps[1], Step::Run(vec![Effect::Refresh]));
        // With a problem as well, the problem is what is said.
        let mut troubled = mock.connected[0].clone();
        troubled.problem = Some("The key no longer works.".into());
        assert_eq!(refreshed_notice(&troubled), Notice::error("The key no longer works."));
    }

    #[test]
    fn a_new_connection_keeps_the_domains_its_key_cannot_change() {
        let mut flow = Providers::adding(None);
        flow.stage = ProviderStage::Key;
        let added = ConnectedProvider {
            id: "porkbun".into(),
            name: "Porkbun".into(),
            domains: vec![crate::model::ConnectedDomain { name: "example.com".into(), used_by: Vec::new(), held_by: None }],
            lists_domains: true,
            blocked: vec![("example.net".to_owned(), API_ACCESS_OFF.to_owned())],
            ..ConnectedProvider::default()
        };
        assert_eq!(flow.apply(&Outcome::ProviderAdded(Ok(added))), Step::Run(vec![Effect::Refresh]));
        let ProviderStage::Done { domains, blocked, .. } = &flow.stage else { panic!("{:?}", flow.stage) };
        assert_eq!(domains, &["example.com".to_owned()]);
        assert_eq!(blocked, &[("example.net".to_owned(), API_ACCESS_OFF.to_owned())]);
    }

    #[test]
    fn a_providers_details_open_from_the_list_and_close_when_it_is_removed() {
        let mut flow = Providers::list();
        flow.open_details();
        assert_eq!(flow.stage, ProviderStage::Details);
        assert_eq!(flow.finish(), Step::Stay, "nothing was just connected");
        assert_eq!(flow.back(), Step::Stay);
        assert_eq!(flow.stage, ProviderStage::List, "back from the details is the list");

        flow.open_details();
        assert_eq!(flow.apply(&Outcome::ProviderRemoved(Err("In use.".into()))), Step::Stay);
        assert_eq!(flow.stage, ProviderStage::Details, "a refused removal leaves the details");
        assert_eq!(flow.apply(&Outcome::ProviderRemoved(Ok(()))), Step::Stay);
        assert_eq!(flow.stage, ProviderStage::List, "its details have nothing left to show");
        // On the list a removal changes nothing here; the notice is the module's.
        assert_eq!(flow.apply(&Outcome::ProviderRemoved(Ok(()))), Step::Stay);
        assert_eq!(flow, Providers::list());
    }

    #[test]
    fn the_catalog_is_only_replaced_while_the_company_is_chosen() {
        let mut mock = MockBackend::new();
        let mut snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let fresh = mock.providers();
        let mut shorter = fresh.clone().expect("the catalog");
        shorter.truncate(2);
        snapshot.providers = shorter.clone();

        let mut flow = Providers::adding(None);
        flow.stage = ProviderStage::Key;
        catalog_loaded(&mut snapshot, Some(&flow), &fresh);
        assert_eq!(snapshot.providers, shorter, "a later step holds a place in the list");
        catalog_loaded(&mut snapshot, None, &fresh);
        assert_eq!(snapshot.providers, shorter);
        catalog_loaded(&mut snapshot, Some(&flow), &Err("No answer.".into()));
        assert_eq!(snapshot.providers_error.as_deref(), Some("No answer."));

        flow.stage = ProviderStage::Pick;
        catalog_loaded(&mut snapshot, Some(&flow), &fresh);
        assert_eq!(Ok(snapshot.providers.clone()), fresh);
        assert_eq!(snapshot.providers_error, None);

        snapshot.providers.clear();
        catalog_loaded(&mut snapshot, None, &fresh);
        assert!(!snapshot.providers.is_empty(), "an empty catalog is always filled");
    }
}
