//! Tests for SqliteStore: metadata CRUD, lineage traversal, search filters.

use baml_rt_repository::{
    entry::{
        ChangeRationale, FitnessDomain, ManifestSource, NewEntry, SourceBundle, SourceContent,
        SourceFile, SourcePath, Tag, Timestamp,
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

fn test_new_entry(hash_suffix: &str, name: &str) -> NewEntry {
    let hash_str = format!("{:0>64}", hash_suffix);
    NewEntry {
        hash: hash_str.parse().unwrap(),
        name: name.parse().unwrap(),
        source: test_source_bundle(),
        parentage: Parentage::Original,
        generation: Generation::ROOT,
        change_rationale: ChangeRationale::new("initial creation").unwrap(),
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
    let new = test_new_entry("aabb01", "weather-agent");
    let stored = store.insert_entry(&new).await.unwrap();

    assert_eq!(stored.version_ref.version, Version::FIRST);
    assert_eq!(stored.version_ref.name.as_str(), "weather-agent");

    let loaded = store.get_by_hash(&stored.hash).await.unwrap();
    assert!(loaded.is_some());
    let loaded = loaded.unwrap();
    assert_eq!(loaded.hash, stored.hash);
    assert_eq!(loaded.version_ref, stored.version_ref);
    assert_eq!(loaded.generation, Generation::ROOT);
}

#[tokio::test]
async fn insert_and_get_by_version() {
    let store = setup_store().await;
    let new = test_new_entry("aabb02", "planner-agent");
    let stored = store.insert_entry(&new).await.unwrap();

    let name: AgentName = "planner-agent".parse().unwrap();
    let loaded = store.get_by_version(&name, Version::FIRST).await.unwrap();
    assert!(loaded.is_some());
    assert_eq!(loaded.unwrap().hash, stored.hash);
}

#[tokio::test]
async fn get_latest_returns_highest_version() {
    let store = setup_store().await;
    let e1 = test_new_entry("cc0001", "multi-ver");
    let _s1 = store.insert_entry(&e1).await.unwrap();

    let e2 = test_new_entry("cc0002", "multi-ver");
    let s2 = store.insert_entry(&e2).await.unwrap();
    assert_eq!(s2.version_ref.version, Version::new(2).unwrap());

    let name: AgentName = "multi-ver".parse().unwrap();
    let latest = store.get_latest(&name).await.unwrap().unwrap();
    assert_eq!(latest.version_ref.version, Version::new(2).unwrap());
}

#[tokio::test]
async fn resolve_hash_returns_correct_hash() {
    let store = setup_store().await;
    let new = test_new_entry("dd0001", "resolver-agent");
    let stored = store.insert_entry(&new).await.unwrap();

    let vref = VersionRef {
        name: "resolver-agent".parse().unwrap(),
        version: Version::FIRST,
    };
    let hash = store.resolve_hash(&vref).await.unwrap().unwrap();
    assert_eq!(hash, stored.hash);
}

#[tokio::test]
async fn version_auto_assigned_starts_at_one() {
    let store = setup_store().await;
    let new = test_new_entry("ee0001", "brand-new-agent");
    let stored = store.insert_entry(&new).await.unwrap();
    assert_eq!(stored.version_ref.version, Version::FIRST);
}

#[tokio::test]
async fn version_auto_increments() {
    let store = setup_store().await;
    let e1 = test_new_entry("ee0001", "incrementor");
    let s1 = store.insert_entry(&e1).await.unwrap();
    assert_eq!(s1.version_ref.version, Version::FIRST);

    let e2 = test_new_entry("ee0002", "incrementor");
    let s2 = store.insert_entry(&e2).await.unwrap();
    assert_eq!(s2.version_ref.version, Version::new(2).unwrap());
}

#[tokio::test]
async fn list_versions_returns_all() {
    let store = setup_store().await;
    let e1 = test_new_entry("ff0001", "listed-agent");
    let e2 = test_new_entry("ff0002", "listed-agent");
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
        .insert_entry(&test_new_entry("110001", "alpha-agent"))
        .await
        .unwrap();
    store
        .insert_entry(&test_new_entry("110002", "alpha-agent"))
        .await
        .unwrap();
    store
        .insert_entry(&test_new_entry("110003", "beta-agent"))
        .await
        .unwrap();

    let agents = store.list_agents().await.unwrap();
    assert_eq!(agents.len(), 2);
}

#[tokio::test]
async fn duplicate_hash_is_rejected() {
    let store = setup_store().await;
    let new = test_new_entry("220001", "dup-agent");
    store.insert_entry(&new).await.unwrap();

    // Same hash under different name should fail
    let dup = NewEntry {
        hash: new.hash.clone(),
        name: "other-agent".parse().unwrap(),
        source: test_source_bundle(),
        parentage: Parentage::Original,
        generation: Generation::ROOT,
        change_rationale: ChangeRationale::new("duplicate test").unwrap(),
        tags: vec![],
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
    let new = test_new_entry("330001", "fit-agent");
    let stored = store.insert_entry(&new).await.unwrap();

    store
        .record_fitness(
            &stored.hash,
            FitnessDomain::new("accuracy"),
            0.95,
            Timestamp::new("2026-03-01T00:00:00Z"),
        )
        .await
        .unwrap();

    let loaded = store.get_by_hash(&stored.hash).await.unwrap().unwrap();
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
    let new = test_new_entry("440001", "tag-agent");
    let stored = store.insert_entry(&new).await.unwrap();

    store
        .add_tag(&stored.hash, Tag::new("stable"))
        .await
        .unwrap();
    store
        .add_tag(&stored.hash, Tag::new("production"))
        .await
        .unwrap();

    let loaded = store.get_by_hash(&stored.hash).await.unwrap().unwrap();
    assert_eq!(loaded.tags.len(), 2);

    store
        .remove_tag(&stored.hash, &Tag::new("stable"))
        .await
        .unwrap();
    let loaded = store.get_by_hash(&stored.hash).await.unwrap().unwrap();
    assert_eq!(loaded.tags.len(), 1);
    assert_eq!(loaded.tags[0].as_str(), "production");
}

#[tokio::test]
async fn add_tag_idempotent() {
    let store = setup_store().await;
    let new = test_new_entry("440002", "idem-agent");
    let stored = store.insert_entry(&new).await.unwrap();

    store
        .add_tag(&stored.hash, Tag::new("stable"))
        .await
        .unwrap();
    store
        .add_tag(&stored.hash, Tag::new("stable"))
        .await
        .unwrap();

    let loaded = store.get_by_hash(&stored.hash).await.unwrap().unwrap();
    assert_eq!(loaded.tags.len(), 1);
}

// -------------------------------------------------------------------------
// LineageStore: edges, parents, children, ancestors
// -------------------------------------------------------------------------

#[tokio::test]
async fn record_edges_and_query_parents() {
    let store = setup_store().await;
    let parent_new = test_new_entry("550001", "parent-agent");
    let parent = store.insert_entry(&parent_new).await.unwrap();

    let mut child_new = test_new_entry("550002", "parent-agent");
    child_new.parentage = Parentage::Forked {
        parent: parent.hash.clone(),
        description: EdgeDescription::new("iterated").unwrap(),
    };
    child_new.generation = Generation::new(1);
    let child = store.insert_entry(&child_new).await.unwrap();

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
    let parent_new = test_new_entry("660001", "root-agent");
    let parent = store.insert_entry(&parent_new).await.unwrap();

    let child_new = test_new_entry("660002", "root-agent");
    let child = store.insert_entry(&child_new).await.unwrap();

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

    let gp_new = test_new_entry("770001", "lineage-agent");
    let grandparent = store.insert_entry(&gp_new).await.unwrap();

    let p_new = test_new_entry("770002", "lineage-agent");
    let parent = store.insert_entry(&p_new).await.unwrap();

    let c_new = test_new_entry("770003", "lineage-agent");
    let child = store.insert_entry(&c_new).await.unwrap();

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
    let hashes: Vec<_> = ancestors.iter().map(|a| a.hash.clone()).collect();
    assert!(hashes.contains(&grandparent.hash));
    assert!(hashes.contains(&parent.hash));
}

#[tokio::test]
async fn ancestors_respects_depth_limit() {
    let store = setup_store().await;

    let g_new = test_new_entry("880001", "depth-agent");
    let g = store.insert_entry(&g_new).await.unwrap();

    let p_new = test_new_entry("880002", "depth-agent");
    let p = store.insert_entry(&p_new).await.unwrap();

    let c_new = test_new_entry("880003", "depth-agent");
    let c = store.insert_entry(&c_new).await.unwrap();

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

    let parent_new = test_new_entry("990001", "sub-agent");
    let parent = store.insert_entry(&parent_new).await.unwrap();

    let focal_new = test_new_entry("990002", "sub-agent");
    let focal = store.insert_entry(&focal_new).await.unwrap();

    let child_new = test_new_entry("990003", "sub-agent");
    let child = store.insert_entry(&child_new).await.unwrap();

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

    let source_new = test_new_entry("aa0001", "influence-src");
    let source = store.insert_entry(&source_new).await.unwrap();

    let influenced_new = test_new_entry("aa0002", "influenced-agent");
    let influenced = store.insert_entry(&influenced_new).await.unwrap();

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
        .insert_entry(&test_new_entry("bb0001", "search-alpha"))
        .await
        .unwrap();
    store
        .insert_entry(&test_new_entry("bb0002", "search-beta"))
        .await
        .unwrap();

    let results = store.search(&SearchQuery::default()).await.unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn search_by_agent_name() {
    let store = setup_store().await;
    store
        .insert_entry(&test_new_entry("bb0003", "target-agent"))
        .await
        .unwrap();
    store
        .insert_entry(&test_new_entry("bb0004", "other-agent"))
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
    let mut entry = test_new_entry("bb0005", "tagged-agent");
    entry.tags = vec![Tag::new("production"), Tag::new("stable")];
    store.insert_entry(&entry).await.unwrap();

    store
        .insert_entry(&test_new_entry("bb0006", "untagged-agent"))
        .await
        .unwrap();

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
        .insert_entry(&test_new_entry("bb0007", "tool-agent"))
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

    let e0 = test_new_entry("bb0008", "gen-zero");
    store.insert_entry(&e0).await.unwrap();

    let mut e1 = test_new_entry("bb0009", "gen-one");
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
            .insert_entry(&test_new_entry(&hash, &name))
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

    let e1 = test_new_entry("dd0001", "fit-high");
    let s1 = store.insert_entry(&e1).await.unwrap();

    let e2 = test_new_entry("dd0002", "fit-low");
    let s2 = store.insert_entry(&e2).await.unwrap();

    store
        .record_fitness(
            &s1.hash,
            FitnessDomain::new("accuracy"),
            0.99,
            Timestamp::new("2026-03-01"),
        )
        .await
        .unwrap();
    store
        .record_fitness(
            &s2.hash,
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
