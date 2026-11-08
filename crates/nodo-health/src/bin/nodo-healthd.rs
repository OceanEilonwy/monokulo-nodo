//! The health daemon. Runs as root under runit.
//!
//! nodo-healthd [--config <file>] [--once]
//! nodo-healthd [--config <file>] --abstract-sockets
//!
//! `--abstract-sockets` prints the abstract Unix sockets listening now, who holds them, whether
//! the configuration expects them, and whether this kernel's sandbox can keep services away
//! from them. It exits 1 when an unexpected one is listening. Owners are found only as root.

use std::path::PathBuf;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use nodo_health::report::{abstract_sockets, owner_text, AbstractSockets};
use nodo_health::sys::{RealSystem, System};
use nodo_health::{tick, Config, State};

fn load(path: &PathBuf) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Config::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn sandbox_line(s: &AbstractSockets) -> String {
    let abi = s.landlock_abi.map(|a| a.to_string()).unwrap_or_else(|| "none".to_owned());
    let kernel = s.kernel.as_deref().unwrap_or("unknown");
    let verdict = if s.sandbox_blocks {
        "the sandbox keeps services away from abstract sockets outside it"
    } else {
        "the sandbox cannot keep services granted a Unix socket away from abstract sockets"
    };
    format!("kernel {kernel}, Landlock ABI {abi}: {verdict}")
}

fn list_abstract_sockets(cfg: &Config, sys: &RealSystem) -> ExitCode {
    let found = abstract_sockets(&cfg.abstract_sockets, sys);
    println!("{}", sandbox_line(&found));
    if !found.scanned {
        println!("cannot read /proc/net/unix");
        return ExitCode::from(2);
    }
    if found.listeners.is_empty() {
        println!("no abstract Unix sockets are listening");
    }
    for l in &found.listeners {
        let mark = if l.expected { "expected  " } else { "UNEXPECTED" };
        println!("{mark} {} {:?} inode {}: {}", l.name, l.kind, l.inode, owner_text(&l.owners));
    }
    if !rustix::process::geteuid().is_root() && found.listeners.iter().any(|l| l.owners.is_empty()) {
        println!("run as root to see every owner");
    }
    if found.unexpected().next().is_some() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut config = PathBuf::from("/etc/nodo/health.toml");
    let mut once = false;
    let mut list = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = args.next().map(PathBuf::from).unwrap_or_default(),
            "--once" => once = true,
            "--abstract-sockets" => list = true,
            _ => {
                eprintln!("usage: nodo-healthd [--config <file>] [--once | --abstract-sockets]");
                return ExitCode::from(2);
            }
        }
    }
    // A broken config must not leave the disk unprotected, so fall back to the defaults.
    let cfg = load(&config).unwrap_or_else(|e| {
        println!("using default thresholds: {e}");
        Config::default()
    });
    let sys = RealSystem { sv_dir: cfg.sv_dir.clone() };
    if list {
        return list_abstract_sockets(&cfg, &sys);
    }
    let mut state = State::load(&sys, &cfg.state_path);
    let mut last_stopped = Vec::new();
    let mut last_levels = state.levels.clone();
    let mut last_sockets: Option<Vec<String>> = None;
    loop {
        let report = tick(&cfg, &mut state, &sys);
        let stopped: Vec<String> = report.stopped.iter().map(|s| s.service.clone()).collect();
        if state.levels != last_levels || stopped != last_stopped {
            for d in &report.disks {
                println!("{}: {:?}, {} bytes free, mounted {}", d.name, d.level, d.free_bytes, d.mounted);
            }
            println!("stopped to protect disks: [{}] at {}", stopped.join(", "), sys.now());
            last_stopped = stopped;
            last_levels = state.levels.clone();
        }
        let sockets = &report.abstract_sockets;
        let unexpected: Vec<String> =
            sockets.unexpected().map(|l| format!("{} held by {}", l.name, owner_text(&l.owners))).collect();
        if last_sockets.as_ref() != Some(&unexpected) {
            if last_sockets.is_none() {
                println!("{}", sandbox_line(sockets));
            }
            println!("unexpected abstract Unix sockets: [{}] at {}", unexpected.join("; "), sys.now());
            last_sockets = Some(unexpected);
        }
        if once {
            return ExitCode::SUCCESS;
        }
        sleep(Duration::from_secs(cfg.interval_secs.max(1)));
    }
}
