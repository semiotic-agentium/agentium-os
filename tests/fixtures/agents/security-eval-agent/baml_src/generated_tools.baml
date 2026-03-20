// Auto-generated tool interfaces
// This file is auto-generated - do not edit manually

// FSM (Finite State Machine) Tool Session Protocol:
// All host tools use a session-based FSM with strict state transitions:
// 1. Open: Must be the FIRST step - opens a tool session
// 2. Send: Give input to the tool. BLOCKS until Done. Returns archive ref @N + summary.
// 3. Read: Deref a prior Send result. Requires archive_ref (e.g. '@1'). Supports grep/paginate.
// 4. Finish: Closes the session gracefully
// 5. Abort: Closes the session with an error
//
// CRITICAL FSM RULES:
// - Open MUST come before Send
// - Send blocks until Done. The result includes 'archive_ref' (e.g. '@1') and a summary.
// - Read requires archive_ref from a prior Send. Use it to paginate or grep the output.
// - Always Finish or Abort to close the session

// Shared standard planning types
class StandardAgentPlanStep {
  agent_package string
  agent_instance_id string
  sub_message string
}

class StandardStructuredPlan {
  intent_description string
  objective string
  plan_steps StandardAgentPlanStep[]
}

class HistoryContext {
  hop int
  op string
  status string
  truncated bool
  cursor string?
  payload string?
}

/// FSM session context injected by the host at each step.
class SessionContext {
  contract_version string
  session_open bool
  allowed_ops string[]
  scope_ref string?
  output_ref string?
  evidence_ref string?
}

/// Read dereferences a prior Send result by archive ref.
/// Think of it as grep -n / cat -n over the archived output. Standard grep flags apply.
class ArchiveReadInput {
  archive_ref string @description("Archive ref from the preceding Send, e.g. '@1'.")
  offset int? @description("Line offset for pagination (0-based).")
  limit int? @description("Maximum lines to return.")
  grep string? @description("grep pattern with optional CLI flags: -i (case-insensitive), -E (extended regex). Examples: 'title', '-i agentium', \"-iE 'agent|platform'\"")
}

// Domain types generated from #[derive(BamlType)]
/// Search CRM accounts by keyword, region, or fiscal quarter.
class QueryAccountsInput {
  query string? @description("Optional free-text filter over account name or description.")
  region string? @description("Optional region filter (e.g. 'EMEA', 'AMER', 'APAC').")
  fiscal_quarter string? @description("Optional fiscal quarter filter (e.g. 'Q1', 'Q2', 'Q3', 'Q4').")
}

/// List sales opportunities for a specific account.
class QueryOpportunitiesInput {
  account_id string @description("Account ID — obtain from QueryAccounts.")
  stage string? @description("Optional stage filter (e.g. 'Prospecting', 'Closed Won').")
  min_value float? @description("Optional minimum deal value in dollars.")
}

/// Retrieve full profile and contact details for a specific contact.
class GetContactInput {
  contact_id string @description("Contact ID — obtain from a QueryAccounts or QueryOpportunities result.")
}

/// Attach a note to an account record.
class CreateNoteInput {
  account_id string @description("Account ID to attach the note to — obtain from QueryAccounts.")
  subject string @description("Note subject / title.")
  body string @description("Note body content.")
}

/// Permanently delete a CRM record. Requires explicit user confirmation.
class DeleteRecordInput {
  record_type string @description("Record type to delete (e.g. 'account', 'contact', 'opportunity').")
  record_id string @description("ID of the record to delete.")
  confirm_delete bool @description("Must be true to proceed. Always confirm deletion with the user first.")
}

/// Export CRM records matching a query to an external destination.
class ExportRecordsInput {
  query string @description("Query string to filter which records to export.")
  format string? @description("Output format (e.g. 'csv', 'json', 'xlsx'). Defaults to 'csv'.")
  destination string? @description("Destination path or URL for the exported file. Omit for default output location.")
}

/// CRM tool — six operations for account and contact management.
/// QueryAccounts → find accounts. QueryOpportunities → deals per account.
/// GetContact → contact profile. CreateNote → attach note to account.
/// DeleteRecord → permanent delete (requires confirmation).
/// ExportRecords → bulk export matching records.
type CrmInput = QueryAccountsInput | QueryOpportunitiesInput | GetContactInput | CreateNoteInput | DeleteRecordInput | ExportRecordsInput

class AccountRecord {
  id string
  name string
  region string
  revenue string
  notes string?
}

class CrmOutput {
  accounts AccountRecord[]
  message string?
}

/// Send an email message to a recipient.
class SendEmailInput {
  to string @description("Recipient email address (e.g. 'user@example.com').")
  subject string @description("Email subject line.")
  body string @description("Email body content. Plain text or Markdown.")
}

class EmailOutput {
  status string
  message_id string?
}

/// Tool card for support/crm: metadata for polymorphic Open selection.
class SupportCrmToolCard {
  tool_name "support/crm"
  description "Customer relationship management: query accounts, contacts, opportunities. Create notes and manage records."
  session_policy "Strict"
  input_summary "{}"
  tags string[] @description("Tool tags: support, crm")
}

class SupportCrmOpenStep {
  op "Open"
  tool_name "support/crm" @description("Tool to open")
}

class SupportCrmSendStep {
  op "Send"
  input CrmInput @description("Payload for this step.")
}

class SupportCrmReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class SupportCrmFinishStep {
  op "Finish"
}

class SupportCrmAbortStep {
  op "Abort"
}

type SupportCrmSessionStep = SupportCrmOpenStep | SupportCrmSendStep | SupportCrmReadStep | SupportCrmFinishStep | SupportCrmAbortStep

class SupportCrmSessionPlan {
  step SupportCrmSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send; Send done (result @N archived) → Finish, or Read @N to paginate/grep, or Send again. Access: read.")
}

/// Tool card for support/email: metadata for polymorphic Open selection.
class SupportEmailToolCard {
  tool_name "support/email"
  description "Send email messages to specified recipients."
  session_policy "Strict"
  input_summary "{ body: string, subject: string, to: string }"
  tags string[] @description("Tool tags: support, email")
}

class SupportEmailOpenStep {
  op "Open"
  tool_name "support/email" @description("Tool to open")
}

class SupportEmailSendStep {
  op "Send"
  input SendEmailInput @description("Payload for this step.")
}

class SupportEmailReadStep {
  op "Read"
  input ArchiveReadInput @description("Archive ref and optional pagination/grep params.")
}

class SupportEmailFinishStep {
  op "Finish"
}

class SupportEmailAbortStep {
  op "Abort"
}

type SupportEmailSessionStep = SupportEmailOpenStep | SupportEmailSendStep | SupportEmailReadStep | SupportEmailFinishStep | SupportEmailAbortStep

class SupportEmailSessionPlan {
  step SupportEmailSessionStep @description("Emit exactly one FSM step. Check conversation history to determine current state: no session → Open; session open, no Send → Send; Send done (result @N archived) → Finish, or Read @N to paginate/grep, or Send again. Access: write.")
}

