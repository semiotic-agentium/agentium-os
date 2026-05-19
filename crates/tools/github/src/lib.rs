//! GitHub host tool — `support/github`.
//!
//! Declares GitHub Issues as an event source and owns `host.source-records.v1` payload schemas.

mod event_source_type;
pub mod poll;
mod source_records;

use std::sync::Arc;

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::{
    baml_tool,
    bundles::Support,
    tools::{BamlTool, ToolHandler, create_tool_handler},
};
pub use poll::GithubPollState;
use serde::{Deserialize, Serialize};
pub use source_records::{
    GithubIssueRecord, GithubIssueRecordInput, GithubIssuesProjectContext,
    GithubIssuesSourceRecordsBatch, batch_from_issue_records,
    github_issues_source_records_json_schema,
};

/// Placeholder input for GitHub tool registration (invoke surface expands later).
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct GithubPingInput {}

impl baml_rt_tools::DescribeAction for GithubPingInput {
    fn describe(&self) -> String {
        "github tool ping (not implemented)".to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct GithubPingOutput {
    pub status: String,
}

#[derive(Default)]
pub struct GithubTool;

impl GithubTool {
    pub fn new() -> Self {
        Self
    }
}

fn build_github_tool() -> Result<Arc<dyn ToolHandler>> {
    create_tool_handler(GithubTool::new()).map(|(_, handler)| handler)
}

#[baml_tool(
    name = "support/github",
    description = "GitHub Issues integration: event source schemas and future read APIs.",
    tags = ["support", "github"],
    event_sources = ["github_issues"],
    access = Read,
    baml_types = [GithubPingInput, GithubPingOutput],
    build_with = build_github_tool,
)]
#[async_trait]
impl BamlTool for GithubTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "github";
    type OpenInput = ();
    type Input = GithubPingInput;
    type Output = GithubPingOutput;

    fn description(&self) -> &'static str {
        "GitHub Issues integration: event source schemas and future read APIs."
    }

    async fn execute(&self, _args: Self::Input) -> Result<Self::Output> {
        Err(BamlRtError::InvalidArgument(
            "support/github invoke is not implemented; use task-daemon for Issues polling".into(),
        ))
    }
}
