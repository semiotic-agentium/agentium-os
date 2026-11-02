// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

use std::{fs, path::Path, sync::Arc};

use axum::extract::{Json, State};
use baml_rt_builder::builder::{
    AgentDir, BuildDir, BuilderService, FileSystem, RuntimeTypeGenerator, StdFileSystem,
    StdPackager, TscCompiler,
};
use baml_rt_core::{ArtifactHash, join_error_message};
use baml_rt_repository::{
    RepositoryService,
    commands::{ForkCommand, PublishCommand, PublishResult},
    entry::{SourceBundle, VersionReservation},
    ids::ContentHash,
    package::source_bundle_from_tar_gz,
};
// Register tool inventory for repository-side artifact builds (POST /repository/publish).
// Feature-gated so a default build does not link these tool crates.
#[cfg(feature = "notion")]
use baml_tools_notion as _;
#[cfg(feature = "security-eval")]
use baml_tools_security_eval as _;
use http_api_problem::HttpApiProblem;

/// Publish an agent: build from source and store in repository (POST /repository/publish, operator-authenticated).
///
/// The version is reserved before the build, so the artifact manifest and the stored entry
/// carry the same version even when several publishes run at once. The blob is written
/// before the entry; an orphan blob is harmless because blob storage is content addressed.
pub async fn publish_with_build(
    State(svc): State<Arc<RepositoryService>>,
    Json(cmd): Json<PublishCommand>,
) -> Result<Json<PublishResult>, HttpApiProblem> {
    let reservation = svc
        .reserve_version(&cmd.name)
        .await
        .map_err(HttpApiProblem::from)?;

    let outcome = publish_reserved_artifact(svc.clone(), cmd, &reservation).await;
    if outcome.is_err() {
        // `publish_reserved` releases its own failures; this covers build, parse, and blob
        // failures that happen before it. Releasing a consumed reservation is a no-op.
        if let Err(e) = svc.release_version(&reservation).await {
            tracing::warn!(
                agent = %reservation.name,
                version = reservation.version.as_u32(),
                error = %e,
                "failed to release version reservation after publish failure; it will expire"
            );
        }
    }
    outcome.map(Json)
}

struct BuiltArtifact {
    bytes: Arc<[u8]>,
    artifact_hash: ArtifactHash,
    identity_digest: baml_rt_repository::AgentIdentityDigest,
    source: SourceBundle,
    expected_hash: ContentHash,
    dev_artifacts: Option<baml_rt_repository::DevArtifactsBundle>,
}

async fn publish_reserved_artifact(
    svc: Arc<RepositoryService>,
    cmd: PublishCommand,
    reservation: &VersionReservation,
) -> Result<PublishResult, HttpApiProblem> {
    let repository_version = reservation.version.as_u32();
    tracing::info!(
        agent = %cmd.name,
        repository_version,
        "building repository artifact"
    );
    let built = build_reserved_artifact(
        svc.clone(),
        cmd.source.with_manifest_version(repository_version),
        repository_version,
    )
    .await?;

    // Blob first: a failure here leaves neither an entry nor a claim on one, and an orphan
    // blob is harmless because it is content addressed.
    svc.put_built_blob(&built.artifact_hash, &built.bytes)
        .await
        .map_err(HttpApiProblem::from)?;

    let cmd = PublishCommand {
        name: cmd.name,
        source: built.source,
        rationale: cmd.rationale,
        origin: cmd.origin,
    };
    let result = svc
        .publish_built(cmd, reservation, built.artifact_hash, built.identity_digest)
        .await
        .map_err(HttpApiProblem::from)?;
    verify_published_hash(&result, &built.expected_hash)?;
    store_dev_artifacts(&svc, &result.hash, built.dev_artifacts).await?;
    Ok(result)
}

/// Fork an existing entry and publish the built forked source (POST /repository/fork).
pub async fn fork_with_build(
    State(svc): State<Arc<RepositoryService>>,
    Json(cmd): Json<ForkCommand>,
) -> Result<Json<PublishResult>, HttpApiProblem> {
    let reservation = svc
        .reserve_version(&cmd.new_name)
        .await
        .map_err(HttpApiProblem::from)?;

    let outcome = fork_reserved_artifact(svc.clone(), cmd, &reservation).await;
    if outcome.is_err()
        && let Err(e) = svc.release_version(&reservation).await
    {
        tracing::warn!(
            agent = %reservation.name,
            version = reservation.version.as_u32(),
            error = %e,
            "failed to release version reservation after fork failure; it will expire"
        );
    }
    outcome.map(Json)
}

async fn fork_reserved_artifact(
    svc: Arc<RepositoryService>,
    cmd: ForkCommand,
    reservation: &VersionReservation,
) -> Result<PublishResult, HttpApiProblem> {
    let repository_version = reservation.version.as_u32();
    tracing::info!(
        agent = %cmd.new_name,
        forked_from = %cmd.source_hash,
        repository_version,
        "building forked repository artifact"
    );
    let built = build_reserved_artifact(
        svc.clone(),
        cmd.source.with_manifest_version(repository_version),
        repository_version,
    )
    .await?;

    svc.put_built_blob(&built.artifact_hash, &built.bytes)
        .await
        .map_err(HttpApiProblem::from)?;

    let cmd = ForkCommand {
        source_hash: cmd.source_hash,
        new_name: cmd.new_name,
        source: built.source,
        rationale: cmd.rationale,
        fork_description: cmd.fork_description,
        tags: cmd.tags,
    };
    let result = svc
        .fork_built(cmd, reservation, built.artifact_hash, built.identity_digest)
        .await
        .map_err(HttpApiProblem::from)?;
    verify_published_hash(&result, &built.expected_hash)?;
    store_dev_artifacts(&svc, &result.hash, built.dev_artifacts).await?;
    Ok(result)
}

async fn build_reserved_artifact(
    svc: Arc<RepositoryService>,
    source_versioned: SourceBundle,
    repository_version: u32,
) -> Result<BuiltArtifact, HttpApiProblem> {
    let (built, dev_artifacts): (Arc<[u8]>, Option<baml_rt_repository::DevArtifactsBundle>) = {
        let (bytes, artifacts) = build_artifact(source_versioned, svc.clone())
            .await
            .map_err(|e| {
                HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Artifact build failed")
                    .detail(format!("stage=build: {}", e))
            })?;
        (Arc::from(bytes), artifacts)
    };

    let artifact_hash =
        baml_rt_repository::artifact_hash_from_tar_gz(built.as_ref()).map_err(|e| {
            HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
                .title("Artifact identity failed")
                .detail(format!("stage=artifact-hash: {e}"))
        })?;

    let identity_bytes = Arc::clone(&built);
    let identity_digest = tokio::task::spawn_blocking(move || {
        baml_rt_repository::identity_digest_from_tar_gz(identity_bytes.as_ref())
    })
    .await
    .map_err(|e| {
        HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
            .title("Artifact identity failed")
            .detail(join_error_message("identity digest", e))
    })?
    .map_err(|e| {
        HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
            .title("Artifact identity failed")
            .detail(format!("stage=identity: {}", e))
    })?;

    let parse_bytes = Arc::clone(&built);
    let (_, extracted) =
        tokio::task::spawn_blocking(move || source_bundle_from_tar_gz(parse_bytes.as_ref()))
            .await
            .map_err(|e| {
                HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Artifact parse failed")
                    .detail(join_error_message("artifact parse", e))
            })?
            .map_err(|e| {
                HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR)
                    .title("Built artifact did not parse as a source bundle")
                    .detail(e.to_string())
            })?;

    let expected_hash = extracted
        .with_manifest_version(repository_version)
        .compute_hash();

    Ok(BuiltArtifact {
        bytes: built,
        artifact_hash,
        identity_digest,
        source: extracted,
        expected_hash,
        dev_artifacts,
    })
}

fn verify_published_hash(
    result: &PublishResult,
    expected_hash: &ContentHash,
) -> Result<(), HttpApiProblem> {
    if result.hash.as_str() == expected_hash.as_str() {
        return Ok(());
    }
    Err(HttpApiProblem::new(http_api_problem::StatusCode::INTERNAL_SERVER_ERROR).detail(format!(
        "internal invariant: published hash {} != canonical hash {} from packaged artifact (same rules as insert_entry)",
        result.hash.as_str(),
        expected_hash.as_str()
    )))
}

async fn store_dev_artifacts(
    svc: &RepositoryService,
    hash: &ContentHash,
    artifacts: Option<baml_rt_repository::DevArtifactsBundle>,
) -> Result<(), HttpApiProblem> {
    if let Some(artifacts) = artifacts {
        svc.put_dev_artifacts(hash, &artifacts)
            .await
            .map_err(HttpApiProblem::from)?;
    }
    Ok(())
}

async fn build_artifact(
    source: SourceBundle,
    svc: Arc<RepositoryService>,
) -> anyhow::Result<(Vec<u8>, Option<baml_rt_repository::DevArtifactsBundle>)> {
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(build_artifact_inner(&source, svc)))
        .await
        .map_err(|e| anyhow::anyhow!("{}", join_error_message("artifact build", e)))?
}

async fn build_artifact_inner(
    source: &SourceBundle,
    svc: Arc<RepositoryService>,
) -> anyhow::Result<(Vec<u8>, Option<baml_rt_repository::DevArtifactsBundle>)> {
    let workspace = unique_temp_dir("baml-repository-publish");
    fs::create_dir_all(&workspace)?;

    let result = async {
        materialize_source_bundle(source, &workspace)?;
        let manifest_tools = source.manifest.tools();
        let mcp_tools: Vec<&str> = manifest_tools
            .iter()
            .copied()
            .filter(|tool| tool.starts_with("mcp/"))
            .collect();
        if !mcp_tools.is_empty() {
            tracing::info!(
                mcp_tools = ?mcp_tools,
                "repository build will resolve MCP tools from registry"
            );
        }

        let agent_dir = AgentDir::new(workspace.clone())?;
        let build_dir = BuildDir::new()?;
        let fs_impl = StdFileSystem;
        fs_impl.copy_dir_all(&agent_dir.baml_src(), &build_dir.join("baml_src"))?;
        let output = build_dir.join("package.tar.gz");
        let builder = BuilderService::new(
            TscCompiler::new(),
            RuntimeTypeGenerator::with_registry_service(svc),
            StdPackager::new(),
        );
        builder
            .build_package(&agent_dir, &build_dir, &output)
            .await
            .map_err(anyhow::Error::from)?;

        let bytes = fs::read(&output)?;
        let dev_artifacts = read_dev_artifacts(&workspace, build_dir.as_path());
        Ok::<_, anyhow::Error>((bytes, dev_artifacts))
    }
    .await;

    let _ = fs::remove_dir_all(&workspace);
    result
}

fn read_dev_artifacts(
    workspace: &Path,
    build_dir: &Path,
) -> Option<baml_rt_repository::DevArtifactsBundle> {
    let prelude = fs::read_to_string(workspace.join("baml_src/_baml_runtime.baml"))
        .or_else(|_| fs::read_to_string(build_dir.join("baml_src/_baml_runtime.baml")))
        .ok()?;
    let dts = fs::read_to_string(workspace.join("src/baml-runtime.d.ts"))
        .or_else(|_| fs::read_to_string(build_dir.join("src/baml-runtime.d.ts")))
        .ok()?;
    Some(baml_rt_repository::DevArtifactsBundle {
        baml_runtime: prelude,
        baml_runtime_dts: dts,
    })
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
