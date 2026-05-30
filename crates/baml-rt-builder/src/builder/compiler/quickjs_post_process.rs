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
//! - Only the **basename** `index.js` is treated as the entry; nested `index.js` files
//!   use the same rule (consistent with prior behavior).

use std::{borrow::Cow, ffi::OsStr, fs, path::Path};

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

/// Post-process a non-entry JS chunk: strip trailing empty exports only.
pub fn post_process_js_chunk_for_quickjs(content: &str) -> String {
    strip_trailing_empty_export_for_quickjs(content).into_owned()
}

/// Post-process the bundle entry (`index.js`): strip exports, then prepend the A2A shim.
pub fn post_process_js_index_for_quickjs(content: &str, index_shim: &str) -> String {
    let stripped = strip_trailing_empty_export_for_quickjs(content);
    format!("{}\n{}", index_shim.trim_end(), stripped.as_ref())
}

/// Walk `dist_dir`, rewrite each `.js` file in place for QuickJS + A2A shim on `index.js`.
///
/// Visible only within the `compiler` module tree (see [`crate::builder::compiler`]).
pub(in crate::builder::compiler) fn post_process_dist_dir(dist_dir: &Path) -> Result<()> {
    for path in collect_js_files(dist_dir)? {
        let content = fs::read_to_string(&path)?;
        let out = if path.file_name() == Some(OsStr::new("index.js")) {
            post_process_js_index_for_quickjs(&content, render_a2a_shim()?.as_str())
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
    fn post_process_index_prepends_shim() {
        let s = "run();\nexport {};\n";
        let out = post_process_js_index_for_quickjs(s, "// shim");
        assert_eq!(out, "// shim\nrun();");
    }
}
