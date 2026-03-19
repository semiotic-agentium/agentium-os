//! Interactive prompts for the CLI.
//!
//! Uses `inquire` to provide a guided experience when arguments are missing.

use anyhow::{Result, bail};
use baml_rt_tools::{InventoryCatalog, ToolCatalog};
use inquire::{MultiSelect, Select, Text};

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
    let catalog = InventoryCatalog::new();
    let mut tools: Vec<_> = catalog.iter().collect();
    tools.sort_by_key(|t| t.name.to_string());

    if tools.is_empty() {
        println!("No tools found in inventory.");
        return Ok(None);
    }

    let options: Vec<ToolOption> = tools
        .iter()
        .map(|t| ToolOption {
            id: t.name.to_string(),
            description: truncate(&t.description, 45),
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

/// Truncate a string to a maximum length.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

// ---------------------------------------------------------------------------
// Confirmation prompts
// ---------------------------------------------------------------------------

/// Prompt for confirmation before proceeding.
/// Returns true if user confirms, false otherwise.
pub fn confirm_proceed() -> Result<bool> {
    let confirmed = inquire::Confirm::new("Proceed?")
        .with_default(true)
        .prompt()?;

    Ok(confirmed)
}
