//! Slack `ts` string comparison helpers (shared by polling implementations).

use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SlackTimestamp {
    seconds: u64,
    micros: u32,
}

fn parse_slack_timestamp(ts: &str) -> Option<SlackTimestamp> {
    let trimmed = ts.trim();
    let (seconds, fractional) = match trimmed.split_once('.') {
        Some((seconds, fractional)) => (seconds, Some(fractional)),
        None => (trimmed, None),
    };

    if seconds.is_empty() || !seconds.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let seconds: u64 = seconds.parse().ok()?;
    let micros = match fractional {
        Some(frac) => {
            let digits: String = frac.chars().take(6).collect();
            if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
                return None;
            }
            let padded = format!("{digits:0<6}");
            padded.parse().ok()?
        }
        None => 0,
    };
    Some(SlackTimestamp { seconds, micros })
}

/// True when `left` is strictly newer than `right`.
pub fn ts_gt(left: &str, right: &str) -> bool {
    match (parse_slack_timestamp(left), parse_slack_timestamp(right)) {
        (Some(left), Some(right)) => left > right,
        (None, _) | (_, None) => {
            warn!(
                left_ts = left,
                right_ts = right,
                "comparing Slack timestamps with lexical fallback after parse failure"
            );
            left > right
        }
    }
}

/// Ordering for sorting messages by Slack `ts`.
pub fn ts_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    match (parse_slack_timestamp(left), parse_slack_timestamp(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (None, _) | (_, None) => {
            warn!(
                left_ts = left,
                right_ts = right,
                "sorting Slack timestamps with lexical fallback after parse failure"
            );
            left.cmp(right)
        }
    }
}

/// Compact `seconds.micros` into Slack permalink segment (`p{seconds}{micros6}`).
pub fn compact_ts_for_permalink(ts: &str) -> Option<String> {
    let (left, right) = ts.split_once('.')?;
    if left.len() < 9 || left.len() > 10 {
        return None;
    }
    let mut micros = right.chars().take(6).collect::<String>();
    while micros.len() < 6 {
        micros.push('0');
    }
    Some(format!("{left}{micros}"))
}

/// Latest of two optional Slack timestamps.
pub fn max_ts(left: Option<&str>, right: Option<&str>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => {
            if ts_gt(left, right) {
                Some(left.to_string())
            } else {
                Some(right.to_string())
            }
        }
        (Some(left), None) => Some(left.to_string()),
        (None, Some(right)) => Some(right.to_string()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_gt_orders_parsed_timestamps() {
        assert!(ts_gt("1700000001.000000", "1700000000.999999"));
        assert!(!ts_gt("1700000000.000000", "1700000001.000000"));
    }

    #[test]
    fn max_ts_picks_latest() {
        assert_eq!(
            max_ts(Some("1700000001.000000"), Some("1700000000.000000")).as_deref(),
            Some("1700000001.000000")
        );
    }

    #[test]
    fn compact_ts_for_permalink_formats_micros() {
        assert_eq!(
            compact_ts_for_permalink("1700000000.000042").as_deref(),
            Some("1700000000000042")
        );
    }
}
