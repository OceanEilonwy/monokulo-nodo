//! A site's Hosting page: who can open it (home network, Tor, the internet) and, for the
//! internet, at which address and which other names.
//!
//! The three choices are independent flags. Turning anything off never asks. The internet is
//! never turned on by Save itself: Save applies the other two, saves the names, and starts the
//! reachability check, whose result leads to the confirmation (`super::public`).
//!
//! A site that is public already has no such confirmation ahead of it, so a name it gains would
//! go on the internet at once. Save asks about those names first, starting on Cancel, and the
//! helper refuses them unless they come confirmed (`Effect::SetPublicHost::confirm_added`).

use nodo_api::{AliasMode, Exposure, PublicAlias};

use super::super::{Confirm, Dest, Notice, Question, Step};
use crate::effect::ExposureAfter;
use crate::model::{Site, Snapshot};
use crate::Effect;

/// The address a part before the domain and a domain make. An empty part is the domain itself.
pub fn join_host(sub: &str, domain: &str) -> String {
    let sub = sub.trim().trim_matches('.');
    if sub.is_empty() { domain.to_owned() } else { format!("{sub}.{domain}") }
}

fn under(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// The domain a host is under, the longest when several fit, and the part before it. `domains`
/// are `Snapshot::domain_choices`. `None` when the host is under none of them.
pub fn split_host(host: &str, domains: &[(String, String)]) -> Option<(String, String)> {
    let (domain, _) = domains.iter().filter(|(domain, _)| under(host, domain)).max_by_key(|(domain, _)| domain.len())?;
    let sub = host.strip_suffix(domain.as_str()).unwrap_or_default().trim_end_matches('.');
    Some((sub.to_owned(), domain.clone()))
}

/// `current` when it is one of the domains that can be chosen, else the first of them.
pub fn domain_or_first(current: &str, domains: &[(String, String)]) -> String {
    if !current.is_empty() && domains.iter().any(|(domain, _)| domain == current) {
        current.to_owned()
    } else {
        domains.first().map(|(domain, _)| domain.clone()).unwrap_or_default()
    }
}

/// Every name the other sites answer at, with the site, as the helper checks them.
pub fn names_of_other_sites(sites: &[Site], site: &str) -> Vec<(String, String)> {
    sites
        .iter()
        .filter(|other| other.package != site)
        .flat_map(|other| {
            let names = other.public_host.iter().cloned().chain(other.public_aliases.iter().map(|a| a.host.clone()));
            names.map(|name| (name, other.package.clone())).collect::<Vec<_>>()
        })
        .collect()
}

/// A helper message as a sentence on its own. It often starts with a name, which keeps its case.
fn sentence(text: &str) -> String {
    if text.ends_with('.') { text.to_owned() } else { format!("{text}.") }
}

/// What the Hosting page offers, in order, given the draft. A frontend draws these and keeps
/// its own cursor over them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Home,
    Tor,
    Internet,
    /// The domain of the main address, one of the domains of the connected providers.
    Domain,
    /// The part of the main address before the domain.
    Address,
    /// One of the other names, by index into `Hosting::aliases`. It can be removed.
    Alias(usize),
    /// The way to add another name. A frontend may keep its controls behind a button until the
    /// owner asks for another address, so a site with one address shows only that one.
    NewAlias,
    /// The internet is ticked and no provider is connected: the way to add one.
    AddProvider,
    Save,
}

/// The draft of one site's hosting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hosting {
    pub site: String,
    pub draft: Exposure,
    /// The part of the public address before the domain, such as `pay` in `pay.example.com`.
    pub sub: String,
    /// The chosen domain, one of `Snapshot::domain_choices`.
    pub domain: String,
    /// A domain provider is connected, so the internet choice can pick a domain.
    pub has_provider: bool,
    /// The other names the site answers at, as they will be saved.
    pub aliases: Vec<PublicAlias>,
    /// Why the last new name could not be added. It belongs beside the new name.
    pub alias_error: Option<String>,
}

/// The Hosting page of an installed app, or the remark that it has none.
pub fn open(snapshot: &Snapshot, site: &str) -> Result<Hosting, Notice> {
    let Some(entry) = snapshot.web.sites.iter().find(|entry| entry.package == site) else {
        return Err(Notice::info(format!("{site} has no web page to put online.")));
    };
    let mut hosting = Hosting::new(&entry.package, entry.exposure, entry.public_host.as_deref(), &snapshot.domain_choices());
    hosting.aliases = entry.public_aliases.clone();
    Ok(hosting)
}

impl Hosting {
    /// `host` is the site's saved public host; `domains` are `Snapshot::domain_choices`. The
    /// domain is the longest one the host is under, or the first. Without a saved host under a
    /// domain, the address starts as the last part of the site's name.
    pub fn new(site: &str, draft: Exposure, host: Option<&str>, domains: &[(String, String)]) -> Self {
        let split = host.and_then(|host| split_host(host, domains));
        let (sub, domain) = match split {
            Some(split) => split,
            None => (
                site.split('-').next_back().unwrap_or(site).to_owned(),
                domains.first().map(|(domain, _)| domain.clone()).unwrap_or_default(),
            ),
        };
        Self {
            site: site.to_owned(),
            draft,
            sub,
            domain,
            has_provider: !domains.is_empty(),
            aliases: Vec::new(),
            alias_error: None,
        }
    }

    /// Brings the draft in line with the providers connected now: whether there is one, and a
    /// chosen domain that is still one of theirs. A frontend calls it before acting on a draft
    /// that was made from an older snapshot.
    pub fn sync(&mut self, snapshot: &Snapshot) {
        let choices = snapshot.domain_choices();
        self.has_provider = !choices.is_empty();
        self.domain = domain_or_first(&self.domain, &choices);
    }

    /// The three choices at once. Each is its own flag.
    pub fn set_exposure(&mut self, local: bool, onion: bool, public: bool) {
        self.draft = Exposure { local, onion, public };
    }

    /// The part of the main address before the domain.
    pub fn set_sub(&mut self, sub: &str) {
        self.sub = sub.to_owned();
    }

    /// Chooses the domain of the main address. False when it is not a domain that can be chosen.
    pub fn set_domain(&mut self, domain: &str, snapshot: &Snapshot) -> bool {
        let known = snapshot.domain_choices().iter().any(|(choice, _)| choice == domain);
        if known {
            self.domain = domain.to_owned();
        }
        known
    }

    /// The main address by its two parts.
    pub fn set_host(&mut self, sub: &str, domain: &str, snapshot: &Snapshot) -> bool {
        let known = self.set_domain(domain, snapshot);
        if known {
            self.set_sub(sub);
        }
        known
    }

    /// The main address typed whole, for a frontend with one box. It must be under a domain of
    /// a connected provider; the error belongs beside the box.
    pub fn set_host_text(&mut self, host: &str, snapshot: &Snapshot) -> Result<(), String> {
        let host = host.trim().trim_matches('.');
        let Some((sub, domain)) = split_host(host, &snapshot.domain_choices()) else {
            return Err("The address must be under one of your domains.".to_owned());
        };
        self.sub = sub;
        self.domain = domain;
        Ok(())
    }

    /// The public host the choices make: the address part and the chosen domain.
    pub fn host(&self) -> String {
        join_host(&self.sub, &self.domain)
    }

    /// Other names can be added: with the internet ticked and a provider to give them a domain.
    /// A site that is public already can gain one too; saving then asks first (`save`).
    pub fn can_add_alias(&self) -> bool {
        self.draft.public && self.has_provider
    }

    /// Puts another name on the list, after the same checks the helper makes on saving, so a name
    /// it would refuse is refused here at once, in its words. False when it was refused;
    /// `alias_error` then says why.
    pub fn add_alias(&mut self, name: &str, mode: AliasMode, snapshot: &Snapshot) -> bool {
        let owned: Vec<String> = snapshot.domain_choices().into_iter().map(|(domain, _)| domain).collect();
        let mut next = self.aliases.clone();
        next.push(PublicAlias { host: name.to_owned(), mode });
        match nodo_api::check_aliases(&self.host(), &next, &owned, &names_of_other_sites(&snapshot.web.sites, &self.site)) {
            Ok(()) => {
                self.aliases = next;
                self.alias_error = None;
                true
            }
            Err(reason) => {
                self.alias_error = Some(sentence(&reason));
                false
            }
        }
    }

    /// Takes one of the other names off the list. Nothing happens when it is not on it.
    pub fn remove_alias(&mut self, name: &str) {
        self.aliases.retain(|alias| alias.host != name);
    }

    /// The new name is being typed again, so the last refusal no longer stands.
    pub fn clear_alias_error(&mut self) {
        self.alias_error = None;
    }

    /// A provider was connected from this page, which sent the owner to add one: the internet is
    /// ticked, and the first of the provider's domains is chosen.
    pub fn provider_connected(&mut self, domains: &[String]) {
        self.has_provider = true;
        self.draft.public = true;
        if let Some(first) = domains.first() {
            self.domain = first.clone();
        }
    }

    /// What the page offers, in order.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Home, Row::Tor, Row::Internet];
        if self.draft.public {
            if self.has_provider {
                rows.extend([Row::Domain, Row::Address]);
                rows.extend((0..self.aliases.len()).map(Row::Alias));
                rows.push(Row::NewAlias);
            } else {
                rows.push(Row::AddProvider);
            }
        }
        if self.can_save() {
            rows.push(Row::Save);
        }
        rows
    }

    /// Save is offered: always, except with the internet ticked and no provider to give it a name.
    pub fn can_save(&self) -> bool {
        !self.draft.public || self.has_provider
    }

    /// Saves the draft.
    ///
    /// - Nothing changed, or the internet ticked without a provider: a remark, nothing done.
    /// - Not going public: the exposure is set, and the page is left once that worked.
    /// - Going public: home network and Tor are applied now, since they stay as chosen while the
    ///   internet is checked, and then the reachability check runs. The internet itself only
    ///   comes on after the confirmation.
    /// - Whenever the names change, and always on the way to public, the names are saved first
    ///   and the rest runs only once the helper took them (`Effect::SetPublicHost::then`).
    /// - A site that is public and stays public would gain its new names at once, with no
    ///   going-public confirmation to list them: they are asked about first, and nothing is saved
    ///   until the owner confirms (`Confirm::AddPublicNames`).
    pub fn save(&self, snapshot: &Snapshot) -> Step {
        self.save_confirmed(snapshot, &[])
    }

    /// The names this draft adds to a site that is public and stays public: the ones that go on
    /// the internet as soon as they are saved. Empty for a site that is not public yet, since
    /// going public confirms every name.
    pub fn names_added_while_public(&self, snapshot: &Snapshot) -> Vec<PublicAlias> {
        let Some(site) = snapshot.web.sites.iter().find(|site| site.package == self.site) else {
            return Vec::new();
        };
        if !(self.draft.public && site.exposure.public) {
            return Vec::new();
        }
        self.aliases.iter().filter(|alias| !site.public_aliases.iter().any(|old| old.host == alias.host)).cloned().collect()
    }

    /// The question before new names go on a site that is public. It starts on Cancel.
    fn ask_add_public_names(&self, added: &[PublicAlias]) -> Question {
        let host = self.host();
        let names: Vec<String> = added.iter().map(|alias| alias.host.clone()).collect();
        let title = match names.as_slice() {
            [one] => format!("Add {one} to the internet?"),
            many => format!("Add {} addresses to the internet?", many.len()),
        };
        let mut body: Vec<String> = added
            .iter()
            .map(|alias| match alias.mode {
                AliasMode::Redirect => format!("https://{}  redirects to {host}", alias.host),
                AliasMode::Serve => format!("https://{}  shows the same site", alias.host),
            })
            .collect();
        body.push(format!("{} is on the internet, so this happens as soon as you add it.", self.site));
        body.push("It points to your home's internet address, like the main address.".to_owned());
        body.push(format!("Public certificate logs list {} permanently.", names.join(", ")));
        Question::careful(title, body, "Add", Confirm::AddPublicNames { hosting: Box::new(self.clone()), names })
    }

    /// Saves the draft as `save` does. `confirmed` are the new names the owner agreed to put on a
    /// site that is public; when the draft adds others, the question is asked (again) and nothing
    /// is saved.
    pub fn save_confirmed(&self, snapshot: &Snapshot, confirmed: &[String]) -> Step {
        let Some(site) = snapshot.web.sites.iter().find(|site| site.package == self.site) else {
            return Step::Go(Dest::LeaveHosting { site: self.site.clone() });
        };
        let draft = self.draft;
        let host = self.host();
        let aliases = self.aliases.clone();
        let name = self.site.clone();
        if draft.public && !self.has_provider {
            return Step::Done(Notice::info("Add a domain provider first, so the internet can use your web address."));
        }
        let added = self.names_added_while_public(snapshot);
        let added_names: Vec<String> = added.iter().map(|alias| alias.host.clone()).collect();
        if !added_names.is_empty() && added_names != confirmed {
            return Step::Ask(self.ask_add_public_names(&added));
        }
        // Going public sends the names even unchanged, so the helper checks them again first: a
        // provider removed since they were saved would leave a name with no record or certificate.
        let going_public = draft.public && !site.exposure.public;
        let names_change = draft.public
            && (site.public_host.as_deref() != Some(host.as_str())
                || aliases != site.public_aliases
                || (going_public && !aliases.is_empty()));
        if draft == site.exposure && !names_change {
            return Step::Done(Notice::info("There are no changes to save."));
        }
        let mut effects = Vec::new();
        if going_public {
            let others = Exposure { public: false, ..draft };
            if others != site.exposure {
                effects.push(Effect::SetExposure { site: name.clone(), exposure: others, after: ExposureAfter::Stay, confirmation: None });
            }
            effects.push(Effect::CheckReachability { site: name.clone(), going_public: true });
        } else if draft != site.exposure {
            effects.push(Effect::SetExposure { site: name.clone(), exposure: draft, after: ExposureAfter::Back, confirmation: None });
        }
        if names_change {
            // The names go first, and the rest only once the helper took them.
            return Step::Run(vec![Effect::SetPublicHost { site: name, host, aliases, confirm_added: added_names, then: effects }]);
        }
        Step::Run(effects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot, run_batch};
    use crate::flows::web::{self, public::Found};
    use crate::mock::MockBackend;
    use crate::Outcome;

    fn start() -> (MockBackend, Snapshot) {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        (mock, snapshot)
    }

    fn domains(names: &[&str]) -> Vec<(String, String)> {
        names.iter().map(|name| ((*name).to_owned(), "Porkbun".to_owned())).collect()
    }

    fn exposure(mock: &mut MockBackend, site: &str) -> Exposure {
        let snapshot = load_snapshot(mock).expect("the mock answers");
        snapshot.web.sites.iter().find(|entry| entry.package == site).expect("the site").exposure
    }

    /// Runs a step's effects as one batch, as a frontend's worker does, following every step the
    /// results lead to. Returns the steps that were not effects.
    fn follow(mock: &mut MockBackend, step: Step) -> Vec<Step> {
        let mut seen = Vec::new();
        let mut waiting = vec![step];
        while let Some(step) = waiting.pop() {
            match step {
                Step::Run(effects) => {
                    let mut outcomes = Vec::new();
                    run_batch(mock, effects, |batch| outcomes.extend(batch));
                    let steps: Vec<Step> = outcomes.iter().map(web::apply).filter(|step| *step != Step::Stay).collect();
                    waiting.extend(steps.into_iter().rev());
                }
                Step::All(steps) => waiting.extend(steps.into_iter().rev()),
                other => seen.push(other),
            }
        }
        seen
    }

    #[test]
    fn a_host_is_its_part_and_its_domain() {
        assert_eq!(join_host("pay", "example.com"), "pay.example.com");
        assert_eq!(join_host("  .pay. ", "example.com"), "pay.example.com");
        assert_eq!(join_host("", "example.com"), "example.com", "an empty part is the domain itself");
        assert_eq!(join_host("a.b", "example.com"), "a.b.example.com");
    }

    #[test]
    fn a_saved_host_splits_at_the_longest_domain_it_is_under() {
        let both = domains(&["example.com", "shop.example.com", "other.net"]);
        assert_eq!(split_host("pay.example.com", &both), Some(("pay".into(), "example.com".into())));
        assert_eq!(split_host("a.shop.example.com", &both), Some(("a".into(), "shop.example.com".into())));
        assert_eq!(split_host("shop.example.com", &both), Some((String::new(), "shop.example.com".into())));
        assert_eq!(split_host("notexample.com", &both), None, "a suffix that is not a label does not count");
        assert_eq!(split_host("pay.example.org", &both), None);

        let hosting = Hosting::new("monokulo-pay", Exposure::default(), Some("pay.example.com"), &both);
        assert_eq!((hosting.sub.as_str(), hosting.domain.as_str()), ("pay", "example.com"));
        assert_eq!(hosting.host(), "pay.example.com");
        // No saved host, or one under no domain: the last part of the app's name, the first domain.
        let fresh = Hosting::new("monokulo-pay", Exposure::default(), None, &both);
        assert_eq!(fresh.host(), "pay.example.com");
        let stray = Hosting::new("monokulo-pay", Exposure::default(), Some("pay.gone.org"), &both);
        assert_eq!(stray.host(), "pay.example.com");
        let none = Hosting::new("shop", Exposure::default(), None, &[]);
        assert!(!none.has_provider && none.domain.is_empty());
    }

    #[test]
    fn the_whole_address_is_taken_only_under_a_connected_domain() {
        let (_, snapshot) = start();
        let mut hosting = open(&snapshot, "monokulo-pay").expect("a site");
        assert_eq!(hosting.set_host_text(" store.example.com ", &snapshot), Ok(()));
        assert_eq!((hosting.sub.as_str(), hosting.domain.as_str()), ("store", "example.com"));
        assert_eq!(hosting.set_host_text("store.elsewhere.net", &snapshot), Err("The address must be under one of your domains.".into()));
        assert_eq!(hosting.host(), "store.example.com", "a refused address changes nothing");
        assert!(!hosting.set_domain("elsewhere.net", &snapshot));
        assert!(hosting.set_host("pay", "example.com", &snapshot));
        assert_eq!(hosting.host(), "pay.example.com");
        assert_eq!(open(&snapshot, "cuprate").unwrap_err().text, "cuprate has no web page to put online.");
    }

    #[test]
    fn the_rows_follow_the_draft() {
        let (_, snapshot) = start();
        let mut hosting = open(&snapshot, "monokulo-pay").expect("a site");
        assert_eq!(hosting.rows(), [Row::Home, Row::Tor, Row::Internet, Row::Save]);
        hosting.set_exposure(true, true, true);
        assert_eq!(hosting.rows(), [Row::Home, Row::Tor, Row::Internet, Row::Domain, Row::Address, Row::NewAlias, Row::Save]);
        assert!(hosting.add_alias("www.example.com", AliasMode::Redirect, &snapshot));
        assert!(hosting.rows().contains(&Row::Alias(0)));
        hosting.has_provider = false;
        assert_eq!(hosting.rows(), [Row::Home, Row::Tor, Row::Internet, Row::AddProvider], "no Save without a provider");
        assert!(!hosting.can_save());
        // A site that is public already can gain a name too; saving it is what asks.
        let public = open(&snapshot, "shop").expect("a site");
        assert!(public.draft.public && public.rows().contains(&Row::NewAlias) && public.can_add_alias());
    }

    #[test]
    fn another_name_is_checked_as_the_helper_checks_it() {
        let (_, snapshot) = start();
        let mut hosting = open(&snapshot, "monokulo-pay").expect("a site");
        hosting.set_exposure(true, true, true);
        assert!(hosting.add_alias("www.example.com", AliasMode::Redirect, &snapshot));
        assert_eq!(hosting.aliases, [PublicAlias { host: "www.example.com".into(), mode: AliasMode::Redirect }]);

        // A name another site answers at.
        assert!(!hosting.add_alias("shop.example.com", AliasMode::Serve, &snapshot));
        assert_eq!(hosting.alias_error.as_deref(), Some("shop.example.com is already an address of shop."));
        // Twice, the main address itself, and a domain no provider holds.
        assert!(!hosting.add_alias("www.example.com", AliasMode::Serve, &snapshot));
        assert_eq!(hosting.alias_error.as_deref(), Some("www.example.com is listed twice."));
        assert!(!hosting.add_alias("pay.example.com", AliasMode::Serve, &snapshot));
        assert!(hosting.alias_error.as_deref().is_some_and(|e| e.contains("main address")), "{:?}", hosting.alias_error);
        assert!(!hosting.add_alias("pay.elsewhere.net", AliasMode::Serve, &snapshot));
        assert!(hosting.alias_error.is_some());
        assert_eq!(hosting.aliases.len(), 1, "a refused name is not listed");
        hosting.clear_alias_error();
        assert_eq!(hosting.alias_error, None);

        hosting.remove_alias("www.example.com");
        hosting.remove_alias("never.example.com");
        assert!(hosting.aliases.is_empty());

        // A public site's draft takes a new name the same way; nothing is said until Save.
        let mut public = open(&snapshot, "shop").expect("a site");
        assert!(public.add_alias("www.example.com", AliasMode::Redirect, &snapshot));
        assert_eq!(public.alias_error, None);
    }

    fn alias(host: &str, mode: AliasMode) -> PublicAlias {
        PublicAlias { host: host.into(), mode }
    }

    #[test]
    fn a_public_site_gains_a_name_only_once_the_owner_confirms_it() {
        let (mut mock, snapshot) = start();
        // The shop is public and answers at example.com already.
        let mut hosting = open(&snapshot, "shop").expect("a site");
        assert_eq!(hosting.aliases, [alias("example.com", AliasMode::Redirect)]);
        assert!(hosting.names_added_while_public(&snapshot).is_empty());
        assert!(hosting.add_alias("www.example.com", AliasMode::Serve, &snapshot));
        assert_eq!(hosting.names_added_while_public(&snapshot), [alias("www.example.com", AliasMode::Serve)]);

        // Save asks first, about the new name only, starting on Cancel, and sends nothing.
        let Step::Ask(question) = hosting.save(&snapshot) else { panic!("a question") };
        assert!(question.cancel_is_default);
        assert_eq!(question.title, "Add www.example.com to the internet?");
        assert_eq!(question.confirm_label, "Add");
        assert_eq!(
            question.body,
            [
                "https://www.example.com  shows the same site",
                "shop is on the internet, so this happens as soon as you add it.",
                "It points to your home's internet address, like the main address.",
                "Public certificate logs list www.example.com permanently.",
            ]
        );
        assert!(!question.confirm.is_long());
        assert!(mock.calls.iter().all(|call| !matches!(call, nodo_api::Request::SetPublicHost { .. })));

        // Confirmed, the names go to the helper with the new one named, and the page is left.
        let step = question.confirm.step(&snapshot);
        let Step::Run(effects) = &step else { panic!("{step:?}") };
        let [Effect::SetPublicHost { aliases, confirm_added, then, .. }] = &effects[..] else { panic!("{effects:?}") };
        assert_eq!(aliases.len(), 2);
        assert_eq!(confirm_added, &["www.example.com".to_owned()]);
        assert!(then.is_empty(), "the exposure does not change");
        assert_eq!(follow(&mut mock, step), [Step::Go(Dest::LeaveHosting { site: "shop".into() }), Step::Done(Notice::ok("Saved."))]);
        assert_eq!(mock.web.sites[0].public_aliases.len(), 2);
        assert!(exposure(&mut mock, "shop").public, "it stayed public throughout");
    }

    #[test]
    fn a_confirmation_covers_only_the_names_it_listed() {
        let (_, snapshot) = start();
        let mut hosting = open(&snapshot, "shop").expect("a site");
        assert!(hosting.add_alias("www.example.com", AliasMode::Redirect, &snapshot));
        assert!(hosting.add_alias("store.example.com", AliasMode::Serve, &snapshot));
        let Step::Ask(question) = hosting.save(&snapshot) else { panic!("a question") };
        assert_eq!(question.title, "Add 2 addresses to the internet?");
        assert_eq!(question.body[0], "https://www.example.com  redirects to shop.example.com");
        assert_eq!(question.body[1], "https://store.example.com  shows the same site");
        assert_eq!(question.body[4], "Public certificate logs list www.example.com, store.example.com permanently.");
        // Agreeing to one of two is not agreeing to both: it is asked again.
        assert!(matches!(hosting.save_confirmed(&snapshot, &["www.example.com".to_owned()]), Step::Ask(_)));
        let both = ["www.example.com".to_owned(), "store.example.com".to_owned()];
        assert!(matches!(hosting.save_confirmed(&snapshot, &both), Step::Run(_)));
    }

    #[test]
    fn names_that_publish_nothing_new_are_saved_without_a_question() {
        let (mut mock, snapshot) = start();
        // Taking a name away, and changing whether a name redirects: the name is public already.
        let mut hosting = open(&snapshot, "shop").expect("a site");
        hosting.aliases[0].mode = AliasMode::Serve;
        let step = hosting.save(&snapshot);
        assert!(matches!(&step, Step::Run(effects) if matches!(&effects[..], [Effect::SetPublicHost { confirm_added, .. }] if confirm_added.is_empty())), "{step:?}");
        assert_eq!(follow(&mut mock, step).len(), 2);
        assert_eq!(mock.web.sites[0].public_aliases, [alias("example.com", AliasMode::Serve)]);

        // On the way to public the going-public confirmation lists every name, so Save asks nothing.
        let mut going = open(&snapshot, "moneropay").expect("a site");
        going.set_exposure(true, false, true);
        assert!(going.add_alias("www.example.com", AliasMode::Redirect, &snapshot));
        assert!(going.names_added_while_public(&snapshot).is_empty());
        assert!(matches!(going.save(&snapshot), Step::Run(_)));

        // A public site taken off the internet drops the question with the internet.
        let mut leaving = open(&snapshot, "shop").expect("a site");
        assert!(leaving.add_alias("www.example.com", AliasMode::Redirect, &snapshot));
        leaving.set_exposure(true, false, false);
        assert!(matches!(leaving.save(&snapshot), Step::Run(_)));
    }

    #[test]
    fn the_helper_refuses_a_new_name_of_a_public_site_that_was_not_confirmed() {
        let (mut mock, _) = start();
        let names = vec![alias("example.com", AliasMode::Redirect), alias("www.example.com", AliasMode::Serve)];
        let unconfirmed = Effect::SetPublicHost {
            site: "shop".into(),
            host: "shop.example.com".into(),
            aliases: names,
            confirm_added: Vec::new(),
            then: Vec::new(),
        };
        let seen = follow(&mut mock, Step::Run(vec![unconfirmed]));
        assert!(matches!(&seen[..], [Step::Failed(notice)] if notice.text.contains("confirm adding it")), "{seen:?}");
        assert_eq!(mock.web.sites[0].public_aliases.len(), 1);
    }

    #[test]
    fn the_names_of_other_sites_are_their_hosts_and_aliases() {
        let (_, mut snapshot) = start();
        snapshot.web.sites[0].public_aliases = vec![PublicAlias { host: "store.example.com".into(), mode: AliasMode::Serve }];
        let names = names_of_other_sites(&snapshot.web.sites, "monokulo-pay");
        assert!(names.contains(&("shop.example.com".into(), "shop".into())));
        assert!(names.contains(&("store.example.com".into(), "shop".into())));
        assert!(names.iter().all(|(_, site)| site != "monokulo-pay"));
    }

    #[test]
    fn saving_nothing_says_so_and_a_missing_provider_is_asked_for() {
        let (_, snapshot) = start();
        let mut hosting = open(&snapshot, "monokulo-pay").expect("a site");
        assert_eq!(hosting.save(&snapshot), Step::Done(Notice::info("There are no changes to save.")));
        hosting.set_exposure(true, true, true);
        hosting.has_provider = false;
        assert_eq!(
            hosting.save(&snapshot),
            Step::Done(Notice::info("Add a domain provider first, so the internet can use your web address."))
        );
        hosting.site = "gone".into();
        assert_eq!(hosting.save(&snapshot), Step::Go(Dest::LeaveHosting { site: "gone".into() }));
    }

    #[test]
    fn offline_to_the_home_network_or_tor_sets_the_exposure_and_leaves() {
        let (mut mock, snapshot) = start();
        let mut hosting = open(&snapshot, "wallet-relay").expect("a site");
        hosting.set_exposure(true, false, false);
        let step = hosting.save(&snapshot);
        assert_eq!(
            step,
            Step::Run(vec![Effect::SetExposure {
                site: "wallet-relay".into(),
                exposure: Exposure { local: true, onion: false, public: false },
                after: ExposureAfter::Back,
                confirmation: None,
            }])
        );
        assert_eq!(follow(&mut mock, step), [Step::Go(Dest::LeaveHosting { site: "wallet-relay".into() }), Step::Done(Notice::ok("Saved."))]);
        assert!(exposure(&mut mock, "wallet-relay").local);

        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let mut hosting = open(&snapshot, "wallet-relay").expect("a site");
        hosting.set_exposure(true, true, false);
        let seen = follow(&mut mock, hosting.save(&snapshot));
        assert_eq!(seen.len(), 2);
        assert_eq!(exposure(&mut mock, "wallet-relay"), Exposure { local: true, onion: true, public: false });
    }

    #[test]
    fn going_public_saves_the_names_then_the_rest_then_checks_and_never_turns_the_internet_on() {
        let (mut mock, snapshot) = start();
        // moneropay: home network only, a saved host. Tor and the internet are ticked.
        let mut hosting = open(&snapshot, "moneropay").expect("a site");
        hosting.set_exposure(true, true, true);
        hosting.set_sub("pay2");
        let step = hosting.save(&snapshot);
        let others = Effect::SetExposure {
            site: "moneropay".into(),
            exposure: Exposure { local: true, onion: true, public: false },
            after: ExposureAfter::Stay,
            confirmation: None,
        };
        let check = Effect::CheckReachability { site: "moneropay".into(), going_public: true };
        assert_eq!(
            step,
            Step::Run(vec![Effect::SetPublicHost {
                site: "moneropay".into(),
                host: "pay2.example.com".into(),
                aliases: Vec::new(),
                confirm_added: Vec::new(),
                then: vec![others, check],
            }])
        );
        let seen = follow(&mut mock, step);
        let [Step::Done(saved), Step::Go(Dest::Reachability(Found::Clear(confirm)))] = &seen[..] else { panic!("{seen:?}") };
        assert_eq!(saved.text, "Saved.");
        assert_eq!(confirm.plan.host, "pay2.example.com");
        assert_eq!(exposure(&mut mock, "moneropay"), Exposure { local: true, onion: true, public: false }, "not public yet");
    }

    #[test]
    fn going_public_with_the_saved_name_checks_without_saving_names() {
        let (_, snapshot) = start();
        let mut hosting = open(&snapshot, "monokulo-pay").expect("a site");
        hosting.set_exposure(true, true, true);
        // The other two are as saved, so only the check runs.
        assert_eq!(hosting.save(&snapshot), Step::Run(vec![Effect::CheckReachability { site: "monokulo-pay".into(), going_public: true }]));
        // With another name the names go to the helper again even though the host is unchanged.
        assert!(hosting.add_alias("www.example.com", AliasMode::Redirect, &snapshot));
        let Step::Run(effects) = hosting.save(&snapshot) else { panic!("effects") };
        assert!(matches!(&effects[..], [Effect::SetPublicHost { then, .. }] if then.len() == 1), "{effects:?}");
    }

    #[test]
    fn going_public_without_a_provider_does_nothing_until_one_is_connected() {
        let (mut mock, _) = start();
        mock.connected.clear();
        mock.web.provider.clear();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        assert!(!snapshot.has_provider());
        let mut hosting = open(&snapshot, "wallet-relay").expect("a site");
        hosting.set_exposure(false, false, true);
        assert!(matches!(hosting.save(&snapshot), Step::Done(_)));
        assert!(mock.calls.iter().all(|call| !matches!(call, nodo_api::Request::SetExposure { .. })));

        // The provider page reports the domains its key controls.
        hosting.provider_connected(&["myshop.net".to_owned(), "example.org".to_owned()]);
        assert!(hosting.has_provider && hosting.draft.public);
        assert_eq!(hosting.host(), "relay.myshop.net");
    }

    #[test]
    fn a_refused_name_stops_before_the_check() {
        let (mut mock, snapshot) = start();
        let mut hosting = open(&snapshot, "moneropay").expect("a site");
        hosting.set_exposure(true, true, true);
        hosting.set_sub("Not A Name");
        let seen = follow(&mut mock, hosting.save(&snapshot));
        let [Step::Failed(notice)] = &seen[..] else { panic!("{seen:?}") };
        assert!(notice.text.starts_with("Not saved: "), "{}", notice.text);
        assert_eq!(exposure(&mut mock, "moneropay"), Exposure { local: true, onion: false, public: false }, "nothing else ran");
    }

    #[test]
    fn public_to_offline_needs_no_question_and_leaves() {
        let (mut mock, snapshot) = start();
        let mut hosting = open(&snapshot, "shop").expect("a site");
        hosting.set_exposure(false, false, false);
        let step = hosting.save(&snapshot);
        assert!(matches!(&step, Step::Run(effects) if matches!(&effects[..], [Effect::SetExposure { after: ExposureAfter::Back, confirmation: None, .. }])));
        assert_eq!(follow(&mut mock, step), [Step::Go(Dest::LeaveHosting { site: "shop".into() }), Step::Done(Notice::ok("Saved."))]);
        assert_eq!(exposure(&mut mock, "shop"), Exposure::default());
    }

    #[test]
    fn changing_the_host_of_a_public_site_is_refused_by_the_helper_and_nothing_else_runs() {
        let (mut mock, snapshot) = start();
        let mut hosting = open(&snapshot, "shop").expect("a site");
        hosting.set_sub("store");
        hosting.set_exposure(true, false, true);
        let step = hosting.save(&snapshot);
        // The names first; turning Tor off waits for them.
        let Step::Run(effects) = &step else { panic!("effects") };
        let [Effect::SetPublicHost { host, then, .. }] = &effects[..] else { panic!("{effects:?}") };
        assert_eq!(host, "store.example.com");
        assert!(matches!(&then[..], [Effect::SetExposure { after: ExposureAfter::Back, .. }]));
        let seen = follow(&mut mock, step);
        assert!(matches!(&seen[..], [Step::Failed(notice)] if notice.text.contains("before changing the host")), "{seen:?}");
        assert!(exposure(&mut mock, "shop").onion, "the exposure waited for the names and never ran");
    }

    #[test]
    fn a_name_change_alone_is_saved_and_the_page_left() {
        let (mut mock, snapshot) = start();
        // Not public, the internet stays unticked: names are not part of the draft at all.
        let mut hosting = open(&snapshot, "moneropay").expect("a site");
        hosting.set_sub("other");
        assert!(matches!(hosting.save(&snapshot), Step::Done(_)), "names only matter with the internet ticked");
        // A public site dropping one of its names.
        mock.web.sites[0].public_aliases = vec![PublicAlias { host: "www.example.com".into(), mode: AliasMode::Redirect }];
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let mut hosting = open(&snapshot, "shop").expect("a site");
        hosting.remove_alias("www.example.com");
        let seen = follow(&mut mock, hosting.save(&snapshot));
        assert_eq!(seen, [Step::Go(Dest::LeaveHosting { site: "shop".into() }), Step::Done(Notice::ok("Saved."))]);
        assert!(mock.web.sites[0].public_aliases.is_empty());
    }

    #[test]
    fn a_batch_on_the_way_to_public_stops_at_the_first_failure() {
        let (mut mock, snapshot) = start();
        // The owner confirmed, but the site's protections cannot be applied: it must not go public.
        mock.knobs.fail_public_access = true;
        let confirm = crate::flows::web::public::PublicConfirm::new(
            "monokulo-pay",
            crate::model::PublicPlan {
                url: "https://pay.example.com".into(),
                host: "pay.example.com".into(),
                dns_record: String::new(),
                firewall: String::new(),
            },
        );
        let effects = confirm.confirm("pay.example.com", &snapshot).expect("the host, exactly");
        let mut outcomes = Vec::new();
        run_batch(&mut mock, effects.clone(), |batch| outcomes.extend(batch));
        assert!(matches!(&outcomes[..], [Outcome::Failed(_)]), "{outcomes:?}");
        assert!(!exposure(&mut mock, "monokulo-pay").public);

        // The same effects one by one, without the batch's rule, would have gone public.
        mock.knobs.fail_public_access = false;
        for effect in effects {
            for outcome in execute_effect(&mut mock, effect) {
                assert!(!matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
            }
        }
        assert!(exposure(&mut mock, "monokulo-pay").public);
    }

    #[test]
    fn a_failed_exposure_on_the_way_to_public_never_reaches_the_check() {
        let (mut mock, snapshot) = start();
        let mut hosting = open(&snapshot, "moneropay").expect("a site");
        hosting.set_exposure(true, true, true);
        let Step::Run(mut effects) = hosting.save(&snapshot) else { panic!("effects") };
        // The site is gone by the time the batch runs, so its exposure cannot be set.
        mock.web.sites.retain(|site| site.package != "moneropay");
        if let [Effect::SetPublicHost { then, .. }] = &mut effects[..] {
            effects = std::mem::take(then);
        }
        assert!(matches!(&effects[..], [Effect::SetExposure { .. }, Effect::CheckReachability { .. }]), "{effects:?}");
        let mut outcomes = Vec::new();
        run_batch(&mut mock, effects, |batch| outcomes.extend(batch));
        assert!(outcomes.iter().any(|o| matches!(o, Outcome::ExposureSet { result: Err(_), .. })));
        assert!(outcomes.iter().all(|o| !matches!(o, Outcome::Reachability { .. })), "the check never ran: {outcomes:?}");
    }
}
