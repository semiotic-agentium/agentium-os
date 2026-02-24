//! Package loading seam: extract tar.gz and parse manifest.
//! Tests can use this with fixture tarballs or inject (extract_dir, manifest).

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use baml_rt_core::{AgentManifest, AgentPackageName, BamlRtError, Result};
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

    if AgentPackageName::parse(&manifest.name).is_none() {
        return Err(BamlRtError::InvalidArgument(format!(
            "manifest.json has invalid 'name' (must be non-empty ASCII [a-zA-Z0-9_-] with no surrounding whitespace): {}",
            manifest.name
        )));
    }

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
    use std::fs;

    use flate2::{Compression, write::GzEncoder};
    use tar::Builder;

    use super::{load_package, next_extract_dir};

    #[test]
    fn next_extract_dir_is_unique() {
        let a = next_extract_dir().expect("first path");
        let b = next_extract_dir().expect("second path");
        assert_ne!(a, b, "extract directories must be unique");
    }

    #[tokio::test]
    async fn load_package_rejects_invalid_manifest_name_before_use() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "runner-package-invalid-name-test-{}-{}",
            std::process::id(),
            unique
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("baml_src")).expect("create baml_src");
        fs::create_dir_all(root.join("dist")).expect("create dist");
        fs::write(
            root.join("dist/index.js"),
            "globalThis.onChatMessage = async () => {};",
        )
        .expect("write index");
        fs::write(
            root.join("baml_src/dummy.baml"),
            "function Dummy() -> string { client \"x\" prompt #\"ok\"# }",
        )
        .expect("write dummy baml");
        fs::write(
            root.join("manifest.json"),
            serde_json::json!({
                "version": "1.0.0",
                "name": "../escape",
                "entry_point": "dist/index.js",
                "signature": "invalid@1.0.0",
                "tools": [],
            })
            .to_string(),
        )
        .expect("write manifest");

        let tar_path = std::env::temp_dir().join(format!(
            "runner-package-invalid-name-test-{}-{}.tar.gz",
            std::process::id(),
            unique
        ));
        {
            let file = fs::File::create(&tar_path).expect("create tar");
            let enc = GzEncoder::new(file, Compression::default());
            let mut tar = Builder::new(enc);
            tar.append_dir_all(".", &root).expect("append package");
            tar.finish().expect("finish tar");
        }

        let err = load_package(&tar_path)
            .await
            .expect_err("invalid name should fail");
        assert!(
            err.to_string().contains("invalid 'name'"),
            "unexpected error: {err}"
        );

        let _ = fs::remove_file(tar_path);
        let _ = fs::remove_dir_all(root);
    }
}
