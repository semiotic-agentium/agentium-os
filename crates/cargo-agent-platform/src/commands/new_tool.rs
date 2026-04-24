//! `new-tool` subcommand implementation (default, external-tool path).
//!
//! Creates a standalone external tool scaffold in its own directory —
//! not compiled into the platform workspace. The runner picks the tool up via
//! `BAML_EXTERNAL_TOOLS_DIR` at deploy time.
//!
//! For scaffolding a tool *inside* the platform workspace (system bundles,
//! compiled-in integrations), use `new-static-tool` instead.
//!
//! Most stringly-typed knobs were lifted into typed enums
//! ([`Access`], [`Language`]) so clap validates user input and the template
//! dispatcher gets an exhaustive `match`. Bundle stays free-form by design.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use baml_rt_tools::{BundleName, external_tools::SandboxImageRef};
use console::style;

use crate::{
    templates::external_tool::{
        Access, GeneratedFile, Language, Runtime, SandboxSource, ScaffoldContext, metadata_json,
        readme_md,
    },
    transaction::TransactionalWriter,
};

/// Execution mode for the scaffolder. Collapses the two `bool` flags that used
/// to be carried around separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Write files after printing the summary. Default CLI behaviour.
    Apply,
    /// Print the plan, prompt for confirmation before writing.
    Confirm,
    /// Print the plan and stop; never touch the filesystem.
    DryRun,
}

impl RunMode {
    fn shows_summary(self) -> bool {
        matches!(self, Self::Confirm | Self::DryRun)
    }
}

/// Inputs for the `new-tool` scaffolder entrypoint.
#[derive(Debug, Clone)]
pub struct NewToolRunArgs<'a> {
    pub name: &'a str,
    pub bundle: &'a str,
    pub lang: Language,
    pub access: Access,
    pub runtime: Runtime,
    pub sandbox_source: SandboxSource,
    pub sandbox_image: Option<&'a str>,
    pub runtime_digest: Option<&'a str>,
    pub sandbox_entrypoint: &'a [String],
    pub generate_docker: bool,
    pub description: &'a str,
    pub output: Option<&'a str>,
    pub mode: RunMode,
}

pub fn run(args: NewToolRunArgs<'_>) -> Result<()> {
    let NewToolRunArgs {
        name,
        bundle,
        lang,
        access,
        runtime,
        sandbox_source,
        sandbox_image,
        runtime_digest,
        sandbox_entrypoint,
        generate_docker,
        description,
        output,
        mode,
    } = args;

    validate_tool_name(name)?;
    // Reuse the runtime's BundleName rule (non-empty, no '/') so the scaffolder
    // can't produce a tool-metadata.json the runner would later reject.
    BundleName::new(bundle).map_err(|e| {
        anyhow!("Error: invalid bundle '{bundle}': {e}\nHint: use a non-empty name without '/'.")
    })?;

    let output_dir = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(name));
    validate_output_dir(&output_dir)?;

    let default_description = format!("{} external tool for BAML runtime", capitalize_first(name));
    let description = if description.trim().is_empty() {
        default_description.as_str()
    } else {
        description.trim()
    };

    validate_generate_docker_option(runtime, sandbox_source, generate_docker)?;

    let (sandbox_image, runtime_digest) = validate_runtime_options(
        runtime,
        sandbox_source,
        sandbox_image,
        runtime_digest,
        sandbox_entrypoint,
    )?;

    let ctx = ScaffoldContext {
        name,
        bundle,
        access,
        language: lang,
        description,
        runtime,
        sandbox_source: Some(sandbox_source),
        sandbox_image,
        runtime_digest,
        sandbox_entrypoint: sandbox_entrypoint.to_vec(),
        generate_docker,
    };

    let files = build_file_set(&ctx);

    if mode.shows_summary() {
        println!();
        println!("{}", style("Summary:").bold());
        println!("  Name:      {}", style(name).cyan());
        println!("  Tool ID:   {}", style(ctx.tool_id()).cyan());
        println!("  Language:  {}", style(lang.as_str()).cyan());
        println!("  Access:    {}", style(access.as_str()).cyan());
        println!("  Runtime:   {}", style(runtime.as_str()).cyan());
        if runtime == Runtime::Sandbox {
            println!(
                "  Source:    {}",
                style(match ctx.sandbox_source.unwrap_or(SandboxSource::Oci) {
                    SandboxSource::Oci => "oci",
                    SandboxSource::Bind => "bind",
                })
                .cyan()
            );
            println!(
                "  Image:     {}",
                style(format!("{:?}", ctx.sandbox_image)).cyan()
            );
            println!(
                "  Digest:    {}",
                style(ctx.runtime_digest.as_deref().unwrap_or("(missing)")).cyan()
            );
            if !ctx.sandbox_entrypoint.is_empty() {
                println!(
                    "  Entrypoint:{}",
                    style(format!(" {}", ctx.sandbox_entrypoint.join(" "))).cyan()
                );
            }
            if ctx.sandbox_source == Some(SandboxSource::Bind) {
                println!(
                    "  Docker:    {}",
                    style(if ctx.generate_docker {
                        "generate adapter Dockerfile"
                    } else {
                        "manual rootfs mode"
                    })
                    .cyan()
                );
            }
        }
        println!("  Output:    {}", style(output_dir.display()).cyan());
        println!("  Desc:      {}", style(description).cyan());
    }

    let mut writer = TransactionalWriter::new();
    writer.stage_mkdir(&output_dir);

    for file in &files {
        writer.stage_create(
            output_dir.join(&file.relative_path),
            file.content.as_bytes(),
        )?;
    }

    println!();
    println!("{}", style("Operations to perform:").bold());
    for line in writer.summary() {
        println!("  {line}");
    }

    match mode {
        RunMode::DryRun => {
            println!();
            println!(
                "{}",
                style("Dry run successful - validation passed, no changes made.").yellow()
            );
            writer.discard();
            return Ok(());
        }
        RunMode::Confirm => {
            println!();
            if !crate::interactive::confirm_proceed()? {
                println!("{}", style("Aborted - no changes made.").yellow());
                writer.discard();
                return Ok(());
            }
        }
        RunMode::Apply => {}
    }

    println!();
    println!("{}", style("Applying changes...").cyan());
    writer.commit().map_err(|e| {
        anyhow!(
            "Error: failed to apply scaffold files.\nCause: {e}\nHint: verify write permissions and output path."
        )
    })?;

    set_executable_bits(&output_dir, &files)?;

    println!();
    println!(
        "{} Created external tool scaffold: {}",
        style("✓").green(),
        style(name).bold()
    );
    println!("  Path: {}", style(output_dir.display()).cyan());
    println!();
    println!("{}", style("Next steps:").bold());
    println!("  1. Implement logic in scaffolded language files");
    println!(
        "  2. Run setup/build instructions in {}",
        style(output_dir.join("README.md").display()).cyan()
    );
    println!(
        "  3. Set {} and run an agent that references {}",
        style("BAML_EXTERNAL_TOOLS_DIR").cyan(),
        style(ctx.tool_id()).cyan()
    );

    Ok(())
}

/// Compose metadata + README + language-specific files into the final set.
///
/// Exposed so tests can snapshot-check the scaffold without touching the CLI.
pub fn build_file_set(ctx: &ScaffoldContext<'_>) -> Vec<GeneratedFile> {
    let mut files = vec![
        GeneratedFile::new("tool-metadata.json", metadata_json::generate(ctx)),
        GeneratedFile::new("README.md", readme_md::generate(ctx)),
    ];
    files.extend(ctx.language.files(ctx));
    if ctx.runtime == Runtime::Sandbox && ctx.sandbox_source == Some(SandboxSource::Bind) {
        files.extend(crate::templates::external_tool::bind_sandbox::files(ctx));
        ensure_gitignore_entry(&mut files, ".tmp/");
    }
    files
}

fn ensure_gitignore_entry(files: &mut Vec<GeneratedFile>, entry: &str) {
    if let Some(file) = files.iter_mut().find(|f| f.relative_path == ".gitignore") {
        let has_entry = file.content.lines().any(|line| line.trim() == entry);
        if !has_entry {
            if !file.content.is_empty() && !file.content.ends_with('\n') {
                file.content.push('\n');
            }
            file.content.push_str(entry);
            file.content.push('\n');
        }
        return;
    }

    files.push(GeneratedFile::new(".gitignore", format!("{entry}\n")));
}

fn validate_output_dir(output_dir: &Path) -> Result<()> {
    if output_dir.exists() {
        if !output_dir.is_dir() {
            bail!(
                "Error: output path exists and is not a directory: {}",
                output_dir.display()
            );
        }
        let entries = fs::read_dir(output_dir)?;
        if entries.count() > 0 {
            bail!(
                "Error: output directory already exists and is non-empty: {}\nHint: pass --output <new-dir> or clean the directory.",
                output_dir.display()
            );
        }
    }
    Ok(())
}

fn validate_tool_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!(
            "Error: tool name cannot be empty.\nHint: use lowercase like `echo`, `clickup_sync`, or `clickup-sync`."
        );
    }

    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        bail!(
            "Error: invalid tool name '{}'.\nHint: use lowercase letters, numbers, underscores, or hyphens only.",
            name
        );
    }

    if name.starts_with('_')
        || name.ends_with('_')
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("__")
        || name.contains("--")
    {
        bail!(
            "Error: invalid tool name '{}'.\nHint: avoid leading/trailing separators and consecutive '_' or '-'.",
            name
        );
    }

    Ok(())
}

const BIND_ROOTFS_PLACEHOLDER: &str = "<rootfs-path>";
const ZERO_RUNTIME_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn validate_runtime_options(
    runtime: Runtime,
    sandbox_source: SandboxSource,
    sandbox_image: Option<&str>,
    runtime_digest: Option<&str>,
    sandbox_entrypoint: &[String],
) -> Result<(Option<SandboxImageRef>, Option<String>)> {
    match runtime {
        Runtime::Process => {
            if sandbox_image.is_some() || runtime_digest.is_some() || !sandbox_entrypoint.is_empty()
            {
                bail!(
                    "Error: sandbox-only options were supplied with --runtime process.\nHint: remove --sandbox-* options or use --runtime sandbox."
                );
            }
            Ok((None, None))
        }
        Runtime::Sandbox => match sandbox_source {
            SandboxSource::Oci => {
                let image = sandbox_image.ok_or_else(|| {
                    anyhow!("Error: --runtime sandbox --sandbox-source oci requires --sandbox-image <ref@sha256:...>.")
                })?;
                let Some((_, digest_from_image)) = image.split_once("@") else {
                    bail!(
                        "Error: --sandbox-image must be digest-pinned (missing @sha256:): {image}"
                    );
                };
                if !digest_from_image.starts_with("sha256:") {
                    bail!("Error: --sandbox-image must include @sha256:<64-hex>: {image}");
                }
                let digest = runtime_digest.unwrap_or(digest_from_image);
                validate_runtime_digest(digest)?;
                Ok((
                    Some(SandboxImageRef::Oci {
                        r#ref: image.to_string(),
                    }),
                    Some(digest.to_string()),
                ))
            }
            SandboxSource::Bind => {
                let digest = runtime_digest.unwrap_or(ZERO_RUNTIME_DIGEST);
                validate_runtime_digest(digest)?;
                Ok((
                    Some(SandboxImageRef::Bind {
                        path: PathBuf::from(BIND_ROOTFS_PLACEHOLDER),
                    }),
                    Some(digest.to_string()),
                ))
            }
        },
    }
}

fn validate_runtime_digest(digest: &str) -> Result<()> {
    let digest_ok = digest.starts_with("sha256:")
        && digest.len() == 71
        && digest[7..].chars().all(|c| c.is_ascii_hexdigit());
    if !digest_ok {
        bail!("Error: --runtime-digest must match sha256:<64-hex>; got '{digest}'.");
    }
    Ok(())
}

fn validate_generate_docker_option(
    runtime: Runtime,
    sandbox_source: SandboxSource,
    generate_docker: bool,
) -> Result<()> {
    if generate_docker && !(runtime == Runtime::Sandbox && sandbox_source == SandboxSource::Bind) {
        bail!(
            "Error: --generate-docker is only supported with --runtime sandbox --sandbox-source bind."
        );
    }
    Ok(())
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn set_executable_bits(output_dir: &Path, files: &[GeneratedFile]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for file in files.iter().filter(|f| f.executable) {
            let path = output_dir.join(&file.relative_path);
            let metadata = fs::metadata(&path)?;
            let mut perms = metadata.permissions();
            let mode = perms.mode();
            perms.set_mode(mode | 0o111);
            fs::set_permissions(path, perms)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use baml_rt_tools::external_tools::{
        SandboxImageRef, ToolRuntime, metadata_catalog::ExternalMetadataCatalog,
    };

    use super::*;

    fn ctx<'a>(lang: Language) -> ScaffoldContext<'a> {
        ScaffoldContext {
            name: "echo",
            bundle: "dev",
            access: Access::Read,
            language: lang,
            description: "Echo external tool",
            runtime: Runtime::Process,
            sandbox_source: Some(SandboxSource::Oci),
            sandbox_image: None,
            runtime_digest: None,
            sandbox_entrypoint: Vec::new(),
            generate_docker: false,
        }
    }

    fn assert_scaffold_has(files: &[GeneratedFile], path: &str) {
        assert!(
            files.iter().any(|f| f.relative_path == path),
            "scaffold missing expected file `{path}`; got {:?}",
            files.iter().map(|f| &f.relative_path).collect::<Vec<_>>()
        );
    }

    /// Golden fixtures per language — catches silent drift when adding a
    /// language or renaming a scaffolded file.
    #[test]
    fn scaffold_contains_expected_files() {
        let rust_files = build_file_set(&ctx(Language::Rust));
        assert_scaffold_has(&rust_files, "tool-metadata.json");
        assert_scaffold_has(&rust_files, "README.md");
        assert_scaffold_has(&rust_files, "Cargo.toml");
        assert_scaffold_has(&rust_files, "src/main.rs");
        assert_scaffold_has(&rust_files, "tool-server");

        let bash_files = build_file_set(&ctx(Language::Bash));
        assert_scaffold_has(&bash_files, "tool-metadata.json");
        assert_scaffold_has(&bash_files, "tool-server");

        let py_files = build_file_set(&ctx(Language::Python));
        assert_scaffold_has(&py_files, "main.py");
        assert_scaffold_has(&py_files, "tool-server");

        let ts_files = build_file_set(&ctx(Language::Typescript));
        assert_scaffold_has(&ts_files, "package.json");
        assert_scaffold_has(&ts_files, "tsconfig.json");
        assert_scaffold_has(&ts_files, "src/main.ts");
        assert_scaffold_has(&ts_files, "tool-server");
    }

    /// `tool-metadata.json` must round-trip through the runtime's catalog
    /// loader — this is the hard invariant that prevents silent drift between
    /// scaffold output and the runtime's `RawToolMetadata` parser.
    #[test]
    fn scaffolded_metadata_is_loadable_by_runtime_catalog() {
        for lang in [
            Language::Rust,
            Language::Bash,
            Language::Python,
            Language::Typescript,
        ] {
            let ctx = ctx(lang);
            let files = build_file_set(&ctx);

            let tmp = tempfile::tempdir().expect("tmp dir");
            for file in &files {
                // Only metadata + server matter for this check; skip README.
                if file.relative_path == "tool-metadata.json" {
                    std::fs::write(
                        tmp.path().join(&file.relative_path),
                        file.content.as_bytes(),
                    )
                    .unwrap();
                }
            }

            let catalog = ExternalMetadataCatalog::from_dirs(&[tmp.path().to_path_buf()])
                .unwrap_or_else(|e| {
                    panic!("{lang:?} metadata should load via catalog: {e}");
                });
            assert_eq!(catalog.len(), 1, "{lang:?} catalog should have one tool");
        }
    }

    /// Generated metadata JSON parses as valid JSON (no bad escapes from
    /// control chars, quotes, etc.).
    #[test]
    fn scaffolded_metadata_is_valid_json() {
        let mut desc_with_specials = String::from("weird ");
        desc_with_specials.push('\u{0007}'); // control char
        desc_with_specials.push('"');
        desc_with_specials.push('\\');
        desc_with_specials.push_str(" text");

        let ctx = ScaffoldContext {
            name: "echo",
            bundle: "dev",
            access: Access::Read,
            language: Language::Bash,
            description: &desc_with_specials,
            runtime: Runtime::Process,
            sandbox_source: Some(SandboxSource::Oci),
            sandbox_image: None,
            runtime_digest: None,
            sandbox_entrypoint: Vec::new(),
            generate_docker: false,
        };

        let raw = metadata_json::generate(&ctx);
        serde_json::from_str::<serde_json::Value>(&raw)
            .expect("metadata must round-trip through serde_json regardless of description chars");
    }

    #[test]
    fn scaffolded_metadata_emits_explicit_process_runtime_block() {
        let ctx = ctx(Language::Rust);
        let raw = metadata_json::generate(&ctx);
        let parsed: baml_rt_tools::external_tools::ExternalToolMetadata =
            serde_json::from_str(&raw).expect("metadata parses");

        match parsed.runtime {
            Some(ToolRuntime::Process(spec)) => {
                assert_eq!(spec.command, vec!["./tool-server".to_string()]);
            }
            other => panic!("expected explicit process runtime, got {other:?}"),
        }
        assert!(parsed.runtime_digest.is_none());
    }

    #[test]
    fn scaffolded_metadata_emits_sandbox_runtime_block_when_requested() {
        let mut ctx = ctx(Language::Rust);
        ctx.runtime = Runtime::Sandbox;
        ctx.sandbox_source = Some(SandboxSource::Oci);
        ctx.sandbox_image = Some(SandboxImageRef::Oci {
            r#ref: "ghcr.io/org/echo@sha256:1111111111111111111111111111111111111111111111111111111111111111"
                .to_string(),
        });
        ctx.runtime_digest = Some(
            "sha256:2222222222222222222222222222222222222222222222222222222222222222".to_string(),
        );
        ctx.sandbox_entrypoint = vec!["/app/tool-adapter".to_string()];

        let raw = metadata_json::generate(&ctx);
        let parsed: baml_rt_tools::external_tools::ExternalToolMetadata =
            serde_json::from_str(&raw).expect("metadata parses");

        match parsed.runtime {
            Some(ToolRuntime::Sandbox(spec)) => {
                match spec.image {
                    SandboxImageRef::Oci { r#ref } => assert!(r#ref.contains("@sha256:")),
                    _ => panic!("expected oci image"),
                }
                assert_eq!(spec.entrypoint, vec!["/app/tool-adapter".to_string()]);
            }
            other => panic!("expected sandbox runtime, got {other:?}"),
        }
        assert_eq!(
            parsed.runtime_digest.as_deref(),
            Some("sha256:2222222222222222222222222222222222222222222222222222222222222222")
        );
    }

    #[test]
    fn bind_sandbox_defaults_to_placeholder_path_and_zero_digest() {
        let (image, digest) =
            validate_runtime_options(Runtime::Sandbox, SandboxSource::Bind, None, None, &[])
                .expect("bind scaffolding defaults should be valid");

        match image {
            Some(SandboxImageRef::Bind { path }) => {
                assert_eq!(path, PathBuf::from(BIND_ROOTFS_PLACEHOLDER));
            }
            other => panic!("expected bind image, got {other:?}"),
        }

        assert_eq!(digest.as_deref(), Some(ZERO_RUNTIME_DIGEST));
    }

    #[test]
    fn bind_scaffold_does_not_emit_setup_or_docker_artifacts_by_default() {
        let mut bind_ctx = ctx(Language::Python);
        bind_ctx.runtime = Runtime::Sandbox;
        bind_ctx.sandbox_source = Some(SandboxSource::Bind);
        bind_ctx.sandbox_image = Some(SandboxImageRef::Bind {
            path: PathBuf::from(BIND_ROOTFS_PLACEHOLDER),
        });
        bind_ctx.runtime_digest = Some(ZERO_RUNTIME_DIGEST.to_string());

        let files = build_file_set(&bind_ctx);
        assert!(
            files
                .iter()
                .all(|f| f.relative_path != "setup_bind_sandbox.sh"),
            "default bind scaffolds should not emit bind setup script"
        );
        assert!(
            files
                .iter()
                .all(|f| f.relative_path != "adapter/Dockerfile"),
            "default bind scaffolds should not emit adapter Dockerfile"
        );
        assert!(
            files
                .iter()
                .all(|f| f.relative_path != "adapter/tool-adapter"),
            "default bind scaffolds should not emit adapter tool-adapter"
        );
    }

    #[test]
    fn bind_scaffold_can_emit_docker_artifacts_when_requested() {
        let mut bind_ctx = ctx(Language::Python);
        bind_ctx.runtime = Runtime::Sandbox;
        bind_ctx.sandbox_source = Some(SandboxSource::Bind);
        bind_ctx.sandbox_image = Some(SandboxImageRef::Bind {
            path: PathBuf::from(BIND_ROOTFS_PLACEHOLDER),
        });
        bind_ctx.runtime_digest = Some(ZERO_RUNTIME_DIGEST.to_string());
        bind_ctx.generate_docker = true;

        let files = build_file_set(&bind_ctx);
        assert_scaffold_has(&files, "setup_bind_sandbox.sh");
        assert_scaffold_has(&files, "adapter/Dockerfile");
        assert_scaffold_has(&files, "adapter/tool-adapter");
    }

    #[test]
    fn generate_docker_flag_is_restricted_to_bind_sandbox() {
        assert!(
            validate_generate_docker_option(Runtime::Sandbox, SandboxSource::Bind, true).is_ok()
        );
        assert!(
            validate_generate_docker_option(Runtime::Process, SandboxSource::Bind, true).is_err()
        );
        assert!(
            validate_generate_docker_option(Runtime::Sandbox, SandboxSource::Oci, true).is_err()
        );
    }

    #[test]
    fn readme_supported_methods_do_not_duplicate_describe() {
        let files = build_file_set(&ctx(Language::Python));
        let readme = files
            .iter()
            .find(|f| f.relative_path == "README.md")
            .expect("README emitted");

        let describe_bullet_count = readme.content.matches("- `tool/describe`").count();
        assert_eq!(
            describe_bullet_count, 1,
            "README should list tool/describe only once in supported-methods bullets"
        );
    }

    #[test]
    fn bind_setup_script_uses_tool_relative_rootfs_and_sync_command() {
        let mut bind_ctx = ctx(Language::Python);
        bind_ctx.runtime = Runtime::Sandbox;
        bind_ctx.sandbox_source = Some(SandboxSource::Bind);
        bind_ctx.sandbox_image = Some(SandboxImageRef::Bind {
            path: PathBuf::from(BIND_ROOTFS_PLACEHOLDER),
        });
        bind_ctx.runtime_digest = Some(ZERO_RUNTIME_DIGEST.to_string());
        bind_ctx.generate_docker = true;

        let files = build_file_set(&bind_ctx);
        let script = files
            .iter()
            .find(|f| f.relative_path == "setup_bind_sandbox.sh")
            .expect("setup script emitted");

        assert!(
            script.content.contains("ROOTFS=\"$TOOL_DIR/.tmp/"),
            "script should default ROOTFS under TOOL_DIR"
        );
        assert!(
            script.content.contains("sandbox-bind-sync"),
            "script should delegate to sandbox-bind-sync"
        );
    }

    #[test]
    fn bind_scaffold_gitignore_ignores_tmp_rootfs() {
        let mut bind_ctx = ctx(Language::Python);
        bind_ctx.runtime = Runtime::Sandbox;
        bind_ctx.sandbox_source = Some(SandboxSource::Bind);
        bind_ctx.sandbox_image = Some(SandboxImageRef::Bind {
            path: PathBuf::from(BIND_ROOTFS_PLACEHOLDER),
        });
        bind_ctx.runtime_digest = Some(ZERO_RUNTIME_DIGEST.to_string());

        let files = build_file_set(&bind_ctx);
        let gitignore = files
            .iter()
            .find(|f| f.relative_path == ".gitignore")
            .expect(".gitignore emitted");

        assert!(
            gitignore.content.lines().any(|line| line.trim() == ".tmp/"),
            ".gitignore should ignore local bind rootfs artifacts"
        );
    }

    #[test]
    fn bind_scaffold_adds_gitignore_for_languages_without_one() {
        let mut bind_ctx = ctx(Language::Bash);
        bind_ctx.runtime = Runtime::Sandbox;
        bind_ctx.sandbox_source = Some(SandboxSource::Bind);
        bind_ctx.sandbox_image = Some(SandboxImageRef::Bind {
            path: PathBuf::from(BIND_ROOTFS_PLACEHOLDER),
        });
        bind_ctx.runtime_digest = Some(ZERO_RUNTIME_DIGEST.to_string());

        let files = build_file_set(&bind_ctx);
        let gitignore = files
            .iter()
            .find(|f| f.relative_path == ".gitignore")
            .expect(".gitignore emitted for bash bind scaffold");

        assert_eq!(gitignore.content, ".tmp/\n");
    }
}
