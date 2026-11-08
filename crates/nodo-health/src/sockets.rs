//! Abstract Unix sockets that accept connections, and who holds them.
//!
//! Services share the host's network namespace, and abstract sockets (`@name`, no file) belong
//! to it, so no file permission stands between a sandboxed service and them. nodo-confine closes
//! that with Landlock scoping, which needs Linux 6.12. On an older kernel, such as the T6N's 6.1,
//! a service granted a Unix socket can still connect to any of them, so what actually listens
//! there is the residual risk. This module finds out.
//!
//! The kernel lists every Unix socket in `/proc/net/unix`:
//!
//! ```text
//! Num       RefCount Protocol Flags    Type St Inode Path
//! 0000000000000000: 00000002 00000000 00010000 0001 01 44242 @/tmp/.X11-unix/X0
//! ```
//!
//! An abstract name is printed with a leading `@`, and every NUL byte inside it as `@` too. The
//! name is printed byte for byte, so it may hold any byte, including a newline, and is read as
//! bytes here and escaped before anyone sees it. A line that does not parse is taken to be the
//! rest of the previous name after an embedded newline. A name built to look like a second entry
//! after its newline can only add a listener that is not there, never hide one that is.
//!
//! Counted as listening: stream and seqpacket sockets in the listening state (the `__SO_ACCEPTCON`
//! flag), and datagram sockets bound to a name and not connected, since anyone can send to those.
//! Connections a listener accepted carry its name too, but are connected, so they are left out.
//!
//! The owner is found by matching the socket's inode against `/proc/<pid>/fd/*` links, which
//! read `socket:[<inode>]`. Only root can read other users' fd directories.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde::Serialize;

/// `__SO_ACCEPTCON`, which the kernel prints in the flags column of a listening socket.
const SO_ACCEPTCON: u32 = 1 << 16;
const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;
const SOCK_SEQPACKET: u32 = 5;
/// `SS_UNCONNECTED` in the state column.
const SS_UNCONNECTED: u32 = 1;

/// Longest name kept, after escaping. The kernel allows 107 bytes, each up to four characters.
const MAX_NAME: usize = 4 * 108;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Stream,
    Seqpacket,
    Datagram,
}

/// One abstract socket that accepts connections or datagrams.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Listener {
    /// As the kernel prints it, starting with `@`, with bytes outside printable ASCII and
    /// backslashes escaped as `\xNN`.
    pub name: String,
    pub kind: Kind,
    pub inode: u64,
}

/// A process holding a socket.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Owner {
    pub pid: u32,
    /// From `/proc/<pid>/comm`, escaped like names.
    pub process: String,
    pub uid: u32,
    /// The user name, or the number when `/etc/passwd` has no such user.
    pub user: String,
}

/// Printable ASCII is kept; everything else, and the backslash, becomes `\xNN`.
pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if (0x20..0x7f).contains(&b) && b != b'\\' {
            out.push(b as char);
        } else {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

fn hex(field: &[u8]) -> Option<u32> {
    u32::from_str_radix(std::str::from_utf8(field).ok()?, 16).ok()
}

/// Splits off the next field separated by spaces, returning it and the rest after it.
fn field(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let start = line.iter().position(|&b| b != b' ')?;
    let line = &line[start..];
    let end = line.iter().position(|&b| b == b' ').unwrap_or(line.len());
    Some((&line[..end], &line[end..]))
}

/// One entry: (flags, type, state, inode, path bytes). `None` when the line is not an entry.
fn entry(line: &[u8]) -> Option<(u32, u32, u32, u64, &[u8])> {
    let (num, rest) = field(line)?;
    // The kernel's socket pointer, often printed as zeros, then a colon.
    let digits = num.strip_suffix(b":")?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let (_refcount, rest) = field(rest)?;
    let (_protocol, rest) = field(rest)?;
    let (flags, rest) = field(rest)?;
    let (kind, rest) = field(rest)?;
    let (state, rest) = field(rest)?;
    let (inode, rest) = field(rest)?;
    let inode: u64 = std::str::from_utf8(inode).ok()?.parse().ok()?;
    // A path, when there is one, follows the inode after one space.
    let path = rest.strip_prefix(b" ").unwrap_or(rest);
    Some((hex(flags)?, hex(kind)?, hex(state)?, inode, path))
}

/// The listening abstract sockets in the text of `/proc/net/unix`, in the kernel's order.
pub fn parse(text: &[u8]) -> Vec<Listener> {
    // Every named socket is kept until the end, so a continuation line can extend its name.
    let mut sockets: Vec<(Option<Kind>, u64, Vec<u8>)> = Vec::new();
    // The newline ending the last entry does not start another line.
    let text = text.strip_suffix(b"\n").unwrap_or(text);
    let mut lines = text.split(|&b| b == b'\n');
    let _header = lines.next();
    for line in lines {
        match entry(line) {
            Some((flags, kind, state, inode, path)) => {
                let kind = match kind {
                    SOCK_STREAM if flags & SO_ACCEPTCON != 0 => Some(Kind::Stream),
                    SOCK_SEQPACKET if flags & SO_ACCEPTCON != 0 => Some(Kind::Seqpacket),
                    SOCK_DGRAM if state == SS_UNCONNECTED => Some(Kind::Datagram),
                    _ => None,
                };
                sockets.push((kind, inode, path.to_vec()));
            }
            None => {
                if let Some((_, _, path)) = sockets.last_mut() {
                    if !path.is_empty() {
                        path.push(b'\n');
                        path.extend_from_slice(line);
                    }
                }
            }
        }
    }
    sockets
        .into_iter()
        .filter_map(|(kind, inode, path)| {
            let kind = kind?;
            if path.first() != Some(&b'@') {
                return None;
            }
            let mut name = escape(&path);
            if name.len() > MAX_NAME {
                name.truncate(MAX_NAME);
            }
            Some(Listener { name, kind, inode })
        })
        .collect()
}

/// The inode in an fd link target such as `socket:[12345]`.
pub fn socket_inode(target: &[u8]) -> Option<u64> {
    let digits = target.strip_prefix(b"socket:[")?.strip_suffix(b"]")?;
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// The real user ID on the `Uid:` line of `/proc/<pid>/status`.
fn status_uid(status: &str) -> Option<u32> {
    status.lines().find_map(|l| l.strip_prefix("Uid:")?.split_whitespace().next()?.parse().ok())
}

/// User names by ID from the text of `/etc/passwd`.
pub fn passwd_names(passwd: &str) -> BTreeMap<u32, String> {
    passwd
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let uid = fields.nth(1)?.parse().ok()?;
            Some((uid, name.to_owned()))
        })
        .collect()
}

/// The processes holding each of these socket inodes, found under `proc` (normally `/proc`).
/// Processes that exit or cannot be read meanwhile are skipped.
pub fn find_owners(proc: &Path, passwd: &Path, inodes: &BTreeSet<u64>) -> BTreeMap<u64, Vec<Owner>> {
    let mut found: BTreeMap<u64, Vec<Owner>> = BTreeMap::new();
    if inodes.is_empty() {
        return found;
    }
    let names = fs::read_to_string(passwd).map(|t| passwd_names(&t)).unwrap_or_default();
    let Ok(dir) = fs::read_dir(proc) else { return found };
    for entry in dir.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let base = entry.path();
        let Ok(fds) = fs::read_dir(base.join("fd")) else { continue };
        let mut held = BTreeSet::new();
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else { continue };
            if let Some(inode) = socket_inode(target.as_os_str().as_encoded_bytes()) {
                if inodes.contains(&inode) {
                    held.insert(inode);
                }
            }
        }
        if held.is_empty() {
            continue;
        }
        let process = fs::read(base.join("comm")).map(|b| escape(b.strip_suffix(b"\n").unwrap_or(&b))).unwrap_or_default();
        let uid = fs::read_to_string(base.join("status"))
            .ok()
            .and_then(|s| status_uid(&s))
            .or_else(|| fs::metadata(&base).ok().map(|m| m.uid()));
        let Some(uid) = uid else { continue };
        let user = names.get(&uid).cloned().unwrap_or_else(|| uid.to_string());
        for inode in held {
            found.entry(inode).or_default().push(Owner { pid, process: process.clone(), uid, user: user.clone() });
        }
    }
    for owners in found.values_mut() {
        owners.sort();
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "Num       RefCount Protocol Flags    Type St Inode Path\n";

    fn names(text: &[u8]) -> Vec<(String, Kind, u64)> {
        parse(text).into_iter().map(|l| (l.name, l.kind, l.inode)).collect()
    }

    #[test]
    fn finds_listening_abstract_sockets_in_a_real_sample() {
        // From a desktop running X11, D-Bus and systemd, inode padding included.
        let text = format!(
            "{HEADER}\
0000000000000000: 00000003 00000000 00000000 0001 03 85372225
0000000000000000: 00000003 00000000 00000000 0001 03 50551 /run/user/1000/bus
0000000000000000: 00000003 00000000 00000000 0001 03 48233 @/tmp/.X11-unix/X0
0000000000000000: 00000003 00000000 00000000 0001 03  5697 @/tmp/.X11-unix/X0
0000000000000000: 00000002 00000000 00010000 0001 01 44350 @/tmp/.ICE-unix/2488
0000000000000000: 00000002 00000000 00000000 0002 01 35843 @14324274185488981868
0000000000000000: 00000003 00000000 00000000 0001 03 36239 @da8f9986447bdeb7/bus/systemd-timesyn/bus-api-timesync
0000000000000000: 00000002 00000000 00010000 0001 01 44242 @/tmp/.X11-unix/X0
0000000000000000: 00000002 00000000 00010000 0001 01 22059 /run/dbus/system_bus_socket
0000000000000000: 00000003 00000000 00000000 0005 03 74770512 @22a07
0000000000000000: 00000002 00000000 00000000 0002 03 86230928
0000000000000000: 00000002 00000000 00000000 0002 01 17006
0000000000000000: 00000002 00000000 00010000 0005 01  9001 @seq
"
        );
        assert_eq!(
            names(text.as_bytes()),
            [
                ("@/tmp/.ICE-unix/2488".to_owned(), Kind::Stream, 44350),
                ("@14324274185488981868".to_owned(), Kind::Datagram, 35843),
                ("@/tmp/.X11-unix/X0".to_owned(), Kind::Stream, 44242),
                ("@seq".to_owned(), Kind::Seqpacket, 9001),
            ]
        );
    }

    #[test]
    fn pathname_unnamed_connected_and_accepted_sockets_are_ignored() {
        let text = format!(
            "{HEADER}\
0000000000000000: 00000002 00000000 00010000 0001 01 10 /run/nodo/helper.sock
0000000000000000: 00000002 00000000 00010000 0001 01 11
0000000000000000: 00000003 00000000 00000000 0001 03 12 @accepted-connection
0000000000000000: 00000002 00000000 00000000 0001 01 13 @bound-but-not-listening
0000000000000000: 00000002 00000000 00000000 0002 03 14 @connected-datagram
0000000000000000: 00000002 00000000 00000000 0002 01 15 /run/datagram.sock
"
        );
        assert!(parse(text.as_bytes()).is_empty());
    }

    #[test]
    fn weird_names_are_kept_whole_and_escaped() {
        // An embedded NUL is printed as '@' by the kernel, so it arrives as '@' here.
        let mut text = HEADER.as_bytes().to_vec();
        text.extend_from_slice(b"0000000000000000: 00000002 00000000 00010000 0001 01 21 @a@b@@\n");
        text.extend_from_slice(b"0000000000000000: 00000002 00000000 00010000 0001 01 22 @with space  \n");
        text.extend_from_slice(b"0000000000000000: 00000002 00000000 00010000 0001 01 23 @\xff\xfe\x1b[31m\\\n");
        text.extend_from_slice(b"0000000000000000: 00000002 00000000 00010000 0001 01 24 @\n");
        text.extend_from_slice("0000000000000000: 00000002 00000000 00010000 0001 01 25 @caf\u{e9}\n".as_bytes());
        assert_eq!(
            names(&text),
            [
                ("@a@b@@".to_owned(), Kind::Stream, 21),
                ("@with space  ".to_owned(), Kind::Stream, 22),
                ("@\\xff\\xfe\\x1b[31m\\x5c".to_owned(), Kind::Stream, 23),
                ("@".to_owned(), Kind::Stream, 24),
                ("@caf\\xc3\\xa9".to_owned(), Kind::Stream, 25),
            ]
        );
    }

    #[test]
    fn an_embedded_newline_continues_the_name_and_cannot_hide_it() {
        let mut text = HEADER.as_bytes().to_vec();
        text.extend_from_slice(b"0000000000000000: 00000002 00000000 00010000 0001 01 31 @first\nsecond part\n");
        text.extend_from_slice(b"0000000000000000: 00000002 00000000 00010000 0001 01 32 @next\n");
        assert_eq!(
            names(&text),
            [("@first\\x0asecond part".to_owned(), Kind::Stream, 31), ("@next".to_owned(), Kind::Stream, 32)]
        );
    }

    #[test]
    fn missing_or_garbled_input_gives_nothing() {
        assert!(parse(b"").is_empty());
        assert!(parse(HEADER.as_bytes()).is_empty());
        assert!(parse(b"garbage\nmore garbage\n0000: zz\n").is_empty());
        // Too few fields.
        let text = format!("{HEADER}0000000000000000: 00000002 00000000 00010000 0001 01\n");
        assert!(parse(text.as_bytes()).is_empty());
    }

    #[test]
    fn reads_socket_links_and_passwd() {
        assert_eq!(socket_inode(b"socket:[12345]"), Some(12345));
        assert_eq!(socket_inode(b"pipe:[12345]"), None);
        assert_eq!(socket_inode(b"/tmp/socket:[1]"), None);
        assert_eq!(socket_inode(b"socket:[]"), None);
        let names = passwd_names("root:x:0:0::/root:/bin/sh\n_caddy:x:990:990::/:/bin/false\nbroken\n");
        assert_eq!(names.get(&990).map(String::as_str), Some("_caddy"));
        assert_eq!(names.len(), 2);
        assert_eq!(status_uid("Name:\tsleep\nUid:\t65534\t65534\t65534\t65534\n"), Some(65534));
    }

    #[test]
    fn finds_owners_through_fd_links() {
        let dir = tempfile::tempdir().unwrap();
        let proc = dir.path().join("proc");
        let process = |pid: u32, comm: &str, uid: u32, links: &[&str]| {
            let base = proc.join(pid.to_string());
            fs::create_dir_all(base.join("fd")).unwrap();
            fs::write(base.join("comm"), format!("{comm}\n")).unwrap();
            fs::write(base.join("status"), format!("Name:\t{comm}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n")).unwrap();
            for (fd, target) in links.iter().enumerate() {
                std::os::unix::fs::symlink(target, base.join("fd").join(fd.to_string())).unwrap();
            }
        };
        process(100, "listener", 65534, &["/dev/null", "socket:[500]", "socket:[777]"]);
        process(101, "child", 0, &["socket:[500]"]);
        process(102, "other", 0, &["socket:[600]", "pipe:[500]"]);
        fs::create_dir_all(proc.join("self")).unwrap();
        fs::create_dir_all(proc.join("103")).unwrap(); // exited, no fd directory left
        let passwd = dir.path().join("passwd");
        fs::write(&passwd, "root:x:0:0::/root:/bin/sh\nnobody:x:65534:65534::/:/bin/false\n").unwrap();

        let owners = find_owners(&proc, &passwd, &BTreeSet::from([500, 501]));
        assert_eq!(owners.len(), 1);
        assert_eq!(
            owners[&500],
            [
                Owner { pid: 100, process: "listener".into(), uid: 65534, user: "nobody".into() },
                Owner { pid: 101, process: "child".into(), uid: 0, user: "root".into() },
            ]
        );
        // A user missing from passwd is shown by number.
        let owners = find_owners(&proc, &dir.path().join("no-passwd"), &BTreeSet::from([500]));
        assert_eq!(owners[&500][0].user, "65534");
        assert!(find_owners(&proc, &passwd, &BTreeSet::new()).is_empty());
    }
}
