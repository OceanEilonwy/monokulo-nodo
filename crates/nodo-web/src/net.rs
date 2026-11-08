//! The real HTTP client, for the `nodo-ddns` binary and the root helper. The rest of the library
//! only sees [`HttpClient`], so its tests never reach the network.

use crate::http::{HttpClient, HttpError, HttpRequest, HttpResponse, Method};
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
/// Void's ca-certificates bundle. It follows system trust updates, unlike roots compiled into the binary.
const SYSTEM_ROOTS: &str = "/etc/ssl/certs/ca-certificates.crt";

/// Falls back to the bundled Mozilla roots when the system bundle is missing or unreadable.
fn root_certs() -> (ureq::tls::RootCerts, bool) {
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
        (ureq::tls::RootCerts::WebPki, false)
    } else {
        (ureq::tls::RootCerts::new_with_certs(&certs), true)
    }
}

pub struct UreqClient {
    agent: ureq::Agent,
}

impl UreqClient {
    pub fn new() -> Self {
        let (roots, system) = root_certs();
        if !system {
            eprintln!("nodo-ddns: {SYSTEM_ROOTS} is unusable, using the bundled roots");
        }
        let agent = ureq::Agent::config_builder()
            .tls_config(ureq::tls::TlsConfig::builder().root_certs(roots).build())
            .http_status_as_error(false)
            // A redirect could carry a credential header to another host, and no API here needs one.
            .max_redirects(0)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .into();
        Self { agent }
    }
}

impl HttpClient for UreqClient {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let method = match request.method {
            Method::Get => ureq::http::Method::GET,
            Method::Post => ureq::http::Method::POST,
            Method::Put => ureq::http::Method::PUT,
            Method::Patch => ureq::http::Method::PATCH,
            Method::Delete => ureq::http::Method::DELETE,
        };
        let mut builder = ureq::http::Request::builder().method(method).uri(&request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let result = match &request.body {
            Some(body) => builder.body(body.as_slice()).map(|built| self.agent.run(built)),
            None => builder.body(()).map(|built| self.agent.run(built)),
        };
        let mut response = result.map_err(|_| HttpError::Other)?.map_err(map_error)?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned())))
            .collect();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(map_error)?;
        Ok(HttpResponse { status, headers, body })
    }
}

/// The text of a ureq error can include the request URL, so only the category is kept.
fn map_error(error: ureq::Error) -> HttpError {
    match error {
        ureq::Error::Timeout(_) => HttpError::Timeout,
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => HttpError::Connect,
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => HttpError::Tls,
        _ => HttpError::Other,
    }
}


impl Default for UreqClient {
    fn default() -> Self {
        Self::new()
    }
}
