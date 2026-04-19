//! Config store integration tests (SurrealConfigStore).

use baml_rt_config::{
    ConfigReader, ConfigWriter, InternalConfigReader, InternalConfigWriter, SurrealConfigStore,
};
use baml_rt_tools::BundleName;
use serde_json::json;

async fn mem_store() -> SurrealConfigStore {
    SurrealConfigStore::in_memory()
        .await
        .expect("in-memory config store")
}

async fn remote_mem_store() -> SurrealConfigStore {
    SurrealConfigStore::remote("mem://", None)
        .await
        .expect("remote mem:// config store")
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

// ── File-backed (SurrealKV) constructor ──────────────────────────────────

#[tokio::test]
async fn file_backed_store_roundtrip() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("config.db");
    let store = SurrealConfigStore::open(&db_path).await.expect("open");
    let bundle = BundleName::new("llm").expect("bundle name");
    let config = json!({ "default": "FileBacked" });
    let written = store.set(&bundle, config).await.expect("set");
    assert_eq!(written.version.0, 1);
    let got = store.get(&bundle).await.expect("get").expect("present");
    assert_eq!(
        got.get("default").and_then(|v| v.as_str()),
        Some("FileBacked")
    );
}

// ── In-memory isolation ──────────────────────────────────────────────────

#[tokio::test]
async fn in_memory_stores_are_isolated() {
    let store_a = mem_store().await;
    let store_b = mem_store().await;
    let bundle = BundleName::new("llm").expect("bundle name");
    store_a
        .set(&bundle, json!({ "source": "A" }))
        .await
        .expect("set on A");
    let from_b = store_b.get(&bundle).await.expect("get from B");
    assert!(
        from_b.is_none(),
        "in-memory stores must be isolated; B should not see A's data"
    );
}

// ── Remote constructor ───────────────────────────────────────────────────

#[tokio::test]
async fn remote_store_connection_refused() {
    let result = SurrealConfigStore::remote("ws://127.0.0.1:1", None).await;
    assert!(
        result.is_err(),
        "connecting to a refused port must return Err"
    );
}

#[tokio::test]
async fn remote_store_roundtrip() {
    let store = remote_mem_store().await;
    let bundle = BundleName::new("llm").expect("bundle name");
    let config = json!({
        "default": "GPT4",
        "clients": {
            "GPT4": { "name": "GPT4", "provider": "openrouter", "options": { "model": "openai/gpt-4o" } }
        }
    });
    let written = store.set(&bundle, config.clone()).await.expect("set");
    assert_eq!(written.version.0, 1);
    let got = store.get(&bundle).await.expect("get").expect("present");
    assert_eq!(got.get("default").and_then(|v| v.as_str()), Some("GPT4"));
}

#[tokio::test]
async fn remote_store_version_history() {
    let store = remote_mem_store().await;
    let bundle = BundleName::new("llm").expect("bundle name");
    store.set(&bundle, json!({ "v": 1 })).await.expect("v1");
    store.set(&bundle, json!({ "v": 2 })).await.expect("v2");
    let versions = store.list_versions(&bundle).await.expect("list_versions");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].version.0, 2);
    assert_eq!(versions[1].version.0, 1);
}

// ── Internal config (secret links persistence) ──────────────────────────

#[tokio::test]
async fn internal_config_roundtrip() {
    let store = remote_mem_store().await;
    let key = "_secret_links";
    let value = json!({
        "links": { "CLICKUP_API_KEY": "env.CLICKUP_API_KEY" },
        "unlinked": ["SLACK_BOT_TOKEN"]
    });
    store
        .set_internal(key, value.clone())
        .await
        .expect("set_internal");
    let got = store
        .get_internal(key)
        .await
        .expect("get_internal")
        .expect("present");
    assert_eq!(got, value);
}

#[tokio::test]
async fn internal_config_upsert() {
    let store = remote_mem_store().await;
    let key = "_secret_links";
    let v1 = json!({ "links": { "A": "a" }, "unlinked": [] });
    let v2 = json!({ "links": { "A": "a", "B": "b" }, "unlinked": ["C"] });
    store.set_internal(key, v1).await.expect("set v1");
    store.set_internal(key, v2.clone()).await.expect("set v2");
    let got = store
        .get_internal(key)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got, v2, "set_internal must upsert, not append");
}
