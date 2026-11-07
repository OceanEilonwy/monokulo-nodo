//! Crafted archives, built byte by byte so tests can produce what no well behaved tool would.

use super::*;

const ID: &str = "shop";
const DIR: &str = "./opt/nodo/apps/shop";

/// One tar header's worth of fields. Defaults describe a harmless regular file.
#[derive(Clone)]
struct Raw {
    name: Vec<u8>,
    link: Vec<u8>,
    type_flag: u8,
    mode: u32,
    uid: u64,
    gid: u64,
    body: Vec<u8>,
    /// Overrides the size field without changing the body, for lying headers.
    claimed_size: Option<u64>,
    /// Pax records sent ahead of this entry.
    pax: Vec<(String, Vec<u8>)>,
}

impl Raw {
    fn file(name: &str, body: &[u8]) -> Self {
        Raw {
            name: name.as_bytes().to_vec(),
            link: Vec::new(),
            type_flag: b'0',
            mode: 0o644,
            uid: 0,
            gid: 0,
            body: body.to_vec(),
            claimed_size: None,
            pax: Vec::new(),
        }
    }

    fn dir(name: &str) -> Self {
        Raw { type_flag: b'5', mode: 0o755, ..Raw::file(name, b"") }
    }

    fn symlink(name: &str, target: &str) -> Self {
        Raw { type_flag: b'2', mode: 0o777, link: target.as_bytes().to_vec(), ..Raw::file(name, b"") }
    }

    fn hardlink(name: &str, target: &str) -> Self {
        Raw { type_flag: b'1', link: target.as_bytes().to_vec(), ..Raw::file(name, b"") }
    }

    fn of_type(name: &str, type_flag: u8) -> Self {
        Raw { type_flag, ..Raw::file(name, b"") }
    }

    fn mode(mut self, mode: u32) -> Self {
        self.mode = mode;
        self
    }
}

fn octal_field(out: &mut [u8], value: u64) {
    let text = format!("{:0width$o}\0", value, width = out.len() - 1);
    out.copy_from_slice(text.as_bytes());
}

fn header_block(raw: &Raw, size: u64, type_flag: u8) -> [u8; 512] {
    let mut block = [0u8; 512];
    let name_len = raw.name.len().min(100);
    block[..name_len].copy_from_slice(&raw.name[..name_len]);
    octal_field(&mut block[100..108], u64::from(raw.mode));
    octal_field(&mut block[108..116], raw.uid);
    octal_field(&mut block[116..124], raw.gid);
    octal_field(&mut block[124..136], size);
    octal_field(&mut block[136..148], 0);
    block[156] = type_flag;
    let link_len = raw.link.len().min(100);
    block[157..157 + link_len].copy_from_slice(&raw.link[..link_len]);
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[265..269].copy_from_slice(b"root");
    block[297..301].copy_from_slice(b"root");
    set_checksum(&mut block);
    block
}

fn set_checksum(block: &mut [u8; 512]) {
    block[148..156].copy_from_slice(b"        ");
    let sum: u64 = block.iter().map(|b| u64::from(*b)).sum();
    let text = format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(text.as_bytes());
}

fn pad(out: &mut Vec<u8>) {
    let extra = out.len() % 512;
    if extra != 0 {
        out.resize(out.len() + 512 - extra, 0);
    }
}

fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
    let base = key.len() + value.len() + 3;
    let mut length = base + base.to_string().len();
    if (base + length.to_string().len()) != length {
        length = base + length.to_string().len();
    }
    let mut record = format!("{length} {key}=").into_bytes();
    record.extend_from_slice(value);
    record.push(b'\n');
    assert_eq!(record.len(), length);
    record
}

fn write_entry(out: &mut Vec<u8>, raw: &Raw) {
    if !raw.pax.is_empty() {
        let body: Vec<u8> = raw.pax.iter().flat_map(|(key, value)| pax_record(key, value)).collect();
        let pax_header = Raw::file("PaxHeader/x", b"");
        out.extend_from_slice(&header_block(&pax_header, body.len() as u64, b'x'));
        out.extend_from_slice(&body);
        pad(out);
    }
    let size = raw.claimed_size.unwrap_or(raw.body.len() as u64);
    out.extend_from_slice(&header_block(raw, size, raw.type_flag));
    out.extend_from_slice(&raw.body);
    pad(out);
}

fn tar_bytes(entries: &[Raw]) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in entries {
        write_entry(&mut out, entry);
    }
    out.extend_from_slice(&[0u8; 1024]);
    out
}

fn compress(bytes: &[u8]) -> Vec<u8> {
    zstd::encode_all(bytes, 3).unwrap()
}

fn props_for(name: &str, version: &str, extra: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n<key>architecture</key>\n<string>aarch64</string>\n<key>pkgname</key>\n<string>{name}</string>\n<key>pkgver</key>\n<string>{name}-{version}</string>\n<key>version</key>\n<string>{version}</string>\n<key>run_depends</key>\n<array>\n<string>glibc&gt;=2.36_1</string>\n</array>\n{extra}</dict>\n</plist>\n"
    )
    .into_bytes()
}

fn manifest_for(id: &str, version: &str) -> Vec<u8> {
    format!("api_version = 1\nid = \"{id}\"\nversion = \"{version}\"\nexec = [\"bin/shop\"]\n").into_bytes()
}

/// Lists the payload entries in `files.plist`, as `xbps-create` would.
fn files_plist(entries: &[Raw]) -> Vec<u8> {
    let list = |kinds: &[u8]| -> String {
        entries
            .iter()
            .filter(|e| kinds.contains(&e.type_flag))
            .map(|e| {
                let name = String::from_utf8_lossy(&e.name);
                let path = name.strip_prefix('.').unwrap_or(&name).trim_end_matches('/').to_string();
                format!("<dict><key>file</key><string>{path}</string></dict>\n")
            })
            .collect()
    };
    format!(
        "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n<key>dirs</key>\n<array>\n{}</array>\n<key>files</key>\n<array>\n{}</array>\n<key>links</key>\n<array>\n{}</array>\n</dict>\n</plist>\n",
        list(b"5"),
        list(b"01"),
        list(b"2"),
    )
    .into_bytes()
}

/// The directories and files of a well formed package, without metadata.
fn payload() -> Vec<Raw> {
    vec![
        Raw::dir("./opt/"),
        Raw::dir("./opt/nodo/"),
        Raw::dir("./opt/nodo/apps/"),
        Raw::dir("./opt/nodo/apps/shop/"),
        Raw::file("./opt/nodo/apps/shop/manifest.toml", &manifest_for(ID, "1.0")),
        Raw::dir("./opt/nodo/apps/shop/bin/"),
        Raw::file("./opt/nodo/apps/shop/bin/shop", b"#!/bin/true\n").mode(0o755),
        Raw::symlink("./opt/nodo/apps/shop/bin/alias", "shop"),
    ]
}

/// Builds an archive with correct metadata around the given payload.
fn package(payload: Vec<Raw>) -> Vec<u8> {
    package_with_props(payload, &props_for(ID, "1.0_1", ""))
}

fn package_with_props(payload: Vec<Raw>, props: &[u8]) -> Vec<u8> {
    let mut entries = vec![Raw::file("./props.plist", props), Raw::file("./files.plist", &files_plist(&payload))];
    entries.extend(payload);
    compress(&tar_bytes(&entries))
}

fn inspect(archive: &[u8]) -> Inspection {
    inspect_package(archive, &Policy::default())
}

fn violations(archive: &[u8]) -> Vec<Violation> {
    inspect(archive).violations
}

fn with(entry: Raw) -> Vec<u8> {
    let mut payload = payload();
    payload.push(entry);
    package(payload)
}

fn assert_has(found: &[Violation], wanted: &Violation) {
    assert!(found.contains(wanted), "wanted {wanted:?}\nfound {found:#?}");
}

macro_rules! assert_violation {
    ($found:expr, $pattern:pat $(if $guard:expr)?) => {{
        let found = &$found;
        assert!(
            found.iter().any(|violation| matches!(violation, $pattern $(if $guard)?)),
            "wanted {}\nfound {found:#?}",
            stringify!($pattern)
        );
    }};
}

#[test]
fn a_well_formed_package_is_clean() {
    let inspection = inspect(&package(payload()));
    assert_eq!(inspection.violations, []);
    assert!(inspection.is_clean());
    let info = inspection.package.unwrap();
    assert_eq!(info.name, "shop");
    assert_eq!(info.version, "1.0_1");
    assert_eq!(info.architecture, "aarch64");
    assert_eq!(info.run_depends, ["glibc>=2.36_1"]);
    assert_eq!(inspection.manifest.unwrap().id, "shop");
    assert_eq!(inspection.entries, 10);
}

/// `xbps-create` stores no directory entries and lists every parent directory in `files.plist`,
/// as seen in real packages from the Void repository.
#[test]
fn accepts_the_shape_xbps_create_produces() {
    let list = "<dict><key>dirs</key><array>\
        <dict><key>file</key><string>/opt</string></dict>\
        <dict><key>file</key><string>/opt/nodo</string></dict>\
        <dict><key>file</key><string>/opt/nodo/apps</string></dict>\
        <dict><key>file</key><string>/opt/nodo/apps/shop</string></dict>\
        <dict><key>file</key><string>/opt/nodo/apps/shop/bin</string></dict>\
        </array><key>files</key><array>\
        <dict><key>file</key><string>/opt/nodo/apps/shop/manifest.toml</string><key>sha256</key><string>00</string><key>size</key><integer>4</integer></dict>\
        <dict><key>file</key><string>/opt/nodo/apps/shop/bin/shop</string><key>sha256</key><string>00</string><key>size</key><integer>4</integer></dict>\
        </array><key>links</key><array>\
        <dict><key>file</key><string>/opt/nodo/apps/shop/bin/alias</string><key>target</key><string>/opt/nodo/apps/shop/bin/shop</string></dict>\
        </array></dict>";
    let list = format!("<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n{list}\n</plist>\n");
    let entries = vec![
        Raw::file("./props.plist", &props_for(ID, "1.0_1", "")),
        Raw::file("./files.plist", list.as_bytes()),
        Raw::symlink("./opt/nodo/apps/shop/bin/alias", "shop"),
        Raw::file("./opt/nodo/apps/shop/bin/shop", b"#!/bin/true\n").mode(0o755),
        Raw::file("./opt/nodo/apps/shop/manifest.toml", &manifest_for(ID, "1.0")),
    ];
    assert_eq!(violations(&compress(&tar_bytes(&entries))), []);
}

#[test]
fn a_listed_directory_cannot_be_a_file_in_the_archive() {
    let found = violations(&archive_with_files_list(
        "<dict><key>dirs</key><array><dict><key>file</key><string>/opt/nodo/apps/shop/manifest.toml</string></dict></array></dict>",
    ));
    assert_violation!(found, Violation::FilesListMismatch { detail, .. } if detail.contains("different type"));
}

#[test]
fn metadata_may_come_after_the_payload() {
    let mut entries = payload();
    entries.push(Raw::file("./files.plist", &files_plist(&payload())));
    entries.push(Raw::file("./props.plist", &props_for(ID, "1.0_1", "")));
    assert_eq!(violations(&compress(&tar_bytes(&entries))), []);
}

#[test]
fn reports_install_and_remove_scripts() {
    let mut entries = vec![Raw::file("./INSTALL", b"#!/bin/sh\n"), Raw::file("./REMOVE", b"#!/bin/sh\n")];
    entries.extend(payload());
    let files = files_plist(&payload());
    entries.push(Raw::file("./props.plist", &props_for(ID, "1.0_1", "")));
    entries.push(Raw::file("./files.plist", &files));
    let found = violations(&compress(&tar_bytes(&entries)));
    assert_has(&found, &Violation::InstallScript { path: "INSTALL".into() });
    assert_has(&found, &Violation::RemoveScript { path: "REMOVE".into() });
}

#[test]
fn even_an_empty_script_counts() {
    let mut entries = vec![Raw::file("INSTALL", b"")];
    entries.extend(payload());
    entries.push(Raw::file("./props.plist", &props_for(ID, "1.0_1", "")));
    entries.push(Raw::file("./files.plist", &files_plist(&payload())));
    assert_violation!(violations(&compress(&tar_bytes(&entries))), Violation::InstallScript { .. });
}

#[test]
fn reports_setuid_setgid_and_sticky() {
    let path = "./opt/nodo/apps/shop/bin/evil";
    let shown = || "opt/nodo/apps/shop/bin/evil".to_string();
    assert_has(&violations(&with(Raw::file(path, b"x").mode(0o4755))), &Violation::Setuid { path: shown() });
    assert_has(&violations(&with(Raw::file(path, b"x").mode(0o2755))), &Violation::Setgid { path: shown() });
    assert_has(&violations(&with(Raw::file(path, b"x").mode(0o1755))), &Violation::Sticky { path: shown() });
    let both = violations(&with(Raw::file(path, b"x").mode(0o6755)));
    assert_has(&both, &Violation::Setuid { path: shown() });
    assert_has(&both, &Violation::Setgid { path: shown() });
}

#[test]
fn reports_setuid_directories_and_hard_links() {
    assert_violation!(violations(&with(Raw::dir("./opt/nodo/apps/shop/d/").mode(0o2755))), Violation::Setgid { .. });
    let link = Raw::hardlink("./opt/nodo/apps/shop/l", "./opt/nodo/apps/shop/bin/shop").mode(0o4755);
    assert_violation!(violations(&with(link)), Violation::Setuid { .. });
}

#[test]
fn reports_world_writable_files_but_not_symlinks() {
    let found = violations(&with(Raw::file("./opt/nodo/apps/shop/w", b"x").mode(0o666)));
    assert_violation!(found, Violation::WorldWritable { .. });
    let found = violations(&with(Raw::dir("./opt/nodo/apps/shop/d/").mode(0o777)));
    assert_violation!(found, Violation::WorldWritable { .. });
    assert_eq!(violations(&package(payload())), [], "the payload's 0777 symlink is fine");
}

#[test]
fn reports_foreign_owners() {
    let mut file = Raw::file("./opt/nodo/apps/shop/f", b"x");
    file.uid = 1000;
    file.gid = 1001;
    assert_violation!(violations(&with(file)), Violation::UnexpectedOwner { uid: 1000, gid: 1001, .. });
}

#[test]
fn reports_device_nodes_and_fifos() {
    let found = violations(&with(Raw::of_type("./opt/nodo/apps/shop/mem", b'3')));
    assert_violation!(found, Violation::DeviceNode { .. });
    let found = violations(&with(Raw::of_type("./opt/nodo/apps/shop/sda", b'4')));
    assert_violation!(found, Violation::DeviceNode { .. });
    let found = violations(&with(Raw::of_type("./opt/nodo/apps/shop/pipe", b'6')));
    assert_violation!(found, Violation::Fifo { .. });
}

#[test]
fn reports_unknown_entry_types() {
    let found = violations(&with(Raw { body: b"data".to_vec(), ..Raw::of_type("./opt/nodo/apps/shop/q", b'Z') }));
    assert_violation!(found, Violation::UnknownEntryType { kind: b'Z', .. });
}

#[test]
fn reports_absolute_paths() {
    assert_has(&violations(&with(Raw::file("/etc/shadow", b"x"))), &Violation::AbsolutePath { path: "/etc/shadow".into() });
}

#[test]
fn reports_parent_traversal() {
    for path in [
        "../../etc/passwd",
        "./../etc/passwd",
        "./opt/nodo/apps/shop/../../../../etc/passwd",
        "./opt/nodo/apps/shop/..",
        "..",
    ] {
        assert_violation!(violations(&with(Raw::file(path, b"x"))), Violation::ParentTraversal { .. });
    }
}

#[test]
fn reports_non_canonical_paths() {
    for path in [
        "./opt/nodo/apps/shop//double",
        "./opt/nodo/apps/shop/./dot",
        "././opt/nodo/apps/shop/dot",
        "./opt/nodo/apps/shop/file/",
    ] {
        let found = violations(&with(Raw::file(path, b"x")));
        assert_violation!(found, Violation::NonCanonicalPath { .. } | Violation::OutsideAppDir { .. });
    }
    assert_violation!(violations(&with(Raw::file("", b"x"))), Violation::NonCanonicalPath { .. });
}

#[test]
fn reports_unsafe_path_characters_and_escapes_them_for_display() {
    let mut raw = Raw::file("x", b"x");
    raw.name = b"./opt/nodo/apps/shop/\x1b[2Jbad\nname".to_vec();
    let found = violations(&with(raw));
    let Some(Violation::UnsafePathCharacters { path }) =
        found.iter().find(|v| matches!(v, Violation::UnsafePathCharacters { .. }))
    else {
        panic!("{found:#?}");
    };
    assert!(!path.contains('\x1b') && !path.contains('\n'), "{path:?}");

    let mut raw = Raw::file("x", b"x");
    raw.name = b"./opt/nodo/apps/shop/\xff\xfe".to_vec();
    assert_violation!(violations(&with(raw)), Violation::UnsafePathCharacters { .. });
    assert_violation!(violations(&with(Raw::file("./opt/nodo/apps/shop/a\\b", b"x"))), Violation::UnsafePathCharacters { .. });
}

#[test]
fn every_display_is_free_of_control_characters() {
    let mut raw = Raw::file("x", b"x");
    raw.name = b"/etc/\x1b[2J\n".to_vec();
    for violation in violations(&with(raw)) {
        assert!(!violation.to_string().chars().any(char::is_control), "{violation:?}");
    }
}

#[test]
fn reports_paths_outside_the_app_directory() {
    for path in [
        "./etc/passwd",
        "./usr/bin/sudo",
        "./opt/other",
        "./opt/nodo/x",
        "./opt/nodo/apps/other/x",
        "./opt/nodo/apps/shopper/x",
        "./opt/nodo/apps/shop2",
        "./INSTALL.msg",
        "./README",
    ] {
        assert_violation!(violations(&with(Raw::file(path, b"x"))), Violation::OutsideAppDir { .. });
    }
}

#[test]
fn shared_directories_must_stay_plain() {
    for (name, mode) in [("./opt/", 0o777), ("./opt/nodo/", 0o700), ("./opt/nodo/apps/", 0o775), ("./opt/nodo/apps/shop/", 0o775), ("./", 0o700)] {
        let mut entries: Vec<Raw> = payload().into_iter().filter(|e| e.name != name.as_bytes()).collect();
        entries.push(Raw::dir(name).mode(mode));
        assert_violation!(violations(&package(entries)), Violation::AncestorChanged { .. });
    }
}

#[test]
fn the_app_directory_cannot_be_a_symlink_or_file() {
    let entries: Vec<Raw> = payload().into_iter().filter(|e| e.name != b"./opt/nodo/apps/shop/").collect();
    let mut with_link = entries.clone();
    with_link.push(Raw::symlink("./opt/nodo/apps/shop", "/etc"));
    let found = violations(&package(with_link));
    assert_violation!(found, Violation::AncestorChanged { .. });
    assert_violation!(found, Violation::SymlinkEscape { .. });
    assert_violation!(found, Violation::PathThroughSymlink { .. });

    let mut opt_link: Vec<Raw> = payload().into_iter().filter(|e| e.name != b"./opt/").collect();
    opt_link.push(Raw::symlink("./opt", "/etc"));
    assert_violation!(violations(&package(opt_link)), Violation::AncestorChanged { .. });
}

#[test]
fn reports_symlinks_escaping_the_app_directory() {
    for (path, target) in [
        ("./opt/nodo/apps/shop/l", "/etc/passwd"),
        ("./opt/nodo/apps/shop/l", "/opt/nodo/apps/other"),
        ("./opt/nodo/apps/shop/l", "/opt/nodo/apps/shopper/x"),
        ("./opt/nodo/apps/shop/l", "/opt/nodo/apps/shop/../other"),
        ("./opt/nodo/apps/shop/l", "/opt/nodo/apps/shop/./x"),
        ("./opt/nodo/apps/shop/l", ".."),
        ("./opt/nodo/apps/shop/l", "../other/x"),
        ("./opt/nodo/apps/shop/l", "../../../../../../etc/passwd"),
        ("./opt/nodo/apps/shop/bin/l", "../../other"),
        ("./opt/nodo/apps/shop/l", "a/../../other"),
        ("./opt/nodo/apps/shop/l", "bin/../.."),
        ("./opt/nodo/apps/shop/l", ""),
        ("./opt/nodo/apps/shop/l", "./x/../.."),
        ("./opt/nodo/apps/shop/l", "x\ny"),
    ] {
        assert_violation!(violations(&with(Raw::symlink(path, target))), Violation::SymlinkEscape { .. });
    }
}

#[test]
fn accepts_symlinks_inside_the_app_directory() {
    for (path, target) in [
        ("./opt/nodo/apps/shop/l", "bin/shop"),
        ("./opt/nodo/apps/shop/bin/l", "../manifest.toml"),
        ("./opt/nodo/apps/shop/bin/l", ".."),
        ("./opt/nodo/apps/shop/l", "/opt/nodo/apps/shop/bin/shop"),
        ("./opt/nodo/apps/shop/l", "/opt/nodo/apps/shop"),
    ] {
        let found = violations(&with(Raw::symlink(path, target)));
        assert_eq!(found, [], "{path} -> {target}");
    }
}

#[test]
fn a_symlink_cannot_be_used_as_a_directory() {
    // Lexically `d/x` -> `../y` stays inside. Physically `d` is the package root, so `..` is the
    // shared apps directory and the link reaches another app.
    let mut entries = payload();
    entries.push(Raw::symlink("./opt/nodo/apps/shop/d", "."));
    let found = violations(&with_all(entries.clone(), Raw::symlink("./opt/nodo/apps/shop/d/x", "../y")));
    assert_violation!(found, Violation::PathThroughSymlink { .. });

    // The same trick with a file written through the link, in either order.
    let found = violations(&with_all(entries, Raw::file("./opt/nodo/apps/shop/d/x", b"x")));
    assert_violation!(found, Violation::PathThroughSymlink { .. });
    let mut reversed = vec![Raw::file("./opt/nodo/apps/shop/d/x", b"x")];
    reversed.extend(payload());
    reversed.push(Raw::symlink("./opt/nodo/apps/shop/d", "/etc"));
    assert_violation!(violations(&package(reversed)), Violation::PathThroughSymlink { .. });
}

#[test]
fn a_symlink_target_cannot_climb_through_another_symlink() {
    // `a` is the package root, so `a/../x` is physically another app even though it reads as inside.
    let mut entries = payload();
    entries.push(Raw::symlink("./opt/nodo/apps/shop/a", "."));
    let found = violations(&with_all(entries, Raw::symlink("./opt/nodo/apps/shop/l", "a/../x")));
    assert_violation!(found, Violation::SymlinkEscape { .. });
}

fn with_all(mut entries: Vec<Raw>, extra: Raw) -> Vec<u8> {
    entries.push(extra);
    package(entries)
}

#[test]
fn reports_hard_links_outside_the_package() {
    for target in [
        "/etc/shadow",
        "./etc/shadow",
        "etc/shadow",
        "../../etc/shadow",
        "./opt/nodo/apps/other/x",
        "./opt/nodo/apps/shop/not-in-archive",
        "./opt/nodo/apps/shop/bin",
        "./opt/nodo/apps/shop/bin/alias",
    ] {
        let found = violations(&with(Raw::hardlink("./opt/nodo/apps/shop/l", target)));
        assert_violation!(found, Violation::HardlinkEscape { .. });
    }
}

#[test]
fn accepts_a_hard_link_to_an_earlier_file() {
    assert_eq!(violations(&with(Raw::hardlink("./opt/nodo/apps/shop/l", "./opt/nodo/apps/shop/bin/shop"))), []);
}

#[test]
fn a_hard_link_cannot_point_at_a_later_file() {
    let mut entries = vec![Raw::hardlink("./opt/nodo/apps/shop/l", "./opt/nodo/apps/shop/bin/shop")];
    entries.extend(payload());
    assert_violation!(violations(&package(entries)), Violation::HardlinkEscape { .. });
}

#[test]
fn reports_duplicate_paths() {
    let replaced = Raw::symlink("./opt/nodo/apps/shop/bin/shop", "/usr/bin/sudo");
    assert_violation!(violations(&with(replaced)), Violation::DuplicatePath { .. });
    let spelled_differently = Raw::file("opt/nodo/apps/shop/bin/shop", b"other");
    assert_violation!(violations(&with(spelled_differently)), Violation::DuplicatePath { .. });
    let mut twice = vec![Raw::file("./props.plist", &props_for("evil", "1.0_1", ""))];
    twice.push(Raw::file("./props.plist", &props_for(ID, "1.0_1", "")));
    twice.extend(payload());
    twice.push(Raw::file("./files.plist", &files_plist(&payload())));
    assert_violation!(violations(&compress(&tar_bytes(&twice))), Violation::DuplicatePath { .. });
}

#[test]
fn reports_base_system_names() {
    for name in ["glibc", "xbps", "runit", "sudo", "openssh", "bash", "base-system", "base-files", "linux", "linux6-6", "linux-firmware", "cuprated", "monerod", "monokulo-base", "nodo-helper"] {
        let props = props_for(name, "1.0_1", "");
        let found = violations(&package_with_props(payload(), &props));
        assert_has(&found, &Violation::BasePackageName { name: name.into() });
    }
}

#[test]
fn accepts_names_that_only_resemble_base_packages() {
    for name in ["glibcish", "sedan", "tarball", "basement"] {
        assert!(!is_base_package(name, &Policy::default()), "{name}");
    }
}

#[test]
fn the_deny_list_can_be_extended() {
    let policy = Policy { denied_packages: vec!["shop".into()], ..Policy::default() };
    let found = inspect_package(package(payload()).as_slice(), &policy).violations;
    assert_has(&found, &Violation::BasePackageName { name: "shop".into() });
}

#[test]
fn reports_invalid_package_names() {
    for name in ["Shop", "sh_op", "sh.op", "../x", "a b", "root"] {
        let props = props_for(name, "1.0_1", "");
        assert_violation!(violations(&package_with_props(payload(), &props)), Violation::InvalidPackageName { .. });
    }
}

#[test]
fn reports_a_package_name_not_matching_the_manifest() {
    let mut entries: Vec<Raw> = payload().into_iter().filter(|e| e.name != b"./opt/nodo/apps/shop/manifest.toml").collect();
    entries.push(Raw::file("./opt/nodo/apps/shop/manifest.toml", &manifest_for("other", "1.0")));
    let found = violations(&package(entries));
    assert_has(&found, &Violation::PackageNameMismatch { package: "shop".into(), manifest: "other".into() });
}

#[test]
fn reports_a_version_not_matching_the_manifest() {
    let props = props_for(ID, "2.0_1", "");
    let found = violations(&package_with_props(payload(), &props));
    assert_has(&found, &Violation::VersionMismatch { package: "2.0_1".into(), manifest: "1.0".into() });
}

#[test]
fn reports_a_missing_or_invalid_manifest() {
    let entries: Vec<Raw> = payload().into_iter().filter(|e| e.name != b"./opt/nodo/apps/shop/manifest.toml").collect();
    assert_has(&violations(&package(entries.clone())), &Violation::ManifestMissing);

    let mut bad = entries.clone();
    bad.push(Raw::file("./opt/nodo/apps/shop/manifest.toml", b"not toml ="));
    assert_violation!(violations(&package(bad)), Violation::ManifestInvalid { .. });

    let mut escaping = entries.clone();
    escaping.push(Raw::file(
        "./opt/nodo/apps/shop/manifest.toml",
        b"api_version = 1\nid = \"shop\"\nversion = \"1.0\"\nexec = [\"/bin/sh\"]\n",
    ));
    assert_violation!(violations(&package(escaping)), Violation::ManifestInvalid { .. });

    let mut binary = entries.clone();
    binary.push(Raw::file("./opt/nodo/apps/shop/manifest.toml", b"\xff\xfe"));
    assert_violation!(violations(&package(binary)), Violation::ManifestInvalid { .. });

    let mut link = entries;
    link.push(Raw::symlink("./opt/nodo/apps/shop/manifest.toml", "bin/shop"));
    assert_has(&violations(&package(link)), &Violation::ManifestMissing);
}

#[test]
fn a_manifest_in_another_directory_does_not_count() {
    let mut entries: Vec<Raw> = payload().into_iter().filter(|e| e.name != b"./opt/nodo/apps/shop/manifest.toml").collect();
    entries.push(Raw::file("./opt/nodo/apps/other/manifest.toml", &manifest_for(ID, "1.0")));
    let found = violations(&package(entries));
    assert_has(&found, &Violation::ManifestMissing);
    assert_violation!(found, Violation::OutsideAppDir { .. });
}

#[test]
fn reports_properties_that_act_outside_the_package() {
    for key in ["alternatives", "replaces", "provides", "reverts", "shlib-provides", "install-script", "remove-script", "install-msg", "remove-msg"] {
        let extra = format!("<key>{key}</key>\n<array>\n<string>sh:/usr/bin/sh:/opt/x</string>\n</array>\n");
        let props = props_for(ID, "1.0_1", &extra);
        assert_has(&violations(&package_with_props(payload(), &props)), &Violation::ForbiddenProperty { key: key.into() });
    }
}

#[test]
fn reports_conf_files_outside_the_package() {
    let extra = "<key>conf_files</key>\n<array>\n<string>/etc/passwd</string>\n</array>\n";
    assert_violation!(violations(&package_with_props(payload(), &props_for(ID, "1.0_1", extra))), Violation::FilesListMismatch { .. });
    let extra = "<key>conf_files</key>\n<array>\n<string>/opt/nodo/apps/shop/../../x</string>\n</array>\n";
    assert_violation!(violations(&package_with_props(payload(), &props_for(ID, "1.0_1", extra))), Violation::FilesListMismatch { .. });
    let extra = "<key>conf_files</key>\n<array>\n<string>/opt/nodo/apps/shop/shop.conf</string>\n</array>\n";
    assert_eq!(violations(&package_with_props(payload(), &props_for(ID, "1.0_1", extra))), []);
}

fn archive_with_files_list(files: &str) -> Vec<u8> {
    let list = format!("<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n{files}\n</plist>\n");
    let mut entries = vec![Raw::file("./props.plist", &props_for(ID, "1.0_1", "")), Raw::file("./files.plist", list.as_bytes())];
    entries.extend(payload());
    compress(&tar_bytes(&entries))
}

#[test]
fn a_files_list_cannot_name_system_files_for_removal() {
    for (list, file) in [("files", "/usr/bin/sudo"), ("links", "/bin/sh"), ("dirs", "/etc"), ("conf_files", "/etc/passwd")] {
        let found = violations(&archive_with_files_list(&format!(
            "<dict><key>{list}</key><array><dict><key>file</key><string>{file}</string></dict></array></dict>"
        )));
        assert_violation!(found, Violation::FilesListMismatch { path, .. } if path == file);
    }
}

#[test]
fn a_files_list_must_match_the_payload() {
    let found = violations(&archive_with_files_list(
        "<dict><key>files</key><array><dict><key>file</key><string>/opt/nodo/apps/shop/ghost</string></dict></array></dict>",
    ));
    assert_violation!(found, Violation::FilesListMismatch { detail, .. } if detail.contains("not in the archive"));
    assert_violation!(found, Violation::FilesListMismatch { detail, .. } if detail.contains("not listed"));

    let found = violations(&archive_with_files_list(
        "<dict><key>links</key><array><dict><key>file</key><string>/opt/nodo/apps/shop/manifest.toml</string></dict></array></dict>",
    ));
    assert_violation!(found, Violation::FilesListMismatch { detail, .. } if detail.contains("different type"));

    let found = violations(&archive_with_files_list(
        "<dict><key>files</key><array><dict><key>file</key><string>relative</string></dict></array></dict>",
    ));
    assert_violation!(found, Violation::FilesListMismatch { detail, .. } if detail.contains("not an absolute"));

    let found = violations(&archive_with_files_list("<dict><key>surprise</key><array/></dict>"));
    assert_violation!(found, Violation::MetadataInvalid { file, .. } if file == "files.plist");
}

#[test]
fn reports_missing_and_broken_metadata() {
    let found = violations(&compress(&tar_bytes(&payload())));
    assert_has(&found, &Violation::MetadataMissing { file: "props.plist".into() });
    assert_has(&found, &Violation::MetadataMissing { file: "files.plist".into() });

    let found = violations(&package_with_props(payload(), b"not a plist"));
    assert_violation!(found, Violation::MetadataInvalid { file, .. } if file == "props.plist");

    let no_name = b"<plist><dict><key>pkgver</key><string>a-1_1</string></dict></plist>";
    assert_violation!(violations(&package_with_props(payload(), no_name)), Violation::MetadataInvalid { .. });

    let two_names = b"<plist><dict><key>pkgname</key><string>shop</string><key>pkgname</key><string>glibc</string></dict></plist>";
    assert_violation!(violations(&package_with_props(payload(), two_names)), Violation::MetadataInvalid { .. });

    let wrong_pkgver = b"<plist><dict><key>pkgname</key><string>shop</string><key>pkgver</key><string>glibc-1_1</string><key>architecture</key><string>x</string></dict></plist>";
    assert_violation!(violations(&package_with_props(payload(), wrong_pkgver)), Violation::MetadataInvalid { detail, .. } if detail.contains("pkgver"));

    let symlinked = vec![Raw::symlink("./props.plist", "/etc/passwd"), Raw::file("./files.plist", &files_plist(&payload()))];
    assert_has(&violations(&compress(&tar_bytes(&symlinked))), &Violation::MetadataNotRegular { path: "props.plist".into() });
}

#[test]
fn pkgver_and_version_must_agree_and_have_a_revision() {
    let plist = |pkgver: &str, version: &str| {
        format!(
            "<plist><dict><key>architecture</key><string>x</string><key>pkgname</key><string>shop</string><key>pkgver</key><string>{pkgver}</string><key>version</key><string>{version}</string></dict></plist>"
        )
    };
    for (pkgver, version) in [("shop-1.0_1", "9.9_9"), ("shop-1.0", "1.0"), ("shop-1.0_", "1.0_"), ("shop-1.0_x", "1.0_x")] {
        let found = violations(&package_with_props(payload(), plist(pkgver, version).as_bytes()));
        assert_violation!(found, Violation::MetadataInvalid { file, .. } if file == "props.plist");
    }
}

#[test]
fn package_info_is_safe_to_display() {
    let props = "<plist><dict><key>architecture</key><string>x\u{1b}[2J</string><key>pkgname</key><string>shop</string><key>pkgver</key><string>shop-1.0_1</string><key>run_depends</key><array><string>a\u{1b}b</string></array></dict></plist>";
    let info = inspect(&package_with_props(payload(), props.as_bytes())).package.unwrap();
    assert!(!info.architecture.contains('\u{1b}'));
    assert!(!info.run_depends[0].contains('\u{1b}'));
}

#[test]
fn checks_still_apply_when_no_package_name_is_known() {
    let mut entries = payload();
    entries.push(Raw::file("/etc/shadow", b"x"));
    entries.push(Raw::file("./opt/nodo/apps/any/f", b"x").mode(0o4755));
    entries.push(Raw::file("./usr/bin/x", b"x"));
    let found = violations(&compress(&tar_bytes(&entries)));
    assert_violation!(found, Violation::AbsolutePath { .. });
    assert_violation!(found, Violation::Setuid { .. });
    assert_violation!(found, Violation::OutsideAppDir { .. });
}

#[test]
fn reports_every_violation_rather_than_only_the_first() {
    let mut entries = vec![Raw::file("./INSTALL", b"x"), Raw::file("./REMOVE", b"x")];
    entries.extend(payload());
    entries.extend([
        Raw::file("/etc/shadow", b"x"),
        Raw::file("../up", b"x"),
        Raw::file("./opt/nodo/apps/shop/su", b"x").mode(0o4755),
        Raw::file("./opt/nodo/apps/shop/sg", b"x").mode(0o2755),
        Raw::of_type("./opt/nodo/apps/shop/mem", b'3'),
        Raw::symlink("./opt/nodo/apps/shop/esc", "/etc/passwd"),
        Raw::file("./usr/bin/x", b"x"),
    ]);
    let list = "<plist><dict><key>files</key><array><dict><key>file</key><string>/etc/passwd</string></dict></array></dict></plist>";
    entries.push(Raw::file("./props.plist", &props_for("glibc", "1.0_1", "<key>replaces</key><array/>")));
    entries.push(Raw::file("./files.plist", list.as_bytes()));
    let found = violations(&compress(&tar_bytes(&entries)));

    assert_violation!(found, Violation::InstallScript { .. });
    assert_violation!(found, Violation::RemoveScript { .. });
    assert_violation!(found, Violation::AbsolutePath { .. });
    assert_violation!(found, Violation::ParentTraversal { .. });
    assert_violation!(found, Violation::Setuid { .. });
    assert_violation!(found, Violation::Setgid { .. });
    assert_violation!(found, Violation::DeviceNode { .. });
    assert_violation!(found, Violation::SymlinkEscape { .. });
    assert_violation!(found, Violation::OutsideAppDir { .. });
    assert_violation!(found, Violation::BasePackageName { .. });
    assert_violation!(found, Violation::ForbiddenProperty { .. });
    assert_violation!(found, Violation::FilesListMismatch { .. });
}

fn small_policy(max_decompressed_bytes: u64) -> Policy {
    Policy { limits: InspectLimits { max_decompressed_bytes, ..InspectLimits::default() }, ..Policy::default() }
}

#[test]
fn stops_a_decompression_bomb() {
    let zeros = vec![0u8; 8 * 1024 * 1024];
    let bomb = Raw::file("./opt/nodo/apps/shop/bomb", &zeros);
    let mut entries = payload();
    entries.push(bomb);
    let archive = package(entries);
    assert!(archive.len() < 16 * 1024, "the crafted archive should be tiny, got {}", archive.len());
    let limit = 1024 * 1024;
    let inspection = inspect_package(archive.as_slice(), &small_policy(limit));
    assert_has(&inspection.violations, &Violation::DecompressedTooLarge { limit });
    assert!(inspection.decompressed_bytes <= limit + 256 * 1024, "stopped late: {}", inspection.decompressed_bytes);
}

#[test]
fn stops_a_bomb_hidden_after_the_end_marker() {
    let mut tar = tar_bytes(&payload());
    tar.resize(tar.len() + 8 * 1024 * 1024, 0);
    let limit = 1024 * 1024;
    let found = inspect_package(compress(&tar).as_slice(), &small_policy(limit)).violations;
    assert_has(&found, &Violation::DecompressedTooLarge { limit });
}

#[test]
fn refuses_a_false_size_without_reading_or_allocating_it() {
    let mut raw = Raw::file("./opt/nodo/apps/shop/huge", b"x");
    raw.claimed_size = Some(0o77777777777);
    let limit = 1024 * 1024;
    let inspection = inspect_package(with(raw).as_slice(), &small_policy(limit));
    assert_has(&inspection.violations, &Violation::DecompressedTooLarge { limit });
}

#[test]
fn a_pax_size_is_subject_to_the_same_limit() {
    let mut raw = Raw::file("./opt/nodo/apps/shop/huge", b"x");
    raw.pax = vec![("size".into(), b"18446744073709551615".to_vec())];
    assert_violation!(violations(&with(raw)), Violation::DecompressedTooLarge { .. });
}

#[test]
fn oversized_metadata_is_not_held_in_memory() {
    let big = vec![b' '; 2 * 1024 * 1024];
    let policy = Policy { limits: InspectLimits { max_metadata_bytes: 1024 * 1024, ..InspectLimits::default() }, ..Policy::default() };
    let archive = package_with_props(payload(), &big);
    let found = inspect_package(archive.as_slice(), &policy).violations;
    assert_violation!(found, Violation::MetadataTooLarge { .. });
}

#[test]
fn many_manifests_share_one_metadata_budget() {
    let policy = Policy { limits: InspectLimits { max_metadata_bytes: 1024 * 1024, ..InspectLimits::default() }, ..Policy::default() };
    let mut entries = payload();
    for index in 0..4 {
        entries.push(Raw::file(&format!("./opt/nodo/apps/s{index}/manifest.toml"), &vec![b' '; 512 * 1024]));
    }
    let found = inspect_package(package(entries).as_slice(), &policy).violations;
    assert_violation!(found, Violation::MetadataTooLarge { .. });
}

#[test]
fn limits_the_number_of_entries() {
    let policy = Policy { limits: InspectLimits { max_entries: 5, ..InspectLimits::default() }, ..Policy::default() };
    let found = inspect_package(package(payload()).as_slice(), &policy).violations;
    assert_has(&found, &Violation::TooManyEntries { limit: 5 });
}

#[test]
fn refuses_oversized_zstd_windows() {
    // A valid frame whose header asks for a 256 MiB window, which a decoder allocates up front.
    let mut entries = vec![Raw::file("./props.plist", &props_for(ID, "1.0_1", "")), Raw::file("./files.plist", &files_plist(&payload()))];
    entries.extend(payload());
    let tar = tar_bytes(&entries);
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    encoder.window_log(28).unwrap();
    std::io::Write::write_all(&mut encoder, &tar).unwrap();
    let archive = encoder.finish().unwrap();

    assert_has(&violations(&archive), &Violation::NotZstd);

    let generous = Policy { limits: InspectLimits { window_log_max: 28, ..InspectLimits::default() }, ..Policy::default() };
    assert_eq!(inspect_package(archive.as_slice(), &generous).violations, [], "the frame itself is valid");
}

#[test]
fn refuses_non_zstd_input() {
    for data in [&b""[..], b"not zstd at all, just some text", &[0x1f, 0x8b, 0x08, 0x00], &[0xfd, b'7', b'z', b'X', b'Z', 0x00]] {
        let found = violations(data);
        assert_violation!(found, Violation::NotZstd | Violation::Truncated);
        assert_has(&found, &Violation::MetadataMissing { file: "props.plist".into() });
    }
}

#[test]
fn refuses_an_uncompressed_tar() {
    assert_violation!(violations(&tar_bytes(&payload())), Violation::NotZstd | Violation::Truncated);
}

#[test]
fn accepts_several_zstd_frames_as_libarchive_does() {
    let tar = {
        let mut entries = vec![Raw::file("./props.plist", &props_for(ID, "1.0_1", "")), Raw::file("./files.plist", &files_plist(&payload()))];
        entries.extend(payload());
        tar_bytes(&entries)
    };
    let (first, second) = tar.split_at(1536);
    let mut archive = compress(first);
    archive.extend_from_slice(&compress(second));
    assert_eq!(violations(&archive), []);
}

#[test]
fn refuses_garbage_after_the_zstd_stream() {
    let mut archive = package(payload());
    archive.extend_from_slice(b"trailing garbage that is not a frame");
    assert!(!inspect(&archive).is_clean());
}

#[test]
fn reports_a_truncated_compressed_stream() {
    let archive = package(payload());
    let cut = &archive[..archive.len() / 2];
    assert!(!inspect(cut).is_clean());
}

#[test]
fn reports_a_truncated_tar() {
    let tar = tar_bytes(&payload());
    assert_violation!(violations(&compress(&tar[..tar.len() - 1024])), Violation::Truncated);
    assert_violation!(violations(&compress(&tar[..700])), Violation::Truncated);
    assert_violation!(violations(&compress(&tar[..512])), Violation::Truncated);
}

#[test]
fn reports_a_body_shorter_than_its_header_claims() {
    let mut out = Vec::new();
    let mut raw = Raw::file("./opt/nodo/apps/shop/f", b"short");
    raw.claimed_size = Some(4096);
    write_entry(&mut out, &raw);
    assert_violation!(violations(&compress(&out)), Violation::Truncated);
}

#[test]
fn reports_entries_hidden_after_the_end_marker() {
    let mut tar = tar_bytes(&payload());
    write_entry(&mut tar, &Raw::file("./etc/shadow", b"x"));
    assert_violation!(violations(&compress(&tar)), Violation::Malformed { detail } if detail.contains("after the end"));
}

#[test]
fn a_single_zero_block_ends_the_archive_as_it_does_for_libarchive() {
    let mut tar = Vec::new();
    for entry in payload() {
        write_entry(&mut tar, &entry);
    }
    tar.extend_from_slice(&[0u8; 512]);
    write_entry(&mut tar, &Raw::file("./etc/shadow", b"x"));
    tar.extend_from_slice(&[0u8; 1024]);
    assert_violation!(violations(&compress(&tar)), Violation::Malformed { detail } if detail.contains("after the end"));
}

#[test]
fn reports_bad_checksums() {
    let mut block = header_block(&Raw::file("./x", b""), 0, b'0');
    block[0] ^= 1;
    let mut tar = block.to_vec();
    tar.extend_from_slice(&[0u8; 1024]);
    assert_violation!(violations(&compress(&tar)), Violation::Malformed { detail } if detail.contains("checksum"));
}

fn single_block(modify: impl Fn(&mut [u8; 512])) -> Vec<u8> {
    let mut block = header_block(&Raw::file("./opt/nodo/apps/shop/x", b""), 0, b'0');
    modify(&mut block);
    set_checksum(&mut block);
    let mut tar = block.to_vec();
    tar.extend_from_slice(&[0u8; 1024]);
    compress(&tar)
}

#[test]
fn refuses_other_tar_dialects() {
    let found = violations(&single_block(|block| block[257..265].copy_from_slice(b"ustar  \0")));
    assert_violation!(found, Violation::UnsupportedFeature { .. });
    let found = violations(&single_block(|block| block[257..265].fill(0)));
    assert_violation!(found, Violation::UnsupportedFeature { .. });
}

#[test]
fn refuses_gnu_extensions() {
    for flag in *b"LKS" {
        assert_violation!(violations(&with(Raw::of_type("././@LongLink", flag))), Violation::UnsupportedFeature { .. });
    }
}

#[test]
fn refuses_base_256_and_non_octal_numbers() {
    let found = violations(&single_block(|block| {
        block[124] = 0x80;
    }));
    assert_violation!(found, Violation::UnsupportedFeature { .. });
    let found = violations(&single_block(|block| {
        block[124..136].copy_from_slice(b"00000000009\0");
    }));
    assert_violation!(found, Violation::Malformed { .. });
    let found = violations(&single_block(|block| {
        block[124..136].copy_from_slice(b"0000 000001\0");
    }));
    assert_violation!(found, Violation::Malformed { .. });
}

#[test]
fn refuses_links_and_directories_with_bodies() {
    for type_flag in *b"123456" {
        let raw = Raw { body: b"hidden".to_vec(), ..Raw::of_type("./opt/nodo/apps/shop/x", type_flag) };
        assert_violation!(violations(&with(raw)), Violation::Malformed { detail } if detail.contains("body"));
    }
}

#[test]
fn honours_pax_paths() {
    // A safe looking ustar name with a pax path pointing elsewhere must be judged by the pax path,
    // because that is what the extractor will use.
    let mut raw = Raw::file("./opt/nodo/apps/shop/innocent", b"x");
    raw.pax = vec![("path".into(), b"../../etc/shadow".to_vec())];
    assert_violation!(violations(&with(raw)), Violation::ParentTraversal { .. });

    let mut raw = Raw::symlink("./opt/nodo/apps/shop/l", "inside");
    raw.pax = vec![("linkpath".into(), b"/etc/shadow".to_vec())];
    assert_violation!(violations(&with(raw)), Violation::SymlinkEscape { .. });

    let mut raw = Raw::file("./opt/nodo/apps/shop/f", b"x");
    raw.pax = vec![("uid".into(), b"1000".to_vec())];
    assert_violation!(violations(&with(raw)), Violation::UnexpectedOwner { uid: 1000, .. });
}

#[test]
fn long_paths_work_through_pax() {
    let long = format!("{DIR}/{}/file", "d".repeat(150));
    let mut raw = Raw::file("./opt/nodo/apps/shop/short", b"x");
    raw.pax = vec![("path".into(), long.into_bytes())];
    let found = violations(&with(raw));
    // Only the absence from files.plist differs, since the test helper lists the ustar name.
    assert!(found.iter().all(|v| matches!(v, Violation::FilesListMismatch { .. })), "{found:#?}");
}

#[test]
fn ustar_prefix_is_honoured() {
    let mut block = header_block(&Raw::file("shadow", b""), 0, b'0');
    block[345..348].copy_from_slice(b"etc");
    set_checksum(&mut block);
    let mut tar = block.to_vec();
    tar.extend_from_slice(&[0u8; 1024]);
    assert_has(&violations(&compress(&tar)), &Violation::OutsideAppDir { path: "etc/shadow".into() });
}

#[test]
fn reports_extended_attributes() {
    for key in ["SCHILY.xattr.security.capability", "LIBARCHIVE.xattr.security.capability", "RHT.security.selinux", "SCHILY.acl.access", "GNU.sparse.map", "hdrcharset"] {
        let mut raw = Raw::file("./opt/nodo/apps/shop/f", b"x");
        raw.pax = vec![(key.into(), b"value".to_vec())];
        assert_violation!(violations(&with(raw)), Violation::UnsupportedPaxKey { key: found, .. } if found == key);
    }
}

#[test]
fn ignores_harmless_pax_keys() {
    let mut raw = Raw::file("./opt/nodo/apps/shop/f", b"x");
    raw.pax = vec![("mtime".into(), b"1700000000.5".to_vec()), ("uname".into(), b"root".to_vec())];
    assert_eq!(violations(&with(raw)), []);
}

#[test]
fn refuses_malformed_pax_records() {
    for body in [&b"nonsense"[..], b"5 x=y\n", b"999 path=x\n", b"10 pathx\n\n\n", b"12 path=x\0y\n"] {
        let mut out = Vec::new();
        out.extend_from_slice(&header_block(&Raw::file("PaxHeader/x", b""), body.len() as u64, b'x'));
        out.extend_from_slice(body);
        pad(&mut out);
        write_entry(&mut out, &Raw::file("./opt/nodo/apps/shop/f", b"x"));
        out.extend_from_slice(&[0u8; 1024]);
        assert_violation!(violations(&compress(&out)), Violation::Malformed { .. });
    }
}

#[test]
fn refuses_pax_structure_tricks() {
    let empty = Raw::file("PaxHeader/x", b"");
    let mut twice = Vec::new();
    twice.extend_from_slice(&header_block(&empty, 0, b'x'));
    twice.extend_from_slice(&header_block(&empty, 0, b'x'));
    twice.extend_from_slice(&[0u8; 1024]);
    assert_violation!(violations(&compress(&twice)), Violation::Malformed { .. });

    let mut dangling = header_block(&empty, 0, b'x').to_vec();
    dangling.extend_from_slice(&[0u8; 1024]);
    assert_violation!(violations(&compress(&dangling)), Violation::Malformed { .. });

    let mut global = header_block(&empty, 0, b'g').to_vec();
    global.extend_from_slice(&[0u8; 1024]);
    assert_violation!(violations(&compress(&global)), Violation::UnsupportedFeature { .. });

    let mut huge = header_block(&empty, 2 * 1024 * 1024, b'x').to_vec();
    huge.extend_from_slice(&[0u8; 1024]);
    assert_violation!(violations(&compress(&huge)), Violation::Malformed { .. });
}

/// Checks a real package when one is supplied, which unit tests must not download themselves:
/// `NODO_FEED_REAL_XBPS=/path/to/x.xbps cargo test -p nodo-feed real_package -- --ignored --nocapture`
#[test]
#[ignore = "needs a real .xbps file named by NODO_FEED_REAL_XBPS"]
fn real_package() {
    let path = std::env::var("NODO_FEED_REAL_XBPS").expect("NODO_FEED_REAL_XBPS must name a file");
    let file = std::fs::File::open(path).unwrap();
    let inspection = inspect_package(file, &Policy::default());
    println!("package: {:?}", inspection.package);
    println!("entries: {}, decompressed: {}", inspection.entries, inspection.decompressed_bytes);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for violation in &inspection.violations {
        let text = format!("{violation:?}");
        let name = text.split([' ', '{']).next().unwrap_or_default().to_string();
        *counts.entry(name).or_default() += 1;
    }
    println!("violations by kind: {counts:#?}");
    for violation in inspection.violations.iter().take(40) {
        println!("  {violation}");
    }
    let structural = inspection.violations.iter().any(|v| {
        matches!(
            v,
            Violation::Malformed { .. }
                | Violation::Truncated
                | Violation::NotZstd
                | Violation::UnsupportedFeature { .. }
                | Violation::MetadataInvalid { .. }
                | Violation::MetadataMissing { .. }
        )
    });
    assert!(inspection.package.is_some(), "props.plist was not understood");
    assert!(!structural, "a real package should parse, whatever rules it breaks");
}
