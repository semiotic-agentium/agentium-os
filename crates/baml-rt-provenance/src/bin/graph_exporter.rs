use anyhow::{Context, bail};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

use baml_rt_provenance::graph_export::dot::{DotOptions, render_dot};
use baml_rt_provenance::graph_export::sequence::render_sequence_diagram;
use baml_rt_provenance::graph_export::simplify::simplify_graph;
use baml_rt_provenance::{GraphExporter, GraphqliteProvenanceStore, GraphqliteStoreBuilder};
use clap::ValueEnum;

#[derive(Parser)]
#[command(
    name = "graph_exporter",
    about = "Export provenance graphs from GraphQLite as Mermaid, DOT, or JSON"
)]
struct Cli {
    /// Path to GraphQLite database (or ":memory:" for in-memory).
    #[arg(long, default_value = "provenance.db")]
    db: PathBuf,

    /// Export by context_id.
    #[arg(long, group = "scope")]
    context_id: Option<String>,

    /// Export by task_id.
    #[arg(long, group = "scope")]
    task_id: Option<String>,

    /// Output format (defaults to mermaid).
    #[arg(long, value_enum, default_value_t = OutputFormat::Mermaid)]
    format: OutputFormat,

    /// Show edge labels (mermaid/dot).
    #[arg(long, default_value_t = true)]
    edge_labels: bool,

    /// Group nodes by type into subgraphs (mermaid/dot).
    #[arg(long)]
    group: bool,

    /// Simplify the graph before rendering.
    #[arg(long)]
    simplify: bool,

    /// Write output to file instead of stdout.
    #[arg(short, long)]
    output: Option<String>,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Mermaid,
    Dot,
    Json,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.db.to_string_lossy() == ":memory:" {
        bail!(
            "--db :memory: creates a new empty in-memory DB in this process. \
Use the same file path passed to baml-agent-runner --provenance-db <path>."
        );
    }

    let scope = cli
        .context_id
        .as_ref()
        .map(|c| ("context_id", c.as_str()))
        .or_else(|| cli.task_id.as_ref().map(|t| ("task_id", t.as_str())));
    let (scope_name, scope_value) =
        scope.context("Must specify either --context-id or --task-id")?;

    let store: Arc<GraphqliteProvenanceStore> = GraphqliteStoreBuilder::file(&cli.db).build()?;
    let exporter = GraphExporter::new(store);

    eprintln!(
        "Exporting {scope_name}={scope_value} from {}",
        cli.db.display()
    );
    let graph = match scope_name {
        "context_id" => exporter.export_by_context(scope_value).await?,
        _ => exporter.export_by_task(scope_value).await?,
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
        OutputFormat::Json => serde_json::to_string_pretty(&graph).context("JSON serialization")?,
    };

    if let Some(path) = &cli.output {
        std::fs::write(path, &rendered).with_context(|| format!("Failed to write to {path}"))?;
        eprintln!("Wrote {} bytes to {path}", rendered.len());
    } else {
        println!("{rendered}");
    }

    Ok(())
}
