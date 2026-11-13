//! The Bell recipe's own files, checked with the same code the device uses.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use nodo_install::{Permission, Permissions};
use nodo_manifest::{HealthCheck, Manifest};
use nodo_settings::{env_file, load_settings, Generate, Installed, InstalledPackage, Limits, SecretString, SettingValue, Value};

fn files() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/mk-bell/files")
}

fn manifest() -> Manifest {
    let text = std::fs::read_to_string(files().join("manifest.toml")).unwrap();
    Manifest::parse(&text.replace("@VERSION@", "0.1.0pre20261107")).unwrap()
}

#[test]
fn the_manifest_passes_feed_validation_and_names_its_files() {
    let m = manifest();
    assert_eq!(m.id, "mk-bell");
    assert_eq!(m.exec[0], "bin/monokulo-bell");
    assert!(files().join("monokulo-bell").exists());
    assert!(m.needs.is_empty(), "the Bell needs no node");
    assert!(m.storage.encrypted, "registrations are user data");
    assert_eq!(m.web[0].upstream, "127.0.0.1:8445");
    assert_eq!(m.health, Some(HealthCheck::Http { url: "http://127.0.0.1:8447/status".into() }));
    for panel in &m.panels {
        assert!(files().join(&panel.script).exists(), "{}", panel.script);
    }
    let p = Permissions::of(&m, &[]);
    assert!(p.granted.contains(&Permission::OutboundInternet));
    assert!(!p.granted.contains(&Permission::UnencryptedStorage));
}

#[test]
fn settings_load_and_resolve() {
    let source = std::fs::read_to_string(files().join("settings.lua")).unwrap();
    let loaded = load_settings(&source, Limits::default()).unwrap();
    let schema = loaded.schema().clone();
    assert_eq!(schema.settings.len(), 2 + 6 * 2, "the address, the VAPID key and six integrations");
    let key_def = schema.setting("vapid_key").unwrap();
    assert_eq!(key_def.generate, Some(Generate::Hex { bytes: 32 }));

    let key = key_def.generate.unwrap().encode(&[0x7e; 32]).unwrap();
    let mut values = BTreeMap::new();
    values.insert("vapid_key".to_string(), SettingValue::Literal(Value::Secret(key)));
    values.insert("public_uri".to_string(), SettingValue::Literal(Value::Url("https://bell.example.org".into())));
    values.insert(
        "square_production_application_id".to_string(),
        SettingValue::Literal(Value::String("sq0idp-AbC_123-xyz".into())),
    );
    values.insert(
        "square_production_webhook_key".to_string(),
        SettingValue::Literal(Value::Secret(SecretString::new("whsk_Zx9+/="))),
    );
    let mut installed = Installed::default();
    installed.packages.insert("mk-bell".into(), InstalledPackage { manifest: manifest(), schema, values });
    let resolved = installed.resolve("mk-bell").unwrap();
    assert!(resolved.dependencies.is_empty() && resolved.network.is_empty());
    assert!(loaded.validate_values(&resolved.values).is_empty());
    env_file(&resolved.values).unwrap();

    let mut bad = resolved.values.clone();
    bad.insert("clover_europe_application_id".into(), Value::String("short".into()));
    bad.insert("square_sandbox_application_id".into(), Value::String("sq0idp-x\"y".into()));
    assert_eq!(loaded.validate_values(&bad).len(), 2);
}

fn launch(env: &[(&str, &str)]) -> (bool, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let (bin, libexec, home) = (dir.path().join("bin"), dir.path().join("libexec"), dir.path().join("home"));
    for d in [&bin, &libexec, &home] {
        std::fs::create_dir(d).unwrap();
    }
    std::fs::copy(files().join("monokulo-bell"), bin.join("monokulo-bell")).unwrap();
    let stand_in = libexec.join("monokulo-bell");
    std::fs::write(
        &stand_in,
        "#!/bin/sh\nfor a do echo \"arg:$a\"; done\nenv | grep '^MONOKULO_BELL_' | sort\necho \"nodo:$(env | grep -c '^NODO_')\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new("sh");
    command.arg(bin.join("monokulo-bell")).env_clear().env("PATH", "/usr/bin:/bin").env("HOME", &home);
    for (k, v) in env {
        command.env(k, v);
    }
    let out = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).replace(home.to_str().unwrap(), "$HOME");
    (out.status.success(), stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

const BASE: [(&str, &str); 2] = [("NODO_VAPID_KEY", "00ff"), ("NODO_PUBLIC_URI", "https://bell.example.org")];

#[test]
fn the_launcher_maps_settings_onto_bell_options() {
    let mut env = BASE.to_vec();
    env.extend([
        ("NODO_SQUARE_PRODUCTION_APPLICATION_ID", "sq0idp-abc"),
        ("NODO_SQUARE_PRODUCTION_WEBHOOK_KEY", "sig"),
        ("NODO_CLOVER_NORTH_AMERICA_APPLICATION_ID", "MONOKULARBELL"),
        ("NODO_CLOVER_NORTH_AMERICA_AUTH_CODE", "code"),
    ]);
    let (ok, out, err) = launch(&env);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "arg:--options\narg:$HOME/bell.toml\narg:--bell-store-path\narg:$HOME/bell.db\n\
         arg:--bell-clover-north-america-application-id\narg:MONOKULARBELL\n\
         arg:--bell-square-production-application-id\narg:sq0idp-abc\n\
         arg:--bell-public-uri\narg:https://bell.example.org\n\
         MONOKULO_BELL_CLOVER_NORTH_AMERICA_AUTH_CODE=code\nMONOKULO_BELL_SQUARE_PRODUCTION_WEBHOOK_KEY=sig\n\
         MONOKULO_BELL_VAPID_KEY=00ff\nnodo:0\n"
    );
}

#[test]
fn the_launcher_refuses_an_incomplete_integration() {
    let (ok, _, err) = launch(&BASE);
    assert!(!ok && err.contains("no point of sale"), "{err}");
    let mut env = BASE.to_vec();
    env.push(("NODO_SQUARE_SANDBOX_APPLICATION_ID", "sandbox-sq0idb-x"));
    let (ok, _, err) = launch(&env);
    assert!(!ok && err.contains("square_sandbox_webhook_key"), "{err}");
    let mut env = BASE.to_vec();
    env.push(("NODO_CLOVER_EUROPE_AUTH_CODE", "code"));
    let (ok, _, err) = launch(&env);
    assert!(!ok && err.contains("clover_europe_application_id"), "{err}");
}
