//! Shared `Retry-After` header parsing for HTTP integration clients.

use std::time::Duration;

/// Parsed value of an HTTP `Retry-After` response header.
///
/// Only the seconds-form is interpreted as a concrete delay via
/// [`RetryAfter::as_duration`]; HTTP-date values and unparseable bytes are
/// surfaced as [`RetryAfter::Unknown`] so that callers can log them while
/// falling back to their own backoff schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryAfter {
    Seconds(u64),
    Unknown(String),
    Missing,
}

impl RetryAfter {
    /// Return the header value as a [`Duration`] when it was a seconds count.
    pub fn as_duration(&self) -> Option<Duration> {
        match self {
            RetryAfter::Seconds(seconds) => Some(Duration::from_secs(*seconds)),
            RetryAfter::Unknown(_) | RetryAfter::Missing => None,
        }
    }
}

impl std::fmt::Display for RetryAfter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RetryAfter::Seconds(seconds) => write!(f, "{seconds}s"),
            RetryAfter::Unknown(raw) => write!(f, "unknown({raw})"),
            RetryAfter::Missing => write!(f, "missing"),
        }
    }
}

/// Parse an HTTP `Retry-After` header value into a [`RetryAfter`].
///
/// Accepts the raw header bytes (e.g. `HeaderValue::as_bytes()`) so this crate
/// stays independent of any specific HTTP client. A missing header yields
/// [`RetryAfter::Missing`]; non-UTF-8 bytes and arbitrary strings (such as
/// HTTP-date forms of `Retry-After`) are surfaced verbatim in
/// [`RetryAfter::Unknown`]. Only the integer-seconds form yields
/// [`RetryAfter::Seconds`].
pub fn parse_retry_after(value: Option<&[u8]>) -> RetryAfter {
    let Some(bytes) = value else {
        return RetryAfter::Missing;
    };
    let raw = match std::str::from_utf8(bytes) {
        Ok(raw) => raw,
        Err(_) => return RetryAfter::Unknown("invalid-utf8".to_string()),
    };
    match raw.trim().parse::<u64>() {
        Ok(seconds) => RetryAfter::Seconds(seconds),
        Err(_) => RetryAfter::Unknown(raw.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_header_yields_missing() {
        assert!(matches!(parse_retry_after(None), RetryAfter::Missing));
    }

    #[test]
    fn integer_seconds_parses() {
        assert!(matches!(
            parse_retry_after(Some(b"42")),
            RetryAfter::Seconds(42)
        ));
    }

    #[test]
    fn whitespace_around_seconds_is_trimmed() {
        assert!(matches!(
            parse_retry_after(Some(b"  7 ")),
            RetryAfter::Seconds(7)
        ));
    }

    #[test]
    fn http_date_value_is_unknown() {
        match parse_retry_after(Some(b"Wed, 21 Oct 2015 07:28:00 GMT")) {
            RetryAfter::Unknown(raw) => assert_eq!(raw, "Wed, 21 Oct 2015 07:28:00 GMT"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn non_utf8_bytes_are_unknown() {
        match parse_retry_after(Some(&[0xFF, 0xFE])) {
            RetryAfter::Unknown(raw) => assert_eq!(raw, "invalid-utf8"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn as_duration_only_for_seconds() {
        assert_eq!(
            RetryAfter::Seconds(3).as_duration(),
            Some(Duration::from_secs(3))
        );
        assert!(RetryAfter::Unknown("x".to_string()).as_duration().is_none());
        assert!(RetryAfter::Missing.as_duration().is_none());
    }

    #[test]
    fn display_formats() {
        assert_eq!(RetryAfter::Seconds(5).to_string(), "5s");
        assert_eq!(
            RetryAfter::Unknown("foo".to_string()).to_string(),
            "unknown(foo)"
        );
        assert_eq!(RetryAfter::Missing.to_string(), "missing");
    }
}
