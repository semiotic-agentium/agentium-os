// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Typed per-resource ops SQL builders and shared agent-scope helpers.

use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    agent_runtime_index::{TaskAgentPackageCheck, task_agent_package_check},
};
use crate::{
    error::Result,
    metamodel::{
        AgentRuntimeInstanceNodeId, ContextNodeId, FilterOp, GraphQuery, ScopeState,
        TaskExecutionNodeId, TaskNodeId, keys, labels,
    },
    store::ProvenanceOpsFilters,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpsAgentPackageResolution {
    None,
    TaskValidatedOmit,
    Empty,
    ApplyInstances,
    /// Registry miss — follow the canonical `for_agent_package` edge chain.
    ApplyPackageEdge,
}

impl SurrealProvenanceStore {
    pub(super) async fn resolve_ops_agent_package_filter(
        &self,
        filters: &ProvenanceOpsFilters,
        index: &super::agent_runtime_index::AgentRuntimeIndex,
    ) -> Result<OpsAgentPackageResolution> {
        let Some(pkg) = filters.agent_package.as_deref() else {
            return Ok(OpsAgentPackageResolution::None);
        };

        let task_resolution = if let Some(ref task_id) = filters.task_id {
            Some(self.get_task_agent_id(task_id).await?)
        } else {
            None
        };

        match task_agent_package_check(
            filters.task_id.as_ref(),
            Some(pkg),
            task_resolution.as_ref(),
            index,
        ) {
            TaskAgentPackageCheck::OmitAgentFilter => {
                Ok(OpsAgentPackageResolution::TaskValidatedOmit)
            }
            TaskAgentPackageCheck::MismatchEmpty => Ok(OpsAgentPackageResolution::Empty),
            TaskAgentPackageCheck::ApplyPackageFilter => {
                match index.instance_node_ids_by_package.get(pkg) {
                    Some(instances) if instances.is_empty() => Ok(OpsAgentPackageResolution::Empty),
                    Some(_) => Ok(OpsAgentPackageResolution::ApplyInstances),
                    None => Ok(OpsAgentPackageResolution::ApplyPackageEdge),
                }
            }
        }
    }
}

pub(super) fn apply_ops_wall_time_range<Subject, S>(
    mut q: GraphQuery<Subject, S>,
    filters: &ProvenanceOpsFilters,
) -> GraphQuery<Subject, S>
where
    Subject: labels::NodeLabelTy,
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter,
{
    if filters.from_timestamp_ms.is_some() || filters.to_timestamp_ms.is_some() {
        q = q.with_wall_time_range(filters.from_timestamp_ms, filters.to_timestamp_ms);
    }
    q
}

pub(super) fn apply_message_agent_scope<
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
>(
    mut q: GraphQuery<labels::Message, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::Message, S> {
    if let Some(ref agent_id) = filters.agent_id {
        q = q.for_agent(AgentRuntimeInstanceNodeId::for_agent_id(agent_id));
    } else {
        q = apply_agent_package_scope(q, filters, package_resolution, package_instances);
    }
    q
}

pub(super) fn apply_llm_agent_scope<
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
>(
    mut q: GraphQuery<labels::LlmCall, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::LlmCall, S> {
    if let Some(ref agent_id) = filters.agent_id {
        q = q.for_agent(AgentRuntimeInstanceNodeId::for_agent_id(agent_id));
    } else {
        q = apply_agent_package_scope(q, filters, package_resolution, package_instances);
    }
    q
}

pub(super) fn apply_tool_agent_scope<
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
>(
    mut q: GraphQuery<labels::ToolCall, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::ToolCall, S> {
    if let Some(ref agent_id) = filters.agent_id {
        q = q.for_agent(AgentRuntimeInstanceNodeId::for_agent_id(agent_id));
    } else {
        q = apply_agent_package_scope(q, filters, package_resolution, package_instances);
    }
    q
}

pub(super) fn apply_lifecycle_agent_scope<
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
>(
    mut q: GraphQuery<labels::AgentStop, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::AgentStop, S> {
    if let Some(ref agent_id) = filters.agent_id {
        q = q.for_agent(AgentRuntimeInstanceNodeId::for_agent_id(agent_id));
    } else {
        q = apply_agent_package_scope(q, filters, package_resolution, package_instances);
    }
    q
}

fn apply_agent_package_scope<Subject, S>(
    q: GraphQuery<Subject, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<Subject, S>
where
    Subject: AgentPackageScopable,
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
{
    if package_resolution == OpsAgentPackageResolution::ApplyInstances
        && let Some(instances) = package_instances
    {
        Subject::scope_instances(q, instances)
    } else if package_resolution == OpsAgentPackageResolution::ApplyPackageEdge
        && let Some(pkg) = filters.agent_package.as_deref()
    {
        Subject::scope_package(q, pkg)
    } else {
        q
    }
}

trait AgentPackageScopable: labels::NodeLabelTy {
    fn scope_instances<
        S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
    >(
        q: GraphQuery<Self, S>,
        instances: &[String],
    ) -> GraphQuery<Self, S>
    where
        Self: labels::NodeLabelTy + Sized;
    fn scope_package<
        S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
    >(
        q: GraphQuery<Self, S>,
        package: &str,
    ) -> GraphQuery<Self, S>
    where
        Self: labels::NodeLabelTy + Sized;
}

impl AgentPackageScopable for labels::Message {
    fn scope_instances<
        S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
    >(
        q: GraphQuery<Self, S>,
        instances: &[String],
    ) -> GraphQuery<Self, S> {
        q.for_agent_instances(instances)
    }
    fn scope_package<
        S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
    >(
        q: GraphQuery<Self, S>,
        package: &str,
    ) -> GraphQuery<Self, S> {
        q.for_agent_package(crate::metamodel::node_ids::AgentPackage(
            package.to_string(),
        ))
    }
}

impl AgentPackageScopable for labels::LlmCall {
    fn scope_instances<
        S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
    >(
        q: GraphQuery<Self, S>,
        instances: &[String],
    ) -> GraphQuery<Self, S> {
        q.for_agent_instances(instances)
    }
    fn scope_package<
        S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
    >(
        q: GraphQuery<Self, S>,
        package: &str,
    ) -> GraphQuery<Self, S> {
        q.for_agent_package(crate::metamodel::node_ids::AgentPackage(
            package.to_string(),
        ))
    }
}

impl AgentPackageScopable for labels::ToolCall {
    fn scope_instances<
        S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
    >(
        q: GraphQuery<Self, S>,
        instances: &[String],
    ) -> GraphQuery<Self, S> {
        q.for_agent_instances(instances)
    }
    fn scope_package<
        S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
    >(
        q: GraphQuery<Self, S>,
        package: &str,
    ) -> GraphQuery<Self, S> {
        q.for_agent_package(crate::metamodel::node_ids::AgentPackage(
            package.to_string(),
        ))
    }
}

impl AgentPackageScopable for labels::AgentStop {
    fn scope_instances<
        S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
    >(
        q: GraphQuery<Self, S>,
        instances: &[String],
    ) -> GraphQuery<Self, S> {
        q.for_agent_instances(instances)
    }
    fn scope_package<
        S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
    >(
        q: GraphQuery<Self, S>,
        package: &str,
    ) -> GraphQuery<Self, S> {
        q.for_agent_package(crate::metamodel::node_ids::AgentPackage(
            package.to_string(),
        ))
    }
}

pub(super) fn build_messages_query(
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
    sql_page: Option<(u64, u64, bool, Option<&str>)>,
) -> (String, Value) {
    if let Some(ref ctx) = filters.context_id {
        let q = GraphQuery::<labels::Message, _>::new()
            .scoped_to_ctx(ContextNodeId::for_context_id(ctx));
        let q = apply_message_filters(q, filters, package_resolution, package_instances);
        emit_query(q, sql_page)
    } else {
        let q = GraphQuery::<labels::Message, _>::new().all();
        let q = apply_message_filters(q, filters, package_resolution, package_instances);
        emit_query(q, sql_page)
    }
}

pub(super) fn build_llm_query(
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
    sql_page: Option<(u64, u64, bool, Option<&str>)>,
) -> (String, Value) {
    if let Some(ref ctx) = filters.context_id {
        let q = GraphQuery::<labels::LlmCall, _>::new()
            .scoped_to_ctx(ContextNodeId::for_context_id(ctx));
        let q = apply_llm_filters(q, filters, package_resolution, package_instances);
        emit_query(q, sql_page)
    } else {
        let q = GraphQuery::<labels::LlmCall, _>::new().all();
        let q = apply_llm_filters(q, filters, package_resolution, package_instances);
        emit_query(q, sql_page)
    }
}

pub(super) fn build_tool_query(
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
    sql_page: Option<(u64, u64, bool, Option<&str>)>,
) -> (String, Value) {
    if let Some(ref ctx) = filters.context_id {
        let q = GraphQuery::<labels::ToolCall, _>::new()
            .scoped_to_ctx(ContextNodeId::for_context_id(ctx));
        let q = apply_tool_filters(q, filters, package_resolution, package_instances);
        emit_query(q, sql_page)
    } else {
        let q = GraphQuery::<labels::ToolCall, _>::new().all();
        let q = apply_tool_filters(q, filters, package_resolution, package_instances);
        emit_query(q, sql_page)
    }
}

pub(super) fn build_lifecycle_query(
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
    sql_page: Option<(u64, u64, bool, Option<&str>)>,
) -> (String, Value) {
    if let Some(ref ctx) = filters.context_id {
        let q = GraphQuery::<labels::AgentStop, _>::new()
            .scoped_to_ctx(ContextNodeId::for_context_id(ctx));
        let q = apply_lifecycle_filters(q, filters, package_resolution, package_instances);
        emit_query(q, sql_page)
    } else {
        let q = GraphQuery::<labels::AgentStop, _>::new().all();
        let q = apply_lifecycle_filters(q, filters, package_resolution, package_instances);
        emit_query(q, sql_page)
    }
}

fn apply_message_filters<
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
>(
    mut q: GraphQuery<labels::Message, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::Message, S> {
    q = apply_ops_wall_time_range(q, filters);
    if let Some(ref task_id) = filters.task_id {
        q = q.for_task(TaskNodeId::for_task_id(task_id));
    }
    apply_message_agent_scope(q, filters, package_resolution, package_instances)
}

fn apply_llm_filters<
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
>(
    mut q: GraphQuery<labels::LlmCall, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::LlmCall, S> {
    q = apply_ops_wall_time_range(q, filters);
    if let Some(ref task_id) = filters.task_id {
        q = q.for_task_execution(TaskExecutionNodeId::for_task_id(task_id));
    }
    let mut q = apply_llm_agent_scope(q, filters, package_resolution, package_instances);
    if let Some(ref provider) = filters.provider {
        q = q.filter(keys::Provider, FilterOp::Eq, provider.clone());
    }
    if let Some(ref model) = filters.model {
        q = q.filter(keys::Model, FilterOp::Eq, model.clone());
    }
    q
}

fn apply_tool_filters<
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
>(
    mut q: GraphQuery<labels::ToolCall, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::ToolCall, S> {
    q = apply_ops_wall_time_range(q, filters);
    if let Some(ref task_id) = filters.task_id {
        q = q.for_task_execution(TaskExecutionNodeId::for_task_id(task_id));
    }
    let mut q = apply_tool_agent_scope(q, filters, package_resolution, package_instances);
    if let Some(ref tool_name) = filters.tool_name {
        q = q.filter(keys::ToolName, FilterOp::Eq, tool_name.clone());
    }
    q
}

fn apply_lifecycle_filters<
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
>(
    mut q: GraphQuery<labels::AgentStop, S>,
    filters: &ProvenanceOpsFilters,
    package_resolution: OpsAgentPackageResolution,
    package_instances: Option<&[String]>,
) -> GraphQuery<labels::AgentStop, S> {
    q = apply_ops_wall_time_range(q, filters);
    apply_lifecycle_agent_scope(q, filters, package_resolution, package_instances)
}

fn emit_query<Subject, S>(
    q: GraphQuery<Subject, S>,
    sql_page: Option<(u64, u64, bool, Option<&str>)>,
) -> (String, Value)
where
    Subject: labels::NodeLabelTy,
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
{
    match sql_page {
        Some((offset, limit, sort_desc, sort_by)) => {
            apply_sql_page(q, offset, limit, sort_desc, sort_by).into_surreal()
        }
        None => q.into_surreal(),
    }
}

pub(super) fn apply_sql_page<Subject, S>(
    q: GraphQuery<Subject, S>,
    offset: u64,
    limit: u64,
    sort_desc: bool,
    sort_by: Option<&str>,
) -> GraphQuery<Subject, S>
where
    Subject: labels::NodeLabelTy,
    S: ScopeState + crate::metamodel::query::ScopeQueryEmitter + crate::metamodel::query::Scoped,
{
    use crate::metamodel::query::{SortDir, SortKey};
    let sort_key = match sort_by {
        Some("event_order") => SortKey::EventOrder,
        Some("activity_anchor") | Some("activity_id") => SortKey::ActivityAnchor,
        Some("timestamp_ms") | None => SortKey::ProvTime,
        _ => SortKey::EventOrder,
    };
    q.order_by(
        sort_key,
        if sort_desc {
            SortDir::Desc
        } else {
            SortDir::Asc
        },
    )
    .paginate(offset, limit)
}
