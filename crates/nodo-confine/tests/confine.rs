//! Runs the real binary on this machine's kernel. The program it confines is this test
//! executable again, in client mode, which tries one socket operation and reports the outcome
//! through its exit code.

use std::io::Read;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const CLIENT: &str = "NODO_CONFINE_TEST_CLIENT";
const CONNECTED: i32 = 0;
const REFUSED: i32 = 3;

/// The client. A no-op in a normal test run.
#[test]
fn client_entry() {
    let Ok(target) = std::env::var(CLIENT) else { return };
    let connected = if let Some(name) = target.strip_prefix("self@") {
        // Its own abstract socket, created inside the sandbox.
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = match UnixListener::bind_addr(&addr) {
            Ok(listener) => listener,
            Err(_) => std::process::exit(REFUSED),
        };
        let ok = UnixStream::connect_addr(&addr).is_ok();
        drop(listener);
        ok
    } else if let Some(name) = target.strip_prefix('@') {
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        UnixStream::connect_addr(&addr).is_ok()
    } else {
        UnixStream::connect(&target).is_ok()
    };
    std::process::exit(if connected { CONNECTED } else { REFUSED });
}

fn unique(tag: &str) -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("nodo-confine-test-{tag}-{}-{nanos}", std::process::id())
}

fn client(confine: Option<&[&str]>, target: &str) -> i32 {
    let me = std::env::current_exe().unwrap();
    let mut command = match confine {
        Some(flags) => {
            let mut c = Command::new(env!("CARGO_BIN_EXE_nodo-confine"));
            c.args(flags).arg("--").arg(&me);
            c
        }
        None => Command::new(&me),
    };
    let out = command
        .args(["--exact", "client_entry", "--nocapture", "--test-threads", "1"])
        .env(CLIENT, target)
        .output()
        .unwrap();
    out.status.code().unwrap_or(-1)
}

fn abstract_listener(name: &str) -> UnixListener {
    UnixListener::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap()).unwrap()
}

fn scoping_supported() -> bool {
    nodo_confine::landlock_abi().is_some_and(|abi| abi >= nodo_confine::LANDLOCK_SCOPE_ABI)
}

#[test]
fn an_unconfined_client_reaches_the_abstract_socket() {
    let name = unique("control");
    let _listener = abstract_listener(&name);
    assert_eq!(client(None, &format!("@{name}")), CONNECTED);
}

#[test]
fn without_a_unix_grant_no_unix_socket_can_be_made() {
    let name = unique("nogrant");
    let _listener = abstract_listener(&name);
    assert_eq!(client(Some(&[]), &format!("@{name}")), REFUSED);
    let dir = tempfile_dir();
    let path = dir.join("granted.sock");
    let _fs = UnixListener::bind(&path).unwrap();
    assert_eq!(client(Some(&[]), path.to_str().unwrap()), REFUSED);
}

#[test]
fn with_a_unix_grant_filesystem_sockets_work_and_outside_abstract_ones_do_not() {
    let dir = tempfile_dir();
    let path = dir.join("granted.sock");
    let _fs = UnixListener::bind(&path).unwrap();
    assert_eq!(client(Some(&["--unix"]), path.to_str().unwrap()), CONNECTED);
    if !scoping_supported() {
        eprintln!("skipped: this kernel has no Landlock scoping");
        return;
    }
    let name = unique("grant");
    let listener = abstract_listener(&name);
    listener.set_nonblocking(true).unwrap();
    assert_eq!(client(Some(&["--unix"]), &format!("@{name}")), REFUSED);
    // Nothing reached the listener.
    assert!(listener.accept().is_err());
    // The sandbox's own abstract sockets keep working.
    assert_eq!(client(Some(&["--unix"]), &format!("self@{}", unique("own"))), CONNECTED);
}

#[test]
fn usage_errors_run_nothing() {
    let out = Command::new(env!("CARGO_BIN_EXE_nodo-confine")).args(["/bin/true"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = Command::new(env!("CARGO_BIN_EXE_nodo-confine")).args(["--", "/nonexistent/x"]).output().unwrap();
    assert_eq!(out.status.code(), Some(126));
    let mut text = String::new();
    out.stderr.as_slice().read_to_string(&mut text).unwrap();
    assert!(text.contains("cannot run /nonexistent/x"), "{text}");
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(unique("dir"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
