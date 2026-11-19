//! What the Monokulo Nodo interfaces share: the terminal interface (`nodo-tui`), the built-in
//! screen (`nodo-gui`) and any later one.
//!
//! Nothing here knows a terminal or a toolkit. `nodo_api::Backend` carries what the helper API
//! covers today, and `model::ScreenData` carries the rest until that API grows. `helper_backend`
//! is both against the real helper, `mock` is both in memory, and `contract` holds the scenarios
//! the two must pass alike.
//!
//! An interface asks for work as an `Effect`; `exec` performs it against the backend and answers
//! with `Outcome`s. What the screens work out from the device's state alone is in `queries`.

// A dropped result is how failures went unreported. Where dropping is right, an #[allow] says why.
// CI also enforces these for the tests (.github/workflows/crates.yml).
#![deny(clippy::let_underscore_must_use, unused_must_use)]

pub mod contract;
pub mod effect;
pub mod exec;
pub mod helper_backend;
pub mod helper_manage;
pub mod mock;
pub mod model;
pub mod queries;
pub mod refs;
pub mod screen;
pub mod secret;

pub use effect::{Effect, Outcome};
