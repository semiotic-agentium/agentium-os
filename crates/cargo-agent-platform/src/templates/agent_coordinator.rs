//! Coordinator agent template — multi-agent delegator pattern.
//!
//! Based on the coordinator-agent pattern:
//! 1. Discover available specialist agents
//! 2. Plan workflow DAG to decompose user requests
//! 3. Execute workflow by delegating to specialists
//! 4. Synthesize results into coherent response

use baml_rt_core::{AgentManifest, package::ManifestDiscovery};

/// Generate manifest.json content for a coordinator agent.
pub fn generate_manifest(
    name: &str,
    description: &str,
    tags: &[String],
    tool_ids: &[String],
) -> String {
    let desc = if description.is_empty() {
        "Coordinator agent for downstream specialist orchestration and synthesis".to_string()
    } else {
        description.to_string()
    };

    let discovery = Some(ManifestDiscovery {
        description: Some(desc),
        capabilities: vec![
            "coordination:routing".to_string(),
            "coordination:synthesis".to_string(),
        ],
        subscriptions: vec![],
    });

    let manifest = AgentManifest {
        version: "1.0.0".to_string(),
        name: name.to_string(),
        entry_point: "src/index.ts".to_string(),
        signature: format!("{}@1.0.0", name),
        tools: tool_ids.to_vec(),
        tags: tags.to_vec(),
        discovery,
    };

    serde_json::to_string_pretty(&manifest).expect("manifest serializes to JSON")
}

/// Generate the planner.baml file for workflow planning.
pub fn generate_planner_baml(_prompt_name: &str) -> String {
    include_str!("coordinator_templates/planner.baml").to_string()
}

/// Generate the coordinator prompt BAML file for synthesis.
pub fn generate_coordinator_baml(_prompt_name: &str) -> String {
    include_str!("coordinator_templates/coordinator_prompt.baml").to_string()
}

/// Generate the index.ts file for a coordinator agent.
pub fn generate_index_ts(agent_package: &str) -> String {
    let template = include_str!("coordinator_templates/index.ts.template");
    template.replace("{{AGENT_NAME}}", agent_package)
}
