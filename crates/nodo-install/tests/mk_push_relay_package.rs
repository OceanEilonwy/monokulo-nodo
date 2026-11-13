//! The push relay recipe's own files, checked with the same code the device uses.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use nodo_install::{Permission, Permissions};
use nodo_manifest::{HealthCheck, Manifest};
use nodo_settings::{env_file, load_settings, Generate, Installed, InstalledPackage, Limits, SecretString, SettingValue, Value};

fn files() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/mk-push-relay/files")
}

fn manifest() -> Manifest {
    let text = std::fs::read_to_string(files().join("manifest.toml")).unwrap();
    Manifest::parse(&text.replace("@VERSION@", "0.1.0pre20261107")).unwrap()
}

#[test]
fn the_manifest_passes_feed_validation_and_names_its_files() {
    let m = manifest();
    assert_eq!(m.id, "mk-push-relay");
    assert_eq!(m.exec[0], "bin/monokulo-push-relay");
    assert!(files().join("monokulo-push-relay").exists());
    assert!(m.needs.is_empty(), "the relay needs no node");
    assert!(m.storage.encrypted, "device tokens are user data");
    assert_eq!(m.web[0].upstream, "127.0.0.1:8444");
    assert!(m.exec.windows(2).any(|w| w == ["--server-listen", "127.0.0.1:8444"]), "loopback only");
    assert_eq!(m.health, Some(HealthCheck::Http { url: "http://127.0.0.1:8446/status".into() }));
    for panel in &m.panels {
        assert!(files().join(&panel.script).exists(), "{}", panel.script);
    }
    let p = Permissions::of(&m, &[]);
    assert!(p.granted.contains(&Permission::OutboundInternet), "APNs and FCM are on the internet");
    assert!(!p.granted.contains(&Permission::UnencryptedStorage));
}

#[test]
fn settings_load_and_resolve_with_only_the_public_address_given() {
    let source = std::fs::read_to_string(files().join("settings.lua")).unwrap();
    let loaded = load_settings(&source, Limits::default()).unwrap();
    let schema = loaded.schema().clone();
    let key_def = schema.setting("handle_key").unwrap();
    assert_eq!(key_def.generate, Some(Generate::Hex { bytes: 32 }));
    assert!(key_def.public.is_none());

    let key = key_def.generate.unwrap().encode(&[0x5a; 32]).unwrap();
    let mut values = BTreeMap::new();
    values.insert("handle_key".to_string(), SettingValue::Literal(Value::Secret(key)));
    values.insert("public_uri".to_string(), SettingValue::Literal(Value::Url("https://push.example.org".into())));
    let mut installed = Installed::default();
    installed.packages.insert("mk-push-relay".into(), InstalledPackage { manifest: manifest(), schema, values });
    let resolved = installed.resolve("mk-push-relay").unwrap();
    assert!(resolved.dependencies.is_empty() && resolved.network.is_empty());
    assert_eq!(resolved.values["app_name"], Value::String("monokular".into()));
    assert!(!resolved.values.contains_key("apns_key"), "unset optional settings stay unset");
    assert!(loaded.validate_values(&resolved.values).is_empty());
    assert!(!resolved.exec.iter().any(|a| a.contains("5a5a")), "the key never reaches argv");
    env_file(&resolved.values).unwrap();

    let mut bad = resolved.values.clone();
    bad.insert("public_uri".into(), Value::Url("http://push.example.org".into()));
    bad.insert("apns_key".into(), Value::Secret(SecretString::new("-----BEGIN PRIVATE KEY-----\nabc")));
    assert_eq!(loaded.validate_values(&bad).len(), 2, "plain http and a multi-line key are refused");
}

/// Runs the launcher with a stand-in binary that prints what it was given.
fn launch(env: &[(&str, &str)]) -> (bool, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let (bin, libexec, home) = (dir.path().join("bin"), dir.path().join("libexec"), dir.path().join("home"));
    for d in [&bin, &libexec, &home] {
        std::fs::create_dir(d).unwrap();
    }
    std::fs::copy(files().join("monokulo-push-relay"), bin.join("monokulo-push-relay")).unwrap();
    let stand_in = libexec.join("monokulo-push-relay");
    std::fs::write(
        &stand_in,
        "#!/bin/sh\nfor a do echo \"arg:$a\"; done\necho \"handle:$MONOKULO_RELAY_HANDLE_KEY\"\n\
         echo \"apns:${MONOKULO_RELAY_APNS_KEY-unset}\"\necho \"nodo:$(env | grep -c '^NODO_')\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new("sh");
    command.arg(bin.join("monokulo-push-relay")).arg("--status-listen").arg("127.0.0.1:8446");
    command.env_clear().env("PATH", "/usr/bin:/bin").env("HOME", &home);
    for (k, v) in env {
        command.env(k, v);
    }
    let out = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).replace(home.to_str().unwrap(), "$HOME");
    (out.status.success(), stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

const BASE: [(&str, &str); 2] = [("NODO_HANDLE_KEY", "00ff"), ("NODO_PUBLIC_URI", "https://push.example.org")];

#[test]
fn the_launcher_maps_settings_onto_relay_options() {
    let (ok, out, err) = launch(&BASE);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "arg:--options\narg:$HOME/relay.toml\narg:--store-path\narg:$HOME/push-relay.db\n\
         arg:--server-public-uri\narg:https://push.example.org\narg:--status-listen\narg:127.0.0.1:8446\n\
         handle:00ff\napns:unset\nnodo:0\n"
    );

    let body = "A".repeat(100);
    let pem = format!("-----BEGIN PRIVATE KEY----- {body} -----END PRIVATE KEY-----");
    let mut env = BASE.to_vec();
    env.extend([
        ("NODO_APNS_KEY", pem.as_str()),
        ("NODO_APNS_TOPIC", "com.example.monokular"),
        ("NODO_FCM_PROJECT_ID", "monokular-12345"),
        ("NODO_APNS_TEAM_ID", "ABCDE12345"),
        ("NODO_APNS_KEY_ID", "FGHIJ67890"),
        ("NODO_APNS_SANDBOX", "true"),
    ]);
    let (ok, out, err) = launch(&env);
    assert!(ok, "{err}");
    assert!(
        out.contains("arg:--handles-apps\narg:{\"monokular\":{\"apns_topic\":\"com.example.monokular\",\"fcm_project_id\":\"monokular-12345\"}}\n"),
        "{out}"
    );
    assert!(out.contains("arg:--apns-key-id\narg:FGHIJ67890\narg:--apns-team-id\narg:ABCDE12345\n"), "{out}");
    assert!(out.contains("arg:--apns-endpoint\narg:https://api.sandbox.push.apple.com\n"), "{out}");
    let expected_pem = format!("apns:-----BEGIN PRIVATE KEY-----\n{}\n{}\n-----END PRIVATE KEY-----\n", &body[..64], &body[64..]);
    assert!(out.contains(&expected_pem), "{out}");
    assert!(!out.lines().any(|l| l.starts_with("arg:") && l.contains("AAAA")), "the key never reaches argv");
}

#[test]
fn the_launcher_refuses_what_it_cannot_express() {
    let (ok, _, err) = launch(&[("NODO_PUBLIC_URI", "https://push.example.org")]);
    assert!(!ok && err.contains("handle_key"), "{err}");
    let mut env = BASE.to_vec();
    env.push(("NODO_APNS_TOPIC", "com.example\",\"x"));
    let (ok, out, err) = launch(&env);
    assert!(!ok && out.is_empty() && err.contains("apns_topic"), "{err}");
}
