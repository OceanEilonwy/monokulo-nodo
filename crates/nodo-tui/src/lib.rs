//! The Monokulo Nodo terminal interface.
//!
//! State and key handling (`app`, `update`) are pure, rendering (`view`) reads the state, and only
//! `runtime` touches a terminal or a backend. `nodo_api::Backend` carries what the helper API
//! covers today, and `model::ScreenData` carries the rest until that API grows.

// A dropped result is how failures went unreported. Where dropping is right, an #[allow] says why.
// CI also enforces these for the tests and examples (.github/workflows/crates.yml).
#![deny(clippy::let_underscore_must_use, unused_must_use)]

pub mod app;
pub mod clipboard;
pub mod ctl;
pub mod fixtures;
pub mod hit;
pub mod input;
pub mod proposal;
pub mod runtime;
pub mod theme;
pub mod update;
pub mod view;
