//! Planner agent template — 3-phase architecture: Intent -> Plan -> Execute -> Present.

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

fn session_plan_type_for_tool_id(tool_id: &str) -> String {
    let parts: Vec<&str> = tool_id.splitn(2, '/').collect();
    if parts.len() == 2 {
        format!(
            "{}{}SessionPlan",
            to_pascal_case(parts[0]),
            to_pascal_case(parts[1])
        )
    } else {
        format!("{}SessionPlan", to_pascal_case(tool_id))
    }
}

/// Generate manifest.json content.
pub fn generate_manifest(
    name: &str,
    description: &str,
    tags: &[String],
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
        tags: tags.to_vec(),
        discovery,
    };

    serde_json::to_string_pretty(&manifest).expect("manifest serializes to JSON")
}

/// Generate the BAML prompt file for a planner agent.
pub fn generate_baml_prompt(prompt_name: &str, tool_ids: &[String]) -> String {
    let pascal_name = to_pascal_case(prompt_name);

    if tool_ids.is_empty() {
        return format!(
            r##"class NeedClarification {{
  question string
}}

class NotRelevant {{
  reason string
}}

class {pascal_name}Intent {{
  intent string
}}

function Infer{pascal_name}Intent(user_message: string) -> NeedClarification | NotRelevant | {pascal_name}Intent {{
  client DefaultClient
  prompt #"
    You classify user intent for a {pascal_name} assistant.

    - Return {pascal_name}Intent when request is relevant.
    - Return NeedClarification only if no actionable topic is present.
    - Return NotRelevant when request is outside this assistant's domain.

    {{{{ ctx.output_format }}}}

    {{{{ _.role('user') }}}}
    {{{{ user_message }}}}
  "#
}}

function Present{pascal_name}ToUser(user_message: string, goal: string) -> StructuredReply {{
  client DefaultClient
  prompt #"
    Produce the final response for the user.
    Return StructuredReply JSON exactly.

    User request: {{{{ user_message }}}}
    Goal: {{{{ goal }}}}

    {{{{ ctx.output_format }}}}
  "#
}}

client DefaultClient {{
  provider openai-generic
  options {{
    model "openai/gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }}
}}
"##,
            pascal_name = pascal_name
        );
    }

    let session_union = tool_ids
        .iter()
        .map(|tool_id| session_plan_type_for_tool_id(tool_id))
        .collect::<Vec<_>>()
        .join(" | ");

    format!(
        r##"/// Phase 1 — Intent inference.
class NeedClarification {{
  question string @description("A clarifying question when the request is too vague to act on.")
}}

class NotRelevant {{
  reason string @description("Why this message is outside this agent's domain.")
}}

class {pascal_name}Intent {{
  intent string @description("Clean, distilled goal statement.")
  operation_kind "read" | "write" | "delete" @description("Broad operation class for planning.")
}}

function Infer{pascal_name}Intent(user_message: string) -> NeedClarification | NotRelevant | {pascal_name}Intent {{
  client DefaultClient
  prompt #"
    You classify user intent for a {pascal_name} assistant.

    Decision rules:
    - Return {pascal_name}Intent when relevant.
    - Return NeedClarification only if essential detail is missing.
    - Return NotRelevant when unrelated.

    {{{{ ctx.output_format }}}}

    {{% if ctx.tags.conversation_history %}}
    {{% for msg in ctx.tags.conversation_history %}}
    {{{{ msg.role }}}}: {{{{ msg.content }}}}
    {{% endfor %}}
    {{% endif %}}

    {{{{ _.role('user') }}}}
    {{{{ user_message }}}}
  "#
}}

/// Phase 2 — Planning.
class {pascal_name}Plan {{
  goal string
  steps {pascal_name}PlanStep[]
}}

class {pascal_name}PlanStep {{
  id string
  description string
  kind "navigate" | "execute" | "synthesize"
}}

function Plan{pascal_name}Work(intent: string, operation_kind: string) -> {pascal_name}Plan {{
  client DefaultClient
  prompt #"
    You are planning work for a {pascal_name} assistant.

    Intent: {{{{ intent }}}}
    Operation kind: {{{{ operation_kind }}}}

    Rules:
    - Return 2-4 concise steps.
    - Always end with a synthesize step.
    - Keep step IDs unique, lowercase, kebab-case.

    {{{{ ctx.output_format }}}}
  "#
}}

/// Phase 3 — Tool execution step.
function Choose{pascal_name}Action(
  goal: string,
  step_description: string,
  operation_kind: string,
) -> {session_union} {{
  client DefaultClient
  prompt #"
    Execute ONE plan step using tools.

    Goal: {{{{ goal }}}}
    Step: {{{{ step_description }}}}
    Operation kind: {{{{ operation_kind }}}}

    {{% if ctx.tags.conversation_history %}}
    {{% for msg in ctx.tags.conversation_history %}}
    {{{{ msg.role }}}}: {{{{ msg.content }}}}
    {{% endfor %}}
    {{% endif %}}

    Return exactly one valid tool-session step object for the current phase.

    Host session tools distinguish archive inspection modes:
    - SearchRead: grep-filtered search (pattern required in step input).
    - PageRead: contiguous paging over rendered archive lines (no grep).

    {{{{ ctx.output_format }}}}
  "#
}}

/// Phase 4 — User-facing synthesis.
function Present{pascal_name}ToUser(
  user_message: string,
  goal: string,
) -> StructuredReply {{
  client DefaultClient
  prompt #"
    You have completed tool execution. Produce the final user-visible answer.
    Return StructuredReply JSON exactly.

    User request: {{{{ user_message }}}}
    Goal: {{{{ goal }}}}

    {{% if ctx.tags.conversation_history %}}
    {{% for msg in ctx.tags.conversation_history %}}
    {{{{ msg.role }}}}: {{{{ msg.content }}}}
    {{% endfor %}}
    {{% endif %}}

    {{{{ ctx.output_format }}}}
  "#
}}

client DefaultClient {{
  provider openai-generic
  options {{
    model "openai/gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }}
}}
"##,
        pascal_name = pascal_name,
        session_union = session_union
    )
}

/// Generate the index.ts file for a planner agent.
pub fn generate_index_ts(prompt_name: &str, tool_ids: &[String]) -> String {
    let pascal_name = to_pascal_case(prompt_name);
    let has_tools = !tool_ids.is_empty();

    format!(
        r##"/// <reference path="./baml-runtime.d.ts" />
import type {{
  ReplyPart,
  RunContext,
  SessionResult,
  StructuredReply,
}} from "./baml-runtime";

const MAX_REACT_STEPS = 10;
const MAX_CLARIFY = 2;

type NeedClarification = {{ question: string }};
type NotRelevant = {{ reason: string }};
type {pascal_name}Intent = {{ intent: string; operation_kind?: "read" | "write" | "delete" }};
type {pascal_name}PlanStep = {{ id: string; description: string; kind: "navigate" | "execute" | "synthesize" }};
type {pascal_name}Plan = {{ goal: string; steps: {pascal_name}PlanStep[] }};

function textReply(text: string): StructuredReply {{
  const parts: ReplyPart[] = [{{ type: "text", text }}];
  return {{ parts, citations: [] }};
}}

function isObject(v: unknown): v is Record<string, unknown> {{
  return v != null && typeof v === "object";
}}

function slugGoal(goal: string): string {{
  return goal.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 48) || "goal";
}}

function isNeedClarification(v: unknown): v is NeedClarification {{
  return isObject(v) && typeof v.question === "string" && v.question.trim().length > 0;
}}

function isNotRelevant(v: unknown): v is NotRelevant {{
  return isObject(v) && typeof v.reason === "string";
}}

function is{pascal_name}Intent(v: unknown): v is {pascal_name}Intent {{
  return isObject(v) && typeof v.intent === "string" && v.intent.trim().length > 0;
}}

function is{pascal_name}Plan(v: unknown): v is {pascal_name}Plan {{
  return isObject(v) && typeof v.goal === "string" && Array.isArray(v.steps);
}}

{run_plan_function}

__chat_register({{
  run: async (ctx) => {{
    const originalText = typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : "unknown";
    let text = originalText;

    let resolvedIntent: {pascal_name}Intent | null = null;
    for (let i = 0; i <= MAX_CLARIFY; i++) {{
      const intentResult = await Infer{pascal_name}Intent({{ user_message: text }});

      if (is{pascal_name}Intent(intentResult)) {{
        resolvedIntent = intentResult;
        break;
      }}
      if (isNotRelevant(intentResult)) {{
        return {{ message: textReply(`This request is not relevant to this agent: ${{intentResult.reason}}`) }};
      }}
      if (isNeedClarification(intentResult) && i < MAX_CLARIFY) {{
        const reply = await ctx.emit.awaitInput(intentResult.question);
        const clarifiedText = messageText(reply).trim();
        if (clarifiedText) text = clarifiedText;
      }} else {{
        resolvedIntent = {{ intent: text, operation_kind: "read" }};
        break;
      }}
    }}

    if (!resolvedIntent) return {{ error: "Could not determine a valid intent." }};

    const planResult = await Plan{pascal_name}Work({{
      intent: resolvedIntent.intent,
      operation_kind: resolvedIntent.operation_kind || "read",
    }});

    const plan: {pascal_name}Plan = is{pascal_name}Plan(planResult) ? planResult : {{
      goal: resolvedIntent.intent,
      steps: [
        {{ id: "step-execute", description: "Execute the request with available tools.", kind: "execute" }},
        {{ id: "step-synthesize", description: "Synthesize the final response.", kind: "synthesize" }},
      ],
    }};

    return run{pascal_name}Plan(ctx, text, plan, resolvedIntent.operation_kind || "read");
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
            r##"async function run{pascal_name}Plan(
  _ctx: RunContext,
  userText: string,
  plan: {pascal_name}Plan,
  operationKind: string,
): Promise<SessionResult> {{
  const {{ goal, steps }} = plan;

  const executionSession = typeof openA2aExecutionSession === "function"
    ? await openA2aExecutionSession("planner-" + Date.now().toString())
    : null;
  const intentId = "intent-" + slugGoal(goal);

  const intentPhase = executionSession
    ? await executionSession.submitIntent({{ intentId, description: goal, citations: [] }})
    : null;
  const executable = intentPhase
    ? await intentPhase.submitPlan({{
        intentId,
        planId: "plan-" + slugGoal(goal),
        steps: steps.map((s, i) => ({{
          stepId: s.id,
          description: s.description,
          order: i,
          dependsOn: i > 0 ? [steps[i - 1]!.id] : [],
        }})),
      }})
    : null;

  try {{
    for (const toolStep of steps.filter((s) => s.kind !== "synthesize")) {{
      if (executable) await executable.startStep?.(toolStep.id, ["#1"]);

      await runGeneratedStepExecutor("Choose{pascal_name}Action", {{
        goal,
        step_description: toolStep.description,
        operation_kind: operationKind,
      }}, {{ max_steps: MAX_REACT_STEPS }});

      if (executable) await executable.completeStep?.(toolStep.id, ["#1"]);
    }}

    const synthStep = steps.find((s) => s.kind === "synthesize");
    if (synthStep && executable) await executable.startStep?.(synthStep.id, ["#1"]);

    let finalMessage: StructuredReply;
    try {{
      finalMessage = await Present{pascal_name}ToUser({{ user_message: userText, goal }});
    }} catch (_) {{
      finalMessage = textReply("Completed execution, but synthesis failed.");
    }}

    if (synthStep && executable) await executable.completeStep?.(synthStep.id, ["#1"]);
    if (executable) await executable.finish?.();

    return {{ message: finalMessage }};
  }} catch (e) {{
    const errMsg = e instanceof Error ? e.message : String(e);
    try {{ if (executable) await executable.abort?.(errMsg); }} catch (_) {{}}
    return {{ error: `Agent error: ${{errMsg}}` }};
  }}
}}"##,
            pascal_name = pascal_name
        )
    } else {
        format!(
            r##"async function run{pascal_name}Plan(
  _ctx: RunContext,
  userText: string,
  plan: {pascal_name}Plan,
  _operationKind: string,
): Promise<SessionResult> {{
  const goal = plan.goal || userText;
  const message = await Present{pascal_name}ToUser({{ user_message: userText, goal }});
  return {{ message }};
}}"##,
            pascal_name = pascal_name
        )
    }
}
