//! Agent runtime package → instance index (registry + in-process cache).

use std::collections::HashMap;

use baml_rt_core::ids::{AgentId, TaskId};
use serde_json::Value;

use super::{
    SurrealProvenanceStore,
    helpers::{check_and_take_zero, map_surreal_error},
};
use crate::{
    error::Result,
    id_semantics::{AgentRuntimeInstanceId, AgentRuntimeInstanceInput},
    metamodel::{GraphQuery, labels},
    store::TaskAgentResolution,
    surreal_tables::TBL_AGENT_PACKAGE_INSTANCE,
    types::ProvAgentId,
};

/// Outcome of checking whether `agent_package` adds information when `task_id`
/// is already set (task head pointer implies executing agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskAgentPackageCheck {
    /// Task agent matches package — omit redundant agent edge filter.
    OmitAgentFilter,
    /// Task agent belongs to a different package — empty result.
    MismatchEmpty,
    /// No task scope or agent could not be validated — apply package edge filter.
    ApplyPackageFilter,
}

pub(super) fn task_agent_package_check(
    task_id: Option<&TaskId>,
    agent_package: Option<&str>,
    resolution: Option<&TaskAgentResolution>,
    index: &AgentRuntimeIndex,
) -> TaskAgentPackageCheck {
    let (Some(_task_id), Some(pkg)) = (task_id, agent_package) else {
        return TaskAgentPackageCheck::ApplyPackageFilter;
    };
    match resolution {
        Some(TaskAgentResolution::Resolved(agent_id)) => {
            match index.identity_by_agent_id.get(agent_id.as_str()) {
                Some((task_pkg, _)) if task_pkg == pkg => TaskAgentPackageCheck::OmitAgentFilter,
                Some(_) => TaskAgentPackageCheck::MismatchEmpty,
                None => TaskAgentPackageCheck::OmitAgentFilter,
            }
        }
        Some(TaskAgentResolution::NotLinked | TaskAgentResolution::TimedOut) | None => {
            TaskAgentPackageCheck::OmitAgentFilter
        }
    }
}

/// Agent runtime rows indexed once per request (package filter uses instance node ids).
#[derive(Debug, Clone, Default)]
pub(super) struct AgentRuntimeIndex {
    pub identity_by_agent_id: HashMap<String, (String, String)>,
    pub instance_node_ids_by_package: HashMap<String, Vec<String>>,
}

impl SurrealProvenanceStore {
    pub(super) fn invalidate_agent_runtime_index_cache(&self) {
        if let Ok(mut guard) = self.agent_runtime_index_cache.write() {
            *guard = None;
        }
    }

    pub(super) async fn load_agent_runtime_index(&self) -> Result<AgentRuntimeIndex> {
        if let Ok(guard) = self.agent_runtime_index_cache.read()
            && let Some(index) = guard.as_ref()
        {
            return Ok(index.clone());
        }

        let index = self.load_agent_runtime_index_uncached().await?;
        if let Ok(mut guard) = self.agent_runtime_index_cache.write() {
            *guard = Some(index.clone());
        }
        Ok(index)
    }

    async fn load_agent_runtime_index_uncached(&self) -> Result<AgentRuntimeIndex> {
        let registry_index = self.load_agent_runtime_index_from_registry().await?;
        if !registry_index.identity_by_agent_id.is_empty() {
            return Ok(registry_index);
        }
        self.scan_agent_runtime_index_from_graph().await
    }

    async fn load_agent_runtime_index_from_registry(&self) -> Result<AgentRuntimeIndex> {
        let sql = format!(
            "SELECT instance_node_id, agent_package, agent_id, agent_version \
             FROM {TBL_AGENT_PACKAGE_INSTANCE}"
        );
        let response = self.db.query(&sql).await.map_err(map_surreal_error)?;
        let rows: Vec<Value> = check_and_take_zero(response, map_surreal_error)?;
        Ok(index_from_registry_rows(&rows))
    }

    async fn scan_agent_runtime_index_from_graph(&self) -> Result<AgentRuntimeIndex> {
        let (sql, binds) = GraphQuery::<labels::AgentRuntimeInstance, _>::new()
            .all()
            .into_surreal();
        let rows = self.execute_typed_node_query(&sql, &binds).await?;
        let boot_complete_rows: Vec<Value> = rows
            .iter()
            .filter(|row| {
                row.get("props")
                    .and_then(Value::as_object)
                    .and_then(|props| props.get("a2a_archive_path").and_then(Value::as_str))
                    .is_some_and(|s| !s.is_empty())
            })
            .cloned()
            .collect();
        let index = index_from_graph_rows(&boot_complete_rows);
        for row in &boot_complete_rows {
            let Some(props) = row.get("props").and_then(Value::as_object) else {
                continue;
            };
            let Some(instance_node_id) = row
                .get("node_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let Some(agent_id) = props
                .get("a2a_agent_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let agent_package = normalize_agent_field(
                props.get("a2a_agent_type").and_then(Value::as_str),
                "unknown",
            );
            let agent_version = normalize_agent_field(
                props.get("a2a_agent_version").and_then(Value::as_str),
                "unknown",
            );
            self.upsert_agent_package_registry_row(
                instance_node_id,
                &agent_package,
                agent_id,
                &agent_version,
            )
            .await?;
        }
        Ok(index)
    }

    pub(super) async fn upsert_agent_package_registry_on_boot(
        &self,
        agent_id: &AgentId,
        agent_package: &str,
        agent_version: &str,
    ) -> Result<()> {
        let instance_node_id =
            ProvAgentId::derived::<AgentRuntimeInstanceId>(AgentRuntimeInstanceInput { agent_id })
                .into_string();
        self.upsert_agent_package_registry_row(
            &instance_node_id,
            agent_package,
            agent_id.as_str(),
            agent_version,
        )
        .await?;
        self.invalidate_agent_runtime_index_cache();
        Ok(())
    }

    async fn upsert_agent_package_registry_row(
        &self,
        instance_node_id: &str,
        agent_package: &str,
        agent_id: &str,
        agent_version: &str,
    ) -> Result<()> {
        let sql = format!(
            "UPSERT {TBL_AGENT_PACKAGE_INSTANCE} SET \
               instance_node_id = $instance_node_id, \
               agent_package = $agent_package, \
               agent_id = $agent_id, \
               agent_version = $agent_version \
             WHERE instance_node_id = $instance_node_id"
        );
        self.db
            .query(&sql)
            .bind(("instance_node_id", instance_node_id.to_string()))
            .bind(("agent_package", agent_package.to_string()))
            .bind(("agent_id", agent_id.to_string()))
            .bind(("agent_version", agent_version.to_string()))
            .await
            .map_err(map_surreal_error)?
            .check()
            .map_err(map_surreal_error)?;
        Ok(())
    }

    pub(super) async fn execute_typed_node_query(
        &self,
        sql: &str,
        binds: &Value,
    ) -> Result<Vec<Value>> {
        let mut q = self.db.query(sql);
        if let Some(obj) = binds.as_object() {
            for (k, v) in obj {
                q = q.bind((k.clone(), v.clone()));
            }
        }
        let response = q.await.map_err(map_surreal_error)?;
        check_and_take_zero(response, map_surreal_error)
    }
}

fn normalize_agent_field(raw: Option<&str>, fallback: &str) -> String {
    raw.map(str::trim)
        .filter(|s| !s.is_empty() && *s != "null")
        .unwrap_or(fallback)
        .to_string()
}

fn index_from_graph_rows(rows: &[Value]) -> AgentRuntimeIndex {
    let mut identity_by_agent_id: HashMap<String, (String, String)> = HashMap::new();
    let mut instance_node_ids_by_package: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let Some(props) = row.get("props").and_then(Value::as_object) else {
            continue;
        };
        let Some(node_id) = row
            .get("node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(agent_id) = props
            .get("a2a_agent_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let agent_id = agent_id.to_string();
        let agent_package = normalize_agent_field(
            props.get("a2a_agent_type").and_then(Value::as_str),
            "unknown",
        );
        let agent_version = normalize_agent_field(
            props.get("a2a_agent_version").and_then(Value::as_str),
            "unknown",
        );
        identity_by_agent_id.insert(agent_id, (agent_package.clone(), agent_version));
        instance_node_ids_by_package
            .entry(agent_package)
            .or_default()
            .push(node_id.to_string());
    }
    AgentRuntimeIndex {
        identity_by_agent_id,
        instance_node_ids_by_package,
    }
}

fn index_from_registry_rows(rows: &[Value]) -> AgentRuntimeIndex {
    let mut identity_by_agent_id: HashMap<String, (String, String)> = HashMap::new();
    let mut instance_node_ids_by_package: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let Some(instance_node_id) = row
            .get("instance_node_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let agent_package =
            normalize_agent_field(row.get("agent_package").and_then(Value::as_str), "unknown");
        let agent_id = normalize_agent_field(row.get("agent_id").and_then(Value::as_str), "");
        let agent_version =
            normalize_agent_field(row.get("agent_version").and_then(Value::as_str), "unknown");
        if !agent_id.is_empty() {
            identity_by_agent_id.insert(agent_id.clone(), (agent_package.clone(), agent_version));
        }
        instance_node_ids_by_package
            .entry(agent_package)
            .or_default()
            .push(instance_node_id.to_string());
    }
    AgentRuntimeIndex {
        identity_by_agent_id,
        instance_node_ids_by_package,
    }
}

pub(super) fn normalize_agent_field_for_ops(raw: Option<&str>, fallback: &str) -> String {
    normalize_agent_field(raw, fallback)
}

#[cfg(test)]
mod task_agent_package_tests {
    use std::collections::HashMap;

    use baml_rt_core::ids::{AgentId, ExternalId, TaskId};
    use baml_rt_id::UuidId;

    use super::*;
    use crate::store::TaskAgentResolution;

    const AGENT_UUID: &str = "11111111-2222-3333-4444-555555555555";

    fn task_id() -> TaskId {
        TaskId::from_external(ExternalId::new("dispatch-unit-test"))
    }

    fn agent_id() -> AgentId {
        AgentId::from_uuid(UuidId::parse_str(AGENT_UUID).expect("valid uuid"))
    }

    fn index_with_package(package: &str) -> AgentRuntimeIndex {
        let agent_id = agent_id().as_str().to_string();
        let mut identity_by_agent_id = HashMap::new();
        identity_by_agent_id.insert(agent_id, (package.to_string(), "1".to_string()));
        let mut instance_node_ids_by_package = HashMap::new();
        instance_node_ids_by_package.insert(
            package.to_string(),
            vec![format!("agent_instance:{AGENT_UUID}")],
        );
        AgentRuntimeIndex {
            identity_by_agent_id,
            instance_node_ids_by_package,
        }
    }

    #[test]
    fn omit_filter_when_task_agent_matches_package() {
        let index = index_with_package("slack-agent");
        let resolution = TaskAgentResolution::Resolved(agent_id());
        assert_eq!(
            task_agent_package_check(
                Some(&task_id()),
                Some("slack-agent"),
                Some(&resolution),
                &index,
            ),
            TaskAgentPackageCheck::OmitAgentFilter,
        );
    }

    #[test]
    fn empty_on_package_mismatch() {
        let index = index_with_package("slack-agent");
        let resolution = TaskAgentResolution::Resolved(agent_id());
        assert_eq!(
            task_agent_package_check(
                Some(&task_id()),
                Some("clickup-agent"),
                Some(&resolution),
                &index,
            ),
            TaskAgentPackageCheck::MismatchEmpty,
        );
    }

    #[test]
    fn apply_filter_without_task_scope() {
        let index = index_with_package("slack-agent");
        assert_eq!(
            task_agent_package_check(None, Some("slack-agent"), None, &index),
            TaskAgentPackageCheck::ApplyPackageFilter,
        );
    }
}
