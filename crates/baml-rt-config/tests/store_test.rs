//! Config store integration tests (SqliteConfigStore).
//!
//! **Coverage (no duplicate scenarios):**
//!
//! | Test                    | Backend   | Operations covered                                      |
//! |-------------------------|-----------|----------------------------------------------------------|
//! | store_roundtrip         | file      | set, get, list_with_config, get_with_version (StoredConfig) |
//! | store_version_history   | file      | set (×2), get_version, list_versions (order)             |
//! | store_delete            | file      | set, get, delete, get (None), list_with_config (empty)  |
//! | store_in_memory          | in_memory | set, get (same code path as runner E2E config store)     |
//!
//! **Not duplicated:** Runner test `test_client_registry_substitution_from_config` is E2E
//! (config → store → LlmClientConfig → StaticResolver → registry); it uses in_memory store
//! but asserts resolver/registry, not store behaviour. Store behaviour is covered here.

use baml_rt_config::{ConfigReader, ConfigWriter, SqliteConfigStore};
use baml_rt_tools::BundleName;
use serde_json::json;
use tempfile::tempdir;

fn file_store() -> (SqliteConfigStore, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("config.db");
    let store = SqliteConfigStore::open(&path).expect("file store");
    (store, dir)
}

#[test]
fn store_roundtrip() {
    let (store, _dir) = file_store();
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
    let written = store.set(&bundle, config.clone()).expect("set");
    assert_eq!(written.version.0, 1);

    let got = store.get(&bundle).expect("get").expect("config present");
    assert_eq!(got.get("default").and_then(|v| v.as_str()), Some("Default"));

    let list = store.list_with_config().expect("list_with_config");
    assert!(list.iter().any(|b| b.as_str() == "llm"));

    let with_ver = store
        .get_with_version(&bundle)
        .expect("get_with_version")
        .expect("some");
    assert_eq!(
        with_ver.config.get("default").and_then(|v| v.as_str()),
        Some("Default")
    );
    assert_eq!(with_ver.version.0, 1);
}

#[test]
fn store_version_history() {
    let (store, _dir) = file_store();
    let bundle = BundleName::new("llm").expect("bundle name");
    store.set(&bundle, json!({ "v": 1 })).expect("set v1");
    store.set(&bundle, json!({ "v": 2 })).expect("set v2");

    let at_1 = store
        .get_version(&bundle, 1)
        .expect("get_version 1")
        .expect("version 1");
    let at_2 = store
        .get_version(&bundle, 2)
        .expect("get_version 2")
        .expect("version 2");
    assert_eq!(at_1.config.get("v").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(at_2.config.get("v").and_then(|v| v.as_i64()), Some(2));

    let versions = store.list_versions(&bundle).expect("list_versions");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].version.0, 2);
    assert_eq!(versions[1].version.0, 1);
}

#[test]
fn store_delete() {
    let (store, _dir) = file_store();
    let bundle = BundleName::new("llm").expect("bundle name");
    store.set(&bundle, json!({ "x": 1 })).expect("set");
    assert!(store.get(&bundle).expect("get").is_some());

    store.delete(&bundle).expect("delete");
    assert!(store.get(&bundle).expect("get after delete").is_none());
    let list = store.list_with_config().expect("list");
    assert!(list.is_empty());
}

#[test]
fn store_in_memory() {
    let store = SqliteConfigStore::in_memory().expect("in_memory store");
    let bundle = BundleName::new("llm").expect("bundle name");
    store
        .set(&bundle, json!({ "default": "Default" }))
        .expect("set");
    let got = store.get(&bundle).expect("get").expect("config present");
    assert_eq!(got.get("default").and_then(|v| v.as_str()), Some("Default"));
}
