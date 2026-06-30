// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `agentium` — unified Agentium platform host and developer SDK.

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
    dispatch::run(cli::Cli::parse())
}
