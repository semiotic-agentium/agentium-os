use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    ToolCapability, ToolFailure, ToolHandler, ToolSession, ToolSessionError, ToolStep,
    tools::{ToolFunctionMetadata, ToolSessionContext},
};
use serde_json::Value;

use crate::{
    metadata::system_workflow_routing_metadata,
    tools::{
        WorkflowRoutingConfig, WorkflowRoutingNextOutput, WorkflowRoutingOpenInput,
        WorkflowRoutingRule, WorkflowRoutingSendInput,
    },
};

fn normalize_non_empty_strings(values: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .filter_map(|value| {
            let normalized = value.trim();
            if normalized.is_empty() || !seen.insert(normalized.to_string()) {
                return None;
            }
            Some(normalized.to_string())
        })
        .collect()
}

fn rule_matches(rule: &WorkflowRoutingRule, input: &WorkflowRoutingSendInput) -> bool {
    if !rule.decision_kinds.is_empty() && !rule.decision_kinds.contains(&input.decision_kind) {
        return false;
    }
    if !rule.source_kinds.is_empty() && !rule.source_kinds.contains(&input.source_kind) {
        return false;
    }
    if !rule.project_keys.is_empty() {
        let Some(project_key) = input.project_key.as_ref() else {
            return false;
        };
        if !rule
            .project_keys
            .iter()
            .any(|candidate| candidate == project_key)
        {
            return false;
        }
    }
    if !rule.source_keys.is_empty()
        && !rule
            .source_keys
            .iter()
            .any(|candidate| candidate == &input.source_key)
    {
        return false;
    }
    if !rule.source_key_prefixes.is_empty()
        && !rule
            .source_key_prefixes
            .iter()
            .any(|prefix| input.source_key.starts_with(prefix))
    {
        return false;
    }
    true
}

fn resolve_route<'a>(
    config: &'a WorkflowRoutingConfig,
    input: &WorkflowRoutingSendInput,
) -> Option<&'a WorkflowRoutingRule> {
    config
        .routes
        .iter()
        .find(|rule| rule_matches(rule, input))
        .or(config.default_route.as_ref())
}

fn route_to_output(
    rule: &WorkflowRoutingRule,
) -> std::result::Result<WorkflowRoutingNextOutput, ToolSessionError> {
    let required_capabilities = normalize_non_empty_strings(&rule.required_capabilities);
    if required_capabilities.is_empty() {
        return Err(ToolSessionError::Tool(ToolFailure::from_error(
            &BamlRtError::InvalidArgument(
                "workflow_routing matched a rule with no required capabilities".to_string(),
            ),
        )));
    }
    Ok(WorkflowRoutingNextOutput {
        required_capabilities,
        preferred_agent_package: rule
            .preferred_agent_package
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        matched_rule: rule.name.clone(),
        done: true,
    })
}

struct WorkflowRoutingSession {
    config: WorkflowRoutingConfig,
    pending: Option<Value>,
}

#[async_trait]
impl ToolSession for WorkflowRoutingSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        let send: WorkflowRoutingSendInput = serde_json::from_value(input)
            .map_err(|e| ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e))))?;
        let Some(rule) = resolve_route(&self.config, &send) else {
            return Err(ToolSessionError::Tool(ToolFailure::from_error(
                &BamlRtError::InvalidArgument(format!(
                    "workflow_routing found no route for decision={} source={} project={}",
                    serde_json::to_string(&send.decision_kind)
                        .unwrap_or_else(|_| "unknown".to_string()),
                    serde_json::to_string(&send.source_kind)
                        .unwrap_or_else(|_| "unknown".to_string()),
                    send.project_key.as_deref().unwrap_or(""),
                )),
            )));
        };
        let output = route_to_output(rule)?;
        self.pending =
            Some(serde_json::to_value(output).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::from_error(&BamlRtError::Json(e)))
            })?);
        Ok(())
    }

    async fn next(&mut self) -> std::result::Result<ToolStep, ToolSessionError> {
        let payload = self.pending.take().unwrap_or(Value::Null);
        Ok(ToolStep::Done {
            output: Some(payload),
        })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        self.pending = None;
        Ok(())
    }
}

struct WorkflowRoutingTool {
    metadata: ToolFunctionMetadata,
}

#[async_trait]
impl ToolHandler for WorkflowRoutingTool {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::Streaming
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        let _: WorkflowRoutingOpenInput =
            serde_json::from_value(open_input).map_err(BamlRtError::Json)?;
        let config_value = ctx.config.unwrap_or_else(|| {
            serde_json::to_value(WorkflowRoutingConfig::default())
                .expect("workflow routing config default must serialize")
        });
        let config: WorkflowRoutingConfig =
            serde_json::from_value(config_value).map_err(|error| {
                BamlRtError::InvalidArgument(format!("Invalid workflow_routing config: {error}"))
            })?;
        Ok(Box::new(WorkflowRoutingSession {
            config,
            pending: None,
        }))
    }
}

pub fn workflow_routing_handler() -> Arc<dyn ToolHandler> {
    Arc::new(WorkflowRoutingTool {
        metadata: system_workflow_routing_metadata(),
    })
}

#[cfg(test)]
mod tests {
    use super::{resolve_route, route_to_output};
    use crate::tools::{
        WorkflowRoutingConfig, WorkflowRoutingDecisionKind, WorkflowRoutingRule,
        WorkflowRoutingSendInput, WorkflowRoutingSourceKind,
    };

    #[test]
    fn default_config_routes_slack_pm_work_to_clickup() {
        let config = WorkflowRoutingConfig::default();
        let input = WorkflowRoutingSendInput {
            decision_kind: WorkflowRoutingDecisionKind::CreatePmWork,
            source_kind: WorkflowRoutingSourceKind::Slack,
            source_key: "slack:channel".to_string(),
            project_key: Some("agent-platform".to_string()),
        };

        let route = resolve_route(&config, &input).expect("default route");
        let output = route_to_output(route).expect("valid route output");
        assert_eq!(output.required_capabilities, vec!["clickup:create-task"]);
        assert_eq!(
            output.preferred_agent_package.as_deref(),
            Some("clickup-agent")
        );
    }

    #[test]
    fn custom_project_rule_overrides_default() {
        let config = WorkflowRoutingConfig {
            routes: vec![
                WorkflowRoutingRule {
                    name: Some("linear-override".to_string()),
                    decision_kinds: vec![WorkflowRoutingDecisionKind::CreatePmWork],
                    source_kinds: vec![WorkflowRoutingSourceKind::Slack],
                    project_keys: vec!["agent-platform".to_string()],
                    required_capabilities: vec!["linear:create-task".to_string()],
                    preferred_agent_package: Some("linear-agent".to_string()),
                    ..WorkflowRoutingRule::default()
                },
                WorkflowRoutingRule {
                    name: Some("clickup-default".to_string()),
                    decision_kinds: vec![WorkflowRoutingDecisionKind::CreatePmWork],
                    source_kinds: vec![WorkflowRoutingSourceKind::Slack],
                    required_capabilities: vec!["clickup:create-task".to_string()],
                    preferred_agent_package: Some("clickup-agent".to_string()),
                    ..WorkflowRoutingRule::default()
                },
            ],
            default_route: None,
        };
        let input = WorkflowRoutingSendInput {
            decision_kind: WorkflowRoutingDecisionKind::CreatePmWork,
            source_kind: WorkflowRoutingSourceKind::Slack,
            source_key: "slack:channel".to_string(),
            project_key: Some("agent-platform".to_string()),
        };

        let route = resolve_route(&config, &input).expect("custom route");
        let output = route_to_output(route).expect("valid route output");
        assert_eq!(output.required_capabilities, vec!["linear:create-task"]);
        assert_eq!(
            output.preferred_agent_package.as_deref(),
            Some("linear-agent")
        );
    }

    #[test]
    fn source_key_prefix_rule_matches_list_specific_routing() {
        let config = WorkflowRoutingConfig {
            routes: vec![WorkflowRoutingRule {
                name: Some("clickup-list-routing".to_string()),
                decision_kinds: vec![WorkflowRoutingDecisionKind::ExecuteExistingWork],
                source_kinds: vec![WorkflowRoutingSourceKind::Clickup],
                source_key_prefixes: vec!["clickup:list:123".to_string()],
                required_capabilities: vec!["coordination:routing".to_string()],
                preferred_agent_package: Some("coordinator-agent".to_string()),
                ..WorkflowRoutingRule::default()
            }],
            default_route: None,
        };
        let input = WorkflowRoutingSendInput {
            decision_kind: WorkflowRoutingDecisionKind::ExecuteExistingWork,
            source_kind: WorkflowRoutingSourceKind::Clickup,
            source_key: "clickup:list:123/task:999".to_string(),
            project_key: None,
        };

        let route = resolve_route(&config, &input).expect("prefix route");
        assert_eq!(route.name.as_deref(), Some("clickup-list-routing"));
    }
}
