#![allow(dead_code)]

use std::sync::Arc;

use axum::Router;
use baml_rt_repository::{
    entry::{ManifestSource, SourceBundle, SourceContent, SourceFile, SourcePath},
    router::repository_router,
    service::RepositoryService,
    storage::{BlobStore, LineageStore, MetadataStore, SearchStore},
    surreal_store::SurrealStore,
};

pub fn make_source(
    content: &str,
    manifest_name: &str,
    tools: &[&str],
    description: &str,
    capabilities: &[&str],
) -> SourceBundle {
    SourceBundle {
        manifest: ManifestSource::new(serde_json::json!({
            "name": manifest_name,
            "version": "1.0.0",
            "tools": tools,
            "discovery": {
                "description": description,
                "capabilities": capabilities
            }
        })),
        ts_sources: vec![SourceFile {
            path: SourcePath::new("src/index.ts").expect("valid test source path"),
            content: SourceContent::new(content),
        }],
        baml_sources: vec![],
    }
}

pub async fn setup_store() -> SurrealStore {
    SurrealStore::open_in_memory()
        .await
        .expect("in-memory store")
}

pub async fn setup_service() -> RepositoryService {
    let store = Arc::new(setup_store().await);

    RepositoryService::new(
        store.clone() as Arc<dyn BlobStore>,
        store.clone() as Arc<dyn MetadataStore>,
        store.clone() as Arc<dyn LineageStore>,
        store as Arc<dyn SearchStore>,
    )
}

pub async fn setup_app() -> Router {
    let store = Arc::new(setup_store().await);
    let svc = Arc::new(RepositoryService::new(
        store.clone() as Arc<dyn BlobStore>,
        store.clone() as Arc<dyn MetadataStore>,
        store.clone() as Arc<dyn LineageStore>,
        store as Arc<dyn SearchStore>,
    ));
    repository_router(svc)
}
