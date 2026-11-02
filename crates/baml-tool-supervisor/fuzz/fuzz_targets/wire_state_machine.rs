// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Fuzz the v2 JSON message parsers.
//!
//! Frame-length/state-machine handling is exercised end-to-end by
//! `tests/parser_fuzz_smoke.rs` (random frames → the supervisor must retire the
//! connection). This target covers the serde surface the reader hits first:
//! malformed host messages, oversized `bundle_len`, and guest-message drift.

#![no_main]

use baml_sandbox_protocol::protocol_v2::{GuestMessage, HostMessage};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<HostMessage>(data);
    let _ = serde_json::from_slice::<GuestMessage>(data);
});
