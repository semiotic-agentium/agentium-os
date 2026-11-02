// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Deterministic seed-corpus smoke for the two attacker-reachable parsers.
//!
//! `cargo fuzz` lives in `fuzz/`; this test runs the same inputs in the normal
//! lane so a regression cannot land just because nobody ran the fuzzer.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use baml_ktool::{
    CapabilityRequest, DESCRIPTOR_ABI, SideEffect, ToolDescriptor, audit_ktool_bundle,
    build_ktool_bundle, unpack_ktool_bundle,
};
use baml_sandbox_protocol::{
    codec::{FrameReader, FrameWriter},
    protocol_v2::{HostMessage, PROTOCOL_V2_VERSION},
};
use baml_tool_supervisor::{DirectBackend, Supervisor, SupervisorLimits, WorkerBackend};
use tokio::sync::mpsc;

/// Tiny deterministic PRNG so the corpus is reproducible.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = (self.next() as usize) % max.saturating_add(1);
        (0..len).map(|_| (self.next() >> 24) as u8).collect()
    }
}

fn descriptor() -> ToolDescriptor {
    ToolDescriptor {
        descriptor_abi: DESCRIPTOR_ABI,
        name: "test/fuzz".into(),
        version: "0.1.0".into(),
        description: "seed".into(),
        input_schema: serde_json::json!({}),
        output_schema: serde_json::json!({}),
        error_classes: vec![],
        side_effect: SideEffect::Read,
        idempotent: true,
        output_trust: baml_ktool::OutputTrust::Untrusted,
        requested: CapabilityRequest::default(),
    }
}

fn seed_bundle() -> Vec<u8> {
    build_ktool_bundle(
        &descriptor(),
        b"export default {};".to_vec(),
        vec![],
        br#"{}"#.to_vec(),
    )
    .unwrap()
}

#[test]
fn audit_and_unpack_never_panic_or_escape_on_mutated_bundles() {
    let seed = seed_bundle();
    let audited = audit_ktool_bundle(&seed).unwrap();
    let root = std::env::temp_dir().join(format!(
        "karakara-fuzz-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut rng = Lcg(0x5eed);

    for iteration in 0..256 {
        let bytes = if iteration % 4 == 0 {
            rng.bytes(4096)
        } else {
            let mut mutated = seed.clone();
            for _ in 0..1 + (rng.next() % 8) {
                let index = (rng.next() as usize) % mutated.len();
                mutated[index] ^= (rng.next() & 0xff) as u8;
            }
            mutated
        };
        // Parsers must return, never panic.
        let _ = audit_ktool_bundle(&bytes);

        let dest = root.join(format!("case-{iteration}"));
        let unpacked = unpack_ktool_bundle(
            &bytes,
            &audited.tool_digest.to_string(),
            &audited.manifest.runtime_image.digest.to_string(),
            &dest,
            1024 * 1024,
        );
        if unpacked.is_ok() {
            assert_tree_contained(&dest, &dest);
        }
        let _ = std::fs::remove_dir_all(&dest);
    }
    let _ = std::fs::remove_dir_all(&root);
}

fn assert_tree_contained(root: &Path, current: &Path) {
    let entries = match std::fs::read_dir(current) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(!metadata.file_type().is_symlink(), "fuzzer wrote a symlink");
        let canonical = path.canonicalize().unwrap();
        assert!(
            canonical.starts_with(root.canonicalize().unwrap()),
            "unpack escaped its root: {}",
            canonical.display()
        );
        if metadata.is_dir() {
            assert_tree_contained(root, &path);
        }
    }
}

#[test]
fn arbitrary_json_frames_retire_the_connection() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = std::env::temp_dir().join(format!(
            "karakara-wire-fuzz-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (host, guest) = tokio::io::duplex(64 * 1024);
        let (guest_reader, guest_writer) = tokio::io::split(guest);
        let (outbound, receiver) = mpsc::unbounded_channel();
        let writer = tokio::spawn(async move {
            let mut writer = FrameWriter::new(guest_writer);
            let mut receiver = receiver;
            while let Some(message) = receiver.recv().await {
                if writer
                    .send(&serde_json::to_value(&message).unwrap())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let backend: Arc<dyn WorkerBackend> = Arc::new(DirectBackend);
        let supervisor =
            Supervisor::new(SupervisorLimits::default(), root, backend, outbound).unwrap();
        let task = tokio::spawn(supervisor.clone().run(FrameReader::new(guest_reader)));

        let (host_reader, host_writer) = tokio::io::split(host);
        let mut host = baml_sandbox_protocol::TsrpcChannel::new(host_reader, host_writer);
        host.send(
            &serde_json::to_value(HostMessage::Hello {
                proto: PROTOCOL_V2_VERSION,
                grant_class: "fuzz".into(),
            })
            .unwrap(),
        )
        .await
        .unwrap();
        let _ = host.recv().await;

        let mut rng = Lcg(0xfeed);
        for _ in 0..64 {
            let payload = rng.bytes(512);
            if host.send_bytes(&payload).await.is_err() {
                break;
            }
        }
        // Close the host end so a supervisor waiting for bundle chunks sees EOF.
        drop(host);
        let finished = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(
            finished.is_ok(),
            "the supervisor must retire the connection"
        );
        writer.abort();
        supervisor.shutdown().await;
    });
}
