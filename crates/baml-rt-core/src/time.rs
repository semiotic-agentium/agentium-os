// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/// Milliseconds since UNIX epoch.
///
/// Logs a warning on clock skew (system clock before epoch) and returns `0`
/// rather than panicking. All workspace code that needs "now as unix millis"
/// should call this instead of inlining `SystemTime` arithmetic.
pub fn now_unix_ms(clock_event: &'static str) -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(error) => {
            tracing::warn!(
                clock_event,
                error = %error,
                "clock skew detected; using zero unix timestamp"
            );
            0
        }
    }
}

/// Seconds since UNIX epoch.
///
/// Same clock-skew handling as [`now_unix_ms`] but at second granularity.
pub fn now_unix_secs(clock_event: &'static str) -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(error) => {
            tracing::warn!(
                clock_event,
                error = %error,
                "clock skew detected; using zero unix timestamp"
            );
            0
        }
    }
}
