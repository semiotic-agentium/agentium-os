// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Host-side `.ktool` surface.
//!
//! Types, digest helpers, audit, and guarded unpacking live in `baml-ktool` so
//! the guest supervisor uses the identical contract. This module re-exports
//! them and keeps the host-only snapshot cross-check (`verify_snapshot`), which
//! depends on the host's `ExternalToolSnapshot`.

pub use baml_ktool::ktool::{
    AuditedKtoolBundle, BundleFile, KTOOL_BUNDLE_SCHEMA_VERSION, KTOOL_DESCRIPTOR_PATH,
    KTOOL_MANIFEST_PATH, KTOOL_NODE_ENTRYPOINT, KTOOL_SBOM_PATH, KtoolManifest, KtoolRuntimeImage,
    KtoolRuntimeTier, MAX_KTOOL_BUNDLE_BYTES, MAX_KTOOL_UNPACKED_BYTES, build_ktool_bundle,
    build_ktool_bundle_for_tier, compute_bundle_digest, compute_tool_digest_v2,
    default_node_runtime_image, host_native_runtime_image, runtime_image_for_tier,
};
use baml_rt_core::{BamlRtError, Result};

use super::{ExternalToolSnapshot, ToolDelivery, validate_external_tool_snapshot};

/// Audit a `.ktool` without running it, mapping bundle errors into the host
/// error taxonomy.
pub fn audit_ktool_bundle(bytes: &[u8]) -> Result<AuditedKtoolBundle> {
    baml_ktool::audit_ktool_bundle(bytes).map_err(|error| invalid(error.to_string()))
}

/// Match audited bytes to the approved contract without executing the bundle.
pub fn verify_ktool_snapshot(
    audited: &AuditedKtoolBundle,
    snapshot: &ExternalToolSnapshot,
) -> Result<()> {
    validate_external_tool_snapshot(snapshot)?;
    let v2 = snapshot
        .v2
        .as_ref()
        .ok_or_else(|| invalid("a .ktool bundle requires a v2 snapshot"))?;
    let ToolDelivery::Bundled {
        bundle_digest,
        tool_digest,
        runtime_image,
    } = &v2.delivery
    else {
        return Err(invalid("a Service snapshot has no .ktool bundle identity"));
    };
    if *bundle_digest != audited.bundle_digest
        || *tool_digest != audited.tool_digest
        || v2.descriptor != audited.descriptor
        || *runtime_image != audited.manifest.runtime_image
    {
        return Err(invalid(
            "the external-tool snapshot does not match the audited .ktool bundle",
        ));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> BamlRtError {
    BamlRtError::InvalidArgument(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::external_tools::{
        CapabilityRequest, DESCRIPTOR_ABI, ErrorClass, OutputTrust, SideEffect,
        UnifiedToolDescriptor, compute_descriptor_digest,
    };

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

    #[test]
    fn audits_and_computes_identities() {
        let bytes = baml_ktool::build_ktool_bundle(
            &descriptor(),
            b"export default {};".to_vec(),
            vec![],
            br#"{}"#.to_vec(),
        )
        .unwrap();
        let audited = audit_ktool_bundle(&bytes).unwrap();
        assert_eq!(audited.descriptor.name, "support/echo");
        assert_eq!(audited.bundle_digest, compute_bundle_digest(&bytes));
        assert_eq!(
            audited.tool_digest,
            compute_tool_digest_v2(&bytes, audited.manifest.runtime_image.digest)
        );
        assert_eq!(
            audited.manifest.descriptor_digest,
            compute_descriptor_digest(&audited.descriptor)
        );
    }
}
