//! The gate between untrusted feeds and the system.

mod pins;
mod version;

pub use pins::{Approval, ApprovalError, BlockReason, Decision, FeedPin, KeyError, KeyFingerprint, PinStore};
pub use version::compare_versions;
