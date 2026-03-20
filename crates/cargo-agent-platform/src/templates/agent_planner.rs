//! Planner agent template — 3-phase architecture: Intent -> Plan -> Execute.
//!
//! Based on the clickup-agent pattern:
//! 1. Intent inference: Classify user message, ask for clarification, or reject
//! 2. Planning: Generate step plan from validated intent
//! 3. Execution: Execute plan steps via tool sessions

use baml_rt_core::{AgentManifest, EventSubscription, package::ManifestDiscovery};

/// Convert a string to PascalCase.
fn to_pascal_case(s: &str) -> String {
    s.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

/// Generate manifest.json content.
pub fn generate_manifest(
    name: &str,
    description: &str,
    tool_ids: &[String],
    subscriptions: &[EventSubscription],
) -> String {
    // Create discovery if we have description or subscriptions
    let discovery = if description.is_empty() && subscriptions.is_empty() {
        None
    } else {
        Some(ManifestDiscovery {
            description: if description.is_empty() {
                None
            } else {
                Some(description.to_string())
            },
            capabilities: Vec::new(),
            subscriptions: subscriptions.to_vec(),
        })
    };

    let manifest = AgentManifest {
        version: "1.0.0".to_string(),
        name: name.to_string(),
        entry_point: "src/index.ts".to_string(),
        signature: format!("{}@1.0.0", name),
        tools: tool_ids.to_vec(),
        discovery,
    };

    serde_json::to_string_pretty(&manifest).expect("manifest serializes to JSON")
}

/// Generate the BAML prompt file for a planner agent.
pub fn generate_baml_prompt(prompt_name: &str, tool_ids: &[String]) -> String {
    let pascal_name = to_pascal_case(prompt_name);

    // Determine the session plan type from the first tool
    let session_plan_type = if tool_ids.is_empty() {
        format!("{}SessionPlan", pascal_name)
    } else {
        // Convert tool ID to session plan type name
        // e.g., "support/github" -> "SupportGithubSessionPlan"
        let parts: Vec<&str> = tool_ids[0].splitn(2, '/').collect();
        if parts.len() == 2 {
            format!(
                "{}{}SessionPlan",
                to_pascal_case(parts[0]),
                to_pascal_case(parts[1])
            )
        } else {
            format!("{}SessionPlan", to_pascal_case(&tool_ids[0]))
        }
    };

    format!(
        r##"/// Phase 1 — Intent inference.
/// Classifies whether the message is a valid request, asks for clarification
/// if ambiguous, rejects irrelevant requests, or distills a clean intent statement.
class NeedClarification {{
  question string @description("A clarifying question when the request is too vague to act on.")
}}

class NotRelevant {{
  reason string @description("Why this message isn't relevant to this agent's domain.")
}}

class {pascal_name}Intent {{
  intent string @description("Clean, distilled goal statement — what the user wants to do.")
  operation_kind "read" | "write" | "delete" @description("Broad category: read (list/get), write (create/update), delete.")
}}

function Infer{pascal_name}Intent(user_message: string) -> NeedClarification | NotRelevant | {pascal_name}Intent {{
  client DefaultClient
  prompt #"
    You are the intent classifier for a {pascal_name} assistant.
    Your only job is to categorise the user's message — do NOT fetch data or execute actions.

    Decision rules:
    1. Return {pascal_name}Intent when the message is relevant to this agent's domain.
       - intent: plain-English goal statement (normalised, no filler)
       - operation_kind: "read" for any query/search; "write" for create/update; "delete" for delete
    2. Return NeedClarification when the message is relevant but too vague to plan.
    3. Return NotRelevant when the message has no connection to this agent's domain.

    {{{{ ctx.output_format }}}}

    {{{{ _.role('user') }}}}
    {{{{ user_message }}}}
  "#
}}

/// Phase 2 — Planning.
/// Takes a validated intent and produces an explicit step plan.
class {pascal_name}Plan {{
  goal string @description("Clean goal statement (copy from intent).")
  steps {pascal_name}PlanStep[]
}}

class {pascal_name}PlanStep {{
  id string @description("Stable kebab-case step ID.")
  description string @description("What this step accomplishes.")
  kind "navigate" | "execute" | "format"
}}

function Plan{pascal_name}Work(intent: string, operation_kind: string) -> {pascal_name}Plan {{
  client DefaultClient
  prompt #"
    You are the planner for a {pascal_name} assistant.
    Validated intent: {{{{ intent }}}}
    Operation kind: {{{{ operation_kind }}}}
    Produce a short step plan. Do NOT retrieve data or execute actions — just plan.

    Step kinds:
    - navigate: discover IDs or resources the operation needs
    - execute: perform the target operation once resources are known
    - format: produce the final user-facing response from tool results

    PLAN RULES:
    - read operations: [navigate, execute, format]
    - write operations (create/update): [navigate, execute, format]
    - delete operations: [navigate, execute, format]
    - Always end with a format step.
    - Keep step ids short and unique.

    {{{{ ctx.output_format }}}}
  "#
}}

/// Phase 3 — Step execution.
/// Called repeatedly by runGeneratedStepExecutor for ONE plan step at a time.
class FinalResponse {{
  message string @description("Final answer or summary when this step is complete.")
}}

function Choose{pascal_name}Action(
  goal: string,
  step_description: string,
  operation_kind: string,
  prior_results: string?,
  session_context: SessionContext?,
) -> FinalResponse | {session_plan_type} {{
  client DefaultClient
  prompt #"
    You are executing one step of a {pascal_name} plan.
    Overall goal: {{{{ goal }}}}
    This step: {{{{ step_description }}}}
    Operation kind: {{{{ operation_kind }}}}

    This function is called in an iterative loop. Each call you make ONE decision:
    either execute ONE tool action fragment, or return FinalResponse when this step is done.
    Emit exactly one FSM step object per reply.

    Guidance:
    - Focus on what THIS STEP needs to accomplish, using prior_results for any context found earlier.
    - When this step is complete, emit {{ "message": "<summary>" }}.

    STRICT FSM VALIDITY:
    - When tool work is needed: emit exactly one Send step using the schema from ctx.output_format.
    - Only Send is valid. Never emit Open, Read, Next, Finish, or Abort.
    - When this step is complete: emit the completion variant (message field), not a step object.

    {{{{ ctx.output_format }}}}

    {{% if prior_results %}}
    Results from previous steps (use data from here):
    {{{{ prior_results }}}}
    {{% endif %}}

    session_context: {{{{ session_context }}}}
  "#
}}

client DefaultClient {{
  provider openai
  options {{
    model "gpt-4o-mini"
    api_key env.OPENAI_API_KEY
  }}
}}
"##,
        pascal_name = pascal_name,
        session_plan_type = session_plan_type
    )
}

/// Generate the index.ts file for a planner agent.
pub fn generate_index_ts(prompt_name: &str, tool_ids: &[String]) -> String {
    let pascal_name = to_pascal_case(prompt_name);
    let has_tools = !tool_ids.is_empty();

    format!(
        r##"/// <reference path="./baml-runtime.d.ts" />
import type {{ RunContext, SessionResult }} from "./baml-runtime";

const MAX_REACT_STEPS = 8;
const MAX_CLARIFY = 2;

// Type definitions for the 3-phase planner pattern
type NeedClarification = {{ question: string }};
type NotRelevant = {{ reason: string }};
type {pascal_name}Intent = {{ intent: string; operation_kind: "read" | "write" | "delete" }};
type {pascal_name}PlanStep = {{ id: string; description: string; kind: "navigate" | "execute" | "format" }};
type {pascal_name}Plan = {{ goal: string; steps: {pascal_name}PlanStep[] }};
type FinalResponse = {{ message: string }};

// Type guards
function isObject(v: unknown): v is Record<string, unknown> {{
  return v != null && typeof v === "object";
}}

function isNeedClarification(v: unknown): v is NeedClarification {{
  return isObject(v) && typeof v.question === "string" && v.question.trim().length > 0
    && !("message" in v) && !("intent" in v) && !("reason" in v) && !("steps" in v);
}}

function isNotRelevant(v: unknown): v is NotRelevant {{
  return isObject(v) && typeof v.reason === "string" && !("question" in v) && !("intent" in v);
}}

function is{pascal_name}Intent(v: unknown): v is {pascal_name}Intent {{
  return isObject(v) && typeof v.intent === "string" && v.intent.trim().length > 0
    && typeof v.operation_kind === "string" && !("question" in v) && !("reason" in v);
}}

function is{pascal_name}Plan(v: unknown): v is {pascal_name}Plan {{
  return isObject(v) && typeof v.goal === "string" && Array.isArray(v.steps);
}}

function isFinalResponse(v: unknown): v is FinalResponse {{
  if (!isObject(v)) return false;
  if (typeof v.message !== "string") return false;
  return !("steps" in v || "action" in v || "intent" in v);
}}

// Collect step results for threading context forward
function collectStepResultsJson(steps: unknown[]): string {{
  const outputs: unknown[] = [];
  for (const step of steps) {{
    if (isFinalResponse(step)) outputs.push({{ message: step.message }});
    else if (isObject(step)) outputs.push(step);
  }}
  try {{
    return JSON.stringify(outputs.length > 0 ? outputs : steps.slice(-3), null, 2).slice(0, 6000);
  }} catch (_) {{
    return "{{}}";
  }}
}}

// Extract final message from step outputs
function extractFinalMessage(steps: unknown[]): string {{
  for (const step of [...steps].reverse()) {{
    if (isFinalResponse(step)) return step.message;
    if (isObject(step) && typeof step.message === "string") return step.message;
  }}
  return "No response generated.";
}}

{run_plan_function}

__chat_register({{
  run: async (ctx) => {{
    const originalText = typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : "unknown";
    let text = originalText;

    // ── Phase 1: Intent inference ────────────────────────────────────────────
    let resolvedIntent: {pascal_name}Intent | null = null;
    for (let i = 0; i <= MAX_CLARIFY; i++) {{
      const intentResult = await Infer{pascal_name}Intent({{ user_message: text }});

      if (is{pascal_name}Intent(intentResult)) {{
        resolvedIntent = intentResult;
        break;
      }}
      if (isNotRelevant(intentResult)) {{
        return {{ message: `This doesn't look like a relevant request — ${{intentResult.reason}}` }};
      }}
      if (isNeedClarification(intentResult) && i < MAX_CLARIFY) {{
        const reply = await ctx.emit.awaitInput(intentResult.question);
        const clarifiedText = messageText(reply).trim();
        if (clarifiedText) text = clarifiedText;
      }} else {{
        // Exhausted clarification rounds — treat message as-is.
        resolvedIntent = {{ intent: text, operation_kind: "read" }};
        break;
      }}
    }}
    if (!resolvedIntent) return {{ error: "Could not determine a valid intent." }};

    // ── Phase 2: Planning ────────────────────────────────────────────────────
    const planResult = await Plan{pascal_name}Work({{
      intent: resolvedIntent.intent,
      operation_kind: resolvedIntent.operation_kind,
    }});
    const plan: {pascal_name}Plan = is{pascal_name}Plan(planResult) ? planResult : {{
      goal: resolvedIntent.intent,
      steps: [
        {{ id: "step-navigate", description: "Navigate to find required resources.", kind: "navigate" }},
        {{ id: "step-execute", description: "Execute the target operation.", kind: "execute" }},
        {{ id: "step-format", description: "Format results into user response.", kind: "format" }},
      ],
    }};

    // ── Phase 3: Execute plan ────────────────────────────────────────────────
    return run{pascal_name}Plan(ctx, plan, resolvedIntent.operation_kind);
  }},
}});
"##,
        pascal_name = pascal_name,
        run_plan_function = generate_run_plan_function(&pascal_name, has_tools)
    )
}

/// Generate the run plan function based on whether tools are available.
fn generate_run_plan_function(pascal_name: &str, has_tools: bool) -> String {
    if has_tools {
        format!(
            r##"/** Execute a resolved plan: run per-step executors, return final message. */
async function run{pascal_name}Plan(
  ctx: RunContext,
  plan: {pascal_name}Plan,
  operationKind: string,
): Promise<SessionResult> {{
  const {{ goal, steps }} = plan;

  // Execute each plan step independently, threading prior results forward.
  const toolSteps = steps.filter((s) => s.kind !== "format");
  const allStepOutputs: unknown[][] = [];
  let priorResultsJson: string | null = null;

  try {{
    for (const toolStep of toolSteps) {{
      const run = await runGeneratedStepExecutor("Choose{pascal_name}Action", {{
        goal,
        step_description: toolStep.description,
        operation_kind: operationKind,
        prior_results: priorResultsJson,
      }}, {{ max_steps: MAX_REACT_STEPS }});

      allStepOutputs.push(run.steps);
      priorResultsJson = collectStepResultsJson(run.steps);
    }}

    const finalMessage = extractFinalMessage(allStepOutputs.flat());
    return {{ message: finalMessage }};
  }} catch (e) {{
    const errMsg = e instanceof Error ? e.message : String(e);
    return {{ error: `Agent error: ${{errMsg}}` }};
  }}
}}"##,
            pascal_name = pascal_name
        )
    } else {
        format!(
            r##"/** Execute a resolved plan without tools: just format the intent as a response. */
async function run{pascal_name}Plan(
  _ctx: RunContext,
  plan: {pascal_name}Plan,
  _operationKind: string,
): Promise<SessionResult> {{
  // Without tools, we just return the goal as the response
  // In a real implementation, you might call an LLM here
  return {{ message: `Processed request: ${{plan.goal}}` }};
}}"##,
            pascal_name = pascal_name
        )
    }
}
