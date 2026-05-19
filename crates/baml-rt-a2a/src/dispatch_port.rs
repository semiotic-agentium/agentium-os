//! [`AgentDispatchPort`] adapter for [`AgentRegistry`].

use async_trait::async_trait;
use baml_rt_core::{
    AgentDispatchAck, AgentDispatchPort, AgentDispatchRequest, AgentRouteKey, Result,
};

use crate::AgentRegistry;

/// Delivers dispatch requests through an [`AgentRegistry`].
pub struct RegistryDispatchPort<'a> {
    registry: &'a dyn AgentRegistry,
}

impl<'a> RegistryDispatchPort<'a> {
    pub fn new(registry: &'a dyn AgentRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl AgentDispatchPort for RegistryDispatchPort<'_> {
    async fn dispatch(
        &self,
        target: &AgentRouteKey,
        request: AgentDispatchRequest,
    ) -> Result<AgentDispatchAck> {
        self.registry.handle_dispatch(target, request).await
    }
}
