//! The health daemon. Runs as root under runit.
//!
//! nodo-healthd [--config <file>] [--once]

use std::path::PathBuf;
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

use nodo_health::sys::{RealSystem, System};
use nodo_health::{tick, Config, State};

fn load(path: &PathBuf) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Config::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut config = PathBuf::from("/etc/nodo/health.toml");
    let mut once = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = args.next().map(PathBuf::from).unwrap_or_default(),
            "--once" => once = true,
            _ => {
                eprintln!("usage: nodo-healthd [--config <file>] [--once]");
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
    let mut state = State::load(&sys, &cfg.state_path);
    let mut last_stopped = Vec::new();
    let mut last_levels = state.levels.clone();
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
        if once {
            return ExitCode::SUCCESS;
        }
        sleep(Duration::from_secs(cfg.interval_secs.max(1)));
    }
}
