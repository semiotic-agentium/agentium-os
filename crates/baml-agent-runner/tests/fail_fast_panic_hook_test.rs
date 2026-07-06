// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "fail-fast-test")]

use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn background_task_panic_exits_legacy_runner_process_with_101() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_baml-agent-runner"))
        .env("AGENTIUM_TEST_PANIC_BACKGROUND", "1")
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn baml-agent-runner");

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("baml-agent-runner did not exit after injected background panic");
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr pipe")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    assert_eq!(status.code(), Some(101));
    assert!(
        stderr.contains("test-injected background panic"),
        "stderr did not contain panic message:\n{stderr}"
    );
    assert!(
        stderr.contains("fatal: unhandled panic (storage_panic=false"),
        "stderr did not contain fail-fast hook line:\n{stderr}"
    );
}
