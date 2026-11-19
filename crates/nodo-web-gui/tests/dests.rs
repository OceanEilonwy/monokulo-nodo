//! Every destination the core can name leads to a page this interface serves.

mod common;

use axum::http::StatusCode;
use common::App;
use nodo_ui_core::exec::load_snapshot;
use nodo_ui_core::flows::install::{Review, ReviewPurpose};
use nodo_ui_core::flows::Dest;
use nodo_ui_core::mock::{sample_bundle_review, MockBackend};
use nodo_ui_core::model::{Document, InstallGate, InstallSnapshot, PackageId, ScreenData};
use nodo_web_gui::dest;

/// The variant's name. The match is exhaustive, so a destination the core gains does not compile
/// here until it is listed in `every_destination` below (and mapped in `dest::url`).
fn name(dest: &Dest) -> &'static str {
    match dest {
        Dest::Back => "Back",
        Dest::Apps => "Apps",
        Dest::Updates { .. } => "Updates",
        Dest::Unlock { .. } => "Unlock",
        Dest::Resume => "Resume",
        Dest::GetApps { .. } => "GetApps",
        Dest::ConfirmSource(_) => "ConfirmSource",
        Dest::KeyChange(_) => "KeyChange",
        Dest::Sources { .. } => "Sources",
        Dest::AppPage(_) => "AppPage",
        Dest::Detail(_) => "Detail",
        Dest::Review(_) => "Review",
        Dest::Bundle(_) => "Bundle",
        Dest::Rejected(_) => "Rejected",
        Dest::MissingCapability(_) => "MissingCapability",
        Dest::Installing(_) => "Installing",
        Dest::ApplyUpdates { .. } => "ApplyUpdates",
        Dest::Document(_) => "Document",
    }
}

/// How many variants `name` knows, kept beside it.
const VARIANTS: usize = 18;

/// Destinations whose own page is not built in this slice. Each is sent to the closest page that
/// exists, marked `TODO(page)` in `dest::url`; the entry goes when the page comes.
const NOT_BUILT_YET: [(&str, &str); 4] =
    [("Updates", "/system"), ("Review of an update", "/system"), ("Bundle", "/apps/get"), ("ApplyUpdates", "/system")];

fn every_destination() -> Vec<(&'static str, Dest)> {
    let mut mock = MockBackend::new();
    let snapshot = load_snapshot(&mut mock).expect("the mock answers");
    let feed_of = |name: &str| snapshot.catalog.iter().find(|pkg| pkg.id.name == name).map(|pkg| pkg.id.clone()).expect("on offer");
    let gate = |name: &str| mock.install_gate(&feed_of(name));
    let InstallGate::Review(review) = gate("moneropay") else { panic!("moneropay is reviewed") };
    let InstallGate::Rejected(rejection) = gate("fast-sync-helper") else { panic!("fast-sync-helper is rejected") };
    let InstallGate::MissingCapability(capability) = gate("zmq-indexer") else { panic!("zmq-indexer lacks a capability") };
    let moneropay = PackageId::new("official", "moneropay");
    let update = ReviewPurpose::Update { batch: vec!["moneropay".into()], pending_reviews: Vec::new(), approvals: Vec::new() };
    let all = vec![
        ("Back", Dest::Back),
        ("Apps", Dest::Apps),
        ("Updates", Dest::Updates { only: Some("moneropay".into()) }),
        ("Unlock", Dest::Unlock { resume: None }),
        ("Resume", Dest::Resume),
        ("GetApps", Dest::GetApps { source: Some("monokulo".into()) }),
        ("GetApps", Dest::GetApps { source: None }),
        ("ConfirmSource", Dest::ConfirmSource(mock.probe_feed("https://github.com/example/monero-tools-feed").expect("a source"))),
        ("KeyChange", Dest::KeyChange(mock.key_change("old-tools").expect("its key changed"))),
        ("Sources", Dest::Sources { select: Some("community".into()) }),
        ("AppPage", Dest::AppPage("tor".into())),
        ("Detail", Dest::Detail(mock.package_detail(&moneropay).expect("details"))),
        ("Review", Dest::Review(Review::new(review.clone(), ReviewPurpose::Install))),
        ("Review of an update", Dest::Review(Review::new(review, update))),
        ("Bundle", Dest::Bundle(Box::new(sample_bundle_review()))),
        ("Rejected", Dest::Rejected(rejection)),
        ("MissingCapability", Dest::MissingCapability(capability)),
        ("Installing", Dest::Installing(InstallSnapshot::starting(&moneropay, "2.7.0"))),
        ("ApplyUpdates", Dest::ApplyUpdates { names: vec!["moneropay".into()], approvals: Vec::new() }),
        ("Document", Dest::Document(Document { title: "A report".into(), lines: vec!["one line".into()] })),
    ];
    let mut seen: Vec<&str> = all.iter().map(|(_, dest)| name(dest)).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), VARIANTS, "a variant of Dest is missing from the list: {seen:?}");
    all
}

#[tokio::test]
async fn every_destination_leads_to_a_page_that_is_served() {
    let app = App::new();
    let mut browser = app.login().await;
    let session = app.session(&browser).await;
    let back = "/system/keys";
    for (label, destination) in every_destination() {
        // As `Ctx::respond` does: keep what the destination carries, then go to its URL.
        let url = dest::land(&destination, back, &mut session.inner().stash);
        assert!(dest::local_path(&url).is_some(), "{label} leads off this origin: {url}");
        let answer = browser.get(&url).await;
        assert_eq!(answer.status, StatusCode::OK, "{label} leads to {url}, which answered {}", answer.location());
        answer.assert_has("<main id=\"main\">");

        match NOT_BUILT_YET.iter().find(|(name, _)| *name == label) {
            Some((_, stand_in)) => assert_eq!(url, *stand_in, "{label} has its own page now: take it off NOT_BUILT_YET"),
            None => match label {
                // The two that are relative lead to the page that asked.
                "Back" | "Resume" => assert_eq!(url, back),
                // Each of the others shows what it was sent with.
                "ConfirmSource" => {
                    answer.assert_has("monero-tools");
                }
                "KeyChange" => {
                    answer.assert_has("is now signed with a different key");
                }
                "Review" => {
                    answer.assert_has("Install moneropay?");
                }
                "Rejected" => {
                    answer.assert_has("can't be installed");
                }
                "MissingCapability" => {
                    answer.assert_has("needs a different Monero node first");
                }
                "Installing" => {
                    answer.assert_has("Installing moneropay");
                }
                "Document" => {
                    answer.assert_has("one line").assert_has(&format!("href=\"{back}\""));
                }
                "Detail" => {
                    answer.assert_has("Payment gateway for Monero");
                }
                "AppPage" => {
                    answer.assert_has("<h1>tor</h1>");
                }
                "GetApps" => {
                    answer.assert_has("Get apps");
                }
                "Apps" => {
                    answer.assert_has("<h1>Installed</h1>");
                }
                "Sources" => {
                    answer.assert_has("<h1>Sources</h1>");
                }
                "Unlock" => {
                    answer.assert_has("<title>Home");
                }
                other => panic!("{other} is checked nowhere"),
            },
        }
    }
}
