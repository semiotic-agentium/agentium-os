// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Post-process JavaScript emitted by `tsc` so it can run as a script in QuickJS.
//!
//! # Invariants (must hold for packaged agents to load in QuickJS)
//!
//! - QuickJS evaluates **scripts**, not ESM modules; `tsc` may emit a trailing empty
//!   `export {}` / `export {};` which must be stripped or evaluation can fail.
//! - The compiled entry file named `index.js` must be prefixed with the A2A runtime shim
//!   so `__chat_register` and dispatch wiring match what the host expects.
//! - Sibling `./foo` imports in `index.js` are inlined into the entry script so agents may
//!   split TypeScript across modules without a separate bundler step.
//! - Only the **basename** `index.js` is treated as the entry; nested `index.js` files
//!   use the same rule (consistent with prior behavior).

use std::{
    borrow::Cow,
    collections::HashSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use crate::builder::{a2a_shim_gen::render_a2a_shim, error::Result};

/// Strip trailing empty `export {}` / `export {};` that `tsc` may emit for module shape.
///
/// Returns [`Cow::Borrowed`] when no change is needed (no allocation). Otherwise returns
/// an owned string with trailing export removed.
pub fn strip_trailing_empty_export_for_quickjs(content: &str) -> Cow<'_, str> {
    let trimmed = content.trim_end();
    if trimmed.ends_with("export {};") {
        Cow::Owned(
            trimmed
                .strip_suffix("export {};")
                .unwrap_or(trimmed)
                .trim_end()
                .to_string(),
        )
    } else if trimmed.ends_with("export {}") {
        Cow::Owned(
            trimmed
                .strip_suffix("export {}")
                .unwrap_or(trimmed)
                .trim_end()
                .to_string(),
        )
    } else {
        Cow::Borrowed(content)
    }
}

/// Remove ESM `export` keywords from a chunk that will be evaluated as a script.
fn strip_export_keywords_for_quickjs(content: &str) -> String {
    content
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed == "export {};" || trimmed == "export {}" {
                String::new()
            } else if trimmed.starts_with("export {") && !trimmed.contains(" from ") {
                // Named re-exports without a value binding are dropped after sibling inlining.
                String::new()
            } else if trimmed.starts_with("export ") {
                line.replacen("export ", "", 1)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_relative_import_spec(line: &str) -> Option<&str> {
    parse_relative_module_spec(line, "import ")
}

/// `export { ... } from "./sibling"` — inline sibling and drop the re-export line.
fn parse_relative_reexport_from_spec(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if !trimmed.starts_with("export {") {
        return None;
    }
    parse_relative_module_spec(line, "export {")
}

fn parse_relative_module_spec<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = line.trim();
    if !trimmed.starts_with(prefix) {
        return None;
    }
    let from_idx = trimmed.find(" from ")?;
    let spec = trimmed[from_idx + " from ".len()..].trim();
    let spec = spec.strip_suffix(';').unwrap_or(spec).trim();
    let spec = spec
        .strip_prefix('"')
        .or_else(|| spec.strip_prefix('\''))
        .and_then(|s| s.strip_suffix('"').or_else(|| s.strip_suffix('\'')))?;
    spec.starts_with("./").then_some(spec)
}

fn resolve_relative_import(spec: &str, dist_dir: &Path) -> PathBuf {
    let rel = spec.strip_prefix("./").unwrap_or(spec);
    let rel = if rel.ends_with(".js") {
        rel.to_string()
    } else {
        format!("{rel}.js")
    };
    dist_dir.join(rel)
}

/// Inline relative `./` imports so the entry script is self-contained for QuickJS.
pub fn inline_local_imports_for_quickjs(
    content: &str,
    dist_dir: &Path,
    visiting: &mut HashSet<PathBuf>,
) -> Result<String> {
    let mut inlined = String::new();
    let mut body = String::new();

    for line in content.lines() {
        let reexport_or_import =
            parse_relative_import_spec(line).or_else(|| parse_relative_reexport_from_spec(line));
        if let Some(spec) = reexport_or_import {
            let dep_path = resolve_relative_import(spec, dist_dir);
            if dep_path.is_file() && visiting.insert(dep_path.clone()) {
                let dep_content = fs::read_to_string(&dep_path)?;
                let dep_inlined =
                    inline_local_imports_for_quickjs(&dep_content, dist_dir, visiting)?;
                let dep_script = strip_export_keywords_for_quickjs(&dep_inlined);
                let dep_script = strip_trailing_empty_export_for_quickjs(&dep_script);
                if !dep_script.trim().is_empty() {
                    if !inlined.is_empty() {
                        inlined.push('\n');
                    }
                    inlined.push_str(dep_script.as_ref());
                }
                visiting.remove(&dep_path);
            }
            continue;
        }
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(line);
    }

    if inlined.is_empty() {
        Ok(body)
    } else if body.is_empty() {
        Ok(inlined)
    } else {
        Ok(format!("{inlined}\n{body}"))
    }
}

/// Post-process a non-entry JS chunk: strip trailing empty exports only.
pub fn post_process_js_chunk_for_quickjs(content: &str) -> String {
    strip_trailing_empty_export_for_quickjs(content).into_owned()
}

/// Post-process the bundle entry (`index.js`): inline siblings, strip exports, prepend shim.
pub fn post_process_js_index_for_quickjs(
    content: &str,
    index_shim: &str,
    dist_dir: &Path,
) -> Result<String> {
    let inlined = inline_local_imports_for_quickjs(content, dist_dir, &mut HashSet::new())?;
    let stripped = strip_trailing_empty_export_for_quickjs(&inlined);
    Ok(format!("{}\n{}", index_shim.trim_end(), stripped.as_ref()))
}

/// Walk `dist_dir`, rewrite each `.js` file in place for QuickJS + A2A shim on `index.js`.
///
/// Visible only within the `compiler` module tree (see [`crate::builder::compiler`]).
pub(in crate::builder::compiler) fn post_process_dist_dir(dist_dir: &Path) -> Result<()> {
    let index_shim = render_a2a_shim()?;
    for path in collect_js_files(dist_dir)? {
        let content = fs::read_to_string(&path)?;
        let out = if path.file_name() == Some(OsStr::new("index.js")) {
            post_process_js_index_for_quickjs(&content, index_shim.as_str(), dist_dir)?
        } else {
            post_process_js_chunk_for_quickjs(&content)
        };
        fs::write(&path, out)?;
    }
    Ok(())
}

/// Recursively collect `.js` files under a directory.
fn collect_js_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    collect_js_files_recursive(dir, &mut files)?;
    Ok(files)
}

fn collect_js_files_recursive(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_js_files_recursive(&path, files)?;
        } else if path.extension() == Some(OsStr::new("js")) {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    #[test]
    fn strip_export_semicolon_and_whitespace() {
        let s = "console.log(1);\n\nexport {};\n\n";
        assert_eq!(
            strip_trailing_empty_export_for_quickjs(s).as_ref(),
            "console.log(1);"
        );
    }

    #[test]
    fn strip_export_without_semicolon() {
        let s = "x\nexport {}";
        assert_eq!(strip_trailing_empty_export_for_quickjs(s).as_ref(), "x");
    }

    #[test]
    fn no_strip_when_not_trailing_export() {
        let s = "export {}\nconsole.log(1)";
        let cow = strip_trailing_empty_export_for_quickjs(s);
        assert!(matches!(cow, Cow::Borrowed(_)));
        assert_eq!(cow.as_ref(), s);
    }

    #[test]
    fn post_process_chunk_no_shim() {
        let s = "a();\nexport {};\n";
        let out = post_process_js_chunk_for_quickjs(s);
        assert_eq!(out, "a();");
    }

    #[test]
    fn post_process_index_inlines_export_reexport_from_sibling() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("workflow.js"),
            "export const PKG = \"clickup-execute\";\n",
        )
        .expect("write workflow");
        let index = "export { PKG } from \"./workflow\";\nrun();\n";
        let out =
            post_process_js_index_for_quickjs(index, "// shim", dir.path()).expect("post-process");
        assert!(out.contains("const PKG = \"clickup-execute\""));
        assert!(out.contains("run();"));
        assert!(!out.contains(" from \"./workflow\""));
        assert!(!out.contains("{ PKG } from"));
    }

    #[test]
    fn post_process_index_prepends_shim_and_inlines_sibling() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("helper.js"),
            "export async function helper() { return 1; }\n",
        )
        .expect("write helper");
        let index = "import { helper } from \"./helper\";\nrun();\nexport {};\n";
        let out =
            post_process_js_index_for_quickjs(index, "// shim", dir.path()).expect("post-process");
        assert!(out.starts_with("// shim\n"));
        assert!(out.contains("async function helper()"));
        assert!(out.contains("run();"));
        assert!(!out.contains("import "));
    }
}
