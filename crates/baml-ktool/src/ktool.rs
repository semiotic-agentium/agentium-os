// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Portable `.ktool` bundle metadata, digest identity, load-time audit, and
//! guarded unpacking.
//!
//! A bundle is a deterministic tar.gz containing the generated descriptor,
//! requests-only manifest, SBOM, and vendored executable bytes. The host reads
//! and validates the descriptor without executing tool code; the guest
//! supervisor re-verifies the same bytes and unpacks them behind the same
//! guards.

use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tar::Archive;

use crate::{
    descriptor::{CapabilityRequest, ToolDescriptor, compute_descriptor_digest},
    digest::{Digest, canonical_digest},
    error::{KtoolError, KtoolResult, invalid},
};

pub const KTOOL_BUNDLE_SCHEMA_VERSION: u32 = 2;
pub const KTOOL_MANIFEST_PATH: &str = "ktool.json";
pub const KTOOL_TOOLS_PATH: &str = "tools.json";
pub const KTOOL_DESCRIPTOR_PATH: &str = "tool-descriptor.json";
pub const KTOOL_SBOM_PATH: &str = "sbom.json";
pub const KTOOL_NODE_ENTRYPOINT: &str = "code/tool.js";
pub const MAX_KTOOL_BUNDLE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_KTOOL_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;

/// Built-in P2 runtime-image registry entry for the blessed Node path.
///
/// The digest covers the platform runtime contract. Replacing the image or its
/// supervisor/runtime ABI creates a new registry digest and therefore a new
/// tool digest.
pub fn default_node_runtime_image() -> KtoolRuntimeImage {
    let record = serde_json::json!({
        "name": "node",
        "registry_version": 1,
        "node_major": 22,
        "protocol": 2,
        "supervisor_abi": 1,
    });
    KtoolRuntimeImage {
        name: "node".to_string(),
        digest: canonical_digest(&record),
    }
}

/// Runtime-image sentinel for the `Trusted` tier (external tools v2, P5.3).
///
/// A `Trusted` tool runs unsandboxed in the operator's host environment, not a
/// pinned OCI microVM image, so there is no content hash of a rootfs to bind.
/// The sentinel keeps the one tool-digest formula
/// (`sha256(bundle || runtime_image_digest)`) intact while signalling "host
/// environment, operator-vouched". The host environment is the operator's trust
/// problem (§3.2), not something the digest can attest.
pub fn host_native_runtime_image() -> KtoolRuntimeImage {
    let record = serde_json::json!({
        "name": "host-native",
        "registry_version": 1,
        "trusted": true,
    });
    KtoolRuntimeImage {
        name: "host-native".to_string(),
        digest: canonical_digest(&record),
    }
}

/// The runtime selected from the platform runtime-image registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KtoolRuntimeImage {
    /// Registry key, for example `node`.
    pub name: String,
    /// Content digest of the runtime image registry entry.
    pub digest: Digest,
}

/// Requests-only metadata embedded in a `.ktool`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KtoolManifest {
    pub bundle_schema_version: u32,
    /// Toolset name. Individual tools are identified by `name` in `tools`.
    pub name: String,
    pub version: semver::Version,
    pub runtime_tier: KtoolRuntimeTier,
    pub runtime_image: KtoolRuntimeImage,
    /// MCP server argv, relative to the unpacked bundle root. `node` and
    /// `python3` are allowed as image interpreters.
    pub command: Vec<String>,
    pub requested: CapabilityRequest,
    pub tools: Vec<KtoolToolEntry>,
    /// How the OS obtains this toolset's supervisor on its node (P5.5). Only
    /// meaningful for the `Trusted` tier; absent ⇒ the OS spawns its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_launch: Option<KtoolTrustedLaunch>,
}

/// Placement declaration for a `Trusted` toolset's supervisor (§P5.3).
///
/// This is a scheduling fact, not authority: the host still requires an
/// operator-trust record before the process runs, and deploy fails when the
/// declared node or socket is unreachable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum KtoolTrustedLaunch {
    /// The OS spawns `baml-tool-supervisor --tier trusted` for this toolset.
    Spawn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<String>,
    },
    /// The operator already runs the supervisor; the OS attaches to its socket.
    Attach {
        /// Host-local stream socket the operator's daemon listens on.
        socket_path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<String>,
    },
}

impl KtoolTrustedLaunch {
    /// Node this toolset is pinned to. `None` means the local node.
    pub fn node(&self) -> Option<&str> {
        match self {
            Self::Spawn { node } | Self::Attach { node, .. } => node.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KtoolToolEntry {
    pub name: String,
    pub descriptor_digest: Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KtoolRuntimeTier {
    /// The blessed portable path: runs in a shared microVM (node OCI image).
    Sandbox,
    /// Operator-vouched, unsandboxed host process (`host_native` image). §3.2.
    Trusted,
}

/// Result of auditing a bundle's inert metadata and exact bytes.
#[derive(Debug, Clone)]
pub struct AuditedKtoolBundle {
    /// Compatibility view for callers that still consume a single-tool
    /// snapshot. New code must use `descriptors`.
    pub descriptor: ToolDescriptor,
    pub descriptors: Vec<ToolDescriptor>,
    pub manifest: KtoolManifest,
    pub bundle_digest: Digest,
    pub tool_digest: Digest,
}

/// Compute the exact bundle-byte digest used as the repository blob key.
pub fn compute_bundle_digest(bytes: &[u8]) -> Digest {
    Digest::from_bytes(Sha256::digest(bytes).into())
}

/// Compute executable identity from exact bundle bytes and runtime image digest.
pub fn compute_tool_digest_v2(bytes: &[u8], runtime_image_digest: Digest) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.update(runtime_image_digest.as_bytes());
    Digest::from_bytes(hasher.finalize().into())
}

/// Audit a `.ktool` without running it.
///
/// This rejects links, unsafe paths, duplicate archive entries, oversized
/// unpacked content, missing required files, and any descriptor/manifest drift.
pub fn audit_ktool_bundle(bytes: &[u8]) -> KtoolResult<AuditedKtoolBundle> {
    if bytes.len() > MAX_KTOOL_BUNDLE_BYTES {
        return Err(invalid(format!(
            ".ktool is {} bytes; maximum is {} bytes",
            bytes.len(),
            MAX_KTOOL_BUNDLE_BYTES
        )));
    }

    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|e| invalid(format!("failed to read .ktool archive: {e}")))?;

    let mut seen = HashSet::new();
    let mut unpacked = 0_u64;
    let mut tools_json = None;
    let mut manifest_json = None;
    let mut has_sbom = false;
    let mut files = HashSet::new();

    for entry in entries {
        let mut entry = entry.map_err(|e| invalid(format!("invalid .ktool entry: {e}")))?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            continue;
        }
        if !entry_type.is_file() {
            return Err(invalid(
                ".ktool may contain only regular files and directories",
            ));
        }

        let path = entry
            .path()
            .map_err(|e| invalid(format!("invalid .ktool path: {e}")))?;
        validate_archive_path(&path)?;
        let path = path.to_string_lossy().replace('\\', "/");
        if !seen.insert(path.clone()) {
            return Err(invalid(format!("duplicate .ktool entry: {path}")));
        }

        let size = entry.header().size().unwrap_or(0);
        unpacked = unpacked.saturating_add(size);
        if unpacked > MAX_KTOOL_UNPACKED_BYTES {
            return Err(invalid(format!(
                ".ktool unpacked size exceeds {} bytes",
                MAX_KTOOL_UNPACKED_BYTES
            )));
        }

        match path.as_str() {
            KTOOL_DESCRIPTOR_PATH => {
                let _ = read_entry(&mut entry, &path)?;
            }
            KTOOL_TOOLS_PATH => tools_json = Some(read_entry(&mut entry, &path)?),
            KTOOL_MANIFEST_PATH => manifest_json = Some(read_entry(&mut entry, &path)?),
            KTOOL_SBOM_PATH => {
                let _: serde_json::Value = serde_json::from_slice(&read_entry(&mut entry, &path)?)
                    .map_err(|e| invalid(format!("invalid {KTOOL_SBOM_PATH}: {e}")))?;
                has_sbom = true;
            }
            _ => {
                files.insert(path);
            }
        }
    }

    let descriptors: Vec<ToolDescriptor> = serde_json::from_slice(
        &tools_json.ok_or_else(|| invalid(format!("missing {KTOOL_TOOLS_PATH}")))?,
    )
    .map_err(|e| invalid(format!("invalid {KTOOL_TOOLS_PATH}: {e}")))?;
    let manifest: KtoolManifest = serde_json::from_slice(
        &manifest_json.ok_or_else(|| invalid(format!("missing {KTOOL_MANIFEST_PATH}")))?,
    )
    .map_err(|e| invalid(format!("invalid {KTOOL_MANIFEST_PATH}: {e}")))?;

    if !has_sbom {
        return Err(invalid(format!("missing {KTOOL_SBOM_PATH}")));
    }
    if manifest.bundle_schema_version != KTOOL_BUNDLE_SCHEMA_VERSION {
        return Err(invalid(format!(
            "unsupported .ktool schema version {} (expected {})",
            manifest.bundle_schema_version, KTOOL_BUNDLE_SCHEMA_VERSION
        )));
    }
    if descriptors.is_empty() || manifest.tools.len() != descriptors.len() {
        return Err(invalid(
            "tools.json and manifest.tools must contain the same non-empty set",
        ));
    }
    let mut names = HashSet::new();
    for pair in descriptors.windows(2) {
        if pair[0].name >= pair[1].name {
            return Err(invalid("tools.json must be sorted by unique tool name"));
        }
    }
    for descriptor in &descriptors {
        if !names.insert(descriptor.name.clone()) {
            return Err(invalid(format!("duplicate tool name: {}", descriptor.name)));
        }
    }
    let mut manifest_tools = manifest.tools.clone();
    manifest_tools.sort_by(|a, b| a.name.cmp(&b.name));
    let mut expected_tools = descriptors
        .iter()
        .map(|descriptor| KtoolToolEntry {
            name: descriptor.name.clone(),
            descriptor_digest: compute_descriptor_digest(descriptor),
        })
        .collect::<Vec<_>>();
    expected_tools.sort_by(|a, b| a.name.cmp(&b.name));
    if manifest_tools != expected_tools {
        return Err(invalid(
            "tools.json and manifest.tools descriptor digests do not match",
        ));
    }
    if manifest.command.is_empty() {
        return Err(invalid("ktool command must not be empty"));
    }
    let command_exists = files.contains(&manifest.command[0]);
    let interpreter = matches!(manifest.command[0].as_str(), "node" | "python3");
    if !command_exists && !interpreter {
        return Err(invalid(format!(
            "command[0] '{}' is not present in the bundle or a supported image interpreter",
            manifest.command[0]
        )));
    }
    // Each tier pins exactly one registered runtime image. Trusted uses the
    // host-native sentinel (unsandboxed); Sandbox uses the node OCI image. A
    // cross-tier image (e.g. Trusted claiming the node image, or Sandbox
    // claiming host-native) is rejected here.
    let registered_runtime = match manifest.runtime_tier {
        KtoolRuntimeTier::Sandbox => default_node_runtime_image(),
        KtoolRuntimeTier::Trusted => host_native_runtime_image(),
    };
    if manifest.runtime_image != registered_runtime {
        return Err(invalid(format!(
            "runtime image is not registered for the {:?} tier: expected {}@{}, got {}@{}",
            manifest.runtime_tier,
            registered_runtime.name,
            registered_runtime.digest,
            manifest.runtime_image.name,
            manifest.runtime_image.digest
        )));
    }
    // A launch declaration on a tier the OS runs itself would be a lie about
    // placement. Only `Trusted` tools can be pinned or attached.
    if manifest.trusted_launch.is_some() && manifest.runtime_tier != KtoolRuntimeTier::Trusted {
        return Err(invalid(
            "trusted_launch is only valid for the trusted runtime tier",
        ));
    }
    if let Some(KtoolTrustedLaunch::Attach { socket_path, .. }) = &manifest.trusted_launch
        && !socket_path.is_absolute()
    {
        return Err(invalid(
            "trusted_launch attach socket_path must be absolute",
        ));
    }
    let bundle_name = manifest.name.clone();
    if descriptors.iter().any(|descriptor| {
        descriptor
            .name
            .split_once('/')
            .is_some_and(|(bundle, _)| bundle != bundle_name.as_str())
    }) {
        return Err(invalid(
            "tool names must use the manifest toolset name as their bundle prefix",
        ));
    }

    let bundle_digest = compute_bundle_digest(bytes);
    let tool_digest = compute_tool_digest_v2(bytes, manifest.runtime_image.digest);
    let descriptor = descriptors
        .first()
        .cloned()
        .ok_or_else(|| invalid("tools.json must contain at least one descriptor"))?;
    Ok(AuditedKtoolBundle {
        descriptor,
        descriptors,
        manifest,
        bundle_digest,
        tool_digest,
    })
}

/// A bundle audited, identity-checked, and unpacked into a private directory.
#[derive(Debug)]
pub struct UnpackedKtool {
    pub descriptor: ToolDescriptor,
    pub descriptors: Vec<ToolDescriptor>,
    pub manifest: KtoolManifest,
    pub bundle_digest: Digest,
    pub tool_digest: Digest,
    /// `dest`, after extraction.
    pub dir: PathBuf,
}

/// Verify identity and unpack `bytes` into `dest`.
///
/// Defense-in-depth for the guest: the host already audited the bundle, but the
/// supervisor re-checks that the exact bytes it received hash to the announced
/// `tool_digest`, and re-runs every archive guard before writing.
///
/// `dest` is treated as owned by this call: an existing directory is removed
/// first, then recreated. Callers must pass a path under their private root.
pub fn unpack_ktool_bundle(
    bytes: &[u8],
    expected_tool_digest: &str,
    expected_runtime_image_digest: &str,
    dest: &Path,
    max_unpacked_bytes: u64,
) -> KtoolResult<UnpackedKtool> {
    let audited = audit_ktool_bundle(bytes)?;
    if audited.manifest.runtime_image.digest.to_string() != expected_runtime_image_digest {
        return Err(invalid(
            "bundle runtime image does not match the load request",
        ));
    }
    // The header value must equal the manifest value; compute identity from
    // the exact bytes with the manifest's registered image digest.
    let computed = compute_tool_digest_v2(bytes, audited.manifest.runtime_image.digest);
    if computed.to_string() != expected_tool_digest {
        return Err(invalid(format!(
            "bundle identity mismatch: requested {expected_tool_digest}, computed {computed}"
        )));
    }

    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    fs::create_dir_all(dest)?;

    if let Err(error) = extract_entries(bytes, dest, max_unpacked_bytes) {
        // Never leave a partially unpacked tree behind.
        let _ = fs::remove_dir_all(dest);
        return Err(error);
    }

    Ok(UnpackedKtool {
        descriptor: audited.descriptor,
        descriptors: audited.descriptors,
        manifest: audited.manifest,
        bundle_digest: audited.bundle_digest,
        tool_digest: audited.tool_digest,
        dir: dest.to_path_buf(),
    })
}

/// One file to place in a generated bundle.
#[derive(Debug, Clone)]
pub struct BundleFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub mode: u32,
}

impl BundleFile {
    pub fn new(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
            mode: 0o644,
        }
    }
}

/// Build a deterministic `.ktool` from a descriptor and its executable files.
///
/// Used by the DevBackend (`karak tool test`) and tests; the publish path keeps
/// its richer SPDX SBOM and tree walk.
pub fn build_ktool_bundle(
    descriptor: &ToolDescriptor,
    entrypoint: impl Into<Vec<u8>>,
    extra: Vec<BundleFile>,
    sbom: impl Into<Vec<u8>>,
) -> KtoolResult<Vec<u8>> {
    build_ktool_bundle_for_tier(
        descriptor,
        KtoolRuntimeTier::Sandbox,
        entrypoint,
        extra,
        sbom,
    )
}

/// The registered runtime image for a tier (the audit pins exactly this).
pub fn runtime_image_for_tier(tier: KtoolRuntimeTier) -> KtoolRuntimeImage {
    match tier {
        KtoolRuntimeTier::Sandbox => default_node_runtime_image(),
        KtoolRuntimeTier::Trusted => host_native_runtime_image(),
    }
}

/// Build a deterministic `.ktool` for a specific runtime tier. Both tiers run
/// the node entrypoint; they differ in where they run (microVM vs operator host)
/// and therefore in the registered runtime image.
pub fn build_ktool_bundle_for_tier(
    descriptor: &ToolDescriptor,
    tier: KtoolRuntimeTier,
    entrypoint: impl Into<Vec<u8>>,
    extra: Vec<BundleFile>,
    sbom: impl Into<Vec<u8>>,
) -> KtoolResult<Vec<u8>> {
    let toolset_name = descriptor
        .name
        .split_once('/')
        .map(|(bundle, _)| bundle.to_string())
        .unwrap_or_else(|| descriptor.name.clone());
    let manifest = KtoolManifest {
        bundle_schema_version: KTOOL_BUNDLE_SCHEMA_VERSION,
        name: toolset_name,
        version: semver::Version::new(0, 1, 0),
        runtime_tier: tier,
        runtime_image: runtime_image_for_tier(tier),
        command: vec!["node".to_string(), KTOOL_NODE_ENTRYPOINT.to_string()],
        // R1 (§R1.1): capabilities are toolset-level and live only on the
        // manifest. The convenience builders produce a no-capability toolset;
        // the publish path sets `requested` explicitly from `karakara.toml`.
        requested: CapabilityRequest::default(),
        tools: vec![KtoolToolEntry {
            name: descriptor.name.clone(),
            descriptor_digest: compute_descriptor_digest(descriptor),
        }],
        // The convenience builder never declares placement; the publish path
        // sets it from `karakara.toml`.
        trusted_launch: None,
    };

    let mut files = vec![
        // Keep the singular descriptor as a harmless compatibility aid for
        // local tooling. Audit identity comes only from tools.json.
        BundleFile::new(
            KTOOL_DESCRIPTOR_PATH,
            serde_json::to_vec(descriptor).map_err(|e| invalid(e.to_string()))?,
        ),
        BundleFile::new(
            KTOOL_MANIFEST_PATH,
            serde_json::to_vec(&manifest).map_err(|e| invalid(e.to_string()))?,
        ),
        BundleFile::new(
            KTOOL_TOOLS_PATH,
            serde_json::to_vec(&vec![descriptor]).map_err(|e| invalid(e.to_string()))?,
        ),
        BundleFile::new(KTOOL_SBOM_PATH, sbom),
        BundleFile::new(KTOOL_NODE_ENTRYPOINT, entrypoint),
    ];
    files.extend(extra);
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut seen = HashSet::new();
    let mut total = 0_u64;
    for file in &files {
        let path = Path::new(&file.path);
        validate_archive_path(path)?;
        let key = path.to_string_lossy().replace('\\', "/");
        if !seen.insert(key.clone()) {
            return Err(invalid(format!("duplicate bundle file: {key}")));
        }
        total = total.saturating_add(file.bytes.len() as u64);
        if total > MAX_KTOOL_UNPACKED_BYTES {
            return Err(invalid(format!(
                "bundle unpacked size exceeds {MAX_KTOOL_UNPACKED_BYTES} bytes"
            )));
        }
    }

    let mut out = Vec::new();
    {
        let encoder = flate2::GzBuilder::new()
            .mtime(0)
            .write(&mut out, flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for file in &files {
            let mut header = tar::Header::new_gnu();
            header.set_size(file.bytes.len() as u64);
            header.set_mode(file.mode);
            header.set_cksum();
            archive
                .append_data(&mut header, &file.path, file.bytes.as_slice())
                .map_err(|e| invalid(format!("failed to append {}: {e}", file.path)))?;
        }
        archive
            .into_inner()
            .map_err(|e| invalid(format!("failed to finish bundle archive: {e}")))?
            .finish()
            .map_err(|e| invalid(format!("failed to finish bundle compression: {e}")))?;
    }
    // Fail closed: never hand out a bundle the host would reject.
    audit_ktool_bundle(&out)?;
    Ok(out)
}

fn extract_entries(bytes: &[u8], dest: &Path, max_unpacked_bytes: u64) -> KtoolResult<()> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|e| invalid(format!("failed to read .ktool archive: {e}")))?;

    let mut seen = HashSet::new();
    let mut unpacked = 0_u64;
    for entry in entries {
        let mut entry = entry.map_err(|e| invalid(format!("invalid .ktool entry: {e}")))?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            continue;
        }
        if !entry_type.is_file() {
            return Err(invalid(
                ".ktool may contain only regular files and directories",
            ));
        }
        let relative = entry
            .path()
            .map_err(|e| invalid(format!("invalid .ktool path: {e}")))?
            .into_owned();
        validate_archive_path(&relative)?;
        let key = relative.to_string_lossy().replace('\\', "/");
        if !seen.insert(key.clone()) {
            return Err(invalid(format!("duplicate .ktool entry: {key}")));
        }

        let declared = entry.header().size().unwrap_or(0);
        unpacked = unpacked.saturating_add(declared);
        if unpacked > max_unpacked_bytes {
            return Err(invalid(format!(
                ".ktool unpacked size exceeds {max_unpacked_bytes} bytes"
            )));
        }

        let target = dest.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // create_new refuses to follow a pre-existing symlink or overwrite a
        // duplicate entry. Modes are sanitised: no setuid/setgid/sticky bits.
        let mode = entry.header().mode().unwrap_or(0o644) & 0o777;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&target)?;
        std::io::copy(&mut (&mut entry).take(declared), &mut file)?;
        file.flush()?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

fn read_entry<R: Read>(entry: &mut R, path: &str) -> KtoolResult<Vec<u8>> {
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .map_err(|e| invalid(format!("failed to read {path}: {e}")))?;
    Ok(bytes)
}

pub(crate) fn validate_archive_path(path: &Path) -> KtoolResult<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(invalid(format!(
            "unsafe .ktool archive path: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Map a bundle error into a printable host message without leaking internals.
pub fn error_message(error: &KtoolError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use baml_sandbox_protocol::ErrorClass;
    use serde_json::json;

    use super::*;
    use crate::descriptor::{DESCRIPTOR_ABI, OutputTrust, SideEffect};

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor {
            descriptor_abi: DESCRIPTOR_ABI,
            name: "support/echo".into(),
            version: "0.1.0".into(),
            description: "echo".into(),
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            error_classes: vec![ErrorClass::Execution],
            side_effect: SideEffect::Read,
            idempotent: true,
            output_trust: OutputTrust::Untrusted,
        }
    }

    fn bundle() -> Vec<u8> {
        build_ktool_bundle(
            &descriptor(),
            b"export default {};".to_vec(),
            vec![],
            br#"{}"#.to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn audits_valid_bundle_and_computes_identities() {
        let bytes = bundle();
        let audited = audit_ktool_bundle(&bytes).unwrap();
        assert_eq!(audited.descriptor.name, "support/echo");
        assert_eq!(audited.bundle_digest, compute_bundle_digest(&bytes));
        assert_eq!(
            audited.tool_digest,
            compute_tool_digest_v2(&bytes, audited.manifest.runtime_image.digest)
        );
    }

    #[test]
    fn trusted_launch_round_trips_and_changes_identity() {
        let bytes = build_ktool_bundle_for_tier(
            &descriptor(),
            KtoolRuntimeTier::Trusted,
            b"export default {};".to_vec(),
            vec![],
            br#"{}"#.to_vec(),
        )
        .unwrap();
        let plain = audit_ktool_bundle(&bytes).unwrap();
        let declared = rewrite_manifest(&bytes, |manifest| {
            manifest.trusted_launch = Some(KtoolTrustedLaunch::Attach {
                socket_path: PathBuf::from("/run/karakara/gpu.sock"),
                node: Some("gpu-1".into()),
            });
        });
        let audited = audit_ktool_bundle(&declared).unwrap();

        let launch = audited.manifest.trusted_launch.clone().unwrap();
        assert_eq!(launch.node(), Some("gpu-1"));
        assert!(matches!(
            launch,
            KtoolTrustedLaunch::Attach { ref socket_path, .. }
                if socket_path == Path::new("/run/karakara/gpu.sock")
        ));
        // Placement is part of the artifact's declared contract, so it is part
        // of the executable identity the operator approves.
        assert_ne!(plain.tool_digest, audited.tool_digest);
    }

    #[test]
    fn rejects_trusted_launch_on_sandbox_tier() {
        let bytes = bundle();
        let tampered = rewrite_manifest(&bytes, |manifest| {
            manifest.trusted_launch = Some(KtoolTrustedLaunch::Spawn { node: None });
        });
        let err = audit_ktool_bundle(&tampered).unwrap_err();
        assert!(
            err.to_string()
                .contains("only valid for the trusted runtime tier"),
            "{err}"
        );
    }

    #[test]
    fn rejects_relative_attach_socket() {
        let bytes = build_ktool_bundle_for_tier(
            &descriptor(),
            KtoolRuntimeTier::Trusted,
            b"export default {};".to_vec(),
            vec![],
            br#"{}"#.to_vec(),
        )
        .unwrap();
        let tampered = rewrite_manifest(&bytes, |manifest| {
            manifest.trusted_launch = Some(KtoolTrustedLaunch::Attach {
                socket_path: PathBuf::from("relative.sock"),
                node: None,
            });
        });
        let err = audit_ktool_bundle(&tampered).unwrap_err();
        assert!(err.to_string().contains("must be absolute"), "{err}");
    }

    #[test]
    fn rejects_descriptor_drift() {
        let mut descriptor = descriptor();
        descriptor.side_effect = SideEffect::Irreversible;
        let bytes = build_ktool_bundle(
            &descriptor,
            b"export default {};".to_vec(),
            vec![],
            br#"{}"#.to_vec(),
        )
        .unwrap();
        // Rewrite the manifest digest while keeping descriptor bytes intact.
        let bytes = rewrite_manifest(&bytes, |manifest| {
            manifest.tools[0].descriptor_digest = Digest::new(format!("sha256:{}", "f".repeat(64)));
        });
        let err = audit_ktool_bundle(&bytes).unwrap_err();
        assert!(
            err.to_string()
                .contains("tools.json and manifest.tools descriptor digests do not match"),
            "unexpected audit error: {err}"
        );
    }

    #[test]
    fn unpacks_into_the_requested_directory() {
        let bytes = bundle();
        let audited = audit_ktool_bundle(&bytes).unwrap();
        let root = std::env::temp_dir().join(format!("ktool-unpack-{}", uuid_like()));
        let dest = root.join(audited.tool_digest.to_string().replace(':', "-"));
        let unpacked = unpack_ktool_bundle(
            &bytes,
            &audited.tool_digest.to_string(),
            &audited.manifest.runtime_image.digest.to_string(),
            &dest,
            MAX_KTOOL_UNPACKED_BYTES,
        )
        .unwrap();
        assert!(unpacked.dir.join(KTOOL_NODE_ENTRYPOINT).is_file());
        assert!(unpacked.dir.join(KTOOL_MANIFEST_PATH).is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_wrong_identity() {
        let bytes = bundle();
        let dir = std::env::temp_dir().join(format!("ktool-id-{}", uuid_like()));
        let err = unpack_ktool_bundle(
            &bytes,
            &format!("sha256:{}", "a".repeat(64)),
            &default_node_runtime_image().digest.to_string(),
            &dir,
            1024,
        )
        .unwrap_err();
        assert!(err.to_string().contains("identity mismatch"));
    }

    #[test]
    fn rejects_path_traversal_and_links() {
        let descriptor = descriptor();
        for (path, body) in [
            ("../escape", b"x".to_vec()),
            ("/absolute", b"x".to_vec()),
            ("a/../../escape", b"x".to_vec()),
        ] {
            let err = build_ktool_bundle(
                &descriptor,
                b"export default {};".to_vec(),
                vec![BundleFile::new(path, body)],
                br#"{}"#.to_vec(),
            )
            .unwrap_err();
            assert!(err.to_string().contains("unsafe"), "{path}: {err}");
        }
    }

    #[test]
    fn rejects_duplicate_entries() {
        let bytes = bundle();
        let tampered = duplicate_entry(&bytes, KTOOL_NODE_ENTRYPOINT);
        let err = audit_ktool_bundle(&tampered).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn guest_unpack_cap_is_enforced() {
        let bytes = bundle();
        let audited = audit_ktool_bundle(&bytes).unwrap();
        let dir = std::env::temp_dir().join(format!("ktool-cap-{}", uuid_like()));
        let err = unpack_ktool_bundle(
            &bytes,
            &audited.tool_digest.to_string(),
            &audited.manifest.runtime_image.digest.to_string(),
            &dir,
            16,
        )
        .unwrap_err();
        assert!(err.to_string().contains("exceeds 16 bytes"), "{err}");
        assert!(
            !dir.exists(),
            "a rejected unpack must not leave a directory"
        );
    }

    #[test]
    fn audits_trusted_tier_with_host_native_image() {
        let bytes = build_ktool_bundle_for_tier(
            &descriptor(),
            KtoolRuntimeTier::Trusted,
            b"export default {};".to_vec(),
            vec![],
            br#"{}"#.to_vec(),
        )
        .unwrap();
        let audited = audit_ktool_bundle(&bytes).unwrap();
        assert_eq!(audited.manifest.runtime_tier, KtoolRuntimeTier::Trusted);
        assert_eq!(audited.manifest.runtime_image, host_native_runtime_image());
        assert_eq!(
            audited.tool_digest,
            compute_tool_digest_v2(&bytes, host_native_runtime_image().digest)
        );
    }

    #[test]
    fn trusted_and_sandbox_identities_differ() {
        let sandbox = build_ktool_bundle_for_tier(
            &descriptor(),
            KtoolRuntimeTier::Sandbox,
            b"export default {};".to_vec(),
            vec![],
            br#"{}"#.to_vec(),
        )
        .unwrap();
        let trusted = build_ktool_bundle_for_tier(
            &descriptor(),
            KtoolRuntimeTier::Trusted,
            b"export default {};".to_vec(),
            vec![],
            br#"{}"#.to_vec(),
        )
        .unwrap();
        // The host-native sentinel differs from the node image, so the tier is
        // folded into the tool digest: the same bytes are two distinct tools.
        assert_ne!(
            audit_ktool_bundle(&sandbox).unwrap().tool_digest,
            audit_ktool_bundle(&trusted).unwrap().tool_digest
        );
    }

    #[test]
    fn rejects_cross_tier_runtime_image() {
        // Hand-build a bundle that claims the Trusted tier but the node image.
        let descriptor = descriptor();
        let manifest = KtoolManifest {
            bundle_schema_version: KTOOL_BUNDLE_SCHEMA_VERSION,
            name: "support".into(),
            version: semver::Version::new(0, 1, 0),
            runtime_tier: KtoolRuntimeTier::Trusted,
            runtime_image: default_node_runtime_image(),
            command: vec!["node".into(), KTOOL_NODE_ENTRYPOINT.into()],
            requested: CapabilityRequest::default(),
            tools: vec![KtoolToolEntry {
                name: descriptor.name.clone(),
                descriptor_digest: compute_descriptor_digest(&descriptor),
            }],
            trusted_launch: None,
        };
        let files = vec![
            BundleFile::new(
                KTOOL_DESCRIPTOR_PATH,
                serde_json::to_vec(&descriptor).unwrap(),
            ),
            BundleFile::new(KTOOL_MANIFEST_PATH, serde_json::to_vec(&manifest).unwrap()),
            BundleFile::new(
                KTOOL_TOOLS_PATH,
                serde_json::to_vec(&vec![descriptor]).unwrap(),
            ),
            BundleFile::new(KTOOL_SBOM_PATH, b"{}".to_vec()),
            BundleFile::new(KTOOL_NODE_ENTRYPOINT, b"export default {};".to_vec()),
        ];
        let mut out = Vec::new();
        {
            let encoder = flate2::GzBuilder::new()
                .mtime(0)
                .write(&mut out, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            for file in &files {
                let mut header = tar::Header::new_gnu();
                header.set_size(file.bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive
                    .append_data(&mut header, &file.path, file.bytes.as_slice())
                    .unwrap();
            }
            archive.into_inner().unwrap().finish().unwrap();
        }
        let err = audit_ktool_bundle(&out).unwrap_err();
        assert!(
            err.to_string()
                .contains("not registered for the Trusted tier"),
            "{err}"
        );
    }

    #[test]
    fn rejects_symlink_entries() {
        let bytes = bundle();
        let tampered = append_symlink(&bytes, "code/link", "/etc/passwd");
        let err = audit_ktool_bundle(&tampered).unwrap_err();
        assert!(err.to_string().contains("regular files"), "{err}");
    }

    fn uuid_like() -> u128 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    /// Re-open a generated archive and patch `manifest.json` for negative tests.
    fn rewrite_manifest(bytes: &[u8], patch: impl FnOnce(&mut KtoolManifest)) -> Vec<u8> {
        let mut patch = Some(patch);
        let decoder = GzDecoder::new(Cursor::new(bytes));
        let mut archive = Archive::new(decoder);
        let mut files = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            let mut body = Vec::new();
            entry.read_to_end(&mut body).unwrap();
            if path == KTOOL_MANIFEST_PATH {
                let mut manifest: KtoolManifest = serde_json::from_slice(&body).unwrap();
                if let Some(patch) = patch.take() {
                    patch(&mut manifest);
                }
                body = serde_json::to_vec(&manifest).unwrap();
            }
            files.push((path, body));
        }
        let mut out = Vec::new();
        {
            let encoder = flate2::GzBuilder::new()
                .mtime(0)
                .write(&mut out, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            for (path, body) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive
                    .append_data(&mut header, path, body.as_slice())
                    .unwrap();
            }
            archive.into_inner().unwrap().finish().unwrap();
        }
        out
    }

    fn duplicate_entry(bytes: &[u8], duplicate: &str) -> Vec<u8> {
        let files = entries(bytes);
        let mut out = Vec::new();
        {
            let encoder = flate2::GzBuilder::new()
                .mtime(0)
                .write(&mut out, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            for (path, body) in &files {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive
                    .append_data(&mut header, path, body.as_slice())
                    .unwrap();
            }
            let body = files
                .iter()
                .find(|(path, _)| path == duplicate)
                .map(|(_, body)| body.clone())
                .unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(&mut header, duplicate, body.as_slice())
                .unwrap();
            archive.into_inner().unwrap().finish().unwrap();
        }
        out
    }

    fn entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let decoder = GzDecoder::new(Cursor::new(bytes));
        let mut archive = Archive::new(decoder);
        let mut files = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            let mut body = Vec::new();
            entry.read_to_end(&mut body).unwrap();
            files.push((path, body));
        }
        files
    }

    fn append_symlink(bytes: &[u8], path: &str, target: &str) -> Vec<u8> {
        let decoder = GzDecoder::new(Cursor::new(bytes));
        let mut archive = Archive::new(decoder);
        let mut files = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let p = entry.path().unwrap().to_string_lossy().to_string();
            let mut body = Vec::new();
            entry.read_to_end(&mut body).unwrap();
            files.push((p, body));
        }
        let mut out = Vec::new();
        {
            let encoder = flate2::GzBuilder::new()
                .mtime(0)
                .write(&mut out, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            for (p, body) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                archive
                    .append_data(&mut header, p, body.as_slice())
                    .unwrap();
            }
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            archive.append_link(&mut header, path, target).unwrap();
            archive.into_inner().unwrap().finish().unwrap();
        }
        out
    }
}
