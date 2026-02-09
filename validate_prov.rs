use serde_json::Value;
use text_to_cypher::core::execute_cypher_query;

const CONNECTION: &str = "falkor://127.0.0.1:6379";
const GRAPH: &str = "baml_prov";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!("=== Validating Provenance Graph ===\n");

    // 1. Count all nodes by type
    println!("1. Node counts by type:");
    let query = r#"
        MATCH (n)
        RETURN labels(n)[0] as type, count(*) as count
        ORDER BY count DESC
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 2. Count all relationships by type
    println!("2. Relationship counts by type:");
    let query = r#"
        MATCH ()-[r]->()
        RETURN type(r) as rel_type, count(*) as count
        ORDER BY count DESC
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 3. Find disconnected nodes
    println!("3. Disconnected nodes (no edges):");
    let query = r#"
        MATCH (n)
        WHERE NOT (n)--()
        RETURN labels(n)[0] as type, n.name as name
        LIMIT 20
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 4. Check agent nodes - should have agent_id
    println!("4. Agent nodes (checking for agent_id):");
    let query = r#"
        MATCH (n:Agent)
        RETURN n.name as name,
               n.`a2a:agent_id` as agent_id,
               n.`a2a:agent_type` as agent_type,
               keys(n) as all_keys
        LIMIT 20
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 5. Check task entities - should have agent_id
    println!("5. Task entities (checking for agent_id):");
    let query = r#"
        MATCH (n:A2ATask)
        RETURN n.name as name, n.`a2a:task_id` as task_id, n.`a2a:agent_id` as agent_id
        LIMIT 20
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 6. Find LLM calls without agent runtime instance associations
    println!("6. LLM calls without executing agent association:");
    let query = r#"
        MATCH (llm:LlmCall)
        WHERE NOT (llm)-[:WAS_EXECUTED_BY]->(:AgentRuntimeInstance)
        RETURN llm.name as name, llm.`a2a:function_name` as function_name
        LIMIT 20
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 7. Find tool calls without agent runtime instance associations
    println!("7. Tool calls without executing agent association:");
    let query = r#"
        MATCH (tool:ToolCall)
        WHERE NOT (tool)-[:WAS_EXECUTED_BY]->(:AgentRuntimeInstance)
        RETURN tool.name as name, tool.`a2a:tool_name` as tool_name
        LIMIT 20
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 8. Check task executions - should be associated with agent runtime instances
    println!("8. Task executions and their agent associations:");
    let query = r#"
        MATCH (task_exec:A2ATaskExecution)
        OPTIONAL MATCH (task_exec)-[assoc:WAS_EXECUTED_BY|WAS_INVOKED_BY|WAS_CALLED_BY]->(agent:AgentRuntimeInstance)
        RETURN task_exec.name as task_exec_id,
               task_exec.`a2a:task_id` as task_id,
               collect(DISTINCT {agent_id: agent.`a2a:agent_id`, role: assoc.`prov:role`}) as agents
        LIMIT 10
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 9. Check for AgentBooted events - should have archive, boot activity, instance
    println!("9. Agent boot activities (checking boot chain):");
    let query = r#"
        MATCH (boot)
        WHERE boot.`prov:type` = "a2a:AgentBoot"
        OPTIONAL MATCH (archive:AgentArchive)-[r1]->(boot)
        OPTIONAL MATCH (instance:AgentRuntimeInstance)-[r2]->(boot)
        OPTIONAL MATCH (boot)-[r3:WAS_EXECUTED_BY]->(runner:AgentRuntimeInstance)
        RETURN boot.name as boot_id,
               boot.`a2a:agent_id` as agent_id,
               type(r1) as archive_rel,
               archive.name as archive_id,
               type(r2) as instance_rel,
               instance.name as instance_id,
               type(r3) as runner_rel,
               runner.name as runner_id
        LIMIT 10
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 9b. Check all activities for AgentBoot
    println!("9b. All activities with AgentBoot type:");
    let query = r#"
        MATCH (activity)
        WHERE activity.`prov:type` = "a2a:AgentBoot" OR activity.`a2a:agent_id` IS NOT NULL
        RETURN labels(activity) as labels, activity.`prov:type` as prov_type, activity.name as name
        LIMIT 20
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 10. Find activities without agent runtime instance associations
    println!("10. Activities without any agent association:");
    let query = r#"
        MATCH (activity)
        WHERE 'ProvActivity' IN labels(activity)
        AND NOT (activity)-[:WAS_EXECUTED_BY|WAS_INVOKED_BY|WAS_CALLED_BY]->(:AgentRuntimeInstance)
        RETURN labels(activity)[0] as activity_type, activity.name as name
        LIMIT 20
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 11. Check message processing - should have agent runtime instance associations
    println!("11. Message processing activities and agents:");
    let query = r#"
        MATCH (msg_proc:A2AMessageProcessing)
        OPTIONAL MATCH (msg_proc)-[assoc:WAS_EXECUTED_BY|WAS_INVOKED_BY|WAS_CALLED_BY]->(agent:AgentRuntimeInstance)
        RETURN msg_proc.name as msg_proc_id,
               msg_proc.`a2a:message_id` as message_id,
               collect(DISTINCT {agent_id: agent.`a2a:agent_id`, role: assoc.`prov:role`}) as agents
        LIMIT 10
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 12. Context connectivity (basic) - check for nodes with no edges by context
    println!("12. Context connectivity (basic edge presence):");
    let query = r#"
        MATCH (n)
        WHERE n.`a2a:context_id` IS NOT NULL
        WITH n.`a2a:context_id` as context_id, collect(DISTINCT n) as nodes
        UNWIND nodes as node
        WITH context_id, node, size(nodes) as node_count,
             CASE WHEN (node)--() THEN 1 ELSE 0 END as has_edge
        WITH context_id, node_count, sum(has_edge) as connected_nodes
        RETURN context_id, node_count, connected_nodes,
               CASE WHEN connected_nodes = node_count THEN 'NO_ISOLATED' ELSE 'HAS_ISOLATED' END as status
        ORDER BY node_count DESC
        LIMIT 5
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    // 13. Raw PROV edges that were not derived into semantic labels
    println!("13. Raw PROV edges (USED/WAS_GENERATED_BY):");
    let query = r#"
        MATCH ()-[r]->()
        WHERE type(r) IN ["USED", "WAS_GENERATED_BY"]
        RETURN type(r) as rel_type, count(*) as count
        ORDER BY count DESC
    "#;
    let result = execute_cypher_query(query, GRAPH, CONNECTION, false).await?;
    println!("{}", format_result(&result));
    println!();

    Ok(())
}

fn format_result(result: &str) -> String {
    match serde_json::from_str::<Value>(result) {
        Ok(json) => serde_json::to_string_pretty(&json).unwrap_or_else(|_| result.to_string()),
        Err(_) => result.to_string(),
    }
}
