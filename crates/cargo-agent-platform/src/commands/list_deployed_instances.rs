//! `list-deployed-instances` subcommand — list currently loaded runner instances.

use anyhow::Result;
use console::style;

use super::{
    agent_discovery::AgentDiscoveryEntry,
    utils::{AgentPlatform, HTTP_OP_LIST_DEPLOYED_INSTANCES, join_url},
};

pub struct ListDeployedInstancesOutput {
    pub entries: Vec<AgentDiscoveryEntry>,
    pub agents_url: String,
}

impl AgentPlatform {
    pub fn list_deployed_instances(&self, base_url: &str) -> Result<ListDeployedInstancesOutput> {
        let agents_url = join_url(base_url, "/agents");
        let entries: Vec<AgentDiscoveryEntry> =
            self.get_json(&agents_url, HTTP_OP_LIST_DEPLOYED_INSTANCES)?;

        Ok(ListDeployedInstancesOutput {
            entries,
            agents_url,
        })
    }
}

pub fn list_deployed_instances(base_url: &str) -> Result<ListDeployedInstancesOutput> {
    let http = AgentPlatform::new()?;
    http.list_deployed_instances(base_url)
}

pub fn run(base_url: &str) -> Result<()> {
    let output = list_deployed_instances(base_url)?;

    if output.entries.is_empty() {
        println!("{}", style("No deployed instances found.").yellow().bold());
        println!("  url: {}", style(output.agents_url).dim());
        return Ok(());
    }

    println!("{}", style("Deployed instances").bold());
    for entry in output.entries {
        println!(
            "- {}  package={} instance={}",
            style(entry.agent_card.name).cyan(),
            entry.agent_card.agent_package,
            entry.agent_card.agent_instance_id
        );
    }
    println!("  url: {}", style(output.agents_url).dim());

    Ok(())
}
