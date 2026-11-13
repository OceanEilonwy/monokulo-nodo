//! Safe shutdown (docs/SCREEN.md, "Shutting down").
//!
//! `nodo-power` runs as root and listens on [`SOCKET`], which only root and `_nodo_screen` may
//! open. It accepts exactly one request, [`REQUEST`], and answers with progress lines while it
//! stops every service, writes everything to disk and turns the device off. Nothing else can be
//! asked of it: no service names, no arguments, no other commands.
//!
//! This module holds the protocol and the shutdown sequence, with the system calls behind
//! [`System`] so that tests can run the sequence without stopping anything.

use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub const SOCKET: &str = "/run/nodo-power/socket";
/// Created just before the device turns off, so that the screen's finish script leaves the last
/// screen ("Safe to unplug") in place instead of clearing it.
pub const OFF_MARKER: &str = "/run/nodo-power/off";
/// Created before the first service stops, so that the boot splash's shutdown hook
/// (`/etc/runit/shutdown.d/05-nodo-splash.sh`) does not draw "shutting down…" over the screen's own
/// shutdown screens and "Safe to unplug".
pub const SPLASH_SKIP: &str = "/run/nodo-splash.skip";
/// The only request there is.
pub const REQUEST: &str = "poweroff";
/// A request longer than this is refused without reading further.
pub const MAX_REQUEST: usize = 64;

/// Services that are never stopped here: this service, and the screen that shows the progress.
/// runit stops them as the device turns off.
pub const KEEP_RUNNING: [&str; 2] = ["nodo-power", "nodo-screen"];

/// How long a service may take to stop. A node writing its database during the first sync can take
/// minutes; it is never killed while it saves.
pub fn stop_timeout(service: &str) -> Duration {
    match service {
        "cuprated" | "monerod" => Duration::from_secs(300),
        _ => Duration::from_secs(90),
    }
}

/// How long Void's runit stage 3 waits for a service before killing it (`sv force-stop` with the
/// default SVWAIT). Services that may need longer are stopped first, by `--before-shutdown`.
pub const RUNIT_FORCE_STOP_WAIT: Duration = Duration::from_secs(7);
/// The longest stop a manifest may declare.
pub const MAX_STOP: Duration = Duration::from_secs(3600);

/// The services an ordinary shutdown (poweroff or reboot over SSH, a critical temperature) must
/// stop before runit's general stop would kill them: each enabled service whose package declares a
/// stop longer than [`RUNIT_FORCE_STOP_WAIT`] (`stop_timeout_secs`), and the node whatever it
/// declares, with at least its [`stop_timeout`].
pub fn long_stops(enabled: &[String], declared: &std::collections::BTreeMap<String, u32>) -> Vec<(String, Duration)> {
    enabled
        .iter()
        .filter_map(|service| {
            let mut wait = declared.get(service).map(|s| Duration::from_secs(u64::from(*s)));
            if matches!(service.as_str(), "cuprated" | "monerod") {
                wait = Some(wait.unwrap_or_default().max(stop_timeout(service)));
            }
            let wait = wait?.min(MAX_STOP);
            (wait > RUNIT_FORCE_STOP_WAIT).then(|| (service.clone(), wait))
        })
        .collect()
}

/// Stops the given services at the same time, each with its own wait. Returns each service and
/// whether it stopped in time.
pub fn stop_all(system: &dyn System, stops: &[(String, Duration)]) -> Vec<(String, bool)> {
    thread::scope(|scope| {
        let handles: Vec<_> = stops.iter().map(|(service, wait)| scope.spawn(move || (service.clone(), system.stop(service, *wait)))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

/// One line of progress, as sent over the socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// How many services will be stopped, all at once.
    Services(usize),
    Stopping(String),
    Stopped { service: String, millis: u64 },
    /// It did not stop in time. Shutdown goes on; runit stops it as the device turns off.
    TimedOut { service: String, millis: u64 },
    Syncing,
    Synced { millis: u64 },
    /// Everything is stopped and on disk; the device turns off now.
    Off,
    Error(String),
}

impl Event {
    pub fn line(&self) -> String {
        match self {
            Event::Services(n) => format!("services {n}"),
            Event::Stopping(s) => format!("stopping {s}"),
            Event::Stopped { service, millis } => format!("stopped {service} {millis}"),
            Event::TimedOut { service, millis } => format!("timeout {service} {millis}"),
            Event::Syncing => "syncing".into(),
            Event::Synced { millis } => format!("synced {millis}"),
            Event::Off => "off".into(),
            Event::Error(text) => format!("error {}", text.replace(['\n', '\r'], " ")),
        }
    }

    /// Reads one line from the service. None for a line this version does not know.
    pub fn parse(line: &str) -> Option<Event> {
        let line = line.trim_end_matches(['\n', '\r']);
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let mut parts = rest.split(' ');
        let name_and_millis = |mut parts: std::str::Split<'_, char>| -> Option<(String, u64)> {
            let name = parts.next().filter(|n| valid_service_name(n))?.to_string();
            let millis = parts.next()?.parse().ok()?;
            Some((name, millis))
        };
        Some(match word {
            "services" => Event::Services(rest.parse().ok()?),
            "stopping" if valid_service_name(rest) => Event::Stopping(rest.to_string()),
            "stopped" => {
                let (service, millis) = name_and_millis(parts)?;
                Event::Stopped { service, millis }
            }
            "timeout" => {
                let (service, millis) = name_and_millis(parts)?;
                Event::TimedOut { service, millis }
            }
            "syncing" => Event::Syncing,
            "synced" => Event::Synced { millis: parts.next()?.parse().ok()? },
            "off" => Event::Off,
            "error" => Event::Error(rest.to_string()),
            _ => return None,
        })
    }
}

/// runit service directory names: what a service may be called in a progress line.
pub fn valid_service_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b)) && !name.starts_with('.')
}

/// Reads the request: exactly `poweroff`, with or without a newline. Anything else is refused.
pub fn read_request(stream: &mut impl Read) -> Result<(), String> {
    let mut buf = Vec::new();
    stream.take(MAX_REQUEST as u64 + 1).read_to_end(&mut buf).map_err(|e| format!("reading the request: {e}"))?;
    if buf.len() > MAX_REQUEST {
        return Err("the request is too long".into());
    }
    let text = std::str::from_utf8(&buf).map_err(|_| "the request is not text".to_string())?;
    if text.strip_suffix('\n').unwrap_or(text) == REQUEST {
        Ok(())
    } else {
        Err("unknown request: only poweroff is accepted".into())
    }
}

/// What the shutdown needs from the system.
pub trait System: Sync {
    /// Notes that this shutdown draws its own screens ([`SPLASH_SKIP`]), before anything stops.
    fn begin(&self);
    /// The services to stop: every enabled one except [`KEEP_RUNNING`].
    fn services(&self) -> io::Result<Vec<String>>;
    /// Stops one service, waiting up to `timeout`. True when it stopped.
    fn stop(&self, service: &str, timeout: Duration) -> bool;
    fn sync(&self);
    /// Marks the device as turning off and starts turning it off.
    fn poweroff(&self) -> io::Result<()>;
}

/// Enabled runit services, from the service directory's entries.
pub fn enabled_services(service_dir: &Path) -> io::Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(service_dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| valid_service_name(n) && !KEEP_RUNNING.contains(&n.as_str()))
        .collect();
    names.sort();
    Ok(names)
}

/// Runs the whole shutdown, sending each step to `send`. Every service is stopped at the same
/// time, each with its own timeout, so one slow service does not hold up the rest.
pub fn shut_down(system: &dyn System, send: &mut dyn FnMut(&Event)) -> io::Result<()> {
    let services = match system.services() {
        Ok(services) => services,
        Err(e) => {
            send(&Event::Error(format!("cannot list the services: {e}")));
            return Err(e);
        }
    };
    system.begin();
    send(&Event::Services(services.len()));
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        for service in &services {
            send(&Event::Stopping(service.clone()));
            let tx = tx.clone();
            scope.spawn(move || {
                let started = Instant::now();
                let stopped = system.stop(service, stop_timeout(service));
                let millis = started.elapsed().as_millis() as u64;
                let event = if stopped { Event::Stopped { service: service.clone(), millis } } else { Event::TimedOut { service: service.clone(), millis } };
                let _ = tx.send(event);
            });
        }
        drop(tx);
        for event in rx {
            send(&event);
        }
    });
    send(&Event::Syncing);
    let started = Instant::now();
    system.sync();
    send(&Event::Synced { millis: started.elapsed().as_millis() as u64 });
    send(&Event::Off);
    system.poweroff()
}

/// Reads progress lines until the connection closes, as the screen does.
pub fn read_events(reader: impl BufRead, mut on_event: impl FnMut(Event)) -> io::Result<()> {
    for line in reader.lines() {
        if let Some(event) = Event::parse(&line?) {
            on_event(event);
        }
    }
    Ok(())
}

/// The screen's side: asks for a shutdown and passes on each step. Returns when the service closes
/// the connection, which it does just before the device turns off.
pub fn request_poweroff(socket: &Path, on_event: impl FnMut(Event)) -> io::Result<()> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.write_all(REQUEST.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    read_events(io::BufReader::new(stream), on_event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn every_event_survives_a_round_trip() {
        let events = [
            Event::Services(3),
            Event::Stopping("cuprated".into()),
            Event::Stopped { service: "mk-pay".into(), millis: 1100 },
            Event::TimedOut { service: "cuprated".into(), millis: 300_000 },
            Event::Syncing,
            Event::Synced { millis: 42 },
            Event::Off,
            Event::Error("cannot list the services: gone".into()),
        ];
        for event in events {
            assert_eq!(Event::parse(&event.line()), Some(event.clone()), "{}", event.line());
        }
        assert_eq!(Event::parse("reboot"), None, "unknown lines are ignored");
        assert_eq!(Event::parse("stopping ../x"), None, "service names are checked");
        assert_eq!(Event::parse("stopped mk-pay soon"), None);
        assert_eq!(Event::Error("a\nb".into()).line(), "error a b", "one line each");
    }

    #[test]
    fn only_the_one_request_is_accepted() {
        assert!(read_request(&mut "poweroff\n".as_bytes()).is_ok());
        assert!(read_request(&mut "poweroff".as_bytes()).is_ok());
        for bad in ["", "reboot\n", "poweroff now\n", "poweroff\n\n", "POWEROFF\n", "sv stop cuprated\n"] {
            assert!(read_request(&mut bad.as_bytes()).is_err(), "{bad:?}");
        }
        let long = "x".repeat(MAX_REQUEST + 10);
        assert_eq!(read_request(&mut long.as_bytes()), Err("the request is too long".into()));
        assert!(read_request(&mut [0xff, 0xfe].as_slice()).is_err());
    }

    #[test]
    fn the_screen_and_this_service_are_left_running() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["cuprated", "nodo-screen", "nodo-power", "sshd", ".hidden", "mk-pay"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        assert_eq!(enabled_services(dir.path()).unwrap(), ["cuprated", "mk-pay", "sshd"]);
        assert_eq!(stop_timeout("cuprated"), Duration::from_secs(300));
        assert_eq!(stop_timeout("sshd"), Duration::from_secs(90));
    }

    struct Fake {
        log: Mutex<Vec<String>>,
        slow: &'static str,
    }

    impl System for Fake {
        fn begin(&self) {
            self.log.lock().unwrap().push("begin".into());
        }
        fn services(&self) -> io::Result<Vec<String>> {
            Ok(vec!["cuprated".into(), "mk-pay".into(), "sshd".into()])
        }
        fn stop(&self, service: &str, timeout: Duration) -> bool {
            if service == self.slow {
                thread::sleep(Duration::from_millis(50));
            }
            self.log.lock().unwrap().push(format!("stop {service} {}", timeout.as_secs()));
            service != "sshd"
        }
        fn sync(&self) {
            self.log.lock().unwrap().push("sync".into());
        }
        fn poweroff(&self) -> io::Result<()> {
            self.log.lock().unwrap().push("poweroff".into());
            Ok(())
        }
    }

    #[test]
    fn shutdown_stops_everything_then_syncs_then_turns_off() {
        let fake = Fake { log: Mutex::new(Vec::new()), slow: "cuprated" };
        let mut lines = Vec::new();
        shut_down(&fake, &mut |e| lines.push(e.clone())).unwrap();
        assert_eq!(lines[0], Event::Services(3));
        assert_eq!(&lines[1..4], &[Event::Stopping("cuprated".into()), Event::Stopping("mk-pay".into()), Event::Stopping("sshd".into())]);
        // The slow node reports last; the others do not wait for it.
        assert!(matches!(&lines[6], Event::Stopped { service, .. } if service == "cuprated"), "{lines:?}");
        assert!(lines.iter().any(|e| matches!(e, Event::TimedOut { service, .. } if service == "sshd")));
        assert_eq!(&lines[7..], &[Event::Syncing, lines[8].clone(), Event::Off]);
        assert!(matches!(lines[8], Event::Synced { .. }));
        let log = fake.log.lock().unwrap();
        assert!(log.contains(&"stop cuprated 300".to_string()));
        assert_eq!(log[0], "begin", "the splash is told to stay away before anything stops");
        assert_eq!(&log[4..], &["sync", "poweroff"], "sync and power off come after every stop");
    }

    #[test]
    fn an_ordinary_shutdown_first_waits_for_the_node_and_slow_packages() {
        let enabled: Vec<String> = ["cuprated", "mk-pay", "mk-bell", "sshd", "quick"].iter().map(|s| s.to_string()).collect();
        let declared: std::collections::BTreeMap<String, u32> =
            [("cuprated".to_string(), 120), ("mk-pay".to_string(), 30), ("quick".to_string(), 5), ("absent".to_string(), 600)].into_iter().collect();
        assert_eq!(
            long_stops(&enabled, &declared),
            [("cuprated".to_string(), Duration::from_secs(300)), ("mk-pay".to_string(), Duration::from_secs(30))],
            "the node gets at least 300 s, mk-pay its own 30 s; sshd declares nothing, quick fits runit's 7 s, absent is not enabled"
        );
        // The node is waited for even without a manifest; a longer declared stop wins, up to an hour.
        let none = std::collections::BTreeMap::new();
        assert_eq!(long_stops(&["cuprated".to_string()], &none), [("cuprated".to_string(), Duration::from_secs(300))]);
        let long: std::collections::BTreeMap<String, u32> = [("cuprated".to_string(), 7200)].into_iter().collect();
        assert_eq!(long_stops(&["cuprated".to_string()], &long), [("cuprated".to_string(), MAX_STOP)]);
    }

    #[test]
    fn slow_services_are_stopped_together_with_their_own_waits() {
        let fake = Fake { log: Mutex::new(Vec::new()), slow: "cuprated" };
        let stops = [("cuprated".to_string(), Duration::from_secs(300)), ("sshd".to_string(), Duration::from_secs(30))];
        let mut result = stop_all(&fake, &stops);
        result.sort();
        assert_eq!(result, [("cuprated".to_string(), true), ("sshd".to_string(), false)]);
        let mut log = fake.log.lock().unwrap().clone();
        log.sort();
        assert_eq!(log, ["stop cuprated 300", "stop sshd 30"]);
    }

    #[test]
    fn the_screen_reads_the_stream() {
        let stream = "services 1\nstopping mk-pay\nnonsense\nstopped mk-pay 5\nsyncing\nsynced 1\noff\n";
        let mut seen = Vec::new();
        read_events(stream.as_bytes(), |e| seen.push(e)).unwrap();
        assert_eq!(seen.len(), 6);
        assert_eq!(seen.last(), Some(&Event::Off));
    }
}
