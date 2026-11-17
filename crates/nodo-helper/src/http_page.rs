//! Reading one HTTP/1.1 answer from a package's own service: 200 only, a bounded body, chunked or
//! not. The service is untrusted, so every length is checked before it is used.

/// The body of a complete response held in `bytes`. Only `200` is accepted; the body is at most
/// `limit` bytes, whether sized by `Content-Length`, sent chunked, or ended by closing.
pub(crate) fn parse_response(bytes: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("the answer is not HTTP")?;
    let head =
        std::str::from_utf8(&bytes[..split]).map_err(|_| "the answer's headers are not text")?;
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let mut parts = status.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let code = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err("the answer is not HTTP/1".to_owned());
    }
    if code != "200" {
        let shown: String = status
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        return Err(format!("it answered {shown}"));
    }
    let mut length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| "its Content-Length is not a number")?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.eq_ignore_ascii_case("chunked");
            if !chunked {
                return Err("it uses a transfer encoding other than chunked".to_owned());
            }
        }
    }
    let body = &bytes[split + 4..];
    if chunked {
        return dechunk(body, limit);
    }
    match length {
        Some(n) if n > limit => Err(format!("its answer is larger than {} KiB", limit / 1024)),
        Some(n) if body.len() < n => Err("its answer ended early".to_owned()),
        Some(n) => Ok(body[..n].to_vec()),
        None if body.len() > limit => {
            Err(format!("its answer is larger than {} KiB", limit / 1024))
        }
        None => Ok(body.to_vec()),
    }
}

fn dechunk(mut body: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("its chunked answer ended early")?;
        let size_text =
            std::str::from_utf8(&body[..end]).map_err(|_| "a chunk size is not text")?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size =
            usize::from_str_radix(size_text, 16).map_err(|_| "a chunk size is not a number")?;
        body = &body[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if out.len().saturating_add(size) > limit {
            return Err(format!("its answer is larger than {} KiB", limit / 1024));
        }
        if body.len() < size + 2 || &body[size..size + 2] != b"\r\n" {
            return Err("its chunked answer ended early".to_owned());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sized_chunked_and_closed_answers() {
        let sized = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4\r\n\r\n{}\r\nxx";
        assert_eq!(parse_response(sized, 100).unwrap(), b"{}\r\n");
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n4;ext=1\r\n\": 1\r\n1\r\n}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked, 100).unwrap(), b"{\"a\": 1}");
        let closed = b"HTTP/1.0 200 OK\r\n\r\n{\"x\":2}";
        assert_eq!(parse_response(closed, 100).unwrap(), b"{\"x\":2}");
    }

    #[test]
    fn refuses_other_statuses_oversized_and_broken_answers() {
        assert!(
            parse_response(b"HTTP/1.1 302 Found\r\nLocation: http://x\r\n\r\n", 100)
                .unwrap_err()
                .contains("302")
        );
        assert!(
            parse_response(b"HTTP/1.1 204 No Content\r\n\r\n", 100).is_err(),
            "200 only"
        );
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\nabc", 100).is_err());
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc", 100).is_err());
        assert!(parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nff\r\nabc",
            100
        )
        .is_err());
        assert!(parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n80\r\n",
            100
        )
        .is_err());
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n", 100).is_err()
        );
        assert!(parse_response(b"SSH-2.0-OpenSSH\r\n\r\n", 100).is_err());
        assert!(parse_response(&[b'x'; 50], 100).is_err());
        let long = format!("HTTP/1.1 200 OK\r\n\r\n{}", "x".repeat(101));
        assert!(parse_response(long.as_bytes(), 100).is_err());
        // A status line meant for a terminal is shown without its escapes.
        let e = parse_response(b"HTTP/1.1 500 \x1b[2Jboom\r\n\r\n", 100).unwrap_err();
        assert!(!e.contains('\x1b'), "{e}");
    }
}
