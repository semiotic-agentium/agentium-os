// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! Prompt-scoped ref hydration.
//!
//! An agent-scoped projection hydrates the closure of refs its prompt can reference: `@N` values
//! mentioned by the rows in the window, plus its unconsumed Send receipts (advertised as archived
//! but not visible, and readable by the next hop). Everything else stays out, which is what keeps
//! projection cost bounded by the window instead of by the length of the context.
//!
//! The operator principal is deliberately unbounded: the observation routes render from it.

use std::collections::HashMap;

use baml_rt_core::ids::{AgentId, ContextId, UuidId};
use baml_rt_provenance::{
    AgentType, ProvEvent, ProvenanceReadPrincipal, ProvenanceWriter, SurrealProvenanceStore,
    hydrate_ref_table,
};
use baml_rt_tools::{
    archive_read::{ShortRef, render_to_lines},
    archive_refs::{ArchiveEntry, RefTable},
    citations::{ParsedCitation, ResolvedCitation},
};
use serde_json::json;
use test_support::testing::provenance_fixtures::build_isolated_store;

fn agent_id(seed: &str) -> AgentId {
    AgentId::from_uuid(UuidId::parse_str(seed).expect("valid uuid"))
}

async fn boot(store: &SurrealProvenanceStore, agent: &AgentId, package: &str) {
    store
        .add_event(ProvEvent::agent_booted(
            agent.clone(),
            AgentType::new(package).expect("valid agent type"),
            "1.0.0".to_string(),
            format!("{package}@1.0.0").to_string(),
        ))
        .await
        .expect("boot agent");
}

fn sample_entry(anchor: &str) -> ArchiveEntry {
    ArchiveEntry::new(
        render_to_lines(&json!({"k": "v"})),
        "support/clickup".to_string(),
        None,
        anchor.to_string(),
        "tool_result".to_string(),
    )
}

/// Whether the table can resolve `@<local>` for this agent.
fn resolves_archive(table: &RefTable, local: u32) -> bool {
    let parsed = ParsedCitation::parse(&ShortRef::new_prefixed(1, local).to_string())
        .expect("parse archive citation");
    ResolvedCitation::resolve(&parsed, table).is_some()
}

#[tokio::test]
async fn agent_hydration_is_bounded_to_the_prompt_closure() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9600, 1);
    let agent = agent_id("00000000-0000-0000-0000-0000000000c1");
    boot(&store, &agent, "clickup-agent").await;

    // Three archive bodies: one referenced by the prompt window, one pending Send receipt, one
    // neither (an older, already-consumed result the window does not mention).
    let referenced = store
        .archive_allocate_and_put(&ctx, &agent, "referenced", sample_entry("referenced"))
        .await
        .expect("allocate referenced");
    let pending_entry = sample_entry("pending").with_send_signature(Some(
        "support/clickup\u{0}{\"query\":\"pending\"}".to_string(),
    ));
    let pending = store
        .archive_allocate_and_put(&ctx, &agent, "pending", pending_entry)
        .await
        .expect("allocate pending");
    let unconsumed = store
        .archive_allocate_and_put(&ctx, &agent, "unconsumed", sample_entry("unconsumed"))
        .await
        .expect("allocate unconsumed");

    // A window row that mentions the referenced body, e.g. a PageRead of it.
    let mut known_text = HashMap::new();
    known_text.insert(
        "anchor-window".to_string(),
        format!("cat -n {}", ShortRef::new_prefixed(1, referenced.local)),
    );

    let table = hydrate_ref_table(
        &store,
        &ctx,
        &ProvenanceReadPrincipal::Agent(agent.clone()),
        &known_text,
    )
    .await
    .expect("hydrate agent table");

    assert!(
        resolves_archive(&table, referenced.local),
        "a body the prompt references must resolve"
    );
    assert!(
        resolves_archive(&table, pending.local),
        "a pending Send receipt must resolve: the prompt advertises it as readable"
    );
    assert!(
        !resolves_archive(&table, unconsumed.local),
        "an unreferenced, consumed body must not be hydrated into an agent prompt"
    );

    // Consuming the receipt removes it from the closure: nothing advertises it any more.
    store
        .archive_mark_send_consumed(&ctx, pending, &agent)
        .await
        .expect("mark consumed");
    let after_consume = hydrate_ref_table(
        &store,
        &ctx,
        &ProvenanceReadPrincipal::Agent(agent.clone()),
        &known_text,
    )
    .await
    .expect("re-hydrate agent table");
    assert!(
        resolves_archive(&after_consume, referenced.local),
        "the referenced body still resolves"
    );
    assert!(
        !resolves_archive(&after_consume, pending.local),
        "a consumed receipt is no longer in the closure"
    );

    // The operator path stays complete: observation routes need every body.
    let operator = hydrate_ref_table(
        &store,
        &ctx,
        &ProvenanceReadPrincipal::Operator,
        &HashMap::new(),
    )
    .await
    .expect("hydrate operator table");
    for local in [referenced.local, pending.local, unconsumed.local] {
        assert!(
            resolves_archive(&operator, local),
            "operator hydration must include every body (local {local})"
        );
    }
}

#[tokio::test]
async fn agent_hydration_only_numbers_anchors_in_the_window() {
    let store = build_isolated_store().await;
    let ctx = ContextId::new(9601, 1);
    let agent = agent_id("00000000-0000-0000-0000-0000000000c2");
    boot(&store, &agent, "clickup-agent").await;

    // Two history refs; the prompt window only carries the second.
    let outside = store
        .history_ref_ensure(&ctx, "outside-window", "message")
        .await
        .expect("ensure outside");
    let inside = store
        .history_ref_ensure(&ctx, "inside-window", "message")
        .await
        .expect("ensure inside");

    let mut known_text = HashMap::new();
    known_text.insert("inside-window".to_string(), "visible row".to_string());

    let table = hydrate_ref_table(
        &store,
        &ctx,
        &ProvenanceReadPrincipal::Agent(agent),
        &known_text,
    )
    .await
    .expect("hydrate agent table");

    let resolve = |n: u32| {
        let parsed = ParsedCitation::parse(&format!("#{n}")).expect("parse history ref");
        ResolvedCitation::resolve(&parsed, &table).is_some()
    };
    assert!(resolve(inside), "an in-window anchor resolves");
    assert!(
        !resolve(outside),
        "an out-of-window anchor does not enter the prompt table"
    );

    let operator = hydrate_ref_table(
        &store,
        &ctx,
        &ProvenanceReadPrincipal::Operator,
        &HashMap::new(),
    )
    .await
    .expect("hydrate operator table");
    let parse = ParsedCitation::parse(&format!("#{outside}")).expect("parse");
    assert!(
        ResolvedCitation::resolve(&parse, &operator).is_some(),
        "operator hydration numbers every anchor"
    );
}
