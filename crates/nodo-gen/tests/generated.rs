mod harness;

use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Command;

use harness::{run_stubbed, Scenario};
use nodo_gen::{
    generate, run_script, volume_mount, Artifacts, GenError, LanGrant, Profile, ResolvedGrants,
    ServiceSpec,
};
use nodo_manifest::{Manifest, Protocol};

fn manifest(id: &str, exec: &[&str]) -> Manifest {
    let toml_exec: Vec<String> = exec.iter().map(|arg| toml_string(arg)).collect();
    Manifest::parse_system(&format!(
        "api_version = 1\nid = {}\nversion = \"1\"\nexec = [{}]\n",
        toml_string(id),
        toml_exec.join(", ")
    ))
    .unwrap()
}

/// TOML basic strings share JSON's escapes for everything the tests use.
fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 || c == '\x7f' => {
                out.push_str(&format!("\\u{:04X}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn app(id: &str) -> Manifest {
    manifest(id, &["bin/app"])
}

fn golden_grants(name: &str) -> (Profile, ResolvedGrants) {
    match name {
        "cuprated" => (
            Profile::System,
            ResolvedGrants {
                unrestricted_loopback: true,
                ..Default::default()
            },
        ),
        "shop" => (
            Profile::App,
            ResolvedGrants {
                loopback_tcp_ports: vec![18081],
                socket_deps: vec!["cuprated".into()],
                lan: vec![LanGrant {
                    address: "192.168.1.50".parse().unwrap(),
                    prefix: 32,
                    port: 631,
                    protocol: Protocol::Tcp,
                }],
                data_mount: Some("/mnt/shop".into()),
                ..Default::default()
            },
        ),
        "vault" => (Profile::App, ResolvedGrants::default()),
        other => panic!("unknown golden {other}"),
    }
}

fn golden_artifacts(name: &str) -> Artifacts {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let text = fs::read_to_string(dir.join("manifest.toml")).unwrap();
    let (profile, grants) = golden_grants(name);
    generate(&Manifest::parse_system(&text).unwrap(), profile, &grants).unwrap()
}

/// Set `UPDATE_GOLDEN=1` to rewrite the expected files, then review the diff.
#[test]
fn golden_files() {
    for name in ["cuprated", "shop", "vault"] {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        let artifacts = golden_artifacts(name);
        let files = [
            ("run", &artifacts.run),
            ("log_run", &artifacts.log_run),
            ("control_t", &artifacts.control_t),
            ("rules.nft", &artifacts.nft),
        ];
        for (file, generated) in files {
            let path = dir.join(file);
            if std::env::var_os("UPDATE_GOLDEN").is_some() {
                fs::write(&path, generated).unwrap();
            }
            let expected = fs::read_to_string(&path).unwrap();
            assert_eq!(generated, &expected, "{name}/{file}");
        }
    }
}

#[test]
fn service_user_is_derived_from_the_id() {
    assert_eq!(
        generate(&app("shop"), Profile::App, &ResolvedGrants::default())
            .unwrap()
            .user,
        "_shop"
    );
}

const HOSTILE_IDS: &[&str] = &[
    "",
    " ",
    "a b",
    "a;b",
    "a'b",
    "a\"b",
    "a\nb",
    "a\0b",
    "$(id)",
    "`id`",
    "a}b",
    "a{b",
    "a#b",
    "a*",
    "../etc",
    "a/b",
    ".",
    "..",
    "-rf",
    "a-",
    "a--b",
    "1a",
    "_a",
    "A",
    "\u{e9}",
    "a\" accept\nmeta skuid \"root",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
];

#[test]
fn hostile_ids_are_rejected() {
    for id in HOSTILE_IDS {
        let result = generate(&app(id), Profile::App, &ResolvedGrants::default());
        assert_eq!(result, Err(GenError::Id), "id {id:?}");
    }
}

#[test]
fn longest_id_still_fits_a_linux_user_name() {
    let id = "a".repeat(31);
    let artifacts = generate(&app(&id), Profile::App, &ResolvedGrants::default()).unwrap();
    assert_eq!(artifacts.user.len(), 32);
}

#[test]
fn hostile_programs_are_rejected() {
    let hostile = [
        "",
        "/usr/bin/evil",
        "../../usr/bin/sh",
        "bin/../../../usr/bin/sh",
        "bin//app",
        "./bin/app",
        "-x",
        "bin/-x",
        "bin/a b",
        "bin/a;id",
        "bin/$(id)",
        "bin/`id`",
        "bin/a'b",
        "bin/a\nb",
        "bin/a*",
    ];
    for program in hostile {
        let result = generate(
            &manifest("shop", &[program]),
            Profile::App,
            &ResolvedGrants::default(),
        );
        assert!(
            matches!(result, Err(GenError::Exec(_))),
            "program {program:?}: {result:?}"
        );
    }
}

#[test]
fn empty_exec_is_rejected() {
    let m = Manifest::parse_system("api_version = 1\nid = \"a\"\nversion = \"1\"\nexec = []\n").unwrap();
    assert!(matches!(
        generate(&m, Profile::App, &ResolvedGrants::default()),
        Err(GenError::Exec(_))
    ));
}

#[test]
fn only_system_packages_run_absolute_programs_and_only_from_usr() {
    let system = |program| {
        generate(
            &manifest("a", &[program]),
            Profile::System,
            &ResolvedGrants::default(),
        )
    };
    assert!(system("/usr/bin/a").is_ok());
    assert!(system("/bin/a").is_err());
    assert!(system("bin/a").is_err());
    assert!(system("/usr/../bin/a").is_err());
}

#[test]
fn hostile_arguments_reach_the_program_verbatim_and_never_execute() {
    let hostile = [
        "; touch canary #",
        "$(touch canary)",
        "`touch canary`",
        "' ; touch canary ; '",
        "\" ; touch canary ; \"",
        "'\\''",
        "&& touch canary",
        "| touch canary",
        "> canary",
        "*",
        "~",
        "$HOME",
        "${PATH}",
        "$(pkg.setting)",
        "\\",
        "trailing\\",
        "",
        " ",
        "-",
        "--ro-bind / /",
        "#",
        "\u{e9}\u{1f512}",
    ];
    for arg in hostile {
        let m = manifest("shop", &["bin/app", "--first", arg, "--last"]);
        let artifacts = generate(&m, Profile::App, &ResolvedGrants::default()).unwrap();
        let (out, canary) = run_stubbed(
            &artifacts.run,
            &Scenario {
                conf: None,
                mounted: false,
            },
        );
        assert_eq!(out.code, Some(0), "arg {arg:?}: {out:?}");
        assert!(!canary, "arg {arg:?} executed");
        let expected = format!("[/opt/nodo/apps/shop/bin/app]\n[--first]\n[{arg}]\n[--last]\n");
        assert!(out.log.ends_with(&expected), "arg {arg:?}:\n{}", out.log);
    }
}

#[test]
fn control_characters_in_arguments_are_rejected() {
    for arg in ["a\nb", "a\rb", "a\tb", "a\x1b[2Jb", "a\x7fb", "a\0b"] {
        let m = manifest("shop", &["bin/app", arg]);
        assert!(
            matches!(
                generate(&m, Profile::App, &ResolvedGrants::default()),
                Err(GenError::Exec(_))
            ),
            "arg {arg:?}"
        );
    }
}

#[test]
fn oversized_exec_is_rejected() {
    let long = "a".repeat(4097);
    assert!(generate(
        &manifest("a", &["bin/a", &long]),
        Profile::App,
        &ResolvedGrants::default()
    )
    .is_err());
    let many = vec!["x"; 258];
    let mut exec = vec!["bin/a"];
    exec.extend(many);
    assert!(generate(
        &manifest("a", &exec),
        Profile::App,
        &ResolvedGrants::default()
    )
    .is_err());
}

#[test]
fn app_ids_cannot_collide_with_system_directories() {
    for id in ["nodo", "ssh", "sv", "xbps"] {
        let artifacts = generate(&app(id), Profile::App, &ResolvedGrants::default()).unwrap();
        assert!(
            !artifacts.run.contains(&format!(" /var/lib/{id}")),
            "{}",
            artifacts.run
        );
        assert!(
            !artifacts.run.contains(&format!(" /etc/{id}")),
            "{}",
            artifacts.run
        );
        assert!(!artifacts.log_run.contains(&format!(" /var/log/{id}")));
    }
}

fn with_web(upstream: &str) -> Manifest {
    Manifest::parse_system(&format!(
        "api_version = 1\nid = \"shop\"\nversion = \"1\"\nexec = [\"bin/a\"]\n\n[[web]]\nname = \"w\"\nupstream = {}\n",
        toml_string(upstream)
    ))
    .unwrap()
}

#[test]
fn hostile_web_upstreams_are_rejected() {
    for upstream in [
        "unix:/run/apps/other/web.sock",
        "unix:/run/apps/shop/../other/web.sock",
        "unix:/run/apps/shop/a/b.sock",
        "unix:/run/apps/shop/a b.sock",
        "unix:/run/apps/shop/web",
        "unix:/etc/shadow",
        "10.0.0.1:80",
        "127.0.0.1:0",
        "127.0.0.1:+80",
        "127.0.0.1:99999",
        "127.0.0.1:80 accept",
        "http://127.0.0.1:80",
    ] {
        let result = generate(
            &with_web(upstream),
            Profile::App,
            &ResolvedGrants::default(),
        );
        assert!(
            matches!(result, Err(GenError::Web(_))),
            "{upstream}: {result:?}"
        );
    }
    assert!(generate(
        &with_web("127.0.0.1:80"),
        Profile::App,
        &ResolvedGrants::default()
    )
    .is_ok());
    assert!(generate(
        &with_web("unix:/run/apps/shop/web.sock"),
        Profile::App,
        &ResolvedGrants::default()
    )
    .is_ok());
}

#[test]
fn hostile_web_names_are_rejected() {
    let m = Manifest::parse_system(
        "api_version = 1\nid = \"shop\"\nversion = \"1\"\nexec = [\"bin/a\"]\n\n[[web]]\nname = \"a;b\"\nupstream = \"127.0.0.1:80\"\n",
    )
    .unwrap();
    assert!(matches!(
        generate(&m, Profile::App, &ResolvedGrants::default()),
        Err(GenError::Web(_))
    ));
}

#[test]
fn control_t_handles_kernel_truncation_and_regex_characters() {
    let long = golden_artifacts("vault").control_t;
    // /proc/<pid>/comm holds 15 bytes, so the full name would never match.
    assert!(long.contains("-x vault-with-a-ve\n"), "{long}");
    let (out, _) = run_stubbed(
        &golden_artifacts("shop").control_t,
        &Scenario {
            conf: None,
            mounted: false,
        },
    );
    assert!(out.log.contains("[-x]\n[shop\\.server]\n"), "{}", out.log);
}

#[test]
fn outbound_is_denied_by_default() {
    let nft = generate(&app("shop"), Profile::App, &ResolvedGrants::default())
        .unwrap()
        .nft;
    let rules: Vec<&str> = nft
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("meta skuid"))
        .collect();
    assert_eq!(rules, ["meta skuid \"_shop\" drop"]);
}

#[test]
fn final_output_rule_is_always_a_drop() {
    for name in ["cuprated", "shop", "vault"] {
        let nft = golden_artifacts(name).nft;
        let last = nft
            .lines()
            .rfind(|l| l.contains("meta skuid"))
            .unwrap()
            .trim();
        assert!(last.ends_with("\" drop"), "{name}: {last}");
    }
}

/// Loopback carries unauthenticated RPC, including cuprated's, so apps only reach what they own
/// or were granted.
#[test]
fn apps_reach_only_granted_loopback_ports() {
    let grants = ResolvedGrants {
        loopback_tcp_ports: vec![18081],
        ..Default::default()
    };
    let nft = generate(&app("shop"), Profile::App, &grants).unwrap().nft;
    assert!(
        nft.contains("meta skuid \"_shop\" oifname \"lo\" tcp dport 18081 accept"),
        "{nft}"
    );
    assert!(!nft.contains("oifname \"lo\" accept"), "{nft}");
}

#[test]
fn only_system_packages_get_unrestricted_loopback() {
    let grants = ResolvedGrants {
        unrestricted_loopback: true,
        ..Default::default()
    };
    assert!(matches!(
        generate(&app("shop"), Profile::App, &grants),
        Err(GenError::Grant(_))
    ));
}

#[test]
fn private_ranges_are_blocked_after_local_grants_and_in_both_families() {
    let nft = golden_artifacts("shop").nft;
    let granted = nft
        .find("ip daddr 192.168.1.50/32 tcp dport 631 accept")
        .expect(&nft);
    let v4 = nft.find("ip daddr { 10.0.0.0/8").expect(&nft);
    let v6 = nft.find("ip6 daddr { fc00::/7").expect(&nft);
    let tcp = nft.find("meta l4proto tcp accept").expect(&nft);
    assert!(granted < v4 && v4 < tcp && v6 < tcp, "{nft}");
}

/// Without this a listener with no outbound access could not answer its own clients.
#[test]
fn listeners_without_outbound_can_still_reply() {
    let m = Manifest::parse_system(
        "api_version = 1\nid = \"a\"\nversion = \"1\"\nexec = [\"bin/a\"]\n\n[[network.inbound]]\nport = 80\nprotocol = \"tcp\"\nscope = \"lan\"\n",
    )
    .unwrap();
    let nft = generate(&m, Profile::App, &ResolvedGrants::default())
        .unwrap()
        .nft;
    let reply = nft.find("ct state established,related accept").expect(&nft);
    let drop = nft.find("meta skuid \"_a\" drop").unwrap();
    assert!(reply < drop, "{nft}");
    assert!(
        nft.contains("ip saddr {"),
        "LAN scope must be source limited: {nft}"
    );
}

#[test]
fn lan_grants_must_stay_inside_private_ranges() {
    let grant = |address: &str, prefix: u8| ResolvedGrants {
        lan: vec![LanGrant {
            address: address.parse::<IpAddr>().unwrap(),
            prefix,
            port: 80,
            protocol: Protocol::Tcp,
        }],
        ..Default::default()
    };
    let check = |g: ResolvedGrants| generate(&app("a"), Profile::App, &g);
    assert!(check(grant("192.168.1.0", 24)).is_ok());
    assert!(check(grant("10.1.2.3", 32)).is_ok());
    assert!(check(grant("172.16.0.0", 12)).is_ok());
    assert!(check(grant("fd00::1", 128)).is_ok());
    assert!(check(grant("fe80::", 10)).is_ok());
    // Public addresses, and prefixes wide enough to escape the private range.
    assert!(check(grant("8.8.8.8", 32)).is_err());
    assert!(check(grant("0.0.0.0", 0)).is_err());
    assert!(check(grant("10.0.0.0", 7)).is_err());
    assert!(check(grant("2001:db8::", 32)).is_err());
    assert!(check(grant("::", 0)).is_err());
    // Malformed.
    assert!(check(grant("192.168.1.1", 24)).is_err());
    assert!(check(grant("192.168.1.0", 33)).is_err());
    assert!(check(grant("fd00::", 129)).is_err());
    let mut zero = grant("10.0.0.1", 32);
    zero.lan[0].port = 0;
    assert!(check(zero).is_err());
}

#[test]
fn port_zero_is_rejected() {
    let m = Manifest::parse_system(
        "api_version = 1\nid = \"a\"\nversion = \"1\"\nexec = [\"bin/a\"]\n\n[[network.inbound]]\nport = 0\nprotocol = \"tcp\"\nscope = \"lan\"\n",
    )
    .unwrap();
    assert_eq!(
        generate(&m, Profile::App, &ResolvedGrants::default()),
        Err(GenError::Port)
    );
    let grants = ResolvedGrants {
        loopback_tcp_ports: vec![0],
        ..Default::default()
    };
    assert_eq!(
        generate(&app("a"), Profile::App, &grants),
        Err(GenError::Port)
    );
}

#[test]
fn grants_cannot_expose_another_packages_volume_or_system_paths() {
    for mount in [
        "/run/nodo/volumes/other",
        "/",
        "/etc",
        "/var/lib/nodo",
        "/mnt",
        "/mnt/",
        "/mnt/../etc",
        "/mnt/a b",
        "/mnt/$(id)",
        "relative",
    ] {
        let grants = ResolvedGrants {
            data_mount: Some(mount.into()),
            ..Default::default()
        };
        assert!(
            generate(&app("a"), Profile::App, &grants).is_err(),
            "{mount}"
        );
    }
    for dep in ["../nodo/volumes/other", "a b", "$(id)", ""] {
        let grants = ResolvedGrants {
            socket_deps: vec![dep.into()],
            ..Default::default()
        };
        assert!(generate(&app("a"), Profile::App, &grants).is_err(), "{dep}");
    }
}

#[test]
fn required_data_mount_cannot_be_overridden_by_conf() {
    let grants = ResolvedGrants {
        data_mount: Some("/mnt/data".into()),
        ..Default::default()
    };
    let run = generate(&app("a"), Profile::App, &grants).unwrap().run;
    let scenario = Scenario {
        conf: Some("DATA_MOUNT=/tmp/elsewhere\n"),
        mounted: false,
    };
    let (out, _) = run_stubbed(&run, &scenario);
    assert_eq!(out.code, Some(1));
    assert!(
        out.stdout
            .contains("refusing to start: /mnt/data is not mounted"),
        "{out:?}"
    );
    assert!(!out.log.contains("chpst"), "{}", out.log);
}

fn encrypted(id: &str) -> Manifest {
    Manifest::parse_system(&format!(
        "api_version = 1\nid = \"{id}\"\nversion = \"1\"\nexec = [\"bin/a\"]\n\n[storage]\nencrypted = true\n"
    ))
    .unwrap()
}

#[test]
fn encrypted_package_waits_while_locked() {
    let run = generate(
        &encrypted("vault"),
        Profile::App,
        &ResolvedGrants::default(),
    )
    .unwrap()
    .run;
    let (out, _) = run_stubbed(
        &run,
        &Scenario {
            conf: None,
            mounted: false,
        },
    );
    assert_eq!(out.code, Some(1));
    assert!(
        out.stdout
            .contains("waiting: encrypted storage for vault is locked"),
        "{out:?}"
    );
    assert!(out.log.contains("[/run/nodo/volumes/vault]"), "{}", out.log);
    assert!(out.log.contains("--- sleep"), "must not spin: {}", out.log);
    assert!(
        !out.log.contains("chpst"),
        "started while locked: {}",
        out.log
    );
}

#[test]
fn encrypted_package_binds_only_its_own_volume_once_unlocked() {
    let run = generate(
        &encrypted("vault"),
        Profile::App,
        &ResolvedGrants::default(),
    )
    .unwrap()
    .run;
    let (out, _) = run_stubbed(
        &run,
        &Scenario {
            conf: None,
            mounted: true,
        },
    );
    assert_eq!(out.code, Some(0));
    assert!(
        out.log
            .contains("[--bind]\n[/run/nodo/volumes/vault]\n[/var/lib/apps/vault]\n"),
        "{}",
        out.log
    );
    // Only this exact path. The parent would show every other package's volume.
    let mounts = out.log.matches("/run/nodo/volumes").count();
    assert_eq!(mounts, 2, "one mountpoint check, one bind: {}", out.log);
}

#[test]
fn encrypted_package_never_appears_in_another_sandbox() {
    let vault = volume_mount("vault").unwrap();
    for name in ["cuprated", "shop"] {
        assert!(!golden_artifacts(name).run.contains(&vault));
    }
    let other = generate(
        &encrypted("other"),
        Profile::App,
        &ResolvedGrants::default(),
    )
    .unwrap();
    assert!(!other.run.contains(&vault));
}

#[test]
fn conf_cannot_redirect_encrypted_data_to_plaintext() {
    let spec = ServiceSpec::new(
        &encrypted("vault"),
        Profile::App,
        &ResolvedGrants::default(),
    )
    .unwrap();
    let run = run_script(&spec);
    let scenario = Scenario {
        conf: Some("data=/tmp/plain\nDATA_MOUNT=/tmp/plain\n"),
        mounted: true,
    };
    let (out, _) = run_stubbed(&run, &scenario);
    assert!(!out.log.contains("/tmp/plain"), "{}", out.log);
    assert!(out.log.contains("[/run/nodo/volumes/vault]"), "{}", out.log);
}

#[test]
fn encrypted_package_has_no_plaintext_data_directory() {
    let artifacts = generate(
        &encrypted("vault"),
        Profile::App,
        &ResolvedGrants::default(),
    )
    .unwrap();
    assert!(
        artifacts
            .directories
            .iter()
            .all(|d| !d.path.contains("/var/lib")),
        "{artifacts:?}"
    );
    let plain = generate(&app("vault"), Profile::App, &ResolvedGrants::default()).unwrap();
    assert!(plain
        .directories
        .iter()
        .any(|d| d.path == "/var/lib/apps/vault"));
}

#[test]
fn encrypted_storage_refuses_a_data_mount() {
    let grants = ResolvedGrants {
        data_mount: Some("/mnt/data".into()),
        ..Default::default()
    };
    assert!(matches!(
        generate(&encrypted("vault"), Profile::App, &grants),
        Err(GenError::Grant(_))
    ));
}

#[test]
fn sandbox_always_links_lib64_and_detaches_stdin() {
    for name in ["cuprated", "shop", "vault"] {
        let run = golden_artifacts(name).run;
        assert!(run.contains("--symlink usr/lib /lib64"), "{name}");
        assert!(run.contains("exec </dev/null\n"), "{name}");
    }
}

#[test]
fn services_without_outbound_do_not_see_resolver_config() {
    let run = golden_artifacts("vault").run;
    assert!(
        !run.contains("resolv.conf") && !run.contains("/etc/ssl"),
        "{run}"
    );
}

/// Validates generated rules with the real parser. Run with `cargo test -- --ignored`. Needs `nft`
/// and unprivileged user namespaces, and works in a throwaway network namespace.
#[test]
#[ignore = "requires nft and unprivileged user namespaces"]
fn nft_accepts_generated_rules() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["cuprated", "shop", "vault"] {
        // nft resolves user names while parsing, and these service users do not exist here.
        let nft = golden_artifacts(name)
            .nft
            .replace(&format!("\"_{name}\""), "\"nobody\"");
        let path = dir.path().join(format!("{name}.nft"));
        fs::write(&path, nft).unwrap();
        let out = Command::new("unshare")
            .args(["-rn", "nft", "-c", "-f"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Run with `cargo test -- --ignored` where shellcheck is installed.
#[test]
#[ignore = "requires shellcheck"]
fn shellcheck_accepts_generated_scripts() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["cuprated", "shop", "vault"] {
        let artifacts = golden_artifacts(name);
        for (file, text) in [
            ("run", &artifacts.run),
            ("log", &artifacts.log_run),
            ("t", &artifacts.control_t),
        ] {
            let path = dir.path().join(format!("{name}-{file}"));
            fs::write(&path, text).unwrap();
            // SC1091: `conf` is deliberately only present on some boards.
            let out = Command::new("shellcheck")
                .args(["-e", "SC1091"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{name}/{file}: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }
}

/// An app allowed out to the internet must still not reach arbitrary local ports, because the
/// general TCP accept also matches loopback traffic.
#[test]
fn internet_access_does_not_open_loopback() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/shop/rules.nft"),
    )
    .unwrap();
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let drop = lines
        .iter()
        .position(|l| *l == "meta skuid \"_shop\" oifname \"lo\" drop")
        .expect("loopback drop rule");
    let accept = lines
        .iter()
        .position(|l| *l == "meta skuid \"_shop\" meta l4proto tcp accept")
        .expect("internet accept rule");
    assert!(drop < accept, "loopback must be dropped before the internet accept");
}
