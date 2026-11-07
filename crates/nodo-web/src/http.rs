//! The injected HTTP interface. Providers and IP detection only ever see this trait.

use std::fmt;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

#[derive(Clone)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

// Headers and bodies carry credentials, and a query string could, so Debug stops at the host.
impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let after_scheme = self.url.split_once("://").map_or(self.url.as_str(), |(_, rest)| rest);
        let host = after_scheme.split(['/', '?']).next().unwrap_or_default();
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("host", &host)
            .finish_non_exhaustive()
    }
}

impl HttpRequest {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self { method, url: url.into(), headers: Vec::new(), body: None }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    pub fn json(self, body: &serde_json::Value) -> Self {
        let mut request = self.header("Content-Type", "application/json");
        request.body = Some(body.to_string().into_bytes());
        request
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Carries a category only. Client libraries put the full URL in their errors, and a URL may hold a
/// credential, so implementations must not pass their error text through.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum HttpError {
    #[error("the request timed out")]
    Timeout,
    #[error("could not connect")]
    Connect,
    #[error("the TLS connection failed")]
    Tls,
    #[error("the request failed")]
    Other,
}

pub trait HttpClient: Send + Sync {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError>;
}

pub type SharedHttp = Arc<dyn HttpClient>;

/// Percent-encodes everything outside the RFC 3986 unreserved set.
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod fake {
    //! A scripted client. It answers from a queue and records every request, so tests can assert on
    //! exactly what would have been sent. Running out of responses fails the test.

    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeHttp {
        responses: Mutex<VecDeque<Result<HttpResponse, HttpError>>>,
        requests: Mutex<Vec<HttpRequest>>,
    }

    impl FakeHttp {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        pub fn respond(&self, status: u16, body: &str) -> &Self {
            let response = HttpResponse { status, headers: Vec::new(), body: body.as_bytes().to_vec() };
            self.responses.lock().unwrap().push_back(Ok(response));
            self
        }

        pub fn respond_with(&self, response: HttpResponse) -> &Self {
            self.responses.lock().unwrap().push_back(Ok(response));
            self
        }

        pub fn fail(&self, error: HttpError) -> &Self {
            self.responses.lock().unwrap().push_back(Err(error));
            self
        }

        pub fn requests(&self) -> Vec<HttpRequest> {
            self.requests.lock().unwrap().clone()
        }

        pub fn request(&self, index: usize) -> HttpRequest {
            self.requests()[index].clone()
        }

        pub fn assert_drained(&self) {
            assert!(self.responses.lock().unwrap().is_empty(), "scripted responses were not all used");
        }
    }

    impl HttpClient for FakeHttp {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
            self.requests.lock().unwrap().push(request.clone());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected request {} {}", request.method.as_str(), request.url))
        }
    }

    pub fn body_json(request: &HttpRequest) -> serde_json::Value {
        serde_json::from_slice(request.body.as_deref().expect("request has a body")).unwrap()
    }

    pub fn header<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
        request.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_query_headers_and_body() {
        let request = HttpRequest::new(Method::Post, "https://api.test/path?ApiKey=hunter2")
            .header("Authorization", "Bearer hunter2")
            .json(&serde_json::json!({"secret": "hunter2"}));
        let shown = format!("{request:?}");
        assert!(shown.contains("api.test"));
        assert!(!shown.contains("hunter2"));
    }

    #[test]
    fn encoding_matches_rfc3986_unreserved() {
        assert_eq!(encode_component("a b/c*~-_."), "a%20b%2Fc%2A~-_.");
    }
}
