//! Package loading seam: extract tar.gz and parse manifest.
//! Tests can use this with fixture tarballs or inject (extract_dir, manifest).

use baml_rt_core::{AgentManifest, BamlRtError, Result};
use baml_rt_observability::spans;
use std::path::{Path, PathBuf};
use tracing::info;

/// Load an agent package from a tar.gz path: extract to a temp dir and return
/// (extract_dir, validated manifest). Caller is responsible for ensuring
/// baml_src exists under extract_dir if they need it.
pub async fn load_package(package_path: &Path) -> Result<(PathBuf, AgentManifest)> {
    let span = spans::load_agent_package(package_path);
    let _guard = span.enter();

    let epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| BamlRtError::InvalidArgument(e.to_string()))?
        .as_secs();
    let extract_dir = std::env::temp_dir().join(format!("baml-agent-{}", epoch_secs));
    std::fs::create_dir_all(&extract_dir).map_err(BamlRtError::Io)?;

    {
        let extract_span = spans::extract_package(&extract_dir);
        let _extract_guard = extract_span.enter();

        let tar_gz = std::fs::File::open(package_path).map_err(BamlRtError::Io)?;
        let tar = flate2::read::GzDecoder::new(tar_gz);
        let mut archive = tar::Archive::new(tar);
        archive.unpack(&extract_dir).map_err(BamlRtError::Io)?;
    }

    let manifest_path = extract_dir.join("manifest.json");
    let manifest_content = std::fs::read_to_string(&manifest_path).map_err(BamlRtError::Io)?;
    let manifest: AgentManifest =
        serde_json::from_str(&manifest_content).map_err(BamlRtError::Json)?;

    if manifest.signature.is_empty() {
        return Err(BamlRtError::InvalidArgument(
            "manifest.json missing or empty 'signature' field".to_string(),
        ));
    }

    info!(
        name = manifest.name,
        version = manifest.version,
        entry_point = manifest.entry_point,
        "Agent manifest loaded"
    );

    let baml_src = extract_dir.join("baml_src");
    if !baml_src.exists() {
        return Err(BamlRtError::InvalidArgument(
            "Package missing baml_src directory".to_string(),
        ));
    }

    Ok((extract_dir, manifest))
}
