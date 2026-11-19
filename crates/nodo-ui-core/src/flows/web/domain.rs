//! Domains and certificates: the email for expiry notices, the staging server, how local sites
//! are named, and the provider's separate certificate credentials when it needs them.
//!
//! The helper checks what is saved and its refusal is shown above the fields, in its words;
//! nothing is checked twice here. The credentials are secrets: they are taken out of their boxes
//! only when both are typed, and are never shown again.

use nodo_api::{AcmeSettingsInput, LocalNameCheck};

use super::super::{Notice, Step};
use crate::model::AcmeView;
use crate::secret::Input;
use crate::{Effect, Outcome};

/// A field of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Email,
    Staging,
    /// Local sites are named `site.local`, with the device's own CA.
    DotLocal,
    /// Local sites are named under the owner's domain, with public certificates.
    UnderDomain,
    Dns01First,
    Dns01Second,
}

impl Field {
    /// Fields that take typing rather than a choice.
    pub fn is_text(self) -> bool {
        matches!(self, Field::Email | Field::UnderDomain | Field::Dns01First | Field::Dns01Second)
    }
}

/// The fields the page has, in order: the certificate credentials only for a provider whose
/// certificate module needs its own.
pub fn fields(acme: &AcmeView) -> Vec<Field> {
    let mut order = vec![Field::Email, Field::Staging, Field::DotLocal, Field::UnderDomain];
    if !acme.dns01_fields.is_empty() {
        order.extend([Field::Dns01First, Field::Dns01Second]);
    }
    order
}

/// The name local sites would go under when the owner has not chosen one: `home.` and the domain.
/// Empty without a domain.
pub fn suggested_local_domain(acme: &AcmeView, domain: &str) -> String {
    match &acme.local_domain {
        Some(saved) => saved.clone(),
        None if domain.contains('.') => format!("home.{domain}"),
        None => String::new(),
    }
}

/// How certificates are validated, in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    /// Through the provider's DNS, so no port is needed.
    pub dns01: bool,
    pub method: String,
    /// What that asks of the owner's network.
    pub note: String,
}

pub fn validation(acme: &AcmeView) -> Validation {
    if acme.dns01 {
        Validation { dns01: true, method: acme.challenge.clone(), note: "No open ports needed.".to_owned() }
    } else {
        Validation {
            dns01: false,
            method: "HTTP-01 or TLS-ALPN-01".to_owned(),
            note: format!("Needs ports 80 and 443 forwarded. {}", acme.challenge),
        }
    }
}

/// The labels of the two certificate credentials, as the provider names them.
pub fn dns01_labels(acme: &AcmeView) -> Vec<String> {
    acme.dns01_fields.iter().map(|field| field.replace('_', " ")).collect()
}

/// Tests whether this network resolves the local names under the domain.
pub fn check_local_names() -> Vec<Effect> {
    vec![Effect::CheckLocalNames]
}

/// A save's result when the page is no longer shown: said as a notice.
pub fn apply(outcome: &Outcome) -> Step {
    match outcome {
        Outcome::AcmeSaved(Ok(())) => Step::Done(saved()),
        Outcome::AcmeSaved(Err(text)) => Step::Failed(Notice::error(text.clone())),
        _ => Step::Stay,
    }
}

fn saved() -> Notice {
    Notice::ok("Certificate settings saved and applied.")
}

/// The choices of the page. What is typed (the email, the name local sites go under, the
/// credentials) stays with the frontend and is handed to `save`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain {
    pub staging: bool,
    /// Local sites are named under the owner's domain instead of `.local`.
    pub under_domain: bool,
    /// The last test of the local names, or why it could not run.
    pub checks: Option<Result<Vec<LocalNameCheck>, String>>,
    /// The helper's refusal of the last save. It belongs above the fields.
    pub error: Option<String>,
}

impl Domain {
    pub fn new(acme: &AcmeView) -> Self {
        Self { staging: acme.staging, under_domain: acme.local_domain.is_some(), checks: None, error: None }
    }

    pub fn set_staging(&mut self, on: bool) {
        self.staging = on;
    }

    /// How local sites are named: under the owner's domain, or `.local`.
    pub fn set_under_domain(&mut self, under_domain: bool) {
        self.under_domain = under_domain;
    }

    /// Saves the page. An empty email is no email. The name local sites go under is only sent
    /// when that way of naming is chosen. The credentials are only sent, and only then taken out
    /// of their boxes, when both are typed: one alone changes nothing.
    pub fn save(&self, email: &str, local_domain: &str, dns01_first: &mut Input, dns01_second: &mut Input) -> Vec<Effect> {
        let text = |typed: &str| Some(typed.trim().to_owned()).filter(|text| !text.is_empty());
        let settings = AcmeSettingsInput {
            email: text(email),
            staging: self.staging,
            local_domain: if self.under_domain { text(local_domain) } else { None },
        };
        let dns01 = (!dns01_first.is_empty() && !dns01_second.is_empty())
            .then(|| (dns01_first.take_secret(), dns01_second.take_secret()));
        vec![Effect::SaveAcme { settings, dns01 }]
    }

    /// What a result means while the page is shown. A refused save is kept in `error`; a save
    /// that worked clears it, and a frontend then empties the credential boxes.
    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::AcmeSaved(Ok(())) => {
                self.error = None;
                Step::Done(saved())
            }
            Outcome::AcmeSaved(Err(text)) => {
                self.error = Some(text.clone());
                Step::Stay
            }
            Outcome::LocalNamesChecked(result) => {
                self.checks = Some(result.clone());
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
    use crate::mock::MockBackend;
    use crate::model::Snapshot;

    fn start() -> (MockBackend, Snapshot) {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        (mock, snapshot)
    }

    fn typed(text: &str) -> Input {
        let mut input = Input::secret();
        for c in text.chars() {
            input.push(c);
        }
        input
    }

    fn settings(effects: &[Effect]) -> (&AcmeSettingsInput, bool) {
        let [Effect::SaveAcme { settings, dns01 }] = effects else { panic!("{effects:?}") };
        (settings, dns01.is_some())
    }

    #[test]
    fn the_credential_fields_exist_only_for_a_provider_that_needs_them() {
        let (_, snapshot) = start();
        let mut acme = snapshot.web.acme.clone();
        acme.dns01_fields.clear();
        assert_eq!(fields(&acme), [Field::Email, Field::Staging, Field::DotLocal, Field::UnderDomain]);
        acme.dns01_fields = vec!["api_key".into(), "secret_api_key".into()];
        assert_eq!(fields(&acme)[4..], [Field::Dns01First, Field::Dns01Second]);
        assert_eq!(dns01_labels(&acme), ["api key", "secret api key"]);
        assert!(Field::Email.is_text() && Field::UnderDomain.is_text() && Field::Dns01Second.is_text());
        assert!(!Field::Staging.is_text() && !Field::DotLocal.is_text());
    }

    #[test]
    fn the_local_name_starts_as_the_saved_one_or_home_under_the_domain() {
        let mut acme = AcmeView::default();
        assert_eq!(suggested_local_domain(&acme, "example.com"), "home.example.com");
        assert_eq!(suggested_local_domain(&acme, ""), "");
        assert_eq!(suggested_local_domain(&acme, "localhost"), "", "not a domain");
        acme.local_domain = Some("lan.example.com".into());
        assert_eq!(suggested_local_domain(&acme, "example.com"), "lan.example.com");
        assert!(Domain::new(&acme).under_domain);
        acme.local_domain = None;
        assert!(!Domain::new(&acme).under_domain);
    }

    #[test]
    fn validation_is_said_with_what_it_needs() {
        let mut acme = AcmeView { dns01: true, challenge: "DNS-01 through Porkbun".into(), ..AcmeView::default() };
        let through_dns = validation(&acme);
        assert_eq!((through_dns.method.as_str(), through_dns.note.as_str()), ("DNS-01 through Porkbun", "No open ports needed."));
        acme.dns01 = false;
        acme.challenge = "GoDaddy needs its own credentials for DNS-01.".into();
        let ports = validation(&acme);
        assert!(!ports.dns01);
        assert_eq!(ports.method, "HTTP-01 or TLS-ALPN-01");
        assert_eq!(ports.note, "Needs ports 80 and 443 forwarded. GoDaddy needs its own credentials for DNS-01.");
    }

    #[test]
    fn saving_sends_what_is_chosen_and_only_whole_credentials() {
        let (_, snapshot) = start();
        let mut page = Domain::new(&snapshot.web.acme);
        page.set_staging(true);
        page.set_under_domain(false);
        let (mut first, mut second) = (typed("key"), Input::secret());
        let effects = page.save("  owner@example.com ", "home.example.com", &mut first, &mut second);
        let (sent, credentials) = settings(&effects);
        assert_eq!(sent.email.as_deref(), Some("owner@example.com"));
        assert!(sent.staging);
        assert_eq!(sent.local_domain, None, "the name is only sent when local sites go under the domain");
        assert!(!credentials, "one credential alone is not sent");
        assert!(!first.is_empty(), "and stays in its box");

        page.set_under_domain(true);
        let mut second = typed("secret");
        let effects = page.save("   ", " home.example.com ", &mut first, &mut second);
        let (sent, credentials) = settings(&effects);
        assert_eq!(sent.email, None, "an empty email is no email");
        assert_eq!(sent.local_domain.as_deref(), Some("home.example.com"));
        assert!(credentials);
        assert!(first.is_empty() && second.is_empty(), "both were taken out of their boxes");
        // Under the domain with no name typed: nothing to go under.
        let effects = page.save("", "  ", &mut first, &mut second);
        assert_eq!(settings(&effects).0.local_domain, None);
    }

    #[test]
    fn a_save_is_applied_or_its_refusal_kept_above_the_fields() {
        let (mut mock, snapshot) = start();
        let mut page = Domain::new(&snapshot.web.acme);
        page.set_staging(!snapshot.web.acme.staging);
        let (mut first, mut second) = (Input::secret(), Input::secret());
        let effects = page.save("owner@example.com", "", &mut first, &mut second);
        let outcomes = execute_effect(&mut mock, effects.into_iter().next().expect("one effect"));
        assert_eq!(page.apply(&outcomes[0]), Step::Done(Notice::ok("Certificate settings saved and applied.")));
        let after = load_snapshot(&mut mock).expect("the mock answers");
        assert_eq!(after.web.acme.staging, !snapshot.web.acme.staging);
        assert_eq!(after.web.acme.email, "owner@example.com");

        let refused = Outcome::AcmeSaved(Err("the email address is not valid".into()));
        assert_eq!(page.apply(&refused), Step::Stay);
        assert_eq!(page.error.as_deref(), Some("the email address is not valid"));
        assert_eq!(page.apply(&Outcome::AcmeSaved(Ok(()))), Step::Done(Notice::ok("Certificate settings saved and applied.")));
        assert_eq!(page.error, None, "a save that worked clears the refusal");

        // The owner left the page before the answer came: it is said as a notice.
        assert_eq!(apply(&refused), Step::Failed(Notice::error("the email address is not valid")));
        assert!(matches!(apply(&Outcome::AcmeSaved(Ok(()))), Step::Done(_)));
        assert_eq!(apply(&Outcome::LocalNamesChecked(Ok(Vec::new()))), Step::Stay);
    }

    #[test]
    fn the_local_names_test_is_kept_for_the_page() {
        let (mut mock, snapshot) = start();
        let mut page = Domain::new(&snapshot.web.acme);
        assert_eq!(page.checks, None);
        let outcomes = execute_effect(&mut mock, check_local_names().remove(0));
        assert_eq!(page.apply(&outcomes[0]), Step::Stay);
        assert!(matches!(page.checks, Some(Ok(_))), "{:?}", page.checks);
        assert_eq!(page.apply(&Outcome::LocalNamesChecked(Err("No resolver answered.".into()))), Step::Stay);
        assert_eq!(page.checks, Some(Err("No resolver answered.".into())));
    }
}
