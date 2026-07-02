// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Legacy entrypoint for integration tests (`CARGO_BIN_EXE_baml-agent-runner`).
//! Production uses `agentium serve`.

use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = baml_agent_runner::RunnerCli::parse();
    baml_agent_runner::run(cli).await
}
