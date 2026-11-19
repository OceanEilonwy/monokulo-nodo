//! The three files a page loads, compiled into the binary: htmx, the stylesheet, and a few
//! lines of script for the dialog's focus.
//!
//! Each is served under a name that holds a hash of its bytes, so it can be cached for a year:
//! a new build with a changed file has a new name. Nothing is loaded from the Internet, as the
//! device may be offline and the content security policy allows only this origin anyway.

use std::sync::OnceLock;

use axum::extract::Path;
use axum::http::header::{self, HeaderValue};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// htmx 4.0.2, the minified build from the npm package (`dist/htmx.min.js`), unchanged. Its
/// licence is beside it in `assets/htmx-LICENSE.txt`.
const HTMX: &[u8] = include_bytes!("../assets/htmx-4.0.2.min.js");
const CSS: &[u8] = include_bytes!("../assets/app.css");
const SCRIPT: &[u8] = include_bytes!("../assets/app.js");

/// FNV-1a over the file. It names a file for caches; it protects nothing.
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

struct Names {
    htmx: String,
    css: String,
    script: String,
}

fn names() -> &'static Names {
    static NAMES: OnceLock<Names> = OnceLock::new();
    NAMES.get_or_init(|| Names {
        htmx: format!("htmx-4.0.2-{:016x}.js", hash(HTMX)),
        css: format!("app-{:016x}.css", hash(CSS)),
        script: format!("app-{:016x}.js", hash(SCRIPT)),
    })
}

/// The path of htmx, for the page's `<script src>`.
pub fn htmx_path() -> String {
    format!("/static/{}", names().htmx)
}

/// The path of the stylesheet, for the page's `<link href>`.
pub fn css_path() -> String {
    format!("/static/{}", names().css)
}

/// The path of this crate's own script, for the page's second `<script src>`.
pub fn script_path() -> String {
    format!("/static/{}", names().script)
}

/// `GET /static/{name}`. Open to everyone: the login page needs these files.
pub async fn serve(Path(name): Path<String>) -> Response {
    let names = names();
    let (bytes, kind) = if name == names.htmx {
        (HTMX, "text/javascript; charset=utf-8")
    } else if name == names.css {
        (CSS, "text/css; charset=utf-8")
    } else if name == names.script {
        (SCRIPT, "text/javascript; charset=utf-8")
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(kind)),
            (header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable")),
        ],
        bytes,
    )
        .into_response()
}
