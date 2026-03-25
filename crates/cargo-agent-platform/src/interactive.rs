//! Interactive prompts for the CLI.
//!
//! Uses `inquire` to provide a guided experience when arguments are missing.

use anyhow::{Result, bail};
use baml_rt_tools::{InventoryCatalog, ToolCatalog};
use inquire::{Confirm, MultiSelect, Select, Text};

use crate::{text::truncate_for_display, tool_catalog::load_cli_tools_for_picker};

/// Known schema versions for event delivery.
///
/// NOTE: Hardcoded for now. The task-daemon is currently the only event producer,
/// so `task-daemon.interpretation.v1` is the only known schema version.
/// When adding new event producers, add their schema versions here.
pub const KNOWN_SCHEMA_VERSIONS: &[&str] = &["task-daemon.interpretation.v1"];

/// Common source kinds that agents typically subscribe to.
/// These are suggested even if no tools currently declare them as event_sources.
const COMMON_SOURCE_KINDS: &[&str] = &["slack", "clickup", "github_issues"];

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

/// Access level options for new-tool.
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
        .with_help_message("Only 'support' is currently available")
        .prompt()?;

    Ok(selected.value.to_string())
}

/// Prompt for access level.
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

/// Prompt for tool description.
pub fn prompt_tool_description() -> Result<String> {
    let description = Text::new("Description (optional):")
        .with_help_message("Human-readable description shown in tool discovery/list")
        .prompt()?;

    Ok(description.trim().to_string())
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
}

impl std::fmt::Display for SchemaOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value)
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
            "Event subscriptions allow the agent to receive dispatched events from sources like Slack, ClickUp",
        )
        .prompt()?;

    if !wants_events {
        return Ok(None);
    }

    // Select schema versions
    let schema_options: Vec<SchemaOption> = KNOWN_SCHEMA_VERSIONS
        .iter()
        .map(|s| SchemaOption {
            value: s.to_string(),
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

    // Add common source kinds that might not be in tools yet
    for common in COMMON_SOURCE_KINDS {
        if !seen_sources.contains(*common) {
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
