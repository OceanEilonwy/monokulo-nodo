//! The mock the other tests drive must do what the device does. The same scenarios run against
//! `HelperBackend` and the real helper in nodo-helper's `tui_contract_tests`.

use nodo_tui::contract;
use nodo_tui::helper_backend::HelperBackend;
use nodo_tui::mock::MockBackend;
use nodo_tui::model::*;

/// The guard that keeps the two in step: raising a capability in one and not the other fails here.
#[test]
fn the_mock_can_do_what_the_helper_can_and_no_more() {
    assert_eq!(MockBackend::new().capabilities(), HelperBackend::CAPABILITIES);
    assert_eq!(MockBackend::with_missing_drive().capabilities(), HelperBackend::CAPABILITIES);
    assert_eq!(Capabilities::default(), HelperBackend::CAPABILITIES, "a screen that has not loaded assumes the least");
}

#[test]
fn the_mock_installs_as_the_helper_does() {
    let mut mock = MockBackend::new();
    let done = contract::install(&mut mock, &PackageId::new("community", "node-exporter"));
    assert_eq!(done.outcome, InstallOutcome::Succeeded);
    let failed = contract::install(&mut mock, &PackageId::new("community", "paywall-relay"));
    assert!(matches!(failed.outcome, InstallOutcome::Failed(_)), "{failed:?}");
}

/// The test knobs keep to the same contract for what they claim.
#[test]
fn the_test_knobs_keep_the_contract_for_what_they_claim() {
    let mut mock = MockBackend::with_stepped_installs();
    let done = contract::install(&mut mock, &PackageId::new("community", "node-exporter"));
    assert_eq!(done.outcome, InstallOutcome::Succeeded);
    mock.knobs.capabilities.copy_report = true;
    mock.knobs.capabilities.site_password = true;
    contract::features_match_capabilities(&mut mock, "shop");
}

#[test]
fn the_mock_refuses_what_the_helper_refuses() {
    let mut mock = MockBackend::new();
    contract::missing_sources_fail(&mut mock);
    contract::features_match_capabilities(&mut mock, "shop");
}
