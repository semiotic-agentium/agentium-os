// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed host dispatch target: route identity + live booted runtime id.

use super::keys::AgentRouteKey;
use crate::ids::AgentId;

/// Host dispatch routing target with the live booted [`AgentId`] for graph edges.
///
/// Route (`package` + `instance`) keys transcript idempotency; `agent_id` is the
/// authoritative runtime identity for `HOST_DISPATCH_TARGET` and execution attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchTarget {
    pub route: AgentRouteKey,
    /// Live booted runtime. When absent, operational transcript rows are still written
    /// but no `HOST_DISPATCH_TARGET` edge is emitted.
    pub agent_id: Option<AgentId>,
}

impl DispatchTarget {
    #[must_use]
    pub fn new(route: AgentRouteKey, agent_id: AgentId) -> Self {
        Self {
            route,
            agent_id: Some(agent_id),
        }
    }

    #[must_use]
    pub fn with_optional_agent(route: AgentRouteKey, agent_id: Option<AgentId>) -> Self {
        Self { route, agent_id }
    }

    pub fn package(&self) -> &str {
        self.route.agent_package.as_str()
    }

    pub fn instance(&self) -> &str {
        self.route.agent_instance_id.as_str()
    }
}
