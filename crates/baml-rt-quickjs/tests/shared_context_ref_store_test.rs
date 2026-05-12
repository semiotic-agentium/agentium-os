//! Two runtime managers sharing one [`baml_rt_tools::SharedContextRefStore`] must resolve the same
//! `@N` table for a provenance `context_id` (coordinator vs internal-A2A callee).

use baml_rt_quickjs::BamlRuntimeManager;
use baml_rt_tools::{
    SharedContextRefStore,
    archive_read::render_to_lines,
    archive_refs::{self, ArchiveEntry},
};
use serde_json::json;

#[test]
fn shared_store_two_managers_same_ref_table_and_visibility() {
    let store = SharedContextRefStore::new();
    let a = BamlRuntimeManager::builder()
        .with_shared_context_ref_store(store.clone())
        .build()
        .expect("manager a");
    let b = BamlRuntimeManager::builder()
        .with_shared_context_ref_store(store.clone())
        .build()
        .expect("manager b");

    let ctx = "shared-provenance-context";
    let ta = archive_refs::get_or_create_ref_table(a.archive_ref_tables().as_ref(), ctx);
    let tb = archive_refs::get_or_create_ref_table(b.archive_ref_tables().as_ref(), ctx);
    assert!(
        std::sync::Arc::ptr_eq(&ta, &tb),
        "both managers must share the same RefTable Arc for one context_id"
    );

    let content = render_to_lines(&json!([{"name": "alice"}]));
    let entry = ArchiveEntry::new(
        content,
        "support/crm".into(),
        Some("listed 1 account".into()),
        String::new(),
        "tool_result".into(),
    );
    let short = ta.insert(entry);
    assert!(
        tb.get(short).is_some(),
        "insert on one manager's path must be visible on the other"
    );
}
