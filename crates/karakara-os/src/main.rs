// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! `karak` — unified Karakara platform host and developer SDK.

mod cli;
mod commands;
mod dispatch;
mod eval;
mod event_schemas;
mod generated_baml;
mod interactive;
mod patchers;
mod project;
mod serve;
mod skills;
mod templates;
mod text;
mod tool_catalog;
mod transaction;
mod workspace;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    // SurrealDB constructs sysinfo::System::new_all(), which keeps one /proc
    // descriptor per indexed process. On busy Linux hosts that can push the
    // next microsandbox/libkrun descriptor past FD_SETSIZE and abort in FD_SET.
    // The runner only needs its own process metrics, so cap sysinfo's cache
    // before any repository or provenance backend is initialized.
    #[cfg(target_os = "linux")]
    sysinfo::set_open_files_limit(512);

    dispatch::run(cli::Cli::parse())
}
