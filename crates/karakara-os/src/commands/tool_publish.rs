// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Build, audit, and publish a v2 TypeScript tool as a `.ktool` bundle.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use baml_rt_repository::http::PublishExternalToolResponse;
use baml_rt_tools::external_tools::{
    KTOOL_BUNDLE_SCHEMA_VERSION, KTOOL_DESCRIPTOR_PATH, KTOOL_MANIFEST_PATH, KTOOL_NODE_ENTRYPOINT,
    KTOOL_SBOM_PATH, KtoolManifest, KtoolRuntimeTier, MAX_KTOOL_UNPACKED_BYTES,
    UnifiedToolDescriptor, audit_ktool_bundle, runtime_image_for_tier,
};
use console::style;
use flate2::{Compression, GzBuilder};
use inquire::Confirm;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tar::{Builder, Header};

use super::{
    tool_build,
    utils::{AgentPlatform, RunnerToken, join_url},
};

const TOOL_PUBLISH_OP: &str = "Publish external tool";

pub fn run(
    dir: &str,
    repository_url: &str,
    approved_by: Option<&str>,
    yes: bool,
    trusted: bool,
    runner_token: Option<RunnerToken>,
) -> Result<()> {
    let project_dir = Path::new(dir);
    if !project_dir.is_dir() {
        bail!("Tool directory not found: {}", project_dir.display());
    }

    let tier = if trusted {
        KtoolRuntimeTier::Trusted
    } else {
        KtoolRuntimeTier::Sandbox
    };
    let built = build_bundle(project_dir, tier)?;
    print_review(&built.descriptor, &built.bundle_path);
    if trusted {
        println!(
            "  {}",
            style("tier: TRUSTED — runs UNSANDBOXED with full host access; the OS enforces no network/secret grants")
                .yellow()
        );
    }
    if !yes
        && !Confirm::new("Approve this descriptor and its requested capabilities?")
            .with_default(false)
            .prompt()?
    {
        bail!("Tool publish cancelled");
    }

    let approver = approved_by
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("USER")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "operator".to_string());
    let mut url = reqwest::Url::parse(&join_url(repository_url, "/external-tools/publish"))
        .context("invalid repository URL")?;
    url.query_pairs_mut().append_pair("approved_by", &approver);

    let response: PublishExternalToolResponse = AgentPlatform::new(runner_token)?.post_bytes(
        url.as_str(),
        "application/vnd.karakara.ktool+gzip",
        built.bytes,
        TOOL_PUBLISH_OP,
    )?;

    println!(
        "{} published {}",
        style("✓").green(),
        style(&response.tool_name).cyan()
    );
    println!("  registry version:  {}", response.version);
    println!(
        "  descriptor digest: {}",
        style(response.descriptor_digest).cyan()
    );
    println!(
        "  bundle digest:     {}",
        style(response.bundle_digest).cyan()
    );
    println!(
        "  tool digest:       {}",
        style(response.tool_digest).cyan()
    );
    println!(
        "  bundle:            {}",
        style(built.bundle_path.display()).dim()
    );
    Ok(())
}

struct BuiltBundle {
    descriptor: UnifiedToolDescriptor,
    bundle_path: PathBuf,
    bytes: Vec<u8>,
}

fn build_bundle(project_dir: &Path, tier: KtoolRuntimeTier) -> Result<BuiltBundle> {
    if !project_dir.join("package.json").is_file() {
        bail!("No package.json in {}", project_dir.display());
    }
    if !project_dir.join("node_modules").is_dir() {
        bail!(
            "Dependencies are not installed in {}. Run `npm install` first.",
            project_dir.display()
        );
    }

    tool_build::rebuild(project_dir)?;
    let descriptor_json = tool_build::extract_descriptor_json(project_dir)?;
    let (descriptor_digest, descriptor_pretty) = tool_build::finish_build(&descriptor_json)?;
    let descriptor: UnifiedToolDescriptor = serde_json::from_str(&descriptor_pretty)?;

    let descriptor_path = project_dir.join(KTOOL_DESCRIPTOR_PATH);
    fs::write(&descriptor_path, &descriptor_pretty)
        .with_context(|| format!("writing {}", descriptor_path.display()))?;
    // The lock file is written once, after packaging, with the full digest set
    // (below). Writing a descriptor-only lock here first would leave a partial
    // lock on disk if packaging fails.
    let lock_path = project_dir.join("tool.lock.json");

    let manifest = KtoolManifest {
        bundle_schema_version: KTOOL_BUNDLE_SCHEMA_VERSION,
        name: descriptor.name.clone(),
        runtime_tier: tier,
        runtime_image: runtime_image_for_tier(tier),
        entrypoint: KTOOL_NODE_ENTRYPOINT.to_string(),
        descriptor_digest,
        requested: descriptor.requested.clone(),
    };

    let mut files = BTreeMap::<String, (Vec<u8>, u32)>::new();
    add_bytes(
        &mut files,
        KTOOL_DESCRIPTOR_PATH,
        descriptor_pretty.into_bytes(),
        0o644,
    )?;
    add_bytes(
        &mut files,
        KTOOL_MANIFEST_PATH,
        format!("{}\n", serde_json::to_string_pretty(&manifest)?).into_bytes(),
        0o644,
    )?;
    add_bytes(
        &mut files,
        KTOOL_SBOM_PATH,
        render_sbom(project_dir, &descriptor, descriptor_digest)?,
        0o644,
    )?;
    add_bytes(
        &mut files,
        KTOOL_NODE_ENTRYPOINT,
        fs::read(project_dir.join("dist/tool.js")).context("reading dist/tool.js")?,
        0o644,
    )?;
    add_bytes(
        &mut files,
        "code/package.json",
        b"{\n  \"private\": true,\n  \"type\": \"module\"\n}\n".to_vec(),
        0o644,
    )?;

    let mut active_dirs = HashSet::new();
    collect_tree(
        &project_dir.join("node_modules"),
        "code/node_modules",
        &mut files,
        &mut active_dirs,
    )?;
    let unpacked: u64 = files.values().map(|(bytes, _)| bytes.len() as u64).sum();
    if unpacked > MAX_KTOOL_UNPACKED_BYTES {
        bail!(
            "Vendored tool is {unpacked} bytes unpacked; maximum is {MAX_KTOOL_UNPACKED_BYTES} bytes"
        );
    }

    let bytes = render_archive(files)?;
    let audited = audit_ktool_bundle(&bytes).context("host audit of generated .ktool failed")?;
    if audited.descriptor != descriptor {
        bail!("Generated .ktool descriptor changed during packaging");
    }
    fs::write(
        &lock_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "descriptor_digest": descriptor_digest,
                "runtime_image_digest": audited.manifest.runtime_image.digest,
                "bundle_digest": audited.bundle_digest,
                "tool_digest": audited.tool_digest,
            }))?
        ),
    )
    .with_context(|| format!("writing {}", lock_path.display()))?;

    let local_name = descriptor
        .name
        .split_once('/')
        .map(|(_, local)| local)
        .unwrap_or("tool");
    let bundle_path = project_dir.join("dist").join(format!("{local_name}.ktool"));
    fs::write(&bundle_path, &bytes)
        .with_context(|| format!("writing {}", bundle_path.display()))?;

    Ok(BuiltBundle {
        descriptor,
        bundle_path,
        bytes,
    })
}

fn add_bytes(
    files: &mut BTreeMap<String, (Vec<u8>, u32)>,
    path: &str,
    bytes: Vec<u8>,
    mode: u32,
) -> Result<()> {
    if files.insert(path.to_string(), (bytes, mode)).is_some() {
        bail!("Duplicate bundle path: {path}");
    }
    Ok(())
}

fn collect_tree(
    actual: &Path,
    logical: &str,
    files: &mut BTreeMap<String, (Vec<u8>, u32)>,
    active_dirs: &mut HashSet<PathBuf>,
) -> Result<()> {
    let metadata = fs::metadata(actual)
        .with_context(|| format!("reading vendored dependency {}", actual.display()))?;
    if metadata.is_file() {
        return add_bytes(
            files,
            logical,
            fs::read(actual).with_context(|| format!("reading {}", actual.display()))?,
            stable_mode(&metadata),
        );
    }
    if !metadata.is_dir() {
        bail!(
            "Unsupported file type in node_modules: {}",
            actual.display()
        );
    }

    let canonical = actual
        .canonicalize()
        .with_context(|| format!("resolving {}", actual.display()))?;
    if !active_dirs.insert(canonical.clone()) {
        bail!("Symlink cycle while vendoring {}", actual.display());
    }

    let mut entries = fs::read_dir(actual)
        .with_context(|| format!("reading {}", actual.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let child_logical = format!("{logical}/{name}");
        collect_tree(&entry.path(), &child_logical, files, active_dirs)?;
    }
    active_dirs.remove(&canonical);
    Ok(())
}

fn stable_mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 != 0 {
            return 0o755;
        }
    }
    0o644
}

fn render_archive(files: BTreeMap<String, (Vec<u8>, u32)>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let encoder = GzBuilder::new()
            .mtime(0)
            .write(&mut out, Compression::default());
        let mut archive = Builder::new(encoder);
        for (path, (bytes, mode)) in files {
            let mut header = Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(mode);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_cksum();
            archive
                .append_data(&mut header, path, bytes.as_slice())
                .context("writing .ktool archive")?;
        }
        let encoder = archive.into_inner().context("finishing .ktool tar")?;
        encoder.finish().context("finishing .ktool gzip")?;
    }
    Ok(out)
}

fn render_sbom(
    project_dir: &Path,
    descriptor: &UnifiedToolDescriptor,
    descriptor_digest: baml_rt_tools::mcp_snapshot::Digest,
) -> Result<Vec<u8>> {
    let package_lock = fs::read(project_dir.join("package-lock.json")).unwrap_or_default();
    let lock_digest = format!("{:x}", Sha256::digest(&package_lock));
    let namespace_digest = descriptor_digest
        .to_string()
        .trim_start_matches("sha256:")
        .to_string();
    let sbom = json!({
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": descriptor.name,
        "documentNamespace": format!("https://karakara.dev/spdx/{namespace_digest}"),
        "creationInfo": {
            "created": "1970-01-01T00:00:00Z",
            "creators": ["Tool: karak"]
        },
        "packages": [{
            "name": descriptor.name,
            "SPDXID": "SPDXRef-Tool",
            "versionInfo": descriptor.version,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": false,
            "checksums": [{"algorithm": "SHA256", "checksumValue": lock_digest}]
        }]
    });
    Ok(format!("{}\n", serde_json::to_string_pretty(&sbom)?).into_bytes())
}

fn print_review(descriptor: &UnifiedToolDescriptor, bundle_path: &Path) {
    println!("{} {}", style("Tool:").bold(), descriptor.name);
    println!("  version:     {}", descriptor.version);
    println!("  side effect: {:?}", descriptor.side_effect);
    println!("  output:      {:?}", descriptor.output_trust);
    if descriptor.requested.net.is_empty() {
        println!("  network:     none");
    } else {
        println!("  network:");
        for request in &descriptor.requested.net {
            println!("    - {}:{}", request.host, request.port);
        }
    }
    if descriptor.requested.secrets.is_empty() {
        println!("  secrets:     none");
    } else {
        println!("  secrets:");
        for request in &descriptor.requested.secrets {
            println!("    - {} ({:?})", request.name, request.binding);
        }
    }
    println!("  bundle:      {}", bundle_path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_output_is_deterministic() {
        let mut files = BTreeMap::new();
        add_bytes(&mut files, "b", b"two".to_vec(), 0o644).unwrap();
        add_bytes(&mut files, "a", b"one".to_vec(), 0o644).unwrap();
        assert_eq!(
            render_archive(files.clone()).unwrap(),
            render_archive(files).unwrap()
        );
    }
}
