use std::{path::PathBuf, sync::Arc, time::Instant};

use anyhow::{Context, bail};
use baml_rt_provenance::{
    GraphExporter, GraphqliteProvenanceStore, GraphqliteStoreBuilder,
    graph_export::{
        dot::{DotOptions, render_dot},
        sequence::render_sequence_diagram,
        simplify::simplify_graph,
    },
};
use clap::{Parser, ValueEnum};

#[derive(Parser)]
#[command(
    name = "graph_exporter",
    about = "Export provenance graphs from GraphQLite as Mermaid, DOT, or JSON"
)]
struct Cli {
    /// Path to GraphQLite database (or ":memory:" for in-memory).
    #[arg(long, default_value = "provenance.db")]
    db: PathBuf,

    /// List distinct context IDs and exit (no export). Does not require --context-id or --task-id.
    #[arg(long)]
    list_contexts: bool,

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

    /// Print timing breakdown for each pipeline phase (export, simplify, render).
    #[arg(long)]
    profile: bool,

    /// Diagnose: count total graph size, time main Cypher vs parse vs boot chain. Explains slowness.
    #[arg(long)]
    diagnose: bool,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Mermaid,
    Dot,
    Json,
}

fn print_diagnose(graph: &baml_rt_provenance::ExportedGraph, scope_name: &str, export_ms: u128) {
    eprintln!("[diagnose] export_by_{scope_name}: {export_ms}ms");
    eprintln!("[diagnose] result: {} nodes, {} edges", graph.nodes.len(), graph.edges.len());
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

    let store: Arc<GraphqliteProvenanceStore> = GraphqliteStoreBuilder::file(&cli.db).build()?;
    let exporter = GraphExporter::new(store.clone());

    if cli.list_contexts {
        let ids = exporter.list_contexts().await?;
        for id in ids {
            println!("{id}");
        }
        return Ok(());
    }

    let scope = cli
        .context_id
        .as_ref()
        .map(|c| ("context_id", c.as_str()))
        .or_else(|| cli.task_id.as_ref().map(|t| ("task_id", t.as_str())));
    let (scope_name, scope_value) =
        scope.context("Must specify either --context-id or --task-id (or use --list-contexts)")?;

    eprintln!(
        "Exporting {scope_name}={scope_value} from {}",
        cli.db.display()
    );

    let t0 = Instant::now();
    let graph = match scope_name {
        "context_id" => exporter.export_by_context(scope_value).await?,
        _ => exporter.export_by_task(scope_value).await?,
    };
    let export_ms = t0.elapsed().as_millis();

    if cli.diagnose {
        print_diagnose(&graph, scope_name, export_ms);
    }
    if cli.profile {
        eprintln!("[profile] export_by_{scope_name}: {export_ms}ms ({} nodes, {} edges)", graph.nodes.len(), graph.edges.len());
    }

    eprintln!(
        "Exported {} nodes and {} edges",
        graph.nodes.len(),
        graph.edges.len()
    );

    let t1 = Instant::now();
    let graph = if cli.simplify {
        let simplified = simplify_graph(&graph);
        if cli.profile {
            eprintln!("[profile] simplify_graph: {}ms ({} nodes, {} edges)", t1.elapsed().as_millis(), simplified.nodes.len(), simplified.edges.len());
        }
        eprintln!(
            "Simplified to {} nodes and {} edges",
            simplified.nodes.len(),
            simplified.edges.len()
        );
        simplified
    } else {
        if cli.profile {
            eprintln!("[profile] simplify_graph: 0ms (skipped)");
        }
        graph
    };

    let t2 = Instant::now();
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
    if cli.profile {
        eprintln!("[profile] render ({}): {}ms ({} bytes)", match cli.format { OutputFormat::Mermaid => "mermaid", OutputFormat::Dot => "dot", OutputFormat::Json => "json" }, t2.elapsed().as_millis(), rendered.len());
        eprintln!("[profile] total: {}ms", t0.elapsed().as_millis());
    }

    if let Some(path) = &cli.output {
        std::fs::write(path, &rendered).with_context(|| format!("Failed to write to {path}"))?;
        eprintln!("Wrote {} bytes to {path}", rendered.len());
    } else {
        println!("{rendered}");
    }

    Ok(())
}
