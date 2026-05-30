// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! HTTP transport plumbing for MCP: URL/network policy validation, reserved
//! header guards, secret injection, and reqwest client construction.

pub mod headers;
pub mod policy;
pub mod transport;
