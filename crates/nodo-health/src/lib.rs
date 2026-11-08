//! Keeps the device healthy and says so.
//!
//! A full disk crash-loops cuprated: it panics with "No space left on device" and runit restarts it
//! forever. So the daemon here watches free space and stops the services writing to a filesystem,
//! cleanly, before it fills, keeps them stopped across restarts and reboots, and starts them again
//! once there is room. It also bounds log sizes, places services in memory-limited cgroups, reads
//! the time sync status, and writes one structured report for the Home screen.
//!
//! All system access goes through [`sys::System`], so every decision is testable without root.

pub mod config;
pub mod disk;
pub mod logs;
pub mod memory;
pub mod report;
pub mod sys;
pub mod time;
pub mod tick;

pub use config::Config;
pub use report::Report;
pub use tick::{tick, State};
