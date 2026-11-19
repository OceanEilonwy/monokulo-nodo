//! The time sessions and jobs are measured by, so tests can move it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Time since some fixed start. Only differences are ever used.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Duration;
}

/// The machine's monotonic clock, counted from when the process started.
pub struct SystemClock(Instant);

impl SystemClock {
    pub fn new() -> Self {
        Self(Instant::now())
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// A clock that moves only when told to, for tests of expiry and waits.
#[derive(Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    pub fn advance(&self, by: Duration) {
        self.0.fetch_add(by.as_millis() as u64, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}
