//! Regenerate test fixture artifacts (baml-runtime.d.ts etc.).
//!
//! This binary links baml-rt-tools-internal-dev so the tool catalog can resolve
//! internal-dev/* when processing fixture manifests.

use baml_rt_builder::builder::{BuildDir, RuntimeTypeGenerator, TypeGenerator};
use baml_rt_core::{BamlRtError, Result};
use std::path::{Path, PathBuf};

// Pull in internal-dev so inventory has internal-dev/* tool metadata for fixture resolution.
#[allow(unused_imports)]
use baml_rt_tools_internal_dev::InternalDev;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("regen-fixtures is under crates/tools/")
        .to_path_buf()
}

fn fixture_dir(name: &str) -> PathBuf {
    workspace_root()
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
            root.display()
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
    let _ = std::any::type_name::<InternalDev>();
    regen_fixture("stream-baml-tool").await?;
    regen_fixture("stream-js-tool").await?;
    Ok(())
}
