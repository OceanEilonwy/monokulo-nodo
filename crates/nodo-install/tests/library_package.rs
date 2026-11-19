//! The Library recipe's own files, checked with the same code the device uses.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use nodo_install::{Permission, Permissions};
use nodo_manifest::{HealthCheck, Manifest, Outbound};
use nodo_settings::{load_settings, Installed, InstalledPackage, Limits, Value};

fn files() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/library/files")
}

fn manifest() -> Manifest {
    let text = std::fs::read_to_string(files().join("manifest.toml")).unwrap();
    Manifest::parse(&text.replace("@VERSION@", "0.1.0")).unwrap()
}

#[test]
fn the_manifest_passes_feed_validation_and_names_its_files() {
    let m = manifest();
    assert_eq!(m.id, "library");
    assert_eq!(m.exec[0], "bin/library");
    assert!(files().join("library").exists());
    assert!(m.needs.is_empty());
    assert!(m.storage.encrypted, "the catalogue, borrowers' names and photos are user data");
    assert_eq!(m.storage.size_mib, Some(4096), "room for photos and the search model");
    assert_eq!(m.network.outbound, Outbound::Internet, "lookups and the model's first download");
    assert!(m.network.inbound.is_empty(), "visitors come through the proxy only");
    assert_eq!(m.web.len(), 1);
    assert_eq!(m.web[0].upstream, "127.0.0.1:8790");
    assert!(m.web[0].multi_address, "every link is relative");
    assert_eq!(m.health, Some(HealthCheck::Http { url: "http://127.0.0.1:8790/healthz".into() }));
    let p = Permissions::of(&m, &[]);
    assert!(p.granted.contains(&Permission::OutboundInternet));
    assert!(!p.granted.contains(&Permission::UnencryptedStorage));
}

#[test]
fn settings_load_and_resolve_with_defaults_only() {
    let source = std::fs::read_to_string(files().join("settings.lua")).unwrap();
    let loaded = load_settings(&source, Limits::default()).unwrap();
    let schema = loaded.schema().clone();
    let mut installed = Installed::default();
    installed.packages.insert("library".into(), InstalledPackage { manifest: manifest(), schema, values: BTreeMap::new() });
    let resolved = installed.resolve("library").unwrap();
    assert_eq!(resolved.exec[1..], ["--listen", "127.0.0.1:8790"]);
    assert_eq!(resolved.values.get("title"), Some(&Value::String("The Library".into())));
    assert_eq!(resolved.values.get("semantic_search"), Some(&Value::Bool(true)));
    assert_eq!(resolved.values.get("online_lookups"), Some(&Value::Bool(true)));
    for optional in ["admin_password", "site_password", "tmdb_token", "lookup_contact"] {
        assert!(!resolved.values.contains_key(optional), "{optional} has no default");
    }
    assert!(resolved.dependencies.is_empty());

    let mut bad = resolved.values.clone();
    bad.insert("title".into(), Value::String(String::new()));
    bad.insert("lookup_contact".into(), Value::String("a\"b".into()));
    assert!(!loaded.validate_values(&bad).is_empty());
    let mut good = resolved.values.clone();
    good.insert("title".into(), Value::String("The Hollingworth Library".into()));
    good.insert("lookup_contact".into(), Value::String("https://example.org".into()));
    assert!(loaded.validate_values(&good).is_empty());
}

fn launch(env: &[(&str, &str)]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let (bin, libexec, home) = (dir.path().join("bin"), dir.path().join("libexec"), dir.path().join("home"));
    for d in [&bin, &libexec, &home] {
        std::fs::create_dir(d).unwrap();
    }
    std::fs::copy(files().join("library"), bin.join("library")).unwrap();
    let stand_in = libexec.join("library");
    std::fs::write(
        &stand_in,
        "#!/bin/sh\nfor a do echo \"arg:$a\"; done\nenv | grep '^LIBRARY_' | sort\necho \"nodo:$(env | grep -c '^NODO_')\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new("sh");
    command.arg(bin.join("library")).args(["--listen", "127.0.0.1:8790"]);
    command.env_clear().env("PATH", "/usr/bin:/bin").env("HOME", &home);
    for (k, v) in env {
        command.env(k, v);
    }
    let out = command.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).replace(home.to_str().unwrap(), "$HOME")
}

#[test]
fn the_launcher_maps_settings_and_keeps_data_in_home() {
    let args = "arg:--config\narg:$HOME/library.toml\narg:serve\narg:--data-dir\narg:$HOME\n\
                arg:--listen\narg:127.0.0.1:8790\n";
    assert_eq!(
        launch(&[("NODO_TITLE", "The Library"), ("NODO_SEMANTIC_SEARCH", "true"), ("NODO_ONLINE_LOOKUPS", "true")]),
        format!("{args}LIBRARY_LOOKUPS=on\nLIBRARY_SEMANTIC=on\nLIBRARY_TITLE=The Library\nnodo:0\n")
    );
    assert_eq!(
        launch(&[
            ("NODO_ADMIN_PASSWORD", "pa$(ss) word"),
            ("NODO_SITE_PASSWORD", "family"),
            ("NODO_TMDB_TOKEN", "tok"),
            ("NODO_LOOKUP_CONTACT", "https://example.org"),
            ("NODO_SEMANTIC_SEARCH", "false"),
            ("NODO_ONLINE_LOOKUPS", "false"),
            ("NODO_UNRELATED", "x"),
            ("LIBRARY_ADMIN_PASSWORD_HASH", "smuggled"),
        ]),
        format!(
            "{args}LIBRARY_ADMIN_PASSWORD=pa$(ss) word\nLIBRARY_LOOKUPS=off\nLIBRARY_LOOKUP_CONTACT=https://example.org\n\
             LIBRARY_SEMANTIC=off\nLIBRARY_SITE_PASSWORD=family\nLIBRARY_TMDB_TOKEN=tok\nnodo:0\n"
        ),
        "passwords pass through untouched, and nothing else from the environment reaches the app"
    );
}
