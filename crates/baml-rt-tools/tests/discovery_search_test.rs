//! Tests for tool discovery: ToolRegistry::search_tools is global (whole catalog from inventory).

use baml_rt_tools::ToolRegistry;

#[tokio::test]
async fn search_tools_global_catalog_exact_full_name_ranks_first() {
    let registry = ToolRegistry::new();
    let results = registry.search_tools("support/calculate", 10);
    assert!(
        !results.is_empty(),
        "inventory should contain support/calculate"
    );
    assert_eq!(
        results[0].name.to_string(),
        "support/calculate",
        "exact full name should rank first"
    );
}

#[tokio::test]
async fn search_tools_respects_limit() {
    let registry = ToolRegistry::new();
    let results = registry.search_tools("", 1);
    assert_eq!(results.len(), 1);
}
