//! Integration test for issue #343: tracing-fmt output must flush per event
//! so a stalled runner does not silently swallow its last log lines into an
//! 8 KB pipe BufWriter inside the container.
//!
//! Strategy: re-invoke the test binary as a child process with a fixture
//! env var set. The child calls `init_tracing()` and emits N tracing events
//! at fixed gaps. The parent pipes the child's stderr, timestamps each
//! marker line on arrival, and asserts the spread of arrival times is
//! close to the emission spread — proving per-event line flush rather than
//! end-of-process batch flush.
//!
//! With the unfixed default-stdout writer, this test fails because tracing
//! events go to stdout (which the parent does not pipe) and never appear
//! on the captured stderr stream at all.

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const FIXTURE_ENV: &str = "BAML_RT_OBSERVABILITY_STDERR_FLUSH_FIXTURE";
const FIXTURE_TEST_NAME: &str = "stderr_flushes_per_event";
const MARKER: &str = "stderr_flush_fixture_marker";

fn run_fixture() {
    // Force the console filter to admit our `info!` events regardless of
    // the host shell's RUST_LOG.
    unsafe {
        std::env::set_var("RUST_LOG", "info");
    }
    baml_rt_observability::init_tracing();

    let n: u32 = std::env::var("STDERR_FLUSH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let gap_ms: u64 = std::env::var("STDERR_FLUSH_GAP_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);

    for i in 0..n {
        tracing::info!(target: "baml_rt_observability_test", event_index = i, "{MARKER}");
        std::thread::sleep(Duration::from_millis(gap_ms));
    }
}

#[test]
fn stderr_flushes_per_event() {
    if std::env::var(FIXTURE_ENV).is_ok() {
        run_fixture();
        return;
    }

    let n: u32 = 8;
    let gap_ms: u64 = 150;

    let exe = std::env::current_exe().expect("current_exe");
    let mut child = Command::new(&exe)
        .args(["--exact", FIXTURE_TEST_NAME, "--nocapture"])
        .env(FIXTURE_ENV, "1")
        .env("STDERR_FLUSH_N", n.to_string())
        .env("STDERR_FLUSH_GAP_MS", gap_ms.to_string())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn fixture child");

    let start = Instant::now();
    let stderr = child.stderr.take().expect("child stderr piped");
    let reader = BufReader::new(stderr);

    let mut arrivals: Vec<Duration> = Vec::with_capacity(n as usize);
    for line in reader.lines() {
        let line = line.expect("read child stderr line");
        if line.contains(MARKER) {
            arrivals.push(start.elapsed());
        }
    }

    let status = child.wait().expect("wait fixture child");
    assert!(
        status.success(),
        "fixture child exited non-zero: {status:?}"
    );

    assert_eq!(
        arrivals.len(),
        n as usize,
        "expected {n} marker lines on child stderr, got {} — \
         arrivals={arrivals:?}. If zero, the fmt layer is likely \
         writing to stdout instead of stderr (issue #343).",
        arrivals.len(),
    );

    let span = arrivals
        .last()
        .unwrap()
        .saturating_sub(*arrivals.first().unwrap());

    // Expected spread if events flush per-line: (n-1) * gap_ms.
    // Allow 50% slack for CI / scheduling jitter on the floor.
    let min_span = Duration::from_millis((u64::from(n) - 1) * gap_ms / 2);

    assert!(
        span >= min_span,
        "marker lines arrived in {span:?} for {n} events at {gap_ms}ms gaps \
         (need ≥{min_span:?}); this suggests buffered batching rather than \
         per-event line flush. arrivals={arrivals:?}",
    );
}
