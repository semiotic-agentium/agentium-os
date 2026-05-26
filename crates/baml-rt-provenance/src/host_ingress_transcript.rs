//! Format host ingress lines for operator conversation-history rows.

use baml_rt_conversation::operational::{
    OperationalEventContent, OperationalEventKind, OperationalEventSeverity,
};
use baml_rt_vocabulary::vocabulary::a2a;
use serde_json::{Map, Value};

/// Summary line for `HostSourcePollRecorded` transcript rows.
#[must_use]
pub fn format_source_poll_summary(
    source_kind: &str,
    source_key: &str,
    schema_version: &str,
    source_cursor: &str,
    record_count: usize,
) -> String {
    format!(
        "Host source poll: {source_kind} {source_key} — {record_count} record(s), schema {schema_version}, cursor {source_cursor}"
    )
}

/// Summary line for `HostDispatchAccepted` transcript rows.
#[must_use]
pub fn format_dispatch_accepted_summary(
    routing_key: &str,
    target_package: &str,
    target_instance: &str,
    schema_version: &str,
    source_kind: &str,
    source_key: &str,
) -> String {
    format!(
        "Host dispatch accepted: {routing_key} → {target_package}/{target_instance} ({schema_version}, {source_kind} {source_key})"
    )
}

/// Summary line for `HostDispatchRejected` / transport failure transcript rows.
#[must_use]
pub fn format_dispatch_rejected_summary(
    routing_key: &str,
    target_package: &str,
    target_instance: &str,
    detail: &str,
    transport: bool,
) -> String {
    let verb = if transport {
        "transport failed"
    } else {
        "rejected"
    };
    format!("Host dispatch {verb}: {routing_key} → {target_package}/{target_instance} — {detail}")
}

/// Map persisted `a2a:host_ingress_kind` + props to an operational summary when content is absent.
#[must_use]
pub fn host_ingress_summary_from_props(
    props: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let kind = prop_str(props, a2a::HOST_INGRESS_KIND)?;
    match kind.as_str() {
        "source_poll_recorded" => {
            let source_kind =
                prop_str(props, a2a::HOST_INGRESS_SOURCE_KIND).unwrap_or_else(|| "source".into());
            let source_key =
                prop_str(props, a2a::HOST_INGRESS_SOURCE_KEY).unwrap_or_else(|| "unknown".into());
            let schema = "unknown".to_string();
            let cursor = source_key.clone();
            let count = props
                .get("a2a_host_ingress_record_count")
                .or_else(|| props.get("a2a:host_ingress_record_count"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            Some(format_source_poll_summary(
                &source_kind,
                &source_key,
                &schema,
                &cursor,
                count,
            ))
        }
        "dispatch_accepted" => {
            let routing =
                prop_str(props, a2a::HOST_INGRESS_ROUTING_KEY).unwrap_or_else(|| "dispatch".into());
            let package = prop_str(props, a2a::HOST_INGRESS_TARGET_PACKAGE)
                .unwrap_or_else(|| "unknown".into());
            let instance = prop_str(props, a2a::HOST_INGRESS_TARGET_INSTANCE)
                .unwrap_or_else(|| "default".into());
            let schema = "unknown".to_string();
            let source_kind =
                prop_str(props, a2a::HOST_INGRESS_SOURCE_KIND).unwrap_or_else(|| "unknown".into());
            let source_key =
                prop_str(props, a2a::HOST_INGRESS_SOURCE_KEY).unwrap_or_else(|| "unknown".into());
            Some(format_dispatch_accepted_summary(
                &routing,
                &package,
                &instance,
                &schema,
                &source_kind,
                &source_key,
            ))
        }
        "dispatch_rejected" | "dispatch_transport_error" => {
            let routing =
                prop_str(props, a2a::HOST_INGRESS_ROUTING_KEY).unwrap_or_else(|| "dispatch".into());
            let package = prop_str(props, a2a::HOST_INGRESS_TARGET_PACKAGE)
                .unwrap_or_else(|| "unknown".into());
            let instance = prop_str(props, a2a::HOST_INGRESS_TARGET_INSTANCE)
                .unwrap_or_else(|| "default".into());
            let detail = prop_str(props, a2a::REASON).unwrap_or_else(|| "rejected".into());
            Some(format_dispatch_rejected_summary(
                &routing,
                &package,
                &instance,
                &detail,
                kind == "dispatch_transport_error",
            ))
        }
        _ => None,
    }
}

fn prop_str(props: &Map<String, Value>, key: &str) -> Option<String> {
    let storage_key = key.replace(':', "_");
    props
        .get(&storage_key)
        .or_else(|| props.get(key))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Map a host-role Message node to an operator [`OperationalEventContent`].
#[must_use]
pub fn operational_from_host_message(
    text: &str,
    props: &Map<String, Value>,
) -> Option<OperationalEventContent> {
    let ingress_kind = prop_str(props, a2a::HOST_INGRESS_KIND)?;
    let (kind, severity) = match ingress_kind.as_str() {
        "source_poll_recorded" => (
            OperationalEventKind::SourcePollRecorded,
            OperationalEventSeverity::Info,
        ),
        "dispatch_accepted" => (
            OperationalEventKind::DispatchAccepted,
            OperationalEventSeverity::Info,
        ),
        "dispatch_rejected" => (
            OperationalEventKind::DispatchRejected,
            OperationalEventSeverity::Error,
        ),
        "dispatch_transport_error" => (
            OperationalEventKind::DispatchTransportError,
            OperationalEventSeverity::Error,
        ),
        _ => return None,
    };
    let summary = if text.trim().is_empty() {
        host_ingress_summary_from_props(props)?
    } else {
        text.to_string()
    };
    let detail = prop_str(props, a2a::REASON);
    let agent_package = prop_str(props, a2a::HOST_INGRESS_TARGET_PACKAGE);
    let agent_instance = prop_str(props, a2a::HOST_INGRESS_TARGET_INSTANCE);
    Some(OperationalEventContent {
        kind,
        severity,
        summary,
        detail,
        agent_package,
        agent_instance_id: agent_instance,
        failure_class: None,
        failure_evidence: None,
        old_status: None,
        new_status: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_summary_shape() {
        let s = format_source_poll_summary(
            "slack",
            "slack:C1",
            "host.source-records.v1",
            "slack:C1",
            2,
        );
        assert!(s.contains("Host source poll"));
        assert!(s.contains("2 record(s)"));
    }
}
