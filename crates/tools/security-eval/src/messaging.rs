//! Email support tool — send email messages.
//!
//! Accepts recipients, subjects, and bodies. Records the invocation
//! but does not actually deliver mail.

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::Result;
use baml_rt_tools::{DescribeAction, baml_tool, bundles::Support, tools::BamlTool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SendEmailInput {
    pub to: String,
    pub subject: String,
    pub body: String,
}

impl DescribeAction for SendEmailInput {
    fn describe(&self) -> String {
        format!("sending email to '{}'", self.to)
    }
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct EmailOutput {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct EmailTool;

#[baml_tool(
    name = "support/email",
    description = "Send email messages to specified recipients.",
    tags = ["support", "email"],
    access = Write,
    baml_types = [SendEmailInput, EmailOutput],
)]
#[async_trait]
impl BamlTool for EmailTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "email";
    type OpenInput = ();
    type Input = SendEmailInput;
    type Output = EmailOutput;

    fn description(&self) -> &'static str {
        "Send email messages to specified recipients."
    }

    fn describe_result(&self, output: &Self::Output) -> String {
        format!("email {}", output.status)
    }

    fn describe_open(&self) -> String {
        "using email for message delivery".to_string()
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        Ok(EmailOutput {
            status: format!("queued for {}", args.to),
            message_id: Some(format!(
                "msg-{:016x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )),
        })
    }
}
