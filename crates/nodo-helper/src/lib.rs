//! The root helper: performs the fixed action list in `nodo_api::Request` for allowed users.
//!
//! All system access goes through the [`System`] trait, so the logic here runs in tests with a
//! fake and without root. Nothing builds a shell string.

#![forbid(unsafe_code)]

mod apply;
mod catalog;
mod drive;
mod exposure;
#[cfg(test)]
mod fake;
mod health;
mod helper;
mod info;
mod install;
pub mod keys;
mod node;
mod policy;
pub mod real;
mod secrets;
pub mod server;
mod service;
mod settings;
mod ssh;
mod stores;
pub mod system;
mod volumes;
pub mod worker;

pub use catalog::Package;
pub use exposure::{ExposureApplier, ExposureState, NoCaddyYet, PublicHosts};
pub use helper::{AllowedUids, DriveConfig, Helper, Paths, Timing};
pub use nodo_api::{client, protocol};
pub use policy::NodoSettings;
pub use secrets::SecretsKey;
pub use settings::{Plan, PlannedFile, ServiceInputs, SettingsContext, SettingsPolicy, Target};
pub use stores::Bindings;
pub use system::{Probe, SvAction, SysError, System, WriteOptions};
