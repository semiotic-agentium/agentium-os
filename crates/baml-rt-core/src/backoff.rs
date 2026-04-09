//! Exponential backoff utilities.

use std::time::Duration;

/// Compute the delay for a single backoff attempt.
///
/// `base * 2^attempt`, capped at `max`. Uses saturating arithmetic to avoid
/// overflow regardless of the attempt count.
pub fn backoff_delay(base: Duration, max: Duration, attempt: u32) -> Duration {
    let shift = attempt.min(16);
    let delay = base.saturating_mul(2u32.pow(shift));
    delay.min(max)
}

/// Stateful exponential backoff tracker.
///
/// Starts at `base` and doubles on each call to [`next_delay`](Self::next_delay),
/// capping at `max`. Call [`reset`](Self::reset) after a successful connection to
/// restart the sequence.
pub struct ExponentialBackoff {
    base: Duration,
    max: Duration,
    attempts: u32,
}

impl ExponentialBackoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            attempts: 0,
        }
    }

    pub fn reset(&mut self) {
        self.attempts = 0;
    }

    /// Return the next backoff delay and advance the attempt counter.
    ///
    /// The first call returns `base` (not zero), which is intentional for
    /// reconnect scenarios where an immediate retry after failure is
    /// undesirable. Subsequent calls double the delay up to `max`.
    pub fn next_delay(&mut self) -> Duration {
        let delay = backoff_delay(self.base, self.max, self.attempts);
        self.attempts = self.attempts.saturating_add(1);
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_delay_doubles() {
        let base = Duration::from_millis(500);
        let max = Duration::from_secs(30);
        assert_eq!(backoff_delay(base, max, 0), Duration::from_millis(500));
        assert_eq!(backoff_delay(base, max, 1), Duration::from_millis(1000));
        assert_eq!(backoff_delay(base, max, 2), Duration::from_millis(2000));
        assert_eq!(backoff_delay(base, max, 3), Duration::from_millis(4000));
    }

    #[test]
    fn backoff_delay_caps_at_max() {
        let base = Duration::from_millis(500);
        let max = Duration::from_secs(5);
        assert_eq!(backoff_delay(base, max, 0), Duration::from_millis(500));
        assert_eq!(backoff_delay(base, max, 1), Duration::from_millis(1000));
        assert_eq!(backoff_delay(base, max, 2), Duration::from_millis(2000));
        assert_eq!(backoff_delay(base, max, 3), Duration::from_millis(4000));
        assert_eq!(backoff_delay(base, max, 4), Duration::from_millis(5000));
    }

    #[test]
    fn exponential_backoff_first_delay_is_base() {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_millis(500), Duration::from_secs(30));
        assert_eq!(backoff.next_delay(), Duration::from_millis(500));
    }

    #[test]
    fn exponential_backoff_doubles_each_attempt() {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_millis(100), Duration::from_secs(60));
        assert_eq!(backoff.next_delay(), Duration::from_millis(100));
        assert_eq!(backoff.next_delay(), Duration::from_millis(200));
        assert_eq!(backoff.next_delay(), Duration::from_millis(400));
    }

    #[test]
    fn exponential_backoff_caps_at_max() {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_millis(500), Duration::from_secs(30));
        for _ in 0..20 {
            backoff.next_delay();
        }
        assert_eq!(backoff.next_delay(), Duration::from_secs(30));
    }

    #[test]
    fn exponential_backoff_reset_restarts_from_base() {
        let mut backoff =
            ExponentialBackoff::new(Duration::from_millis(500), Duration::from_secs(30));
        backoff.next_delay();
        backoff.next_delay();
        backoff.next_delay();
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_millis(500));
    }
}
