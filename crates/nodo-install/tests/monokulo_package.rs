//! The Monokulo recipe's own files, checked with the same code the device uses.
//!
//! The package is built by xbps-src, but its manifest, settings and launcher are plain files in
//! `packages/monokulo/files`, so they are tested here without a build.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use nodo_install::{Permission, Permissions};
use nodo_manifest::Manifest;
use nodo_settings::{
    load_settings, Access, Dependency, Generate, Installed, InstalledPackage, Limits, NetworkGrant, PortProtocol,
    SettingValue, Value,
};

fn files() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/monokulo/files")
}

fn manifest() -> Manifest {
    let text = std::fs::read_to_string(files().join("manifest.toml")).unwrap();
    Manifest::parse(&text.replace("@VERSION@", "0.1.0pre20261107")).unwrap()
}

/// cuprated as the base image would describe it once it exports its RPC address.
fn cuprated() -> InstalledPackage {
    let manifest = Manifest::parse_system(
        "api_version = 1\nid = \"cuprated\"\nversion = \"0.1.0\"\nexec = [\"/usr/bin/cuprated\"]\n\
         provides = [\"monero-daemon\"]\n[[network.inbound]]\nport = 18089\nprotocol = \"tcp\"\nscope = \"local\"\n",
    )
    .unwrap();
    let settings = load_settings(
        r#"return { api_version = 1, settings = {
            { name = "rpc_url", type = "url", label = "RPC", default = "http://127.0.0.1:18089",
              public = "public_rpc_url", endpoint = "tcp" },
        } }"#,
        Limits::default(),
    )
    .unwrap();
    InstalledPackage { manifest, schema: settings.schema().clone(), values: BTreeMap::new() }
}

#[test]
fn the_manifest_passes_feed_validation_and_names_its_files() {
    let m = manifest();
    assert_eq!(m.id, "monokulo");
    assert_eq!(m.exec[0], "bin/monokulo");
    assert_eq!(m.settings.as_deref(), Some("settings.lua"));
    assert!(m.storage.encrypted);
    assert_eq!(m.needs, ["monero-daemon"]);
}

#[test]
fn settings_resolve_against_the_node_role() {
    let source = std::fs::read_to_string(files().join("settings.lua")).unwrap();
    let loaded = load_settings(&source, Limits::default()).unwrap();
    let schema = loaded.schema().clone();
    let key_def = schema.setting("encryption_key").unwrap();
    assert_eq!(key_def.generate, Some(Generate::Hex { bytes: 32 }));
    assert!(key_def.public.is_none());

    // What the device does on install: create the key from fresh random bytes.
    let key = key_def.generate.unwrap().encode(&[0xab; 32]).unwrap();
    let mut values = BTreeMap::new();
    values.insert("encryption_key".to_string(), SettingValue::Literal(Value::Secret(key)));

    let mut installed = Installed::default();
    installed.packages.insert("cuprated".into(), cuprated());
    installed
        .packages
        .insert("monokulo".into(), InstalledPackage { manifest: manifest(), schema, values });
    let resolved = installed.resolve("monokulo").unwrap();

    assert_eq!(resolved.values["daemon_url"], Value::Url("http://127.0.0.1:18089".into()));
    assert!(resolved.exec.contains(&"--daemon-url=http://127.0.0.1:18089".to_string()), "{:?}", resolved.exec);
    assert!(!resolved.exec.iter().any(|arg| arg.contains("abab")), "the key never reaches argv");
    assert_eq!(
        resolved.dependencies.iter().collect::<Vec<_>>(),
        [&Dependency { package: "cuprated".into(), role: Some("monero-daemon".into()) }]
    );
    assert_eq!(
        resolved.network.iter().collect::<Vec<_>>(),
        [&NetworkGrant {
            consumer: "monokulo".into(),
            provider: "cuprated".into(),
            access: Access::Port { protocol: PortProtocol::Tcp, port: 18089 },
        }]
    );
    assert!(loaded.validate_values(&resolved.values).is_empty());
    let mut bad = resolved.values.clone();
    bad.insert("daemon_url".into(), Value::Url("http://127.0.0.1:18089/rpc".into()));
    assert_eq!(loaded.validate_values(&bad).len(), 1, "a path is refused by the package's validator");
}

#[test]
fn the_permission_summary_is_what_the_user_sees() {
    let p = Permissions::of(&manifest(), &["glibc>=2.41_1".into(), "libgcc>=4.4.0_1".into(), "libstdc++>=4.4.0_1".into()]);
    assert!(p.granted.contains(&Permission::OutboundInternet));
    assert!(!p.granted.contains(&Permission::UnencryptedStorage));
    assert!(p.admin_paths["web"].contains("/admin/*"));
}

/// Runs the launcher with a stand-in binary that prints what it was given.
fn launch(args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let libexec = dir.path().join("libexec");
    let home = dir.path().join("home");
    for d in [&bin, &libexec, &home] {
        std::fs::create_dir(d).unwrap();
    }
    std::fs::copy(files().join("monokulo"), bin.join("monokulo")).unwrap();
    let stand_in = libexec.join("monokulo");
    std::fs::write(&stand_in, "#!/bin/sh\nfor a do echo \"arg:$a\"; done\necho \"key:$MONOKULO_ENCRYPTION_KEY\"\necho \"nodo:${NODO_ENCRYPTION_KEY-unset}\"\n").unwrap();
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new("sh");
    command.arg(bin.join("monokulo")).args(args).env_clear().env("PATH", "/usr/bin:/bin").env("HOME", &home);
    for (k, v) in env {
        command.env(k, v);
    }
    let out = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).replace(home.to_str().unwrap(), "$HOME");
    (out.status.success(), stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn the_launcher_maps_settings_onto_monokulo_options() {
    let key = [("NODO_ENCRYPTION_KEY", "00ff")];
    let (ok, out, err) = launch(&["--server-bind", "127.0.0.1:8081", "--daemon-url=http://127.0.0.1:18089"], &key);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "arg:--options\narg:$HOME/monokulo.toml\narg:--database-path\narg:$HOME/monokulo.db\n\
         arg:--engine-database-path\narg:$HOME/engine.db\narg:--engine-monero-node-mainnet\n\
         arg:{\"host\":\"127.0.0.1\",\"port\":18089,\"ssl\":false}\narg:--server-bind\narg:127.0.0.1:8081\n\
         key:00ff\nnodo:unset\n"
    );
    for (url, json) in [
        ("https://node.example.com/", r#"{"host":"node.example.com","port":443,"ssl":true}"#),
        ("http://node.example.com", r#"{"host":"node.example.com","port":80,"ssl":false}"#),
        ("http://[::1]:18081", r#"{"host":"[::1]","port":18081,"ssl":false}"#),
        ("https://[fd00::2]", r#"{"host":"[fd00::2]","port":443,"ssl":true}"#),
    ] {
        let (ok, out, err) = launch(&[&format!("--daemon-url={url}")], &key);
        assert!(ok, "{url}: {err}");
        assert!(out.contains(&format!("arg:{json}\n")), "{url}: {out}");
    }
}

#[test]
fn the_launcher_refuses_what_it_cannot_express() {
    let key = [("NODO_ENCRYPTION_KEY", "00ff")];
    for url in ["ws://h:1", "http://h:1/json_rpc", "http://h:x", "http://h\"x:1", "http://:1"] {
        let (ok, out, err) = launch(&[&format!("--daemon-url={url}")], &key);
        assert!(!ok && out.is_empty(), "{url} was accepted: {out}");
        assert!(err.contains("monokulo launcher"), "{err}");
    }
    let (ok, _, err) = launch(&["--daemon-url=http://h:1"], &[]);
    assert!(!ok && err.contains("encryption_key"), "{err}");
    let (ok, _, err) = launch(&[], &key);
    assert!(!ok && err.contains("--daemon-url"), "{err}");
}
