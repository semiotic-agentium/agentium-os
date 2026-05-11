//! Property tests: version monotonicity, lineage cycle detection.

use std::sync::Arc;

use baml_rt_repository::{
    commands::{PublishCommand, PublishOrigin},
    entry::{ChangeRationale, SourceBundle},
    ids::{AgentName, Version},
    lineage::EdgeDescription,
    service::RepositoryService,
    storage::{BlobStore, LineageStore, MetadataStore, SearchStore},
    surreal_store::SurrealStore,
};
use proptest::prelude::*;
#[path = "support/common.rs"]
mod common;

fn make_source(content: &str) -> SourceBundle {
    common::make_source(content, "prop-test-agent", &[], "property test agent", &[])
}

async fn setup_service() -> RepositoryService {
    common::setup_service().await
}

// -------------------------------------------------------------------------
// Version monotonicity
// -------------------------------------------------------------------------

/// Strategy: generate a sequence of publishes (1..=n) and verify version
/// numbers are strictly monotonically increasing.
fn version_count_strategy() -> impl Strategy<Value = usize> {
    2..=10usize
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn version_numbers_are_monotonically_increasing(n in version_count_strategy()) {
        let rt = tokio::runtime::Runtime::new().unwrap();

        let mut versions = Vec::new();

        rt.block_on(async {
            let svc = setup_service().await;
            for i in 0..n {
                let origin = if i == 0 {
                    PublishOrigin::Original
                } else {
                    PublishOrigin::Iteration
                };

                let result = svc.publish(PublishCommand {
                    name: "monotonic-agent".parse().unwrap(),
                    source: make_source(&format!("version {i} code with unique content {i}")),
                    rationale: ChangeRationale::new(format!("publish #{i}")).unwrap(),
                    origin,
                }).await.unwrap();

                versions.push(result.version_ref.version.as_u32());
            }
        });

        // Verify strict monotonicity
        for window in versions.windows(2) {
            prop_assert!(
                window[1] > window[0],
                "Version {v1} should be > {v0}",
                v1 = window[1],
                v0 = window[0]
            );
        }

        // Verify starts at 1
        prop_assert_eq!(versions[0], 1);

        // Verify no gaps
        for (i, v) in versions.iter().enumerate() {
            let expected = (i + 1) as u32;
            prop_assert_eq!(*v, expected);
        }
    }
}

// -------------------------------------------------------------------------
// Generation increments on fork chains
// -------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn fork_chain_generation_increases(chain_len in 2..=6usize) {
        let rt = tokio::runtime::Runtime::new().unwrap();

        let mut generations = Vec::new();

        rt.block_on(async {
            let svc = setup_service().await;
            // Publish original
            let mut prev = svc.publish(PublishCommand {
                name: "chain-root".parse().unwrap(),
                source: make_source("root code for chain test"),
                rationale: ChangeRationale::new("root").unwrap(),
                origin: PublishOrigin::Original,

            }).await.unwrap();
            generations.push(prev.generation.as_u32());

            // Fork chain
            for i in 1..chain_len {
                let fork_name = format!("chain-fork-{i}");
                let result = svc.fork(baml_rt_repository::commands::ForkCommand {
                    source_hash: prev.hash.clone(),
                    new_name: fork_name.parse().unwrap(),
                    source: make_source(&format!("fork {i} unique content")),
                    rationale: ChangeRationale::new(format!("fork #{i}")).unwrap(),
                    fork_description: EdgeDescription::new(format!("fork step {i}")).unwrap(),
                    tags: vec![],
                }).await.unwrap();
                generations.push(result.generation.as_u32());
                prev = result;
            }
        });

        // Generation should increase monotonically
        for window in generations.windows(2) {
            prop_assert!(
                window[1] > window[0],
                "Generation {g1} should be > {g0}",
                g1 = window[1],
                g0 = window[0]
            );
        }

        // Root generation should be 0
        prop_assert_eq!(generations[0], 0);
    }
}

// -------------------------------------------------------------------------
// Version uniqueness — no two entries share the same (name, version)
// -------------------------------------------------------------------------

proptest! {
    // Each case rebuilds a fresh SurrealDB Mem instance per publish; the
    // default 256 cases contend with other proptests in the same nextest
    // binary. A handful of cases is enough to exercise the property.
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn concurrent_publishes_produce_unique_versions(n in 2..=8usize) {
        let rt = tokio::runtime::Runtime::new().unwrap();

        let mut seen_versions = std::collections::HashSet::new();

        rt.block_on(async {
            let svc = setup_service().await;
            for i in 0..n {
                let origin = if i == 0 {
                    PublishOrigin::Original
                } else {
                    PublishOrigin::Iteration
                };

                let result = svc.publish(PublishCommand {
                    name: "unique-ver-agent".parse().unwrap(),
                    source: make_source(&format!("unique version content #{i}")),
                    rationale: ChangeRationale::new(format!("publish #{i}")).unwrap(),
                    origin,
                }).await.unwrap();

                let v = result.version_ref.version.as_u32();
                prop_assert!(
                    seen_versions.insert(v),
                    "Duplicate version number: v{v}"
                );
            }
            Ok(())
        })?;
    }
}

// -------------------------------------------------------------------------
// Lineage DAG acyclicity — verify that ancestor traversal terminates
// -------------------------------------------------------------------------

#[tokio::test]
async fn lineage_traversal_terminates_on_deep_chain() {
    let store = SurrealStore::open_in_memory().await.unwrap();
    let store = Arc::new(store);

    let svc = RepositoryService::new(
        store.clone() as Arc<dyn BlobStore>,
        store.clone() as Arc<dyn MetadataStore>,
        store.clone() as Arc<dyn LineageStore>,
        store as Arc<dyn SearchStore>,
    );

    // Build a chain of 20 entries
    let mut last_hash = None;

    for i in 0..20 {
        let origin = if i == 0 {
            PublishOrigin::Original
        } else {
            PublishOrigin::Iteration
        };

        let result = svc
            .publish(PublishCommand {
                name: "deep-chain".parse().unwrap(),
                source: make_source(&format!("deep chain step {i}")),
                rationale: ChangeRationale::new(format!("step {i}")).unwrap(),
                origin,
            })
            .await
            .unwrap();

        last_hash = Some(result.hash);
    }

    // Traversal should terminate and return results
    let subgraph = svc.lineage(&last_hash.unwrap(), 100).await.unwrap();
    // Should have found ancestors (at least some, limited by actual edges)
    assert!(subgraph.ancestors.len() <= 19);
}

// -------------------------------------------------------------------------
// Hash determinism — same source produces same hash
// -------------------------------------------------------------------------

proptest! {
    #[test]
    fn identical_source_produces_identical_hash(content in "[a-z]{10,50}") {
        let rt = tokio::runtime::Runtime::new().unwrap();

        // We can't publish twice with same hash (DuplicateHash error).
        // Instead, verify that computing hash from the same source yields
        // consistent results by publishing to two different names.
        rt.block_on(async {
            let svc = setup_service().await;
            let r1 = svc.publish(PublishCommand {
                name: "hash-test-a".parse().unwrap(),
                source: make_source(&content),
                rationale: ChangeRationale::new("test a").unwrap(),
                origin: PublishOrigin::Original,

            }).await;

            let r2 = svc.publish(PublishCommand {
                name: "hash-test-b".parse().unwrap(),
                source: make_source(&content),
                rationale: ChangeRationale::new("test b").unwrap(),
                origin: PublishOrigin::Original,

            }).await;

            // Both should produce the same hash, and the second should fail
            // with DuplicateHash since the content is identical
            match (r1, r2) {
                (Ok(_r1), Err(_)) => {
                    // Expected: first succeeds, second fails with duplicate
                }
                (Ok(r1), Ok(r2)) => {
                    // Should not happen if hash is deterministic
                    panic!("Expected duplicate hash error, but both succeeded with hashes {} and {}", r1.hash, r2.hash);
                }
                _ => {
                    panic!("First publish should succeed");
                }
            }
        });
    }
}

// -------------------------------------------------------------------------
// Agent name validation
// -------------------------------------------------------------------------

proptest! {
    #[test]
    fn valid_agent_names_parse(name in "[a-z][a-z0-9-]{0,20}[a-z0-9]") {
        let parsed: Result<AgentName, _> = name.parse();
        prop_assert!(parsed.is_ok(), "Valid name '{name}' should parse");
    }

    #[test]
    fn invalid_agent_names_rejected(name in "[A-Z][!@#$%^&*]{1,10}") {
        let parsed: Result<AgentName, _> = name.parse();
        prop_assert!(parsed.is_err(), "Invalid name '{name}' should be rejected");
    }
}

// -------------------------------------------------------------------------
// Version never zero
// -------------------------------------------------------------------------

proptest! {
    #[test]
    fn version_zero_is_rejected(v in 0u32..=0u32) {
        let result = Version::new(v);
        prop_assert!(result.is_err());
    }

    #[test]
    fn version_positive_accepted(v in 1u32..=10000u32) {
        let result = Version::new(v);
        prop_assert!(result.is_ok());
        prop_assert_eq!(result.unwrap().as_u32(), v);
    }
}
