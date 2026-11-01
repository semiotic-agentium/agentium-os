// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! `karak tool build` — derive the descriptor for a v2 TypeScript tool.
//!
//! The tool is a `defineTool(...)` default export (`@karakara/tool`). Build:
//! 1. compile the project to `dist/tool.js` (`npm run build`);
//! 2. run the built tool's `.descriptor()` under node to get the descriptor JSON;
//! 3. parse it into the canonical [`UnifiedToolDescriptor`] — this *validates*
//!    the SDK output against the host's one true descriptor type (parity, §P1.1);
//! 4. compute `descriptor_digest` with the platform digest fn and write
//!    `tool-descriptor.json` + `tool.lock.json`.
//!
//! Steps 1-2 need `node` + an installed `@karakara/tool`; step 3-4 are pure and
//! unit-tested. The derived requests-only manifest is the descriptor's
//! `requested` block plus name/version — no hand-authored manifest.

use std::{path::Path, process::Command};

use anyhow::{Context, Result, anyhow, bail};
use baml_rt_tools::external_tools::{UnifiedToolDescriptor, compute_descriptor_digest};
use console::style;

pub fn run(dir: &str) -> Result<()> {
    let dir = Path::new(dir);
    if !dir.is_dir() {
        bail!("Error: not a directory: {}", dir.display());
    }
    ensure_built(dir)?;
    let descriptor_json = extract_descriptor_json(dir)?;
    let (digest, pretty) = finish_build(&descriptor_json)?;

    let desc_path = dir.join("tool-descriptor.json");
    std::fs::write(&desc_path, &pretty)
        .with_context(|| format!("writing {}", desc_path.display()))?;
    let lock = serde_json::json!({ "descriptor_digest": digest.to_string() });
    let lock_path = dir.join("tool.lock.json");
    std::fs::write(
        &lock_path,
        format!("{}\n", serde_json::to_string_pretty(&lock)?),
    )
    .with_context(|| format!("writing {}", lock_path.display()))?;

    println!(
        "{} built {}",
        style("✓").green(),
        style(dir.display()).cyan()
    );
    println!("  descriptor: {}", style(desc_path.display()).cyan());
    println!("  digest:     {}", style(digest.to_string()).cyan());
    Ok(())
}

/// Pure core: validate the SDK's descriptor JSON against the canonical type and
/// compute its digest. Returns (digest, pretty-printed canonical JSON).
pub fn finish_build(
    descriptor_json: &str,
) -> Result<(baml_rt_tools::mcp_snapshot::Digest, String)> {
    let descriptor: UnifiedToolDescriptor = serde_json::from_str(descriptor_json).context(
        "the SDK descriptor did not match the canonical ToolDescriptor (parity check failed)",
    )?;
    let digest = compute_descriptor_digest(&descriptor);
    let pretty = format!("{}\n", serde_json::to_string_pretty(&descriptor)?);
    Ok((digest, pretty))
}

/// Compile the project if `dist/tool.js` is missing or stale-by-absence.
pub(crate) fn ensure_built(dir: &Path) -> Result<()> {
    if dir.join("dist/tool.js").is_file() {
        return Ok(());
    }
    rebuild(dir)
}

/// Recompile even when an older `dist/tool.js` exists. Publish uses this so
/// the bundle always covers the current source tree.
pub(crate) fn rebuild(dir: &Path) -> Result<()> {
    if !dir.join("node_modules").is_dir() {
        bail!(
            "Error: dependencies not installed in {}.\nHint: run `npm install` first (the @karakara/tool SDK must be resolvable).",
            dir.display()
        );
    }
    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(dir)
        .status()
        .with_context(|| "running `npm run build` (is node/npm on PATH?)")?;
    if !status.success() {
        bail!("Error: `npm run build` failed in {}", dir.display());
    }
    if !dir.join("dist/tool.js").is_file() {
        bail!(
            "Error: build did not produce dist/tool.js in {}.\nHint: check tsconfig outDir/rootDir.",
            dir.display()
        );
    }
    Ok(())
}

/// Run node to print `defineTool(...).descriptor()` as JSON.
pub(crate) fn extract_descriptor_json(dir: &Path) -> Result<String> {
    let entry = dir.join("dist/tool.js").canonicalize()?;
    let entry_url = format!("file://{}", entry.display());
    let script = format!(
        "import(\"{entry_url}\").then((m) => {{ \
           const t = m.default; \
           if (!t || typeof t.descriptor !== \"function\") {{ \
             console.error(\"tool default export is not a defineTool() result\"); process.exit(2); \
           }} \
           process.stdout.write(JSON.stringify(t.descriptor())); \
         }}).catch((e) => {{ console.error(String(e)); process.exit(2); }});"
    );
    let out = Command::new("node")
        .args(["--input-type=module", "-e", &script])
        .current_dir(dir)
        .output()
        .with_context(|| "running node to extract the descriptor (is node on PATH?)")?;
    if !out.status.success() {
        return Err(anyhow!(
            "descriptor extraction failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).context("descriptor output was not valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    // A descriptor exactly as the @karakara/tool SDK's `descriptor()` emits it.
    const SDK_DESCRIPTOR: &str = r#"{
      "descriptor_abi": 1,
      "name": "support/weather",
      "version": "1.0.0",
      "description": "Current weather.",
      "input_schema": {"type": "object"},
      "output_schema": {"type": "object"},
      "error_classes": ["invalid_argument", "transient", "execution"],
      "side_effect": "read",
      "idempotent": true,
      "output_trust": "untrusted",
      "requested": {
        "net": [{"host": "api.open-meteo.com", "port": 443}],
        "secrets": [{"name": "METEO_KEY", "binding": "egress_bound"}]
      }
    }"#;

    #[test]
    fn finish_build_parses_and_digests_sdk_output() {
        let (digest, pretty) = finish_build(SDK_DESCRIPTOR).expect("valid SDK descriptor");
        assert!(digest.to_string().starts_with("sha256:"));
        // Round-trips through the canonical type.
        assert!(pretty.contains("\"name\": \"support/weather\""));
        assert!(pretty.contains("\"side_effect\": \"read\""));
    }

    #[test]
    fn finish_build_is_deterministic() {
        let a = finish_build(SDK_DESCRIPTOR).unwrap().0;
        let b = finish_build(SDK_DESCRIPTOR).unwrap().0;
        assert_eq!(a.to_string(), b.to_string());
    }

    #[test]
    fn finish_build_rejects_non_descriptor_json() {
        assert!(finish_build(r#"{"not": "a descriptor"}"#).is_err());
    }
}
