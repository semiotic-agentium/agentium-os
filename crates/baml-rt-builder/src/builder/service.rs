//! Builder service that orchestrates the agent building pipeline

use crate::builder::{
    error::Result,
    traits::{Packager, TypeGenerator, TypeScriptCompiler},
    types::{AgentDir, BuildDir},
};

/// Service that orchestrates the agent building process.
///
/// Pipeline: generate types → compile TypeScript (with type checking) → package.
pub struct BuilderService<TC, TG, P> {
    ts_compiler: TC,
    type_generator: TG,
    packager: P,
}

impl<TC, TG, P> BuilderService<TC, TG, P>
where
    TC: TypeScriptCompiler,
    TG: TypeGenerator,
    P: Packager,
{
    pub fn new(ts_compiler: TC, type_generator: TG, packager: P) -> Self {
        Self {
            ts_compiler,
            type_generator,
            packager,
        }
    }

    /// Build a complete agent package.
    pub async fn build_package(
        &self,
        agent_dir: &AgentDir,
        build_dir: &BuildDir,
        output: &std::path::Path,
    ) -> Result<()> {
        // Stage 1: Generate runtime type declarations (writes src/baml-runtime.d.ts)
        println!("\n📝 Generating runtime type declarations...");
        self.type_generator.generate(agent_dir, build_dir).await?;

        // Stage 2: Compile TypeScript (tsc performs full type checking during compilation)
        println!("\n⚙️  Compiling TypeScript...");
        let dist_dir = build_dir.join("dist");
        self.ts_compiler.compile(agent_dir, &dist_dir).await?;

        // Stage 3: Package
        println!("\n📦 Packaging agent...");
        self.packager.package(agent_dir, build_dir, output).await?;

        Ok(())
    }
}
