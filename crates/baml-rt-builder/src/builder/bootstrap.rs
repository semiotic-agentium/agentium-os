//! Bootstrap a new BAML agent package with interactive TUI.
//!
//! Creates skeletal manifest, BAML prompt, and index.ts; then runs the
//! runtime type generator to produce generated_tools.baml and baml-runtime.d.ts.
//!
//! **Tool catalogue:** The lib does not compose the tool list. The binary
//! (e.g. `baml-agent-builder`) is responsible for building the list of available
//! tools (e.g. from `baml-rt-tools` or another catalogue) and passing the
//! selected tool IDs into [`run_bootstrap`].

use baml_rt_core::{BamlRtError, Result};
use std::fs;
use std::path::Path;

use crate::builder::compiler::RuntimeTypeGenerator;
use crate::builder::traits::TypeGenerator;
use crate::builder::types::BuildDir;

/// Slug for directory/manifest: kebab-case, alphanumeric and hyphens only.
pub fn slug_from_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect();
    s.split_whitespace()
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("-")
}

/// Session plan type name for a tool ID (e.g. "support/calculate" -> "SupportCalculateSessionPlan").
fn session_plan_type_for_tool_id(tool_id: &str) -> String {
    let parts: Vec<&str> = tool_id.splitn(2, '/').collect();
    if parts.len() != 2 {
        return format!("{}SessionPlan", capitalize_first(tool_id));
    }
    let bundle = capitalize_first(parts[0]);
    let local = capitalize_first(parts[1]);
    format!("{}{}SessionPlan", bundle, local)
}

fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(c).collect(),
    }
}

/// Create directory layout and template files, then run the type generator.
pub async fn run_bootstrap(
    root: &Path,
    name: &str,
    description: &str,
    tool_ids: &[String],
) -> Result<()> {
    let slug = slug_from_name(name);
    if slug.is_empty() {
        return Err(BamlRtError::InvalidArgument(
            "Name must contain at least one alphanumeric character".to_string(),
        ));
    }

    let baml_src = root.join("baml_src");
    let src_dir = root.join("src");

    if root.exists() {
        let entries: Vec<_> = fs::read_dir(root).map_err(BamlRtError::Io)?.collect();
        if !entries.is_empty() {
            return Err(BamlRtError::InvalidArgument(format!(
                "Directory already exists and is non-empty: {}",
                root.display()
            )));
        }
    } else {
        fs::create_dir_all(root).map_err(BamlRtError::Io)?;
    }

    fs::create_dir_all(&baml_src).map_err(BamlRtError::Io)?;
    fs::create_dir_all(&src_dir).map_err(BamlRtError::Io)?;

    let prompt_name = slug.replace('-', "_");
    let prompt_file = baml_src.join(format!("{}_prompt.baml", prompt_name));

    let manifest = manifest_json(&slug, description, tool_ids);
    fs::write(root.join("manifest.json"), manifest).map_err(BamlRtError::Io)?;

    let prompt_baml = if tool_ids.is_empty() {
        prompt_template_no_tools(&prompt_name)
    } else {
        let session_plan = session_plan_type_for_tool_id(&tool_ids[0]);
        prompt_template_with_tools(&prompt_name, &session_plan)
    };
    fs::write(&prompt_file, prompt_baml).map_err(BamlRtError::Io)?;

    let index_ts = index_ts_template(&prompt_name, tool_ids.is_empty());
    fs::write(src_dir.join("index.ts"), index_ts).map_err(BamlRtError::Io)?;

    let build_dir = BuildDir::new()?;
    let generator = RuntimeTypeGenerator::new();
    generator.generate(&baml_src, &build_dir).await?;

    let d_ts_src = build_dir.join("dist").join("baml-runtime.d.ts");
    let d_ts_dest = src_dir.join("baml-runtime.d.ts");
    if !d_ts_src.exists() {
        return Err(BamlRtError::InvalidArgument(
            "baml-runtime.d.ts was not generated during bootstrap".to_string(),
        ));
    }
    fs::copy(&d_ts_src, &d_ts_dest).map_err(BamlRtError::Io)?;

    let a2a_ts = include_str!("a2a.ts");
    fs::write(src_dir.join("a2a.ts"), a2a_ts).map_err(BamlRtError::Io)?;

    let tsconfig = r#"{
  "compilerOptions": { "strict": true, "skipLibCheck": true },
  "include": ["src/**/*"]
}
"#;
    fs::write(root.join("tsconfig.json"), tsconfig).map_err(BamlRtError::Io)?;

    Ok(())
}

fn manifest_json(name: &str, description: &str, tools: &[String]) -> String {
    let tools_json = serde_json::to_string(tools).expect("tool ids are valid JSON");
    format!(
        r#"{{"version":"1.0.0","name":"{}","description":"{}","entry_point":"src/index.ts","runtime_version":"0.1.0","tools":{}}}"#,
        name.replace('"', "\\\""),
        description.replace('"', "\\\""),
        tools_json
    )
}

fn prompt_template_no_tools(prompt_name: &str) -> String {
    let fn_name = capitalize_first(&prompt_name.replace('_', " ")).replace(' ', "");
    format!(
        r##"// Agent prompt — edit as needed
function {fn_name}(input: string) -> string {{
  client DefaultClient
  prompt #"
    You are a helpful agent. The user said: {{ input }}
    Reply briefly.
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
        fn_name = fn_name
    )
}

fn prompt_template_with_tools(prompt_name: &str, session_plan_type: &str) -> String {
    let fn_name = format!(
        "Choose{}Tool",
        capitalize_first(&prompt_name.replace('_', " ")).replace(' ', "")
    );
    format!(
        r##"// Agent prompt with tool choice — edit as needed
// (generated_tools.baml is loaded from the same directory by the runtime)

function {fn_name}(user_message: string) -> {session_plan_type} {{
  client DefaultClient
  prompt #"
    The user said: {{ user_message }}
    Decide whether to use a tool and produce a session plan.

    {{ ctx.output_format }}

    {{ _.role('user') }}
    {{ user_message }}
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
        fn_name = fn_name,
        session_plan_type = session_plan_type
    )
}

fn index_ts_template(prompt_name: &str, no_tools: bool) -> String {
    let fn_name = if no_tools {
        capitalize_first(&prompt_name.replace('_', " ")).replace(' ', "")
    } else {
        format!(
            "Choose{}Tool",
            capitalize_first(&prompt_name.replace('_', " ")).replace(' ', "")
        )
    };
    let args = if no_tools {
        "{ input: text }"
    } else {
        "{ user_message: text }"
    };
    format!(
        r#"// @ts-nocheck
// Types from ./a2a.ts (normal TS include).

function extractText(message: unknown): string {{
  if (!message || typeof message !== 'object') return 'unknown';
  const m = message as Record<string, unknown>;
  if (Array.isArray(m.parts) && m.parts.length > 0) {{
    const first = (m.parts as Record<string, unknown>[])[0];
    if (first && typeof first.text === 'string') return first.text;
  }}
  return 'unknown';
}}

async function onChatMessage(message: unknown): Promise<void> {{
  const text = extractText(message);
  const result = await {fn_name}({args});
  __baml_chat_yield({{ message: {{ parts: [{{ text: String(result) }}] }} }});
}}
__baml_chat_register({{ onChatMessage }});
"#,
        fn_name = fn_name,
        args = args
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_from_name_works() {
        assert_eq!(slug_from_name("Voidship Rites"), "voidship-rites");
        assert_eq!(slug_from_name("tony"), "tony");
        assert_eq!(slug_from_name("My Agent 99"), "my-agent-99");
    }

    #[test]
    fn session_plan_type_works() {
        assert_eq!(
            session_plan_type_for_tool_id("support/calculate"),
            "SupportCalculateSessionPlan"
        );
    }
}
