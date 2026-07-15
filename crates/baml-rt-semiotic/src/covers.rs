// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use regex::Regex;
use serde_json::Value;

/// Structural match of artifact `covers` patterns against serialized tool args.
pub fn covers_match(patterns: &[String], tool_name: &str, args: &Value) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let call_text = format!(
        "{tool_name} {}",
        serde_json::to_string(args).unwrap_or_default()
    );
    patterns.iter().any(|pat| {
        Regex::new(pat)
            .map(|re| re.is_match(&call_text))
            .unwrap_or_else(|_| call_text.contains(pat.as_str()))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn regex_covers() {
        assert!(covers_match(
            &["UPDATE users".into()],
            "bash",
            &json!({"command": "UPDATE users SET x=1"})
        ));
    }

    #[test]
    fn mismatch() {
        assert!(!covers_match(
            &["DELETE FROM".into()],
            "bash",
            &json!({"command": "ls"})
        ));
    }
}
