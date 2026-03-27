//! Mermaid sequence diagram service.

use std::sync::Arc;

use baml_rt_provenance::{
    GraphExporter,
    graph_export::{sequence::render_sequence_diagram, simplify::simplify_graph},
};

pub(crate) struct MermaidServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
    cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
}

impl MermaidServiceImpl {
    pub(crate) fn new(
        store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
        cache: Option<Arc<baml_rt_provenance::MermaidCache>>,
    ) -> Self {
        Self { store, cache }
    }

    async fn export_by_context(
        &self,
        context_id: &str,
    ) -> std::result::Result<baml_rt_provenance::ExportedGraph, baml_rt_api::MermaidError> {
        let exporter = GraphExporter::new(self.store.clone());
        exporter
            .export_by_context(context_id)
            .await
            .map_err(|e| baml_rt_api::MermaidError::Other(Box::new(e)))
    }

    async fn export_by_task(
        &self,
        task_id: &str,
    ) -> std::result::Result<baml_rt_provenance::ExportedGraph, baml_rt_api::MermaidError> {
        let exporter = GraphExporter::new(self.store.clone());
        exporter
            .export_by_task(task_id)
            .await
            .map_err(|e| baml_rt_api::MermaidError::Other(Box::new(e)))
    }
}

#[async_trait::async_trait]
impl baml_rt_api::MermaidService for MermaidServiceImpl {
    async fn mermaid_for_context(
        &self,
        context_id: &str,
    ) -> std::result::Result<String, baml_rt_api::MermaidError> {
        if let Some(ref cache) = self.cache
            && let Some(cached) = cache.get(context_id)
        {
            tracing::debug!(context_id = %context_id, "mermaid: cache HIT");
            return Ok(cached);
        }
        tracing::info!(context_id = %context_id, "mermaid: START export_by_context");
        let t0 = std::time::Instant::now();
        let graph = self.export_by_context(context_id).await?;
        tracing::info!(
            context_id = %context_id, export_ms = t0.elapsed().as_millis(),
            nodes = graph.nodes.len(), edges = graph.edges.len(),
            "mermaid: DONE export_by_context"
        );
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        let t1 = std::time::Instant::now();
        let simplified = simplify_graph(&graph);
        tracing::info!(
            context_id = %context_id, simplify_ms = t1.elapsed().as_millis(),
            nodes = simplified.nodes.len(), edges = simplified.edges.len(),
            "mermaid: DONE simplify_graph"
        );
        let t2 = std::time::Instant::now();
        let mermaid = render_sequence_diagram(&simplified);
        tracing::info!(
            context_id = %context_id, render_ms = t2.elapsed().as_millis(),
            bytes = mermaid.len(), "mermaid: DONE render_sequence_diagram"
        );
        if let Some(ref cache) = self.cache {
            cache.insert(context_id, mermaid.clone());
        }
        Ok(mermaid)
    }

    async fn mermaid_for_task(
        &self,
        task_id: &str,
    ) -> std::result::Result<String, baml_rt_api::MermaidError> {
        let t0 = std::time::Instant::now();
        let graph = self.export_by_task(task_id).await?;
        tracing::info!(
            task_id = %task_id, export_ms = t0.elapsed().as_millis(),
            nodes = graph.nodes.len(), edges = graph.edges.len(),
            "mermaid: DONE export_by_task"
        );
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        let t1 = std::time::Instant::now();
        let simplified = simplify_graph(&graph);
        tracing::info!(
            task_id = %task_id, simplify_ms = t1.elapsed().as_millis(),
            nodes = simplified.nodes.len(), edges = simplified.edges.len(),
            "mermaid: DONE simplify_graph"
        );
        let t2 = std::time::Instant::now();
        let mermaid = render_sequence_diagram(&simplified);
        tracing::info!(
            task_id = %task_id, render_ms = t2.elapsed().as_millis(),
            bytes = mermaid.len(), "mermaid: DONE render_sequence_diagram"
        );
        Ok(mermaid)
    }
}
