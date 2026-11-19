//! Every destination the core can name leads to a page this interface serves.

mod common;

use axum::http::StatusCode;
use common::App;
use nodo_ui_core::exec::load_snapshot;
use nodo_ui_core::flows::install::{Review, ReviewPurpose};
use nodo_ui_core::flows::settings::{ApplyFailed, DraftRow};
use nodo_ui_core::flows::updates::Batch;
use nodo_ui_core::flows::web::public::{Found, PublicConfirm};
use nodo_ui_core::flows::Dest;
use nodo_ui_core::mock::{sample_bundle_review, MockBackend};
use nodo_ui_core::model::{
    Document, InstallGate, InstallSnapshot, PackageId, PublicPlan, ScreenData, UpdateReport, UpdateResult,
};
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
        Dest::UpdatePassphrase(_) => "UpdatePassphrase",
        Dest::UpdateResults(_) => "UpdateResults",
        Dest::Settings(_) => "Settings",
        Dest::ApplyFailed(_) => "ApplyFailed",
        Dest::Document(_) => "Document",
        Dest::LeaveHosting => "LeaveHosting",
        Dest::Reachability(_) => "Reachability",
        Dest::PublicConfirm(_) => "PublicConfirm",
        Dest::HostingWithProvider { .. } => "HostingWithProvider",
        Dest::WentPublic => "WentPublic",
    }
}

/// How many variants `name` knows, kept beside it.
const VARIANTS: usize = 26;

/// Destinations whose own page is not built in this slice. Each is sent to the closest page that
/// exists, marked `TODO(page)` in `dest::url`; the entry goes when the page comes.
const NOT_BUILT_YET: [(&str, &str); 1] = [("Bundle", "/apps/get")];

fn every_destination() -> Vec<(&'static str, Dest)> {
    let mut mock = MockBackend::new();
    let snapshot = load_snapshot(&mut mock).expect("the mock answers");
    let reachability = mock.reachability("moneropay");
    let unreachable = mock.reachability("shop");
    let shared_address = mock.reachability("monokulo-pay");
    let form = mock.settings_form("moneropay").expect("moneropay has settings");
    let failed = ApplyFailed {
        package: "moneropay".into(),
        explanation: mock.explain_rollback("moneropay", "listen tcp :18081: bind: address already in use"),
        rows: vec![DraftRow { label: "API port".into(), from: "5000".into(), to: "18081".into(), blamed: true }],
    };
    let batch = Batch { names: vec!["vault".into()], approvals: Vec::new(), growing: vec!["vault: 256 MB to 512 MB".into()] };
    let report = UpdateReport {
        results: vec![UpdateResult { name: "tor".into(), applied: true, version: "0.5.0".into(), note: "Applied".into() }],
        failure: None,
    };
    let plan = PublicPlan { url: "https://pay.example.com".into(), host: "pay.example.com".into(), dns_record: String::new(), firewall: String::new() };
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
        ("UpdatePassphrase", Dest::UpdatePassphrase(batch)),
        ("UpdateResults", Dest::UpdateResults(report)),
        ("Settings", Dest::Settings(form)),
        ("ApplyFailed", Dest::ApplyFailed(failed)),
        ("LeaveHosting", Dest::LeaveHosting),
        ("Reachability", Dest::Reachability(Found::of("moneropay", true, &reachability))),
        ("Reachability with problems", Dest::Reachability(Found::of("shop", true, &unreachable))),
        ("Reachability behind CGNAT", Dest::Reachability(Found::of("monokulo-pay", true, &shared_address))),
        ("PublicConfirm", Dest::PublicConfirm(PublicConfirm::new("moneropay", plan))),
        ("HostingWithProvider", Dest::HostingWithProvider { domains: vec!["example.com".into()] }),
        ("WentPublic", Dest::WentPublic),
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
                // Those that are relative lead to the page that asked, or where the page that
                // can receive them says.
                "Back" | "Resume" | "HostingWithProvider" | "WentPublic" => assert_eq!(url, back),
                "LeaveHosting" => {
                    assert_eq!(url, "/web");
                    answer.assert_has("<h1>Sites online</h1>");
                }
                "Reachability" | "PublicConfirm" => {
                    assert_eq!(url, "/apps/moneropay/hosting/confirm");
                    answer.assert_has("Ready to put moneropay on the internet").assert_has("to confirm");
                }
                "Reachability with problems" => {
                    answer.assert_has("The internet can't reach your Nodo yet").assert_has("Port 80 is not reachable from outside");
                }
                "Reachability behind CGNAT" => {
                    answer.assert_has("shares one address between many homes").assert_has("100.64.0.0/10");
                }
                // Each of the others shows what it was sent with.
                "ConfirmSource" => {
                    answer.assert_has("monero-tools");
                }
                "KeyChange" => {
                    answer.assert_has("is now signed with a different key");
                }
                "Updates" => {
                    answer.assert_has("<h1>Updates</h1>");
                    // Only the app the owner came for is ticked.
                    assert_eq!(answer.body.matches(" checked>").count(), 1, "{}", answer.body);
                }
                "Review of an update" => {
                    answer.assert_has("Update moneropay to").assert_has("This version newly asks to");
                }
                "UpdatePassphrase" => {
                    answer.assert_has("vault: 256 MB to 512 MB").assert_has("autocomplete=\"current-password\"");
                }
                "UpdateResults" => {
                    answer.assert_has("1 update is installed").assert_has("tor 0.5.0");
                }
                "Settings" => {
                    answer.assert_has("<h1>Settings</h1>").assert_has("Daemon RPC");
                }
                "ApplyFailed" => {
                    answer.assert_has("didn't start with the new settings").assert_has("API port").assert_has("address already in use");
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
