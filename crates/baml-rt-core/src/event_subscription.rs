//! Event-subscription types for manifests and discovery.

use std::{error::Error as StdError, fmt};

use serde::{Deserialize, Deserializer, Serialize};

fn parse_trimmed<T, F>(value: impl AsRef<str>, build: F) -> Option<T>
where
    F: FnOnce(String) -> T,
{
    let trimmed = value.as_ref().trim();
    (!trimmed.is_empty()).then(|| build(trimmed.to_string()))
}

fn parse_lowercased<T, F>(value: impl AsRef<str>, build: F) -> Option<T>
where
    F: FnOnce(String) -> T,
{
    let trimmed = value.as_ref().trim();
    (!trimmed.is_empty()).then(|| build(trimmed.to_lowercase()))
}

fn deserialize_string<'de, D, T, F>(
    deserializer: D,
    parse: F,
    type_name: &str,
) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    F: FnOnce(String) -> Option<T>,
{
    let raw = String::deserialize(deserializer)?;
    parse(raw).ok_or_else(|| serde::de::Error::custom(format!("invalid {type_name}")))
}

fn collect_parsed<I, S, T, F>(values: I, mut parse: F) -> Vec<T>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    F: FnMut(&str) -> Option<T>,
{
    values
        .into_iter()
        .filter_map(|value| parse(value.as_ref()))
        .collect()
}

/// Event schema name such as `task-daemon.interpretation.v1`.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct EventSchemaVersion(String);

impl EventSchemaVersion {
    pub fn parse(value: impl AsRef<str>) -> Option<Self> {
        parse_trimmed(value, Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EventSchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventSchemaVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_string(deserializer, Self::parse, "event schema version")
    }
}

/// Event source name such as `slack` or `clickup`.
///
/// Matching lowercases input but does not rewrite punctuation or separators.
/// For task-daemon events, use these source names:
/// `slack`, `clickup`, and `github_issues`.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct EventSourceKind(String);

impl EventSourceKind {
    pub fn parse(value: impl AsRef<str>) -> Option<Self> {
        parse_lowercased(value, Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EventSourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventSourceKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_string(deserializer, Self::parse, "event source kind")
    }
}

/// Exact source identifier for narrower subscriptions.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct EventSourceKey(String);

impl EventSourceKey {
    pub fn parse(value: impl AsRef<str>) -> Option<Self> {
        parse_trimmed(value, Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EventSourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventSourceKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_string(deserializer, Self::parse, "event source key")
    }
}

/// Source-key prefix for broader matching when one exact source id is not enough.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct EventSourceKeyPrefix(String);

impl EventSourceKeyPrefix {
    pub fn parse(value: impl AsRef<str>) -> Option<Self> {
        parse_trimmed(value, Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EventSourceKeyPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventSourceKeyPrefix {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_string(deserializer, Self::parse, "event source key prefix")
    }
}

/// One manifest subscription entry.
///
/// Use this to say which events an agent wants to receive.
///
/// Matching rules:
/// - `schema_versions` are case-sensitive
/// - `source_kinds` are matched case-insensitively
/// - `source_keys` and `source_key_prefixes` are case-sensitive
/// - an entirely empty entry is ignored for event delivery
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventSubscription {
    /// Event schema versions this agent wants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schema_versions: Vec<EventSchemaVersion>,
    /// Source categories this agent wants to receive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_kinds: Vec<EventSourceKind>,
    /// Optional exact source identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_keys: Vec<EventSourceKey>,
    /// Optional source-key prefixes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_key_prefixes: Vec<EventSourceKeyPrefix>,
}

impl EventSubscription {
    fn is_unconstrained(&self) -> bool {
        self.schema_versions.is_empty()
            && self.source_kinds.is_empty()
            && self.source_keys.is_empty()
            && self.source_key_prefixes.is_empty()
    }

    /// Returns true when this subscription matches a discovery filter.
    pub fn matches_filter(&self, filter: &EventSubscriptionFilter) -> bool {
        let schema_ok = filter.required_schema_versions.is_empty()
            || self
                .schema_versions
                .iter()
                .any(|schema| filter.required_schema_versions.contains(schema));
        let source_ok = filter.required_source_kinds.is_empty()
            || self
                .source_kinds
                .iter()
                .any(|kind| filter.required_source_kinds.contains(kind));
        schema_ok && source_ok
    }

    /// Returns true when this subscription matches one published event.
    pub fn matches_published_event(&self, event: &PublishedEvent) -> bool {
        if self.is_unconstrained() {
            return false;
        }

        let schema_ok = self.schema_versions.is_empty()
            || self
                .schema_versions
                .iter()
                .any(|schema| schema == &event.schema_version);
        let source_kind_ok = self.source_kinds.is_empty()
            || self
                .source_kinds
                .iter()
                .any(|kind| kind == &event.source_kind);
        let source_key_ok = self.source_keys.is_empty()
            || self
                .source_keys
                .iter()
                .any(|source_key| source_key == &event.source_key);
        let source_key_prefix_ok = self.source_key_prefixes.is_empty()
            || self
                .source_key_prefixes
                .iter()
                .any(|prefix| event.source_key.as_str().starts_with(prefix.as_str()));

        schema_ok && source_kind_ok && source_key_ok && source_key_prefix_ok
    }
}

/// One published event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedEvent {
    pub schema_version: EventSchemaVersion,
    pub source_kind: EventSourceKind,
    pub source_key: EventSourceKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishedEventBuildError {
    EmptySchemaVersion,
    EmptySourceKind,
    EmptySourceKey,
}

impl fmt::Display for PublishedEventBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySchemaVersion => {
                f.write_str("published event schema version must be non-empty")
            }
            Self::EmptySourceKind => f.write_str("published event source kind must be non-empty"),
            Self::EmptySourceKey => f.write_str("published event source key must be non-empty"),
        }
    }
}

impl StdError for PublishedEventBuildError {}

impl PublishedEvent {
    /// Builds one normalized published event descriptor.
    pub fn try_new(
        schema_version: impl AsRef<str>,
        source_kind: impl AsRef<str>,
        source_key: impl AsRef<str>,
    ) -> Result<Self, PublishedEventBuildError> {
        Ok(Self {
            schema_version: EventSchemaVersion::parse(schema_version)
                .ok_or(PublishedEventBuildError::EmptySchemaVersion)?,
            source_kind: EventSourceKind::parse(source_kind)
                .ok_or(PublishedEventBuildError::EmptySourceKind)?,
            source_key: EventSourceKey::parse(source_key)
                .ok_or(PublishedEventBuildError::EmptySourceKey)?,
        })
    }
}

/// Discovery filter for event subscriptions.
///
/// Lists are OR-within-field and AND-across-fields:
/// - if multiple schema versions are provided, matching any listed schema is enough
/// - if multiple source kinds are provided, matching any listed source kind is enough
/// - when both fields are present, one subscription must satisfy both
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventSubscriptionFilter {
    pub required_schema_versions: Vec<EventSchemaVersion>,
    pub required_source_kinds: Vec<EventSourceKind>,
}

impl EventSubscriptionFilter {
    /// Builds a normalized subscription filter.
    pub fn new<I, J, S1, S2>(required_schema_versions: I, required_source_kinds: J) -> Self
    where
        I: IntoIterator<Item = S1>,
        J: IntoIterator<Item = S2>,
        S1: AsRef<str>,
        S2: AsRef<str>,
    {
        Self {
            required_schema_versions: collect_parsed(required_schema_versions, |value| {
                EventSchemaVersion::parse(value)
            }),
            required_source_kinds: collect_parsed(required_source_kinds, |value| {
                EventSourceKind::parse(value)
            }),
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

/// Returns true when any declared subscription matches one published event.
pub fn subscriptions_match_published_event(
    subscriptions: &[EventSubscription],
    event: &PublishedEvent,
) -> bool {
    subscriptions
        .iter()
        .any(|subscription| subscription.matches_published_event(event))
}

#[cfg(test)]
mod tests {
    use super::{
        EventSchemaVersion, EventSourceKey, EventSourceKeyPrefix, EventSourceKind,
        EventSubscription, EventSubscriptionFilter, PublishedEvent, subscriptions_match_filter,
        subscriptions_match_published_event,
    };

    fn schema(value: &str) -> EventSchemaVersion {
        EventSchemaVersion::parse(value).expect("valid schema version")
    }

    fn kind(value: &str) -> EventSourceKind {
        EventSourceKind::parse(value).expect("valid source kind")
    }

    fn key(value: &str) -> EventSourceKey {
        EventSourceKey::parse(value).expect("valid source key")
    }

    fn key_prefix(value: &str) -> EventSourceKeyPrefix {
        EventSourceKeyPrefix::parse(value).expect("valid source key prefix")
    }

    #[test]
    fn subscription_filter_requires_one_subscription_to_match_all_requested_fields() {
        let subscriptions = vec![
            EventSubscription {
                schema_versions: vec![schema("task-daemon.interpretation.v1")],
                source_kinds: vec![kind("slack")],
                ..EventSubscription::default()
            },
            EventSubscription {
                schema_versions: vec![schema("task-daemon.interpretation.v1")],
                source_kinds: vec![kind("clickup")],
                ..EventSubscription::default()
            },
        ];

        assert!(subscriptions_match_filter(
            &subscriptions,
            &EventSubscriptionFilter {
                required_schema_versions: vec![schema("task-daemon.interpretation.v1")],
                required_source_kinds: vec![kind("clickup")],
            },
        ));
        assert!(!subscriptions_match_filter(
            &subscriptions,
            &EventSubscriptionFilter {
                required_schema_versions: vec![schema("task-daemon.unknown.v1")],
                required_source_kinds: vec![kind("clickup")],
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
            vec![schema("Task-Daemon.Interpretation.V1")]
        );
        assert_eq!(filter.required_source_kinds, vec![kind("clickup")]);
    }

    #[test]
    fn published_event_matching_honors_schema_source_and_prefix_fields() {
        let subscriptions = vec![
            EventSubscription {
                schema_versions: vec![schema("task-daemon.interpretation.v1")],
                source_kinds: vec![kind("ClickUp")],
                source_key_prefixes: vec![key_prefix("clickup:list:")],
                ..EventSubscription::default()
            },
            EventSubscription {
                schema_versions: vec![schema("task-daemon.interpretation.v1")],
                source_kinds: vec![kind("slack")],
                source_keys: vec![key("slack:C123")],
                ..EventSubscription::default()
            },
        ];

        assert!(subscriptions_match_published_event(
            &subscriptions,
            &PublishedEvent::try_new(
                "task-daemon.interpretation.v1",
                "clickup",
                "clickup:list:901325431486",
            )
            .expect("valid published event"),
        ));
        assert!(subscriptions_match_published_event(
            &subscriptions,
            &PublishedEvent::try_new("task-daemon.interpretation.v1", "Slack", "slack:C123")
                .expect("valid published event"),
        ));
        assert!(!subscriptions_match_published_event(
            &subscriptions,
            &PublishedEvent::try_new("task-daemon.interpretation.v1", "slack", "slack:C456")
                .expect("valid published event"),
        ));
    }

    #[test]
    fn published_event_try_new_rejects_empty_fields() {
        assert_eq!(
            PublishedEvent::try_new("", "slack", "slack:C123")
                .expect_err("empty schema version should fail"),
            super::PublishedEventBuildError::EmptySchemaVersion
        );
        assert_eq!(
            PublishedEvent::try_new("task-daemon.interpretation.v1", " ", "slack:C123")
                .expect_err("empty source kind should fail"),
            super::PublishedEventBuildError::EmptySourceKind
        );
        assert_eq!(
            PublishedEvent::try_new("task-daemon.interpretation.v1", "slack", "")
                .expect_err("empty source key should fail"),
            super::PublishedEventBuildError::EmptySourceKey
        );
    }

    #[test]
    fn unconstrained_subscription_does_not_match_published_events() {
        let subscriptions = vec![EventSubscription::default()];

        assert!(!subscriptions_match_published_event(
            &subscriptions,
            &PublishedEvent::try_new("task-daemon.interpretation.v1", "slack", "slack:C123")
                .expect("valid published event"),
        ));
    }
}
