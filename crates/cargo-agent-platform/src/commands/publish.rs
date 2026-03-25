//! `publish` subcommand — upload a built agent package (`.tar.gz`) and publish metadata.

use std::path::Path;

use anyhow::{Context, Result, bail};
use baml_rt_repository::{
    commands::{PublishCommand, PublishOrigin},
    entry::ChangeRationale,
    source_bundle_from_tar_gz,
};
use clap::ValueEnum;
use console::style;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum PublishOriginArg {
    Original,
    Iteration,
}

impl From<PublishOriginArg> for PublishOrigin {
    fn from(value: PublishOriginArg) -> Self {
        match value {
            PublishOriginArg::Original => PublishOrigin::Original,
            PublishOriginArg::Iteration => PublishOrigin::Iteration,
        }
    }
}

/// Upload and publish a prebuilt package tarball.
///
/// Flow:
/// 1. Read `package` bytes
/// 2. Compute `sha256(package_bytes)` lowercase hex (blob key)
/// 3. `PUT {repository_url}/blobs/{hash}` with raw bytes
/// 4. Extract source bundle from tarball
/// 5. `POST {repository_url}/publish` with `PublishCommand`
pub fn run(
    package: &str,
    repository_url: &str,
    rationale: &str,
    origin: PublishOriginArg,
) -> Result<()> {
    let package_path = Path::new(package);
    if !package_path.exists() {
        bail!("Package file not found: {}", package_path.display());
    }
    if !package_path.is_file() {
        bail!("Package path is not a file: {}", package_path.display());
    }
    if package_path.extension().and_then(|s| s.to_str()) != Some("gz") {
        bail!(
            "Expected a .tar.gz package path, got: {}",
            package_path.display()
        );
    }

    let bytes = std::fs::read(package_path)
        .with_context(|| format!("Failed to read package {}", package_path.display()))?;
    if bytes.is_empty() {
        bail!("Package file is empty: {}", package_path.display());
    }

    let hash = sha256_hex(&bytes);
    let (name, source) = source_bundle_from_tar_gz(&bytes).context("Invalid package archive")?;
    let rationale = ChangeRationale::new(rationale.to_string()).context("Invalid rationale")?;
    let origin: PublishOrigin = origin.into();
    let publish_cmd = PublishCommand {
        name,
        source,
        rationale,
        origin,
    };

    let base = repository_url.trim_end_matches('/');
    let put_url = format!("{base}/blobs/{hash}");
    let publish_url = format!("{base}/publish");

    let rt = tokio::runtime::Runtime::new().context("Failed to create async runtime")?;
    let publish_result = rt.block_on(async {
        let client = reqwest::Client::new();
        let blob_resp = client
            .put(&put_url)
            .header("content-type", "application/gzip")
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("Failed to PUT blob to {put_url}"))?;

        let blob_status = blob_resp.status();
        if blob_status != StatusCode::OK && blob_status != StatusCode::CREATED {
            let body = blob_resp.text().await.unwrap_or_default();
            bail!("Blob upload failed ({blob_status}) at {put_url}: {body}");
        }

        let resp = client
            .post(&publish_url)
            .header("content-type", "application/json")
            .json(&publish_cmd)
            .send()
            .await
            .with_context(|| format!("Failed to POST publish to {publish_url}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Metadata publish failed ({status}) at {publish_url}: {body}");
        }
        Ok::<String, anyhow::Error>(body)
    })?;

    println!("{}", style("Blob uploaded successfully.").green().bold());
    println!("  package: {}", style(package_path.display()).cyan());
    println!("  hash:    {}", style(&hash).cyan());
    println!("  url:     {}", style(put_url).dim());
    println!();
    println!(
        "{}",
        style("Metadata published successfully.").green().bold()
    );
    println!("  url:     {}", style(publish_url).dim());
    println!("  result:  {}", style(publish_result).dim());

    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
