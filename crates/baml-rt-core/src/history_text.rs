/// Remove an optional transcript ordinal prefix like `#12 ` before classification.
#[must_use]
pub fn strip_history_notice_prefix(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix('#') else {
        return trimmed;
    };
    let digit_count = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_count == 0 {
        return trimmed;
    }
    rest[digit_count..].trim_start()
}

/// True when `text` is an infrastructure notice, not conversational content.
#[must_use]
pub fn is_history_infrastructure_notice(text: &str) -> bool {
    let core = strip_history_notice_prefix(text);
    core.starts_with("Calling model:") || core.starts_with("Invoking tool:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_numbered_history_prefix() {
        assert_eq!(
            strip_history_notice_prefix("#12 Calling model: foo"),
            "Calling model: foo"
        );
        assert_eq!(
            strip_history_notice_prefix("  #7 Invoking tool: support/calculate"),
            "Invoking tool: support/calculate"
        );
        assert_eq!(
            strip_history_notice_prefix("assistant reply"),
            "assistant reply"
        );
    }

    #[test]
    fn classifies_infrastructure_notices() {
        assert!(is_history_infrastructure_notice(
            "#2 Calling model: openai/gpt-4o-mini"
        ));
        assert!(is_history_infrastructure_notice(
            "Invoking tool: system/discover_agents"
        ));
        assert!(!is_history_infrastructure_notice(
            "Need to search agents now."
        ));
    }
}
