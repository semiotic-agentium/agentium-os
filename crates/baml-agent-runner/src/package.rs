//! Package loading seam: extract tar.gz and parse manifest.
//! Tests can use this with fixture tarballs or inject (extract_dir, manifest).

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use baml_rt_core::{AgentManifest, BamlRtError, Result, join_error_message};
use baml_rt_observability::spans;
use tracing::info;

fn next_extract_dir() -> Result<PathBuf> {
    static EXTRACT_COUNTER: AtomicU64 = AtomicU64::new(0);
    let epoch_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| BamlRtError::InvalidArgument(e.to_string()))?
        .as_nanos();
    let pid = std::process::id();
    let seq = EXTRACT_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(std::env::temp_dir().join(format!("baml-agent-{epoch_nanos}-{pid}-{seq}")))
}

/// Load an agent package from a tar.gz path: extract to a temp dir and return
/// (extract_dir, validated manifest). Caller is responsible for ensuring
/// baml_src exists under extract_dir if they need it.
pub async fn load_package(package_path: &Path) -> Result<(PathBuf, AgentManifest)> {
    let package_path = package_path.to_path_buf();
    tokio::task::spawn_blocking(move || load_package_blocking(&package_path))
        .await
        .map_err(|e| blocking_join_error("agent package load", e))?
}

pub(crate) fn blocking_join_error(operation: &str, err: tokio::task::JoinError) -> BamlRtError {
    BamlRtError::Io(std::io::Error::other(join_error_message(operation, err)))
}

fn load_package_blocking(package_path: &Path) -> Result<(PathBuf, AgentManifest)> {
    let span = spans::load_agent_package(package_path);
    let _guard = span.enter();

    let extract_dir = next_extract_dir()?;
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

#[cfg(test)]
mod tests {
    use super::next_extract_dir;

    #[test]
    fn next_extract_dir_is_unique() {
        let a = next_extract_dir().expect("first path");
        let b = next_extract_dir().expect("second path");
        assert_ne!(a, b, "extract directories must be unique");
    }
}
