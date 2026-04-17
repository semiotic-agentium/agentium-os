//! Interactive prompts for the CLI.
//!
//! Uses `inquire` to provide a guided experience when arguments are missing.

use std::collections::BTreeSet;

use anyhow::{Result, bail};
use baml_rt_tools::{InventoryCatalog, ToolCatalog};
use inquire::{Confirm, MultiSelect, Select, Text};

use crate::{
    event_schemas::{KNOWN_COMPATIBILITY_SOURCE_KINDS, KNOWN_EVENT_SCHEMAS},
    text::truncate_for_display,
    tool_catalog::{load_cli_tools, load_cli_tools_for_picker},
};

/// Bundle type options for new-tool.
#[derive(Debug, Clone)]
pub struct BundleOption {
    pub value: &'static str,
    pub label: &'static str,
}

impl std::fmt::Display for BundleOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

/// Access level options for tool scaffolding.
#[derive(Debug, Clone)]
pub struct AccessOption {
    pub value: &'static str,
    pub label: &'static str,
}

impl std::fmt::Display for AccessOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

/// Runtime options for external tool metadata scaffold.
#[derive(Debug, Clone)]
pub struct ExternalToolRuntimeOption {
    pub value: &'static str,
    pub label: &'static str,
}

impl std::fmt::Display for ExternalToolRuntimeOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

/// Language options for new-tool (external scaffold).
#[derive(Debug, Clone)]
pub struct ExternalToolLanguageOption {
    pub value: &'static str,
    pub label: &'static str,
}

impl std::fmt::Display for ExternalToolLanguageOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

/// Template options for new-agent.
#[derive(Debug, Clone)]
pub struct TemplateOption {
    pub value: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

impl std::fmt::Display for TemplateOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} - {}", self.label, self.description)
    }
}

/// Tool option for multi-select in new-agent.
#[derive(Debug, Clone)]
pub struct ToolOption {
    pub id: String,
    pub description: String,
}

impl std::fmt::Display for ToolOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:<30} {}", self.id, self.description)
    }
}

// ---------------------------------------------------------------------------
// new-tool prompts
// ---------------------------------------------------------------------------

/// Prompt for tool name.
pub fn prompt_tool_name() -> Result<String> {
    let name = Text::new("Tool name (kebab-case):")
        .with_help_message("e.g., github, jira, linear")
        .prompt()?;

    if name.trim().is_empty() {
        bail!("Tool name cannot be empty");
    }

    Ok(name.trim().to_string())
}

/// Prompt for bundle type.
pub fn prompt_bundle() -> Result<String> {
    let options = vec![BundleOption {
        value: "support",
        label: "support (default) - Standard support tool",
    }];

    let selected = Select::new("Bundle type:", options)
        .with_help_message("Only 'support' is currently available for static tool scaffolding")
        .prompt()?;

    Ok(selected.value.to_string())
}

/// Prompt for external tool bundle namespace.
pub fn prompt_external_tool_bundle() -> Result<String> {
    let raw = Text::new("Bundle namespace:")
        .with_default("support")
        .with_help_message(
            "Any non-empty name without '/'. `support` is the default for integrations.",
        )
        .prompt()?;

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("Bundle name cannot be empty");
    }
    Ok(trimmed.to_string())
}

/// Prompt for static tool access level.
pub fn prompt_access() -> Result<String> {
    let options = vec![
        AccessOption {
            value: "read",
            label: "read (default) - Query-only, no side effects",
        },
        AccessOption {
            value: "write",
            label: "write - Can mutate (create, update, delete)",
        },
    ];

    let selected = Select::new("Access level:", options)
        .with_help_message("Choose the tool's permission level")
        .prompt()?;

    Ok(selected.value.to_string())
}

/// Prompt for external tool access level.
pub fn prompt_external_tool_access() -> Result<String> {
    let options = vec![
        AccessOption {
            value: "read",
            label: "read (default) - Query-only, no side effects",
        },
        AccessOption {
            value: "write",
            label: "write - Can create/update data",
        },
        AccessOption {
            value: "delete",
            label: "delete - Can remove data (strictest level)",
        },
    ];

    let selected = Select::new("Access level:", options)
        .with_help_message("Choose the external tool's permission level")
        .prompt()?;

    Ok(selected.value.to_string())
}

/// Prompt for tool description.
pub fn prompt_tool_description() -> Result<String> {
    let description = Text::new("Description (optional):")
        .with_help_message("Human-readable description shown in tool discovery/list")
        .prompt()?;

    Ok(description.trim().to_string())
}

/// Prompt for external tool runtime metadata kind.
pub fn prompt_external_tool_runtime() -> Result<String> {
    let options = vec![
        ExternalToolRuntimeOption {
            value: "process",
            label: "process (default) - local tool-server executable",
        },
        ExternalToolRuntimeOption {
            value: "sandbox",
            label: "sandbox - metadata targets microsandbox backend",
        },
    ];

    let selected = Select::new("Runtime:", options)
        .with_help_message("Choose runtime declaration written into tool-metadata.json")
        .prompt()?;

    Ok(selected.value.to_string())
}

/// Prompt for external tool language.
pub fn prompt_external_tool_language() -> Result<String> {
    let options = vec![
        ExternalToolLanguageOption {
            value: "rust",
            label: "rust (default) - Cargo project with src/main.rs",
        },
        ExternalToolLanguageOption {
            value: "bash",
            label: "bash - Single tool-server script (requires jq)",
        },
        ExternalToolLanguageOption {
            value: "python",
            label: "python - main.py + tool-server shim",
        },
        ExternalToolLanguageOption {
            value: "typescript",
            label: "typescript - src/main.ts compiled to dist/main.js",
        },
    ];

    let selected = Select::new("Language:", options)
        .with_help_message("Choose the scaffold language")
        .prompt()?;

    Ok(selected.value.to_string())
}

/// Prompt for sandbox image reference (`...@sha256:...`).
pub fn prompt_external_tool_sandbox_image() -> Result<String> {
    let image = Text::new("Sandbox image:")
        .with_help_message("Digest-pinned image ref, e.g. ghcr.io/org/tool@sha256:<64hex>")
        .prompt()?;
    let trimmed = image.trim();
    if trimmed.is_empty() {
        bail!("Sandbox image cannot be empty when runtime is sandbox");
    }
    Ok(trimmed.to_string())
}

/// Prompt for runtime identity digest (`sha256:<64hex>`).
pub fn prompt_external_tool_runtime_digest() -> Result<String> {
    let digest = Text::new("Runtime digest:")
        .with_help_message("Runtime identity digest, e.g. sha256:<64hex>")
        .prompt()?;
    let trimmed = digest.trim();
    if trimmed.is_empty() {
        bail!("Runtime digest cannot be empty when runtime is sandbox");
    }
    Ok(trimmed.to_string())
}

/// Prompt for optional sandbox entrypoint argv as comma-separated values.
pub fn prompt_external_tool_sandbox_entrypoint() -> Result<Vec<String>> {
    let raw = Text::new("Sandbox entrypoint (optional, comma-separated):")
        .with_help_message("Example: /app/tool-adapter or leave blank to use image default")
        .prompt()?;

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    Ok(trimmed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

/// Prompt for external tool output directory.
pub fn prompt_external_tool_output(default_dir: &str) -> Result<String> {
    let output = Text::new("Output directory:")
        .with_default(default_dir)
        .with_help_message("Directory to create the standalone external tool scaffold")
        .prompt()?;

    Ok(output.trim().to_string())
}

// ---------------------------------------------------------------------------
// new-agent prompts
// ---------------------------------------------------------------------------

/// Prompt for agent name.
pub fn prompt_agent_name() -> Result<String> {
    let name = Text::new("Agent name:")
        .with_help_message("e.g., github-agent, task-manager")
        .prompt()?;

    if name.trim().is_empty() {
        bail!("Agent name cannot be empty");
    }

    Ok(name.trim().to_string())
}

/// Prompt for agent description.
pub fn prompt_agent_description() -> Result<String> {
    let description = Text::new("Description (optional):")
        .with_help_message("Human-readable description for discovery")
        .prompt()?;

    Ok(description.trim().to_string())
}

const BANNED_SUGGESTED_TAGS: &[&str] = &["support", "read", "write", "system"];

/// Suggest agent tags based on selected tools.
pub fn suggest_agent_tags(selected_tools: &[String]) -> Result<Vec<String>> {
    let mut tags = BTreeSet::new();

    let tools = load_cli_tools()?;
    for tool_id in selected_tools {
        if let Some(tool) = tools.iter().find(|t| t.id == *tool_id) {
            for tag in &tool.tags {
                let normalized = normalize_tag(tag);
                if !normalized.is_empty() && !is_banned_suggested_tag(&normalized) {
                    tags.insert(normalized);
                }
            }
        }
    }

    Ok(tags.into_iter().collect())
}

/// Prompt for agent tags.
pub fn prompt_agent_tags(suggested_tags: &[String]) -> Result<Option<String>> {
    let default_tags = suggested_tags.join(",");
    let tags = Text::new("Tags (optional, comma-separated):")
        .with_default(&default_tags)
        .with_help_message("e.g., support,clickup,prod")
        .prompt()?;

    let trimmed = tags.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn normalize_tag(tag: &str) -> String {
    tag.trim().to_ascii_lowercase().replace(' ', "-")
}

fn is_banned_suggested_tag(tag: &str) -> bool {
    BANNED_SUGGESTED_TAGS.contains(&tag)
}

/// Prompt for agent template.
pub fn prompt_template() -> Result<String> {
    let options = vec![
        TemplateOption {
            value: "simple",
            label: "simple",
            description: "Basic agent without tools (Q&A, chatbot)",
        },
        TemplateOption {
            value: "basic-tools",
            label: "basic-tools",
            description: "Agent with tool support",
        },
        TemplateOption {
            value: "planner",
            label: "planner",
            description: "3-phase: Intent -> Plan -> Execute",
        },
        TemplateOption {
            value: "coordinator",
            label: "coordinator",
            description: "Multi-agent delegator/router",
        },
    ];

    let selected = Select::new("Template:", options)
        .with_help_message("Choose the agent architecture pattern")
        .prompt()?;

    Ok(selected.value.to_string())
}

/// Prompt for tool selection (multi-select from inventory).
pub fn prompt_tools() -> Result<Option<String>> {
    let tools = load_cli_tools_for_picker()?;

    if tools.is_empty() {
        println!("No tools found.");
        return Ok(None);
    }

    let options: Vec<ToolOption> = tools
        .iter()
        .map(|t| ToolOption {
            id: t.id.to_string(),
            description: truncate_for_display(&t.description, 45),
        })
        .collect();

    let selected = MultiSelect::new("Select tools (Space to select, Enter to confirm):", options)
        .with_help_message("Choose which tools this agent can use")
        .prompt()?;

    if selected.is_empty() {
        Ok(None)
    } else {
        let ids: Vec<String> = selected.into_iter().map(|t| t.id).collect();
        Ok(Some(ids.join(",")))
    }
}

// ---------------------------------------------------------------------------
// Subscription prompts
// ---------------------------------------------------------------------------

/// Schema version option for selection.
#[derive(Debug, Clone)]
pub struct SchemaOption {
    pub value: String,
    pub description: &'static str,
}

impl std::fmt::Display for SchemaOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:<32} {}", self.value, self.description)
    }
}

/// Source kind option for selection.
#[derive(Debug, Clone)]
pub struct SourceKindOption {
    pub value: String,
    pub from_tool: Option<String>,
}

impl std::fmt::Display for SourceKindOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.from_tool {
            Some(tool) => write!(f, "{:<20} (from {})", self.value, tool),
            None => write!(f, "{:<20} (common)", self.value),
        }
    }
}

/// Prompt for event subscriptions.
///
/// Returns the subscription string in CLI format if the user wants subscriptions,
/// or None if they don't want to receive events.
pub fn prompt_subscriptions(selected_tools: &[String]) -> Result<Option<String>> {
    // Ask if they want to receive events
    let wants_events = Confirm::new("Does this agent need to receive events?")
        .with_default(false)
        .with_help_message(
            "Event subscriptions allow the agent to receive dispatched events from raw host ingress, task-daemon, or system/callback",
        )
        .prompt()?;

    if !wants_events {
        return Ok(None);
    }

    // Select schema versions
    let schema_options: Vec<SchemaOption> = KNOWN_EVENT_SCHEMAS
        .iter()
        .map(|schema| SchemaOption {
            value: schema.version.to_string(),
            description: schema.description,
        })
        .collect();

    let selected_schemas = MultiSelect::new(
        "Select schema versions to subscribe to:",
        schema_options,
    )
    .with_help_message(
        "Schema versions define the event payload format. Space to select, Enter to confirm.",
    )
    .prompt()?;

    if selected_schemas.is_empty() {
        println!("No schemas selected, skipping subscriptions.");
        return Ok(None);
    }

    // Collect available source kinds from selected tools + catalog
    let catalog = InventoryCatalog::new();
    let mut source_options: Vec<SourceKindOption> = Vec::new();
    let mut seen_sources: std::collections::HashSet<String> = std::collections::HashSet::new();

    // First, add sources from tools that declare event_sources
    for tool in catalog.iter() {
        // Check if this tool is selected or if it declares event sources
        let is_selected = selected_tools.iter().any(|t| t == &tool.name.to_string());
        if is_selected || !tool.event_sources.is_empty() {
            for source in &tool.event_sources {
                let source_str = source.as_str().to_string();
                if !seen_sources.contains(&source_str) {
                    seen_sources.insert(source_str.clone());
                    source_options.push(SourceKindOption {
                        value: source_str,
                        from_tool: Some(tool.name.to_string()),
                    });
                }
            }
        }
    }

    // Add common source kinds that might not be in tools yet.
    // This keeps the interactive surface aligned with task-daemon compatibility
    // plus the host-native system/callback source.
    for common in KNOWN_COMPATIBILITY_SOURCE_KINDS
        .iter()
        .map(|source| source.kind)
        .chain(std::iter::once("system/callback"))
    {
        if !seen_sources.contains(common) {
            seen_sources.insert(common.to_string());
            source_options.push(SourceKindOption {
                value: common.to_string(),
                from_tool: None,
            });
        }
    }

    // Sort by value for consistent display
    source_options.sort_by(|a, b| a.value.cmp(&b.value));

    let selected_sources = MultiSelect::new("Select source kinds to subscribe to:", source_options)
        .with_help_message(
            "Source kinds filter which event producers this agent receives from. Space to select, Enter to confirm.",
        )
        .prompt()?;

    if selected_sources.is_empty() {
        println!("No sources selected, skipping subscriptions.");
        return Ok(None);
    }

    // Build subscription string in CLI format: "schema=<version>,sources=<kind1,kind2>"
    let schema_str = selected_schemas
        .iter()
        .map(|s| s.value.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let sources_str = selected_sources
        .iter()
        .map(|s| s.value.as_str())
        .collect::<Vec<_>>()
        .join(",");

    Ok(Some(format!(
        "schema={},sources={}",
        schema_str, sources_str
    )))
}

// ---------------------------------------------------------------------------
// Confirmation prompts
// ---------------------------------------------------------------------------

/// Prompt for confirmation before proceeding.
/// Returns true if user confirms, false otherwise.
pub fn confirm_proceed() -> Result<bool> {
    let confirmed = Confirm::new("Proceed?").with_default(true).prompt()?;

    Ok(confirmed)
}
