//! Runs the ramdisk hook against the Rust implementation of the same rules, on a regular file
//! standing in for the partition. Nothing here touches a block device.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use nodo_update::slots::{
    boot_decision, mark_good, read_partition, stage, write_partition, BootRecord, Slot, COPY_OFFSETS,
    PARTITION_MIN_LEN, RECORD_LEN,
};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ramdisk/nodo-ab")
}

struct Run {
    root: Option<String>,
    slot: Option<String>,
    fallback: bool,
}

fn run_hook(dir: &Path, dev: &Path) -> Run {
    let param = dir.join("param.conf");
    let run = dir.join("run");
    let _ = fs::remove_file(&param);
    let _ = fs::remove_dir_all(&run);
    let status = shell(dir)
        .arg(script())
        .env("NODO_AB_DEV", dev)
        .env("NODO_AB_PARAM", &param)
        .env("NODO_AB_RUN", &run)
        .env("TMPDIR", dir)
        .status()
        .expect("sh runs");
    assert!(status.success());
    Run {
        root: fs::read_to_string(&param).ok(),
        slot: fs::read_to_string(run.join("boot-slot")).ok().map(|s| s.trim().to_owned()),
        fallback: run.join("boot-fallback").exists(),
    }
}

/// The ramdisk runs busybox. Set NODO_AB_BUSYBOX to a static busybox binary to run the hook with
/// its shell and applets instead of the host's.
fn shell(dir: &Path) -> Command {
    let Ok(busybox) = std::env::var("NODO_AB_BUSYBOX") else { return Command::new("sh") };
    let bin = dir.join("bin");
    if !bin.exists() {
        fs::create_dir(&bin).unwrap();
        for applet in ["sh", "dd", "tr", "wc", "head", "printf", "mkdir", "rm", "sync", "cat", "echo"] {
            std::os::unix::fs::symlink(&busybox, bin.join(applet)).unwrap();
        }
    }
    let mut cmd = Command::new(bin.join("sh"));
    cmd.env("PATH", &bin);
    cmd
}

fn letter(slot: Slot) -> String {
    slot.to_string()
}

/// Runs both implementations on the same partition image and checks they agree on the slot and
/// on every byte written.
fn agree(dir: &Path, part: &[u8]) -> (Slot, Vec<u8>) {
    let dev = dir.join("nodoboot");
    fs::write(&dev, part).unwrap();
    let hook = run_hook(dir, &dev);
    let after = fs::read(&dev).unwrap();

    let (slot, write) = boot_decision(read_partition(part).map(|(_, r)| r));
    let mut expected = part.to_vec();
    if let Some(rec) = write {
        write_partition(&mut expected, &rec);
    }
    assert_eq!(hook.root.as_deref(), Some(format!("ROOT=PARTLABEL=rootfs_{}\n", letter(slot)).as_str()));
    assert_eq!(hook.slot.as_deref(), Some(letter(slot).as_str()));
    assert!(after == expected, "partition bytes differ between the hook and the Rust rules");
    (slot, after)
}

fn image(records: &[(usize, [u8; RECORD_LEN])]) -> Vec<u8> {
    let mut part = vec![0u8; PARTITION_MIN_LEN];
    for (copy, bytes) in records {
        let off = COPY_OFFSETS[*copy];
        part[off..off + RECORD_LEN].copy_from_slice(bytes);
    }
    part
}

fn rec(seq: u32, active: Slot, tries: u8, good_a: bool, good_b: bool) -> [u8; RECORD_LEN] {
    BootRecord { seq, active, tries, good_a, good_b }.encode()
}

#[test]
fn hook_matches_the_rust_rules_for_every_case() {
    let dir = tempfile::tempdir().unwrap();
    let mut torn = rec(50, Slot::B, 3, true, false);
    torn[25..].fill(0xff);
    let mut trailing = rec(50, Slot::B, 0, true, true);
    trailing[RECORD_LEN - 1] = b'z';
    let blank_line = {
        let mut b = [0u8; RECORD_LEN];
        let t = b"NODOAB1\nseq=60\nactive=b\n\ntries=0\ngood=b\nend\n";
        b[..t.len()].copy_from_slice(t);
        b
    };
    let cases: Vec<Vec<u8>> = vec![
        image(&[]),
        image(&[(0, BootRecord::initial().encode())]),
        image(&[(1, rec(4, Slot::B, 3, true, false))]),
        image(&[(0, rec(4, Slot::B, 1, true, false)), (1, rec(3, Slot::A, 0, true, false))]),
        image(&[(0, rec(4, Slot::B, 0, true, false))]),
        image(&[(0, rec(4, Slot::B, 0, false, false))]),
        image(&[(0, rec(4, Slot::B, 2, true, true))]),
        image(&[(0, rec(9, Slot::A, 0, false, true))]),
        image(&[(0, torn), (1, rec(49, Slot::A, 0, true, false))]),
        image(&[(0, trailing), (1, rec(49, Slot::A, 0, true, false))]),
        image(&[(0, blank_line), (1, rec(59, Slot::A, 0, true, false))]),
        image(&[(1, rec(999_999_999, Slot::B, 2, true, false))]),
        vec![0xffu8; PARTITION_MIN_LEN],
    ];
    for (i, part) in cases.iter().enumerate() {
        eprintln!("case {i}");
        agree(dir.path(), part);
    }
}

#[test]
fn full_trial_and_fallback_through_the_hook() {
    let dir = tempfile::tempdir().unwrap();
    let mut part = vec![0u8; PARTITION_MIN_LEN];
    write_partition(&mut part, &BootRecord::initial());
    write_partition(&mut part, &stage(BootRecord::initial(), Slot::A).unwrap());
    for _ in 0..3 {
        let (slot, after) = agree(dir.path(), &part);
        assert_eq!(slot, Slot::B);
        part = after;
    }
    let dev = dir.path().join("nodoboot");
    fs::write(&dev, &part).unwrap();
    let hook = run_hook(dir.path(), &dev);
    assert_eq!(hook.slot.as_deref(), Some("a"));
    assert!(hook.fallback, "the running system is told a fallback happened");
}

#[test]
fn a_slot_marked_good_by_the_system_stays_booted() {
    let dir = tempfile::tempdir().unwrap();
    let mut part = vec![0u8; PARTITION_MIN_LEN];
    write_partition(&mut part, &stage(BootRecord::initial(), Slot::A).unwrap());
    let (slot, after) = agree(dir.path(), &part);
    let mut part = after;
    let good = mark_good(read_partition(&part).unwrap().1, slot).unwrap();
    write_partition(&mut part, &good);
    for _ in 0..5 {
        let (slot, after) = agree(dir.path(), &part);
        assert_eq!(slot, Slot::B);
        assert_eq!(after, part, "a known-good boot writes nothing");
    }
}

#[test]
fn without_the_partition_the_boot_arguments_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let hook = run_hook(dir.path(), &dir.path().join("missing"));
    assert_eq!(hook.root, None);
    assert_eq!(hook.slot, None);
}
