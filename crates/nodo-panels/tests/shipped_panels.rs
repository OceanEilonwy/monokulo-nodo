//! The panels this repository ships, run on data shaped like what their services return.

use nodo_manifest::{Manifest, PanelSource};
use nodo_panels::{run_panel, Status};
use serde_json::json;

const CUPRATED_MANIFEST: &str = include_str!("../../../packages/nodo-tools/files/cuprated/manifest.toml");
const CUPRATED_SYNC: &str = include_str!("../../../packages/nodo-tools/files/cuprated/panels/sync.lua");

#[test]
fn cuprateds_manifest_offers_a_valid_panel() {
    // Base manifests are parsed without validation on the device, and cuprated's uses system paths
    // only base packages may. Its panels must still pass the rules any package's panels meet.
    let manifest = Manifest::parse_system(CUPRATED_MANIFEST).unwrap();
    let panel_issues: Vec<_> = manifest.validate().into_iter().filter(|i| i.field.starts_with("panels")).collect();
    assert!(panel_issues.is_empty(), "{panel_issues:?}");
    let panel = &manifest.panels[0];
    assert_eq!(panel.id, "sync");
    assert_eq!(
        panel.source,
        PanelSource::Jsonrpc { url: "http://127.0.0.1:18081/json_rpc".into(), method: "get_info".into() }
    );
}

/// cuprated 0.1.0's get_info: peer counts are always 0, target_height is 0 once caught up.
fn get_info(height: u64, target: u64) -> serde_json::Value {
    json!({
        "status": "OK", "height": height, "target_height": target, "busy_syncing": target > 0,
        "synchronized": target == 0, "incoming_connections_count": 0, "outgoing_connections_count": 0,
        "database_size": 218_000_000_000u64, "tx_pool_size": 41, "version": "0.1.0", "untrusted": false
    })
}

#[test]
fn cuprateds_panel_while_syncing() {
    let out = run_panel(CUPRATED_SYNC, "cuprated/sync", &get_info(2_121_400, 3_478_000), &[58.0, 60.0]).unwrap();
    assert_eq!(out.big.as_deref(), Some("60%"));
    assert_eq!(out.status, Some(Status::Busy));
    assert_eq!(out.sample, Some(60.0));
    assert_eq!(out.series, vec![58.0, 60.0]);
    assert!(out.rows.contains(&("height".into(), "2,121,400 / 3,478,000".into())), "{:?}", out.rows);
    assert!(out.rows.contains(&("behind".into(), "1,356,600 blocks".into())));
    assert!(!out.rows.iter().any(|(label, _)| label == "peers"), "cuprated reports no real peer counts yet");
}

#[test]
fn cuprateds_panel_once_caught_up_and_while_starting() {
    let out = run_panel(CUPRATED_SYNC, "cuprated/sync", &get_info(3_478_120, 0), &[]).unwrap();
    assert_eq!(out.big.as_deref(), Some("3,478,120"));
    assert_eq!(out.unit.as_deref(), Some("blocks"));
    assert_eq!(out.status, Some(Status::Ok));
    assert_eq!(out.note.as_deref(), Some("up to date with its peers"));
    assert!(out.rows.contains(&("database".into(), "218 GB".into())));
    let starting = run_panel(CUPRATED_SYNC, "cuprated/sync", &json!({ "status": "OK" }), &[]).unwrap();
    assert_eq!(starting.big.as_deref(), Some("--"));
    assert_eq!(starting.status, Some(Status::Idle));
}
