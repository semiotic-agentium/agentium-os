//! Query constants for context-level token usage analytics.
//!
//! These queries are intentionally read-only and operate over the existing
//! provenance graph schema.

/// Aggregate token + call + duration totals for one context session.
///
/// Parameters:
/// - `$context_id` (String)
pub const SESSION_TOTALS_BY_CONTEXT: &str = r#"
MATCH (c:LlmCall)
WHERE c.a2a_context_id = $context_id
  AND c.a2a_usage_total_tokens IS NOT NULL
RETURN
  coalesce(sum(c.a2a_usage_prompt_tokens), 0) AS tokens_in,
  coalesce(sum(c.a2a_usage_completion_tokens), 0) AS tokens_out,
  coalesce(sum(c.a2a_usage_total_tokens), 0) AS tokens_total,
  count(c) AS llm_call_count,
  coalesce(sum(c.a2a_duration_ms), 0) AS llm_duration_ms_total
"#;

/// Aggregate turn-level token + call + duration totals for one context.
///
/// Turn identity is `(context_id, message_id)` through the
/// `A2AMessageProcessing -> LlmCall` edge.
///
/// Parameters:
/// - `$context_id` (String)
pub const TURN_TOTALS_BY_CONTEXT: &str = r#"
MATCH (m:A2AMessageProcessing)-[:WAS_INVOKED_BY]->(c:LlmCall)
WHERE m.a2a_context_id = $context_id
  AND c.a2a_context_id = $context_id
  AND c.a2a_usage_total_tokens IS NOT NULL
RETURN
  m.a2a_message_id AS message_id,
  coalesce(sum(c.a2a_usage_prompt_tokens), 0) AS tokens_in,
  coalesce(sum(c.a2a_usage_completion_tokens), 0) AS tokens_out,
  coalesce(sum(c.a2a_usage_total_tokens), 0) AS tokens_total,
  count(c) AS llm_call_count,
  coalesce(sum(c.a2a_duration_ms), 0) AS llm_duration_ms_total,
  min(c.a2a_event_id) AS first_event_id
ORDER BY first_event_id
"#;

/// Count user prompts per message id for one context.
///
/// Parameters:
/// - `$context_id` (String)
pub const USER_PROMPTS_BY_CONTEXT: &str = r#"
MATCH (msg:Message)
WHERE msg.a2a_context_id = $context_id
  AND msg.a2a_direction = 'received'
  AND (msg.a2a_role = 'user' OR msg.a2a_role = 'ROLE_USER')
RETURN
  msg.a2a_message_id AS message_id,
  count(msg) AS user_prompt_count
"#;
