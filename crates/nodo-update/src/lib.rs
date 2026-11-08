//! System updates for monokulo-nodo.
//!
//! Void is a rolling distribution, so devices never follow it directly. A publisher copies Void's
//! repository at one moment, tests that copy, and signs a small manifest naming it. Devices accept
//! only manifests signed by keys they already trust, never move to an older one, and verify that
//! the repository index they download is the exact one that was tested.
//!
//! - [`manifest`]: the signed snapshot manifest and key rotation statements.
//! - [`trust`]: the device's pinned keys and highest accepted serial.
//! - [`auto`]: the per-package auto-update policy, schedule and engine.
//! - [`slots`]: the A/B boot record read by the ramdisk and written by the running system.
//! - [`store`]: persistence behind a trait, so everything here is testable without root.

pub mod auto;
pub mod manifest;
pub mod slots;
pub mod store;
pub mod trust;

pub use manifest::{KeyStatement, ManifestError, PublicKey, Signature, SignatureFile, SnapshotManifest};
pub use trust::{Accepted, TrustError, TrustState};
