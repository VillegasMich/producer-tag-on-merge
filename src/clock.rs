//! Wall clock + interruptible sleep, abstracted so the poller can be tested with a fake.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

pub trait Clock {
    /// Current time, always UTC.
    fn now(&self) -> DateTime<Utc>;

    /// Sleeps for `duration`. Returns `false` if the sleep was cut short by a shutdown request.
    fn sleep(&self, duration: Duration) -> bool;

    fn shutdown_requested(&self) -> bool;
}

/// Real clock. Sleeps in short ticks so `SIGTERM` is honored within a fraction of a second.
#[derive(Debug, Clone)]
pub struct SystemClock {
    shutdown: Arc<AtomicBool>,
}

const TICK: Duration = Duration::from_millis(250);

impl SystemClock {
    pub fn new(shutdown: Arc<AtomicBool>) -> Self {
        Self { shutdown }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn sleep(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        loop {
            if self.shutdown_requested() {
                return false;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return true;
            }
            std::thread::sleep(remaining.min(TICK));
        }
    }

    fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
pub mod testing {
    use std::cell::{Cell, RefCell};

    use chrono::TimeDelta;

    use super::*;

    /// Deterministic clock: `sleep` advances time instantly. Optionally simulates a shutdown
    /// request after a given number of sleeps.
    pub struct FakeClock {
        now: Cell<DateTime<Utc>>,
        sleeps: RefCell<Vec<Duration>>,
        interrupt_after: Cell<Option<usize>>,
        shutdown: Cell<bool>,
    }

    impl FakeClock {
        pub fn at(rfc3339: &str) -> Self {
            Self {
                now: Cell::new(DateTime::parse_from_rfc3339(rfc3339).unwrap().to_utc()),
                sleeps: RefCell::default(),
                interrupt_after: Cell::new(None),
                shutdown: Cell::new(false),
            }
        }

        /// The `n + 1`-th sleep call reports a shutdown.
        pub fn interrupt_after(self, n: usize) -> Self {
            self.interrupt_after.set(Some(n));
            self
        }

        /// Moves time forward without a sleep call, like a suspended laptop.
        pub fn advance(&self, by: TimeDelta) {
            self.now.set(self.now.get() + by);
        }

        pub fn request_shutdown(&self) {
            self.shutdown.set(true);
        }

        pub fn sleeps(&self) -> Vec<Duration> {
            self.sleeps.borrow().clone()
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            self.now.get()
        }

        fn sleep(&self, duration: Duration) -> bool {
            if self.shutdown.get() || self.interrupt_after.get() == Some(self.sleeps.borrow().len())
            {
                self.shutdown.set(true);
                return false;
            }
            self.sleeps.borrow_mut().push(duration);
            self.now
                .set(self.now.get() + TimeDelta::from_std(duration).unwrap());
            true
        }

        fn shutdown_requested(&self) -> bool {
            self.shutdown.get()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_sleep_is_interrupted_by_shutdown() {
        let flag = Arc::new(AtomicBool::new(true));
        let clock = SystemClock::new(flag);
        let started = Instant::now();
        assert!(!clock.sleep(Duration::from_secs(10)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn system_clock_sleeps() {
        let clock = SystemClock::new(Arc::new(AtomicBool::new(false)));
        assert!(clock.sleep(Duration::from_millis(20)));
    }
}
