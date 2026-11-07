//! A deliberately narrow URL check.
//!
//! This is not a general URL parser. It accepts only what a service endpoint needs, because every
//! leniency here is a chance for this code and the program reading the URL to disagree about which
//! host it names. Credentials are refused outright: they would leak through logs and enable the
//! `http://trusted@evil` spoof.

const MAX_URL_LEN: usize = 2048;
const SCHEMES: [&str; 4] = ["http", "https", "ws", "wss"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl<'a> {
    pub scheme: &'a str,
    /// IPv6 hosts keep their brackets.
    pub host: &'a str,
    pub port: Option<u16>,
}

impl ParsedUrl<'_> {
    /// The explicit port, or the scheme's default.
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.scheme {
            "https" | "wss" => 443,
            _ => 80,
        })
    }

    pub fn is_loopback(&self) -> bool {
        matches!(self.host, "127.0.0.1" | "localhost" | "[::1]")
    }
}

fn is_dns_host(host: &str) -> bool {
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn is_ipv6_host(host: &str) -> bool {
    let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) else {
        return false;
    };
    inner.parse::<std::net::Ipv6Addr>().is_ok()
}

fn is_valid_tail(tail: &str) -> bool {
    let bytes = tail.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                let hex = bytes.get(at + 1..at + 3);
                if !hex.is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) {
                    return false;
                }
                at += 3;
                continue;
            }
            b if b.is_ascii_alphanumeric() => {}
            b'-' | b'.' | b'_' | b'~' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';'
            | b'=' | b':' | b'@' | b'/' | b'?' | b'#' => {}
            _ => return false,
        }
        at += 1;
    }
    true
}

/// Checks a URL and returns its parts.
pub fn check_url(text: &str) -> Result<ParsedUrl<'_>, &'static str> {
    if text.len() > MAX_URL_LEN {
        return Err("is too long");
    }
    let (scheme, rest) = text.split_once("://").ok_or("must start with http:// or https://")?;
    if !SCHEMES.contains(&scheme) {
        return Err("must use http, https, ws or wss");
    }
    let tail_at = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(tail_at);
    if !is_valid_tail(tail) {
        return Err("contains characters that are not valid in a URL");
    }
    if authority.contains('@') {
        return Err("must not contain credentials");
    }
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']').ok_or("has an invalid host")?;
        let (host, after) = authority.split_at(end + 1);
        match after {
            "" => (host, None),
            _ => (host, Some(after.strip_prefix(':').ok_or("has an invalid host")?)),
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if !is_dns_host(host) && !is_ipv6_host(host) {
        return Err("has an invalid host");
    }
    let port = match port {
        None => None,
        Some(port) => {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) || port.starts_with('0') {
                return Err("has an invalid port");
            }
            Some(port.parse::<u16>().map_err(|_| "has an invalid port")?)
        }
    };
    Ok(ParsedUrl { scheme, host, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_service_urls() {
        for (url, host, port) in [
            ("http://127.0.0.1:18081", "127.0.0.1", 18081),
            ("https://node.example.com/rpc?x=1#f", "node.example.com", 443),
            ("http://abcdefghijklmnop234567.onion/", "abcdefghijklmnop234567.onion", 80),
            ("http://[::1]:8080/a%20b", "[::1]", 8080),
            ("wss://example.com:9000/ws", "example.com", 9000),
        ] {
            let parsed = check_url(url).unwrap_or_else(|e| panic!("{url}: {e}"));
            assert_eq!(parsed.host, host);
            assert_eq!(parsed.effective_port(), port);
        }
    }

    #[test]
    fn rejects_anything_ambiguous() {
        let long = format!("http://a/{}", "x".repeat(MAX_URL_LEN));
        for bad in [
            "",
            "127.0.0.1:80",
            "ftp://example.com",
            "file:///etc/passwd",
            "javascript://alert(1)",
            "http://",
            "http:///path",
            "http://user:pw@example.com",
            "http://trusted@evil.example",
            "http://exa mple.com",
            "http://example.com:0",
            "http://example.com:08",
            "http://example.com:+80",
            "http://example.com:65536",
            "http://example.com:",
            "http://exa\nmple.com",
            "http://example.com/a\nb",
            "http://example.com/\"quote",
            "http://example.com\\@evil.example",
            "http://example.com/a b",
            "http://example.com/%zz",
            "http://example.com/%2",
            "http://exämple.com",
            "http://[::1",
            "http://[nothex]",
            "http://[::1]x",
            "http://-bad.example",
            "http://a..b",
            long.as_str(),
        ] {
            assert!(check_url(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn loopback_detection_is_exact() {
        assert!(check_url("http://localhost:1").unwrap().is_loopback());
        assert!(check_url("http://[::1]:1").unwrap().is_loopback());
        assert!(!check_url("http://127.0.0.1.example.com:1").unwrap().is_loopback());
        assert!(!check_url("http://localhost.example:1").unwrap().is_loopback());
    }
}
