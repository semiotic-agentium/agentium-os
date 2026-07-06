// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Legacy entrypoint for integration tests (`CARGO_BIN_EXE_baml-agent-runner`).
//! Production uses `agentium serve`.

use clap::Parser;

fn main() -> anyhow::Result<()> {
    baml_agent_runner::run_blocking(baml_agent_runner::RunnerCli::parse())
}
