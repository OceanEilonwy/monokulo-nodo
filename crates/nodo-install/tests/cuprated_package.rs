//! cuprated's RPC as the device configures it, from the recipes' own files.
//!
//! The base image ships `packages/cuprated-service/files/Cuprated.toml`, and the helper later
//! renders `packages/nodo-tools/files/cuprated/Cuprated.toml.in` from the user's settings. Both
//! must keep RPC restricted and on loopback, and must agree, so the first settings change never
//! moves RPC. The firewall (`cuprated.nft`) opens only the P2P port and has to agree too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nodo_manifest::{HealthCheck, Manifest, Protocol, Scope};
use nodo_settings::{load_settings, Installed, InstalledPackage, Limits};

fn packages() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(packages().join(path)).unwrap()
}

fn manifest() -> Manifest {
    Manifest::parse_system(&read("nodo-tools/files/cuprated/manifest.toml")).unwrap()
}

/// The template rendered with every setting at its default, as on a new device.
fn rendered_defaults() -> toml::Table {
    let settings = load_settings(&read("nodo-tools/files/cuprated/settings.lua"), Limits::default()).unwrap();
    let mut installed = Installed::default();
    installed.packages.insert(
        "cuprated".into(),
        InstalledPackage { manifest: manifest(), schema: settings.schema().clone(), values: BTreeMap::new() },
    );
    let rendered = installed.render_toml("cuprated", &read("nodo-tools/files/cuprated/Cuprated.toml.in")).unwrap();
    toml::from_str(&rendered.text).unwrap()
}

fn shipped() -> toml::Table {
    toml::from_str(&read("cuprated-service/files/Cuprated.toml")).unwrap()
}

fn rpc(config: &toml::Table, server: &str) -> toml::Table {
    config["rpc"][server].as_table().unwrap().clone()
}

fn assert_restricted_on_loopback(config: &toml::Table, which: &str) {
    let restricted = rpc(config, "restricted");
    assert_eq!(restricted["enable"].as_bool(), Some(true), "{which}: restricted RPC must be on");
    assert_eq!(restricted["address"].as_str(), Some("127.0.0.1"), "{which}: restricted RPC on loopback only");
    assert_eq!(restricted["port"].as_integer(), Some(18081), "{which}: the port apps and the helper use");
    assert_eq!(restricted["advertise"].as_bool(), Some(false), "{which}: never advertised to peers");
    let unrestricted = rpc(config, "unrestricted");
    assert_eq!(unrestricted["enable"].as_bool(), Some(false), "{which}: unrestricted RPC must be off");
}

#[test]
fn the_shipped_config_serves_only_restricted_rpc_on_loopback() {
    assert_restricted_on_loopback(&shipped(), "cuprated-service");
}

#[test]
fn the_helper_template_serves_only_restricted_rpc_on_loopback() {
    assert_restricted_on_loopback(&rendered_defaults(), "nodo-tools template");
}

/// The helper overwrites the shipped file on the first settings change. With default settings
/// that must change nothing at all, RPC included.
#[test]
fn the_template_with_defaults_is_the_shipped_config() {
    assert_eq!(rendered_defaults(), shipped());
}

#[test]
fn no_setting_can_move_rpc_onto_the_network() {
    let settings = load_settings(&read("nodo-tools/files/cuprated/settings.lua"), Limits::default()).unwrap();
    let schema = settings.schema();
    assert!(schema.setting("restricted_rpc").is_none(), "the old LAN RPC switch is gone");
    let template = read("nodo-tools/files/cuprated/Cuprated.toml.in");
    let table: toml::Table = toml::from_str(&template).unwrap();
    for server in ["restricted", "unrestricted"] {
        for (key, value) in rpc(&table, server) {
            if key != "port" {
                assert!(!value.to_string().contains("$("), "rpc.{server}.{key} must not come from a setting");
            }
        }
    }
}

/// The manifest declares the RPC port as local only, which is what the helper's generated rules
/// and health check use, and the firewall opens nothing but P2P inbound.
#[test]
fn the_manifest_and_firewall_keep_rpc_local() {
    let m = manifest();
    assert_eq!(m.health, Some(HealthCheck::Tcp { port: 18081 }));
    assert_eq!(m.network.inbound.len(), 1);
    let inbound = &m.network.inbound[0];
    assert_eq!((inbound.port, inbound.protocol, inbound.scope), (18081, Protocol::Tcp, Scope::Local));

    let nft = read("cuprated-service/files/cuprated.nft");
    let input = nft.split("chain input").nth(1).unwrap().split('}').next().unwrap();
    let accepts: Vec<&str> = input.lines().map(str::trim).filter(|l| l.ends_with("accept")).collect();
    assert_eq!(accepts, ["tcp dport 18080 accept"], "only P2P is opened inbound");
    assert!(!nft.contains("18081") && !nft.contains("18089"), "no RPC port in the firewall rules");
}
