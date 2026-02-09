//! Builder service that orchestrates the agent building pipeline

use crate::builder::traits::{Linter, Packager, TypeGenerator, TypeScriptCompiler};
use crate::builder::types::{AgentDir, BuildDir};
use baml_rt_core::Result;

/// Service that orchestrates the agent building process
pub struct BuilderService<L, TC, TG, P> {
    linter: L,
    ts_compiler: TC,
    type_generator: TG,
    packager: P,
}

impl<L, TC, TG, P> BuilderService<L, TC, TG, P>
where
    L: Linter,
    TC: TypeScriptCompiler,
    TG: TypeGenerator,
    P: Packager,
{
    pub fn new(linter: L, ts_compiler: TC, type_generator: TG, packager: P) -> Self {
        Self {
            linter,
            ts_compiler,
            type_generator,
            packager,
        }
    }

    /// Build a complete agent package
    pub async fn build_package(
        &self,
        agent_dir: &AgentDir,
        build_dir: &BuildDir,
        output: &std::path::Path,
        lint: bool,
    ) -> Result<()> {
        // Stage 1: Lint (if enabled)
        if lint {
            println!("\n🔍 Linting source code...");
            self.linter.lint(agent_dir).await?;
        }

        // Stage 2: Generate runtime type declarations from BAML runtime
        println!("\n📝 Generating runtime type declarations...");
        self.type_generator
            .generate(&agent_dir.baml_src(), build_dir)
            .await?;

        // Stage 3: Compile TypeScript
        println!("\n⚙️  Compiling TypeScript...");
        let src_dir = agent_dir.src();
        let dist_dir = build_dir.join("dist");
        self.ts_compiler.compile(&src_dir, &dist_dir).await?;

        // Stage 4: Package
        println!("\n📦 Packaging agent...");
        self.packager.package(agent_dir, build_dir, output).await?;

        Ok(())
    }
}
