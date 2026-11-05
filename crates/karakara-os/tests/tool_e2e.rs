// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-License-Identifier: Apache-2.0

//! End-to-end for the TypeScript authoring loop: TypeBox descriptor generation,
//! DevBackend invocation, packaging, and publish. A one-request HTTP server
//! audits the uploaded `.ktool`, so the test covers
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

use baml_rt_tools::external_tools::{
    KtoolRuntimeTier, audit_ktool_bundle, compute_descriptor_digest, default_node_runtime_image,
    host_native_runtime_image,
};

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
    let fixture = crate_dir.join("tests/fixtures/ts-echo");
    let karak = env!("CARGO_BIN_EXE_karak");

    // 1. Install the local @karakara/tool SDK fixture.
    run(
        Command::new("npm")
            .args(["install", "--no-audit", "--no-fund"])
            .current_dir(&fixture),
        "fixture install",
    );

    // 2. `karak tool build` asks the MCP worker for tools/list.
    run(
        Command::new(karak).args(["tool", "build", fixture.to_str().unwrap()]),
        "tool build",
    );
    let tools = fixture.join("tools.json");
    assert!(tools.is_file(), "tool build must write tools.json");
    let body = std::fs::read_to_string(&tools).unwrap();
    assert!(
        body.contains("\"name\": \"support/ts-echo\""),
        "tools: {body}"
    );
    assert!(body.contains("\"echoed\""), "TypeBox output schema: {body}");
    let manifest = std::fs::read_to_string(fixture.join("tool-manifest.json")).unwrap();
    assert!(manifest.contains("\"name\": \"support\""));
    assert!(manifest.contains("\"requested\": {}"));

    // 3. `karak tool test` → one invocation through the real guest supervisor
    //    in DevBackend mode (same wire + worker isolation as prod).
    run(
        Command::new(env!("CARGO")).args(["build", "-p", "baml-tool-supervisor"]),
        "supervisor build",
    );
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

    // 4. Bad input is rejected by the MCP server as invalid_argument.
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

    // 5. ctx.error metadata keeps its class across MCP and the supervisor.
    let denied = Command::new(karak)
        .args([
            "tool",
            "test",
            fixture.to_str().unwrap(),
            "--input",
            r#"{"message":"forbidden"}"#,
        ])
        .output()
        .expect("tool test classified error");
    let denied_out = String::from_utf8_lossy(&denied.stdout);
    assert!(
        denied_out.contains("permission") && denied_out.contains("fixture denied"),
        "expected permission, got: {denied_out}"
    );

    // 6. Top-level `karak publish` detects the tool project, emits a `.ktool`,
    // and uploads exact bytes. The mock repository audits the received bundle.
    let (repository_url, server) = serve_publish_once(KtoolRuntimeTier::Sandbox);
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
        fixture.join("dist/support.ktool").is_file(),
        "tool publish must write the .ktool bundle"
    );
    let lock = std::fs::read_to_string(fixture.join("tool.lock.json")).unwrap();
    assert!(
        lock.contains("tool_digest"),
        "publish lock must pin executable identity"
    );

    // 7. `karak tool publish --trusted` emits a Trusted-tier bundle: the same
    // node entrypoint, but the host-native image sentinel and no grants. The
    // mock repository asserts the received tier server-side.
    let (trusted_url, trusted_server) = serve_publish_once(KtoolRuntimeTier::Trusted);
    run(
        Command::new(karak).args([
            "tool",
            "publish",
            fixture.to_str().unwrap(),
            "--repository-url",
            &trusted_url,
            "--approved-by",
            "tool-e2e",
            "--yes",
            "--trusted",
        ]),
        "trusted tool publish",
    );
    trusted_server
        .join()
        .expect("trusted publish server thread");

    // Clean generated artifacts (also gitignored).
    let _ = std::fs::remove_file(fixture.join("tools.json"));
    let _ = std::fs::remove_file(fixture.join("tool-manifest.json"));
    let _ = std::fs::remove_file(fixture.join("tool.lock.json"));
    let _ = std::fs::remove_file(fixture.join("dist/support.ktool"));
}

fn serve_publish_once(expected_tier: KtoolRuntimeTier) -> (String, thread::JoinHandle<()>) {
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
        assert_eq!(
            audited.manifest.runtime_tier, expected_tier,
            "published bundle tier mismatch"
        );
        let expected_image = match expected_tier {
            KtoolRuntimeTier::Sandbox => default_node_runtime_image(),
            KtoolRuntimeTier::Trusted => host_native_runtime_image(),
        };
        assert_eq!(
            audited.manifest.runtime_image, expected_image,
            "published bundle runtime image mismatch"
        );

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
