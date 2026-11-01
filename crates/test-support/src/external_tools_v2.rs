// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Deterministic bundles for host and repository boundary tests.

use baml_rt_tools::external_tools::{
    CapabilityRequest, DESCRIPTOR_ABI, ErrorClass, KTOOL_BUNDLE_SCHEMA_VERSION,
    KTOOL_DESCRIPTOR_PATH, KTOOL_MANIFEST_PATH, KTOOL_NODE_ENTRYPOINT, KTOOL_SBOM_PATH,
    KtoolManifest, KtoolRuntimeTier, OutputTrust, SideEffect, UnifiedToolDescriptor,
    compute_descriptor_digest, default_node_runtime_image,
};
use flate2::{Compression, GzBuilder};
use serde_json::json;
use tar::{Builder, Header};

pub fn descriptor(name: &str, requested: CapabilityRequest) -> UnifiedToolDescriptor {
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
        requested,
    }
}

pub fn bundle(descriptor: &UnifiedToolDescriptor) -> Vec<u8> {
    let manifest = KtoolManifest {
        bundle_schema_version: KTOOL_BUNDLE_SCHEMA_VERSION,
        name: descriptor.name.clone(),
        runtime_tier: KtoolRuntimeTier::Sandbox,
        runtime_image: default_node_runtime_image(),
        entrypoint: KTOOL_NODE_ENTRYPOINT.into(),
        descriptor_digest: compute_descriptor_digest(descriptor),
        requested: descriptor.requested.clone(),
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
