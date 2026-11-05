// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Length-prefixed JSON frame codec for the host to supervisor transport. The
//! codec lives in [`baml_sandbox_protocol`] so host and guest share it.

pub use baml_sandbox_protocol::{CodecError, MAX_FRAME_BYTES, TsrpcChannel};
