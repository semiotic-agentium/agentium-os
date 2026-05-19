//! Inventory-backed event source type descriptors (schema authority for host event kinds).

use baml_rt_core::{AgentDispatchRoutingKey, EventSchemaVersion, EventSourceKind};
use serde_json::Value;

/// Rich metadata for one `(wire_schema, source_kind)` event source declared by a tool.
#[derive(Debug, Clone)]
pub struct EventSourceTypeDescriptor {
    /// Stable operator-console id (e.g. `clickup-source-records`).
    pub descriptor_id: &'static str,
    /// Tool inventory name (e.g. `support/clickup`).
    pub tool_name: &'static str,
    pub source_kind: EventSourceKind,
    pub wire_schema: EventSchemaVersion,
    pub default_routing_key: AgentDispatchRoutingKey,
    pub display_name: &'static str,
    pub description: &'static str,
    pub payload_name: &'static str,
    pub json_schema: fn() -> Value,
    pub sample_payload: fn() -> Value,
}

/// Registers [`EventSourceTypeDescriptor`] rows for a tool crate.
pub struct EventSourceTypeDescriptorProvider {
    pub tool_name: &'static str,
    pub descriptors: fn() -> &'static [EventSourceTypeDescriptor],
}

inventory::collect!(EventSourceTypeDescriptorProvider);

/// All descriptors registered via inventory across linked tool crates.
pub fn all_event_source_type_descriptors() -> Vec<&'static EventSourceTypeDescriptor> {
    inventory::iter::<EventSourceTypeDescriptorProvider>()
        .flat_map(|provider| (provider.descriptors)().iter())
        .collect()
}

/// Find a descriptor by wire schema version and source kind.
pub fn find_event_source_type_descriptor(
    wire_schema_version: &str,
    source_kind: &str,
) -> Option<&'static EventSourceTypeDescriptor> {
    let parsed_kind = EventSourceKind::parse(source_kind)?;
    all_event_source_type_descriptors()
        .into_iter()
        .find(|d| d.wire_schema.as_str() == wire_schema_version && d.source_kind == parsed_kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_returns_none_for_unknown_kind() {
        assert!(
            find_event_source_type_descriptor("host.source-records.v1", "unknown_kind").is_none()
        );
    }
}
