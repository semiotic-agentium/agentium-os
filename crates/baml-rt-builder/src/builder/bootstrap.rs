//! Bootstrap a new BAML agent package with interactive TUI.
//!
//! Creates skeletal manifest, BAML prompt, and index.ts; then runs the
//! runtime type generator to produce `_baml_runtime.baml` (shared types incl. StructuredReply,
//! plus tool interfaces when manifest lists tools) and baml-runtime.d.ts.
//!
//! **Tool catalogue:** The lib does not compose the tool list. The binary
//! (e.g. `baml-agent-builder`) is responsible for building the list of available
//! tools (e.g. from `baml-rt-tools` or another catalogue) and passing the
//! selected tool IDs into [`run_bootstrap`].

use std::{fs, path::Path};

use baml_rt_core::{AgentManifest, package::ManifestDiscovery};

use crate::builder::{
    compiler::{RuntimeTypeGenerator, write_canonical_tsconfig},
    error::{BamlBuilderError, Result},
    traits::TypeGenerator,
    types::{AgentDir, BuildDir},
};

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
///
/// The generated index.ts includes `/// <reference path="./baml-runtime.d.ts" />` plus
/// explicit type imports so TypeScript resolves BAML function names (e.g. ChooseCalcTool),
/// run-context types, and openToolSession under isolated-module settings.
pub async fn run_bootstrap(
    root: &Path,
    name: &str,
    description: &str,
    tool_ids: &[String],
) -> Result<()> {
    let slug = slug_from_name(name);
    if slug.is_empty() {
        return Err(BamlBuilderError::InvalidArgument(
            "Name must contain at least one alphanumeric character".to_string(),
        ));
    }

    let baml_src = root.join("baml_src");
    let src_dir = root.join("src");

    if root.exists() {
        let entries: Vec<_> = fs::read_dir(root)?.collect();
        if !entries.is_empty() {
            return Err(BamlBuilderError::InvalidArgument(format!(
                "Directory already exists and is non-empty: {}",
                root.display()
            )));
        }
    } else {
        fs::create_dir_all(root)?;
    }

    fs::create_dir_all(&baml_src)?;
    fs::create_dir_all(&src_dir)?;

    let prompt_name = slug.replace('-', "_");
    let prompt_file = baml_src.join(format!("{}_prompt.baml", prompt_name));

    let manifest = manifest_json(&slug, description, tool_ids);
    fs::write(root.join("manifest.json"), manifest)?;

    let prompt_baml = if tool_ids.is_empty() {
        prompt_template_no_tools(&prompt_name)
    } else {
        let session_plan = session_plan_type_for_tool_id(&tool_ids[0]);
        prompt_template_with_tools(&prompt_name, &session_plan)
    };
    fs::write(&prompt_file, prompt_baml)?;

    let index_ts = index_ts_template(&prompt_name, tool_ids.is_empty());
    fs::write(src_dir.join("index.ts"), index_ts)?;

    // Write the canonical tsconfig.json atomically so concurrent bootstrap/build reads
    // never observe a truncated config.
    write_canonical_tsconfig(root)?;

    // Run type generation — writes src/baml-runtime.d.ts and generated BAML files
    let agent_dir = AgentDir::new(root.to_path_buf())?;
    let build_dir = BuildDir::new()?;
    let generator = RuntimeTypeGenerator::new();
    generator.generate(&agent_dir, &build_dir).await?;

    // Copy generated BAML from build_dir/baml_src (includes `_baml_runtime.baml` prelude).
    let baml_src_build = build_dir.join("baml_src");
    if baml_src_build.exists() {
        for entry in fs::read_dir(&baml_src_build).map_err(BamlBuilderError::Io)? {
            let entry = entry.map_err(BamlBuilderError::Io)?;
            let path = entry.path();
            if path.is_file() {
                let dest = baml_src.join(entry.file_name());
                fs::copy(&path, &dest).map_err(BamlBuilderError::Io)?;
            }
        }
    }

    Ok(())
}

fn manifest_json(name: &str, description: &str, tools: &[String]) -> String {
    let discovery = if description.is_empty() {
        None
    } else {
        Some(ManifestDiscovery {
            description: Some(description.to_string()),
            capabilities: Vec::new(),
            subscriptions: Vec::new(),
        })
    };
    let manifest = AgentManifest {
        version: "1.0.0".to_string(),
        name: name.to_string(),
        entry_point: "src/index.ts".to_string(),
        signature: format!("{}@1.0.0", name),
        tools: tools.to_vec(),
        discovery,
    };
    serde_json::to_string_pretty(&manifest).expect("manifest serializes to JSON")
}

fn prompt_template_no_tools(prompt_name: &str) -> String {
    let fn_name = capitalize_first(&prompt_name.replace('_', " ")).replace(' ', "");
    format!(
        r##"// Agent prompt
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
        r##"// Agent prompt with tool choice
// (_baml_runtime.baml is loaded from the same directory by the runtime)

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
        r#"/// <reference path="./baml-runtime.d.ts" />
// Types from baml-runtime.d.ts (bootstrap-generated). DSL-only; no protocol plumbing.
import type {{ RunContext, SessionResult }} from "./baml-runtime";
__chat_register({{
  run: async (ctx: RunContext): Promise<SessionResult> => {{
    const text = typeof ctx.text === "string" && ctx.text.length > 0 ? ctx.text : "unknown";
    const result = await {fn_name}({args});
    return {{ message: String(result) }};
  }},
}});
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
