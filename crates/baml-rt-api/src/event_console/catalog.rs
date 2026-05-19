//! Project tool-owned event source descriptors into Event Console message shapes.

use baml_rt_core::event_subscription::EventSourceKey;
use baml_rt_tools::{EventSourceTypeDescriptor, all_event_source_type_descriptors};

use super::types::{
    AgentDeliverableMessageShape, MessageShapeDeliveryDefaults, MessageShapeFieldGroup,
    MessageShapeSample, MessageShapeUiHints,
};

/// All message shapes derived from linked tool descriptor inventory.
pub fn message_shapes_from_descriptors() -> Vec<AgentDeliverableMessageShape> {
    all_event_source_type_descriptors()
        .into_iter()
        .map(project_descriptor)
        .collect()
}

fn project_descriptor(descriptor: &EventSourceTypeDescriptor) -> AgentDeliverableMessageShape {
    let payload = (descriptor.sample_payload)();
    let source_key = payload
        .get("source")
        .and_then(|s| s.get("source_key"))
        .and_then(|v| v.as_str())
        .and_then(EventSourceKey::parse);

    let ui_hints = if descriptor.wire_schema.as_str()
        == baml_rt_core::host_wire::wire::HOST_SOURCE_RECORDS_V1
    {
        Some(source_records_ui_hints())
    } else {
        None
    };

    AgentDeliverableMessageShape {
        message_shape_id: descriptor.descriptor_id.to_string(),
        display_name: descriptor.display_name.to_string(),
        description: descriptor.description.to_string(),
        origin: descriptor.tool_name.to_string(),
        payload_name: descriptor.payload_name.to_string(),
        wire_schema_version: descriptor.wire_schema.as_str().to_string(),
        source_kind: descriptor.source_kind.as_str().to_string(),
        payload_schema: (descriptor.json_schema)(),
        samples: vec![MessageShapeSample {
            sample_id: format!("{}-default", descriptor.descriptor_id),
            label: descriptor.display_name.to_string(),
            source_key: source_key.map(|k| k.as_str().to_string()),
            payload,
        }],
        delivery_defaults: MessageShapeDeliveryDefaults {
            routing_key: descriptor.default_routing_key.as_str().to_string(),
        },
        ui_hints,
    }
}

fn source_records_ui_hints() -> MessageShapeUiHints {
    MessageShapeUiHints {
        field_labels: [
            (
                "source.source_label".into(),
                "Channel / source label".into(),
            ),
            ("records".into(), "Source records".into()),
        ]
        .into_iter()
        .collect(),
        field_descriptions: Default::default(),
        field_groups: vec![
            MessageShapeFieldGroup {
                title: "Source".into(),
                json_pointers: vec!["/source".into()],
            },
            MessageShapeFieldGroup {
                title: "Records".into(),
                json_pointers: vec!["/records".into()],
            },
        ],
        primary_record_array_pointer: Some("/records".into()),
    }
}
