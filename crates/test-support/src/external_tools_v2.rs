// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Deterministic bundles for host and repository boundary tests.

use baml_rt_tools::external_tools::{
    CapabilityRequest, DESCRIPTOR_ABI, ErrorClass, KTOOL_BUNDLE_SCHEMA_VERSION,
    KTOOL_DESCRIPTOR_PATH, KTOOL_MANIFEST_PATH, KTOOL_NODE_ENTRYPOINT, KTOOL_SBOM_PATH,
    KTOOL_TOOLS_PATH, KtoolManifest, KtoolRuntimeTier, KtoolToolEntry, KtoolTrustedLaunch,
    OutputTrust, SideEffect, UnifiedToolDescriptor, compute_descriptor_digest,
    host_native_runtime_image,
};
use flate2::{Compression, GzBuilder};
use serde_json::json;
use tar::{Builder, Header};

pub fn descriptor(name: &str) -> UnifiedToolDescriptor {
    UnifiedToolDescriptor {
        descriptor_abi: DESCRIPTOR_ABI,
        name: name.into(),
        version: "0.1.0".into(),
        description: "External tool host fixture".into(),
        input_schema: json!({"type": "object"}),
        output_schema: json!({"type": "object"}),
        error_classes: vec![ErrorClass::Execution],
        side_effect: SideEffect::Read,
        idempotent: true,
        output_trust: OutputTrust::Untrusted,
    }
}

/// Build a single-tool `.ktool` whose toolset-level capability requests are
/// `requested` (R1 §R1.1: capabilities live on the manifest, not the
/// descriptor).
pub fn bundle(descriptor: &UnifiedToolDescriptor, requested: CapabilityRequest) -> Vec<u8> {
    bundle_with_launch(descriptor, requested, KtoolRuntimeTier::Sandbox, None)
}

/// Build a `Trusted` `.ktool` that declares where its supervisor lives
/// (`Spawn` on a node, or `Attach` to an operator daemon). Used by the deploy
/// reachability tests (P5.5).
pub fn trusted_bundle(descriptor: &UnifiedToolDescriptor, launch: KtoolTrustedLaunch) -> Vec<u8> {
    bundle_with_launch(
        descriptor,
        CapabilityRequest::default(),
        KtoolRuntimeTier::Trusted,
        Some(launch),
    )
}

fn bundle_with_launch(
    descriptor: &UnifiedToolDescriptor,
    requested: CapabilityRequest,
    tier: KtoolRuntimeTier,
    trusted_launch: Option<KtoolTrustedLaunch>,
) -> Vec<u8> {
    let runtime_image = match tier {
        KtoolRuntimeTier::Sandbox => baml_rt_tools::external_tools::default_node_runtime_image(),
        KtoolRuntimeTier::Trusted => host_native_runtime_image(),
    };
    let manifest = KtoolManifest {
        bundle_schema_version: KTOOL_BUNDLE_SCHEMA_VERSION,
        name: descriptor
            .name
            .split_once('/')
            .map(|(bundle, _)| bundle)
            .unwrap_or(&descriptor.name)
            .into(),
        version: semver::Version::new(0, 1, 0),
        runtime_tier: tier,
        runtime_image,
        command: vec!["node".into(), KTOOL_NODE_ENTRYPOINT.into()],
        requested,
        tools: vec![KtoolToolEntry {
            name: descriptor.name.clone(),
            descriptor_digest: compute_descriptor_digest(descriptor),
        }],
        trusted_launch,
    };
    let encoder = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::default());
    let mut archive = Builder::new(encoder);
    for (path, body) in [
        (
            KTOOL_DESCRIPTOR_PATH,
            serde_json::to_vec(descriptor).unwrap(),
        ),
        (KTOOL_MANIFEST_PATH, serde_json::to_vec(&manifest).unwrap()),
        (
            KTOOL_TOOLS_PATH,
            serde_json::to_vec(&vec![descriptor]).unwrap(),
        ),
        (KTOOL_SBOM_PATH, b"{}".to_vec()),
        (KTOOL_NODE_ENTRYPOINT, b"export default {};".to_vec()),
    ] {
        let mut header = Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, path, body.as_slice())
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}
