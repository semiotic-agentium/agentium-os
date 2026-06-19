// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Event Console HTTP support: message-shape registry and draft validation.

/// Force-link tool crates so descriptor inventory is present in this binary.
///
/// Each is gated on its tool feature so a default build does not pull the crate
/// (and therefore does not register it). `system` is always linked.
#[allow(unused_imports)]
mod tool_inventory {
    #[cfg(feature = "clickup")]
    use baml_tools_clickup as _;
    #[cfg(feature = "github")]
    use baml_tools_github as _;
    #[cfg(feature = "slack")]
    use baml_tools_slack as _;
    use baml_tools_system as _;
}

pub mod catalog;
pub mod handlers;
pub mod registry;
pub mod types;
pub mod validation;

pub use registry::{
    display_label_for_dispatch, find_message_shape, find_message_shape_by_wire, message_shapes,
    registry_response,
};
pub use types::*;
pub use validation::{build_agent_dispatch_request, validate_draft};
