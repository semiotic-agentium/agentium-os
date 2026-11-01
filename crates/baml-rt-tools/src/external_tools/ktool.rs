// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Portable `.ktool` bundle metadata and load-time audit.
//!
//! A bundle is a deterministic tar.gz containing the generated descriptor,
//! requests-only manifest, SBOM, and vendored executable bytes. The host reads
//! and validates the descriptor without executing tool code.

use std::{
    collections::HashSet,
    io::{Cursor, Read},
    path::{Component, Path},
};

use baml_rt_core::{BamlRtError, Result};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tar::Archive;

use super::{CapabilityRequest, UnifiedToolDescriptor, compute_descriptor_digest};
use crate::mcp_snapshot::{Digest, canonical_digest};

pub const KTOOL_BUNDLE_SCHEMA_VERSION: u32 = 1;
pub const KTOOL_MANIFEST_PATH: &str = "manifest.json";
pub const KTOOL_DESCRIPTOR_PATH: &str = "tool-descriptor.json";
pub const KTOOL_SBOM_PATH: &str = "sbom.spdx.json";
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
    pub name: String,
    pub runtime_tier: KtoolRuntimeTier,
    pub runtime_image: KtoolRuntimeImage,
    pub entrypoint: String,
    pub descriptor_digest: Digest,
    pub requested: CapabilityRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KtoolRuntimeTier {
    Sandbox,
}

/// Result of auditing a bundle's inert metadata and exact bytes.
#[derive(Debug, Clone)]
pub struct AuditedKtoolBundle {
    pub descriptor: UnifiedToolDescriptor,
    pub manifest: KtoolManifest,
    pub bundle_digest: Digest,
    pub tool_digest: Digest,
}

impl AuditedKtoolBundle {
    /// Match audited bytes to the approved contract without executing the bundle.
    pub fn verify_snapshot(&self, snapshot: &super::ExternalToolSnapshot) -> Result<()> {
        super::validate_external_tool_snapshot(snapshot)?;
        let v2 = snapshot
            .v2
            .as_ref()
            .ok_or_else(|| invalid("a .ktool bundle requires a v2 snapshot"))?;
        if v2.bundle_digest != self.bundle_digest
            || v2.tool_digest != self.tool_digest
            || v2.descriptor != self.descriptor
            || v2.runtime_image != self.manifest.runtime_image
        {
            return Err(invalid(
                "the external-tool snapshot does not match the audited .ktool bundle",
            ));
        }
        Ok(())
    }
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
pub fn audit_ktool_bundle(bytes: &[u8]) -> Result<AuditedKtoolBundle> {
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
    let mut descriptor_json = None;
    let mut manifest_json = None;
    let mut has_sbom = false;
    let mut has_entrypoint = false;

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
            KTOOL_DESCRIPTOR_PATH => descriptor_json = Some(read_entry(&mut entry, &path)?),
            KTOOL_MANIFEST_PATH => manifest_json = Some(read_entry(&mut entry, &path)?),
            KTOOL_SBOM_PATH => {
                let _: serde_json::Value = serde_json::from_slice(&read_entry(&mut entry, &path)?)
                    .map_err(|e| invalid(format!("invalid {KTOOL_SBOM_PATH}: {e}")))?;
                has_sbom = true;
            }
            KTOOL_NODE_ENTRYPOINT => has_entrypoint = true,
            _ => {}
        }
    }

    let descriptor: UnifiedToolDescriptor = serde_json::from_slice(
        &descriptor_json.ok_or_else(|| invalid(format!("missing {KTOOL_DESCRIPTOR_PATH}")))?,
    )
    .map_err(|e| invalid(format!("invalid {KTOOL_DESCRIPTOR_PATH}: {e}")))?;
    let manifest: KtoolManifest = serde_json::from_slice(
        &manifest_json.ok_or_else(|| invalid(format!("missing {KTOOL_MANIFEST_PATH}")))?,
    )
    .map_err(|e| invalid(format!("invalid {KTOOL_MANIFEST_PATH}: {e}")))?;

    if !has_sbom {
        return Err(invalid(format!("missing {KTOOL_SBOM_PATH}")));
    }
    if !has_entrypoint || manifest.entrypoint != KTOOL_NODE_ENTRYPOINT {
        return Err(invalid(format!(
            "node bundle entrypoint must be {KTOOL_NODE_ENTRYPOINT}"
        )));
    }
    if manifest.bundle_schema_version != KTOOL_BUNDLE_SCHEMA_VERSION {
        return Err(invalid(format!(
            "unsupported .ktool schema version {} (expected {})",
            manifest.bundle_schema_version, KTOOL_BUNDLE_SCHEMA_VERSION
        )));
    }
    if manifest.runtime_tier != KtoolRuntimeTier::Sandbox {
        return Err(invalid(
            "only the sandbox runtime tier is publishable in P2",
        ));
    }
    let registered_runtime = default_node_runtime_image();
    if manifest.runtime_image != registered_runtime {
        return Err(invalid(format!(
            "runtime image is not registered: expected {}@{}, got {}@{}",
            registered_runtime.name,
            registered_runtime.digest,
            manifest.runtime_image.name,
            manifest.runtime_image.digest
        )));
    }
    if manifest.name != descriptor.name {
        return Err(invalid(format!(
            "manifest name '{}' does not match descriptor name '{}'",
            manifest.name, descriptor.name
        )));
    }
    if manifest.requested != descriptor.requested {
        return Err(invalid(
            "manifest capability requests do not match the descriptor",
        ));
    }

    let descriptor_digest = compute_descriptor_digest(&descriptor);
    if manifest.descriptor_digest != descriptor_digest {
        return Err(invalid(format!(
            "descriptor digest mismatch: manifest has {}, computed {}",
            manifest.descriptor_digest, descriptor_digest
        )));
    }

    let bundle_digest = compute_bundle_digest(bytes);
    let tool_digest = compute_tool_digest_v2(bytes, manifest.runtime_image.digest);
    Ok(AuditedKtoolBundle {
        descriptor,
        manifest,
        bundle_digest,
        tool_digest,
    })
}

fn read_entry<R: Read>(entry: &mut R, path: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    entry
        .read_to_end(&mut bytes)
        .map_err(|e| invalid(format!("failed to read {path}: {e}")))?;
    Ok(bytes)
}

fn validate_archive_path(path: &Path) -> Result<()> {
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

fn invalid(message: impl Into<String>) -> BamlRtError {
    BamlRtError::InvalidArgument(message.into())
}

#[cfg(test)]
mod tests {
    use baml_sandbox_protocol::ErrorClass;
    use flate2::{Compression, GzBuilder};
    use serde_json::json;
    use tar::{Builder, Header};

    use super::*;
    use crate::external_tools::{DESCRIPTOR_ABI, OutputTrust, SideEffect};

    fn descriptor() -> UnifiedToolDescriptor {
        UnifiedToolDescriptor {
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
            requested: CapabilityRequest::default(),
        }
    }

    fn bundle(tamper_digest: bool) -> Vec<u8> {
        let descriptor = descriptor();
        let mut digest = compute_descriptor_digest(&descriptor);
        if tamper_digest {
            digest = Digest::new(format!("sha256:{}", "f".repeat(64)));
        }
        let manifest = KtoolManifest {
            bundle_schema_version: KTOOL_BUNDLE_SCHEMA_VERSION,
            name: descriptor.name.clone(),
            runtime_tier: KtoolRuntimeTier::Sandbox,
            runtime_image: default_node_runtime_image(),
            entrypoint: KTOOL_NODE_ENTRYPOINT.into(),
            descriptor_digest: digest,
            requested: descriptor.requested.clone(),
        };

        let mut out = Vec::new();
        let encoder = GzBuilder::new()
            .mtime(0)
            .write(&mut out, Compression::default());
        let mut tar = Builder::new(encoder);
        for (path, bytes) in [
            (
                KTOOL_DESCRIPTOR_PATH,
                serde_json::to_vec(&descriptor).unwrap(),
            ),
            (KTOOL_MANIFEST_PATH, serde_json::to_vec(&manifest).unwrap()),
            (KTOOL_SBOM_PATH, br#"{}"#.to_vec()),
            (KTOOL_NODE_ENTRYPOINT, b"export default {};".to_vec()),
        ] {
            let mut header = Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, bytes.as_slice())
                .unwrap();
        }
        let encoder = tar.into_inner().unwrap();
        encoder.finish().unwrap();
        out
    }

    #[test]
    fn audits_valid_bundle_and_computes_identities() {
        let bytes = bundle(false);
        let audited = audit_ktool_bundle(&bytes).unwrap();
        assert_eq!(audited.descriptor.name, "support/echo");
        assert_eq!(audited.bundle_digest, compute_bundle_digest(&bytes));
        assert_eq!(
            audited.tool_digest,
            compute_tool_digest_v2(&bytes, audited.manifest.runtime_image.digest)
        );
    }

    #[test]
    fn rejects_descriptor_drift() {
        let err = audit_ktool_bundle(&bundle(true)).unwrap_err();
        assert!(err.to_string().contains("descriptor digest mismatch"));
    }
}
