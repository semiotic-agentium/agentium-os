use anyhow::Context;
use clap::{Parser, ValueEnum};

use baml_rt_provenance::FalkorDbProvenanceConfig;
use baml_rt_provenance::graph_export::GraphExporter;
use baml_rt_provenance::graph_export::dot::{DotOptions, render_dot};
use baml_rt_provenance::graph_export::json::render_json;
use baml_rt_provenance::graph_export::sequence::render_sequence_diagram;
use baml_rt_provenance::graph_export::simplify::simplify_graph;

#[derive(Parser)]
#[command(
    name = "graph_exporter",
    about = "Export provenance graphs from FalkorDB as Mermaid, DOT, or JSON"
)]
struct Cli {
    /// FalkorDB connection string.
    #[arg(long, default_value = "falkor://127.0.0.1:6379")]
    connection: String,

    /// FalkorDB graph name.
    #[arg(long, default_value = "baml_prov")]
    graph: String,

    /// Export by context_id.
    #[arg(long, group = "scope")]
    context_id: Option<String>,

    /// Export by task_id.
    #[arg(long, group = "scope")]
    task_id: Option<String>,

    /// Export the full graph (no scope filter).
    #[arg(long, group = "scope")]
    full: bool,

    /// Output format (defaults to mermaid).
    #[arg(long, value_enum, default_value_t = OutputFormat::Mermaid)]
    format: OutputFormat,

    /// Graph direction (mermaid only): td, lr, bt, rl.
    #[arg(long, default_value = "td")]
    direction: String,

    /// Show edge labels (mermaid/dot).
    #[arg(long, default_value_t = true)]
    edge_labels: bool,

    /// Group nodes by type into subgraphs (mermaid/dot).
    #[arg(long)]
    group: bool,

    /// Simplify the graph by collapsing start/complete pairs, removing
    /// LlmPrompt nodes, and keeping only send-complete FSM tool phases.
    #[arg(long)]
    simplify: bool,

    /// Write output to file instead of stdout.
    #[arg(short, long)]
    output: Option<String>,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    /// Mermaid sequence diagram (temporal narrative view).
    Mermaid,
    Dot,
    Json,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = FalkorDbProvenanceConfig::new(&cli.connection, &cli.graph);
    let exporter = GraphExporter::new(config);

    let graph = if let Some(ctx) = &cli.context_id {
        eprintln!(
            "Exporting context_id={ctx} from {}/{}",
            cli.connection, cli.graph
        );
        exporter
            .export_by_context(ctx)
            .await
            .context("export_by_context failed")?
    } else if let Some(tid) = &cli.task_id {
        eprintln!(
            "Exporting task_id={tid} from {}/{}",
            cli.connection, cli.graph
        );
        exporter
            .export_by_task(tid)
            .await
            .context("export_by_task failed")?
    } else {
        eprintln!("Exporting full graph from {}/{}", cli.connection, cli.graph);
        exporter.export_full().await.context("export_full failed")?
    };

    eprintln!(
        "Exported {} nodes and {} edges",
        graph.nodes.len(),
        graph.edges.len()
    );

    let graph = if cli.simplify {
        let simplified = simplify_graph(&graph);
        eprintln!(
            "Simplified to {} nodes and {} edges",
            simplified.nodes.len(),
            simplified.edges.len()
        );
        simplified
    } else {
        graph
    };

    let rendered = match cli.format {
        OutputFormat::Mermaid => render_sequence_diagram(&graph),
        OutputFormat::Dot => render_dot(
            &graph,
            &DotOptions {
                show_edge_labels: cli.edge_labels,
                cluster_by_type: cli.group,
                ..DotOptions::default()
            },
        ),
        OutputFormat::Json => render_json(&graph, true).context("JSON serialization failed")?,
    };

    if let Some(path) = &cli.output {
        std::fs::write(path, &rendered).with_context(|| format!("Failed to write to {path}"))?;
        eprintln!("Wrote {} bytes to {path}", rendered.len());
    } else {
        println!("{rendered}");
    }

    Ok(())
}
