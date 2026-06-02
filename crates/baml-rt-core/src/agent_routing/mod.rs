// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Agent routing identity and discovery catalogue.
//!
//! - [`keys`]: typed package/instance identifiers, [`AgentRouteKey`], [`route_key_from_request`].
//! - [`discovery`]: [`AgentCard`], [`AgentDiscoveryEntry`], [`AgentLister`] for GET /agents and tools.

mod discovery;
mod dispatch_target;
mod keys;

pub use discovery::{AgentCard, AgentDiscoveryEntry, AgentLister};
pub use dispatch_target::DispatchTarget;
pub use keys::{AgentInstanceId, AgentPackageName, AgentRouteKey, route_key_from_request};
