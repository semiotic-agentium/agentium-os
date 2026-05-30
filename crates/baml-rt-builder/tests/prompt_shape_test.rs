// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::fs;

use baml_rt_builder::builder::{
    AgentDir, BuildDir, RuntimeTypeGenerator, bootstrap::run_bootstrap, traits::TypeGenerator,
};
use tempfile::TempDir;

async fn rewrite_prompt(source: &str) -> String {
    let root = TempDir::new().expect("tempdir");
    run_bootstrap(root.path(), "Shape Agent", "prompt shape tests", &[])
        .await
        .expect("bootstrap");

    let prompt_path = root.path().join("baml_src").join("shape_agent_prompt.baml");
    fs::write(&prompt_path, source).expect("write prompt");

    let agent_dir = AgentDir::new(root.path().to_path_buf()).expect("agent dir");
    let build_dir = BuildDir::new().expect("build dir");
    RuntimeTypeGenerator::new()
        .generate(&agent_dir, &build_dir)
        .await
        .expect("generate");

    fs::read_to_string(build_dir.join("baml_src").join("shape_agent_prompt.baml"))
        .expect("read rewritten prompt")
}

const CLIENT_BLOCK: &str = r#"
client DefaultClient {
  provider openai-generic
  options {
    model "gpt-4o-mini"
    base_url "https://openrouter.ai/api/v1"
    api_key env.OPENROUTER_API_KEY
  }
}
"#;

#[tokio::test]
async fn planning_prompt_gets_single_object_selection_hint() {
    let prompt = rewrite_prompt(&format!(
        r##"class PlanResult {{
  objective string
  plan_steps string[]
}}

function BuildPlan(user_message: string) -> PlanResult {{
  client DefaultClient
  prompt #"Plan the work."#
}}

{CLIENT_BLOCK}"##
    ))
    .await;

    assert!(prompt.contains("Return exactly one `PlanResult` JSON object."));
    assert!(prompt.contains("Do not add text before or after the JSON object."));
}

#[tokio::test]
async fn synthesis_prompt_mentions_nested_part_discriminator() {
    let prompt = rewrite_prompt(&format!(
        r##"class ReplyTextPart {{
  type "text"
  text string
}}

class ReplyMarkdownPart {{
  type "markdown"
  markdown string
}}

class SynthesisReply {{
  parts (ReplyTextPart | ReplyMarkdownPart)[]
}}

function Synthesize(user_message: string) -> SynthesisReply {{
  client DefaultClient
  prompt #"Synthesize the result."#
}}

{CLIENT_BLOCK}"##
    ))
    .await;

    assert!(prompt.contains("Return exactly one `SynthesisReply` JSON object."));
    assert!(
        prompt.contains("If `parts[]` is present, choose each item with discriminator `type`:")
    );
    assert!(prompt.contains("`type: \"markdown\"` -> ReplyMarkdownPart"));
}

#[tokio::test]
async fn coordinator_style_intent_prompt_renders_kind_hint_for_agentium_july_case() {
    let prompt = rewrite_prompt(&format!(
        r##"class CoordinatorReadyIntent {{
  kind "ready"
  inferred_intent string
}}

class CoordinatorNeedTaskClarification {{
  kind "clarify"
  question string
}}

class CoordinatorMetaOnly {{
  kind "meta"
  reason string
}}

function ClassifyCoordinatorTurn(user_message: string) -> CoordinatorReadyIntent | CoordinatorNeedTaskClarification | CoordinatorMetaOnly {{
  client DefaultClient
  prompt #"what it was in july"#
}}

{CLIENT_BLOCK}"##
    ))
    .await;

    assert!(prompt.contains("Select the object shape with discriminator `kind`:"));
    assert!(prompt.contains("`kind: \"ready\"` -> CoordinatorReadyIntent"));
    assert!(prompt.contains("`kind: \"clarify\"` -> CoordinatorNeedTaskClarification"));
    assert!(prompt.contains("`kind: \"meta\"` -> CoordinatorMetaOnly"));
    assert!(!prompt.contains("No markdown"));
    assert!(!prompt.contains("no prose"));
}
