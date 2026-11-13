//! The Monokulo Nodo terminal interface.
//!
//! State and key handling (`app`, `update`) are pure, rendering (`view`) reads the state, and only
//! `runtime` touches a terminal or a backend. `nodo_api::Backend` carries what the helper API
//! covers today, and `model::ScreenData` carries the rest until that API grows.

pub mod app;
pub mod ctl;
pub mod fixtures;
pub mod helper_backend;
pub mod helper_manage;
pub mod mock;
pub mod model;
pub mod proposal;
pub mod runtime;
pub mod secret;
pub mod theme;
pub mod update;
pub mod view;
