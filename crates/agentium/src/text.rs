// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

/// Truncate text for terminal display, appending "..." when truncated.
///
/// This operates on Unicode scalar values (`char`) to avoid panicking on
/// UTF-8 byte boundaries.
pub fn truncate_for_display(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        return s.to_string();
    }

    if max_len <= 3 {
        return ".".repeat(max_len);
    }

    let mut out = String::new();
    out.extend(s.chars().take(max_len - 3));
    out.push_str("...");
    out
}
