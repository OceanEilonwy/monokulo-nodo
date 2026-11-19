//! Every URL, in one place.

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};

use crate::ctx::Polling;
use crate::pages::{apps, backups, bundle, changes, drive, flow, home, hosting, install, keys, login, logs, network, owner, settings, sources, system, updates, web};
use crate::security;
use crate::state::AppState;
use crate::{assets, dest};

async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "There is no such page on this Nodo.").into_response()
}

/// The whole application. Tests drive it in-process; `main` serves it on the Unix socket.
pub fn router(state: AppState) -> Router {
    // What a page asks for by itself, again and again. These do not keep a session alive.
    let polling = Router::new()
        .route(dest::WORKING, get(flow::working))
        .route("/system/logs/{source}/lines", get(logs::poll))
        .route("/apps/{name}/hosting/certificate/status", get(hosting::certificate_status_poll))
        .route("/apps/get/{feed}/{package}/installing/status", get(install::installing_poll))
        .route("/system/network/status", get(network::poll))
        .layer(Extension(Polling));

    Router::new()
        .route("/static/{name}", get(assets::serve))
        .route(dest::LOGIN, get(login::show).post(login::submit))
        .route(dest::LOGOUT, post(login::logout))
        // Home
        .route(dest::HOME, get(home::show))
        .route(dest::UNLOCK, get(home::unlock_page).post(home::unlock))
        .route(dest::CHANGES, get(changes::show).post(changes::act))
        // Apps
        .route(dest::APPS, get(apps::list).post(apps::act_on_ticked))
        .route("/apps/{name}", get(apps::show))
        .route("/apps/{name}/restart", post(apps::restart))
        .route("/apps/{name}/toggle", post(apps::toggle))
        .route("/apps/{name}/update", post(apps::update))
        .route("/apps/{name}/remove", post(apps::remove))
        .route("/apps/{name}/settings", get(settings::show).post(settings::act))
        .route("/apps/{name}/settings/field/{key}", post(settings::change))
        .route("/apps/{name}/settings/reference", get(settings::reference))
        .route("/apps/{name}/settings/reference/choose", post(settings::choose))
        .route("/apps/{name}/settings/reference/grant", post(settings::grant))
        .route("/apps/{name}/settings/reference/decline", post(settings::decline))
        .route("/apps/{name}/settings/failed", get(settings::failed))
        .route("/apps/{name}/settings/failed/keep", post(settings::keep_as_it_is))
        .route("/apps/{name}/owner", get(owner::show).post(owner::unlock))
        .route("/apps/{name}/owner/{what}", post(owner::act))
        .route("/apps/{name}/hosting", get(hosting::show).post(hosting::act))
        .route("/apps/{name}/hosting/check", get(hosting::check))
        .route("/apps/{name}/hosting/check/{what}", get(hosting::check_act).post(hosting::check_act))
        .route("/apps/{name}/hosting/cgnat", get(hosting::cgnat))
        .route("/apps/{name}/hosting/cgnat/{what}", post(hosting::cgnat_act))
        .route("/apps/{name}/hosting/confirm", get(hosting::confirm).post(hosting::go_public))
        .route("/apps/{name}/hosting/certificate", get(hosting::certificate))
        // Get apps and install. `get` is taken here, so an app of that name has no page of
        // its own; no source offers one.
        .route(dest::GET_APPS, get(install::search))
        .route(dest::BUNDLE, get(bundle::show).post(bundle::act))
        .route("/apps/get/{feed}/{package}", get(install::detail))
        .route("/apps/get/{feed}/{package}/install", post(install::gate))
        .route("/apps/get/{feed}/{package}/review", get(install::review).post(install::approve))
        .route("/apps/get/{feed}/{package}/installing", get(install::installing))
        .route("/apps/get/{feed}/{package}/installing/{what}", post(install::after_install))
        .route("/apps/get/{feed}/{package}/rejected", get(install::rejected))
        .route("/apps/get/{feed}/{package}/rejected/report", post(install::report_rejection))
        .route("/apps/get/{feed}/{package}/capability", get(install::capability))
        .route("/apps/get/{feed}/{package}/capability/{what}", post(install::capability_act))
        .route("/apps/get/{feed}/{package}/doc/{kind}", get(install::document))
        // Sources
        .route(dest::SOURCES, get(sources::list))
        .route("/sources/refresh", post(sources::refresh))
        .route(dest::ADD_SOURCE, get(sources::add).post(sources::probe))
        .route(dest::CONFIRM_SOURCE, get(sources::confirm).post(sources::trust))
        .route("/sources/add/confirm/packages", get(sources::probed_packages))
        .route("/sources/of/{name}", get(sources::open))
        .route("/sources/of/{name}/toggle", post(sources::toggle))
        .route("/sources/of/{name}/remove", post(sources::remove))
        .route("/sources/of/{name}/key", get(sources::key).post(sources::trust_key))
        .route("/sources/of/{name}/key/remove", post(sources::remove_from_key))
        // Web
        .route(dest::WEB, get(web::list))
        .route("/web/offline", post(web::all_offline))
        .route(dest::PROVIDERS, get(web::providers_show).post(web::providers_act))
        .route(dest::DOMAIN, get(web::domain_show).post(web::domain_act))
        .route(dest::CERTIFICATES, get(web::certificates))
        .route(dest::TRUST, get(web::trust))
        .route(dest::ONION, get(web::onion))
        .route("/system/onion/{what}", post(web::onion_act))
        // System
        .route(dest::SYSTEM, get(system::show))
        .route(dest::ABOUT, get(system::about))
        .route(dest::UPDATES, get(updates::show).post(updates::act))
        .route("/system/updates/changes/{name}", get(updates::changes))
        .route(dest::UPDATE_REVIEW, get(updates::review).post(updates::approve))
        .route(dest::UPDATE_PASSPHRASE, get(updates::passphrase).post(updates::submit_passphrase))
        .route(dest::UPDATE_RESULTS, get(updates::results))
        .route("/system/updates/results/{what}", post(updates::after_results))
        .route(dest::AUTO_UPDATE, get(updates::auto_update).post(updates::save_auto_update))
        .route("/system/auto-update/history", get(updates::auto_update_history))
        .route(dest::UNDO, get(updates::undo))
        .route("/system/undo/{package}", post(updates::roll_back))
        .route(dest::BACKUPS, get(backups::show).post(backups::act))
        .route(dest::NETWORK, get(network::show).post(network::act))
        .route(dest::LOGS, get(logs::device))
        .route("/system/logs/{source}", get(logs::show))
        .route("/system/logs/{source}/save", post(logs::save))
        .route(dest::KEYS, get(keys::show))
        .route("/system/keys/add", post(keys::add))
        .route("/system/keys/remove", post(keys::remove))
        .route(dest::DRIVE, get(drive::show))
        .route("/system/drive/{action}", post(drive::act))
        // What every feature shares
        .route(dest::CONFIRM, get(flow::question).post(flow::confirm))
        .route(dest::DOCUMENT, get(flow::document))
        .merge(polling)
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(security::MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), security::guard))
        // Outermost, so a refusal by the guard carries the headers too.
        .layer(middleware::from_fn(security::headers))
        .with_state(state)
}
