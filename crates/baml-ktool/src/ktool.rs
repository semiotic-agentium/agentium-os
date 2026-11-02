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
    pub descriptor: ToolDescriptor,
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

    let descriptor: ToolDescriptor = serde_json::from_slice(
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

/// A bundle audited, identity-checked, and unpacked into a private directory.
#[derive(Debug)]
pub struct UnpackedKtool {
    pub descriptor: ToolDescriptor,
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
    let manifest = KtoolManifest {
        bundle_schema_version: KTOOL_BUNDLE_SCHEMA_VERSION,
        name: descriptor.name.clone(),
        runtime_tier: KtoolRuntimeTier::Sandbox,
        runtime_image: default_node_runtime_image(),
        entrypoint: KTOOL_NODE_ENTRYPOINT.to_string(),
        descriptor_digest: compute_descriptor_digest(descriptor),
        requested: descriptor.requested.clone(),
    };

    let mut files = vec![
        BundleFile::new(
            KTOOL_DESCRIPTOR_PATH,
            serde_json::to_vec(descriptor).map_err(|e| invalid(e.to_string()))?,
        ),
        BundleFile::new(
            KTOOL_MANIFEST_PATH,
            serde_json::to_vec(&manifest).map_err(|e| invalid(e.to_string()))?,
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
            requested: CapabilityRequest::default(),
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
            manifest.descriptor_digest = Digest::new(format!("sha256:{}", "f".repeat(64)));
        });
        let err = audit_ktool_bundle(&bytes).unwrap_err();
        assert!(err.to_string().contains("descriptor digest mismatch"));
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
