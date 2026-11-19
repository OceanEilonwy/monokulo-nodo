//! The files a page loads, compiled into the binary: htmx, the stylesheet, a few lines of
//! script for the dialog's focus, the Nodo's logo, and the two typefaces with the stylesheet
//! that names them.
//!
//! Each is served under a name that holds a hash of its bytes, so it can be cached for a year:
//! a new build with a changed file has a new name. Nothing is loaded from the Internet, as the
//! device may be offline and the content security policy allows only this origin anyway; the
//! fonts too are the Nodo's own copies (`assets/fonts/README.md` says where they came from).
//!
//! A stylesheet cannot know a hashed name, so the `@font-face` rules are not in `app.css`: they
//! are a second, small stylesheet made once from the fonts' served names, itself served under a
//! name with its hash.

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
/// The Nodo's logo, a copy of the built-in screen's (`crates/nodo-gui/ui/logo.svg`).
const LOGO: &[u8] = include_bytes!("../assets/logo.svg");
/// Inter, the variable build (weights 100 to 900) of the Latin subset, from Fontsource. Open Font
/// License, beside it in `assets/fonts/Inter-LICENSE.txt`.
const INTER: &[u8] = include_bytes!("../assets/fonts/inter-latin-wght-normal.woff2");
/// JetBrains Mono, the variable build (weights 100 to 800) of the Latin subset, from Fontsource.
/// Open Font License, beside it in `assets/fonts/JetBrainsMono-LICENSE.txt`.
const MONO: &[u8] = include_bytes!("../assets/fonts/jetbrains-mono-latin-wght-normal.woff2");

/// FNV-1a over the file. It names a file for caches; it protects nothing.
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

struct Names {
    htmx: String,
    css: String,
    script: String,
    logo: String,
    inter: String,
    mono: String,
    fonts: String,
}

/// The `@font-face` rules for the two typefaces, pointing at their served names. `app.css`
/// names the families first in its stacks, with the system's fonts after them.
fn font_faces(inter: &str, mono: &str) -> String {
    let face = |family: &str, weights: &str, file: &str| {
        format!(
            "@font-face {{\n  font-family: \"{family}\";\n  font-style: normal;\n  font-weight: {weights};\n  font-display: swap;\n  src: url(\"/static/{file}\") format(\"woff2\");\n}}\n"
        )
    };
    face("Inter", "100 900", inter) + &face("JetBrains Mono", "100 800", mono)
}

/// The font stylesheet's text, made once.
fn fonts_css() -> &'static str {
    static CSS: OnceLock<String> = OnceLock::new();
    CSS.get_or_init(|| {
        let inter = format!("inter-{:016x}.woff2", hash(INTER));
        let mono = format!("jetbrains-mono-{:016x}.woff2", hash(MONO));
        font_faces(&inter, &mono)
    })
}

fn names() -> &'static Names {
    static NAMES: OnceLock<Names> = OnceLock::new();
    NAMES.get_or_init(|| Names {
        htmx: format!("htmx-4.0.2-{:016x}.js", hash(HTMX)),
        css: format!("app-{:016x}.css", hash(CSS)),
        script: format!("app-{:016x}.js", hash(SCRIPT)),
        logo: format!("logo-{:016x}.svg", hash(LOGO)),
        inter: format!("inter-{:016x}.woff2", hash(INTER)),
        mono: format!("jetbrains-mono-{:016x}.woff2", hash(MONO)),
        fonts: format!("fonts-{:016x}.css", hash(fonts_css().as_bytes())),
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

/// The path of the logo, for the brand in the header and the browser's tab.
pub fn logo_path() -> String {
    format!("/static/{}", names().logo)
}

/// The path of the font stylesheet, for the page's first `<link href>`.
pub fn fonts_css_path() -> String {
    format!("/static/{}", names().fonts)
}

/// The paths of the two fonts, Inter then JetBrains Mono, as the font stylesheet names them.
pub fn font_paths() -> [String; 2] {
    let names = names();
    [format!("/static/{}", names.inter), format!("/static/{}", names.mono)]
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
    } else if name == names.logo {
        (LOGO, "image/svg+xml")
    } else if name == names.fonts {
        (fonts_css().as_bytes(), "text/css; charset=utf-8")
    } else if name == names.inter {
        (INTER, "font/woff2")
    } else if name == names.mono {
        (MONO, "font/woff2")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_font_stylesheet_names_the_fonts_by_their_served_paths() {
        let css = fonts_css();
        for path in font_paths() {
            assert!(css.contains(&format!("url(\"{path}\")")), "{css}");
        }
        assert!(css.contains("font-family: \"Inter\";") && css.contains("font-family: \"JetBrains Mono\";"));
        assert_eq!(css.matches("font-display: swap;").count(), 2);
    }

    #[test]
    fn the_fonts_are_woff2_files() {
        for font in [INTER, MONO] {
            assert_eq!(font.get(..4), Some(&b"wOF2"[..]));
        }
    }
}
