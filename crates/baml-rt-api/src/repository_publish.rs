use std::{fs, path::Path, sync::Arc};

use axum::extract::{Json, State};
use baml_rt_builder::builder::{
    AgentDir, BuildDir, BuilderService, FileSystem, RuntimeTypeGenerator, StdFileSystem,
    StdPackager, TscCompiler,
};
use baml_rt_repository::{
    RepositoryService,
    commands::{PublishCommand, PublishResult},
    entry::SourceBundle,
};
use http_api_problem::HttpApiProblem;

pub async fn publish_with_build(
    State(svc): State<Arc<RepositoryService>>,
    Json(cmd): Json<PublishCommand>,
) -> Result<Json<PublishResult>, HttpApiProblem> {
    let result = svc.publish(cmd).await.map_err(HttpApiProblem::from)?;
    let entry = svc
        .get_by_hash(&result.hash)
        .await
        .map_err(HttpApiProblem::from)?
        .ok_or_else(|| {
            HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
                .detail("Published entry missing after metadata insert")
        })?;

    let built = build_artifact(&entry.source).await.map_err(|e| {
        HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
            .title("Artifact build failed")
            .detail(e.to_string())
    })?;

    svc.put_built_blob(&result.hash, &built)
        .await
        .map_err(HttpApiProblem::from)?;

    Ok(Json(result))
}

async fn build_artifact(source: &SourceBundle) -> anyhow::Result<Vec<u8>> {
    let workspace = unique_temp_dir("baml-repository-publish");
    fs::create_dir_all(&workspace)?;

    let result = async {
        materialize_source_bundle(source, &workspace)?;

        let agent_dir = AgentDir::new(workspace.clone())?;
        let build_dir = BuildDir::new()?;
        let fs_impl = StdFileSystem;
        fs_impl.copy_dir_all(&agent_dir.baml_src(), &build_dir.join("baml_src"))?;
        let output = build_dir.join("package.tar.gz");
        let builder = BuilderService::new(
            TscCompiler::new(),
            RuntimeTypeGenerator::new(),
            StdPackager::new(fs_impl),
        );
        builder
            .build_package(&agent_dir, &build_dir, &output)
            .await
            .map_err(anyhow::Error::from)?;

        let bytes = fs::read(&output)?;
        Ok::<_, anyhow::Error>(bytes)
    }
    .await;

    let _ = fs::remove_dir_all(&workspace);
    result
}

fn materialize_source_bundle(source: &SourceBundle, root: &Path) -> anyhow::Result<()> {
    let manifest_path = root.join("manifest.json");
    let manifest = serde_json::to_string_pretty(source.manifest.as_value())?;
    fs::write(&manifest_path, manifest)?;
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("baml_src"))?;

    for file in source.ts_sources.iter().chain(source.baml_sources.iter()) {
        let path = root.join(file.path.as_str());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, file.content.as_str())?;
    }
    Ok(())
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}
