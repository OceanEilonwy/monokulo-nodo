//! `nodo-web-gui`: serves the admin pages to Caddy on a Unix socket, or under `--mock` to a
//! developer's browser on the loopback address.

// A dropped result is how failures went unreported. Where dropping is right, an #[allow] says why.
// CI also enforces these for the tests (.github/workflows/crates.yml).
#![deny(clippy::let_underscore_must_use, unused_must_use)]

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use nodo_web_gui::backends::{Backends, HelperBackends, MockBackends};
use nodo_web_gui::clock::SystemClock;
use nodo_web_gui::{router, AppState, Config};

const USAGE: &str = "usage: nodo-web-gui [--socket PATH] [--listen-unix PATH]
       nodo-web-gui --mock [ADDRESS:PORT]

  --socket PATH        the helper's socket for the web GUI (default /run/nodo/webgui.sock)
  --listen-unix PATH   the Unix socket Caddy reaches this process on
                       (default /run/nodo-web-gui/http.sock)
  --mock [ADDR:PORT]   serve plain HTTP over an in-memory device, for development
                       (default 127.0.0.1:8080; only a loopback address is accepted;
                       the passphrase is the mock's)";

/// The helper allows 16 connections on the web GUI's socket. Each session holds one and a login
/// in flight one more, so 4 sessions need 5; the blocking pool is kept as small as that allows,
/// with room for a session's second request waiting behind its first.
const BLOCKING_THREADS: usize = 12;

enum Listen {
    Unix(PathBuf),
    Mock(SocketAddr),
}

struct Args {
    socket: PathBuf,
    listen: Listen,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut socket = PathBuf::from(nodo_api::WEBGUI_SOCKET);
    let mut listen = Listen::Unix(PathBuf::from(nodo_api::WEBGUI_HTTP_SOCKET));
    let mut rest = args.iter().peekable();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--socket" => socket = PathBuf::from(rest.next().ok_or("--socket needs a path")?),
            "--listen-unix" => listen = Listen::Unix(PathBuf::from(rest.next().ok_or("--listen-unix needs a path")?)),
            "--mock" => {
                let address = match rest.peek() {
                    Some(next) if !next.starts_with("--") => rest.next().map(String::as_str).unwrap_or_default(),
                    _ => "127.0.0.1:8080",
                };
                let address: SocketAddr = address.parse().map_err(|_| format!("--mock takes ADDRESS:PORT, got {address}"))?;
                // The mock logs in with a passphrase everyone knows and serves plain HTTP. It
                // must never be reachable from another machine.
                if !is_loopback(address.ip()) {
                    return Err(format!("--mock serves only on a loopback address, not {}", address.ip()));
                }
                listen = Listen::Mock(address);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args { socket, listen })
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        // An IPv4 loopback address written as IPv6 is not `::1`, and is not taken for one.
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Ends when the service is told to stop. runit sends SIGTERM (through `control/t`, as bwrap
/// forwards no signals); Ctrl-C ends a development run.
async fn stop_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let (mut term, mut int) = match (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) {
        (Ok(term), Ok(int)) => (term, int),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("nodo-web-gui: cannot wait for signals: {error}");
            return std::future::pending().await;
        }
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

/// Ends idle sessions even when no request comes, so the helper's gate closes with the last one.
fn sweep_in_background(state: AppState) {
    drop(tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            state.sweep().await;
        }
    }));
}

/// How long requests in flight get to finish once the service is told to stop. A page that is
/// waiting on the helper for longer is cut off; the helper carries on with what it was asked.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Waits until the stop has been signalled.
async fn stopped(mut stop: tokio::sync::watch::Receiver<bool>) {
    // An error means the sender is gone, which only happens after it sent.
    #[allow(clippy::let_underscore_must_use)]
    let _ = stop.wait_for(|stop| *stop).await;
}

/// Serves until told to stop, then lets requests in flight finish for `STOP_GRACE`.
async fn serve_until_stopped<L>(listener: L, app: axum::Router) -> Result<(), String>
where
    L: axum::serve::Listener,
    L::Addr: std::fmt::Debug,
{
    let (signal, stop) = tokio::sync::watch::channel(false);
    drop(tokio::spawn(async move {
        stop_signal().await;
        // Fails only when the server has ended already.
        #[allow(clippy::let_underscore_must_use)]
        let _ = signal.send(true);
    }));
    let serving = axum::serve(listener, app).with_graceful_shutdown(stopped(stop.clone()));
    tokio::select! {
        served = serving => served.map_err(|e| e.to_string()),
        () = async { stopped(stop).await; tokio::time::sleep(STOP_GRACE).await } => Ok(()),
    }
}

async fn serve(args: Args) -> Result<(), String> {
    let clock = Arc::new(SystemClock::new());
    match args.listen {
        Listen::Mock(address) => {
            let backends: Arc<dyn Backends> = Arc::new(MockBackends::default());
            let state = AppState::new(backends, clock, Config::mock());
            sweep_in_background(state.clone());
            let listener = tokio::net::TcpListener::bind(address).await.map_err(|e| format!("cannot listen on {address}: {e}"))?;
            eprintln!("nodo-web-gui: mock device at http://{address} (passphrase: {})", nodo_ui_core::mock::MOCK_PASSPHRASE);
            serve_until_stopped(listener, router(state)).await
        }
        Listen::Unix(path) => {
            let backends: Arc<dyn Backends> = Arc::new(HelperBackends { socket: args.socket });
            let state = AppState::new(backends, clock, Config::device());
            sweep_in_background(state.clone());
            // The socket is made 0660 from the start: its directory is setgid to Caddy's group,
            // so the group that may connect is Caddy's and nobody else's. The owner and group
            // are left as the directory gives them. The run script removes a socket left from
            // the last run; one that is still there belongs to a process that is running.
            // SAFETY: umask only sets this process's file mode mask and cannot fail.
            unsafe { libc::umask(0o117) };
            let listener = tokio::net::UnixListener::bind(&path).map_err(|e| format!("cannot listen on {}: {e}", path.display()))?;
            eprintln!("nodo-web-gui: listening on {}", path.display());
            serve_until_stopped(listener, router(state)).await
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let args = match parse(&args) {
        Ok(args) => args,
        Err(problem) => {
            eprintln!("nodo-web-gui: {problem}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).max_blocking_threads(BLOCKING_THREADS).enable_all().build();
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("nodo-web-gui: cannot start: {error}");
            return ExitCode::FAILURE;
        }
    };
    let served = runtime.block_on(serve(args));
    // A job still running holds a blocking thread for minutes; the service is stopping, so it
    // is not waited for. The helper finishes or undoes what it was asked on its own.
    runtime.shutdown_background();
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("nodo-web-gui: {problem}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn the_defaults_are_the_devices_sockets() {
        let parsed = parse(&[]).expect("no arguments are fine");
        assert_eq!(parsed.socket, PathBuf::from(nodo_api::WEBGUI_SOCKET));
        assert!(matches!(parsed.listen, Listen::Unix(path) if path == PathBuf::from(nodo_api::WEBGUI_HTTP_SOCKET)));
    }

    #[test]
    fn mock_serves_only_on_a_loopback_address() {
        assert!(matches!(parse(&args("--mock")).map(|a| a.listen), Ok(Listen::Mock(address)) if address.port() == 8080));
        assert!(matches!(parse(&args("--mock 127.0.0.1:9001")).map(|a| a.listen), Ok(Listen::Mock(address)) if address.port() == 9001));
        assert!(matches!(parse(&args("--mock [::1]:9001")).map(|a| a.listen), Ok(Listen::Mock(_))));
        for refused in ["--mock 0.0.0.0:8080", "--mock 192.168.1.20:8080", "--mock [::]:8080", "--mock [::ffff:127.0.0.1]:8080"] {
            assert!(parse(&args(refused)).is_err_and(|problem| problem.contains("loopback")), "{refused}");
        }
        assert!(parse(&args("--mock nonsense")).is_err());
    }

    #[test]
    fn unknown_arguments_are_refused() {
        assert!(parse(&args("--port 80")).is_err());
        assert!(parse(&args("--socket")).is_err());
    }
}
