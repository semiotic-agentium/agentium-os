//! Exploratory integration tests for bootstrap: run_bootstrap in a temp dir
//! and assert on created layout and key content (no TUI).
//! Uses insta snapshot testing for generated artifacts.

use std::fs;

use baml_rt_builder::builder::{
    baml_gen::GENERATED_BAML_PRELUDE_FILE,
    bootstrap::{run_bootstrap, slug_from_name},
};
use tempfile::TempDir;

fn collect_artifacts(root_path: &std::path::Path) -> String {
    let manifest = fs::read_to_string(root_path.join("manifest.json")).unwrap();
    let manifest_json: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let manifest_pretty = serde_json::to_string_pretty(&manifest_json).unwrap();

    let baml_src = root_path.join("baml_src");
    let prompt_name = manifest_json["name"].as_str().unwrap().replace('-', "_");
    let prompt_path = baml_src.join(format!("{}_prompt.baml", prompt_name));
    let prompt = fs::read_to_string(&prompt_path).unwrap_or_else(|_| "<missing>".into());

    let index_ts = fs::read_to_string(root_path.join("src").join("index.ts"))
        .unwrap_or_else(|_| "<missing>".into());

    let d_ts_path = root_path.join("src").join("baml-runtime.d.ts");
    let d_ts = fs::read_to_string(&d_ts_path).unwrap_or_else(|_| "<missing>".into());

    let tsconfig_path = root_path.join("tsconfig.json");
    let tsconfig = fs::read_to_string(&tsconfig_path).unwrap_or_else(|_| "<missing>".into());

    let gen_path = baml_src.join(GENERATED_BAML_PRELUDE_FILE);
    let generated_tools = if gen_path.exists() {
        fs::read_to_string(&gen_path).unwrap()
    } else {
        String::from("<not generated>")
    };

    let catalog_path = baml_src.join(baml_rt_tools::TOOL_SCHEMA_CATALOG_SIDECAR_FILE);
    let catalog_text = if catalog_path.exists() {
        fs::read_to_string(&catalog_path).unwrap()
    } else {
        String::from("<not rendered>")
    };

    format!(
        r#"=== manifest.json ===
{manifest_pretty}

=== baml_src/{}_prompt.baml ===
{prompt}

=== src/index.ts ===
{index_ts}

=== src/baml-runtime.d.ts ===
{d_ts}

=== tsconfig.json ===
{tsconfig}

=== baml_src/{gen_tools_name} ===
{generated_tools}

=== baml_src/{catalog_name} ===
{catalog_text}
"#,
        prompt_name,
        gen_tools_name = GENERATED_BAML_PRELUDE_FILE,
        catalog_name = baml_rt_tools::TOOL_SCHEMA_CATALOG_SIDECAR_FILE,
    )
}

#[tokio::test]
async fn bootstrap_no_tools_artifacts_snapshot() {
    let root = TempDir::new().unwrap();
    let root_path = root.path();

    run_bootstrap(
        root_path,
        "Explorer Agent",
        "Test description for explorer",
        &[],
    )
    .await
    .expect("run_bootstrap should succeed");

    let artifacts = collect_artifacts(root_path);
    insta::assert_snapshot!("bootstrap_no_tools_artifacts", artifacts);
}

#[tokio::test]
async fn bootstrap_with_tools_artifacts_snapshot() {
    let root = TempDir::new().unwrap();
    let root_path = root.path();

    let tools = vec!["support/calculate".to_string()];
    run_bootstrap(
        root_path,
        "Calculator Agent",
        "Uses support/calculate",
        &tools,
    )
    .await
    .expect("run_bootstrap should succeed");

    let artifacts = collect_artifacts(root_path);
    insta::assert_snapshot!("bootstrap_with_tools_artifacts", artifacts);
}

#[tokio::test]
async fn bootstrap_no_tools_creates_layout_and_files() {
    let root = TempDir::new().unwrap();
    let root_path = root.path();

    let result = run_bootstrap(
        root_path,
        "Explorer Agent",
        "Test description for explorer",
        &[],
    )
    .await;

    assert!(
        result.is_ok(),
        "run_bootstrap should succeed: {:?}",
        result.err()
    );

    assert!(root_path.join("manifest.json").exists());
    assert!(root_path.join("baml_src").is_dir());
    assert!(root_path.join("src").is_dir());
    assert!(
        root_path
            .join("baml_src")
            .join("explorer_agent_prompt.baml")
            .exists()
    );
    assert!(root_path.join("src").join("index.ts").exists());
    assert!(root_path.join("src").join("baml-runtime.d.ts").exists());
    assert!(root_path.join("tsconfig.json").exists());
}

#[tokio::test]
async fn bootstrap_with_tools_creates_generated_tools_and_manifest_tools() {
    let root = TempDir::new().unwrap();
    let root_path = root.path();

    let tools = vec!["support/calculate".to_string()];
    let result = run_bootstrap(
        root_path,
        "Calculator Agent",
        "Uses support/calculate",
        &tools,
    )
    .await;

    assert!(
        result.is_ok(),
        "run_bootstrap should succeed: {:?}",
        result.err()
    );

    let manifest_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root_path.join("manifest.json")).unwrap())
            .unwrap();
    let tools_arr = manifest_json["tools"].as_array().unwrap();
    assert_eq!(tools_arr.len(), 1);
    assert_eq!(tools_arr[0], "support/calculate");

    let gen_path = root_path.join("baml_src").join(GENERATED_BAML_PRELUDE_FILE);
    assert!(
        gen_path.exists(),
        "{} should exist when tools selected",
        GENERATED_BAML_PRELUDE_FILE
    );
    let gen_content = fs::read_to_string(&gen_path).unwrap();
    assert!(gen_content.contains("SupportCalculate"));
    assert!(gen_content.contains("SessionPlan"));
}

#[tokio::test]
async fn bootstrap_rejects_empty_name_slug() {
    let root = TempDir::new().unwrap();
    let result = run_bootstrap(root.path(), "   ", "desc", &[]).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("alphanumeric") || msg.contains("Name"));
}

#[tokio::test]
async fn bootstrap_rejects_non_empty_existing_dir() {
    let root = TempDir::new().unwrap();
    let p = root.path();
    fs::write(p.join("existing.txt"), "x").unwrap();
    let result = run_bootstrap(p, "MyAgent", "desc", &[]).await;
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("non-empty") || msg.contains("exists"));
}

#[test]
fn slug_from_name_exploratory() {
    assert_eq!(slug_from_name("Voidship Rites"), "voidship-rites");
    assert_eq!(slug_from_name("tony"), "tony");
    assert_eq!(slug_from_name("My Agent 99"), "my-agent-99");
    assert_eq!(slug_from_name("single"), "single");
    assert_eq!(slug_from_name("UPPER"), "upper");
}
