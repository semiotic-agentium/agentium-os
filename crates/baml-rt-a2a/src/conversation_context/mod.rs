// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Conversation context projection from provenance into BAML wire JSON.

mod projecting_provider;

pub use projecting_provider::{ProjectingConversationContextProvider, to_projection_item};
