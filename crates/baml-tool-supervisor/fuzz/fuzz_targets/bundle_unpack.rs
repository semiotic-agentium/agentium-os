// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Fuzz the bundle auditor and safe unpacker.
//!
//! Invariant: arbitrary bytes never panic, never escape the destination
//! directory, and never exceed the unpacked cap.

#![no_main]

use std::path::PathBuf;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = baml_ktool::audit_ktool_bundle(data);

    let dest = PathBuf::from(format!("/tmp/karakara-fuzz-unpack-{}", std::process::id()));
    let _ = baml_ktool::unpack_ktool_bundle(
        data,
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        &baml_ktool::default_node_runtime_image().digest.to_string(),
        &dest,
        baml_ktool::MAX_KTOOL_UNPACKED_BYTES,
    );
    let _ = std::fs::remove_dir_all(&dest);
});
