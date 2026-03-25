//! Tests for SurrealStore: metadata CRUD, lineage traversal, search filters.

use baml_rt_repository::{
    entry::{
        ChangeRationale, ManifestSource, NewEntry, SourceBundle, SourceContent, SourceFile,
        SourcePath, Tag,
    },
    ids::{AgentName, Generation, LineageEdgeId, Version, VersionRef},
    lineage::{EdgeDescription, LineageEdge, LineageKind, Parentage},
    search::{
        FullTextTerm, GenerationFilter, LineageFilter, LineageRelation, SearchOrder, SearchQuery,
        TagFilter, ToolFilter,
    },
    storage::{LineageStore, MetadataStore, SearchStore},
};
#[path = "support/common.rs"]
mod common;
use common::setup_store;

fn test_manifest() -> ManifestSource {
    ManifestSource::new(serde_json::json!({
        "name": "test-agent",
        "version": "0.0.0",
        "tools": ["calculator", "weather"],
        "discovery": {
            "description": "A test agent for weather planning",
            "capabilities": ["weather-lookup", "math"]
        }
    }))
}

fn test_source_bundle_with(unique: &str) -> SourceBundle {
    SourceBundle {
        manifest: test_manifest(),
        ts_sources: vec![SourceFile {
            path: SourcePath::new("src/index.ts").unwrap(),
            content: SourceContent::new(format!("export function run() {{ return '{unique}'; }}")),
        }],
        baml_sources: vec![SourceFile {
            path: SourcePath::new("baml_src/main.baml").unwrap(),
            content: SourceContent::new("function GetWeather { input: string, output: string }"),
        }],
    }
}

/// Create a NewEntry with unique source content (uses name as uniquifier).
fn test_new_entry(name: &str) -> NewEntry {
    NewEntry {
        name: name.parse().unwrap(),
        source: test_source_bundle_with(name),
        parentage: Parentage::Original,
        generation: Generation::ROOT,
        change_rationale: ChangeRationale::new("initial creation").unwrap(),
        tags: vec![],
    }
}

/// Create a NewEntry with an explicit uniquifier for cases where multiple
/// entries share the same agent name (different versions).
fn test_new_entry_unique(name: &str, suffix: &str) -> NewEntry {
    NewEntry {
        name: name.parse().unwrap(),
        source: test_source_bundle_with(suffix),
        parentage: Parentage::Original,
        generation: Generation::ROOT,
        change_rationale: ChangeRationale::new("initial creation").unwrap(),
        tags: vec![],
    }
}

// -------------------------------------------------------------------------
// MetadataStore CRUD
// -------------------------------------------------------------------------

#[tokio::test]
async fn insert_and_get_by_hash() {
    let store = setup_store().await;
    let new = test_new_entry("weather-agent");
    let stored = store.insert_entry(&new).await.unwrap();

    assert_eq!(stored.version_ref.version, Version::FIRST);
    assert_eq!(stored.version_ref.name.as_str(), "weather-agent");
    // Verify version was written into the manifest
    assert_eq!(
        stored.source.manifest.version(),
        Some("1"),
        "manifest version should be the repository-assigned version"
    );

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
    let new = test_new_entry("planner-agent");
    let stored = store.insert_entry(&new).await.unwrap();

    let name: AgentName = "planner-agent".parse().unwrap();
    let loaded = store.get_by_version(&name, Version::FIRST).await.unwrap();
    assert!(loaded.is_some());
    assert_eq!(loaded.unwrap().hash, stored.hash);
}

#[tokio::test]
async fn get_latest_returns_highest_version() {
    let store = setup_store().await;
    let e1 = test_new_entry_unique("multi-ver", "multi-ver-v1");
    let _s1 = store.insert_entry(&e1).await.unwrap();

    let e2 = test_new_entry_unique("multi-ver", "multi-ver-v2");
    let s2 = store.insert_entry(&e2).await.unwrap();
    assert_eq!(s2.version_ref.version, Version::new(2).unwrap());

    let name: AgentName = "multi-ver".parse().unwrap();
    let latest = store.get_latest(&name).await.unwrap().unwrap();
    assert_eq!(latest.version_ref.version, Version::new(2).unwrap());
}

#[tokio::test]
async fn resolve_hash_returns_correct_hash() {
    let store = setup_store().await;
    let new = test_new_entry("resolver-agent");
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
    let new = test_new_entry("brand-new-agent");
    let stored = store.insert_entry(&new).await.unwrap();
    assert_eq!(stored.version_ref.version, Version::FIRST);
}

#[tokio::test]
async fn version_auto_increments() {
    let store = setup_store().await;
    let e1 = test_new_entry_unique("incrementor", "inc-v1");
    let s1 = store.insert_entry(&e1).await.unwrap();
    assert_eq!(s1.version_ref.version, Version::FIRST);

    let e2 = test_new_entry_unique("incrementor", "inc-v2");
    let s2 = store.insert_entry(&e2).await.unwrap();
    assert_eq!(s2.version_ref.version, Version::new(2).unwrap());
}

#[tokio::test]
async fn version_in_manifest_affects_hash() {
    let store = setup_store().await;

    // Two entries for the same name with same source get different versions,
    // which means different manifest content, which means different hashes.
    let e1 = test_new_entry_unique("hash-test", "same-source");
    let s1 = store.insert_entry(&e1).await.unwrap();

    let e2 = test_new_entry_unique("hash-test", "same-source");
    let s2 = store.insert_entry(&e2).await.unwrap();

    // Different versions → different manifest → different hash
    assert_ne!(s1.hash, s2.hash);
    assert_eq!(s1.source.manifest.version(), Some("1"));
    assert_eq!(s2.source.manifest.version(), Some("2"));
}

#[tokio::test]
async fn list_versions_returns_all() {
    let store = setup_store().await;
    store
        .insert_entry(&test_new_entry_unique("listed-agent", "listed-v1"))
        .await
        .unwrap();
    store
        .insert_entry(&test_new_entry_unique("listed-agent", "listed-v2"))
        .await
        .unwrap();

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
        .insert_entry(&test_new_entry_unique("alpha-agent", "alpha-v1"))
        .await
        .unwrap();
    store
        .insert_entry(&test_new_entry_unique("alpha-agent", "alpha-v2"))
        .await
        .unwrap();
    store
        .insert_entry(&test_new_entry("beta-agent"))
        .await
        .unwrap();

    let agents = store.list_agents().await.unwrap();
    assert_eq!(agents.len(), 2);
}

#[tokio::test]
async fn duplicate_hash_is_rejected() {
    let store = setup_store().await;
    // Insert an entry for "dup-agent"
    let new = test_new_entry("dup-agent");
    store.insert_entry(&new).await.unwrap();

    // Insert with a different name but identical source content.
    // Both get version 1, same manifest content, same source → same hash → rejected.
    let dup = NewEntry {
        name: "other-agent".parse().unwrap(),
        source: test_source_bundle_with("dup-agent"), // same source as above
        parentage: Parentage::Original,
        generation: Generation::ROOT,
        change_rationale: ChangeRationale::new("duplicate test").unwrap(),
        tags: vec![],
    };
    let result = store.insert_entry(&dup).await;
    assert!(result.is_err());
}

// -------------------------------------------------------------------------
// Tags
// -------------------------------------------------------------------------

#[tokio::test]
async fn add_and_remove_tags() {
    let store = setup_store().await;
    let new = test_new_entry("tag-agent");
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
    let new = test_new_entry("idem-agent");
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
    let parent = store
        .insert_entry(&test_new_entry_unique("parent-agent", "parent-v1"))
        .await
        .unwrap();

    let mut child_new = test_new_entry_unique("parent-agent", "parent-v2");
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
    let parent = store
        .insert_entry(&test_new_entry_unique("root-agent", "root-v1"))
        .await
        .unwrap();

    let child = store
        .insert_entry(&test_new_entry_unique("root-agent", "root-v2"))
        .await
        .unwrap();

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

    let grandparent = store
        .insert_entry(&test_new_entry_unique("lineage-agent", "lineage-v1"))
        .await
        .unwrap();
    let parent = store
        .insert_entry(&test_new_entry_unique("lineage-agent", "lineage-v2"))
        .await
        .unwrap();
    let child = store
        .insert_entry(&test_new_entry_unique("lineage-agent", "lineage-v3"))
        .await
        .unwrap();

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

    let g = store
        .insert_entry(&test_new_entry_unique("depth-agent", "depth-v1"))
        .await
        .unwrap();
    let p = store
        .insert_entry(&test_new_entry_unique("depth-agent", "depth-v2"))
        .await
        .unwrap();
    let c = store
        .insert_entry(&test_new_entry_unique("depth-agent", "depth-v3"))
        .await
        .unwrap();

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

    let parent = store
        .insert_entry(&test_new_entry_unique("sub-agent", "sub-v1"))
        .await
        .unwrap();
    let focal = store
        .insert_entry(&test_new_entry_unique("sub-agent", "sub-v2"))
        .await
        .unwrap();
    let child = store
        .insert_entry(&test_new_entry_unique("sub-agent", "sub-v3"))
        .await
        .unwrap();

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
async fn subgraph_edges_only_reference_returned_nodes() {
    let store = setup_store().await;

    let grandparent = store
        .insert_entry(&test_new_entry_unique("subgraph-scope-agent", "scope-v1"))
        .await
        .unwrap();
    let parent = store
        .insert_entry(&test_new_entry_unique("subgraph-scope-agent", "scope-v2"))
        .await
        .unwrap();
    let child = store
        .insert_entry(&test_new_entry_unique("subgraph-scope-agent", "scope-v3"))
        .await
        .unwrap();

    store
        .record_edges(&[
            LineageEdge {
                id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                source: grandparent.hash.clone(),
                target: parent.hash.clone(),
                kind: LineageKind::Fork,
                description: EdgeDescription::new("g to p").unwrap(),
            },
            LineageEdge {
                id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                source: parent.hash.clone(),
                target: child.hash.clone(),
                kind: LineageKind::Fork,
                description: EdgeDescription::new("p to c").unwrap(),
            },
        ])
        .await
        .unwrap();

    let subgraph = store.subgraph(&child.hash, 1).await.unwrap();
    let node_hashes: std::collections::BTreeSet<_> = subgraph
        .ancestors
        .iter()
        .map(|node| node.hash.clone())
        .chain(std::iter::once(subgraph.root.clone()))
        .chain(subgraph.descendants.iter().map(|node| node.hash.clone()))
        .collect();

    assert_eq!(subgraph.edges.len(), 1);
    for edge in &subgraph.edges {
        assert!(node_hashes.contains(&edge.source));
        assert!(node_hashes.contains(&edge.target));
    }
}

#[tokio::test]
async fn influenced_by_returns_influence_children() {
    let store = setup_store().await;

    let source = store
        .insert_entry(&test_new_entry("influence-src"))
        .await
        .unwrap();
    let influenced = store
        .insert_entry(&test_new_entry("influenced-agent"))
        .await
        .unwrap();

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
        .insert_entry(&test_new_entry("search-alpha"))
        .await
        .unwrap();
    store
        .insert_entry(&test_new_entry("search-beta"))
        .await
        .unwrap();

    let results = store.search(&SearchQuery::default()).await.unwrap();
    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn search_by_agent_name() {
    let store = setup_store().await;
    store
        .insert_entry(&test_new_entry("target-agent"))
        .await
        .unwrap();
    store
        .insert_entry(&test_new_entry("other-agent"))
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
    let mut entry = test_new_entry("tagged-agent");
    entry.tags = vec![Tag::new("production"), Tag::new("stable")];
    store.insert_entry(&entry).await.unwrap();

    store
        .insert_entry(&test_new_entry("untagged-agent"))
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
        .insert_entry(&test_new_entry("tool-agent"))
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

    store
        .insert_entry(&test_new_entry("gen-zero"))
        .await
        .unwrap();

    let mut e1 = test_new_entry("gen-one");
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
async fn search_by_full_text_matches_source_content() {
    let store = setup_store().await;
    store
        .insert_entry(&test_new_entry_unique("fts-hit", "needle-token-fts"))
        .await
        .unwrap();
    store
        .insert_entry(&test_new_entry_unique("fts-miss", "different-token"))
        .await
        .unwrap();

    let query = SearchQuery {
        text: Some(FullTextTerm::new("needle-token-fts")),
        ..Default::default()
    };
    let results = store.search(&query).await.unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].version_ref.name.as_str(), "fts-hit");
}

#[tokio::test]
async fn search_by_lineage_descendant_of_filters_results() {
    let store = setup_store().await;

    let root = store
        .insert_entry(&test_new_entry_unique("lineage-search", "lineage-root"))
        .await
        .unwrap();
    let child = store
        .insert_entry(&test_new_entry_unique("lineage-search", "lineage-child"))
        .await
        .unwrap();
    let grandchild = store
        .insert_entry(&test_new_entry_unique(
            "lineage-search",
            "lineage-grandchild",
        ))
        .await
        .unwrap();
    let unrelated = store
        .insert_entry(&test_new_entry("lineage-unrelated"))
        .await
        .unwrap();

    store
        .record_edges(&[
            LineageEdge {
                id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                source: root.hash.clone(),
                target: child.hash.clone(),
                kind: LineageKind::Fork,
                description: EdgeDescription::new("root to child").unwrap(),
            },
            LineageEdge {
                id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                source: child.hash.clone(),
                target: grandchild.hash.clone(),
                kind: LineageKind::Fork,
                description: EdgeDescription::new("child to grandchild").unwrap(),
            },
        ])
        .await
        .unwrap();

    let query = SearchQuery {
        lineage: Some(LineageFilter {
            relation: LineageRelation::DescendantOf {
                ancestor: root.hash.clone(),
                kind: None,
            },
        }),
        order: SearchOrder::Oldest,
        limit: Some(10),
        ..Default::default()
    };
    let results = store.search(&query).await.unwrap();

    let hashes: Vec<_> = results.iter().map(|entry| entry.hash.clone()).collect();
    assert_eq!(results.len(), 2);
    assert!(hashes.contains(&child.hash));
    assert!(hashes.contains(&grandchild.hash));
    assert!(!hashes.contains(&unrelated.hash));
    assert!(!hashes.contains(&root.hash));
}

#[tokio::test]
async fn search_by_lineage_ancestor_of_filters_results() {
    let store = setup_store().await;

    let root = store
        .insert_entry(&test_new_entry_unique("ancestor-search", "ancestor-root"))
        .await
        .unwrap();
    let parent = store
        .insert_entry(&test_new_entry_unique("ancestor-search", "ancestor-parent"))
        .await
        .unwrap();
    let child = store
        .insert_entry(&test_new_entry_unique("ancestor-search", "ancestor-child"))
        .await
        .unwrap();
    let unrelated = store
        .insert_entry(&test_new_entry("ancestor-unrelated"))
        .await
        .unwrap();

    store
        .record_edges(&[
            LineageEdge {
                id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                source: root.hash.clone(),
                target: parent.hash.clone(),
                kind: LineageKind::Fork,
                description: EdgeDescription::new("root to parent").unwrap(),
            },
            LineageEdge {
                id: LineageEdgeId::from_uuid(uuid::Uuid::new_v4()),
                source: parent.hash.clone(),
                target: child.hash.clone(),
                kind: LineageKind::Fork,
                description: EdgeDescription::new("parent to child").unwrap(),
            },
        ])
        .await
        .unwrap();

    let query = SearchQuery {
        lineage: Some(LineageFilter {
            relation: LineageRelation::AncestorOf {
                descendant: child.hash.clone(),
                kind: None,
            },
        }),
        order: SearchOrder::Oldest,
        limit: Some(10),
        ..Default::default()
    };
    let results = store.search(&query).await.unwrap();

    let hashes: Vec<_> = results.iter().map(|entry| entry.hash.clone()).collect();
    assert_eq!(results.len(), 2);
    assert!(hashes.contains(&root.hash));
    assert!(hashes.contains(&parent.hash));
    assert!(!hashes.contains(&unrelated.hash));
    assert!(!hashes.contains(&child.hash));
}

#[tokio::test]
async fn search_with_limit() {
    let store = setup_store().await;
    for i in 1..=5 {
        let name = format!("limit-agent-{i}");
        store.insert_entry(&test_new_entry(&name)).await.unwrap();
    }

    let query = SearchQuery {
        limit: Some(3),
        ..Default::default()
    };
    let results = store.search(&query).await.unwrap();
    assert_eq!(results.len(), 3);
}

#[tokio::test]
async fn get_nonexistent_returns_none() {
    let store = setup_store().await;
    let fake = format!("{:0>64}", "00ff").parse().unwrap();
    let result = store.get_by_hash(&fake).await.unwrap();
    assert!(result.is_none());
}
