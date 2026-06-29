// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the stable tool-schema sidecar and prompt layout invariants.
//!
//! Verifies the key properties of the model-facing prompt surface:
//!
//! 1. **Stable sidecar is source-free and IR-derived.**
//! 2. **All manifest tools and shared operation types are represented.**
//! 3. **Generated prompts share the same byte-identical prefix up to `Session history`.**
//! 4. **Per-hop compact contracts live after history, with no phase cue or inline union dump.**

use std::fs;

#[cfg(feature = "dev-tools")]
use baml_rt_builder::builder::baml_gen::GENERATED_BAML_PRELUDE_FILE;
use baml_rt_builder::builder::bootstrap::run_bootstrap;
use tempfile::TempDir;

#[cfg(feature = "dev-tools")]
const CATALOG_SIDECAR: &str = "_baml_tool_schema_catalog.txt";

/// Locate a generated phase function whose name ends in the given suffix (e.g. `__entry`,
/// `__active__support_calculate`). Returns the byte offset of the `function ` keyword.
#[cfg(feature = "dev-tools")]
fn find_phase_function(generated: &str, suffix: &str) -> Option<usize> {
    for (offset, _) in generated.match_indices("function ") {
        let rest = &generated[offset + "function ".len()..];
        let Some(end) = rest.find('(') else { continue };
        let name = rest[..end].trim();
        if name.ends_with(suffix) {
            return Some(offset);
        }
    }
    None
}

/// Slice from the start of one BAML `function ...` declaration up to the next one (or EOF).
#[cfg(feature = "dev-tools")]
fn function_body(generated: &str, start: usize) -> &str {
    let after = start + "function ".len();
    let next = generated[after..]
        .find("\nfunction ")
        .map(|i| after + i)
        .unwrap_or(generated.len());
    &generated[start..next]
}

/// Pretty-print every `function NAME(` occurrence for diagnostic messages.
#[cfg(feature = "dev-tools")]
fn list_functions(generated: &str) -> String {
    let mut out = String::new();
    for (offset, _) in generated.match_indices("function ") {
        let rest = &generated[offset + "function ".len()..];
        if let Some(end) = rest.find('(') {
            out.push_str("- ");
            out.push_str(rest[..end].trim());
            out.push('\n');
        }
    }
    out
}

#[cfg(feature = "dev-tools")]
async fn build_calculator_agent() -> TempDir {
    let root = TempDir::new().unwrap();
    let tools = vec!["support/calculate".to_string()];
    // Name picked to produce a stable, predictable function name (`ChooseCalcAgentTool`).
    run_bootstrap(
        root.path(),
        "Calc Agent",
        "Drives catalog rendering invariants",
        &tools,
    )
    .await
    .expect("bootstrap should produce a runtime + catalog");
    root
}

#[cfg(feature = "dev-tools")]
fn entry_function_body(generated: &str) -> &str {
    let entry_idx = find_phase_function(generated, "__entry").unwrap_or_else(|| {
        panic!(
            "expected an entry phase function (`__entry`) in generated baml; functions found:\n{}",
            list_functions(generated)
        )
    });
    function_body(generated, entry_idx)
}

async fn build_no_tools_agent() -> TempDir {
    let root = TempDir::new().unwrap();
    run_bootstrap(
        root.path(),
        "Plain Agent",
        "Authored non-FSM function — drives universal compositor invariants",
        &[],
    )
    .await
    .expect("bootstrap should produce an authored prompt");
    root
}

fn read_authored_prompt(root: &std::path::Path, prompt_filename: &str) -> String {
    let path = root.join("baml_src").join(prompt_filename);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read authored prompt at {}: {err}", path.display()))
}

#[cfg(feature = "dev-tools")]
fn read_catalog_text(root: &std::path::Path) -> String {
    let path = root.join("baml_src").join(CATALOG_SIDECAR);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read catalog sidecar at {}: {err}", path.display()))
}

#[cfg(feature = "dev-tools")]
fn read_generated_baml(root: &std::path::Path) -> String {
    let path = root.join("baml_src").join(GENERATED_BAML_PRELUDE_FILE);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read generated baml at {}: {err}", path.display()))
}

#[tokio::test]
#[cfg(feature = "dev-tools")]
async fn catalog_text_is_source_free() {
    let root = build_calculator_agent().await;
    let catalog = read_catalog_text(root.path());

    let forbidden = ["function ", "prompt #", "client ", "class ", "ctx.tags"];
    for needle in forbidden {
        assert!(
            !catalog.contains(needle),
            "catalog must contain no raw BAML source token `{needle}` (model-facing schema): \n{catalog}"
        );
    }

    assert!(
        catalog.contains("Generated from compiled BAML IR."),
        "catalog should declare its IR-derived origin: \n{catalog}"
    );
}

#[tokio::test]
#[cfg(feature = "dev-tools")]
async fn catalog_includes_every_manifest_tool_step_kind() {
    let root = build_calculator_agent().await;
    let catalog = read_catalog_text(root.path());

    for ty in [
        "type SupportCalculateOpenStep = {",
        "type SupportCalculateSendStep = {",
        "type SupportCalculateFinishStep = {",
        "type SupportCalculateAbortStep = {",
    ] {
        assert!(
            catalog.contains(ty),
            "catalog must render the stable operation type `{ty}`: \n{catalog}"
        );
    }

    for shared in [
        "type ArchiveSearchReadStep = {",
        "type ArchivePageReadStep = {",
        "type ReadOnlyFinishStep = {",
    ] {
        assert!(
            catalog.contains(shared),
            "catalog must render the shared stable type `{shared}`: \n{catalog}"
        );
    }

    assert!(
        catalog.contains("tool support/calculate"),
        "catalog must reference the manifest tool by qualified name: \n{catalog}"
    );
    assert!(
        catalog.contains(r#"@description("The binary expression to evaluate.")"#),
        "catalog must preserve IR-derived field descriptions instead of flattening to bare types: \n{catalog}"
    );
}

#[tokio::test]
#[cfg(feature = "dev-tools")]
async fn generated_prompts_share_stable_prefix_before_history() {
    let root = build_calculator_agent().await;
    let generated = read_generated_baml(root.path());

    let entry_body = entry_function_body(&generated);
    let active_idx = find_phase_function(&generated, "__active__support_calculate").unwrap_or_else(|| {
        panic!(
            "expected an active phase function for support/calculate in generated baml; functions found:\n{}",
            list_functions(&generated)
        )
    });
    let active_body = function_body(&generated, active_idx);

    let entry_prompt_start = entry_body.find("prompt #\"").expect("entry prompt body");
    let active_prompt_start = active_body.find("prompt #\"").expect("active prompt body");
    let entry_prefix_end = entry_body
        .find("Session history:")
        .expect("entry history marker");
    let active_prefix_end = active_body
        .find("Session history:")
        .expect("active history marker");
    assert_eq!(
        &entry_body[entry_prompt_start..entry_prefix_end],
        &active_body[active_prompt_start..active_prefix_end],
        "entry and active prompts must share the same pre-history prefix"
    );
}

#[tokio::test]
#[cfg(feature = "dev-tools")]
async fn phase_function_prompts_place_contract_after_history_and_task_body() {
    let root = build_calculator_agent().await;
    let generated = read_generated_baml(root.path());
    let entry_body = entry_function_body(&generated);

    let prelude_pos = entry_body
        .find("{% if ctx.tags['tool_schema_prelude'] %}")
        .expect("entry prompt must inject catalog via tool_schema_prelude tag");
    let history_pos = entry_body
        .find("Session history:")
        .expect("entry prompt must include history");
    let task_pos = entry_body
        .find("The user said: { user_message }")
        .expect("entry prompt must include the stripped task body");
    let contract_pos = entry_body
        .find("Return exactly one JSON object of type `ArchivePageReadStep | ArchiveSearchReadStep | ReadOnlyFinishStep | SupportCalculateOpenStep | SupportCalculateSendStep`.")
        .expect("entry prompt must bind the compact contract after history");

    assert!(
        prelude_pos < history_pos,
        "catalog prelude block must precede session history: {entry_body}"
    );
    assert!(
        history_pos < task_pos,
        "session history must precede task body: {entry_body}"
    );
    assert!(
        task_pos < contract_pos,
        "task body must precede the per-hop compact contract: {entry_body}"
    );
    assert!(
        !entry_body.contains("Phase: ENTRY")
            && !entry_body.contains("Narrowed return union for this hop only:"),
        "entry prompt must not contain legacy phase cues or union footers: {entry_body}"
    );
    assert!(
        !entry_body.contains("Answer in JSON using any of these schemas:"),
        "entry prompt must not contain the expanded inline union schema dump: {entry_body}"
    );
}

#[tokio::test]
#[cfg(feature = "dev-tools")]
async fn phase_function_prompts_omit_per_hop_output_format() {
    let root = build_calculator_agent().await;
    let generated = read_generated_baml(root.path());

    let active_idx = find_phase_function(&generated, "__active__support_calculate").unwrap_or_else(|| {
        panic!(
            "expected an active phase function for support/calculate in generated baml; functions found:\n{}",
            list_functions(&generated)
        )
    });
    let active_body = function_body(&generated, active_idx);

    let direct_output_format_count = active_body.matches("{{ ctx.output_format }}").count();
    assert_eq!(
        direct_output_format_count, 0,
        "tool-session phase prompts must not render per-hop ctx.output_format: {active_body}"
    );

    assert!(
        !active_body.contains("Phase: ACTIVE")
            && !active_body.contains("Narrowed return union for this hop only:"),
        "active prompt must not keep the old phase cue or union footer: {active_body}"
    );
    assert!(
        active_body.contains(
            "Return exactly one JSON object of type `SupportCalculateAbortStep | SupportCalculateFinishStep | SupportCalculatePageReadStep | SupportCalculateSearchReadStep | SupportCalculateSendStep`."
        ),
        "active prompt must use the compact type-reference contract: {active_body}"
    );
}

#[tokio::test]
async fn authored_non_fsm_function_gets_canonical_prefix_after_rewrite() {
    let root = build_no_tools_agent().await;
    let authored = read_authored_prompt(root.path(), "plain_agent_prompt.baml");

    assert!(
        authored.contains("Archive refs: `@N`"),
        "authored prompt must open with the canonical archive prefix after the rewriter runs:\n{authored}"
    );
    assert!(
        authored.contains("{% if ctx.tags['tool_schema_prelude'] %}"),
        "authored prompt must include the catalog if-block:\n{authored}"
    );
    assert!(
        authored.contains("{% if ctx.tags['conversation_transcript'] %}"),
        "authored prompt must include the canonical transcript if-block:\n{authored}"
    );
    assert!(
        authored.contains("You are a helpful agent."),
        "author prose must survive the rewrite:\n{authored}"
    );

    let prefix_pos = authored
        .find("Archive refs: `@N`")
        .expect("canonical prefix");
    let history_pos = authored
        .find("{% if ctx.tags['conversation_transcript'] %}")
        .expect("history block");
    let author_pos = authored
        .find("You are a helpful agent.")
        .expect("author body");
    assert!(
        prefix_pos < history_pos && history_pos < author_pos,
        "canonical prefix and history must precede author task body:\n{authored}"
    );
}

#[tokio::test]
async fn authored_non_fsm_function_has_one_canonical_output_format() {
    let root = build_no_tools_agent().await;
    let authored = read_authored_prompt(root.path(), "plain_agent_prompt.baml");

    let count = authored.matches("{{ ctx.output_format }}").count();
    assert_eq!(
        count, 1,
        "exactly one canonical output_format binding must remain in the authored prompt:\n{authored}"
    );

    let of_pos = authored
        .find("{{ ctx.output_format }}")
        .expect("output_format binding");
    let transcript_pos = authored
        .find("{% if ctx.tags['conversation_transcript'] %}")
        .expect("transcript block");
    let author_pos = authored
        .find("You are a helpful agent.")
        .expect("author body");
    assert!(
        transcript_pos < author_pos && author_pos < of_pos,
        "transcript block must precede author body, which must precede the canonical output_format line:\n{authored}"
    );
}

#[tokio::test]
#[cfg(feature = "dev-tools")]
async fn session_plan_parent_function_is_not_rewritten() {
    // Session-plan parents must keep their original `prompt #"..."#` body because their text
    // is inlined into the generated `__entry` / `__active__*` phase executors which already
    // prepend the canonical prefix. Re-prefixing the parent would duplicate the catalog block.
    let root = build_calculator_agent().await;
    let authored = read_authored_prompt(root.path(), "calc_agent_prompt.baml");

    // The bootstrap template places exactly one `{{ ctx.output_format }}` reference inside the
    // parent function. The rewriter must NOT have moved it to the canonical bottom (which would
    // duplicate it) and must NOT have inserted the canonical archive prefix into the parent.
    assert!(
        !authored.contains("{% if ctx.tags['tool_schema_prelude'] %}"),
        "session-plan parent must not have the catalog if-block injected (it is inlined into __entry which already prepends the prefix):\n{authored}"
    );
    let archive_count = authored.matches("Archive: a `tool: @N`").count();
    assert_eq!(
        archive_count, 0,
        "session-plan parent must not have the canonical archive prefix injected:\n{authored}"
    );
}

#[cfg(feature = "dev-tools")]
fn entry_function_body_from_root(root: &std::path::Path) -> String {
    let generated = read_generated_baml(root);
    entry_function_body(&generated).to_string()
}

#[tokio::test]
#[cfg(feature = "dev-tools")]
async fn entry_function_return_union_includes_send_step() {
    let root = build_calculator_agent().await;
    let entry_body = entry_function_body_from_root(root.path());

    assert!(
        entry_body.contains(
            ") -> ArchivePageReadStep | ArchiveSearchReadStep | ReadOnlyFinishStep | SupportCalculateOpenStep | SupportCalculateSendStep {"
        ),
        "entry function signature must include SupportCalculateSendStep alongside Open: {entry_body}"
    );
}

#[tokio::test]
#[cfg(feature = "dev-tools")]
async fn entry_prompt_discriminator_includes_send() {
    let root = build_calculator_agent().await;
    let entry_body = entry_function_body_from_root(root.path());

    assert!(
        entry_body.contains(
            r#"Use `op` as the discriminator: "Open" | "PageRead" | "ReadOnlyFinish" | "SearchRead" | "Send"."#
        ),
        "entry compact contract must expose Send as a legal discriminator: {entry_body}"
    );
}

#[tokio::test]
#[cfg(feature = "dev-tools")]
async fn entry_prompt_phase_policy_allows_send_for_eligible_tools() {
    let root = build_calculator_agent().await;
    let entry_body = entry_function_body_from_root(root.path());

    assert!(
        entry_body.contains(
            "eligible one-shot tools may emit Send directly (runtime auto-opens and auto-finishes)"
        ),
        "entry phase policy must describe Send-inclusive entry hops: {entry_body}"
    );
    assert!(
        !entry_body.contains("this entry return union excludes `Send`"),
        "entry phase policy must not claim Send is excluded when SendStep is in the union: {entry_body}"
    );
}

#[tokio::test]
#[cfg(feature = "dev-tools")]
async fn entry_union_always_includes_archive_reads_and_read_only_finish() {
    let root = build_calculator_agent().await;
    let entry_body = entry_function_body_from_root(root.path());

    for ty in [
        "ArchivePageReadStep",
        "ArchiveSearchReadStep",
        "ReadOnlyFinishStep",
    ] {
        assert!(
            entry_body.contains(ty),
            "entry union must retain shared archive/read-only step `{ty}`: {entry_body}"
        );
    }
}
