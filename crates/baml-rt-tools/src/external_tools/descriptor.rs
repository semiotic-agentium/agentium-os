// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Unified tool descriptor (external tools rewrite — see
//! `plans/external-tools-v2-p1-spec.md` §P1.1/§P1.2).
//!
//! The type and its digest now live in `baml-ktool`, the shared crate the guest
//! supervisor also compiles. This module re-exports them so every existing host
//! path (`external_tools::{UnifiedToolDescriptor, ...}`) keeps working.

/// Historical host-side name for the one descriptor type.
pub use baml_ktool::ToolDescriptor as UnifiedToolDescriptor;
pub use baml_ktool::descriptor::{
    CapabilityRequest, DESCRIPTOR_ABI, HostPort, McpMappingError, OutputTrust, SecretBinding,
    SecretRequest, SideEffect, ToolDescriptor, compute_descriptor_digest,
};
