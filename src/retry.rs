//! Exponential backoff for failed polls.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Backoff {
    /// Delay before retry number `retry` (0-based): `initial * 2^retry`, capped at `max`.
    pub fn delay(&self, retry: u32) -> Duration {
        let factor = 2u32.saturating_pow(retry);
        self.initial.saturating_mul(factor).min(self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_doubles_and_caps() {
        let b = Backoff {
            initial: Duration::from_secs(60),
            max: Duration::from_secs(600),
        };
        let delays: Vec<u64> = (0..6).map(|r| b.delay(r).as_secs()).collect();
        assert_eq!(delays, [60, 120, 240, 480, 600, 600]);
        assert_eq!(b.delay(u32::MAX), Duration::from_secs(600));
    }
}
