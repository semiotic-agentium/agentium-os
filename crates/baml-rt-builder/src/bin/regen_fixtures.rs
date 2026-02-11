use baml_rt_builder::builder::{BuildDir, RuntimeTypeGenerator, TypeGenerator};
use baml_rt_core::{BamlRtError, Result};
use std::path::{Path, PathBuf};

fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .join("tests")
        .join("fixtures")
        .join("agents")
        .join(name)
}

fn copy_runtime_d_ts(build_dir: &BuildDir, dest_src: &Path) -> Result<()> {
    let d_ts_src = build_dir.join("dist").join("baml-runtime.d.ts");
    let d_ts_dest = dest_src.join("baml-runtime.d.ts");
    if !d_ts_src.exists() {
        return Err(BamlRtError::InvalidArgument(
            "baml-runtime.d.ts was not generated".to_string(),
        ));
    }
    std::fs::create_dir_all(dest_src).map_err(BamlRtError::Io)?;
    std::fs::copy(&d_ts_src, &d_ts_dest).map_err(BamlRtError::Io)?;
    Ok(())
}

async fn regen_fixture(name: &str) -> Result<()> {
    let root = fixture_dir(name);
    let baml_src = root.join("baml_src");
    let src_dir = root.join("src");

    if !baml_src.exists() {
        return Err(BamlRtError::InvalidArgument(format!(
            "fixture missing baml_src: {}",
            baml_src.display()
        )));
    }

    let build_dir = BuildDir::new()?;
    let generator = RuntimeTypeGenerator::new();
    generator.generate(&baml_src, &build_dir).await?;
    copy_runtime_d_ts(&build_dir, &src_dir)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    regen_fixture("stream-baml-tool").await?;
    regen_fixture("stream-js-tool").await?;
    regen_fixture("conversational-context-auto").await?;
    regen_fixture("conversational-persona-demo").await?;
    Ok(())
}
