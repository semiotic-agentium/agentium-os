// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Exponential backoff utilities.

use std::time::Duration;

/// Default base delay for HTTP rate-limit retries.
pub const RATE_LIMIT_BASE_DELAY: Duration = Duration::from_millis(500);
/// Default cap for HTTP rate-limit retry delays.
pub const RATE_LIMIT_MAX_DELAY: Duration = Duration::from_secs(5);
/// Default maximum number of retries when an HTTP request is rate-limited.
pub const MAX_RATE_LIMIT_RETRIES: u32 = 3;

/// Compute the delay for a single backoff attempt.
///
/// `base * 2^attempt`, capped at `max`. Uses saturating arithmetic to avoid
/// overflow regardless of the attempt count.
pub fn backoff_delay(base: Duration, max: Duration, attempt: u32) -> Duration {
    let shift = attempt.min(16);
    let delay = base.saturating_mul(2u32.pow(shift));
    delay.min(max)
}

/// Convenience wrapper for HTTP clients that want the standard rate-limit
/// backoff schedule (`RATE_LIMIT_BASE_DELAY` doubling up to `RATE_LIMIT_MAX_DELAY`).
pub fn rate_limit_backoff_delay(attempt: u32) -> Duration {
    backoff_delay(RATE_LIMIT_BASE_DELAY, RATE_LIMIT_MAX_DELAY, attempt)
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
    fn backoff_schedule_matrix() {
        let base = Duration::from_millis(500);
        let max = Duration::from_secs(30);
        assert_eq!(backoff_delay(base, max, 0), Duration::from_millis(500));
        assert_eq!(backoff_delay(base, max, 1), Duration::from_millis(1000));
        assert_eq!(backoff_delay(base, max, 2), Duration::from_millis(2000));
        assert_eq!(backoff_delay(base, max, 3), Duration::from_millis(4000));

        let cap_max = Duration::from_secs(5);
        assert_eq!(backoff_delay(base, cap_max, 4), Duration::from_millis(5000));

        let mut exp = ExponentialBackoff::new(Duration::from_millis(500), Duration::from_secs(30));
        assert_eq!(exp.next_delay(), Duration::from_millis(500));

        let mut doubling =
            ExponentialBackoff::new(Duration::from_millis(100), Duration::from_secs(60));
        assert_eq!(doubling.next_delay(), Duration::from_millis(100));
        assert_eq!(doubling.next_delay(), Duration::from_millis(200));
        assert_eq!(doubling.next_delay(), Duration::from_millis(400));

        let mut capped =
            ExponentialBackoff::new(Duration::from_millis(500), Duration::from_secs(30));
        for _ in 0..20 {
            capped.next_delay();
        }
        assert_eq!(capped.next_delay(), Duration::from_secs(30));

        assert_eq!(rate_limit_backoff_delay(0), Duration::from_millis(500));
        assert_eq!(rate_limit_backoff_delay(10), RATE_LIMIT_MAX_DELAY);

        exp.next_delay();
        exp.next_delay();
        exp.reset();
        assert_eq!(exp.next_delay(), Duration::from_millis(500));
    }
}
