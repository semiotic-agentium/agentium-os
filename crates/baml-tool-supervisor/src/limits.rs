// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Supervisor limits. Every value is a guest-side ceiling; the host grant is
//! still the authority (the supervisor only narrows, never widens).

use std::{path::PathBuf, time::Duration};

use baml_ktool::{MAX_KTOOL_BUNDLE_BYTES, MAX_KTOOL_UNPACKED_BYTES};

/// Default vsock port for the supervisor. Must match the provider's `--vsock`
/// port contract baked into the runtime image (`supervisor_abi`).
pub const DEFAULT_VSOCK_PORT: u32 = 5000;

/// Absolute ceiling on one call, regardless of the host-supplied deadline.
pub const DEFAULT_MAX_CALL_MS: u64 = 120_000;

#[derive(Debug, Clone)]
pub struct SupervisorLimits {
    pub vsock_port: u32,
    pub max_bundle_bytes: u64,
    pub max_unpacked_bytes: u64,
    /// Bytes of worker stderr captured per call before it is discarded.
    pub default_output_cap: u64,
    /// Largest serialized `Result.output` accepted from a worker.
    pub max_result_bytes: usize,
    pub worker_idle_timeout: Duration,
    /// Bound warm workers per VM (each holds a uid + namespaces open).
    pub max_warm_workers: usize,
    pub max_call_ms: u64,
    pub uid_base: u32,
    pub gid_base: u32,
    /// Per-worker address-space cap.
    pub memory_bytes: u64,
    /// `node_modules` mounted at `/node_modules` so bare imports resolve (the
    /// runtime image ships `@karakara/tool` there; `karak tool test` points at
    /// the project's install).
    pub node_modules: Option<PathBuf>,
    /// DevBackend/test override for the worker entry harness. Unset in prod:
    /// the image contract is the `@karakara/tool` `serve()` loop.
    pub worker_harness: Option<String>,
    /// Apply the pinned seccomp allowlist in the root backend.
    pub seccomp: bool,
}

impl Default for SupervisorLimits {
    fn default() -> Self {
        Self {
            vsock_port: DEFAULT_VSOCK_PORT,
            max_bundle_bytes: MAX_KTOOL_BUNDLE_BYTES as u64,
            max_unpacked_bytes: MAX_KTOOL_UNPACKED_BYTES,
            default_output_cap: 1024 * 1024,
            max_result_bytes: 4 * 1024 * 1024,
            worker_idle_timeout: Duration::from_secs(300),
            max_warm_workers: 16,
            max_call_ms: DEFAULT_MAX_CALL_MS,
            uid_base: 100_000,
            gid_base: 100_000,
            // V8 reserves several GiB of virtual address space for its heap
            // cage; RLIMIT_AS is virtual, not resident. The VM memory grant
            // (host policy) remains the real RSS bound.
            memory_bytes: 4 * 1024 * 1024 * 1024,
            node_modules: None,
            worker_harness: None,
            seccomp: true,
        }
    }
}

impl SupervisorLimits {
    /// Read environment overrides. Unknown/invalid values keep the default
    /// rather than silently disabling a ceiling.
    pub fn from_env() -> Self {
        let mut limits = Self::default();
        if let Ok(port) = std::env::var("KARAKARA_VSOCK_PORT")
            && let Ok(port) = port.parse::<u32>()
            && port > 0
        {
            limits.vsock_port = port;
        }
        if let Ok(bytes) = std::env::var("KARAKARA_MEMORY_BYTES")
            && let Ok(bytes) = bytes.parse::<u64>()
            && bytes > 0
        {
            limits.memory_bytes = bytes;
        }
        if let Ok(path) = std::env::var("KARAKARA_NODE_MODULES") {
            limits.node_modules = Some(PathBuf::from(path));
        }
        if let Ok(harness) = std::env::var("KARAKARA_WORKER_HARNESS") {
            limits.worker_harness = Some(harness);
        }
        if let Ok(ms) = std::env::var("KARAKARA_WORKER_IDLE_MS")
            && let Ok(ms) = ms.parse::<u64>()
            && ms > 0
        {
            limits.worker_idle_timeout = Duration::from_millis(ms);
        }
        limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_bounded_and_consistent() {
        let limits = SupervisorLimits::default();
        assert!(limits.max_unpacked_bytes >= limits.max_bundle_bytes);
        assert!(!limits.worker_idle_timeout.is_zero());
        assert!(limits.max_result_bytes > 0);
        assert!(limits.max_call_ms > 0);
    }
}
