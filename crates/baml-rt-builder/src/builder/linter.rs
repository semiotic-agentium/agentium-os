//! Linter implementation using OXC parser

use crate::builder::traits::{FileSystem, Linter};
use crate::builder::types::AgentDir;
use baml_rt_core::{BamlRtError, Result};
use std::fs;

/// OXC-based linter implementation
pub struct OxcLinter<FS> {
    filesystem: FS,
}

impl<FS: FileSystem> OxcLinter<FS> {
    pub fn new(filesystem: FS) -> Self {
        Self { filesystem }
    }
}

#[async_trait::async_trait]
impl<FS: FileSystem> Linter for OxcLinter<FS> {
    async fn lint(&self, agent_dir: &AgentDir) -> Result<()> {
        let src_dir = agent_dir.src();

        if !src_dir.exists() {
            println!("✓ No src directory found, nothing to lint");
            return Ok(());
        }

        let mut files = Vec::new();
        self.filesystem.collect_ts_js_files(&src_dir, &mut files)?;

        if files.is_empty() {
            println!("✓ No TypeScript/JavaScript files found to lint");
            return Ok(());
        }

        println!("🔍 Linting {} file(s)...", files.len());

        use oxc_allocator::Allocator;
        use oxc_parser::Parser;

        let mut errors = Vec::new();
        for file_path in files {
            let content = fs::read_to_string(&file_path).map_err(BamlRtError::Io)?;
            let file_name = file_path
                .strip_prefix(agent_dir.as_path())
                .unwrap_or(&file_path)
                .display()
                .to_string();

            let allocator = Allocator::default();
            let source_type = oxc_span::SourceType::from_path(&file_path)
                .unwrap_or_else(|_| oxc_span::SourceType::default());
            let parser = Parser::new(&allocator, &content, source_type);
            let parse_result = parser.parse();

            if !parse_result.errors.is_empty() {
                for error in parse_result.errors {
                    errors.push(format!("{} - Syntax error: {:?}", file_name, error));
                }
            } else {
                println!("  ✓ {}", file_name);
            }
        }

        if !errors.is_empty() {
            println!("\n❌ Linting failed with {} error(s):", errors.len());
            for error in &errors {
                println!("  {}", error);
            }
            return Err(BamlRtError::InvalidArgument(format!(
                "Linting failed with {} error(s)",
                errors.len()
            )));
        }

        println!("\n✓ All files passed linting");
        Ok(())
    }
}
