//! The root helper: performs the fixed action list in `nodo_api::Request` for allowed users.
//!
//! All system access goes through the [`System`] trait, so the logic here runs in tests with a
//! fake and without root. Nothing builds a shell string.

#![forbid(unsafe_code)]

mod apps;
mod backup;
mod apply;
mod catalog;
mod drive;
mod exposure;
#[cfg(test)]
mod fake;
mod feeds;
mod health;
mod healthcfg;
mod helper;
mod info;
mod install;
pub mod keys;
pub mod manage;
pub mod pkgsys;
mod node;
mod onion;
mod policy;
pub mod real;
pub mod secrets;
pub mod server;
mod service;
mod settings;
mod ssh;
mod stores;
pub mod sysupdate;
pub mod system;
mod updates;
pub mod usb;
mod volumes;
pub mod web;
pub mod worker;

pub use catalog::Package;
pub use exposure::{ExposureApplier, ExposureState, NoCaddyYet, PublicHosts, WebInput, WebSite};
pub use helper::{AllowedUids, DriveConfig, Helper, Paths, Timing};
pub use nodo_api::{client, protocol};
pub use policy::NodoSettings;
pub use secrets::SecretsKey;
pub use settings::{Plan, PlannedFile, ServiceInputs, SettingsContext, SettingsPolicy, Target};
pub use stores::Bindings;
pub use system::{Probe, SvAction, SysError, System, WriteOptions};
