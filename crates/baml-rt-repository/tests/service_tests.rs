//! Tests for RepositoryService: publish → fork → search flow.

use baml_rt_repository::{
    commands::{ForkCommand, PublishCommand, PublishOrigin},
    entry::{ChangeRationale, SourceBundle, Tag},
    ids::{AgentName, Generation, Version},
    lineage::{EdgeDescription, InfluenceRef, Parentage},
    search::{SearchQuery, TagFilter},
};
#[path = "support/common.rs"]
mod common;
use common::setup_service;

fn make_source(content: &str) -> SourceBundle {
    common::make_source(
        content,
        "test-agent",
        &["calculator"],
        "A test agent",
        &["compute"],
    )
}

// -------------------------------------------------------------------------
// Publish flow
// -------------------------------------------------------------------------

#[tokio::test]
async fn publish_original_assigns_v1() {
    let svc = setup_service().await;

    let result = svc
        .publish(PublishCommand {
            name: "weather-agent".parse().unwrap(),
            source: make_source("v1 code"),
            rationale: ChangeRationale::new("initial version").unwrap(),
            origin: PublishOrigin::Original,
        })
        .await
        .unwrap();

    assert_eq!(result.version_ref.version, Version::FIRST);
    assert_eq!(result.generation, Generation::ROOT);
}

#[tokio::test]
async fn publish_iteration_creates_fork_edge() {
    let svc = setup_service().await;

    // Publish v1
    let r1 = svc
        .publish(PublishCommand {
            name: "iter-agent".parse().unwrap(),
            source: make_source("v1 code"),
            rationale: ChangeRationale::new("initial").unwrap(),
            origin: PublishOrigin::Original,
        })
        .await
        .unwrap();
    assert_eq!(r1.version_ref.version, Version::FIRST);

    // Publish v2 as iteration
    let r2 = svc
        .publish(PublishCommand {
            name: "iter-agent".parse().unwrap(),
            source: make_source("v2 code"),
            rationale: ChangeRationale::new("improved accuracy").unwrap(),
            origin: PublishOrigin::Iteration,
        })
        .await
        .unwrap();
    assert_eq!(r2.version_ref.version, Version::new(2).unwrap());

    // The entry should be forked from v1
    let entry = svc.get_by_hash(&r2.hash).await.unwrap().unwrap();
    match &entry.parentage {
        Parentage::Forked { parent, .. } => {
            assert_eq!(parent, &r1.hash);
        }
        other => panic!("Expected Forked parentage, got {other:?}"),
    }
}

#[tokio::test]
async fn publish_influenced_records_influence_edges() {
    let svc = setup_service().await;

    // Publish two source agents
    let s1 = svc
        .publish(PublishCommand {
            name: "source-one".parse().unwrap(),
            source: make_source("source one code"),
            rationale: ChangeRationale::new("initial").unwrap(),
            origin: PublishOrigin::Original,
        })
        .await
        .unwrap();

    let s2 = svc
        .publish(PublishCommand {
            name: "source-two".parse().unwrap(),
            source: make_source("source two code"),
            rationale: ChangeRationale::new("initial").unwrap(),
            origin: PublishOrigin::Original,
        })
        .await
        .unwrap();

    // Publish influenced agent
    let result = svc
        .publish(PublishCommand {
            name: "synthesized-agent".parse().unwrap(),
            source: make_source("synthesized code"),
            rationale: ChangeRationale::new("combined from sources").unwrap(),
            origin: PublishOrigin::Influenced {
                influences: vec![
                    InfluenceRef {
                        source: s1.hash.clone(),
                        description: EdgeDescription::new("prompt patterns from s1").unwrap(),
                    },
                    InfluenceRef {
                        source: s2.hash.clone(),
                        description: EdgeDescription::new("tool usage from s2").unwrap(),
                    },
                ],
            },
        })
        .await
        .unwrap();

    assert_eq!(result.generation, Generation::new(1));
    let entry = svc.get_by_hash(&result.hash).await.unwrap().unwrap();
    match &entry.parentage {
        Parentage::Synthesized { influences } => {
            assert_eq!(influences.len(), 2);
        }
        other => panic!("Expected Synthesized, got {other:?}"),
    }
}

#[tokio::test]
async fn publish_influenced_missing_source_fails() {
    let svc = setup_service().await;
    let fake_hash = format!("{:0>64}", "deadbeef").parse().unwrap();

    let result = svc
        .publish(PublishCommand {
            name: "bad-synth".parse().unwrap(),
            source: make_source("code"),
            rationale: ChangeRationale::new("testing").unwrap(),
            origin: PublishOrigin::Influenced {
                influences: vec![InfluenceRef {
                    source: fake_hash,
                    description: EdgeDescription::new("missing ref").unwrap(),
                }],
            },
        })
        .await;

    assert!(result.is_err());
}

// -------------------------------------------------------------------------
// Fork flow
// -------------------------------------------------------------------------

#[tokio::test]
async fn fork_creates_new_lineage() {
    let svc = setup_service().await;

    let original = svc
        .publish(PublishCommand {
            name: "original-agent".parse().unwrap(),
            source: make_source("original code"),
            rationale: ChangeRationale::new("initial").unwrap(),
            origin: PublishOrigin::Original,
        })
        .await
        .unwrap();

    let forked = svc
        .fork(ForkCommand {
            source_hash: original.hash.clone(),
            new_name: "forked-agent".parse().unwrap(),
            source: make_source("forked code with improvements"),
            rationale: ChangeRationale::new("forking for new use case").unwrap(),
            fork_description: EdgeDescription::new("adapted for production").unwrap(),
            tags: vec![Tag::new("experimental")],
        })
        .await
        .unwrap();

    assert_eq!(forked.version_ref.version, Version::FIRST);
    assert_eq!(forked.version_ref.name.as_str(), "forked-agent");
    assert_eq!(forked.generation, Generation::new(1));

    // Lineage should show the fork relationship
    let lineage = svc.lineage(&forked.hash, 5).await.unwrap();
    assert_eq!(lineage.ancestors.len(), 1);
    assert_eq!(lineage.ancestors[0].hash, original.hash);
}

#[tokio::test]
async fn fork_missing_source_fails() {
    let svc = setup_service().await;
    let fake = format!("{:0>64}", "cafebabe").parse().unwrap();

    let result = svc
        .fork(ForkCommand {
            source_hash: fake,
            new_name: "orphan-fork".parse().unwrap(),
            source: make_source("code"),
            rationale: ChangeRationale::new("testing").unwrap(),
            fork_description: EdgeDescription::new("no parent").unwrap(),
            tags: vec![],
        })
        .await;

    assert!(result.is_err());
}

// -------------------------------------------------------------------------
// Retrieval
// -------------------------------------------------------------------------

#[tokio::test]
async fn retrieval_by_hash_and_version() {
    let svc = setup_service().await;

    let published = svc
        .publish(PublishCommand {
            name: "retrieve-me".parse().unwrap(),
            source: make_source("retrieval test"),
            rationale: ChangeRationale::new("testing retrieval").unwrap(),
            origin: PublishOrigin::Original,
        })
        .await
        .unwrap();

    let by_hash = svc.get_by_hash(&published.hash).await.unwrap().unwrap();
    assert_eq!(by_hash.version_ref.name.as_str(), "retrieve-me");

    let name: AgentName = "retrieve-me".parse().unwrap();
    let by_version = svc
        .get_by_version(&name, Version::FIRST)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_version.hash, published.hash);

    let latest = svc.get_latest(&name).await.unwrap().unwrap();
    assert_eq!(latest.hash, published.hash);
}

// -------------------------------------------------------------------------
// Search flow
// -------------------------------------------------------------------------

#[tokio::test]
async fn search_finds_published_agents() {
    let svc = setup_service().await;

    svc.publish(PublishCommand {
        name: "searchable-one".parse().unwrap(),
        source: common::make_source(
            "first agent",
            "searchable-one",
            &["calculator"],
            "First searchable agent",
            &["stable"],
        ),
        rationale: ChangeRationale::new("init").unwrap(),
        origin: PublishOrigin::Original,
    })
    .await
    .unwrap();

    svc.publish(PublishCommand {
        name: "searchable-two".parse().unwrap(),
        source: common::make_source(
            "second agent",
            "searchable-two",
            &["calculator"],
            "Second searchable agent",
            &["experimental"],
        ),
        rationale: ChangeRationale::new("init").unwrap(),
        origin: PublishOrigin::Original,
    })
    .await
    .unwrap();

    // Search by tag
    let results = svc
        .search(&SearchQuery {
            tags: vec![TagFilter::new("stable")],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].version_ref.name.as_str(), "searchable-one");

    // Empty search returns all
    let all = svc.search(&SearchQuery::default()).await.unwrap();
    assert_eq!(all.len(), 2);
}

// -------------------------------------------------------------------------
// Blob operations
// -------------------------------------------------------------------------

#[tokio::test]
async fn blob_put_and_get() {
    let svc = setup_service().await;

    let published = svc
        .publish(PublishCommand {
            name: "blob-agent".parse().unwrap(),
            source: make_source("blob test"),
            rationale: ChangeRationale::new("init").unwrap(),
            origin: PublishOrigin::Original,
        })
        .await
        .unwrap();

    let blob_data = b"fake tar.gz content";
    svc.put_blob(&published.hash, blob_data).await.unwrap();

    let loaded = svc.get_blob(&published.hash).await.unwrap().unwrap();
    assert_eq!(loaded, blob_data);
}

// -------------------------------------------------------------------------
// Agent listing
// -------------------------------------------------------------------------

#[tokio::test]
async fn list_agents_and_versions() {
    let svc = setup_service().await;

    svc.publish(PublishCommand {
        name: "listed-a".parse().unwrap(),
        source: make_source("a v1"),
        rationale: ChangeRationale::new("init").unwrap(),
        origin: PublishOrigin::Original,
    })
    .await
    .unwrap();

    svc.publish(PublishCommand {
        name: "listed-a".parse().unwrap(),
        source: make_source("a v2"),
        rationale: ChangeRationale::new("update").unwrap(),
        origin: PublishOrigin::Iteration,
    })
    .await
    .unwrap();

    svc.publish(PublishCommand {
        name: "listed-b".parse().unwrap(),
        source: make_source("b v1"),
        rationale: ChangeRationale::new("init").unwrap(),
        origin: PublishOrigin::Original,
    })
    .await
    .unwrap();

    let agents = svc.list_agents().await.unwrap();
    assert_eq!(agents.len(), 2);

    let name: AgentName = "listed-a".parse().unwrap();
    let versions = svc.list_versions(&name).await.unwrap();
    assert_eq!(versions.len(), 2);
}
