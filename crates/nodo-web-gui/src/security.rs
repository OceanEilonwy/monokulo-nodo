//! What every request is checked for and every response carries, whatever the page.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

/// Only this origin's own files run or style a page: no inline script, no inline style, no font
/// but the Nodo's own copies, nothing from another site, and no page of another site may frame or be posted to from this one.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// Nothing about a page here is told to another site. `same-origin` and not `no-referrer`:
/// under `no-referrer` a browser posts a form with `Origin: null` and no `Referer` (Fetch,
/// "serializing a request origin"; seen with Chromium 156), and the check in [`same_origin`]
/// would then refuse the owner's own clicks. Under `same-origin` the pages of this origin get
/// both headers and every other site gets neither, so links that leave still tell nothing.
pub const REFERRER_POLICY: &str = "same-origin";

/// A form here is a passphrase, an address or a public key. Anything larger is not a form.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Adds the headers every response has. A response that already says how it may be cached (the
/// static files) keeps that; everything else is never stored.
pub async fn headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CONTENT_SECURITY_POLICY));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static(REFERRER_POLICY));
    if !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

fn refuse(status: StatusCode, text: &'static str) -> Response {
    (status, text).into_response()
}

/// The host and port of an `Origin` or `Referer`, if its scheme is the one expected.
fn authority<'a>(url: &'a str, scheme: &str) -> Option<&'a str> {
    let rest = url.strip_prefix(scheme)?.strip_prefix("://")?;
    Some(rest.split(['/', '?', '#']).next().unwrap_or(rest))
}

/// Whether a request that changes something was sent by a page of this origin: its `Origin`,
/// or without one its `Referer`, names the host the request was sent to.
pub fn same_origin(headers: &HeaderMap, scheme: &str) -> bool {
    let text = |name: HeaderName| headers.get(name).and_then(|value| value.to_str().ok());
    let Some(host) = text(header::HOST) else {
        return false;
    };
    match (text(header::ORIGIN), text(header::REFERER)) {
        (Some(origin), _) => authority(origin, scheme) == Some(host),
        (None, Some(referer)) => authority(referer, scheme) == Some(host),
        (None, None) => false,
    }
}

/// Refuses a request for the wrong host, and any request but a GET or HEAD that another site
/// could have sent. The session cookie is `SameSite=Strict` as well; this does not rely on it.
pub async fn guard(State(state): State<AppState>, request: Request<Body>, next: Next) -> Response {
    let config = state.config();
    if let Some(expected) = &config.host {
        let host = request.headers().get(header::HOST).and_then(|value| value.to_str().ok());
        if host != Some(expected.as_str()) {
            return refuse(StatusCode::MISDIRECTED_REQUEST, "This address is not the Nodo's. Open https://nodo.local instead.");
        }
    }
    let reads = matches!(*request.method(), Method::GET | Method::HEAD);
    if !reads && !same_origin(request.headers(), config.scheme()) {
        return refuse(StatusCode::FORBIDDEN, "This request did not come from the Nodo's own pages, so it was refused.");
    }
    next.run(request).await
}

/// The address a guess came from, for the wait after wrong tries. Caddy is the only client this
/// process has, and it writes the browser's address last in `X-Forwarded-For`; whatever a
/// browser put there itself comes before it. Without the header (under `--mock`) every request
/// counts as one address.
pub fn client_address(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|list| list.rsplit(',').next())
        .map(|address| address.trim().to_owned())
        .filter(|address| !address.is_empty())
        .unwrap_or_else(|| "direct".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(HeaderName::from_static(name), HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn a_post_must_come_from_the_host_it_was_sent_to() {
        let ok = headers(&[("host", "nodo.local"), ("origin", "https://nodo.local")]);
        assert!(same_origin(&ok, "https"));
        let other = headers(&[("host", "nodo.local"), ("origin", "https://evil.example")]);
        assert!(!same_origin(&other, "https"));
        let downgraded = headers(&[("host", "nodo.local"), ("origin", "http://nodo.local")]);
        assert!(!same_origin(&downgraded, "https"));
        let prefixed = headers(&[("host", "nodo.local"), ("origin", "https://nodo.local.evil.example")]);
        assert!(!same_origin(&prefixed, "https"));
        let null = headers(&[("host", "nodo.local"), ("origin", "null")]);
        assert!(!same_origin(&null, "https"));
    }

    #[test]
    fn without_an_origin_the_referer_decides_and_without_both_it_is_refused() {
        let referer = headers(&[("host", "nodo.local"), ("referer", "https://nodo.local/apps?x=1")]);
        assert!(same_origin(&referer, "https"));
        let foreign = headers(&[("host", "nodo.local"), ("referer", "https://evil.example/nodo.local")]);
        assert!(!same_origin(&foreign, "https"));
        // A foreign Origin is not rescued by a Referer.
        let both = headers(&[("host", "nodo.local"), ("origin", "https://evil.example"), ("referer", "https://nodo.local/")]);
        assert!(!same_origin(&both, "https"));
        assert!(!same_origin(&headers(&[("host", "nodo.local")]), "https"));
    }

    #[test]
    fn fonts_come_only_from_this_origin() {
        assert!(CONTENT_SECURITY_POLICY.split("; ").any(|directive| directive == "font-src 'self'"));
        assert!(CONTENT_SECURITY_POLICY.starts_with("default-src 'none';"));
    }

    #[test]
    fn the_address_is_the_one_caddy_wrote() {
        assert_eq!(client_address(&headers(&[("x-forwarded-for", "10.0.0.9, 192.168.1.20")])), "192.168.1.20");
        assert_eq!(client_address(&headers(&[])), "direct");
    }
}
