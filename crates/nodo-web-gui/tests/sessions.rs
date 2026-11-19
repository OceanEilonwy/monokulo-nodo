//! Logging in and out, how long a session lasts, and what every request and answer is held to.

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::{form, App, Hold, HOST, ORIGIN};
use nodo_api::ApiError;
use nodo_ui_core::mock::MOCK_PASSPHRASE;
use nodo_web_gui::security::{CONTENT_SECURITY_POLICY, MAX_BODY_BYTES};
use nodo_web_gui::session::MAX_SESSIONS;
use nodo_web_gui::Config;
use tower::ServiceExt;

const MINUTE: Duration = Duration::from_secs(60);

#[tokio::test]
async fn the_login_page_is_open_and_every_other_page_asks_for_a_login() {
    let app = App::new();
    let mut visitor = app.visitor();
    let page = visitor.get("/login").await;
    assert_eq!(page.status, StatusCode::OK);
    page.assert_has("Device passphrase").assert_has(r#"autocomplete="current-password""#).assert_has(r#"<form class="stack" method="post" action="/login">"#);
    // Nothing about the device is on it, and no tabs.
    page.assert_lacks("Log out").assert_lacks("Apps");

    for path in ["/", "/apps", "/apps/tor", "/sources", "/system", "/system/keys", "/working", "/confirm"] {
        let answer = visitor.get(path).await;
        assert_eq!((answer.status, answer.location()), (StatusCode::SEE_OTHER, "/login"), "{path}");
    }
    // htmx is told with its own header, or it would swap the login page into a fragment's place.
    let mut htmx = app.visitor();
    htmx.htmx = true;
    let answer = htmx.get("/apps").await;
    assert_eq!((answer.status, answer.header("hx-redirect")), (StatusCode::OK, "/login"));
    assert!(answer.body.is_empty());
    assert_eq!(app.backends.calls(), Vec::<&str>::new(), "nothing reached the helper");
}

#[tokio::test]
async fn the_right_passphrase_starts_a_session_with_a_host_bound_cookie() {
    let app = App::new();
    let mut browser = app.visitor();
    let answer = browser.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await;
    assert_eq!((answer.status, answer.location()), (StatusCode::SEE_OTHER, "/"));
    let cookie = answer.header("set-cookie");
    let (pair, attributes) = cookie.split_once("; ").expect("attributes");
    let (name, token) = pair.split_once('=').expect("a value");
    assert_eq!(name, "__Host-nodo");
    assert_eq!(token.len(), 64, "256 bits as hex");
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(attributes, "Path=/; Secure; HttpOnly; SameSite=Strict");

    let home = browser.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    home.assert_has("Log out");
    // Someone logged in is not asked again.
    assert_eq!(browser.get("/login").await.location(), "/");

    // Two logins never share a token.
    let other = app.login().await;
    assert_ne!(other.token(), browser.token());
}

#[tokio::test]
async fn the_mock_serves_plain_http_so_its_cookie_is_not_secure_and_has_another_name() {
    let app = App::with(Config::mock());
    let request = Request::builder()
        .method(Method::POST)
        .uri("/login")
        .header(header::HOST, "127.0.0.1:8080")
        .header(header::ORIGIN, "http://127.0.0.1:8080")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form(&[("passphrase", MOCK_PASSPHRASE)])))
        .expect("a request");
    let answer = app.send(request).await;
    assert_eq!(answer.status, StatusCode::SEE_OTHER);
    let cookie = answer.header("set-cookie");
    assert!(cookie.starts_with("nodo-dev="), "{cookie}");
    assert!(cookie.ends_with("; Path=/; HttpOnly; SameSite=Strict"), "{cookie}");
}

#[tokio::test]
async fn a_refused_login_says_why_beside_the_box_and_sets_no_cookie() {
    let app = App::new();
    let mut browser = app.visitor();

    let empty = browser.post("/login", &[("passphrase", "")]).await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
    empty.assert_has("Enter the device passphrase.");

    *app.backends.login_error.lock().expect("the error") = Some(ApiError::NotSetUp);
    let not_set_up = browser.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await;
    assert_eq!(not_set_up.status, StatusCode::SERVICE_UNAVAILABLE);
    not_set_up.assert_has("This Nodo has no device passphrase yet.");

    *app.backends.login_error.lock().expect("the error") = Some(ApiError::Failed("the web GUI is turned off".into()));
    let off = browser.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await;
    assert_eq!(off.status, StatusCode::SERVICE_UNAVAILABLE);
    off.assert_has("The web GUI is turned off.").assert_has("nodo-tui --ctl web-gui 1");

    *app.backends.login_error.lock().expect("the error") = Some(ApiError::RateLimited { retry_after_secs: 30 });
    let limited = browser.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    limited.assert_has("Try again in 30 seconds.");

    *app.backends.login_error.lock().expect("the error") = None;
    let wrong = browser.post("/login", &[("passphrase", "not-the-passphrase")]).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    wrong.assert_has("That passphrase did not unlock this device. Try again.").assert_has(r#"aria-invalid="true""#);
    // What was typed is never sent back.
    wrong.assert_lacks("not-the-passphrase");

    for answer in [&empty, &not_set_up, &off, &limited, &wrong] {
        assert_eq!(answer.header("set-cookie"), "");
    }
    assert!(browser.cookie.is_none());
    assert_eq!(app.state.session_count(), 0);
}

#[tokio::test]
async fn a_wrong_guess_makes_that_address_wait_and_no_other() {
    let app = App::new();
    let mut guesser = app.visitor();
    guesser.address = Some("192.168.1.66");
    assert_eq!(guesser.post("/login", &[("passphrase", "wrong")]).await.status, StatusCode::UNAUTHORIZED);

    // Even the right passphrase is not forwarded until the wait is over.
    let early = guesser.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await;
    assert_eq!(early.status, StatusCode::TOO_MANY_REQUESTS);
    early.assert_has("Please wait 2 seconds before trying again.");
    assert_eq!(app.backends.calls(), Vec::<&str>::new());

    // The owner, at another address, is not held up by it.
    let mut owner = app.visitor();
    owner.address = Some("192.168.1.20");
    assert_eq!(owner.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await.status, StatusCode::SEE_OTHER);

    // A second wrong guess doubles the wait, as the core's unlock rule says.
    app.clock.advance(Duration::from_secs(2));
    assert_eq!(guesser.post("/login", &[("passphrase", "wrong again")]).await.status, StatusCode::UNAUTHORIZED);
    app.clock.advance(Duration::from_secs(2));
    guesser.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await.assert_has("Please wait 2 seconds before trying again.");
    app.clock.advance(Duration::from_secs(2));
    assert_eq!(guesser.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn only_one_login_is_in_flight_and_the_others_are_told_to_try_again() {
    let app = App::new();
    let hold = Arc::new(Hold::default());
    *app.backends.login_hold.lock().expect("the hold") = Some(hold.clone());

    let request = |passphrase: &str| {
        Request::builder()
            .method(Method::POST)
            .uri("/login")
            .header(header::HOST, HOST)
            .header(header::ORIGIN, ORIGIN)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form(&[("passphrase", passphrase)])))
            .expect("a request")
    };
    // The first login is with the helper, which makes it wait.
    let first = tokio::spawn(app.router.clone().oneshot(request(MOCK_PASSPHRASE)));
    while !hold.reached.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    // A second one is not queued behind it, right passphrase or not.
    let second = app.send(request(MOCK_PASSPHRASE)).await;
    assert_eq!(second.status, StatusCode::TOO_MANY_REQUESTS);
    second.assert_has("Another login is being checked. Try again shortly.");
    assert_eq!(second.header("set-cookie"), "");

    hold.release();
    let first = first.await.expect("the task").expect("the router answers");
    assert_eq!(first.status(), StatusCode::SEE_OTHER);
    assert_eq!(app.backends.calls(), ["login"]);

    // With the first one over, the next goes through.
    assert_eq!(app.send(request(MOCK_PASSPHRASE)).await.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn a_session_ends_after_15_idle_minutes_and_polling_does_not_keep_it_alive() {
    let app = App::new();
    let mut browser = app.login().await;

    app.clock.advance(14 * MINUTE);
    assert_eq!(browser.get("/apps").await.status, StatusCode::OK, "a request within the time keeps it");
    app.clock.advance(14 * MINUTE);
    assert_eq!(browser.get("/").await.status, StatusCode::OK);

    // An open tab asks by itself: the job's page, a followed log. Neither is the owner.
    app.clock.advance(10 * MINUTE);
    assert_eq!(browser.get("/working").await.location(), "/", "still logged in, and nothing is running");
    let mut htmx = browser.clone();
    htmx.htmx = true;
    assert_eq!(htmx.get("/system/logs/bell/lines?follow=1").await.status, StatusCode::OK);
    app.clock.advance(6 * MINUTE);

    // 16 minutes since the owner's last own request.
    let answer = browser.get("/").await;
    assert_eq!((answer.status, answer.location()), (StatusCode::SEE_OTHER, "/login"));
    assert_eq!(app.state.session_count(), 0);
    assert_eq!(app.backends.calls(), ["login", "logout"], "the last session ended, so the helper's gate is closed");
}

#[tokio::test]
async fn a_session_ends_12_hours_after_its_login_however_busy() {
    let app = App::new();
    let mut browser = app.login().await;
    // A request every ten minutes, for 11 hours and 50 minutes.
    for _ in 0..71 {
        app.clock.advance(10 * MINUTE);
        assert_eq!(browser.get("/apps").await.status, StatusCode::OK);
    }
    app.clock.advance(10 * MINUTE);
    assert_eq!(browser.get("/apps").await.location(), "/login");
    assert_eq!(app.backends.calls(), ["login", "logout"]);
}

#[tokio::test]
async fn a_fifth_login_is_refused_until_a_session_ends() {
    let app = App::new();
    let mut browsers = Vec::new();
    for _ in 0..MAX_SESSIONS {
        browsers.push(app.login().await);
    }
    let mut fifth = app.visitor();
    let refused = fifth.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await;
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    refused.assert_has("4 browsers are logged in already.");
    assert!(fifth.cookie.is_none());
    assert_eq!(app.backends.calls().len(), MAX_SESSIONS, "the fifth passphrase was not sent to the helper");

    // The four are unharmed.
    assert_eq!(browsers[0].get("/").await.status, StatusCode::OK);
    assert_eq!(browsers[3].post("/logout", &[]).await.status, StatusCode::SEE_OTHER);
    assert_eq!(fifth.post("/login", &[("passphrase", MOCK_PASSPHRASE)]).await.status, StatusCode::SEE_OTHER);
    assert_eq!(app.state.session_count(), MAX_SESSIONS);
}

#[tokio::test]
async fn logging_out_ends_one_session_and_the_helper_is_told_only_after_the_last() {
    let app = App::new();
    let mut first = app.login().await;
    let mut second = app.login().await;
    let old_cookie = first.cookie.clone();

    let answer = first.post("/logout", &[]).await;
    assert_eq!((answer.status, answer.location()), (StatusCode::SEE_OTHER, "/login"));
    assert!(answer.header("set-cookie").starts_with("__Host-nodo=; Max-Age=0;"), "{}", answer.header("set-cookie"));
    assert_eq!(app.backends.calls(), ["login", "login"], "another browser is still logged in");

    // The cookie that was logged out is worth nothing, and the other session goes on.
    first.cookie = old_cookie;
    assert_eq!(first.get("/").await.location(), "/login");
    assert_eq!(second.get("/").await.status, StatusCode::OK);

    assert_eq!(second.post("/logout", &[]).await.status, StatusCode::SEE_OTHER);
    assert_eq!(app.backends.calls(), ["login", "login", "logout"]);
    assert_eq!(app.state.session_count(), 0);

    // Logging out twice, or without a session, tells the helper nothing more.
    assert_eq!(second.post("/logout", &[]).await.status, StatusCode::SEE_OTHER);
    assert_eq!(app.backends.calls().len(), 3);
}

#[tokio::test]
async fn a_post_must_come_from_the_nodos_own_pages() {
    let app = App::new();
    let browser = app.login().await;
    let cookie = browser.cookie.clone().expect("logged in");
    let post = |origin: Option<&str>, referer: Option<&str>, host: &str| {
        let mut request = Request::builder().method(Method::POST).uri("/apps/tor/restart").header(header::HOST, host).header(header::COOKIE, &cookie);
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        if let Some(referer) = referer {
            request = request.header(header::REFERER, referer);
        }
        request.body(Body::empty()).expect("a request")
    };

    for (origin, referer) in [
        (Some("https://evil.example"), None),
        (Some("null"), None),
        (Some("http://nodo.local"), None),
        (Some("https://nodo.local.evil.example"), None),
        (None, None),
        (None, Some("https://evil.example/https://nodo.local/")),
        (Some("https://evil.example"), Some("https://nodo.local/apps")),
    ] {
        let answer = app.send(post(origin, referer, HOST)).await;
        assert_eq!(answer.status, StatusCode::FORBIDDEN, "{origin:?} {referer:?}");
        assert_eq!(answer.header("content-security-policy"), CONTENT_SECURITY_POLICY, "a refusal carries the headers too");
    }
    assert_eq!(app.send(post(Some(ORIGIN), None, HOST)).await.status, StatusCode::SEE_OTHER);
    assert_eq!(app.send(post(None, Some("https://nodo.local/apps/tor"), HOST)).await.status, StatusCode::SEE_OTHER);

    // A request that names another host is not this Nodo's, whatever its origin says.
    let misdirected = app.send(post(Some("https://evil.example"), None, "evil.example")).await;
    assert_eq!(misdirected.status, StatusCode::MISDIRECTED_REQUEST);
    let get = Request::builder().uri("/login").header(header::HOST, "192.168.1.10").body(Body::empty()).expect("a request");
    assert_eq!(app.send(get).await.status, StatusCode::MISDIRECTED_REQUEST);
}

/// The `src` and `href` of what a page loads by itself: scripts, stylesheets, icons, images.
fn loaded_by_the_page(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    for tag in body.split('<').filter(|tag| ["script", "link", "img", "iframe", "source"].iter().any(|name| tag.starts_with(name))) {
        for attribute in ["src=\"", "href=\""] {
            if let Some(start) = tag.find(attribute) {
                found.push(tag[start + attribute.len()..].split('"').next().unwrap_or_default().to_owned());
            }
        }
    }
    found
}

#[tokio::test]
async fn every_answer_carries_the_security_headers_and_pages_load_only_their_own_files() {
    let app = App::new();
    let mut visitor = app.visitor();
    let mut browser = app.login().await;
    let pages = [
        "/", "/apps", "/apps/tor", "/apps/nothing-of-that-name", "/apps/get", "/apps/get?q=pay", "/apps/get/official/moneropay",
        "/apps/get/official/moneropay/review", "/apps/get/official/moneropay/doc/versions", "/sources", "/sources/add", "/sources/add/confirm",
        "/sources/of/old-tools", "/sources/of/old-tools/key", "/web", "/system", "/system/about", "/system/logs", "/system/logs/bell",
        "/system/logs/bell/lines", "/system/keys", "/system/drive", "/working", "/confirm", "/document", "/login", "/no-such-page",
    ];
    let mut assets = Vec::new();
    for path in pages {
        for answer in [visitor.get(path).await, browser.get(path).await] {
            assert_eq!(answer.header("content-security-policy"), CONTENT_SECURITY_POLICY, "{path}");
            assert_eq!(answer.header("x-content-type-options"), "nosniff", "{path}");
            assert_eq!(answer.header("referrer-policy"), "same-origin", "{path}");
            assert_eq!(answer.header("cache-control"), "no-store", "{path}");
            // The policy allows no inline script or style, so a page must not need any.
            for inline in ["style=\"", "<style", "onclick=", "onload=", "javascript:"] {
                answer.assert_lacks(inline);
            }
            for url in loaded_by_the_page(&answer.body) {
                assert!(url.starts_with("/static/") || url == "data:,", "{path} loads {url}");
                assert!(!answer.body.contains("<script>"), "{path} has an inline script");
                if url.starts_with("/static/") && !assets.contains(&url) {
                    assets.push(url);
                }
            }
        }
    }
    // A post's answer, and a refused one.
    for answer in [browser.post("/apps/tor/restart", &[]).await, visitor.post("/apps/tor/restart", &[]).await] {
        assert_eq!(answer.header("content-security-policy"), CONTENT_SECURITY_POLICY);
        assert_eq!(answer.header("cache-control"), "no-store");
    }

    // The two files a page loads are the only answers that may be kept, and for long: their
    // names change with their contents.
    assets.sort();
    assert_eq!(assets.len(), 2, "{assets:?}");
    assert!(assets[0].starts_with("/static/app-") && assets[0].ends_with(".css"));
    assert!(assets[1].starts_with("/static/htmx-4.0.2-") && assets[1].ends_with(".js"));
    for (asset, kind) in assets.iter().zip(["text/css; charset=utf-8", "text/javascript; charset=utf-8"]) {
        let answer = visitor.get(asset).await;
        assert_eq!(answer.status, StatusCode::OK, "served without a login: the login page needs it");
        assert_eq!(answer.header("cache-control"), "public, max-age=31536000, immutable");
        assert_eq!(answer.header("content-type"), kind);
        assert_eq!(answer.header("content-security-policy"), CONTENT_SECURITY_POLICY);
        assert_eq!(answer.header("x-content-type-options"), "nosniff");
    }
    assert_eq!(visitor.get("/static/app-0000000000000000.css").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_helper_that_closed_its_gate_sends_every_browser_back_to_the_login() {
    let app = App::new();
    let mut first = app.login().await;
    let mut second = app.login().await;
    assert_eq!(first.get("/apps").await.status, StatusCode::OK);

    // The helper restarted, or its own time ran out: it answers that nobody is logged in.
    app.backends.gate_open.store(false, Ordering::SeqCst);
    let answer = first.get("/apps").await;
    assert_eq!((answer.status, answer.location()), (StatusCode::SEE_OTHER, "/login"));
    assert_eq!(app.state.session_count(), 0, "every session is dropped, not only the one that noticed");
    assert_eq!(second.get("/").await.location(), "/login");
    assert_eq!(app.backends.calls(), ["login", "login"], "the gate is closed already; no logout is sent");

    // A post is not carried out either.
    let mut third = app.login().await;
    app.backends.gate_open.store(false, Ordering::SeqCst);
    let mut htmx = third.clone();
    htmx.htmx = true;
    assert_eq!(htmx.post("/apps/tor/restart", &[]).await.header("hx-redirect"), "/login");
    assert_eq!(third.get("/").await.location(), "/login");
}

#[tokio::test]
async fn a_form_larger_than_any_form_here_is_refused() {
    let app = App::new();
    let mut visitor = app.visitor();
    let huge = "x".repeat(MAX_BODY_BYTES + 1);
    let answer = visitor.post("/login", &[("passphrase", &huge)]).await;
    assert_eq!(answer.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(app.backends.calls(), Vec::<&str>::new());
}
