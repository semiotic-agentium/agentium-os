//! Config store integration tests (SurrealConfigStore).

use baml_rt_config::{ConfigReader, ConfigWriter, SurrealConfigStore};
use baml_rt_tools::BundleName;
use serde_json::json;

async fn mem_store() -> SurrealConfigStore {
    SurrealConfigStore::in_memory()
        .await
        .expect("in-memory config store")
}

#[tokio::test]
async fn store_roundtrip() {
    let store = mem_store().await;
    let bundle = BundleName::new("llm").expect("bundle name");
    let config = json!({
        "default": "Default",
        "clients": {
            "Default": {
                "name": "Default",
                "provider": "openrouter",
                "options": { "model": "openai/gpt-4o-mini" }
            }
        }
    });
    let written = store.set(&bundle, config.clone()).await.expect("set");
    assert_eq!(written.version.0, 1);

    let got = store
        .get(&bundle)
        .await
        .expect("get")
        .expect("config present");
    assert_eq!(got.get("default").and_then(|v| v.as_str()), Some("Default"));

    let list = store.list_with_config().await.expect("list_with_config");
    assert!(list.iter().any(|b| b.as_str() == "llm"));

    let with_ver = store
        .get_with_version(&bundle)
        .await
        .expect("get_with_version")
        .expect("some");
    assert_eq!(
        with_ver.config.get("default").and_then(|v| v.as_str()),
        Some("Default")
    );
    assert_eq!(with_ver.version.0, 1);
}

#[tokio::test]
async fn store_version_history() {
    let store = mem_store().await;
    let bundle = BundleName::new("llm").expect("bundle name");
    store.set(&bundle, json!({ "v": 1 })).await.expect("set v1");
    store.set(&bundle, json!({ "v": 2 })).await.expect("set v2");

    let at_1 = store
        .get_version(&bundle, 1)
        .await
        .expect("get_version 1")
        .expect("version 1");
    let at_2 = store
        .get_version(&bundle, 2)
        .await
        .expect("get_version 2")
        .expect("version 2");
    assert_eq!(at_1.config.get("v").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(at_2.config.get("v").and_then(|v| v.as_i64()), Some(2));

    let versions = store.list_versions(&bundle).await.expect("list_versions");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].version.0, 2);
    assert_eq!(versions[1].version.0, 1);
}

#[tokio::test]
async fn store_delete() {
    let store = mem_store().await;
    let bundle = BundleName::new("llm").expect("bundle name");
    store.set(&bundle, json!({ "x": 1 })).await.expect("set");
    assert!(store.get(&bundle).await.expect("get").is_some());

    store.delete(&bundle).await.expect("delete");
    assert!(
        store
            .get(&bundle)
            .await
            .expect("get after delete")
            .is_none()
    );
    let list = store.list_with_config().await.expect("list");
    assert!(list.is_empty());
}

#[tokio::test]
async fn store_in_memory() {
    let store = mem_store().await;
    let bundle = BundleName::new("llm").expect("bundle name");
    store
        .set(&bundle, json!({ "default": "Default" }))
        .await
        .expect("set");
    let got = store
        .get(&bundle)
        .await
        .expect("get")
        .expect("config present");
    assert_eq!(got.get("default").and_then(|v| v.as_str()), Some("Default"));
}
