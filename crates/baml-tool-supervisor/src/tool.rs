// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Immutable state for one verified, unpacked tool.

use std::path::{Path, PathBuf};

use baml_ktool::{KtoolRuntimeTier, SecretBinding, UnpackedKtool};

use crate::{
    limits::SupervisorLimits,
    sandbox::{NetNamespace, Rlimits, WorkerSandbox},
};

pub struct LoadedTool {
    pub digest: String,
    pub dir: PathBuf,
    pub command: Vec<String>,
    pub descriptors: Vec<baml_ktool::ToolDescriptor>,
    pub runtime_tier: KtoolRuntimeTier,
    /// False when the toolset manifest declared no `net` destinations.
    pub has_network: bool,
    /// Env names of the toolset's egress-bound secrets. The VM exposes each
    /// as a placeholder (never the value); workers inherit only these.
    pub egress_secrets: Vec<String>,
    pub uid: u32,
    pub gid: u32,
}

impl LoadedTool {
    pub fn from_unpacked(
        unpacked: UnpackedKtool,
        uid: u32,
        gid: u32,
        _limits: &SupervisorLimits,
    ) -> Self {
        Self {
            digest: unpacked.tool_digest.to_string(),
            dir: unpacked.dir,
            command: unpacked.manifest.command,
            descriptors: unpacked.descriptors,
            runtime_tier: unpacked.manifest.runtime_tier,
            has_network: unpacked.manifest.requested.has_network(),
            egress_secrets: unpacked
                .manifest
                .requested
                .secrets
                .iter()
                .filter(|secret| secret.binding == SecretBinding::EgressBound)
                .map(|secret| secret.name.clone())
                .collect(),
            uid,
            gid,
        }
    }

    /// A tool that declared no egress gets a loopback-only namespace. A tool
    /// with declared hosts inherits the VM network, whose egress is compiled
    /// from the host grant *outside* the VM.
    pub fn net_namespace(&self) -> NetNamespace {
        if self.has_network {
            NetNamespace::Inherit
        } else {
            NetNamespace::Isolated
        }
    }

    pub fn sandbox(&self, limits: &SupervisorLimits) -> WorkerSandbox {
        WorkerSandbox {
            uid: self.uid,
            gid: self.gid,
            bundle_dir: self.dir.clone(),
            node_modules: limits.node_modules.clone(),
            tmpdir_bytes: 64 * 1024 * 1024,
            rlimits: Rlimits::for_call(limits.memory_bytes, limits.max_call_ms),
            net: self.net_namespace(),
            seccomp: limits.seccomp,
        }
    }

    pub fn entry_path(&self) -> PathBuf {
        self.command
            .get(1)
            .filter(|arg| arg.starts_with("code/"))
            .map(|arg| self.dir.join(arg))
            .unwrap_or_else(|| self.dir.join(Path::new("code/tool.js")))
    }
}
