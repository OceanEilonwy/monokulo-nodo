//! Downloads feed files over HTTPS or plain HTTP.
//!
//! Nothing downloaded here is trusted: the index names the feed's key, which must match the pin,
//! and each package must match the index and carry a signature by that key. So this client only
//! has to be bounded and honest about failures:
//!
//! - Every response is size-limited, and a server that stops sending fails after a fixed time
//!   instead of hanging the helper. A slow but steady download is allowed a time that grows
//!   with its size.
//! - Redirects are followed by hand, at most [`MAX_REDIRECTS`], to any host (GitHub release
//!   downloads redirect to a storage host), but never from HTTPS down to plain HTTP: that would
//!   hand an attacker on the network the choice of what this device sees.
//! - Errors are short and plain, written for the person reading the TUI.
//!
//! HTTPS uses the system certificate bundle, so a certificate the device does not trust fails
//! like it would in a browser.

use std::io;
use std::time::Duration;

/// Enough for GitHub's two hops, from `latest` to a tag and from the tag to storage.
pub const MAX_REDIRECTS: usize = 5;
const SYSTEM_ROOTS: &str = "/etc/ssl/certs/ca-certificates.crt";
const MAX_URL: usize = 4096;

/// How long a server may take at each step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// Name lookup and TCP connection, each.
    pub connect: Duration,
    /// From sending the request to the response headers.
    pub response: Duration,
    /// Between any two reads of the body: a server that stalls longer has stopped.
    pub stall: Duration,
    /// The minimum rate a whole download must keep, in bytes per second, on top of `base`.
    pub min_rate: u64,
    pub base: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(15),
            response: Duration::from_secs(30),
            stall: Duration::from_secs(30),
            min_rate: 32 * 1024,
            base: Duration::from_secs(120),
        }
    }
}

impl Timeouts {
    fn total(&self, limit: u64) -> Duration {
        self.base + Duration::from_secs(limit / self.min_rate.max(1))
    }
}

/// A plain HTTP(S) client for feed files, and for the system update server.
pub struct Fetcher {
    timeouts: Timeouts,
    roots: ureq::tls::RootCerts,
    /// What errors call the server. Messages are written for feeds.
    server: &'static str,
}

fn system_roots() -> ureq::tls::RootCerts {
    let certs: Vec<_> = std::fs::read(SYSTEM_ROOTS)
        .map(|pem| {
            ureq::tls::parse_pem(&pem)
                .filter_map(|item| match item {
                    Ok(ureq::tls::PemItem::Certificate(cert)) => Some(cert),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    if certs.is_empty() {
        ureq::tls::RootCerts::WebPki
    } else {
        ureq::tls::RootCerts::new_with_certs(&certs)
    }
}

fn failure(text: impl Into<String>) -> io::Error {
    io::Error::other(text.into())
}

/// The scheme and authority of an absolute `http://` or `https://` URL, such as
/// `https://host:8443`.
fn origin(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some((scheme, &url[..scheme.len() + 3 + end]))
}

/// Resolves `.` and `..` in the path of a path-and-query, as RFC 3986 section 5.2.4 does.
fn without_dot_segments(path_and_query: &str) -> String {
    let (path, query) = match path_and_query.find('?') {
        Some(i) => path_and_query.split_at(i),
        None => (path_and_query, ""),
    };
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        match *segment {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                out.pop();
                if last {
                    out.push("");
                }
            }
            other => out.push(other),
        }
    }
    if path.is_empty() {
        return format!("{path}{query}");
    }
    format!("/{}{query}", out.join("/"))
}

/// Where a redirect from `current` to `location` leads, or why it is refused.
pub fn redirect_target(current: &str, location: &str) -> Result<String, String> {
    let location = location.trim();
    let location = location.split('#').next().unwrap_or_default();
    if location.is_empty() || location.len() > MAX_URL || location.chars().any(|c| c.is_control() || c == ' ') {
        return Err("the feed server sent an unusable redirect".into());
    }
    let (scheme, base) = origin(current).ok_or("the feed address is not an http or https URL")?;
    let next = if location.contains("://") {
        location.to_owned()
    } else if let Some(rest) = location.strip_prefix("//") {
        format!("{scheme}://{rest}")
    } else if location.starts_with('/') {
        format!("{base}{location}")
    } else {
        let path_end = current.find(['?', '#']).unwrap_or(current.len());
        let dir_end = current[..path_end].rfind('/').filter(|&i| i >= base.len()).map_or(path_end, |i| i + 1);
        let dir = &current[..dir_end];
        if dir.len() == base.len() {
            format!("{base}/{location}")
        } else {
            format!("{dir}{location}")
        }
    };
    let (next_scheme, next_base) =
        origin(&next).ok_or("the feed server redirected to something that is not http or https")?;
    let next = format!("{next_base}{}", without_dot_segments(&next[next_base.len()..]));
    if scheme == "https" && next_scheme == "http" {
        return Err("the feed server redirected from HTTPS to plain HTTP, which this device refuses".into());
    }
    Ok(next)
}

/// The text of a ureq error can be long and technical. The person reading it needs to know
/// whether to check the address, the network or the server.
fn explain(error: ureq::Error, timeouts: &Timeouts) -> io::Error {
    use ureq::Error;
    const UNTRUSTED: &str =
        "the feed server's HTTPS certificate is not trusted by this device, or the connection was tampered with";
    let text = match error {
        Error::Timeout(ureq::Timeout::Resolve) | Error::Timeout(ureq::Timeout::Connect) => {
            format!("the feed server could not be reached within {} seconds", timeouts.connect.as_secs())
        }
        Error::Timeout(ureq::Timeout::RecvResponse) | Error::Timeout(ureq::Timeout::SendRequest) => {
            format!("the feed server did not answer within {} seconds", timeouts.response.as_secs())
        }
        Error::Timeout(ureq::Timeout::RecvBody) | Error::Timeout(ureq::Timeout::PerCall) | Error::Timeout(ureq::Timeout::Global) => {
            "the download took too long and was stopped".into()
        }
        Error::Timeout(_) => format!("the feed server stopped sending data for {} seconds", timeouts.stall.as_secs()),
        Error::HostNotFound => "the feed's host name could not be found".into(),
        Error::ConnectionFailed => "could not connect to the feed server".into(),
        // rustls reports a bad certificate during the handshake as an I/O error.
        Error::Io(e) if is_tls(&e) => UNTRUSTED.into(),
        Error::Io(e) => match e.kind() {
            io::ErrorKind::ConnectionRefused => "the feed server refused the connection".into(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                format!("the feed server stopped sending data for {} seconds", timeouts.stall.as_secs())
            }
            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => {
                "the feed server closed the connection early".into()
            }
            _ if e.raw_os_error() == Some(libc::EHOSTUNREACH) || e.raw_os_error() == Some(libc::ENETUNREACH) => {
                "the feed server could not be reached: no route to it".into()
            }
            _ => format!("could not download from the feed server: {e}"),
        },
        Error::Tls(_) | Error::Rustls(_) => UNTRUSTED.into(),
        Error::BodyExceedsLimit(limit) => format!("the file is larger than the {} bytes allowed", limit.saturating_sub(1)),
        other => format!("could not download from the feed server: {other}"),
    };
    failure(text)
}

fn is_tls(error: &io::Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    ["certificate", "handshake", "tls", "alert"].iter().any(|word| text.contains(word))
}

impl Fetcher {
    pub fn new() -> Self {
        Fetcher::with(Timeouts::default(), system_roots())
    }

    pub fn with(timeouts: Timeouts, roots: ureq::tls::RootCerts) -> Self {
        Fetcher { timeouts, roots, server: "feed" }
    }

    /// The same client, naming the server differently in its errors, such as `update`.
    pub fn named(mut self, server: &'static str) -> Self {
        self.server = server;
        self
    }

    fn relabel(&self, error: io::Error) -> io::Error {
        if self.server == "feed" {
            return error;
        }
        let text = error
            .to_string()
            .replace("the feed server", &format!("the {} server", self.server))
            .replace("the feed's host", &format!("the {} server's host", self.server))
            .replace("the feed address", &format!("the {} server address", self.server));
        io::Error::new(error.kind(), text)
    }

    fn agent(&self, limit: u64) -> ureq::Agent {
        let t = &self.timeouts;
        ureq::Agent::config_builder()
            .tls_config(ureq::tls::TlsConfig::builder().root_certs(self.roots.clone()).build())
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_resolve(Some(t.connect))
            .timeout_connect(Some(t.connect))
            .timeout_recv_response(Some(t.response))
            .timeout_per_read(Some(t.stall))
            .timeout_global(Some(t.total(limit)))
            .user_agent("monokulo-nodo")
            .build()
            .into()
    }

    /// Downloads one file, refusing anything longer than `limit` bytes. `name` is how the file
    /// is called in errors.
    pub fn get(&self, url: &str, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        self.get_labelled(url, name, limit).map_err(|e| self.relabel(e))
    }

    fn get_labelled(&self, url: &str, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        let agent = self.agent(limit);
        let mut current = url.to_owned();
        origin(&current).ok_or_else(|| failure("the feed address is not an http or https URL"))?;
        for _ in 0..=MAX_REDIRECTS {
            let mut response = agent.get(&current).call().map_err(|e| explain(e, &self.timeouts))?;
            let status = response.status().as_u16();
            match status {
                200 => {}
                301 | 302 | 303 | 307 | 308 => {
                    let location = response
                        .headers()
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(|| failure("the feed server sent a redirect without a target"))?;
                    current = redirect_target(&current, location).map_err(failure)?;
                    continue;
                }
                404 | 410 => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("the feed server has no {name} (HTTP {status})"),
                    ))
                }
                _ => return Err(failure(format!("the feed server answered HTTP {status} for {name}"))),
            }
            let declared = response
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok());
            if declared.is_some_and(|n| n > limit) {
                return Err(failure(format!("{name} is larger than the {limit} bytes allowed")));
            }
            // ureq refuses a body that reaches its limit, so one byte more tells "exactly the
            // limit" from "larger".
            let body = response
                .body_mut()
                .with_config()
                .limit(limit.saturating_add(1))
                .read_to_vec()
                .map_err(|e| explain(e, &self.timeouts))?;
            if body.len() as u64 > limit {
                return Err(failure(format!("{name} is larger than the {limit} bytes allowed")));
            }
            if declared.is_some_and(|n| n != body.len() as u64) {
                return Err(failure("the feed server closed the connection early"));
            }
            return Ok(body);
        }
        Err(failure(format!("the feed server redirected more than {MAX_REDIRECTS} times")))
    }
}

impl Fetcher {
    /// Like [`Fetcher::get`], with a missing file as `None` rather than an error.
    pub fn get_optional(&self, url: &str, name: &str, limit: u64) -> io::Result<Option<Vec<u8>>> {
        match self.get(url, name, limit) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Default for Fetcher {
    fn default() -> Self {
        Fetcher::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Instant;

    #[test]
    fn redirects_resolve_like_a_browser() {
        let base = "https://github.example/o/r/releases/latest/download/x86_64-repodata";
        assert_eq!(
            redirect_target(base, "/o/r/releases/download/v2/x86_64-repodata").unwrap(),
            "https://github.example/o/r/releases/download/v2/x86_64-repodata"
        );
        assert_eq!(
            redirect_target(base, "https://objects.example/a/b?sig=1#frag").unwrap(),
            "https://objects.example/a/b?sig=1"
        );
        assert_eq!(redirect_target(base, "//cdn.example/f").unwrap(), "https://cdn.example/f");
        assert_eq!(
            redirect_target(base, "other").unwrap(),
            "https://github.example/o/r/releases/latest/download/other"
        );
        assert_eq!(redirect_target("http://h:8080", "f").unwrap(), "http://h:8080/f");
        assert_eq!(redirect_target("http://h/a/b/c", "../d?x=../y").unwrap(), "http://h/a/d?x=../y");
        assert_eq!(redirect_target("http://h/a", "/../../etc/x").unwrap(), "http://h/etc/x");
        assert_eq!(redirect_target("http://h/feed/a", "https://h/feed/a").unwrap(), "https://h/feed/a");
    }

    #[test]
    fn redirects_never_drop_https_or_leave_http() {
        assert!(redirect_target("https://h/f", "http://h/f").unwrap_err().contains("plain HTTP"));
        assert!(redirect_target("https://h/f", "//h/f").is_ok());
        for bad in ["file:///etc/passwd", "ftp://h/f", "", "https://user@h/f", "https://h/a b"] {
            assert!(redirect_target("http://h/f", bad).is_err(), "{bad:?}");
        }
    }

    /// A one-file-per-request HTTP server on loopback. Each handler gets the request path and
    /// writes the whole response itself.
    fn serve(handler: impl Fn(&str, &mut TcpStream) + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    request.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
                handler(&path, &mut stream);
            }
        });
        address
    }

    fn respond(stream: &mut TcpStream, status: &str, headers: &[(&str, &str)], body: &[u8]) {
        let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
    }

    fn quick() -> Fetcher {
        Fetcher::with(
            Timeouts {
                connect: Duration::from_secs(2),
                response: Duration::from_secs(1),
                stall: Duration::from_secs(1),
                min_rate: 1024 * 1024,
                base: Duration::from_secs(5),
            },
            ureq::tls::RootCerts::WebPki,
        )
    }

    #[test]
    fn downloads_follow_redirects_and_keep_the_bytes_exact() {
        let url = serve(|path, s| match path {
            "/latest/download/f" => respond(s, "302 Found", &[("Location", "/v2/f")], b""),
            "/v2/f" => respond(s, "302 Found", &[("Location", "../assets/v2/f?sig=x")], b""),
            "/assets/v2/f?sig=x" => respond(s, "200 OK", &[], b"\x00exact\xffbytes"),
            _ => respond(s, "404 Not Found", &[], b""),
        });
        assert_eq!(quick().get(&format!("{url}/latest/download/f"), "f", 100).unwrap(), b"\x00exact\xffbytes");
    }

    #[test]
    fn failures_are_plain() {
        let url = serve(|path, s| match path {
            "/loop" => respond(s, "302 Found", &[("Location", "/loop")], b""),
            "/big" => respond(s, "200 OK", &[], &[0u8; 2000]),
            "/chunked-big" => {
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
                let _ = s.write_all(b"7d0\r\n");
                let _ = s.write_all(&[0u8; 2000]);
                let _ = s.write_all(b"\r\n0\r\n\r\n");
            }
            "/exact" => respond(s, "200 OK", &[], &[7u8; 100]),
            "/short" => {
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nonly a little");
            }
            "/teapot" => respond(s, "500 Internal Server Error", &[], b""),
            "/silent" => std::thread::sleep(Duration::from_secs(4)),
            _ => respond(s, "404 Not Found", &[], b""),
        });
        let f = quick();
        let err = |path: &str, limit: u64| f.get(&format!("{url}{path}"), "x86_64-repodata", limit).unwrap_err().to_string();
        assert!(err("/missing", 100).contains("has no x86_64-repodata (HTTP 404)"), "{}", err("/missing", 100));
        assert_eq!(f.get_optional(&format!("{url}/missing"), "x", 100).unwrap(), None);
        let update = quick().named("update");
        let e = update.get(&format!("{url}/teapot"), "x", 100).unwrap_err().to_string();
        assert_eq!(e, "the update server answered HTTP 500 for x");
        assert!(f.get_optional(&format!("{url}/teapot"), "x", 100).is_err());
        assert!(err("/loop", 100).contains("redirected more than 5 times"));
        assert!(err("/big", 100).contains("larger than the 100 bytes"), "{}", err("/big", 100));
        assert!(err("/chunked-big", 100).contains("larger than the 100 bytes"), "{}", err("/chunked-big", 100));
        assert_eq!(f.get(&format!("{url}/exact"), "x", 100).unwrap(), [7u8; 100], "exactly the limit is allowed");
        assert!(err("/short", 1000).contains("closed the connection early"), "{}", err("/short", 1000));
        assert!(err("/teapot", 100).contains("HTTP 500"));
        let started = Instant::now();
        let silent = err("/silent", 100);
        assert!(silent.contains("did not answer within 1 seconds"), "{silent}");
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn a_closed_port_is_refused_plainly() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let e = quick().get(&format!("http://127.0.0.1:{port}/f"), "f", 10).unwrap_err().to_string();
        assert!(e.contains("could not connect") || e.contains("refused"), "{e}");
    }

    #[test]
    fn a_stalled_body_stops() {
        let url = serve(|_, s| {
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nsome");
            std::thread::sleep(Duration::from_secs(4));
        });
        let started = Instant::now();
        let e = quick().get(&format!("{url}/f"), "f", 1000).unwrap_err().to_string();
        assert!(e.contains("stopped sending data") || e.contains("took too long"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}
