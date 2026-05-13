//! Integration tests for the agent-wide tool schema catalog.
//!
//! Verifies the four invariants the catalog must hold for the model-facing prompt to remain
//! cacheable, source-free, and IR-derived:
//!
//! 1. **No raw BAML source.** The rendered catalog text contains no `function `, `prompt #`,
//!    `client `, `class `, or `ctx.tags` tokens — those are compiler artefacts, not model input.
//! 2. **All manifest tools represented.** Every tool in the agent manifest has its `*OpenStep`,
//!    `*SendStep`, `*FinishStep`, and `*AbortStep` rendered, plus the shared archive-read and
//!    read-only-finish steps.
//! 3. **Phase prompt at the top.** The generated entry / active phase functions inject the
//!    catalog via `{% if ctx.tags['tool_schema_prelude'] %}` *before* IR task body / session
//!    history / output-format directives.
//! 4. **Narrowed-union footer at the bottom.** The narrowed return union appears after the
//!    session-history Jinja block and is paired with the bottom-of-prompt emit instruction.

use std::fs;

use baml_rt_builder::builder::{baml_gen::GENERATED_BAML_PRELUDE_FILE, bootstrap::run_bootstrap};
use tempfile::TempDir;

const CATALOG_SIDECAR: &str = "_baml_tool_schema_catalog.txt";

/// Locate a generated phase function whose name ends in the given suffix (e.g. `__entry`,
/// `__active__support_calculate`). Returns the byte offset of the `function ` keyword.
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
fn function_body(generated: &str, start: usize) -> &str {
    let after = start + "function ".len();
    let next = generated[after..]
        .find("\nfunction ")
        .map(|i| after + i)
        .unwrap_or(generated.len());
    &generated[start..next]
}

/// Pretty-print every `function NAME(` occurrence for diagnostic messages.
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

fn read_catalog_text(root: &std::path::Path) -> String {
    let path = root.join("baml_src").join(CATALOG_SIDECAR);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read catalog sidecar at {}: {err}", path.display()))
}

fn read_generated_baml(root: &std::path::Path) -> String {
    let path = root.join("baml_src").join(GENERATED_BAML_PRELUDE_FILE);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read generated baml at {}: {err}", path.display()))
}

#[tokio::test]
async fn catalog_text_is_source_free() {
    let root = build_calculator_agent().await;
    let catalog = read_catalog_text(root.path());

    let forbidden = [
        "function ",
        "prompt #",
        "client ",
        "class ",
        "ctx.tags",
        "ctx.output_format",
    ];
    for needle in forbidden {
        assert!(
            !catalog.contains(needle),
            "catalog must contain no raw BAML source token `{needle}` (model-facing schema): \n{catalog}"
        );
    }

    assert!(
        catalog.contains("Answer in JSON"),
        "catalog should be a JSON-shape schema rendered by ctx.output_format: \n{catalog}"
    );
}

#[tokio::test]
async fn catalog_includes_every_manifest_tool_step_kind() {
    let root = build_calculator_agent().await;
    let catalog = read_catalog_text(root.path());

    for op in ["Open", "Send", "Finish", "Abort"] {
        let needle = format!("op: \"{op}\"");
        assert!(
            catalog.contains(&needle),
            "catalog must render the {op} step shape for every manifest tool: \n{catalog}"
        );
    }

    for shared in ["SearchRead", "PageRead", "ReadOnlyFinish"] {
        let needle = format!("op: \"{shared}\"");
        assert!(
            catalog.contains(&needle),
            "catalog must render the shared {shared} archive/read-only step shape: \n{catalog}"
        );
    }

    assert!(
        catalog.contains("\"support/calculate\""),
        "catalog must reference the manifest tool by qualified name in the Open shape: \n{catalog}"
    );
}

#[tokio::test]
async fn phase_function_prompts_inject_catalog_before_ir_body() {
    let root = build_calculator_agent().await;
    let generated = read_generated_baml(root.path());

    let entry_idx = find_phase_function(&generated, "__entry").unwrap_or_else(|| {
        panic!(
            "expected an entry phase function (`__entry`) in generated baml; functions found:\n{}",
            list_functions(&generated)
        )
    });
    let entry_body = function_body(&generated, entry_idx);

    let prelude_pos = entry_body
        .find("{% if ctx.tags['tool_schema_prelude'] %}")
        .expect("entry prompt must inject catalog via tool_schema_prelude tag");
    let cue_pos = entry_body
        .find("Phase: ENTRY")
        .expect("entry prompt must include phase cue");
    let footer_pos = entry_body
        .find("Narrowed return union for this hop only:")
        .expect("entry prompt must contain narrowed-union footer");
    let emit_pos = entry_body
        .find("Emit exactly one JSON object matching one of the named types above")
        .expect("entry prompt must contain emit instruction at bottom");

    assert!(
        prelude_pos < cue_pos,
        "catalog prelude block must precede phase cue (cache prefix discipline): {entry_body}"
    );
    assert!(
        cue_pos < footer_pos,
        "phase cue must precede narrowed-union footer: {entry_body}"
    );
    assert!(
        footer_pos < emit_pos,
        "narrowed-union footer must precede emit instruction: {entry_body}"
    );
}

#[tokio::test]
async fn phase_function_prompts_have_no_per_hop_output_format_for_tool_phases() {
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
        "tool-session phase prompts must not duplicate per-hop ctx.output_format — schemas live in the catalog at the top: {active_body}"
    );

    assert!(
        active_body.contains("Narrowed return union for this hop only:"),
        "active prompt must keep the narrowed-union footer: {active_body}"
    );
}

#[tokio::test]
async fn authored_non_fsm_function_gets_canonical_prefix_after_rewrite() {
    let root = build_no_tools_agent().await;
    let authored = read_authored_prompt(root.path(), "plain_agent_prompt.baml");

    assert!(
        authored.contains("Archive: a `tool: @N`"),
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
        .find("Archive: a `tool: @N`")
        .expect("canonical prefix");
    let author_pos = authored
        .find("You are a helpful agent.")
        .expect("author body");
    assert!(
        prefix_pos < author_pos,
        "canonical prefix must precede author task body:\n{authored}"
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
    assert!(
        transcript_pos < of_pos,
        "transcript block must precede the canonical output_format line:\n{authored}"
    );
}

#[tokio::test]
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

#[tokio::test]
async fn catalog_function_in_generated_baml_uses_output_format_only() {
    let root = build_calculator_agent().await;
    let generated = read_generated_baml(root.path());

    let catalog_fn = "function AgentToolSchemaCatalog__bamlrt";
    let idx = generated
        .find(catalog_fn)
        .expect("synthetic catalog function must be appended to generated baml");
    let tail = &generated[idx..];
    assert!(
        tail.contains("{{ ctx.output_format }}"),
        "catalog function prompt must be exactly `{{{{ ctx.output_format }}}}` so BAML's renderer drives the schema: {tail}"
    );
    let union_pos = tail
        .find("->")
        .expect("catalog function must declare a return union");
    let body_open = tail
        .find('{')
        .expect("catalog function must declare a body");
    assert!(
        union_pos < body_open,
        "catalog return union must precede body in declaration: {tail}"
    );
}
