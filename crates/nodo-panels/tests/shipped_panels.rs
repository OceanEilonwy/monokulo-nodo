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

const PUSH_RELAY_MANIFEST: &str = include_str!("../../../packages/push-relay/files/manifest.toml");
const PUSH_RELAY_PANEL: &str = include_str!("../../../packages/push-relay/files/panels/relay.lua");

/// monokulo-push-relay's GET /status, as crates/push-relay/tests/relay_over_http.rs shows it.
fn relay_status(stored: Option<u64>, apns: &str, fcm: &str, last: Option<&str>) -> serde_json::Value {
    json!({
        "service": "monokulo-push-relay", "version": "0.1.0", "commit": "4e3c6042c312b68020e27f22fdb8d954e5df4371",
        "started_at": 1_794_000_000, "listen": "127.0.0.1:8444", "public_uri": "https://push.example.org",
        "apps": ["monokular"], "handles": { "stored": stored },
        "platforms": { "apns": apns, "fcm": fcm },
        "last_delivery": last.map(|outcome| json!({ "at": 1_794_000_100, "outcome": outcome })),
    })
}

#[test]
fn the_push_relays_manifest_and_panel() {
    let manifest = Manifest::parse(&PUSH_RELAY_MANIFEST.replace("@VERSION@", "0.1.0")).unwrap();
    assert_eq!(manifest.panels[0].source, PanelSource::Http { url: "http://127.0.0.1:8446/status".into() });

    let out = run_panel(PUSH_RELAY_PANEL, "push-relay/relay", &relay_status(Some(12), "ready", "ready", Some("sent")), &[]).unwrap();
    assert_eq!(out.big.as_deref(), Some("12"));
    assert_eq!(out.unit.as_deref(), Some("handles"));
    assert_eq!(out.status, Some(Status::Ok));
    assert!(out.rows.contains(&("last push".into(), "sent".into())), "{:?}", out.rows);

    let fresh = run_panel(PUSH_RELAY_PANEL, "push-relay/relay", &relay_status(Some(1), "ready", "not-configured", None), &[]).unwrap();
    assert_eq!(fresh.unit.as_deref(), Some("handle"));
    assert!(fresh.rows.contains(&("FCM".into(), "not set up".into())));
    assert_eq!(fresh.status, Some(Status::Ok));

    let bare = run_panel(PUSH_RELAY_PANEL, "push-relay/relay", &relay_status(Some(0), "not-configured", "not-configured", None), &[]).unwrap();
    assert_eq!(bare.status, Some(Status::Warn));
    assert_eq!(bare.note.as_deref(), Some("no APNs or FCM credentials set"));

    let busy = run_panel(PUSH_RELAY_PANEL, "push-relay/relay", &relay_status(None, "ready", "ready", Some("unavailable")), &[]).unwrap();
    assert_eq!(busy.big.as_deref(), Some("--"));
    assert_eq!(busy.status, Some(Status::Warn));
}

const BELL_MANIFEST: &str = include_str!("../../../packages/bell/files/manifest.toml");
const BELL_PANEL: &str = include_str!("../../../packages/bell/files/panels/bell.lua");

/// monokulo-bell's GET /status, as crates/monokulo-bell/tests/bell_over_http.rs shows it.
fn bell_status(stored: Option<u64>, clover_na: &str, last: Option<&str>) -> serde_json::Value {
    json!({
        "service": "monokulo-bell", "version": "0.1.0", "commit": null, "started_at": 1_794_000_000,
        "listen": "127.0.0.1:8445", "public_uri": "https://bell.example.org",
        "registrations": { "stored": stored },
        "points_of_sale": {
            "square": { "production": "ready", "sandbox": "off" },
            "clover": { "north_america": clover_na, "europe": "off", "latin_america": "off", "sandbox": "off" },
        },
        "last_ding": last.map(|outcome| json!({ "at": 1_794_000_100, "outcome": outcome })),
    })
}

#[test]
fn the_bells_manifest_and_panel() {
    let manifest = Manifest::parse(&BELL_MANIFEST.replace("@VERSION@", "0.1.0")).unwrap();
    assert_eq!(manifest.panels[0].source, PanelSource::Http { url: "http://127.0.0.1:8447/status".into() });

    let out = run_panel(BELL_PANEL, "bell/bell", &bell_status(Some(3), "ready", Some("sent")), &[]).unwrap();
    assert_eq!(out.big.as_deref(), Some("3"));
    assert_eq!(out.unit.as_deref(), Some("tills"));
    assert_eq!(out.status, Some(Status::Ok));
    assert!(out.rows.contains(&("serving".into(), "Square, Clover NA".into())), "{:?}", out.rows);
    assert!(out.rows.contains(&("last ding".into(), "sent".into())));

    let fresh = run_panel(BELL_PANEL, "bell/bell", &bell_status(Some(0), "off", None), &[]).unwrap();
    assert_eq!(fresh.status, Some(Status::Idle));
    assert!(fresh.rows.contains(&("serving".into(), "Square".into())));

    let failed = run_panel(BELL_PANEL, "bell/bell", &bell_status(Some(1), "off", Some("failed")), &[]).unwrap();
    assert_eq!(failed.unit.as_deref(), Some("till"));
    assert_eq!(failed.status, Some(Status::Warn));

    let mut all = bell_status(None, "ready", None);
    all["points_of_sale"] = json!({
        "square": { "production": "ready", "sandbox": "ready" },
        "clover": { "north_america": "ready", "europe": "ready", "latin_america": "ready", "sandbox": "ready" },
    });
    let all = run_panel(BELL_PANEL, "bell/bell", &all, &[]).unwrap();
    assert!(all.rows.contains(&("serving".into(), "6 points of sale".into())), "{:?}", all.rows);
    assert_eq!(all.big.as_deref(), Some("--"));
}
