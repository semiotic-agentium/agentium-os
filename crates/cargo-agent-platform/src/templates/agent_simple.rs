//! Simple agent templates.

use baml_rt_core::{AgentManifest, EventSubscription, package::ManifestDiscovery};

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

pub fn generate_manifest(
    name: &str,
    description: &str,
    tags: &[String],
    tool_ids: &[String],
    subscriptions: &[EventSubscription],
) -> String {
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

pub fn generate_baml_prompt(prompt_name: &str, tool_ids: &[String]) -> String {
    let pascal_name = to_pascal_case(prompt_name);

    if tool_ids.is_empty() {
        return format!(
            r##"function Respond{pascal_name}(user_message: string) -> StructuredReply {{
  client DefaultClient
  prompt #"
    You are a concise assistant. Answer the user clearly and directly.
    Return StructuredReply JSON exactly.

    {{{{ ctx.output_format }}}}

    {{{{ _.role('user') }}}}
    {{{{ user_message }}}}
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
        r##"function Choose{pascal_name}Action(user_message: string) -> {session_union} {{
  client DefaultClient
  prompt #"
    You are a tool-using assistant. Execute the user's request via the allowed tools.
    Return one valid tool session step according to the schema.

    {{{{ ctx.output_format }}}}

    {{% if ctx.tags.conversation_transcript %}}
    {{{{ ctx.tags.conversation_transcript }}}}
    {{% endif %}}

    {{{{ _.role('user') }}}}
    {{{{ user_message }}}}
  "#
}}

function Present{pascal_name}Reply(user_message: string) -> StructuredReply {{
  client DefaultClient
  prompt #"
    Synthesize a final user-facing answer from conversation history.
    Return StructuredReply JSON exactly.

    {{{{ ctx.output_format }}}}

    {{% if ctx.tags.conversation_transcript %}}
    {{{{ ctx.tags.conversation_transcript }}}}
    {{% endif %}}

    {{{{ _.role('user') }}}}
    {{{{ user_message }}}}
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

pub fn generate_index_ts(prompt_name: &str, has_tools: bool) -> String {
    let pascal_name = to_pascal_case(prompt_name);
    if !has_tools {
        return format!(
            r#"/// <reference path="./baml-runtime.d.ts" />
import type {{ RunContext, SessionResult }} from "./baml-runtime";

__chat_register({{
  run: async (ctx: RunContext): Promise<SessionResult> => {{
    const userText = typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : "unknown";
    const message = await Respond{pascal_name}({{ user_message: userText }});
    return {{ message }};
  }},
}});
"#,
            pascal_name = pascal_name
        );
    }

    format!(
        r#"/// <reference path="./baml-runtime.d.ts" />
import type {{ RunContext, SessionResult }} from "./baml-runtime";

const MAX_REACT_STEPS = 8;

__chat_register({{
  run: async (ctx: RunContext): Promise<SessionResult> => {{
    const userText = typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : "unknown";

    await runGeneratedStepExecutor("Choose{pascal_name}Action", {{
      user_message: userText,
    }}, {{ max_steps: MAX_REACT_STEPS }});

    const message = await Present{pascal_name}Reply({{ user_message: userText }});
    return {{ message }};
  }},
}});
"#,
        pascal_name = pascal_name
    )
}

/// Generate the canonical tsconfig.json content.
pub fn generate_tsconfig() -> String {
    r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ES2022",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true,
    "outDir": "./dist",
    "rootDir": "./src",
    "declaration": true,
    "declarationMap": true,
    "sourceMap": true,
    "noEmit": true
  },
  "include": ["src/**/*"],
  "exclude": ["node_modules", "dist"]
}
"#
    .to_string()
}
