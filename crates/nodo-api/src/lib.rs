//! The narrow interface between the unprivileged TUI and the root helper.
//!
//! The helper only performs the actions listed in `Request`. Nothing here accepts a shell
//! command, a file path to write, or arbitrary system configuration.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceState {
    Running,
    Stopped,
    Failed,
    /// Waiting for the device passphrase.
    Locked,
    /// Waiting for a dependency or data drive.
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub id: String,
    pub state: ServiceState,
    pub detail: String,
}

/// Exposures are independent, so a site can use any combination.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exposure {
    pub local: bool,
    pub onion: bool,
    pub public: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    ListServices,
    StartService { id: String },
    StopService { id: String },
    RestartService { id: String },
    /// Opens all encrypted storage with the single device passphrase.
    Unlock { passphrase: String },
    /// Validates, applies, health checks, and rolls back on failure.
    ApplySettings { package: String, values: Vec<(String, String)> },
    SetExposure { package: String, site: String, exposure: Exposure },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Services(Vec<ServiceStatus>),
    /// Settings were restored because the service failed its health check.
    RolledBack { reason: String },
}

#[derive(Debug, Error, Clone, PartialEq, Serialize, Deserialize)]
pub enum ApiError {
    #[error("this user may not perform that action")]
    Denied,
    #[error("that passphrase did not unlock this device")]
    BadPassphrase,
    #[error("unknown package or service: {0}")]
    NotFound(String),
    #[error("{0}")]
    Failed(String),
}

/// Implemented by the helper client and by a mock for TUI development and tests.
pub trait Backend {
    fn call(&mut self, request: Request) -> Result<Response, ApiError>;
}
