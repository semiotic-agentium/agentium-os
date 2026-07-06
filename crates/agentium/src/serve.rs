// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `agentium serve` — start the Agentium platform.

use baml_agent_runner::RunnerCli;

pub fn run(cli: RunnerCli) -> anyhow::Result<()> {
    baml_agent_runner::run_blocking(cli)
}
