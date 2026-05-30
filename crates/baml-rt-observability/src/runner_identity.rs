// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Canonical runner identity for the K8s pilot observability contract.
//!
//! Exposes a single derivation rule for `service.instance.id` that is used by both
//! [`crate::otel_env::build_runner_resource`] (as the OTEL resource attribute) and by
//! span/metric call sites that emit `{ingress,serving,target}_service_instance_id` as
//! explicit labels. One derivation, two consumers — the resource and span/metric values
//! cannot drift.
//!
//! Precedence (top-down):
//! 1. `OTEL_RESOURCE_ATTRIBUTES` `service.instance.id=<value>` override (spec-correct).
//! 2. `POD_NAME` — Kubernetes downward API value.
//! 3. `HOSTNAME` — typical container hostname, and the value surfaced by the cluster
//!    registry at `crates/baml-agent-runner/src/cluster.rs:194`.
//! 4. A generated UUID, stable for the process lifetime.

use std::sync::OnceLock;

/// OTEL environment variable that can override individual resource attributes.
const OTEL_RESOURCE_ATTRIBUTES_ENV: &str = "OTEL_RESOURCE_ATTRIBUTES";
/// Resource attribute key this helper resolves.
pub const SERVICE_INSTANCE_ID_KEY: &str = "service.instance.id";
/// Baggage key marking a forwarded A2A request with the ingress runner's
/// `service.instance.id`. Shared between the router's `forward_request` (which
/// injects it) and the API layer's middleware (which extracts it); any drift
/// silently breaks forwarded classification, so the key lives in one place.
pub const INGRESS_SERVICE_INSTANCE_ID_BAGGAGE_KEY: &str = "ingress_service_instance_id";
/// Sentinel rendered in place of an absent `target_service_instance_id` on
/// spans and metrics. Bounded string — keeps the label set low-cardinality.
pub const UNKNOWN_SERVICE_INSTANCE_ID: &str = "unknown";

/// Pure derivation of the canonical `service.instance.id` for the runner.
///
/// Reads env on every call — callers that want a stable cached value should use
/// [`service_instance_id`] instead.
pub fn derive_service_instance_id() -> String {
    if let Some(v) = parse_otel_resource_attr(SERVICE_INSTANCE_ID_KEY) {
        return v;
    }
    if let Ok(v) = std::env::var("POD_NAME")
        && !v.trim().is_empty()
    {
        return v;
    }
    if let Ok(v) = std::env::var("HOSTNAME")
        && !v.trim().is_empty()
    {
        return v;
    }
    uuid::Uuid::new_v4().to_string()
}

/// `(POD_NAMESPACE, pod_name)` from the K8s downward API, where `pod_name`
/// resolves to `POD_NAME` then `HOSTNAME`. Empty / whitespace-only values are
/// treated as absent, matching the semantics of [`derive_service_instance_id`]
/// so the two derivations cannot disagree on what counts as a valid pod
/// identifier. Returns `None` outside K8s.
pub fn pod_identity() -> Option<(String, String)> {
    let namespace = std::env::var("POD_NAMESPACE")
        .ok()
        .filter(|v| !v.trim().is_empty())?;
    let pod_name = std::env::var("POD_NAME")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .filter(|v| !v.trim().is_empty())?;
    Some((namespace, pod_name))
}

/// Canonical `service.instance.id` for this process. Lazily initialized on first call and
/// cached for the process lifetime so repeated reads never drift.
pub fn service_instance_id() -> &'static str {
    static SERVICE_INSTANCE_ID: OnceLock<String> = OnceLock::new();
    SERVICE_INSTANCE_ID
        .get_or_init(derive_service_instance_id)
        .as_str()
}

/// Parse `OTEL_RESOURCE_ATTRIBUTES` for the last-occurrence value of `key`.
///
/// Accepts the common `k1=v1,k2=v2` shape. Whitespace around keys and values is trimmed.
/// A malformed pair (no `=`, or empty key) is skipped silently. This is deliberately
/// lenient; a fully spec-compliant parser handles percent-encoding and escaped commas,
/// which the pilot contract does not require.
pub(crate) fn parse_otel_resource_attr(key: &str) -> Option<String> {
    let raw = std::env::var(OTEL_RESOURCE_ATTRIBUTES_ENV).ok()?;
    let mut found: Option<String> = None;
    for pair in raw.split(',') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if k.is_empty() {
            continue;
        }
        if k == key {
            found = Some(v.trim().to_string());
        }
    }
    found.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::EnvScope;

    #[test]
    fn prefers_otel_resource_attributes_override() {
        let mut env = EnvScope::new();
        env.set(
            "OTEL_RESOURCE_ATTRIBUTES",
            Some("service.name=x,service.instance.id=pod-from-env,deployment.environment=dev"),
        );
        env.set("POD_NAME", Some("pod-name-ignored"));
        env.set("HOSTNAME", Some("hostname-ignored"));
        assert_eq!(derive_service_instance_id(), "pod-from-env");
    }

    #[test]
    fn falls_back_to_pod_name() {
        let mut env = EnvScope::new();
        env.set("OTEL_RESOURCE_ATTRIBUTES", None);
        env.set("POD_NAME", Some("runner-0"));
        env.set("HOSTNAME", Some("hostname-ignored"));
        assert_eq!(derive_service_instance_id(), "runner-0");
    }

    #[test]
    fn falls_back_to_hostname_when_pod_name_missing() {
        let mut env = EnvScope::new();
        env.set("OTEL_RESOURCE_ATTRIBUTES", None);
        env.set("POD_NAME", None);
        env.set("HOSTNAME", Some("ci-runner"));
        assert_eq!(derive_service_instance_id(), "ci-runner");
    }

    #[test]
    fn falls_back_to_uuid_when_all_env_absent() {
        let mut env = EnvScope::new();
        env.set("OTEL_RESOURCE_ATTRIBUTES", None);
        env.set("POD_NAME", None);
        env.set("HOSTNAME", None);
        let id = derive_service_instance_id();
        assert!(
            uuid::Uuid::parse_str(&id).is_ok(),
            "expected UUID fallback, got {id:?}"
        );
    }

    #[test]
    fn empty_pod_name_falls_through_to_hostname() {
        let mut env = EnvScope::new();
        env.set("OTEL_RESOURCE_ATTRIBUTES", None);
        env.set("POD_NAME", Some("   "));
        env.set("HOSTNAME", Some("hostfb"));
        assert_eq!(derive_service_instance_id(), "hostfb");
    }

    #[test]
    fn malformed_otel_resource_attributes_is_ignored() {
        let mut env = EnvScope::new();
        env.set(
            "OTEL_RESOURCE_ATTRIBUTES",
            Some("no-equals-pair,service.instance.id=good-value,=orphan"),
        );
        env.set("POD_NAME", None);
        env.set("HOSTNAME", None);
        assert_eq!(derive_service_instance_id(), "good-value");
    }

    #[test]
    fn last_occurrence_wins_for_duplicate_keys() {
        let mut env = EnvScope::new();
        env.set(
            "OTEL_RESOURCE_ATTRIBUTES",
            Some("service.instance.id=first,service.instance.id=second"),
        );
        env.set("POD_NAME", None);
        env.set("HOSTNAME", None);
        assert_eq!(derive_service_instance_id(), "second");
    }

    #[test]
    fn empty_value_override_falls_through_to_pod_name() {
        let mut env = EnvScope::new();
        env.set("OTEL_RESOURCE_ATTRIBUTES", Some("service.instance.id="));
        env.set("POD_NAME", Some("pod-fb"));
        env.set("HOSTNAME", None);
        assert_eq!(derive_service_instance_id(), "pod-fb");
    }

    #[test]
    fn pod_identity_returns_namespace_and_pod_name_when_both_set() {
        let mut env = EnvScope::new();
        env.set("POD_NAMESPACE", Some("agentium"));
        env.set("POD_NAME", Some("runner-0"));
        env.set("HOSTNAME", Some("hostname-ignored"));
        assert_eq!(
            pod_identity(),
            Some(("agentium".to_string(), "runner-0".to_string()))
        );
    }

    #[test]
    fn pod_identity_falls_back_to_hostname_for_pod_name() {
        let mut env = EnvScope::new();
        env.set("POD_NAMESPACE", Some("agentium"));
        env.set("POD_NAME", None);
        env.set("HOSTNAME", Some("ci-runner"));
        assert_eq!(
            pod_identity(),
            Some(("agentium".to_string(), "ci-runner".to_string()))
        );
    }

    #[test]
    fn pod_identity_returns_none_without_namespace() {
        let mut env = EnvScope::new();
        env.set("POD_NAMESPACE", None);
        env.set("POD_NAME", Some("runner-0"));
        env.set("HOSTNAME", Some("ci-runner"));
        assert_eq!(pod_identity(), None);
    }

    #[test]
    fn pod_identity_treats_whitespace_as_absent() {
        let mut env = EnvScope::new();
        env.set("POD_NAMESPACE", Some("   "));
        env.set("POD_NAME", Some("runner-0"));
        env.set("HOSTNAME", None);
        assert_eq!(pod_identity(), None);
    }
}
