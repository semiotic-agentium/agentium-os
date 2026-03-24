//! Template for generating tool lib.rs files.

/// Generate the lib.rs content for a new tool crate.
///
/// # Arguments
/// * `name` - Tool name in kebab-case (e.g., "github")
/// * `bundle` - Bundle type (e.g., "support")
/// * `access` - Access level (e.g., "read", "write")
pub fn generate(name: &str, bundle: &str, access: &str) -> String {
    let pascal_name = to_pascal_case(name);
    let snake_name = to_snake_case(name);
    let upper_name = name.to_uppercase().replace('-', "_");
    let bundle_type = to_pascal_case(bundle);

    let access_attr = match access {
        "write" => "Write",
        _ => "Read",
    };

    format!(
        r#"//! {pascal_name} tool — `{bundle}/{snake_name}`.

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{{BamlRtError, Result}};
use baml_rt_tools::{{baml_tool, bundles::{bundle_type}, tools::BamlTool}};
use serde::{{Deserialize, Serialize}};

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// Primary input for the {pascal_name} tool.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(deny_unknown_fields)]
pub struct {pascal_name}Input {{
    /// TODO: Define your input fields
    pub query: String,
}}

impl baml_rt_tools::DescribeAction for {pascal_name}Input {{
    fn describe(&self) -> String {{
        format!("query='{{}}'", self.query)
    }}
}}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Output returned by the {pascal_name} tool.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct {pascal_name}Output {{
    pub message: String,
}}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum {pascal_name}Error {{
    #[error("{pascal_name} operation failed: {{message}}")]
    Operation {{ message: String }},
}}

impl From<{pascal_name}Error> for BamlRtError {{
    fn from(err: {pascal_name}Error) -> Self {{
        BamlRtError::ToolExecution(err.to_string())
    }}
}}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

pub struct {pascal_name}Tool;

impl Default for {pascal_name}Tool {{
    fn default() -> Self {{
        Self
    }}
}}

#[baml_tool(
    name = "{bundle}/{snake_name}",
    description = "TODO: Describe what this tool does.",
    tags = ["{bundle}", "{snake_name}"],
    access = {access_attr},
    // Uncomment and fill in if this tool requires API keys:
    // secrets = [
    //     {{ name = "{upper_name}_API_KEY", description = "{pascal_name} API token", reason = "Required to authenticate" }}
    // ],
    baml_types = [{pascal_name}Input, {pascal_name}Output],
)]
#[async_trait]
impl BamlTool for {pascal_name}Tool {{
    type Bundle = {bundle_type};
    const LOCAL_NAME: &'static str = "{snake_name}";
    type OpenInput = ();
    type Input = {pascal_name}Input;
    type Output = {pascal_name}Output;

    fn description(&self) -> &'static str {{
        "TODO: Describe what this tool does."
    }}

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {{
        // TODO: Implement tool logic
        Ok({pascal_name}Output {{
            message: format!("Executed with query: {{}}", args.query),
        }})
    }}
}}
"#,
        pascal_name = pascal_name,
        snake_name = snake_name,
        upper_name = upper_name,
        bundle = bundle,
        bundle_type = bundle_type,
        access_attr = access_attr,
    )
}

/// Convert kebab-case to PascalCase.
fn to_pascal_case(s: &str) -> String {
    s.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect()
}

/// Convert kebab-case to snake_case.
fn to_snake_case(s: &str) -> String {
    s.replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("github"), "Github");
        assert_eq!(to_pascal_case("my-tool"), "MyTool");
        assert_eq!(to_pascal_case("some-long-name"), "SomeLongName");
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("github"), "github");
        assert_eq!(to_snake_case("my-tool"), "my_tool");
    }

    #[test]
    fn test_generate_lib_rs() {
        let content = generate("github", "support", "read");
        assert!(content.contains("GithubTool"));
        assert!(content.contains("support/github"));
        assert!(content.contains("struct GithubInput"));
        assert!(content.contains("struct GithubOutput"));
    }

    #[test]
    fn test_generate_lib_rs_with_write_access() {
        let content = generate("github", "support", "write");
        assert!(content.contains("access = Write,"));
    }
}
