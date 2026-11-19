//! Logging in and out. The device passphrase is the only credential; the helper checks it.

use axum::extract::State;
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Form;
use maud::html;

use super::PassForm;
use crate::ctx::see_other;
use crate::dest;
use crate::layout::{self, Chrome, Page};
use crate::security::client_address;
use crate::state::AppState;

fn page(problem: Option<&str>, status: StatusCode) -> Response {
    let body = html! {
        div.gate {
            section.card {
                span.brand { span.mark {} "Nodo" }
                h1 { "Log in to your Nodo" }
                form.stack method="post" action=(dest::LOGIN) {
                    (layout::passphrase("passphrase", "Device passphrase", problem, true))
                    button.btn.primary type="submit" { "Log in" }
                }
                p.dim.small { "It is the passphrase that unlocks this Nodo after a restart. Logging in also unlocks it." }
                p.dim.small { "A lost passphrase can't be recovered; a backup brings the data back." }
            }
        }
    };
    let page = Page::plain("Log in", body).status(status);
    (status, layout::document(&page, &Chrome::signed_out())).into_response()
}

/// `GET /login`.
pub async fn show(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = state.config().cookie.read(&headers) {
        if state.authenticate(&token, false).await.is_some() {
            return see_other(dest::HOME);
        }
    }
    page(None, StatusCode::OK)
}

/// `POST /login`. The passphrase goes to the helper and nowhere else; a refusal is shown beside
/// the box with the status that fits it.
pub async fn submit(State(state): State<AppState>, headers: HeaderMap, Form(form): Form<PassForm>) -> Response {
    let address = client_address(&headers);
    match state.login(&address, form.passphrase.expose()).await {
        Ok(session) => {
            let mut response = see_other(dest::HOME);
            if let Ok(cookie) = HeaderValue::from_str(&state.config().cookie.set(session.token())) {
                response.headers_mut().insert(header::SET_COOKIE, cookie);
            }
            response
        }
        Err(refusal) => page(Some(&refusal.message), refusal.status),
    }
}

/// `POST /logout`. Ends this browser's session; the others stay. The helper is told only when
/// it was the last one (`AppState::logout`).
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let cookie = state.config().cookie;
    if let Some(token) = cookie.read(&headers) {
        if let Some(session) = state.authenticate(&token, false).await {
            state.logout(&session).await;
        }
    }
    let mut response = see_other(dest::LOGIN);
    if let Ok(clear) = HeaderValue::from_str(&cookie.clear()) {
        response.headers_mut().insert(header::SET_COOKIE, clear);
    }
    response
}
