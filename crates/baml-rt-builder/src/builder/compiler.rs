//! Compiler implementations for BAML and TypeScript

use crate::builder::baml_gen::render_baml_tool_interfaces;
use crate::builder::baml_signature_gen::extract_baml_signatures;
use crate::builder::traits::{FileSystem, TypeGenerator, TypeScriptCompiler};
use crate::builder::ts_gen::{load_manifest_tools, render_ts_declarations};
use crate::builder::types::BuildDir;
use baml_rt_core::{BamlRtError, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

/// TypeScript compiler using OXC
pub struct OxcTypeScriptCompiler<FS> {
    filesystem: FS,
}

impl<FS: FileSystem> OxcTypeScriptCompiler<FS> {
    pub fn new(filesystem: FS) -> Self {
        Self { filesystem }
    }
}

#[async_trait::async_trait]
impl<FS: FileSystem> TypeScriptCompiler for OxcTypeScriptCompiler<FS> {
    async fn compile(&self, src_dir: &Path, dist_dir: &Path) -> Result<()> {
        self.filesystem.create_dir_all(dist_dir)?;

        let mut files = Vec::new();
        self.filesystem.collect_ts_files(src_dir, &mut files)?;

        use oxc_allocator::Allocator;
        use oxc_codegen::Codegen;
        use oxc_parser::Parser;
        use oxc_semantic::SemanticBuilder;
        use oxc_transformer::{HelperLoaderMode, TransformOptions, Transformer};

        for file_path in files {
            let content = self.filesystem.read_to_string(&file_path)?;

            let allocator = Allocator::default();
            let source_type = oxc_span::SourceType::from_path(&file_path)
                .unwrap_or_else(|_| oxc_span::SourceType::default());
            let parser = Parser::new(&allocator, &content, source_type);
            let parse_result = parser.parse();

            if !parse_result.errors.is_empty() {
                let errors: Vec<String> = parse_result
                    .errors
                    .iter()
                    .map(|e| format!("{:?}", e))
                    .collect();
                return Err(BamlRtError::InvalidArgument(format!(
                    "Parse error in {}: {}",
                    file_path.display(),
                    errors.join(", ")
                )));
            }

            let mut program = parse_result.program;
            let semantic_result = SemanticBuilder::new()
                .with_excess_capacity(2.0)
                .build(&program);
            if !semantic_result.errors.is_empty() {
                let errors: Vec<String> = semantic_result
                    .errors
                    .iter()
                    .map(|e| format!("{:?}", e))
                    .collect();
                return Err(BamlRtError::InvalidArgument(format!(
                    "Semantic error in {}: {}",
                    file_path.display(),
                    errors.join(", ")
                )));
            }

            let scoping = semantic_result.semantic.into_scoping();
            let mut transform_options = TransformOptions::default();
            transform_options.helper_loader.mode = HelperLoaderMode::External;
            let transform_result = Transformer::new(&allocator, &file_path, &transform_options)
                .build_with_scoping(scoping, &mut program);
            if !transform_result.errors.is_empty() {
                let errors: Vec<String> = transform_result
                    .errors
                    .iter()
                    .map(|e| format!("{:?}", e))
                    .collect();
                return Err(BamlRtError::InvalidArgument(format!(
                    "Transform error in {}: {}",
                    file_path.display(),
                    errors.join(", ")
                )));
            }

            let mut js_code = Codegen::new().build(&program).code;
            // QuickJS does not support ESM; strip trailing empty export so script evaluates.
            let trimmed = js_code.trim_end();
            if trimmed.ends_with("export {};") {
                js_code = trimmed
                    .strip_suffix("export {};")
                    .unwrap_or(trimmed)
                    .trim_end()
                    .to_string();
            } else if trimmed.ends_with("export {}") {
                js_code = trimmed
                    .strip_suffix("export {}")
                    .unwrap_or(trimmed)
                    .trim_end()
                    .to_string();
            }
            let relative_path = file_path.strip_prefix(src_dir).map_err(|_| {
                BamlRtError::InvalidArgument(format!(
                    "File {} is not under src directory",
                    file_path.display()
                ))
            })?;

            let output_path = dist_dir.join(relative_path).with_extension("js");
            if let Some(parent) = output_path.parent() {
                self.filesystem.create_dir_all(parent)?;
            }

            if output_path.file_name() == Some(OsStr::new("index.js")) {
                let a2a_shim = include_str!("a2a.js");
                js_code = format!("{}\n{}", a2a_shim.trim_end(), js_code);
            }

            self.filesystem.write_string(&output_path, &js_code)?;
        }

        Ok(())
    }
}

/// Type generator for runtime declarations
pub struct RuntimeTypeGenerator;

impl RuntimeTypeGenerator {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RuntimeTypeGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl TypeGenerator for RuntimeTypeGenerator {
    async fn generate(&self, baml_src: &Path, build_dir: &BuildDir) -> Result<()> {
        use baml_runtime::BamlRuntime;
        use std::collections::HashMap;

        // Generate BAML tool interfaces FIRST (before loading runtime, since prompts may reference them)
        let tool_names = load_manifest_tools(baml_src)?;
        if !tool_names.is_empty() {
            let baml_interfaces = render_baml_tool_interfaces(&tool_names)?;
            let baml_output_path = baml_src.join("generated_tools.baml");
            fs::write(&baml_output_path, baml_interfaces).map_err(BamlRtError::Io)?;
        }

        // Load BAML runtime to discover functions (after generating BAML interfaces)
        let env_vars: HashMap<String, String> = HashMap::new();
        let feature_flags = internal_baml_core::feature_flags::FeatureFlags::default();

        let runtime = BamlRuntime::from_directory(baml_src, env_vars, feature_flags)
            .map_err(|e| BamlRtError::RuntimeLoadFailed { source: e })?;

        // Typed signatures from IR (no BAML source parsing); TS emitter gets IR + tool names so
        // it emits typed BAML function declarations and preserves tool type generation.
        let ir_signature = extract_baml_signatures(&runtime)?;
        let declarations = render_ts_declarations(&ir_signature, &tool_names)?;
        let ts_output_path = build_dir.join("dist").join("baml-runtime.d.ts");
        if let Some(parent) = ts_output_path.parent() {
            fs::create_dir_all(parent).map_err(BamlRtError::Io)?;
        }
        fs::write(&ts_output_path, declarations).map_err(BamlRtError::Io)?;

        Ok(())
    }
}
