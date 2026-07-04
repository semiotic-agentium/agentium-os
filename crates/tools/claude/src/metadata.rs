// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tool metadata registration for the Claude bundle.

use baml_rt_tools::baml_tool;

use crate::tools::{ClaudeToolNextOutput, ClaudeToolOpenInput, ClaudeToolSendInput};

#[baml_tool(
    name = "claude/dev",
    description = "Host-managed Claude streaming session. Open once, send prompt/content, \
        then call next() until completion is DONE/INTERRUPTED or INPUT_REQUIRED for resume.",
    tags = ["claude", "stream", "session"],
    access = Write,
    capability = Streaming,
    metadata_only,
    open_input = ClaudeToolOpenInput,
    input = ClaudeToolSendInput,
    output = ClaudeToolNextOutput,
)]
pub struct ClaudeDev;
