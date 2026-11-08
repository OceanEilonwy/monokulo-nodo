//! nodo-confine [--unix] -- <program> [args...]
//!
//! Run by bwrap as the last step inside a service sandbox. Applies the confinement described in
//! the library, then replaces itself with the program, so the service keeps its own process name
//! and `control/t` still finds it. `--unix` is for services granted a Unix socket.
//!
//! nodo-confine --check prints what this kernel supports, for tests and diagnostics.

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use nodo_confine::{confine, landlock_abi, Options, LANDLOCK_SCOPE_ABI};

/// What a shell reports for a command that was found but could not run.
const CANNOT_RUN: u8 = 126;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--check") {
        let abi = landlock_abi().unwrap_or(0);
        let scope = if abi >= LANDLOCK_SCOPE_ABI { "yes" } else { "no" };
        println!("landlock_abi={abi} abstract_scope={scope}");
        return ExitCode::SUCCESS;
    }
    let mut unix = false;
    let mut rest = args.as_slice();
    loop {
        match rest.first().map(String::as_str) {
            Some("--unix") => unix = true,
            Some("--") => {
                rest = &rest[1..];
                break;
            }
            _ => {
                eprintln!("usage: nodo-confine [--unix] -- <program> [args...]");
                return ExitCode::from(2);
            }
        }
        rest = &rest[1..];
    }
    let Some((program, program_args)) = rest.split_first() else {
        eprintln!("usage: nodo-confine [--unix] -- <program> [args...]");
        return ExitCode::from(2);
    };
    let applied = match confine(Options { unix }) {
        Ok(applied) => applied,
        Err(e) => {
            eprintln!("nodo-confine: refusing to start {program} unconfined: {e}");
            return ExitCode::from(CANNOT_RUN);
        }
    };
    if !applied.abstract_closed() {
        eprintln!(
            "nodo-confine: this kernel has no Landlock scoping (Linux 6.12), so {program} can still reach other services' abstract Unix sockets"
        );
    }
    let error = Command::new(program).args(program_args).exec();
    eprintln!("nodo-confine: cannot run {program}: {error}");
    ExitCode::from(CANNOT_RUN)
}
