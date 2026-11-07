use nodo_manifest::HealthCheck;

use crate::helper::Helper;
use crate::service::parse_sv_status;
use crate::system::{Probe, System};

/// Turns a manifest's health check into a probe, refusing anything that is not loopback. The
/// manifest is untrusted, and an unchecked URL would let it make root connect anywhere.
pub(crate) fn probe_for(check: &HealthCheck) -> Result<Probe, String> {
    match check {
        HealthCheck::Tcp { port: 0 } => Err("health check port is invalid".to_owned()),
        HealthCheck::Tcp { port } => Ok(Probe::Tcp { port: *port }),
        HealthCheck::Http { url } => parse_url(url),
    }
}

fn parse_url(url: &str) -> Result<Probe, String> {
    let invalid = || "health check must be an http URL on 127.0.0.1 or localhost".to_owned();
    let rest = url.strip_prefix("http://").ok_or_else(invalid)?;
    let (authority, path) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        None => (rest, "/"),
    };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            (host, port.parse::<u16>().map_err(|_| invalid())?)
        }
        None => (authority, 80),
    };
    if !matches!(host, "127.0.0.1" | "localhost") || port == 0 {
        return Err(invalid());
    }
    // The path is written into a request, so CR and LF would inject headers.
    if !path.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(invalid());
    }
    Ok(Probe::Http {
        port,
        path: path.to_owned(),
    })
}

impl<S: System> Helper<S> {
    /// Waits for the service to run and pass its check. Without a declared check, it has to stay
    /// up long enough to rule out an immediate crash.
    pub(crate) fn wait_healthy(&self, id: &str, probe: Option<&Probe>) -> Result<(), String> {
        let mut last = "service never started".to_owned();
        for attempt in 0..self.timing.health_attempts {
            if attempt > 0 {
                self.sys.sleep(self.timing.health_interval);
            }
            let text = match self.sys.sv_status(id) {
                Ok(text) => text,
                Err(e) => {
                    last = e.0;
                    continue;
                }
            };
            let status = parse_sv_status(&text);
            if !status.is_running() {
                last = "service is not running".to_owned();
                continue;
            }
            match probe {
                Some(probe) => {
                    if self.sys.probe(probe) {
                        return Ok(());
                    }
                    last = "health check did not pass".to_owned();
                }
                None => {
                    if status
                        .uptime_secs()
                        .is_some_and(|up| up >= self.timing.stable_secs)
                    {
                        return Ok(());
                    }
                    last = "service has not stayed up".to_owned();
                }
            }
        }
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(url: &str) -> Result<Probe, String> {
        probe_for(&HealthCheck::Http {
            url: url.to_owned(),
        })
    }

    #[test]
    fn accepts_loopback_urls() {
        assert_eq!(
            http("http://127.0.0.1:8080/healthz").unwrap(),
            Probe::Http {
                port: 8080,
                path: "/healthz".into()
            }
        );
        assert_eq!(
            http("http://localhost").unwrap(),
            Probe::Http {
                port: 80,
                path: "/".into()
            }
        );
    }

    #[test]
    fn refuses_anything_that_could_leave_the_machine_or_inject_headers() {
        for url in [
            "https://127.0.0.1/",
            "http://192.168.1.1/",
            "http://example.com/",
            "http://127.0.0.1@example.com/",
            "http://127.0.0.1.example.com/",
            "http://127.0.0.1:0/",
            "http://127.0.0.1:99999/",
            "http://127.0.0.1:+80/",
            "http://127.0.0.1:/",
            "http://127.0.0.1/a b",
            "http://127.0.0.1/a\r\nX-Evil: 1",
            "file:///etc/shadow",
            "",
        ] {
            assert!(http(url).is_err(), "{url:?}");
        }
        assert!(probe_for(&HealthCheck::Tcp { port: 0 }).is_err());
    }
}
