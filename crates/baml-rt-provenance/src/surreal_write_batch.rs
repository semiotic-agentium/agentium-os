// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Single `BEGIN…COMMIT` transaction for one provenance event (graph + blob pointers + payloads).

use std::collections::HashMap;

use baml_rt_vocabulary::vocabulary::a2a;
use serde_json::Value;

use crate::{
    graph_model::GraphNodeLabel,
    id_semantics::{context_entity_id_string, task_entity_id_string_raw},
    normalizer::{A2aRelationType, HeadPointerRepoint, NormalizedProv},
    payload_record::PayloadRecord,
    payload_storage,
    prov_write_semantics::{
        semantic_associated_with_label, semantic_derived_from_label, semantic_generated_by_label,
        semantic_used_label,
    },
    surreal_sql::{edge_props_object, storage_safe_props_sorted_keys},
    surreal_tables::{TBL_BLOB, TBL_CONTEXT_TRANSCRIPT_INDEX, TBL_EDGE, TBL_NODE, TBL_PAYLOAD},
    types::ProvNodeRef,
    vocabulary::semantic_labels,
};

/// One named bind for `Query::bind`.
#[derive(Clone)]
pub(crate) struct TxBind {
    pub name: String,
    pub value: Value,
}

pub(crate) struct EventWritePlan {
    pub sql: String,
    pub binds: Vec<TxBind>,
    pub statement_count: usize,
}

/// Executable transaction plan for [`SurrealProvenanceStore::run_event_write_plan`].
pub(crate) trait ExecutableSurrealPlan {
    fn into_sql_and_binds(self) -> (String, Vec<TxBind>);
}

impl ExecutableSurrealPlan for EventWritePlan {
    fn into_sql_and_binds(self) -> (String, Vec<TxBind>) {
        (self.sql, self.binds)
    }
}

struct WriteFragment {
    stmts: Vec<String>,
    binds: Vec<TxBind>,
}

impl WriteFragment {
    fn merge(mut a: Self, mut b: Self) -> Self {
        a.stmts.append(&mut b.stmts);
        a.binds.append(&mut b.binds);
        a
    }

    fn into_event_plan(self) -> EventWritePlan {
        let statement_count = self.stmts.len();
        let sql = if self.stmts.is_empty() {
            String::new()
        } else {
            format!("BEGIN;\n{};\nCOMMIT;", self.stmts.join(";\n"))
        };
        EventWritePlan {
            sql,
            binds: self.binds,
            statement_count,
        }
    }
}

fn merged_txn_estimates(graph: &WriteFragment, data: &WriteFragment) -> (usize, usize) {
    let bind_count = graph.binds.len() + data.binds.len();
    let n = graph.stmts.len() + data.stmts.len();
    let sql_bytes = if n == 0 {
        0
    } else {
        const PREFIX: usize = 8;
        const SUFFIX: usize = 10;
        let stmts_sum: usize = graph
            .stmts
            .iter()
            .chain(data.stmts.iter())
            .map(|s| s.len())
            .sum();
        let sep = if n <= 1 { 0 } else { (n - 1) * 2 };
        PREFIX + stmts_sum + sep + SUFFIX
    };
    (bind_count, sql_bytes)
}

fn label_from_prov_type(prov_type: Option<&str>, default: &str) -> String {
    prov_type
        .map(|t| {
            t.rsplit_once(':')
                .map(|(_, suffix)| suffix)
                .unwrap_or(t)
                .to_string()
        })
        .unwrap_or_else(|| default.to_string())
}

fn label_for_prov_node_ref<'a>(
    node_ref: &ProvNodeRef,
    entity_labels: &'a HashMap<String, String>,
    activity_labels: &'a HashMap<String, String>,
    agent_labels: &'a HashMap<String, String>,
) -> &'a str {
    match node_ref {
        ProvNodeRef::Entity(eid) => entity_labels
            .get(eid.as_str())
            .map(|s| s.as_str())
            .unwrap_or("ProvEntity"),
        ProvNodeRef::Activity(aid) => activity_labels
            .get(aid.as_str())
            .map(|s| s.as_str())
            .unwrap_or("ProvActivity"),
        ProvNodeRef::Agent(agid) => agent_labels
            .get(agid.as_str())
            .map(|s| s.as_str())
            .unwrap_or("ProvAgent"),
    }
}

fn push_scoped_to_context_edge(
    stmts: &mut Vec<String>,
    binds: &mut Vec<TxBind>,
    ei: &mut usize,
    from_id: &str,
    from_label: &str,
    ctx_node_id: &str,
) {
    push_edge_upsert(
        stmts,
        binds,
        ei,
        EdgeUpsertSpec {
            from_id,
            from_label,
            rel_type: crate::vocabulary::context_scope::SCOPED_TO,
            to_id: ctx_node_id,
            to_label: "Context",
        },
        &HashMap::new(),
    );
}

/// Resolve `LlmCall` / `ToolCall` activity `node_id` for this anchor without a post-write SELECT.
pub(crate) fn call_activity_id_from_normalized(
    normalized: &NormalizedProv,
    anchor: &str,
) -> Option<String> {
    for (id, activity) in normalized.document.activities() {
        let label = label_from_prov_type(activity.prov_type.as_deref(), "ProvActivity");
        if label != "LlmCall" && label != "ToolCall" {
            continue;
        }
        if let Some(Value::String(a)) = activity.attributes.get(a2a::ACTIVITY_ANCHOR)
            && a == anchor
        {
            return Some(id.as_str().to_string());
        }
    }
    None
}

fn push_bind(binds: &mut Vec<TxBind>, name: impl Into<String>, value: Value) {
    binds.push(TxBind {
        name: name.into(),
        value,
    });
}

/// From/to ids and labels plus `rel_type` for one `prov_edge` upsert.
struct EdgeUpsertSpec<'a> {
    from_id: &'a str,
    from_label: &'a str,
    rel_type: &'a str,
    to_id: &'a str,
    to_label: &'a str,
}

fn push_node_upsert(
    stmts: &mut Vec<String>,
    binds: &mut Vec<TxBind>,
    ni: &mut usize,
    node_id: &str,
    label: &str,
    props: &HashMap<String, Value>,
) {
    let safe_rows = storage_safe_props_sorted_keys(props);
    let i = *ni;
    *ni += 1;
    let id_k = format!("n{i}_id");
    let lab_k = format!("n{i}_lab");
    push_bind(binds, &id_k, Value::String(node_id.to_string()));
    push_bind(binds, &lab_k, Value::String(label.to_string()));
    let mut sets = vec![format!("node_id = ${id_k}"), format!("label = ${lab_k}")];
    for (j, (k, v)) in safe_rows.iter().enumerate() {
        let pk = format!("n{i}_p{j}");
        push_bind(binds, &pk, v.clone());
        sets.push(format!("props.{k} = ${pk}"));
    }
    stmts.push(format!(
        "UPSERT {TBL_NODE} SET {} WHERE node_id = ${id_k}",
        sets.join(", ")
    ));
}

fn push_edge_upsert(
    stmts: &mut Vec<String>,
    binds: &mut Vec<TxBind>,
    ei: &mut usize,
    spec: EdgeUpsertSpec<'_>,
    props: &HashMap<String, Value>,
) {
    let EdgeUpsertSpec {
        from_id,
        from_label,
        rel_type,
        to_id,
        to_label,
    } = spec;
    let i = *ei;
    *ei += 1;
    let fk = format!("e{i}_fid");
    let flk = format!("e{i}_flab");
    let tk = format!("e{i}_tid");
    let tlk = format!("e{i}_tlab");
    let rk = format!("e{i}_rel");
    let pk = format!("e{i}_pr");
    push_bind(binds, &fk, Value::String(from_id.to_string()));
    push_bind(binds, &flk, Value::String(from_label.to_string()));
    push_bind(binds, &tk, Value::String(to_id.to_string()));
    push_bind(binds, &tlk, Value::String(to_label.to_string()));
    push_bind(binds, &rk, Value::String(rel_type.to_string()));
    push_bind(binds, &pk, edge_props_object(props));
    stmts.push(format!(
        "UPSERT {TBL_EDGE} SET from_id = ${fk}, from_label = ${flk}, \
         to_id = ${tk}, to_label = ${tlk}, rel_type = ${rk}, props = ${pk} \
         WHERE from_id = ${fk} AND rel_type = ${rk} AND to_id = ${tk}"
    ));
}

/// Emit a `DELETE prov_edge WHERE from_id = ? AND rel_type = ?`
/// statement. Used by [`push_head_pointer_repoint`] to clear any prior
/// head-pointer edge from `from_id` before inserting the new one. The two
/// statements run inside the same `BEGIN..COMMIT` transaction as the rest
/// of the event's graph writes, so a concurrent reader cannot observe a
/// gap with zero edges or a window with two competing edges.
fn push_edge_delete_by_from_rel(
    stmts: &mut Vec<String>,
    binds: &mut Vec<TxBind>,
    di: &mut usize,
    from_id: &str,
    rel_type: &str,
) {
    let i = *di;
    *di += 1;
    let fk = format!("hd{i}_fid");
    let rk = format!("hd{i}_rel");
    push_bind(binds, &fk, Value::String(from_id.to_string()));
    push_bind(binds, &rk, Value::String(rel_type.to_string()));
    stmts.push(format!(
        "DELETE {TBL_EDGE} WHERE from_id = ${fk} AND rel_type = ${rk}"
    ));
}

/// Emit the DELETE-then-UPSERT pair that re-points one head-pointer edge
/// (`WAS_LAST_*`) to a new head
/// inside the current event transaction. Cardinality (exactly one
/// `(from_id, rel_type)` row per head-pointer per Task) is enforced
/// procedurally by the DELETE preceding the UPSERT inside `BEGIN..COMMIT`;
/// SurrealDB v3 does not yet support partial / WHERE-filtered UNIQUE
/// indexes, so an index-level backstop on `(rel_type, from_id)` is not
/// available without breaking the existing chain edges that legitimately
/// fan out from the same `from_id`.
#[allow(clippy::too_many_arguments)]
fn push_transcript_index_upsert(
    stmts: &mut Vec<String>,
    binds: &mut Vec<TxBind>,
    ti: &mut usize,
    context_id: &str,
    node_id: &str,
    label: &str,
    event_order: u64,
    task_entity_id: Option<&str>,
) {
    let i = *ti;
    *ti += 1;
    let ck = format!("ti{i}_ctx");
    let nk = format!("ti{i}_nid");
    let lk = format!("ti{i}_lab");
    let ok = format!("ti{i}_ord");
    let tk = format!("ti{i}_task");
    push_bind(binds, &ck, Value::String(context_id.to_string()));
    push_bind(binds, &nk, Value::String(node_id.to_string()));
    push_bind(binds, &lk, Value::String(label.to_string()));
    push_bind(binds, &ok, Value::Number(event_order.into()));
    push_bind(
        binds,
        &tk,
        task_entity_id
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null),
    );
    stmts.push(format!(
        "UPSERT {TBL_CONTEXT_TRANSCRIPT_INDEX} SET \
         context_id = ${ck}, node_id = ${nk}, label = ${lk}, event_order = ${ok}, \
         task_entity_id = ${tk} \
         WHERE context_id = ${ck} AND node_id = ${nk}"
    ));
}

fn push_head_pointer_repoint(
    stmts: &mut Vec<String>,
    binds: &mut Vec<TxBind>,
    di: &mut usize,
    ei: &mut usize,
    repoint: &HeadPointerRepoint,
) {
    let rel_type = repoint.rel.as_rel_str();
    push_edge_delete_by_from_rel(stmts, binds, di, &repoint.from_id, rel_type);
    push_edge_upsert(
        stmts,
        binds,
        ei,
        EdgeUpsertSpec {
            from_id: &repoint.from_id,
            from_label: &repoint.from_label,
            rel_type,
            to_id: &repoint.to_id,
            to_label: &repoint.to_label,
        },
        &HashMap::new(),
    );
}

fn push_blob_upsert(
    stmts: &mut Vec<String>,
    binds: &mut Vec<TxBind>,
    bi: &mut usize,
    hash: &str,
    body: &str,
) {
    let i = *bi;
    *bi += 1;
    let hk = format!("b{i}_h");
    let bk = format!("b{i}_body");
    push_bind(binds, &hk, Value::String(hash.to_string()));
    push_bind(binds, &bk, Value::String(body.to_string()));
    stmts.push(format!(
        "UPSERT {TBL_BLOB} SET content_hash = ${hk}, body = ${bk} WHERE content_hash = ${hk}"
    ));
}

fn push_payload_upsert(
    stmts: &mut Vec<String>,
    binds: &mut Vec<TxBind>,
    pi: &mut usize,
    p: &PayloadRecord,
) {
    let i = *pi;
    *pi += 1;
    let id_k = format!("p{i}_id");
    let aa_k = format!("p{i}_aa");
    let act_k = format!("p{i}_act");
    let kind_k = format!("p{i}_kind");
    let pj_k = format!("p{i}_pj");
    let ch_k = format!("p{i}_ch");
    let sk_k = format!("p{i}_sk");
    let fk_k = format!("p{i}_fk");
    let st_k = format!("p{i}_st");
    push_bind(binds, &id_k, Value::String(p.payload_id.clone()));
    push_bind(binds, &aa_k, Value::String(p.activity_anchor_id.clone()));
    push_bind(
        binds,
        &act_k,
        match &p.activity_id {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        },
    );
    push_bind(binds, &kind_k, Value::String(p.payload_kind.clone()));
    push_bind(binds, &pj_k, Value::String(p.payload_json.clone()));
    push_bind(
        binds,
        &ch_k,
        match &p.content_hash {
            Some(h) => Value::String(h.clone()),
            None => Value::Null,
        },
    );
    push_bind(
        binds,
        &sk_k,
        Value::String(p.storage_kind.as_str().to_string()),
    );
    push_bind(
        binds,
        &fk_k,
        match &p.file_key {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        },
    );
    push_bind(binds, &st_k, Value::String(p.search_text.clone()));
    stmts.push(format!(
        "UPSERT {TBL_PAYLOAD} SET payload_id = ${id_k}, activity_anchor_id = ${aa_k}, \
         activity_id = ${act_k}, payload_kind = ${kind_k}, payload_json = ${pj_k}, \
         content_hash = ${ch_k}, storage_kind = ${sk_k}, file_key = ${fk_k}, search_text = ${st_k} \
         WHERE payload_id = ${id_k}"
    ));
}

struct LabelMaps {
    entity_labels: HashMap<String, String>,
    activity_labels: HashMap<String, String>,
    agent_labels: HashMap<String, String>,
}

fn build_label_maps(normalized: &NormalizedProv) -> LabelMaps {
    let mut entity_labels = HashMap::new();
    for (id, entity) in normalized.document.entities() {
        let label = label_from_prov_type(entity.prov_type.as_deref(), "ProvEntity");
        entity_labels.insert(id.as_str().to_string(), label);
    }
    let mut activity_labels = HashMap::new();
    for (id, activity) in normalized.document.activities() {
        let label = label_from_prov_type(activity.prov_type.as_deref(), "ProvActivity");
        activity_labels.insert(id.as_str().to_string(), label);
    }
    let mut agent_labels = HashMap::new();
    for (id, agent) in normalized.document.agents() {
        let label = label_from_prov_type(agent.prov_type.as_deref(), "ProvAgent");
        agent_labels.insert(id.as_str().to_string(), label);
    }
    for (id, label) in &normalized.agent_labels {
        agent_labels
            .entry(id.clone())
            .or_insert_with(|| label.clone());
    }
    LabelMaps {
        entity_labels,
        activity_labels,
        agent_labels,
    }
}

fn build_graph_fragment(normalized: &NormalizedProv, context_id: Option<&str>) -> WriteFragment {
    let LabelMaps {
        entity_labels,
        activity_labels,
        agent_labels,
    } = build_label_maps(normalized);

    let mut stmts: Vec<String> = Vec::new();
    let mut binds: Vec<TxBind> = Vec::new();
    let mut ni = 0usize;
    let mut ei = 0usize;
    let mut di = 0usize;
    let mut ti = 0usize;

    for (id, entity) in normalized.document.entities() {
        let label = entity_labels
            .get(id.as_str())
            .map(String::as_str)
            .unwrap_or("ProvEntity");
        let mut props = entity.attributes.clone();
        if let Some(ref pt) = entity.prov_type {
            props.insert("prov_type".to_string(), Value::String(pt.clone()));
        }
        push_node_upsert(&mut stmts, &mut binds, &mut ni, id.as_str(), label, &props);
    }

    for (id, activity) in normalized.document.activities() {
        let label = activity_labels
            .get(id.as_str())
            .map(String::as_str)
            .unwrap_or("ProvActivity");
        let mut props = activity.attributes.clone();
        if let Some(ref pt) = activity.prov_type {
            props.insert("prov_type".to_string(), Value::String(pt.clone()));
        }
        if let Some(start) = activity.start_time_ms {
            props.insert("prov_startTime".to_string(), Value::from(start));
        }
        if let Some(end) = activity.end_time_ms {
            props.insert("prov_endTime".to_string(), Value::from(end));
        }
        push_node_upsert(&mut stmts, &mut binds, &mut ni, id.as_str(), label, &props);
    }

    for (id, agent) in normalized.document.agents() {
        let label = agent_labels
            .get(id.as_str())
            .map(String::as_str)
            .unwrap_or("ProvAgent");
        let mut props = agent.attributes.clone();
        if let Some(ref pt) = agent.prov_type {
            props.insert("prov_type".to_string(), Value::String(pt.clone()));
        }
        push_node_upsert(&mut stmts, &mut binds, &mut ni, id.as_str(), label, &props);
    }

    for (_, used) in normalized.document.used() {
        let mut edge_props: HashMap<String, Value> = HashMap::new();
        if let Some(ref role) = used.role {
            edge_props.insert("prov_role".to_string(), Value::String(role.clone()));
        }
        let activity_label = activity_labels
            .get(used.activity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvActivity");
        let entity_label = entity_labels
            .get(used.entity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvEntity");
        let rel_type = semantic_used_label(activity_label, used.role.as_deref());
        push_edge_upsert(
            &mut stmts,
            &mut binds,
            &mut ei,
            EdgeUpsertSpec {
                from_id: used.activity.as_str(),
                from_label: activity_label,
                rel_type,
                to_id: used.entity.as_str(),
                to_label: entity_label,
            },
            &edge_props,
        );
    }

    for (_, generated) in normalized.document.was_generated_by() {
        let edge_props: HashMap<String, Value> = HashMap::new();
        let entity_id = generated.entity.id();
        let entity_label = label_for_prov_node_ref(
            &generated.entity,
            &entity_labels,
            &activity_labels,
            &agent_labels,
        );
        let activity_label = activity_labels
            .get(generated.activity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvActivity");
        let rel_type = semantic_generated_by_label(entity_label, activity_label);
        push_edge_upsert(
            &mut stmts,
            &mut binds,
            &mut ei,
            EdgeUpsertSpec {
                from_id: entity_id,
                from_label: entity_label,
                rel_type,
                to_id: generated.activity.as_str(),
                to_label: activity_label,
            },
            &edge_props,
        );
    }

    for (_, generation) in normalized.document.qualified_generation() {
        let edge_props: HashMap<String, Value> = HashMap::new();
        let entity_id = generation.entity.id();
        let entity_label = label_for_prov_node_ref(
            &generation.entity,
            &entity_labels,
            &activity_labels,
            &agent_labels,
        );
        let activity_label = activity_labels
            .get(generation.activity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvActivity");
        push_edge_upsert(
            &mut stmts,
            &mut binds,
            &mut ei,
            EdgeUpsertSpec {
                from_id: entity_id,
                from_label: entity_label,
                rel_type: crate::vocabulary::prov_relations::QUALIFIED_GENERATION,
                to_id: generation.activity.as_str(),
                to_label: activity_label,
            },
            &edge_props,
        );
    }

    for (_, assoc) in normalized.document.was_associated_with() {
        let mut edge_props: HashMap<String, Value> = HashMap::new();
        if let Some(ref role) = assoc.role {
            edge_props.insert("prov_role".to_string(), Value::String(role.clone()));
        }
        let activity_label = activity_labels
            .get(assoc.activity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvActivity");
        let agent_label = agent_labels
            .get(assoc.agent.as_str())
            .map(String::as_str)
            .unwrap_or("ProvAgent");
        let rel_type = semantic_associated_with_label(assoc.role.as_deref());
        push_edge_upsert(
            &mut stmts,
            &mut binds,
            &mut ei,
            EdgeUpsertSpec {
                from_id: assoc.activity.as_str(),
                from_label: activity_label,
                rel_type,
                to_id: assoc.agent.as_str(),
                to_label: agent_label,
            },
            &edge_props,
        );
    }

    for (_, derived) in normalized.document.was_derived_from() {
        let mut edge_props: HashMap<String, Value> = HashMap::new();
        if let Some(ref pt) = derived.prov_type {
            edge_props.insert("prov_type".to_string(), Value::String(pt.clone()));
        }
        let generated_label = entity_labels
            .get(derived.generated_entity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvEntity");
        let used_label = entity_labels
            .get(derived.used_entity.as_str())
            .map(String::as_str)
            .unwrap_or("ProvEntity");
        let rel_type = semantic_derived_from_label(derived.prov_type.as_deref());
        push_edge_upsert(
            &mut stmts,
            &mut binds,
            &mut ei,
            EdgeUpsertSpec {
                from_id: derived.generated_entity.as_str(),
                from_label: generated_label,
                rel_type,
                to_id: derived.used_entity.as_str(),
                to_label: used_label,
            },
            &edge_props,
        );
    }

    for relation in &normalized.derived_relations {
        let mut edge_props: HashMap<String, Value> = HashMap::new();
        for (k, v) in &relation.attributes {
            edge_props.insert(k.clone(), v.clone());
        }
        let (from_label, to_label, rel_type) = match relation.relation {
            A2aRelationType::IntentReplacedBy => (
                GraphNodeLabel::Intent.as_str(),
                GraphNodeLabel::Intent.as_str(),
                semantic_labels::WAS_REPLACED_BY,
            ),
            A2aRelationType::IntentRefinedBy => (
                GraphNodeLabel::Intent.as_str(),
                GraphNodeLabel::Intent.as_str(),
                semantic_labels::WAS_REFINED_BY,
            ),
            A2aRelationType::PlanReplacedBy => (
                GraphNodeLabel::Plan.as_str(),
                GraphNodeLabel::Plan.as_str(),
                semantic_labels::WAS_REPLACED_BY,
            ),
            A2aRelationType::PlanRefinedBy => (
                GraphNodeLabel::Plan.as_str(),
                GraphNodeLabel::Plan.as_str(),
                semantic_labels::WAS_REFINED_BY,
            ),
            A2aRelationType::InformedByToolInvocation => (
                GraphNodeLabel::SessionStep.as_str(),
                GraphNodeLabel::ToolCall.as_str(),
                semantic_labels::WAS_INFORMED_BY,
            ),
            A2aRelationType::CitedSource => {
                let from_label = label_for_prov_node_ref(
                    &relation.from,
                    &entity_labels,
                    &activity_labels,
                    &agent_labels,
                );
                let to_label = label_for_prov_node_ref(
                    &relation.to,
                    &entity_labels,
                    &activity_labels,
                    &agent_labels,
                );
                (from_label, to_label, semantic_labels::CITED)
            }
            A2aRelationType::HasIntent => (
                GraphNodeLabel::Task.as_str(),
                GraphNodeLabel::Intent.as_str(),
                semantic_labels::HAS_INTENT,
            ),
            A2aRelationType::HasPlan => (
                GraphNodeLabel::Task.as_str(),
                GraphNodeLabel::Plan.as_str(),
                semantic_labels::HAS_PLAN,
            ),
            A2aRelationType::CallbackDispatchScheduledFrom => (
                GraphNodeLabel::Task.as_str(),
                GraphNodeLabel::Task.as_str(),
                semantic_labels::WAS_SCHEDULED_FROM,
            ),
            // All remaining derived relations use dynamic label resolution.
            // Every variant is handled — no silent skipping.
            // All remaining derived relations: resolve labels dynamically,
            // use the canonical a2a_relations string as rel_type.
            A2aRelationType::TaskHasMessage
            | A2aRelationType::TaskHasSessionStep
            | A2aRelationType::TaskHasArtifact
            | A2aRelationType::TaskCall
            | A2aRelationType::TaskStatusTransition
            | A2aRelationType::MessageCall
            | A2aRelationType::InformedByObservation
            | A2aRelationType::LifecycleStopInformedByBoot
            | A2aRelationType::HostDispatchTarget => {
                let from_label = label_for_prov_node_ref(
                    &relation.from,
                    &entity_labels,
                    &activity_labels,
                    &agent_labels,
                );
                let to_label = label_for_prov_node_ref(
                    &relation.to,
                    &entity_labels,
                    &activity_labels,
                    &agent_labels,
                );
                (from_label, to_label, relation.relation.as_str())
            }
        };
        push_edge_upsert(
            &mut stmts,
            &mut binds,
            &mut ei,
            EdgeUpsertSpec {
                from_id: relation.from.id(),
                from_label,
                rel_type,
                to_id: relation.to.id(),
                to_label,
            },
            &edge_props,
        );
    }

    // Head-pointer re-points (`WAS_LAST_TRANSITIONED_TO`,
    // `WAS_LAST_EXECUTED_BY`): emit DELETE-then-UPSERT inside the same
    // transaction so the cardinality-one invariant holds without an
    // index backstop.
    for repoint in &normalized.head_pointer_repoints {
        push_head_pointer_repoint(&mut stmts, &mut binds, &mut di, &mut ei, repoint);
    }

    if let Some(ctx_id) = context_id {
        let ctx_node_id = context_entity_id_string(ctx_id);
        let ctx_props: HashMap<String, Value> = HashMap::new();
        push_node_upsert(
            &mut stmts,
            &mut binds,
            &mut ni,
            &ctx_node_id,
            "Context",
            &ctx_props,
        );

        let ctx_id_str = ctx_id;
        for (id, entity) in normalized.document.entities() {
            let label = entity_labels
                .get(id.as_str())
                .map(String::as_str)
                .unwrap_or("ProvEntity");
            push_scoped_to_context_edge(
                &mut stmts,
                &mut binds,
                &mut ei,
                id.as_str(),
                label,
                &ctx_node_id,
            );
            if matches!(label, "Message" | "ToolCall" | "SessionStep") {
                let event_order = entity
                    .attributes
                    .get(a2a::EVENT_ORDER)
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let task_entity_id = entity
                    .attributes
                    .get(a2a::TASK_ID)
                    .and_then(Value::as_str)
                    .map(task_entity_id_string_raw);
                push_transcript_index_upsert(
                    &mut stmts,
                    &mut binds,
                    &mut ti,
                    ctx_id_str,
                    id.as_str(),
                    label,
                    event_order,
                    task_entity_id.as_deref(),
                );
            }
        }
        for (id, activity) in normalized.document.activities() {
            let label = activity_labels
                .get(id.as_str())
                .map(String::as_str)
                .unwrap_or("ProvActivity");
            push_scoped_to_context_edge(
                &mut stmts,
                &mut binds,
                &mut ei,
                id.as_str(),
                label,
                &ctx_node_id,
            );
            if matches!(label, "ToolCall" | "SessionStep") {
                let event_order = activity
                    .attributes
                    .get(a2a::EVENT_ORDER)
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let task_entity_id = activity
                    .attributes
                    .get(a2a::TASK_ID)
                    .and_then(Value::as_str)
                    .map(task_entity_id_string_raw);
                push_transcript_index_upsert(
                    &mut stmts,
                    &mut binds,
                    &mut ti,
                    ctx_id_str,
                    id.as_str(),
                    label,
                    event_order,
                    task_entity_id.as_deref(),
                );
            }
        }
        for (id, _) in normalized.document.agents() {
            let label = agent_labels
                .get(id.as_str())
                .map(String::as_str)
                .unwrap_or("ProvAgent");
            push_scoped_to_context_edge(
                &mut stmts,
                &mut binds,
                &mut ei,
                id.as_str(),
                label,
                &ctx_node_id,
            );
        }
    }

    WriteFragment { stmts, binds }
}

fn build_blob_payload_fragment(
    payloads: &[PayloadRecord],
    blob_bodies: &[(String, String)],
) -> WriteFragment {
    let mut stmts: Vec<String> = Vec::new();
    let mut binds: Vec<TxBind> = Vec::new();
    let mut bi = 0usize;
    for (hash, body) in blob_bodies {
        push_blob_upsert(&mut stmts, &mut binds, &mut bi, hash, body);
    }
    let mut pi = 0usize;
    for p in payloads {
        push_payload_upsert(&mut stmts, &mut binds, &mut pi, p);
    }
    WriteFragment { stmts, binds }
}

/// One or two `BEGIN…COMMIT` transactions: merged when under bind/SQL size limits, else graph then blobs+payloads.
pub(crate) fn build_event_write_plans(
    normalized: &NormalizedProv,
    context_id: Option<&str>,
    payloads: &[PayloadRecord],
    blob_bodies: &[(String, String)],
) -> Vec<EventWritePlan> {
    let graph = build_graph_fragment(normalized, context_id);
    let data = build_blob_payload_fragment(payloads, blob_bodies);
    let (bind_count, sql_bytes) = merged_txn_estimates(&graph, &data);
    if payload_storage::txn_should_split(bind_count, sql_bytes) {
        let mut out = Vec::new();
        let pg = graph.into_event_plan();
        if !pg.sql.is_empty() {
            out.push(pg);
        }
        let pd = data.into_event_plan();
        if !pd.sql.is_empty() {
            out.push(pd);
        }
        out
    } else {
        let plan = WriteFragment::merge(graph, data).into_event_plan();
        if plan.sql.is_empty() {
            vec![]
        } else {
            vec![plan]
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use baml_rt_core::ids::{ContextId, ExternalId, MessageId};
    use serde_json::json;

    use super::*;
    use crate::{
        document::ProvDocument,
        normalizer::{NormalizedProv, normalize_event},
        payload_record::StorageKind,
        types::{Entity, ProvEntityId},
    };

    #[test]
    fn write_plan_wraps_statements_in_transaction() {
        let event = crate::events::ProvEvent::tool_call_started_global(
            ContextId::new(1, 1),
            MessageId::from_external(ExternalId::new("m-wrap")),
            "tool".to_string(),
            None,
            json!({}),
            json!({
                "message_id": "m-wrap",
                "agent_id": "00000000-0000-0000-0000-000000000010",
            }),
            None,
        );
        let normalized = normalize_event(&event).expect("normalize");
        let plans = build_event_write_plans(&normalized, None, &[], &[]);
        assert_eq!(
            plans.len(),
            1,
            "expected single merged txn for typical event"
        );
        let plan = &plans[0];
        assert!(plan.sql.starts_with("BEGIN;\n"), "{}", plan.sql);
        assert!(plan.sql.ends_with(";\nCOMMIT;"), "{}", plan.sql);
        assert!(plan.statement_count > 0);
    }

    #[test]
    fn write_plan_splits_when_bind_threshold_exceeded() {
        let mut doc = ProvDocument::new();
        for i in 0..2000 {
            let id = ProvEntityId::test_only(format!("stress_entity_{i}"));
            doc.insert_entity(
                id,
                Entity {
                    prov_type: None,
                    attributes: HashMap::new(),
                },
            );
        }
        let normalized = NormalizedProv {
            document: doc,
            derived_relations: vec![],
            agent_labels: HashMap::new(),
            head_pointer_repoints: vec![],
        };
        let payloads: Vec<PayloadRecord> = (0..11)
            .map(|i| PayloadRecord {
                payload_id: format!("stress_payload_{i}"),
                activity_anchor_id: "anchor".into(),
                activity_id: Some("activity".into()),
                payload_kind: "tool_result".into(),
                payload_json: "{}".into(),
                content_hash: None,
                storage_kind: StorageKind::Inline,
                file_key: None,
                search_text: "x".into(),
            })
            .collect();
        let plans = build_event_write_plans(&normalized, None, &payloads, &[]);
        assert_eq!(
            plans.len(),
            2,
            "expected graph txn + payload txn when bind budget exceeded"
        );
    }
}
