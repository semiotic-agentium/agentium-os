// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Fail-fast panic policy for serve binaries.

/// Install a global panic hook that terminates the process after the previous
/// hook has printed the panic message and backtrace.
///
/// The runner treats any unhandled panic — especially panics from embedded
/// SurrealDB/SurrealKV background tasks — as process-fatal: storage state can
/// no longer be trusted, so the process exits non-zero and lets its supervisor
/// restart it.
pub fn install_fail_fast_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);

        // Classify origin for observability only. Exit decision stays
        // unconditional: message strings and thread names are dependency- and
        // version-sensitive, while any unhandled serve-process panic leaves
        // shared runner/storage state suspect. If process::exit ever wedges on
        // atexit handlers, switch this to process::abort at cost of SIGABRT
        // (typically exit 134) instead of clean Kubernetes exit-code reporting.
        let file = info
            .location()
            .map(|location| location.file())
            .unwrap_or("<unknown>");
        let storage_panic = file.contains("surrealkv") || file.contains("surrealdb");
        eprintln!("fatal: unhandled panic (storage_panic={storage_panic}, site={file}); exiting");
        std::process::exit(101);
    }));
}
