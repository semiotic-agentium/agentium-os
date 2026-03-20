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

use std::{ffi::OsStr, fs, path::Path};

use crate::builder::{
    a2a_shim_gen::render_a2a_shim,
    error::{BamlBuilderError, Result},
};

/// Strip trailing empty `export {}` / `export {};` that `tsc` may emit for module shape.
///
/// If neither suffix matches, returns `content` unchanged (including internal newlines).
pub fn strip_trailing_empty_export_for_quickjs(content: &str) -> String {
    let trimmed = content.trim_end();
    if trimmed.ends_with("export {};") {
        trimmed
            .strip_suffix("export {};")
            .unwrap_or(trimmed)
            .trim_end()
            .to_string()
    } else if trimmed.ends_with("export {}") {
        trimmed
            .strip_suffix("export {}")
            .unwrap_or(trimmed)
            .trim_end()
            .to_string()
    } else {
        content.to_string()
    }
}

/// Apply QuickJS-oriented transforms: strip empty exports, then prepend A2A shim for entry JS.
///
/// When `is_index` is true, `index_shim` must be the shim source (typically from
/// [`render_a2a_shim`]); tests pass a fixed string instead of calling the generator.
pub fn post_process_js_for_quickjs(
    content: &str,
    is_index: bool,
    index_shim: Option<&str>,
) -> Result<String> {
    let mut out = strip_trailing_empty_export_for_quickjs(content);
    if is_index {
        let shim = index_shim.ok_or_else(|| {
            BamlBuilderError::InvalidArgument(
                "internal: index.js post-process requires A2A shim".to_string(),
            )
        })?;
        out = format!("{}\n{}", shim.trim_end(), out);
    }
    Ok(out)
}

/// Walk `dist_dir`, rewrite each `.js` file in place for QuickJS + A2A shim on `index.js`.
pub fn post_process_dist_dir(dist_dir: &Path) -> Result<()> {
    for path in collect_js_files(dist_dir)? {
        let content = fs::read_to_string(&path)?;
        let is_index = path.file_name() == Some(OsStr::new("index.js"));
        let shim = if is_index {
            Some(render_a2a_shim()?)
        } else {
            None
        };
        let out = post_process_js_for_quickjs(&content, is_index, shim.as_deref())?;
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
    use super::*;

    #[test]
    fn strip_export_semicolon_and_whitespace() {
        let s = "console.log(1);\n\nexport {};\n\n";
        assert_eq!(
            strip_trailing_empty_export_for_quickjs(s),
            "console.log(1);"
        );
    }

    #[test]
    fn strip_export_without_semicolon() {
        let s = "x\nexport {}";
        assert_eq!(strip_trailing_empty_export_for_quickjs(s), "x");
    }

    #[test]
    fn no_strip_when_not_trailing_export() {
        let s = "export {}\nconsole.log(1)";
        assert_eq!(strip_trailing_empty_export_for_quickjs(s), s);
    }

    #[test]
    fn post_process_non_index_no_shim() {
        let s = "a();\nexport {};\n";
        let out = post_process_js_for_quickjs(s, false, None).unwrap();
        assert_eq!(out, "a();");
    }

    #[test]
    fn post_process_index_prepends_shim() {
        let s = "run();\nexport {};\n";
        let out = post_process_js_for_quickjs(s, true, Some("// shim")).unwrap();
        assert_eq!(out, "// shim\nrun();");
    }

    #[test]
    fn post_process_index_requires_shim() {
        let err = post_process_js_for_quickjs("x", true, None).unwrap_err();
        assert!(matches!(err, BamlBuilderError::InvalidArgument(_)));
    }
}
