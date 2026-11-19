//! Documents: text the owner reads on top of whatever page they are on, and what can be copied
//! or saved for them.
//!
//! The documents the helper writes are asked for with `Effect::ShowDocument` by the feature they
//! belong to (an app's versions, a source's packages, an install's log). Here is the one the
//! device's state alone gives, what a loaded document means, and the reports. Key help is each
//! frontend's own.

use super::{Dest, Step};
use crate::model::{Document, Snapshot};
use crate::{Effect, Outcome};

/// About this Nodo: what the device reports about itself.
pub fn about(snapshot: &Snapshot) -> Document {
    let mut lines: Vec<String> = snapshot.overview.device.iter().map(|(k, v)| format!("{k:<16} {v}")).collect();
    if let Some(note) = &snapshot.system_note {
        lines.push(String::new());
        lines.push(note.clone());
    }
    Document { title: "About this Nodo".to_owned(), lines }
}

/// A document that loaded is shown.
pub fn apply(outcome: &Outcome) -> Step {
    match outcome {
        Outcome::DocumentLoaded(document) => Step::Go(Dest::Document(document.clone())),
        _ => Step::Stay,
    }
}

/// Saves a source's log where the owner can fetch it. The helper answers with where it put it,
/// as one of the outcomes every page shares (`flows::apply`).
pub fn log_report(source: &str) -> Vec<Effect> {
    vec![Effect::CopyReport(format!("{source} log"))]
}

/// Everything on an app's setup page that can be copied, in the order drawn: values, QR codes'
/// data and the DNS records' values, each with its label.
pub fn copy_targets(view: &nodo_api::OwnerPageView) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for step in &view.steps {
        for item in &step.items {
            match item {
                nodo_api::OwnerItem::Value { label, value } => out.push((label.clone(), value.clone())),
                nodo_api::OwnerItem::Qr { label, data } => out.push((label.clone(), data.clone())),
                nodo_api::OwnerItem::Site(site) => {
                    if let Some(host) = &site.host {
                        out.push(("Address".to_owned(), host.clone()));
                    }
                    for record in &site.records {
                        out.push((format!("{} record {}", record.kind, record.name), record.value.clone()));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::flows;
    use crate::mock::MockBackend;
    use crate::model::DocKind;

    #[test]
    fn about_lists_what_the_device_reports_and_its_note_last() {
        let mut mock = MockBackend::new();
        let mut snapshot = load_snapshot(&mut mock).expect("the mock answers");
        snapshot.overview.device = vec![("Model".to_owned(), "Nodo".to_owned()), ("Serial".to_owned(), "42".to_owned())];
        snapshot.system_note = None;
        let plain = about(&snapshot);
        assert_eq!(plain.title, "About this Nodo");
        assert_eq!(plain.lines, ["Model            Nodo", "Serial           42"]);

        snapshot.system_note = Some("The system went back to its previous version.".to_owned());
        let noted = about(&snapshot);
        assert_eq!(noted.lines[2..], [String::new(), "The system went back to its previous version.".to_owned()]);
    }

    #[test]
    fn a_loaded_document_is_shown() {
        let mut mock = MockBackend::new();
        let outcomes = execute_effect(&mut mock, Effect::ShowDocument(DocKind::Versions("moneropay".into())));
        let [Step::Go(Dest::Document(document))] = &outcomes.iter().map(apply).collect::<Vec<_>>()[..] else {
            panic!("{outcomes:?}")
        };
        assert!(!document.title.is_empty());
        assert_eq!(apply(&Outcome::Info("something else".into())), Step::Stay);
    }

    #[test]
    fn a_report_names_what_it_is_about_and_the_answer_is_said() {
        let mut mock = MockBackend::new();
        let effects = log_report("moneropay");
        assert_eq!(effects, [Effect::CopyReport("moneropay log".into())]);
        let outcomes: Vec<Outcome> = effects.into_iter().flat_map(|effect| execute_effect(&mut mock, effect)).collect();
        assert!(matches!(flows::apply(&outcomes[0]), Step::Done(_) | Step::Failed(_)), "{outcomes:?}");
    }

    #[test]
    fn a_setup_pages_values_codes_and_records_can_be_copied_in_order() {
        use nodo_api::{DnsRecord, OwnerCheck, OwnerCheckState, OwnerItem, OwnerPageView, OwnerStep, OwnerStepState, SiteFacts};
        let check = OwnerCheck { label: "DNS".into(), state: OwnerCheckState::Waiting, detail: None };
        let record = |kind: &str, value: &str| DnsRecord { kind: kind.into(), name: "pay".into(), zone: "example.com".into(), value: value.into() };
        let site = SiteFacts {
            package: "moneropay".into(),
            site: "moneropay".into(),
            host: Some("pay.example.com".into()),
            suggested: false,
            public: true,
            records: vec![record("A", "203.0.113.7"), record("AAAA", "2001:db8::7")],
            automatic_dns: None,
            checks: vec![check.clone()],
        };
        let step = |items| OwnerStep { title: "A step".into(), state: OwnerStepState::Todo, items };
        let view = OwnerPageView {
            package: "moneropay".into(),
            title: "Setup".into(),
            intro: None,
            steps: vec![
                step(vec![
                    OwnerItem::Text("Read this first.".into()),
                    OwnerItem::Value { label: "Admin password".into(), value: "s3cret".into() },
                    OwnerItem::Check(check.clone()),
                ]),
                step(vec![
                    OwnerItem::Qr { label: "Pairing code".into(), data: "nodo://pair/abc".into() },
                    OwnerItem::Site(site.clone()),
                    OwnerItem::Site(SiteFacts { host: None, records: Vec::new(), ..site }),
                    OwnerItem::Backup { check },
                ]),
            ],
        };
        // Through the backend, as a frontend gets it.
        let mut mock = MockBackend::new();
        mock.owner_views.insert("moneropay".to_owned(), ("Setup".to_owned(), view));
        let passphrase = crate::secret::Secret::new(crate::mock::MOCK_PASSPHRASE);
        let outcomes = execute_effect(&mut mock, Effect::LoadOwnerPage { package: "moneropay".into(), passphrase });
        let [Outcome::OwnerPageLoaded { result: Ok(loaded), .. }] = &outcomes[..] else { panic!("{outcomes:?}") };

        let pair = |label: &str, value: &str| (label.to_owned(), value.to_owned());
        assert_eq!(
            copy_targets(loaded),
            [
                pair("Admin password", "s3cret"),
                pair("Pairing code", "nodo://pair/abc"),
                pair("Address", "pay.example.com"),
                pair("A record pay", "203.0.113.7"),
                pair("AAAA record pay", "2001:db8::7"),
            ]
        );
    }
}
