//! The TUI's `HelperBackend`, talking to this helper's real request handler over a socket, passes
//! the same scenarios as the `MockBackend` the TUI's tests drive (`nodo_tui::contract`). Together
//! with the TUI's test that the mock's capabilities equal `HelperBackend::CAPABILITIES`, a TUI
//! test cannot pass on behaviour the device does not have.

use std::os::unix::net::UnixListener;
use std::sync::Arc;

use nodo_tui::contract;
use nodo_tui::helper_backend::HelperBackend;
use nodo_tui::model::{InstallOutcome, PackageId, ScreenData};

use super::{setup, FakePackages, FEED};
use crate::helper::AllowedUids;
use crate::server::serve;

/// The TUI's backend, connected to a helper from `setup` that serves on a socket in `dir`, and the
/// scripted feed behind it.
fn connected(dir: &tempfile::TempDir) -> (HelperBackend, FakePackages) {
    let (mut h, packages) = setup();
    h.allowed = AllowedUids::new([rustix::process::geteuid().as_raw()]);
    let path = dir.path().join("helper.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let h = Arc::new(h);
    std::thread::spawn(move || serve(listener, h));
    (HelperBackend::connect(&path).unwrap(), packages)
}

#[test]
fn the_tui_installs_through_the_helper_as_the_mock_does() {
    let dir = tempfile::tempdir().unwrap();
    let (mut backend, _packages) = connected(&dir);
    let probe = backend.probe_feed(FEED).unwrap();
    backend.trust_feed(&probe).unwrap();
    let done = contract::install(&mut backend, &PackageId::new(&probe.name, "demo"));
    assert_eq!(done.outcome, InstallOutcome::Succeeded, "{done:?}");
    assert!(!done.can_cancel);
}

/// A proxy that holds example.com, as the mock's one connected domain.
struct OwnsExampleCom;

impl crate::exposure::ExposureApplier for OwnsExampleCom {
    fn apply(&self, _: &crate::exposure::WebInput) -> Result<(), String> {
        Ok(())
    }
    fn owned_domains(&self) -> Vec<String> {
        vec!["example.com".to_owned()]
    }
}

#[test]
fn the_tui_saves_a_sites_other_names_through_the_helper_as_the_mock_does() {
    let dir = tempfile::tempdir().unwrap();
    let (h, _packages) = setup();
    h.sys.add_manifest(crate::fake::SHOP);
    let mut h = crate::helper::Helper { exposure: Box::new(OwnsExampleCom), ..h };
    h.allowed = AllowedUids::new([rustix::process::geteuid().as_raw()]);
    let path = dir.path().join("helper.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let h = Arc::new(h);
    std::thread::spawn(move || serve(listener, h));
    let mut backend = HelperBackend::connect(&path).unwrap();
    backend.set_public_host("shop/single", "one.example.com", &[], &[]).unwrap();
    contract::site_aliases(&mut backend, "shop/web", "example.com", "one.example.com");
    contract::public_site_gains_confirmed_names(&mut backend, "shop/web", "example.com");
}

/// demo keeps its data on the data drive, and its update moves the data to a newer layout, which
/// needs room on the drive for a copy that is not there.
#[test]
fn the_tui_hears_a_refused_update_as_the_mock_does() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, packages) = setup();
    h.sys.put("/s/bindings.toml", b"data_drive = [\"demo\"]\n");
    h.sys.set_mounted("/media/monero");
    h.allowed = AllowedUids::new([rustix::process::geteuid().as_raw()]);
    let path = dir.path().join("helper.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let h = Arc::new(h);
    let served = Arc::clone(&h);
    std::thread::spawn(move || serve(listener, served));
    let mut backend = HelperBackend::connect(&path).unwrap();
    let probe = backend.probe_feed(FEED).unwrap();
    backend.trust_feed(&probe).unwrap();
    let done = contract::install(&mut backend, &PackageId::new(&probe.name, "demo"));
    assert_eq!(done.outcome, InstallOutcome::Succeeded, "{done:?}");
    let next = super::manifest("\n[storage]\ndata_layout = 2\n");
    packages.publish("2.0_1", &next);
    h.sys.put("/opt/nodo/apps/demo/manifest.toml", next.as_bytes());
    assert_eq!(backend.check_updates().apps, Ok(()));
    packages.0.lock().unwrap().free = Some(100 << 20);
    contract::refused_update_changes_nothing(&mut backend, "demo");
}

#[test]
fn the_tui_hears_the_helper_refuse_what_the_mock_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let (mut backend, _packages) = connected(&dir);
    contract::missing_sources_fail(&mut backend);
    contract::features_match_capabilities(&mut backend, "demo");
}

#[test]
fn the_tui_names_a_source_the_helper_could_not_read_as_the_mock_does() {
    let dir = tempfile::tempdir().unwrap();
    let (mut backend, packages) = connected(&dir);
    let probe = backend.probe_feed(FEED).unwrap();
    backend.trust_feed(&probe).unwrap();
    // The feed stops answering after it was added, as a feed server that went away does.
    packages.0.lock().unwrap().feeds.clear();
    contract::unreadable_source_is_named(&mut backend, &probe.name);
}
