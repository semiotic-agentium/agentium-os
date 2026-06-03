// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Durable `#N` history refs survive store rebuild (graph-backed ref cutover).

use baml_rt_core::ids::{ContextId, ExternalId, MessageId};
use baml_rt_provenance::{ProvEvent, ProvenanceContextReader, ProvenanceWriter, hydrate_ref_table};
use baml_rt_tools::{
    archive_read::HistoryRef,
    citations::{CitationKind, ParsedCitation, ResolvedCitation},
};
use test_support::testing::provenance_fixtures::build_isolated_store;

#[tokio::test]
async fn history_ref_registry_survives_hydrate_after_drop_cache() {
    let store = build_isolated_store().await;
    let context_id = ContextId::new(99, 1);
    let msg_id = MessageId::from_external(ExternalId::new("hist-msg-1"));

    store
        .add_event(ProvEvent::message_received_global(
            context_id.clone(),
            msg_id,
            "user".into(),
            vec!["hello durable refs".into()],
            None,
            baml_rt_core::ids::AgentId::from_uuid(
                baml_rt_core::ids::UuidId::parse_str("00000000-0000-0000-0000-000000000099")
                    .unwrap(),
            ),
            1_700_000_001_000,
        ))
        .await
        .expect("write message");

    let n1 = store
        .history_ref_ensure(
            &context_id,
            store
                .conversation_context(&context_id, None)
                .await
                .expect("ctx")
                .first()
                .expect("one message")
                .activity_anchor
                .as_str(),
            "message",
        )
        .await
        .expect("ensure");

    let table_a = hydrate_ref_table(&store, &context_id)
        .await
        .expect("hydrate a");
    let parsed = ParsedCitation::parse(&format!("#{n1}")).expect("parse");
    let resolved_a = ResolvedCitation::resolve(&parsed, &table_a).expect("resolve a");
    assert!(matches!(resolved_a.kind, CitationKind::History));
    assert_eq!(resolved_a.n, n1);

    let table_b = hydrate_ref_table(&store, &context_id)
        .await
        .expect("hydrate b");
    let resolved_b = ResolvedCitation::resolve(&parsed, &table_b).expect("resolve b");
    assert_eq!(resolved_b.n, n1);

    let n2 = store
        .history_ref_ensure(
            &context_id,
            table_a
                .get_history(HistoryRef::new(n1))
                .expect("entry")
                .activity_anchor
                .as_str(),
            "message",
        )
        .await
        .expect("idempotent");
    assert_eq!(n1, n2);
}
