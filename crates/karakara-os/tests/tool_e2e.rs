// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-License-Identifier: Apache-2.0

//! End-to-end for the v2 TypeScript authoring loop: build, test, package, and
//! publish against a fixture that file-links `@karakara/tool` from this repo.
//! A one-request HTTP server audits the uploaded `.ktool`, so the test covers
//! top-level `karak publish` auto-detection without starting the full runner.
//!
//! Gated behind `KARAKARA_TOOL_E2E=1` (and skipped if node/npm are absent) so
//! the default offline `just test` is unaffected — it needs node + npm + a
//! network-reachable npm registry for the first install. Run it with
//! `just tool-e2e`.

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    thread,
};

use baml_rt_tools::external_tools::{audit_ktool_bundle, compute_descriptor_digest};

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
    run(
        Command::new("npm")
            .args(["run", "build"])
            .current_dir(&sdk_dir),
        "sdk build",
    );
    // 2. Install the fixture (file-links @karakara/tool, pulls typebox).
    run(
        Command::new("npm")
            .args(["install", "--no-audit", "--no-fund"])
            .current_dir(&fixture),
        "fixture install",
    );

    // 3. `karak tool build` → descriptor + digest.
    run(
        Command::new(karak).args(["tool", "build", fixture.to_str().unwrap()]),
        "tool build",
    );
    let descriptor = fixture.join("tool-descriptor.json");
    assert!(
        descriptor.is_file(),
        "tool build must write tool-descriptor.json"
    );
    let body = std::fs::read_to_string(&descriptor).unwrap();
    assert!(
        body.contains("\"name\": \"support/ts-echo\""),
        "descriptor: {body}"
    );

    // 4. `karak tool test` → run one invocation through the SDK over stdio.
    let ok = Command::new(karak)
        .args([
            "tool",
            "test",
            fixture.to_str().unwrap(),
            "--input",
            r#"{"message":"e2e"}"#,
        ])
        .output()
        .expect("tool test");
    let out = String::from_utf8_lossy(&ok.stdout);
    assert!(
        ok.status.success(),
        "tool test failed: {out}{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(
        out.contains("echoed") && out.contains("e2e"),
        "unexpected output: {out}"
    );

    // 5. Bad input is rejected by the SDK as invalid_argument (no author code).
    let bad = Command::new(karak)
        .args([
            "tool",
            "test",
            fixture.to_str().unwrap(),
            "--input",
            r#"{"nope":1}"#,
        ])
        .output()
        .expect("tool test bad");
    let bad_out = String::from_utf8_lossy(&bad.stdout);
    assert!(
        bad_out.contains("invalid_argument"),
        "expected invalid_argument, got: {bad_out}"
    );

    // 6. Top-level `karak publish` detects the tool project, emits a `.ktool`,
    // and uploads exact bytes. The mock repository audits the received bundle.
    let (repository_url, server) = serve_publish_once();
    run(
        Command::new(karak).args([
            "publish",
            "--path",
            fixture.to_str().unwrap(),
            "--repository-url",
            &repository_url,
            "--approved-by",
            "tool-e2e",
            "--yes",
        ]),
        "tool publish",
    );
    server.join().expect("publish server thread");
    assert!(
        fixture.join("dist/ts-echo.ktool").is_file(),
        "tool publish must write the .ktool bundle"
    );
    let lock = std::fs::read_to_string(fixture.join("tool.lock.json")).unwrap();
    assert!(
        lock.contains("tool_digest"),
        "publish lock must pin executable identity"
    );

    // Clean generated artifacts (also gitignored).
    let _ = std::fs::remove_file(fixture.join("tool-descriptor.json"));
    let _ = std::fs::remove_file(fixture.join("tool.lock.json"));
    let _ = std::fs::remove_file(fixture.join("dist/ts-echo.ktool"));
}

fn serve_publish_once() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind publish server");
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept publish request");
        let mut received = Vec::new();
        let mut chunk = [0_u8; 16 * 1024];
        let header_end = loop {
            let count = stream.read(&mut chunk).expect("read publish request");
            assert!(count > 0, "request ended before headers");
            received.extend_from_slice(&chunk[..count]);
            if let Some(offset) = received.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let headers = String::from_utf8_lossy(&received[..header_end]);
        assert!(
            headers.starts_with("POST /repository/external-tools/publish?approved_by=tool-e2e "),
            "unexpected request: {headers}"
        );
        let content_length: usize = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .expect("content-length header");
        while received.len() - header_end < content_length {
            let count = stream.read(&mut chunk).expect("read publish body");
            assert!(count > 0, "request body ended early");
            received.extend_from_slice(&chunk[..count]);
        }
        let body = &received[header_end..header_end + content_length];
        let audited = audit_ktool_bundle(body).expect("uploaded bundle must pass host audit");
        assert_eq!(audited.descriptor.name, "support/ts-echo");

        let response = serde_json::json!({
            "tool_name": audited.descriptor.name,
            "version": 1,
            "descriptor_digest": compute_descriptor_digest(&audited.descriptor),
            "bundle_digest": audited.bundle_digest,
            "tool_digest": audited.tool_digest,
        })
        .to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        )
        .expect("write publish response");
    });
    (format!("http://{addr}/repository"), handle)
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("{what}: spawn failed: {e}"));
    assert!(status.success(), "{what} exited with {status}");
}
