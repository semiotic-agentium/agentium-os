// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Criterion benchmarks for sequence diagram rendering.
//!
//! Measures simplify_graph + render_sequence_diagram on synthetic graphs.
//! Target: < 1ms for ~20 nodes (coordinator-agent–scale graphs).

use std::collections::HashMap;

use baml_rt_provenance::{
    graph_export::{
        ExportScope, ExportedEdge, ExportedGraph, ExportedNode, enrich::enrich_derived_properties,
        sequence::render_sequence_diagram, simplify::simplify_graph,
    },
    graph_model::{
        EDGE_WAS_EXECUTED_BY, EDGE_WAS_INVOKED_BY, EDGE_WAS_RECEIVED_BY, EDGE_WAS_SPAWNED_BY,
    },
    vocabulary::{a2a, semantic_labels},
};
use criterion::{Bencher, Criterion, black_box, criterion_group, criterion_main};

fn edge(from: &str, rel: &str, to: &str) -> ExportedEdge {
    ExportedEdge {
        from: from.to_string(),
        to: to.to_string(),
        relation: rel.to_string(),
        properties: HashMap::new(),
    }
}

fn msg_node(id: &str, role: &str, content: &str, order: Option<u64>) -> ExportedNode {
    let mut props = HashMap::new();
    props.insert(
        a2a::ROLE.to_string(),
        serde_json::Value::String(role.to_string()),
    );
    props.insert(
        a2a::CONTENT.to_string(),
        serde_json::Value::String(content.to_string()),
    );
    ExportedNode {
        id: id.to_string(),
        label: "Message".to_string(),
        display_name: format!("📩 {role}: {content}"),
        properties: props,
        event_order: order,
    }
}

fn agent_node(id: &str, agent_type: &str) -> ExportedNode {
    let mut props = HashMap::new();
    props.insert(
        a2a::AGENT_TYPE.to_string(),
        serde_json::Value::String(agent_type.to_string()),
    );
    props.insert(
        a2a::ARCHIVE_PATH.to_string(),
        serde_json::Value::String(agent_type.to_string()),
    );
    ExportedNode {
        id: id.to_string(),
        label: "AgentRuntimeInstance".to_string(),
        display_name: format!("🖥️ Agent {agent_type}"),
        properties: props,
        event_order: None,
    }
}

#[expect(dead_code, reason = "benchmark helper retained for future bench cases")]
fn llm_node(id: &str, model: &str, duration_ms: u64, order: Option<u64>) -> ExportedNode {
    let mut props = HashMap::new();
    props.insert(
        a2a::MODEL.to_string(),
        serde_json::Value::String(model.to_string()),
    );
    props.insert(a2a::DURATION_MS.to_string(), serde_json::json!(duration_ms));
    props.insert(
        a2a::ACTIVITY_OUTCOME.to_string(),
        serde_json::json!("Success"),
    );
    props.insert(
        a2a::USAGE_PROMPT_TOKENS.to_string(),
        serde_json::Value::String("389".to_string()),
    );
    props.insert(
        a2a::USAGE_COMPLETION_TOKENS.to_string(),
        serde_json::Value::String("123".to_string()),
    );
    props.insert(
        a2a::USAGE_TOTAL_TOKENS.to_string(),
        serde_json::Value::String("512".to_string()),
    );
    props.insert(
        a2a::FUNCTION_NAME.to_string(),
        serde_json::Value::String("RouteIntent".to_string()),
    );
    ExportedNode {
        id: id.to_string(),
        label: "LlmCall".to_string(),
        display_name: format!("🤖 LLM {model} {duration_ms}ms ✅"),
        properties: props,
        event_order: order,
    }
}

fn mp_node(id: &str) -> ExportedNode {
    ExportedNode {
        id: id.to_string(),
        label: "A2AMessageProcessing".to_string(),
        display_name: "MessageProcessing".to_string(),
        properties: HashMap::new(),
        event_order: None,
    }
}

fn boot_node(id: &str) -> ExportedNode {
    ExportedNode {
        id: id.to_string(),
        label: "AgentBoot".to_string(),
        display_name: "Boot".to_string(),
        properties: HashMap::new(),
        event_order: None,
    }
}

fn archive_node(id: &str, archive_path: &str) -> ExportedNode {
    let mut props = HashMap::new();
    props.insert(
        a2a::ARCHIVE_PATH.to_string(),
        serde_json::Value::String(archive_path.to_string()),
    );
    ExportedNode {
        id: id.to_string(),
        label: "AgentArchive".to_string(),
        display_name: format!("Archive {archive_path}"),
        properties: props,
        event_order: None,
    }
}

fn agent_chain_edges(agent_id: &str, boot_id: &str, archive_id: &str) -> Vec<ExportedEdge> {
    vec![
        edge(agent_id, EDGE_WAS_SPAWNED_BY, boot_id),
        edge(boot_id, semantic_labels::WAS_BOOTSTRAPPED_BY, archive_id),
    ]
}

/// ~20 nodes: 1 user msg, 4 LLM calls, 1 agent (typical coordinator-heavy trace).
fn fixture_20_nodes() -> ExportedGraph {
    let agent = "coordinator_agent_1_0_0";
    let mut edges = vec![
        edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"),
        edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"),
        edge("mp1", EDGE_WAS_INVOKED_BY, "llm1"),
        edge("mp1", EDGE_WAS_INVOKED_BY, "llm2"),
        edge("mp1", EDGE_WAS_INVOKED_BY, "llm3"),
        edge("mp1", EDGE_WAS_INVOKED_BY, "llm4"),
    ];
    edges.extend(agent_chain_edges("a1", "boot1", "arch1"));

    let llm_props = |id: &str, func: &str, order: u64, in_tok: &str, out_tok: &str, total: &str| {
        let mut props = HashMap::new();
        props.insert(
            a2a::MODEL.to_string(),
            serde_json::Value::String("openai/generic".to_string()),
        );
        props.insert(a2a::DURATION_MS.to_string(), serde_json::json!(2000u64));
        props.insert(
            a2a::ACTIVITY_OUTCOME.to_string(),
            serde_json::json!("Success"),
        );
        props.insert(
            a2a::USAGE_PROMPT_TOKENS.to_string(),
            serde_json::Value::String(in_tok.to_string()),
        );
        props.insert(
            a2a::USAGE_COMPLETION_TOKENS.to_string(),
            serde_json::Value::String(out_tok.to_string()),
        );
        props.insert(
            a2a::USAGE_TOTAL_TOKENS.to_string(),
            serde_json::Value::String(total.to_string()),
        );
        props.insert(
            a2a::FUNCTION_NAME.to_string(),
            serde_json::Value::String(func.to_string()),
        );
        ExportedNode {
            id: id.to_string(),
            label: "LlmCall".to_string(),
            display_name: format!("🤖 LLM {func}"),
            properties: props,
            event_order: Some(order),
        }
    };

    let nodes = vec![
        msg_node("m1", "user", "hi", Some(1)),
        agent_node("a1", agent),
        mp_node("mp1"),
        boot_node("boot1"),
        archive_node("arch1", agent),
        llm_props("llm1", "RouteIntent", 2, "389", "123", "512"),
        llm_props("llm2", "GetDiscoverAgentsPlan", 4, "723", "64", "787"),
        llm_props("llm3", "RouteIntent", 6, "391", "210", "601"),
        llm_props("llm4", "GetDiscoverAgentsPlan", 8, "723", "66", "789"),
    ];

    ExportedGraph {
        nodes,
        edges,
        scope: ExportScope::Context("ctx-1".to_string()),
    }
}

/// ~100 nodes: stress test.
fn fixture_100_nodes() -> ExportedGraph {
    let mut nodes: Vec<ExportedNode> = Vec::new();
    let mut edges: Vec<ExportedEdge> = Vec::new();

    nodes.push(msg_node("m1", "user", "hi", Some(1)));
    nodes.push(agent_node("a1", "agent"));
    nodes.push(mp_node("mp1"));
    nodes.push(boot_node("boot1"));
    nodes.push(archive_node("arch1", "agent"));

    edges.push(edge("mp1", EDGE_WAS_RECEIVED_BY, "m1"));
    edges.push(edge("mp1", EDGE_WAS_EXECUTED_BY, "a1"));
    edges.extend(agent_chain_edges("a1", "boot1", "arch1"));

    for i in 0..20 {
        let llm_id = format!("llm{i}");
        let mut props = HashMap::new();
        props.insert(
            a2a::MODEL.to_string(),
            serde_json::Value::String("openai/generic".to_string()),
        );
        props.insert(a2a::DURATION_MS.to_string(), serde_json::json!(1000u64));
        props.insert(
            a2a::ACTIVITY_OUTCOME.to_string(),
            serde_json::json!("Success"),
        );
        props.insert(
            a2a::FUNCTION_NAME.to_string(),
            serde_json::Value::String("Step".to_string()),
        );
        nodes.push(ExportedNode {
            id: llm_id.clone(),
            label: "LlmCall".to_string(),
            display_name: format!("LLM {i}"),
            properties: props,
            event_order: Some(10 + i),
        });
        edges.push(edge("mp1", EDGE_WAS_INVOKED_BY, &llm_id));
    }

    ExportedGraph {
        nodes,
        edges,
        scope: ExportScope::Context("ctx-1".to_string()),
    }
}

fn bench_sequence_render(c: &mut Criterion) {
    let mut g = c.benchmark_group("sequence_render");
    g.sample_size(100);
    g.measurement_time(std::time::Duration::from_secs(5));

    g.bench_function("simplify_20_nodes", |b: &mut Bencher| {
        let graph = fixture_20_nodes();
        b.iter(|| black_box(simplify_graph(black_box(&graph))))
    });

    g.bench_function("render_20_nodes", |b: &mut Bencher| {
        let graph = fixture_20_nodes();
        let simplified = simplify_graph(&graph);
        b.iter(|| black_box(render_sequence_diagram(black_box(&simplified))))
    });

    g.bench_function("simplify_then_render_20_nodes", |b: &mut Bencher| {
        let graph = fixture_20_nodes();
        b.iter(|| {
            let simplified = simplify_graph(&graph);
            black_box(render_sequence_diagram(&simplified))
        })
    });

    g.bench_function("simplify_then_render_100_nodes", |b: &mut Bencher| {
        let graph = fixture_100_nodes();
        b.iter(|| {
            let simplified = simplify_graph(&graph);
            black_box(render_sequence_diagram(&simplified))
        })
    });

    g.bench_function("enrich_simplify_render_20_nodes", |b: &mut Bencher| {
        let base = fixture_20_nodes();
        b.iter(|| {
            let mut graph = base.clone();
            enrich_derived_properties(&mut graph);
            let simplified = simplify_graph(&graph);
            black_box(render_sequence_diagram(&simplified))
        })
    });
}

criterion_group!(benches, bench_sequence_render);
criterion_main!(benches);
