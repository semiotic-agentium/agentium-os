// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-License-Identifier: Apache-2.0

//! End-to-end for the v2 TypeScript authoring loop: `karak tool build` +
//! `karak tool test` against a fixture tool that file-links `@karakara/tool`
//! from this repo (`ts/packages/tool`). Proves the SDK → descriptor → digest →
//! DevBackend-invoke path runs for real.
//!
//! Gated behind `KARAKARA_TOOL_E2E=1` (and skipped if node/npm are absent) so
//! the default offline `just test` is unaffected — it needs node + npm + a
//! network-reachable npm registry for the first install. Run it with
//! `just tool-e2e`.

use std::{path::PathBuf, process::Command};

fn have(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn tool_authoring_e2e() {
    if std::env::var("KARAKARA_TOOL_E2E").is_err() {
        eprintln!("skipping tool_authoring_e2e: set KARAKARA_TOOL_E2E=1 to run");
        return;
    }
    if !have("node") || !have("npm") {
        eprintln!("skipping tool_authoring_e2e: node/npm not on PATH");
        return;
    }

    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_dir.parent().unwrap().parent().unwrap().to_path_buf();
    let sdk_dir = repo_root.join("ts/packages/tool");
    let fixture = crate_dir.join("tests/fixtures/ts-echo");
    let karak = env!("CARGO_BIN_EXE_karak");

    // 1. Build the SDK to dist (so the file-link resolves at runtime).
    run(Command::new("npm").args(["run", "build"]).current_dir(&sdk_dir), "sdk build");
    // 2. Install the fixture (file-links @karakara/tool, pulls typebox).
    run(Command::new("npm").args(["install", "--no-audit", "--no-fund"]).current_dir(&fixture), "fixture install");

    // 3. `karak tool build` → descriptor + digest.
    run(Command::new(karak).args(["tool", "build", fixture.to_str().unwrap()]), "tool build");
    let descriptor = fixture.join("tool-descriptor.json");
    assert!(descriptor.is_file(), "tool build must write tool-descriptor.json");
    let body = std::fs::read_to_string(&descriptor).unwrap();
    assert!(body.contains("\"name\": \"support/ts-echo\""), "descriptor: {body}");

    // 4. `karak tool test` → run one invocation through the SDK over stdio.
    let ok = Command::new(karak)
        .args(["tool", "test", fixture.to_str().unwrap(), "--input", r#"{"message":"e2e"}"#])
        .output()
        .expect("tool test");
    let out = String::from_utf8_lossy(&ok.stdout);
    assert!(ok.status.success(), "tool test failed: {out}{}", String::from_utf8_lossy(&ok.stderr));
    assert!(out.contains("echoed") && out.contains("e2e"), "unexpected output: {out}");

    // 5. Bad input is rejected by the SDK as invalid_argument (no author code).
    let bad = Command::new(karak)
        .args(["tool", "test", fixture.to_str().unwrap(), "--input", r#"{"nope":1}"#])
        .output()
        .expect("tool test bad");
    let bad_out = String::from_utf8_lossy(&bad.stdout);
    assert!(bad_out.contains("invalid_argument"), "expected invalid_argument, got: {bad_out}");

    // Clean generated artifacts (also gitignored).
    let _ = std::fs::remove_file(fixture.join("tool-descriptor.json"));
    let _ = std::fs::remove_file(fixture.join("tool.lock.json"));
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd.status().unwrap_or_else(|e| panic!("{what}: spawn failed: {e}"));
    assert!(status.success(), "{what} exited with {status}");
}
