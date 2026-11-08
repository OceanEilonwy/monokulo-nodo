//! Proves the generator reproduces the hand-written reference in `packages/cuprated-service`.

mod harness;

use std::fs;
use std::path::PathBuf;

use harness::{run_stubbed, Scenario};
use nodo_gen::{generate, Artifacts, Profile, ResolvedGrants, CONFINE};
use nodo_manifest::Manifest;

fn reference(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/cuprated-service/files")
        .join(file);
    fs::read_to_string(path).unwrap()
}

fn cuprated() -> Artifacts {
    let manifest = Manifest::parse_system(include_str!("golden/cuprated/manifest.toml")).unwrap();
    let grants = ResolvedGrants {
        unrestricted_loopback: true,
        ..Default::default()
    };
    generate(&manifest, Profile::System, &grants).unwrap()
}

/// Comments explain the reference's one service, while generated ones must suit every service.
fn without_comments(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.starts_with("#!") || !line.trim_start().starts_with('#'))
        .collect()
}

/// The sandbox hardening that the generator adds and the frozen reference does not have yet: a
/// user namespace with no room for nested ones, and the program started through nodo-confine.
/// `packages/cuprated-service` is frozen, so its script needs these two changes when it is next
/// updated. Everything else must still match it exactly.
fn without_hardening(run: &str) -> String {
    let hardening = "\t--unshare-user --disable-userns \\\n";
    assert!(run.contains(hardening), "{run}");
    let confine = format!("{CONFINE} -- ");
    assert!(run.contains(&confine), "{run}");
    run.replace(hardening, "").replace(&confine, "")
}

#[test]
fn run_script_matches_reference_line_for_line() {
    assert_eq!(
        without_comments(&without_hardening(&cuprated().run)),
        without_comments(&reference("cuprated/run"))
    );
}

/// Fails once the frozen script gains the hardening, as a reminder to compare it whole again.
#[test]
fn frozen_reference_still_lacks_the_hardening() {
    let original = reference("cuprated/run");
    assert!(!original.contains("--disable-userns") && !original.contains(CONFINE));
}

#[test]
fn control_t_matches_reference_line_for_line() {
    assert_eq!(
        without_comments(&cuprated().control_t),
        without_comments(&reference("cuprated/control/t"))
    );
}

#[test]
fn log_run_is_byte_identical() {
    assert_eq!(cuprated().log_run, reference("cuprated/log/run"));
}

#[test]
fn service_user_matches_reference() {
    assert_eq!(cuprated().user, "_cuprated");
}

#[test]
fn nft_matches_reference() {
    assert_eq!(
        without_comments(&cuprated().nft),
        without_comments(&reference("cuprated.nft"))
    );
}

/// Line equality could still hide a meaningful ordering error in a comment-stripped script, so
/// also execute both and compare every command they invoke.
#[test]
fn run_script_behaves_like_reference_in_every_scenario() {
    let generated = without_hardening(&cuprated().run);
    let original = reference("cuprated/run");
    let scenarios = [
        (
            "no data drive",
            Scenario {
                conf: None,
                mounted: false,
            },
        ),
        (
            "drive mounted",
            Scenario {
                conf: Some("DATA_MOUNT=/mnt/chain\n"),
                mounted: true,
            },
        ),
        (
            "drive absent",
            Scenario {
                conf: Some("DATA_MOUNT=/mnt/chain\n"),
                mounted: false,
            },
        ),
    ];
    for (name, scenario) in &scenarios {
        let (a, _) = run_stubbed(&generated, scenario);
        let (b, _) = run_stubbed(&original, scenario);
        assert_eq!(a, b, "scenario: {name}");
    }
}

#[test]
fn stdin_is_dev_null_when_the_service_starts() {
    let (out, _) = run_stubbed(
        &cuprated().run,
        &Scenario {
            conf: None,
            mounted: false,
        },
    );
    assert!(out.log.contains("--- chpst stdin=/dev/null"), "{}", out.log);
}

#[test]
fn refuses_to_start_when_the_data_drive_is_absent() {
    let scenario = Scenario {
        conf: Some("DATA_MOUNT=/mnt/chain\n"),
        mounted: false,
    };
    let (out, _) = run_stubbed(&cuprated().run, &scenario);
    assert_eq!(out.code, Some(1));
    assert!(out
        .stdout
        .contains("refusing to start: /mnt/chain is not mounted"));
    assert!(!out.log.contains("chpst"), "{}", out.log);
    assert!(!out.log.contains("--- install"), "{}", out.log);
}

#[test]
fn data_drive_replaces_the_system_disk_in_the_sandbox() {
    let scenario = Scenario {
        conf: Some("DATA_MOUNT=/mnt/chain\n"),
        mounted: true,
    };
    let (out, _) = run_stubbed(&cuprated().run, &scenario);
    assert!(
        out.log
            .contains("[--bind]\n[/mnt/chain/cuprated]\n[/var/lib/cuprated]"),
        "{}",
        out.log
    );
}

#[test]
fn control_t_signals_the_real_process_not_bwrap() {
    let (out, _) = run_stubbed(
        &cuprated().control_t,
        &Scenario {
            conf: None,
            mounted: false,
        },
    );
    assert!(out.log.contains("--- pkill"));
    assert!(
        out.log
            .contains("[-TERM]\n[-u]\n[_cuprated]\n[-x]\n[cuprated]\n"),
        "{}",
        out.log
    );
}
