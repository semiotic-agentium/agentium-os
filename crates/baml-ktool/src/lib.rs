// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Shared `.ktool` bundle contract for the host authority layer and the guest
//! supervisor (external tools v2 — P4).
//!
//! Both sides parse the same descriptor and manifest and compute the same
//! `tool_digest` from the same bytes, so the two cannot drift by construction.
//! This crate intentionally has no dependency on `baml-rt-core` or any other
//! host crate: the supervisor compiles it into a distroless guest image.

pub mod descriptor;
pub mod digest;
pub mod error;
pub mod ktool;

pub use descriptor::{
    CapabilityRequest, DESCRIPTOR_ABI, HostPort, McpMappingError, OutputTrust, SecretBinding,
    SecretRequest, SideEffect, ToolDescriptor, compute_descriptor_digest,
};
pub use digest::{Digest, DigestParseError, canonical_digest};
pub use error::{KtoolError, KtoolResult};
pub use ktool::{
    AuditedKtoolBundle, BundleFile, KTOOL_BUNDLE_SCHEMA_VERSION, KTOOL_DESCRIPTOR_PATH,
    KTOOL_MANIFEST_PATH, KTOOL_NODE_ENTRYPOINT, KTOOL_SBOM_PATH, KtoolManifest, KtoolRuntimeImage,
    KtoolRuntimeTier, MAX_KTOOL_BUNDLE_BYTES, MAX_KTOOL_UNPACKED_BYTES, UnpackedKtool,
    audit_ktool_bundle, build_ktool_bundle, compute_bundle_digest, compute_tool_digest_v2,
    default_node_runtime_image, error_message, unpack_ktool_bundle,
};
