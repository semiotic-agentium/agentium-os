//! Agent-deliverable message-shape registry (projected from tool descriptors).

use super::{
    catalog::message_shapes_from_descriptors,
    types::{AgentDeliverableMessageShape, MessageShapeRegistryResponse},
};

/// All registered message shapes exposed by `GET /message-shapes`.
pub fn message_shapes() -> Vec<AgentDeliverableMessageShape> {
    message_shapes_from_descriptors()
}

pub fn registry_response() -> MessageShapeRegistryResponse {
    MessageShapeRegistryResponse {
        items: message_shapes(),
    }
}

pub fn find_message_shape(message_shape_id: &str) -> Option<AgentDeliverableMessageShape> {
    message_shapes()
        .into_iter()
        .find(|s| s.message_shape_id == message_shape_id)
}

pub fn find_message_shape_by_wire(
    wire_schema_version: &str,
    source_kind: Option<&str>,
) -> Option<AgentDeliverableMessageShape> {
    message_shapes().into_iter().find(|s| {
        s.wire_schema_version == wire_schema_version
            && source_kind.is_none_or(|k| s.source_kind == k)
    })
}

pub fn display_label_for_dispatch(
    wire_schema_version: &str,
    source_kind: Option<&str>,
) -> Option<String> {
    find_message_shape_by_wire(wire_schema_version, source_kind).map(|s| s.display_name)
}

#[cfg(test)]
mod tests {
    use baml_tools_clickup::ClickupSourceRecordsBatch;
    use baml_tools_github::GithubIssuesSourceRecordsBatch;
    use baml_tools_slack::SlackNormalizedBatch;
    use baml_tools_system::callback_dispatch_message::SystemCallbackDispatchMessage;

    use super::*;

    #[test]
    fn every_message_shape_has_origin_and_typed_sample() {
        for shape in message_shapes() {
            assert!(
                !shape.origin.is_empty(),
                "shape {} must declare origin",
                shape.message_shape_id
            );
            assert!(
                !shape.samples.is_empty(),
                "shape {} needs samples",
                shape.message_shape_id
            );
            for sample in &shape.samples {
                if shape.message_shape_id == "slack-source-records" {
                    let _: SlackNormalizedBatch = serde_json::from_value(sample.payload.clone())
                        .expect("slack batch sample deserializes");
                } else if shape.message_shape_id == "clickup-source-records" {
                    let _: ClickupSourceRecordsBatch =
                        serde_json::from_value(sample.payload.clone())
                            .expect("clickup batch sample deserializes");
                } else if shape.message_shape_id == "github-issues-source-records" {
                    let _: GithubIssuesSourceRecordsBatch =
                        serde_json::from_value(sample.payload.clone())
                            .expect("github issues batch sample deserializes");
                } else if shape.wire_schema_version == "system.callback.v1" {
                    let _: SystemCallbackDispatchMessage =
                        serde_json::from_value(sample.payload.clone())
                            .expect("callback sample deserializes");
                }
            }
        }
    }

    #[test]
    fn registry_has_source_record_and_callback_shapes() {
        let shapes = message_shapes();
        assert_eq!(shapes.len(), 4);
        let ids: Vec<&str> = shapes.iter().map(|s| s.message_shape_id.as_str()).collect();
        assert!(ids.contains(&"slack-source-records"));
        assert!(ids.contains(&"clickup-source-records"));
        assert!(ids.contains(&"github-issues-source-records"));
        assert!(ids.contains(&"system-callback-token"));
    }

    #[test]
    fn github_schema_is_not_clickup_lifecycle_schema() {
        let github = find_message_shape("github-issues-source-records").expect("github shape");
        let clickup = find_message_shape("clickup-source-records").expect("clickup shape");
        assert_ne!(github.payload_schema, clickup.payload_schema);
        let github_schema = github.payload_schema.to_string();
        assert!(github_schema.contains("GithubIssueRecord"));
        assert!(!github_schema.contains("ClickupLifecycleTaskRecord"));
    }
}
