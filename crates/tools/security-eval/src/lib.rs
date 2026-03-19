//! Security evaluation tool bundle: CRM + email.
//!
//! Realistic synthetic tools for integration testing of drift scoring
//! and pre-execution tool hijack detection. The CRM tool returns
//! canned business data; the email tool records invocations.

pub mod crm;
pub mod dataset;
pub mod messaging;

pub use crm::{CrmInput, CrmOutput, CrmTool};
pub use messaging::{EmailOutput, EmailTool, SendEmailInput};
