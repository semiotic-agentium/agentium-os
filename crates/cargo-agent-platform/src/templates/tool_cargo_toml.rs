//! Template for generating tool Cargo.toml files.

/// Generate the Cargo.toml content for a new tool crate.
///
/// # Arguments
/// * `name` - Tool name in kebab-case (e.g., "github")
/// * `description` - Tool description
pub fn generate(name: &str, description: &str) -> String {
    format!(
        r#"[package]
name = "baml-tools-{name}"
version = {{ workspace = true }}
edition = {{ workspace = true }}
authors = {{ workspace = true }}
publish = false
description = "{description}"

[dependencies]
baml-rt-core = {{ path = "../../baml-rt-core" }}
baml-rt-tools = {{ path = "../../baml-rt-tools" }}
baml-derive-core = {{ path = "../../baml-derive-core" }}
baml-derive = {{ path = "../../baml-derive" }}
serde = {{ workspace = true }}
async-trait = {{ workspace = true }}
schemars = {{ workspace = true }}
ts-rs = {{ workspace = true }}
thiserror = {{ workspace = true }}
inventory = {{ workspace = true }}
"#,
        name = name,
        description = description
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_cargo_toml() {
        let content = generate("github", "GitHub integration tool for BAML runtime");
        assert!(content.contains("name = \"baml-tools-github\""));
        assert!(content.contains("GitHub integration tool"));
        assert!(content.contains("baml-rt-core"));
        assert!(content.contains("baml-rt-tools"));
    }
}
