//! Handler builders for memory tools.
//!
//! Each function takes pre-built metadata and an `Arc<MemoryManager>`,
//! returning a `ToolHandler` via `create_multi_send_session_tool_from_async`.

use std::sync::Arc;

use baml_rt_tools::{
    ToolHandler, create_multi_send_session_tool_from_async, tools::ToolFunctionMetadata,
};

use crate::{manager::MemoryManager, types::*};

fn map_err(e: crate::manager::MemoryError) -> baml_rt_core::BamlRtError {
    match e {
        crate::manager::MemoryError::InvalidAgentName(_)
        | crate::manager::MemoryError::UnexpectedIngestNodeIds { .. } => {
            baml_rt_core::BamlRtError::InvalidArgument(e.to_string())
        }
        crate::manager::MemoryError::Io(io) => baml_rt_core::BamlRtError::Io(io),
        crate::manager::MemoryError::LockBusy(_)
        | crate::manager::MemoryError::RegistryPoisoned
        | crate::manager::MemoryError::Amem(_)
        | crate::manager::MemoryError::UnknownStatsStatus(_)
        | crate::manager::MemoryError::PersistTaskFailed(_) => {
            baml_rt_core::BamlRtError::ToolExecution(e.to_string())
        }
    }
}

pub fn memory_add_handler(
    metadata: ToolFunctionMetadata,
    manager: Arc<MemoryManager>,
) -> Arc<dyn ToolHandler> {
    create_multi_send_session_tool_from_async::<
        MemoryAddOpenInput,
        MemoryAddSendInput,
        MemoryAddNextOutput,
        _,
    >(metadata, move |input: MemoryAddSendInput| {
        let mgr = manager.clone();
        Box::pin(async move { mgr.add(input).await.map_err(map_err) })
    })
}

pub fn memory_search_handler(
    metadata: ToolFunctionMetadata,
    manager: Arc<MemoryManager>,
) -> Arc<dyn ToolHandler> {
    create_multi_send_session_tool_from_async::<
        MemorySearchOpenInput,
        MemorySearchSendInput,
        MemorySearchNextOutput,
        _,
    >(metadata, move |input: MemorySearchSendInput| {
        let mgr = manager.clone();
        Box::pin(async move { mgr.search(input).await.map_err(map_err) })
    })
}

pub fn memory_traverse_handler(
    metadata: ToolFunctionMetadata,
    manager: Arc<MemoryManager>,
) -> Arc<dyn ToolHandler> {
    create_multi_send_session_tool_from_async::<
        MemoryTraverseOpenInput,
        MemoryTraverseSendInput,
        MemoryTraverseNextOutput,
        _,
    >(metadata, move |input: MemoryTraverseSendInput| {
        let mgr = manager.clone();
        Box::pin(async move { mgr.traverse(input).await.map_err(map_err) })
    })
}

pub fn memory_resolve_handler(
    metadata: ToolFunctionMetadata,
    manager: Arc<MemoryManager>,
) -> Arc<dyn ToolHandler> {
    create_multi_send_session_tool_from_async::<
        MemoryResolveOpenInput,
        MemoryResolveSendInput,
        MemoryResolveNextOutput,
        _,
    >(metadata, move |input: MemoryResolveSendInput| {
        let mgr = manager.clone();
        Box::pin(async move { mgr.resolve(input).await.map_err(map_err) })
    })
}

pub fn memory_impact_handler(
    metadata: ToolFunctionMetadata,
    manager: Arc<MemoryManager>,
) -> Arc<dyn ToolHandler> {
    create_multi_send_session_tool_from_async::<
        MemoryImpactOpenInput,
        MemoryImpactSendInput,
        MemoryImpactNextOutput,
        _,
    >(metadata, move |input: MemoryImpactSendInput| {
        let mgr = manager.clone();
        Box::pin(async move { mgr.impact(input).await.map_err(map_err) })
    })
}

pub fn memory_link_handler(
    metadata: ToolFunctionMetadata,
    manager: Arc<MemoryManager>,
) -> Arc<dyn ToolHandler> {
    create_multi_send_session_tool_from_async::<
        MemoryLinkOpenInput,
        MemoryLinkSendInput,
        MemoryLinkNextOutput,
        _,
    >(metadata, move |input: MemoryLinkSendInput| {
        let mgr = manager.clone();
        Box::pin(async move { mgr.link(input).await.map_err(map_err) })
    })
}

pub fn memory_stats_handler(
    metadata: ToolFunctionMetadata,
    manager: Arc<MemoryManager>,
) -> Arc<dyn ToolHandler> {
    create_multi_send_session_tool_from_async::<
        MemoryStatsOpenInput,
        MemoryStatsSendInput,
        MemoryStatsNextOutput,
        _,
    >(metadata, move |_input: MemoryStatsSendInput| {
        let mgr = manager.clone();
        Box::pin(async move { mgr.stats().await.map_err(map_err) })
    })
}
