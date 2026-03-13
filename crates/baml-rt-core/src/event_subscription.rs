//! Shared event-subscription types used by manifests, discovery, and host-side delivery.

use serde::{Deserialize, Serialize};

fn normalize_trimmed<I, S>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    values
        .into_iter()
        .filter_map(|value| {
            let trimmed = value.as_ref().trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        })
        .collect()
}

fn normalize_source_kinds<I, S>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    values
        .into_iter()
        .filter_map(|value| {
            let trimmed = value.as_ref().trim();
            (!trimmed.is_empty()).then(|| trimmed.to_lowercase())
        })
        .collect()
}

/// One agent-declared event subscription.
///
/// This tells the host which published events an agent wants delivered. It is
/// intentionally coarse-grained: subscriptions identify event families and
/// source categories, not downstream workflow policy.
///
/// Matching semantics:
/// - `schema_versions` are matched case-sensitively
/// - `source_kinds` are normalized to lowercase and matched case-insensitively
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventSubscription {
    /// Event schema versions this agent can consume (for example
    /// `task-daemon.interpretation.v1`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schema_versions: Vec<String>,
    /// Source categories this agent wants to receive (for example `slack` or
    /// `clickup`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_kinds: Vec<String>,
    /// Optional exact source identifiers for narrower subscription matching.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_keys: Vec<String>,
    /// Optional source-key prefixes for coarse matching without exact source
    /// identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_key_prefixes: Vec<String>,
}

impl EventSubscription {
    /// Returns true when this subscription matches the requested discovery
    /// filter.
    pub fn matches_filter(&self, filter: &EventSubscriptionFilter) -> bool {
        let schema_ok = filter.required_schema_versions.is_empty()
            || normalize_trimmed(&self.schema_versions)
                .into_iter()
                .any(|schema| filter.required_schema_versions.contains(&schema));
        let source_ok = filter.required_source_kinds.is_empty()
            || normalize_source_kinds(&self.source_kinds)
                .into_iter()
                .any(|kind| filter.required_source_kinds.contains(&kind));
        schema_ok && source_ok
    }
}

/// Coarse discovery filter for event subscriptions.
///
/// Lists are OR-within-field and AND-across-fields:
/// - if multiple schema versions are provided, matching any listed schema is enough
/// - if multiple source kinds are provided, matching any listed source kind is enough
/// - when both fields are present, one subscription must satisfy both
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventSubscriptionFilter {
    pub required_schema_versions: Vec<String>,
    pub required_source_kinds: Vec<String>,
}

impl EventSubscriptionFilter {
    /// Builds a normalized subscription filter.
    ///
    /// Schema versions preserve case. Source kinds are normalized to lowercase.
    pub fn new<I, J, S1, S2>(required_schema_versions: I, required_source_kinds: J) -> Self
    where
        I: IntoIterator<Item = S1>,
        J: IntoIterator<Item = S2>,
        S1: AsRef<str>,
        S2: AsRef<str>,
    {
        Self {
            required_schema_versions: normalize_trimmed(required_schema_versions),
            required_source_kinds: normalize_source_kinds(required_source_kinds),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.required_schema_versions.is_empty() && self.required_source_kinds.is_empty()
    }
}

/// Returns true when any declared subscription matches the filter.
pub fn subscriptions_match_filter(
    subscriptions: &[EventSubscription],
    filter: &EventSubscriptionFilter,
) -> bool {
    if filter.is_empty() {
        return true;
    }
    subscriptions
        .iter()
        .any(|subscription| subscription.matches_filter(filter))
}

#[cfg(test)]
mod tests {
    use super::{EventSubscription, EventSubscriptionFilter, subscriptions_match_filter};

    #[test]
    fn subscription_filter_requires_one_subscription_to_match_all_requested_fields() {
        let subscriptions = vec![
            EventSubscription {
                schema_versions: vec!["task-daemon.interpretation.v1".to_string()],
                source_kinds: vec!["slack".to_string()],
                ..EventSubscription::default()
            },
            EventSubscription {
                schema_versions: vec!["task-daemon.interpretation.v1".to_string()],
                source_kinds: vec!["clickup".to_string()],
                ..EventSubscription::default()
            },
        ];

        assert!(subscriptions_match_filter(
            &subscriptions,
            &EventSubscriptionFilter {
                required_schema_versions: vec!["task-daemon.interpretation.v1".to_string()],
                required_source_kinds: vec!["clickup".to_string()],
            },
        ));
        assert!(!subscriptions_match_filter(
            &subscriptions,
            &EventSubscriptionFilter {
                required_schema_versions: vec!["task-daemon.unknown.v1".to_string()],
                required_source_kinds: vec!["clickup".to_string()],
            },
        ));
    }

    #[test]
    fn subscription_filter_normalizes_source_kind_case_but_preserves_schema_case() {
        let filter = EventSubscriptionFilter::new(
            vec![
                "Task-Daemon.Interpretation.V1".to_string(),
                "  ".to_string(),
            ],
            vec![" ClickUp ".to_string()],
        );

        assert_eq!(
            filter.required_schema_versions,
            vec!["Task-Daemon.Interpretation.V1".to_string()]
        );
        assert_eq!(filter.required_source_kinds, vec!["clickup".to_string()]);
    }
}
