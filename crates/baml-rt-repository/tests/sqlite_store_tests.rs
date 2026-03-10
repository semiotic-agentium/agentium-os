//! Tests for SqliteStore: metadata CRUD, lineage traversal, search filters.

use baml_rt_repository::{
    entry::{
        ChangeRationale, FitnessDomain, ManifestSource, RepositoryEntry, SourceBundle,
        SourceContent, SourceFile, SourcePath, Tag, Timestamp,
    },
    ids::{AgentName, Generation, LineageEdgeId, Version, VersionRef},
    lineage::{EdgeDescription, LineageEdge, LineageKind, Parentage},
    search::{GenerationFilter, SearchQuery, TagFilter, ToolFilter},
    sqlite_store::SqliteStore,
    storage::{LineageStore, MetadataStore, SearchStore},
};

fn test_manifest() -> ManifestSource {
    ManifestSource::new(serde_json::json!({
        "name": "test-agent",
        "version": "1.0.0",
        "tools": ["calculator", "weather"],
        "discovery": {
            "description": "A test agent for weather planning",
            "capabilities": ["weather-lookup", "math"]
        }
    }))
}

fn test_source_bundle() -> SourceBundle {
    SourceBundle {
        manifest: test_manifest(),
        ts_sources: vec![SourceFile {
            path: SourcePath::new("src/index.ts").unwrap(),
            content: SourceContent::new("export function run() { return 'hello'; }"),
        }],
        baml_sources: vec![SourceFile {
            path: SourcePath::new("baml_src/main.baml").unwrap(),
            content: SourceContent::new("function GetWeather { input: string, output: string }"),
        }],
    }
}

fn test_entry(hash_suffix: &str, name: &str, version: u32) -> RepositoryEntry {
    let hash_str = format!("{:0>64}", hash_suffix);
    RepositoryEntry {
        hash: hash_str.parse().unwrap(),
        version_ref: VersionRef {
            name: name.parse().unwrap(),
            version: Version::new(version).unwrap(),
        },
        source: test_source_bundle(),
        parentage: Parentage::Original,
        generation: Generation::ROOT,
        change_rationale: ChangeRationale::new("initial creation").unwrap(),
        created_at: Timestamp::new("2026-01-01T00:00:00Z"),
        fitness_scores: vec![],
        tags: vec![],
    }
}

async fn setup_store() -> SqliteStore {
    let store = SqliteStore::open_in_memory().unwrap();
    store.init_schema().await.unwrap();
    store
}

// -------------------------------------------------------------------------
// MetadataStore CRUD
// -------------------------------------------------------------------------

#[tokio::test]
async fn insert_and_get_by_hash() {
    let store = setup_store().await;
    let entry = test_entry("aabb01", "weather-agent", 1);
    store.insert_entry(&entry).await.unwrap();

    let loaded = store.get_by_hash(&entry.hash).await.unwrap();
    assert!(loaded.is_some());
    let loaded = loaded.unwrap();
    assert_eq!(loaded.hash, entry.hash);
    assert_eq!(loaded.version_ref, entry.version_ref);
    assert_eq!(loaded.generation, Generation::ROOT);
}

#[tokio::test]
async fn insert_and_get_by_version() {
    let store = setup_store().await;
    let entry = test_entry("aabb02", "planner-agent", 1);
    store.insert_entry(&entry).await.unwrap();

    let name: AgentName = "planner-agent".parse().unwrap();
    let loaded = store.get_by_version(&name, Version::FIRST).await.unwrap();
    assert!(loaded.is_some());
    assert_eq!(loaded.unwrap().hash, entry.hash);
}

#[tokio::test]
async fn get_latest_returns_highest_version() {
    let store = setup_store().await;
    let e1 = test_entry("cc0001", "multi-ver", 1);
    store.insert_entry(&e1).await.unwrap();

    let mut e2 = test_entry("cc0002", "multi-ver", 2);
    e2.created_at = Timestamp::new("2026-01-02T00:00:00Z");
    store.insert_entry(&e2).await.unwrap();

    let name: AgentName = "multi-ver".parse().unwrap();
    let latest = store.get_latest(&name).await.unwrap().unwrap();
    assert_eq!(latest.version_ref.version, Version::new(2).unwrap());
}

#[tokio::test]
async fn resolve_hash_returns_correct_hash() {
    let store = setup_store().await;
    let entry = test_entry("dd0001", "resolver-agent", 1);
    store.insert_entry(&entry).await.unwrap();

    let vref = VersionRef {
        name: "resolver-agent".parse().unwrap(),
        version: Version::FIRST,
    };
    let hash = store.resolve_hash(&vref).await.unwrap().unwrap();
    assert_eq!(hash, entry.hash);
}

#[tokio::test]
async fn next_version_starts_at_one() {
    let store = setup_store().await;
    let name: AgentName = "brand-new".parse().unwrap();
    let v = store.next_version(&name).await.unwrap();
    assert_eq!(v, Version::FIRST);
}

#[tokio::test]
async fn next_version_increments() {
    let store = setup_store().await;
    let entry = test_entry("ee0001", "incrementor", 1);
    store.insert_entry(&entry).await.unwrap();

    let name: AgentName = "incrementor".parse().unwrap();
    let v = store.next_version(&name).await.unwrap();
    assert_eq!(v, Version::new(2).unwrap());
}

#[tokio::test]
async fn list_versions_returns_all() {
    let store = setup_store().await;
    let e1 = test_entry("ff0001", "listed-agent", 1);
    let e2 = test_entry("ff0002", "listed-agent", 2);
    store.insert_entry(&e1).await.unwrap();
    store.insert_entry(&e2).await.unwrap();

    let name: AgentName = "listed-agent".parse().unwrap();
    let versions = store.list_versions(&name).await.unwrap();
    assert_eq!(versions.len(), 2);
    // Should be desc order
    assert_eq!(versions[0].version_ref.version, Version::new(2).unwrap());
    assert_eq!(versions[1].version_ref.version, Version::FIRST);
}

#[tokio::test]
async fn list_agents_returns_distinct_names() {
    let store = setup_store().await;
    store
        .insert_entry(&test_entry("110001", "alpha-agent", 1))
        .await
        .unwrap();
    store
        .insert_entry(&test_entry("110002", "alpha-agent", 2))
        .await
        .unwrap();
    store
        .insert_entry(&test_entry("110003", "beta-agent", 1))
        .await
        .unwrap();

    let agents = store.list_agents().await.unwrap();
    assert_eq!(agents.len(), 2);
}

#[tokio::test]
async fn duplicate_hash_is_rejected() {
    let store = setup_store().await;
    let entry = test_entry("220001", "dup-agent", 1);
    store.insert_entry(&entry).await.unwrap();

    // Same hash, different name/version should fail
    let mut dup = entry.clone();
    dup.version_ref = VersionRef {
        name: "other-agent".parse().unwrap(),
        version: Version::FIRST,
    };
    let result = store.insert_entry(&dup).await;
    assert!(result.is_err());
}

// -------------------------------------------------------------------------
// Fitness scores & tags
// -------------------------------------------------------------------------

#[tokio::test]
async fn record_and_load_fitness() {
    let store = setup_store().await;
    let entry = test_entry("330001", "fit-agent", 1);
    store.insert_entry(&entry).await.unwrap();

    store
        .record_fitness(
            &entry.hash,
            FitnessDomain::new("accuracy"),
            0.95,
            Timestamp::new("2026-03-01T00:00:00Z"),
        )
        .await
        .unwrap();

    let loaded = store.get_by_hash(&entry.hash).await.unwrap().unwrap();
    assert_eq!(loaded.fitness_scores.len(), 1);
    assert_eq!(loaded.fitness_scores[0].score, 0.95);
}

#[tokio::test]
async fn record_fitness_on_missing_entry_fails() {
    let store = setup_store().await;
    let fake_hash = format!("{:0>64}", "deadbeef").parse().unwrap();
    let result = store
        .record_fitness(
            &fake_hash,
            FitnessDomain::new("accuracy"),
            0.5,
            Timestamp::new("2026-03-01T00:00:00Z"),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn add_and_remove_tags() {
    let store = setup_store().await;
    let entry = test_entry("440001", "tag-agent", 1);
    store.insert_entry(&entry).await.unwrap();

    store
        .add_tag(&entry.hash, Tag::new("stable"))
        .await
        .unwrap();
    store
        .add_tag(&entry.hash, Tag::new("production"))
        .await
        .unwrap();

    let loaded = store.get_by_hash(&entry.hash).await.unwrap().unwrap();
    assert_eq!(loaded.tags.len(), 2);

    store
        .remove_tag(&entry.hash, &Tag::new("stable"))
        .await
        .unwrap();
    let loaded = store.get_by_hash(&entry.hash).await.unwrap().unwrap();
    assert_eq!(loaded.tags.len(), 1);
    assert_eq!(loaded.tags[0].as_str(), "production");
}

#[tokio::test]
async fn add_tag_idempotent() {
    let store = setup_store().await;
    let entry = test_entry("440002", "idem-agent", 1);
    store.insert_entry(&entry).await.unwrap();

    store
        .add_tag(&entry.hash, Tag::new("stable"))
        .await
        .unwrap();
    store
        .add_tag(&entry.hash, Tag::new("stable"))
        .await
        .unwrap();

    let loaded = store.get_by_hash(&entry.hash).await.unwrap().unwrap();
    assert_eq!(loaded.tags.len(), 1);
}

// -------------------------------------------------------------------------
// LineageStore: edges, parents, children, ancestors
// -------------------------------------------------------------------------

#[tokio::test]
async fn record_edges_and_query_parents() {
    let store = setup_store().await;
    let parent = test_entry("550001", "parent-agent", 1);
    let mut child = test_entry("550002", "parent-agent", 2);
    child.parentage = Parentage::Forked {
        parent: parent.hash.clone(),
        description: EdgeDescription::new("iterated").unwrap(),
    };
    child.generation = Generation::new(1);

    store.insert_entry(&parent).await.unwrap();
    store.insert_entry(&child).await.unwrap();

    let edge = LineageEdge {
        id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
        source: parent.hash.clone(),
        target: child.hash.clone(),
        kind: LineageKind::Fork,
        description: EdgeDescription::new("iterated from v1").unwrap(),
    };
    store.record_edges(&[edge]).await.unwrap();

    let parents = store.parents(&child.hash).await.unwrap();
    assert_eq!(parents.len(), 1);
    assert_eq!(parents[0].hash, parent.hash);
}

#[tokio::test]
async fn query_children() {
    let store = setup_store().await;
    let parent = test_entry("660001", "root-agent", 1);
    let child = test_entry("660002", "root-agent", 2);

    store.insert_entry(&parent).await.unwrap();
    store.insert_entry(&child).await.unwrap();

    let edge = LineageEdge {
        id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
        source: parent.hash.clone(),
        target: child.hash.clone(),
        kind: LineageKind::Fork,
        description: EdgeDescription::new("iteration").unwrap(),
    };
    store.record_edges(&[edge]).await.unwrap();

    let children = store.children(&parent.hash).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].hash, child.hash);
}

#[tokio::test]
async fn ancestors_recursive_cte() {
    let store = setup_store().await;

    let grandparent = test_entry("770001", "lineage-agent", 1);
    let parent = test_entry("770002", "lineage-agent", 2);
    let child = test_entry("770003", "lineage-agent", 3);

    store.insert_entry(&grandparent).await.unwrap();
    store.insert_entry(&parent).await.unwrap();
    store.insert_entry(&child).await.unwrap();

    let edges = vec![
        LineageEdge {
            id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
            source: grandparent.hash.clone(),
            target: parent.hash.clone(),
            kind: LineageKind::Fork,
            description: EdgeDescription::new("v1 to v2").unwrap(),
        },
        LineageEdge {
            id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
            source: parent.hash.clone(),
            target: child.hash.clone(),
            kind: LineageKind::Fork,
            description: EdgeDescription::new("v2 to v3").unwrap(),
        },
    ];
    store.record_edges(&edges).await.unwrap();

    let ancestors = store.ancestors(&child.hash, 10).await.unwrap();
    assert_eq!(ancestors.len(), 2);
    // Should include both grandparent and parent
    let hashes: Vec<_> = ancestors.iter().map(|a| a.hash.clone()).collect();
    assert!(hashes.contains(&grandparent.hash));
    assert!(hashes.contains(&parent.hash));
}

#[tokio::test]
async fn ancestors_respects_depth_limit() {
    let store = setup_store().await;

    let g = test_entry("880001", "depth-agent", 1);
    let p = test_entry("880002", "depth-agent", 2);
    let c = test_entry("880003", "depth-agent", 3);

    store.insert_entry(&g).await.unwrap();
    store.insert_entry(&p).await.unwrap();
    store.insert_entry(&c).await.unwrap();

    let edges = vec![
        LineageEdge {
            id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
            source: g.hash.clone(),
            target: p.hash.clone(),
            kind: LineageKind::Fork,
            description: EdgeDescription::new("edge 1").unwrap(),
        },
        LineageEdge {
            id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
            source: p.hash.clone(),
            target: c.hash.clone(),
            kind: LineageKind::Fork,
            description: EdgeDescription::new("edge 2").unwrap(),
        },
    ];
    store.record_edges(&edges).await.unwrap();

    // Depth 1: only direct parent
    let ancestors = store.ancestors(&c.hash, 1).await.unwrap();
    assert_eq!(ancestors.len(), 1);
    assert_eq!(ancestors[0].hash, p.hash);
}

#[tokio::test]
async fn subgraph_includes_ancestors_and_descendants() {
    let store = setup_store().await;

    let parent = test_entry("990001", "sub-agent", 1);
    let focal = test_entry("990002", "sub-agent", 2);
    let child = test_entry("990003", "sub-agent", 3);

    store.insert_entry(&parent).await.unwrap();
    store.insert_entry(&focal).await.unwrap();
    store.insert_entry(&child).await.unwrap();

    let edges = vec![
        LineageEdge {
            id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
            source: parent.hash.clone(),
            target: focal.hash.clone(),
            kind: LineageKind::Fork,
            description: EdgeDescription::new("p to f").unwrap(),
        },
        LineageEdge {
            id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
            source: focal.hash.clone(),
            target: child.hash.clone(),
            kind: LineageKind::Fork,
            description: EdgeDescription::new("f to c").unwrap(),
        },
    ];
    store.record_edges(&edges).await.unwrap();

    let subgraph = store.subgraph(&focal.hash, 5).await.unwrap();
    assert_eq!(subgraph.root, focal.hash);
    assert_eq!(subgraph.ancestors.len(), 1);
    assert_eq!(subgraph.descendants.len(), 1);
    assert!(!subgraph.edges.is_empty());
}

#[tokio::test]
async fn influenced_by_returns_influence_children() {
    let store = setup_store().await;

    let source = test_entry("aa0001", "influence-src", 1);
    let influenced = test_entry("aa0002", "influenced-agent", 1);

    store.insert_entry(&source).await.unwrap();
    store.insert_entry(&influenced).await.unwrap();

    let edge = LineageEdge {
        id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
        source: source.hash.clone(),
        target: influenced.hash.clone(),
        kind: LineageKind::Influence,
        description: EdgeDescription::new("inspired by source").unwrap(),
    };
    store.record_edges(&[edge]).await.unwrap();

    let result = store.influenced_by(&source.hash).await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].hash, influenced.hash);
}

// -------------------------------------------------------------------------
// SearchStore: filters
// -------------------------------------------------------------------------

#[tokio::test]
async fn search_empty_query_returns_all() {
    let store = setup_store().await;
    store
        .insert_entry(&test_entry("bb0001", "search-alpha", 1))
        .await
        .unwrap();
    store
        .insert_entry(&test_entry("bb0002", "search-beta", 1))
        .await
        .unwrap();

    let results = store.search(&SearchQuery::default()).await.unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn search_by_agent_name() {
    let store = setup_store().await;
    store
        .insert_entry(&test_entry("bb0003", "target-agent", 1))
        .await
        .unwrap();
    store
        .insert_entry(&test_entry("bb0004", "other-agent", 1))
        .await
        .unwrap();

    let query = SearchQuery {
        name: Some("target-agent".parse().unwrap()),
        ..Default::default()
    };
    let results = store.search(&query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].version_ref.name.as_str(), "target-agent");
}

#[tokio::test]
async fn search_by_tag() {
    let store = setup_store().await;
    let mut entry = test_entry("bb0005", "tagged-agent", 1);
    entry.tags = vec![Tag::new("production"), Tag::new("stable")];
    store.insert_entry(&entry).await.unwrap();

    let other = test_entry("bb0006", "untagged-agent", 1);
    store.insert_entry(&other).await.unwrap();

    let query = SearchQuery {
        tags: vec![TagFilter::new("production")],
        ..Default::default()
    };
    let results = store.search(&query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].version_ref.name.as_str(), "tagged-agent");
}

#[tokio::test]
async fn search_by_tool_filter() {
    let store = setup_store().await;
    store
        .insert_entry(&test_entry("bb0007", "tool-agent", 1))
        .await
        .unwrap();

    let query = SearchQuery {
        tools: vec![ToolFilter::new("calculator")],
        ..Default::default()
    };
    let results = store.search(&query).await.unwrap();
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn search_by_generation_range() {
    let store = setup_store().await;

    let mut e0 = test_entry("bb0008", "gen-zero", 1);
    e0.generation = Generation::ROOT;
    store.insert_entry(&e0).await.unwrap();

    let mut e1 = test_entry("bb0009", "gen-one", 1);
    e1.generation = Generation::new(1);
    store.insert_entry(&e1).await.unwrap();

    let query = SearchQuery {
        generation: Some(GenerationFilter {
            min: Some(Generation::new(1)),
            max: None,
        }),
        ..Default::default()
    };
    let results = store.search(&query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].version_ref.name.as_str(), "gen-one");
}

#[tokio::test]
async fn search_with_limit() {
    let store = setup_store().await;
    for i in 1..=5 {
        let hash = format!("cc{i:04}");
        let name = format!("limit-agent-{i}");
        store
            .insert_entry(&test_entry(&hash, &name, 1))
            .await
            .unwrap();
    }

    let query = SearchQuery {
        limit: Some(3),
        ..Default::default()
    };
    let results = store.search(&query).await.unwrap();
    assert_eq!(results.len(), 3);
}

#[tokio::test]
async fn top_by_fitness_orders_correctly() {
    let store = setup_store().await;

    let e1 = test_entry("dd0001", "fit-high", 1);
    let e2 = test_entry("dd0002", "fit-low", 1);
    store.insert_entry(&e1).await.unwrap();
    store.insert_entry(&e2).await.unwrap();

    store
        .record_fitness(
            &e1.hash,
            FitnessDomain::new("accuracy"),
            0.99,
            Timestamp::new("2026-03-01"),
        )
        .await
        .unwrap();
    store
        .record_fitness(
            &e2.hash,
            FitnessDomain::new("accuracy"),
            0.50,
            Timestamp::new("2026-03-01"),
        )
        .await
        .unwrap();

    let results = store
        .top_by_fitness(&FitnessDomain::new("accuracy"), 10)
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].version_ref.name.as_str(), "fit-high");
    assert_eq!(results[1].version_ref.name.as_str(), "fit-low");
}

#[tokio::test]
async fn get_nonexistent_returns_none() {
    let store = setup_store().await;
    let fake = format!("{:0>64}", "00ff").parse().unwrap();
    let result = store.get_by_hash(&fake).await.unwrap();
    assert!(result.is_none());
}
