//! The root helper: performs the fixed action list in `nodo_api::Request` for allowed users.
//!
//! All system access goes through the [`System`] trait, so the logic here runs in tests with a
//! fake and without root. Nothing builds a shell string.

#![forbid(unsafe_code)]

mod catalog;
pub mod client;
mod exposure;
#[cfg(test)]
mod fake;
mod health;
mod helper;
mod install;
pub mod keys;
pub mod protocol;
pub mod real;
pub mod server;
mod service;
mod settings;
pub mod system;
mod volumes;

pub use exposure::{ExposureApplier, ExposureState, NoCaddyYet};
pub use helper::{AllowedUids, Helper, Paths, Timing};
pub use settings::{FlatTomlSettings, SettingsPolicy};
pub use system::{Probe, SvAction, SysError, System, WriteOptions};
