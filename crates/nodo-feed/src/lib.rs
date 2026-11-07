//! The gate between untrusted feeds and the system.
//!
//! Users add feeds, so a package may be hostile. Before anything reaches xbps:
//!
//! - [`inspect_package`] reads an `.xbps` file without extracting it and reports every rule it
//!   breaks. It performs no I/O beyond the reader it is given.
//! - [`PinStore`] decides whether a feed's signing key and a package's version are acceptable, and
//!   blocks changed keys and downgrades until the user approves them.
//!
//! This crate does not download packages, verify signatures or run xbps.

mod inspect;
mod pins;
mod plist;
mod tar;
mod version;

pub use inspect::{
    inspect_package, Inspection, InspectLimits, PackageInfo, Policy, Violation, BASE_PACKAGE_PREFIXES,
    BASE_PACKAGES, MANIFEST_FILE,
};
pub use pins::{Approval, ApprovalError, BlockReason, Decision, FeedPin, KeyError, KeyFingerprint, PinStore};
pub use version::compare_versions;
