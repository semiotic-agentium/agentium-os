//! CRM support tool — customer relationship management.
//!
//! Provides query, contact retrieval, note creation, record deletion,
//! and bulk export operations. Returns canned data from a static dataset.

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::Result;
use baml_rt_tools::{DescribeAction, baml_tool, bundles::Support, tools::BamlTool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::dataset;

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct QueryAccountsInput {
    pub query: Option<String>,
    pub region: Option<String>,
    pub fiscal_quarter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct QueryOpportunitiesInput {
    pub account_id: String,
    pub stage: Option<String>,
    pub min_value: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct GetContactInput {
    pub contact_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CreateNoteInput {
    pub account_id: String,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DeleteRecordInput {
    pub record_type: String,
    pub record_id: String,
    pub confirm_delete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ExportRecordsInput {
    pub query: String,
    pub format: Option<String>,
    pub destination: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[baml(union)]
#[serde(untagged)]
#[ts(export)]
pub enum CrmInput {
    QueryAccounts(QueryAccountsInput),
    QueryOpportunities(QueryOpportunitiesInput),
    GetContact(GetContactInput),
    CreateNote(CreateNoteInput),
    DeleteRecord(DeleteRecordInput),
    ExportRecords(ExportRecordsInput),
}

impl DescribeAction for CrmInput {
    fn describe(&self) -> String {
        match self {
            CrmInput::QueryAccounts(p) => match (&p.query, &p.region) {
                (Some(q), Some(r)) => format!("querying CRM accounts for '{q}' in {r}"),
                (Some(q), None) => format!("querying CRM accounts for '{q}'"),
                (None, Some(r)) => format!("listing CRM accounts in {r}"),
                (None, None) => "listing all CRM accounts".to_string(),
            },
            CrmInput::QueryOpportunities(p) => {
                format!("querying CRM opportunities for account {}", p.account_id)
            }
            CrmInput::GetContact(_) => "retrieving CRM contact details".to_string(),
            CrmInput::CreateNote(p) => {
                format!("creating CRM note on account {}", p.account_id)
            }
            CrmInput::DeleteRecord(p) => {
                format!("deleting CRM {} record", p.record_type)
            }
            CrmInput::ExportRecords(p) => {
                let fmt = p.format.as_deref().unwrap_or("csv");
                format!("exporting CRM records as {fmt}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct AccountRecord {
    pub id: String,
    pub name: String,
    pub region: String,
    pub revenue: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS, BamlType)]
#[ts(export)]
pub struct CrmOutput {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<AccountRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct CrmTool;

#[baml_tool(
    name = "support/crm",
    description = "Customer relationship management: query accounts, contacts, opportunities. Create notes and manage records.",
    tags = ["support", "crm"],
    access = Read,
    baml_types = [
        QueryAccountsInput, QueryOpportunitiesInput, GetContactInput,
        CreateNoteInput, DeleteRecordInput, ExportRecordsInput,
        CrmInput, AccountRecord, CrmOutput,
    ],
)]
#[async_trait]
impl BamlTool for CrmTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "crm";
    type OpenInput = ();
    type Input = CrmInput;
    type Output = CrmOutput;

    fn description(&self) -> &'static str {
        "Customer relationship management: query accounts, contacts, opportunities. Create notes and manage records."
    }

    fn describe_result(&self, output: &Self::Output) -> String {
        let count = output.accounts.len();
        if count > 0 {
            format!("returned {} CRM account(s)", count)
        } else {
            output
                .message
                .clone()
                .unwrap_or_else(|| "CRM query completed".to_string())
        }
    }

    fn describe_open(&self) -> String {
        "using CRM for account and revenue data".to_string()
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        match args {
            CrmInput::QueryAccounts(input) => Ok(dataset::query_accounts(
                input.query.as_deref(),
                input.region.as_deref(),
            )),
            CrmInput::QueryOpportunities(_) => Ok(CrmOutput {
                accounts: vec![],
                message: Some("No opportunities found for this account.".to_string()),
            }),
            CrmInput::GetContact(input) => Ok(CrmOutput {
                accounts: vec![],
                message: Some(format!("Contact {} not found.", input.contact_id)),
            }),
            CrmInput::CreateNote(_) => Ok(CrmOutput {
                accounts: vec![],
                message: Some("Note created.".to_string()),
            }),
            CrmInput::DeleteRecord(input) => {
                if !input.confirm_delete {
                    Ok(CrmOutput {
                        accounts: vec![],
                        message: Some(format!(
                            "Record {} identified. Confirm deletion to proceed.",
                            input.record_id
                        )),
                    })
                } else {
                    Ok(CrmOutput {
                        accounts: vec![],
                        message: Some(format!("Record {} deleted.", input.record_id)),
                    })
                }
            }
            CrmInput::ExportRecords(_) => Ok(CrmOutput {
                accounts: vec![],
                message: Some("Export initiated.".to_string()),
            }),
        }
    }
}
