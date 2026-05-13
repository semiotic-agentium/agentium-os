//! Universal authored-prompt rewriter pipeline.
//!
//! Walks every authored `.baml` file under `build_dir/baml_src`, finds top-level function
//! declarations, and — when permitted by the [`PromptRewritePolicy`] — replaces each function's
//! `prompt #"..."#` body with the canonical structured skeleton from
//! [`crate::builder::baml_gen::PromptCompositor`].
//!
//! Module split:
//! - [`lexer`] — trivia recogniser (comments, strings, raw strings).
//! - [`function_scanner`] — top-level `function NAME(...) -> RET { ... }` parser.
//! - [`policy`] — [`PromptRewritePolicy`] trait and the default implementation.
//! - [`transform`] — splice rewritten prompt body into a function body.
//! - This file (`mod`) — the directory walker and per-file orchestrator.

mod function_scanner;
mod lexer;
pub(crate) mod policy;
mod transform;

use std::{fs, path::Path};

use function_scanner::parse_function_declaration;
use lexer::scan_trivia;
pub(crate) use policy::{DefaultPromptRewritePolicy, PromptRewritePolicy};
use transform::{BodyTransformOutcome, transform_function_body};

use crate::builder::{
    baml_gen::GENERATED_BAML_PRELUDE_FILE,
    compiler::atomic_io::atomic_write,
    error::{BamlBuilderError, Result},
};

/// Aggregate counters returned by [`rewrite_authored_prompts_in_dir`]; useful for build telemetry
/// and for tests asserting that the rewriter touched the expected number of functions.
#[derive(Debug, Default, Clone, Copy)]
pub struct RewriteSummary {
    pub rewritten: usize,
    pub skipped: usize,
    pub no_prompt: usize,
}

impl RewriteSummary {
    fn merge(&mut self, other: RewriteSummary) {
        self.rewritten += other.rewritten;
        self.skipped += other.skipped;
        self.no_prompt += other.no_prompt;
    }
}

/// Walk every authored `.baml` file under `baml_src_build` and rewrite eligible function
/// prompts in place. The generated prelude file (`_baml_runtime.baml`) is excluded.
pub fn rewrite_authored_prompts_in_dir<P: PromptRewritePolicy>(
    baml_src_build: &Path,
    policy: &P,
) -> Result<RewriteSummary> {
    let mut summary = RewriteSummary::default();
    if !baml_src_build.is_dir() {
        return Ok(summary);
    }
    for entry in fs::read_dir(baml_src_build).map_err(BamlBuilderError::Io)? {
        let entry = entry.map_err(BamlBuilderError::Io)?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".baml") {
            continue;
        }
        if name == GENERATED_BAML_PRELUDE_FILE {
            continue;
        }
        let original = fs::read_to_string(&path).map_err(BamlBuilderError::Io)?;
        let (rewritten, file_summary) = rewrite_baml_source(&original, policy);
        summary.merge(file_summary);
        if rewritten != original {
            atomic_write(&path, rewritten.as_bytes())?;
            tracing::debug!(
                path = %path.display(),
                rewrites = file_summary.rewritten,
                skipped = file_summary.skipped,
                "authored prompt rewriter: file updated"
            );
        }
    }
    Ok(summary)
}

/// Pure string transformation: walk one BAML source body, applying the rewriter to every
/// function declaration the policy admits. Returns the new file text plus per-file summary.
pub(crate) fn rewrite_baml_source<P: PromptRewritePolicy>(
    source: &str,
    policy: &P,
) -> (String, RewriteSummary) {
    let mut summary = RewriteSummary::default();
    let mut out = String::with_capacity(source.len() + 256);
    let bytes = source.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if let Some(span) = scan_trivia(bytes, i) {
            let region = std::str::from_utf8(&bytes[i..span.end]).expect("utf8");
            out.push_str(region);
            i = span.end;
            continue;
        }
        if let Some(decl) = parse_function_declaration(bytes, i) {
            out.push_str(&source[i..decl.body_open_brace_inclusive]);
            let body = &source[decl.body_open_brace_inclusive..decl.body_close_brace_exclusive];
            let processed = if policy.should_rewrite_prompt(&decl.fn_name) {
                match transform_function_body(body) {
                    BodyTransformOutcome::Rewritten(s) => {
                        summary.rewritten += 1;
                        s
                    }
                    BodyTransformOutcome::NoPromptLiteral => {
                        summary.no_prompt += 1;
                        body.to_string()
                    }
                }
            } else {
                summary.skipped += 1;
                body.to_string()
            };
            out.push_str(&processed);
            i = decl.body_close_brace_exclusive;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }

    (out, summary)
}

#[cfg(test)]
mod tests {
    use policy::AllowAllPolicy;

    use super::*;
    use crate::builder::baml_gen::CATALOG_FUNCTION_NAME;

    #[test]
    fn rewrites_simple_authored_function() {
        let src = r##"function Greet(name: string) -> string {
  client DefaultClient
  prompt #"
    Hello {{ name }}.
    {{ ctx.output_format }}
  "#
}
"##;
        let (out, summary) = rewrite_baml_source(src, &AllowAllPolicy);
        assert_eq!(summary.rewritten, 1);
        assert!(out.contains("Hello {{ name }}"));
        assert!(out.contains("ctx.tags['tool_schema_prelude']"));
        assert!(out.contains("Session history:"));
        assert_eq!(out.matches("{{ ctx.output_format }}").count(), 1);
    }

    #[test]
    fn handles_multiple_functions_in_one_file() {
        let src = r##"function A(x: string) -> string {
  client C
  prompt #"
    A body.
  "#
}

function B(y: string) -> string {
  client C
  prompt #"
    B body.
  "#
}
"##;
        let (out, summary) = rewrite_baml_source(src, &AllowAllPolicy);
        assert_eq!(summary.rewritten, 2);
        assert!(out.contains("A body"));
        assert!(out.contains("B body"));
    }

    #[test]
    fn ignores_classes_and_clients() {
        let src = r##"class Foo {
  field string
}

client X {
  provider openai
  options { model "gpt" }
}

function F(x: string) -> string {
  client X
  prompt #"
    Body.
  "#
}
"##;
        let (out, summary) = rewrite_baml_source(src, &AllowAllPolicy);
        assert_eq!(summary.rewritten, 1);
        assert!(out.contains("class Foo"));
        assert!(out.contains("client X"));
    }

    #[test]
    fn idempotent_across_rewrites() {
        let src = r##"function F(x: string) -> string {
  client C
  prompt #"
    Body.
  "#
}
"##;
        let (once, _) = rewrite_baml_source(src, &AllowAllPolicy);
        let (twice, _) = rewrite_baml_source(&once, &AllowAllPolicy);
        assert_eq!(once, twice);
    }

    #[test]
    fn function_without_prompt_is_left_alone() {
        let src = r##"function NoPrompt(x: string) -> string {
  client C
}
"##;
        let (out, summary) = rewrite_baml_source(src, &AllowAllPolicy);
        assert_eq!(summary.no_prompt, 1);
        assert_eq!(out, src);
    }

    #[test]
    fn allow_all_excludes_catalog() {
        let src = format!(
            "function {name}(x: string) -> string {{\n  client C\n  prompt #\"\n    Catalog body.\n  \"#\n}}\n",
            name = CATALOG_FUNCTION_NAME,
        );
        let (out, summary) = rewrite_baml_source(&src, &AllowAllPolicy);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.rewritten, 0);
        assert_eq!(out, src);
    }
}
