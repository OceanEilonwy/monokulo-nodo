//! The A/B boot record, shared between the ramdisk and the running system.
//!
//! FriendlyElec's U-Boot always loads the same kernel and ramdisk partitions, but the ramdisk is
//! ours, and it chooses the root filesystem. So the ramdisk reads this record from a small raw
//! partition, counts down trial boots of a newly written slot, and falls back to the last slot
//! that reached a healthy state. See docs/UPDATES.md for what this does and does not cover.
//!
//! The record is plain text so the ramdisk can parse it with busybox `sh`. Two copies live in the
//! partition, at sectors 0 and 8. Each write goes to the copy not holding the newest record, so a
//! write torn by power loss leaves the previous record intact. A copy counts only if it parses
//! exactly; `ramdisk/nodo-ab` implements the same rules and the tests run both against each other.

use std::fmt;

pub const RECORD_LEN: usize = 512;
pub const COPY_OFFSETS: [usize; 2] = [0, 8 * RECORD_LEN];
pub const PARTITION_MIN_LEN: usize = COPY_OFFSETS[1] + RECORD_LEN;
/// Boots a new slot gets before the ramdisk gives up on it.
pub const TRIAL_BOOTS: u8 = 3;
const MAGIC: &str = "NODOAB1";
/// Nine digits, so the ramdisk shell can compare it as a number.
const MAX_SEQ: u32 = 999_999_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }

    fn letter(self) -> char {
        match self {
            Slot::A => 'a',
            Slot::B => 'b',
        }
    }

    /// The GPT partition label of this slot's root filesystem.
    pub fn partlabel(self) -> &'static str {
        match self {
            Slot::A => "rootfs_a",
            Slot::B => "rootfs_b",
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.letter())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootRecord {
    pub seq: u32,
    pub active: Slot,
    /// Trial boots left for `active` while it is not yet known good.
    pub tries: u8,
    pub good_a: bool,
    pub good_b: bool,
}

impl BootRecord {
    /// What a freshly installed device starts with: slot A, known good.
    pub fn initial() -> Self {
        BootRecord { seq: 1, active: Slot::A, tries: 0, good_a: true, good_b: false }
    }

    pub fn is_good(&self, slot: Slot) -> bool {
        match slot {
            Slot::A => self.good_a,
            Slot::B => self.good_b,
        }
    }

    fn set_good(&mut self, slot: Slot, good: bool) {
        match slot {
            Slot::A => self.good_a = good,
            Slot::B => self.good_b = good,
        }
    }

    /// Wrapping would take a billion writes, more than the eMMC will survive.
    fn next(mut self) -> Self {
        self.seq = if self.seq >= MAX_SEQ { 1 } else { self.seq + 1 };
        self
    }

    pub fn encode(&self) -> [u8; RECORD_LEN] {
        let good = match (self.good_a, self.good_b) {
            (true, true) => "ab",
            (true, false) => "a",
            (false, true) => "b",
            (false, false) => "-",
        };
        let text = format!("{MAGIC}\nseq={}\nactive={}\ntries={}\ngood={good}\nend\n", self.seq, self.active, self.tries);
        let mut out = [0u8; RECORD_LEN];
        out[..text.len()].copy_from_slice(text.as_bytes());
        out
    }

    /// Exact parsing only. Anything unexpected, including trailing bytes, means a torn or foreign
    /// copy, and the other copy is used instead.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..RECORD_LEN)?;
        let len = bytes.iter().position(|b| *b == 0).unwrap_or(RECORD_LEN);
        if bytes[len..].iter().any(|b| *b != 0) {
            return None;
        }
        let text = std::str::from_utf8(&bytes[..len]).ok()?;
        let lines: Vec<&str> = text.strip_suffix('\n')?.split('\n').collect();
        let [magic, seq, active, tries, good, end] = lines.as_slice() else { return None };
        if *magic != MAGIC || *end != "end" {
            return None;
        }
        let seq = digits(seq.strip_prefix("seq=")?, 9)?;
        let active = match active.strip_prefix("active=")? {
            "a" => Slot::A,
            "b" => Slot::B,
            _ => return None,
        };
        let tries = digits(tries.strip_prefix("tries=")?, 1)? as u8;
        let (good_a, good_b) = match good.strip_prefix("good=")? {
            "ab" => (true, true),
            "a" => (true, false),
            "b" => (false, true),
            "-" => (false, false),
            _ => return None,
        };
        if seq == 0 {
            return None;
        }
        Some(BootRecord { seq, active, tries, good_a, good_b })
    }
}

/// Plain decimal, no sign or leading zero, at most `max_len` digits so it fits the shell's
/// arithmetic.
fn digits(text: &str, max_len: usize) -> Option<u32> {
    let plain = !text.is_empty()
        && text.len() <= max_len
        && text.bytes().all(|b| b.is_ascii_digit())
        && !(text.len() > 1 && text.starts_with('0'));
    plain.then(|| text.parse().ok()).flatten()
}

/// Picks the newest valid copy. Sequence numbers are compared as integers; they would need a
/// billion updates to wrap.
pub fn read_partition(bytes: &[u8]) -> Option<(usize, BootRecord)> {
    COPY_OFFSETS
        .iter()
        .enumerate()
        .filter_map(|(i, off)| BootRecord::decode(bytes.get(*off..)?).map(|r| (i, r)))
        .max_by_key(|(_, r)| r.seq)
}

/// Writes a record into the copy that does not hold the current newest one.
pub fn write_partition(bytes: &mut [u8], record: &BootRecord) {
    let target = match read_partition(bytes) {
        Some((newest, _)) => 1 - newest,
        None => 0,
    };
    let off = COPY_OFFSETS[target];
    bytes[off..off + RECORD_LEN].copy_from_slice(&record.encode());
}

/// What the ramdisk does: which slot to boot, and the record to write first, if any.
pub fn boot_decision(record: Option<BootRecord>) -> (Slot, Option<BootRecord>) {
    let Some(rec) = record else { return (Slot::A, None) };
    let slot = rec.active;
    if rec.is_good(slot) {
        return (slot, None);
    }
    if rec.tries > 0 {
        let mut next = rec.next();
        next.tries -= 1;
        return (slot, Some(next));
    }
    let other = slot.other();
    if rec.is_good(other) {
        let mut next = rec.next();
        next.active = other;
        return (other, Some(next));
    }
    // Neither slot is known good. Keep trying the active one rather than stop booting.
    (slot, None)
}

/// Called by the running system once the booted slot is healthy.
pub fn mark_good(rec: BootRecord, booted: Slot) -> Option<BootRecord> {
    if rec.active != booted || (rec.is_good(booted) && rec.tries == 0) {
        return None;
    }
    let mut next = rec.next();
    next.set_good(booted, true);
    next.tries = 0;
    Some(next)
}

/// Called after writing a new root filesystem into the inactive slot, to try it on next boot.
pub fn stage(rec: BootRecord, booted: Slot) -> Result<BootRecord, &'static str> {
    if rec.active != booted {
        return Err("the booted slot is not the active one; a fallback happened and needs attention first");
    }
    if !rec.is_good(booted) {
        return Err("the running slot is not yet marked good, so there would be nothing to fall back to");
    }
    let target = booted.other();
    let mut next = rec.next();
    next.set_good(target, false);
    next.active = target;
    next.tries = TRIAL_BOOTS;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        for rec in [
            BootRecord::initial(),
            BootRecord { seq: 999_999_999, active: Slot::B, tries: 9, good_a: false, good_b: false },
            BootRecord { seq: 7, active: Slot::B, tries: 0, good_a: true, good_b: true },
        ] {
            assert_eq!(BootRecord::decode(&rec.encode()), Some(rec));
        }
    }

    #[test]
    fn damaged_copies_are_ignored() {
        let good = BootRecord::initial().encode();
        assert!(BootRecord::decode(&good).is_some(), "control");
        let mut cases: Vec<Vec<u8>> = vec![vec![0; RECORD_LEN], vec![0xff; RECORD_LEN], good[..100].to_vec()];
        for (from, to) in [("seq=1", "seq=01"), ("seq=1", "seq=-1"), ("active=a", "active=c"), ("tries=0", "tries=10"),
            ("good=a", "good=ba"), ("end", "enx"), ("NODOAB1", "NODOAB2"), ("seq=1", "seq=0")]
        {
            let text = String::from_utf8(good.to_vec()).unwrap().replacen(from, to, 1);
            let mut bytes = text.into_bytes();
            bytes.resize(RECORD_LEN, 0);
            cases.push(bytes);
        }
        let mut trailing = good;
        trailing[RECORD_LEN - 1] = b'x';
        cases.push(trailing.to_vec());
        // A write torn halfway through: new text over the start of an old record.
        let mut torn = BootRecord { seq: 1234, active: Slot::B, tries: 3, good_a: true, good_b: false }.encode();
        torn[20..].copy_from_slice(&[0xff; RECORD_LEN - 20]);
        cases.push(torn.to_vec());
        for (i, case) in cases.iter().enumerate() {
            assert_eq!(BootRecord::decode(case), None, "case {i}");
        }
    }

    #[test]
    fn writes_alternate_so_a_torn_write_keeps_the_previous_record() {
        let mut part = vec![0u8; PARTITION_MIN_LEN];
        let r1 = BootRecord::initial();
        write_partition(&mut part, &r1);
        let r2 = stage(r1, Slot::A).unwrap();
        write_partition(&mut part, &r2);
        assert_eq!(read_partition(&part).unwrap().1, r2);
        // The next write targets the copy holding r1. Tear it.
        let r3 = mark_good(r2, Slot::B).unwrap();
        let target = 1 - read_partition(&part).unwrap().0;
        let off = COPY_OFFSETS[target];
        part[off..off + 30].copy_from_slice(&r3.encode()[..30]);
        part[off + 30..off + RECORD_LEN].fill(0xff);
        assert_eq!(read_partition(&part).unwrap().1, r2);
    }

    #[test]
    fn a_new_slot_gets_three_tries_then_falls_back() {
        let staged = stage(BootRecord::initial(), Slot::A).unwrap();
        assert_eq!((staged.active, staged.tries, staged.good_a, staged.good_b), (Slot::B, 3, true, false));
        let mut rec = staged;
        for left in (0..3).rev() {
            let (slot, write) = boot_decision(Some(rec));
            assert_eq!(slot, Slot::B);
            rec = write.unwrap();
            assert_eq!(rec.tries, left);
        }
        let (slot, write) = boot_decision(Some(rec));
        assert_eq!(slot, Slot::A, "falls back to the last good slot");
        let rec = write.unwrap();
        assert_eq!(boot_decision(Some(rec)), (Slot::A, None), "and stays there");
        assert!(!rec.good_b);
    }

    #[test]
    fn a_healthy_new_slot_is_kept() {
        let staged = stage(BootRecord::initial(), Slot::A).unwrap();
        let (slot, write) = boot_decision(Some(staged));
        let good = mark_good(write.unwrap(), slot).unwrap();
        assert_eq!(boot_decision(Some(good)), (Slot::B, None));
        assert!(good.good_a, "the old slot stays as a fallback for the next update");
        assert_eq!(mark_good(good, Slot::B), None, "marking twice writes nothing");
    }

    #[test]
    fn refuses_to_stage_without_a_fallback() {
        let staged = stage(BootRecord::initial(), Slot::A).unwrap();
        assert!(stage(staged, Slot::B).is_err(), "B is still on trial");
        assert!(stage(staged, Slot::A).is_err(), "A is not active, so a fallback happened");
    }

    #[test]
    fn no_record_boots_a_without_writing() {
        assert_eq!(boot_decision(None), (Slot::A, None));
    }
}
