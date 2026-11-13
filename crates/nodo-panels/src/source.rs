//! Fetching a panel's data: the package's own service on loopback, or the device itself.
//!
//! The client is deliberately small. It only ever talks plain HTTP/1.1 to a loopback address the
//! manifest declared, with a deadline, a size limit and no redirects, so it needs no TLS, no DNS
//! and no connection reuse.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use nodo_manifest::PanelSource;
use serde_json::Value as Json;

pub const TIMEOUT: Duration = Duration::from_secs(2);
pub const MAX_BODY: usize = 256 * 1024;
const MAX_HEADER: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    #[error("cannot reach the service ({0})")]
    Unreachable(String),
    #[error("timed out")]
    Timeout,
    #[error("answered {0}")]
    Status(u16),
    #[error("sent a response the screen cannot read ({0})")]
    Malformed(String),
    #[error("sent more than {MAX_BODY} bytes")]
    TooLarge,
    #[error("sent something other than JSON")]
    NotJson,
    #[error("returned a JSON-RPC error: {0}")]
    Rpc(String),
}

/// A URL already checked by manifest validation, split into what the client needs.
struct Target {
    addr: SocketAddr,
    host: String,
    path: String,
}

fn target(url: &str) -> Result<Target, FetchError> {
    let bad = || FetchError::Malformed("bad URL".into());
    let rest = url.strip_prefix("http://").ok_or_else(bad)?;
    let (authority, path) = match rest.find('/') {
        Some(at) => rest.split_at(at),
        None => (rest, "/"),
    };
    let (host, port) = authority.rsplit_once(':').ok_or_else(bad)?;
    let port: u16 = port.parse().map_err(|_| bad())?;
    // Never resolved through DNS: only the three loopback spellings validation allows.
    let ip = match host {
        "127.0.0.1" | "localhost" => IpAddr::V4(Ipv4Addr::LOCALHOST),
        "[::1]" => IpAddr::V6(Ipv6Addr::LOCALHOST),
        _ => return Err(FetchError::Malformed("not a loopback address".into())),
    };
    Ok(Target { addr: SocketAddr::new(ip, port), host: authority.to_string(), path: path.to_string() })
}

/// Fetches a panel's data. `device_root` is `/` except in tests.
pub fn fetch(source: &PanelSource, device_root: &Path) -> Result<Json, FetchError> {
    match source {
        PanelSource::Http { url } => http(&target(url)?, None),
        PanelSource::Jsonrpc { url, method } => {
            let body = serde_json::json!({ "jsonrpc": "2.0", "id": "0", "method": method }).to_string();
            let reply = http(&target(url)?, Some(&body))?;
            if let Some(error) = reply.get("error").filter(|e| !e.is_null()) {
                let message = error.get("message").and_then(Json::as_str).unwrap_or("no message");
                return Err(FetchError::Rpc(crate::output::clean(message, 80)));
            }
            reply.get("result").cloned().ok_or_else(|| FetchError::Malformed("no result".into()))
        }
        PanelSource::Device {} => Ok(device(device_root)),
    }
}

fn http(target: &Target, post: Option<&str>) -> Result<Json, FetchError> {
    let deadline = Instant::now() + TIMEOUT;
    let mut stream = TcpStream::connect_timeout(&target.addr, TIMEOUT).map_err(|e| match e.kind() {
        std::io::ErrorKind::TimedOut => FetchError::Timeout,
        _ => FetchError::Unreachable(e.kind().to_string()),
    })?;
    let request = match post {
        None => format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
            target.path, target.host
        ),
        Some(body) => format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            target.path,
            target.host,
            body.len()
        ),
    };
    stream.set_write_timeout(Some(TIMEOUT)).ok();
    stream.write_all(request.as_bytes()).map_err(|e| FetchError::Unreachable(e.kind().to_string()))?;
    let raw = read_all(&mut stream, deadline)?;
    let body = parse_response(&raw)?;
    serde_json::from_slice(&body).map_err(|_| FetchError::NotJson)
}

fn read_all(stream: &mut TcpStream, deadline: Instant) -> Result<Vec<u8>, FetchError> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(FetchError::Timeout);
        }
        stream.set_read_timeout(Some(left)).ok();
        match stream.read(&mut buf) {
            Ok(0) => return Ok(raw),
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_HEADER + MAX_BODY + 64 * 1024 {
                    return Err(FetchError::TooLarge);
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err(FetchError::Timeout)
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FetchError::Unreachable(e.kind().to_string())),
        }
    }
}

/// Splits a complete response into its body, checking the status and decoding chunked transfer.
fn parse_response(raw: &[u8]) -> Result<Vec<u8>, FetchError> {
    let malformed = |what: &str| FetchError::Malformed(what.into());
    let end = find(raw, b"\r\n\r\n").ok_or_else(|| malformed("no end of headers"))?;
    if end > MAX_HEADER {
        return Err(malformed("headers too long"));
    }
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| malformed("headers not text"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(malformed("not HTTP/1"));
    }
    let status: u16 = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| malformed("no status"))?;
    if status != 200 {
        return Err(FetchError::Status(status));
    }
    let mut chunked = false;
    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked") {
            chunked = true;
        } else if name.eq_ignore_ascii_case("content-length") {
            length = Some(value.parse::<usize>().map_err(|_| malformed("bad length"))?);
        }
    }
    let body = &raw[end + 4..];
    let body = if chunked {
        dechunk(body)?
    } else if let Some(length) = length {
        if length > MAX_BODY {
            return Err(FetchError::TooLarge);
        }
        body.get(..length).ok_or_else(|| malformed("body shorter than its length"))?.to_vec()
    } else {
        body.to_vec()
    };
    if body.len() > MAX_BODY {
        return Err(FetchError::TooLarge);
    }
    Ok(body)
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, FetchError> {
    let malformed = || FetchError::Malformed("bad chunked body".into());
    let mut out = Vec::new();
    loop {
        let line_end = find(data, b"\r\n").ok_or_else(malformed)?;
        let size_text = std::str::from_utf8(&data[..line_end]).map_err(|_| malformed())?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| malformed())?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if out.len() + size > MAX_BODY {
            return Err(FetchError::TooLarge);
        }
        let chunk = data.get(..size).ok_or_else(malformed)?;
        out.extend_from_slice(chunk);
        data = data.get(size + 2..).ok_or_else(malformed)?;
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// The device's own state for the base system's panels. Each value is left out when it cannot be
/// read, rather than failing the panel.
pub fn device(root: &Path) -> Json {
    let read = |path: &str| std::fs::read_to_string(root.join(path.trim_start_matches('/'))).ok();
    let mut out = serde_json::Map::new();
    // The hottest thermal zone: the board's zones are the SoC, its CPU clusters and the GPU.
    let zones = std::fs::read_dir(root.join("sys/class/thermal")).into_iter().flatten().flatten();
    let hottest = zones
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("thermal_zone"))
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("temp")).ok())
        .filter_map(|text| text.trim().parse::<i64>().ok())
        .max();
    if let Some(milli) = hottest {
        out.insert("temp_c".into(), Json::from(milli as f64 / 1000.0));
    }
    // The SSD's own sensor, through the kernel's NVMe hwmon driver (CONFIG_NVME_HWMON). The
    // hottest NVMe drive when there are several; absent when none reports.
    let hwmons = std::fs::read_dir(root.join("sys/class/hwmon")).into_iter().flatten().flatten();
    let ssd = hwmons
        .filter(|entry| std::fs::read_to_string(entry.path().join("name")).is_ok_and(|n| n.trim() == "nvme"))
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("temp1_input")).ok())
        .filter_map(|text| text.trim().parse::<i64>().ok())
        .max();
    if let Some(milli) = ssd {
        out.insert("ssd_temp_c".into(), Json::from(milli as f64 / 1000.0));
    }
    if let Some(meminfo) = read("/proc/meminfo") {
        let field = |name: &str| {
            meminfo
                .lines()
                .find_map(|line| line.strip_prefix(name)?.trim().strip_suffix("kB")?.trim().parse::<u64>().ok())
        };
        if let (Some(total), Some(available)) = (field("MemTotal:"), field("MemAvailable:")) {
            out.insert("mem_total".into(), Json::from(total * 1024));
            out.insert("mem_used".into(), Json::from(total.saturating_sub(available) * 1024));
        }
    }
    if let Some(uptime) = read("/proc/uptime").and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok()) {
        out.insert("uptime_secs".into(), Json::from(uptime as u64));
    }
    // The blockchain drive when it is mounted, else the root filesystem.
    let drive = ["media/monero", ""].iter().map(|p| root.join(p)).find_map(|path| statvfs(&path));
    if let Some((total, free)) = drive {
        out.insert("ssd_total".into(), Json::from(total));
        out.insert("ssd_used".into(), Json::from(total.saturating_sub(free)));
    }
    Json::Object(out)
}

/// Total and available bytes of the filesystem holding `path`, only if `path` is a mount point's
/// filesystem the caller asked for (an unmounted `/media/monero` reports the root's, which is
/// still a useful answer, so it is not distinguished).
fn statvfs(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs writes only into `stat`, and `c_path` is a valid NUL terminated string.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block = stat.f_frsize as u64;
    Some((stat.f_blocks as u64 * block, stat.f_bavail as u64 * block))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// A one-shot server that answers the next connection with `reply` and hands back the request.
    fn serve(reply: &'static [u8]) -> (u16, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut request = vec![0u8; 4096];
            let n = conn.read(&mut request).unwrap();
            conn.write_all(reply).unwrap();
            String::from_utf8_lossy(&request[..n]).into_owned()
        });
        (port, handle)
    }

    #[test]
    fn gets_json_from_the_service() {
        let (port, server) = serve(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\r\n{\"sales\": 37}");
        let data = fetch(&PanelSource::Http { url: format!("http://127.0.0.1:{port}/stats") }, Path::new("/")).unwrap();
        assert_eq!(data["sales"], 37);
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /stats HTTP/1.1\r\n"), "{request}");
        assert!(!request.to_ascii_lowercase().contains("cookie"));
    }

    #[test]
    fn calls_json_rpc_and_returns_the_result() {
        let (port, server) = serve(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\n{\"result\":{\"heig\r\na\r\nht\":2121}}\r\n0\r\n\r\n",
        );
        let source = PanelSource::Jsonrpc { url: format!("http://localhost:{port}/json_rpc"), method: "get_info".into() };
        assert_eq!(fetch(&source, Path::new("/")).unwrap()["height"], 2121);
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /json_rpc HTTP/1.1\r\n"));
        assert!(request.ends_with(r#"{"id":"0","jsonrpc":"2.0","method":"get_info"}"#), "{request}");
    }

    #[test]
    fn failures_are_reported_not_drawn() {
        type Case = (&'static [u8], fn(&FetchError) -> bool);
        let cases: [Case; 4] = [
            (b"HTTP/1.1 503 Busy\r\nContent-Length: 0\r\n\r\n", |e| *e == FetchError::Status(503)),
            (b"HTTP/1.1 302 Found\r\nLocation: http://evil/\r\nContent-Length: 0\r\n\r\n", |e| *e == FetchError::Status(302)),
            (b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello", |e| *e == FetchError::NotJson),
            (b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n{}", |e| *e == FetchError::TooLarge),
        ];
        for (reply, check) in cases {
            let (port, server) = serve(reply);
            let error = fetch(&PanelSource::Http { url: format!("http://127.0.0.1:{port}/") }, Path::new("/")).unwrap_err();
            assert!(check(&error), "{error:?}");
            server.join().unwrap();
        }
        let (port, server) = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 45\r\n\r\n{\"error\":{\"code\":-32601,\"message\":\"no such\"}}");
        let source = PanelSource::Jsonrpc { url: format!("http://127.0.0.1:{port}/json_rpc"), method: "x".into() };
        assert_eq!(fetch(&source, Path::new("/")).unwrap_err(), FetchError::Rpc("no such".into()));
        server.join().unwrap();
        // Nothing listens: closed port.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(matches!(
            fetch(&PanelSource::Http { url: format!("http://127.0.0.1:{closed}/") }, Path::new("/")),
            Err(FetchError::Unreachable(_))
        ));
        // A non-loopback URL never connects, whatever slipped past validation.
        assert!(matches!(target("http://10.0.0.1:8081/"), Err(FetchError::Malformed(_))));
    }

    #[test]
    fn a_service_that_never_answers_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _hold = thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(4));
            drop(conn);
        });
        let started = Instant::now();
        let error = fetch(&PanelSource::Http { url: format!("http://127.0.0.1:{port}/") }, Path::new("/")).unwrap_err();
        assert_eq!(error, FetchError::Timeout);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn reads_the_device() {
        let root = std::env::temp_dir().join(format!("nodo-panels-device-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (zone, temp) in [("thermal_zone0", "48000"), ("thermal_zone1", "52500")] {
            std::fs::create_dir_all(root.join("sys/class/thermal").join(zone)).unwrap();
            std::fs::write(root.join("sys/class/thermal").join(zone).join("temp"), temp).unwrap();
        }
        std::fs::create_dir_all(root.join("proc")).unwrap();
        std::fs::write(root.join("proc/meminfo"), "MemTotal:       32000000 kB\nMemFree: 1 kB\nMemAvailable:   26000000 kB\n").unwrap();
        std::fs::write(root.join("proc/uptime"), "259200.42 1000.0\n").unwrap();
        // Only the hwmon device named "nvme" is the SSD.
        for (hwmon, name, temp) in [("hwmon0", "package_thermal", "90000"), ("hwmon9", "nvme", "61850")] {
            let dir = root.join("sys/class/hwmon").join(hwmon);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("name"), format!("{name}\n")).unwrap();
            std::fs::write(dir.join("temp1_input"), temp).unwrap();
        }
        let data = device(&root);
        assert_eq!(data["temp_c"], 52.5);
        assert_eq!(data["ssd_temp_c"], 61.85);
        assert_eq!(data["mem_total"], 32000000u64 * 1024);
        assert_eq!(data["mem_used"], 6000000u64 * 1024);
        assert_eq!(data["uptime_secs"], 259200);
        assert!(data["ssd_total"].as_u64().unwrap() > 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_device_leaves_out_an_ssd_temperature_it_cannot_read() {
        let root = std::env::temp_dir().join(format!("nodo-panels-device-nossd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sys/class/hwmon/hwmon0")).unwrap();
        std::fs::write(root.join("sys/class/hwmon/hwmon0/name"), "gpu_thermal\n").unwrap();
        std::fs::write(root.join("sys/class/hwmon/hwmon0/temp1_input"), "45000").unwrap();
        let data = device(&root);
        assert!(data.get("ssd_temp_c").is_none(), "{data}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
