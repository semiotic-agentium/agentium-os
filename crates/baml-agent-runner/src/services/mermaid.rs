// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

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
        let t0 = std::time::Instant::now();
        let graph = self.export_by_context(context_id).await?;
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        let t1 = std::time::Instant::now();
        let simplified = simplify_graph(&graph);
        let t2 = std::time::Instant::now();
        let mermaid = render_sequence_diagram(&simplified);
        tracing::debug!(
            context_id = %context_id,
            export_ms = t0.elapsed().as_millis(),
            simplify_ms = t1.elapsed().as_millis(),
            render_ms = t2.elapsed().as_millis(),
            graph_nodes = graph.nodes.len(),
            graph_edges = graph.edges.len(),
            simplified_nodes = simplified.nodes.len(),
            simplified_edges = simplified.edges.len(),
            bytes = mermaid.len(),
            "mermaid context export"
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
        if graph.nodes.is_empty() {
            return Err(baml_rt_api::MermaidError::NotFound);
        }
        let t1 = std::time::Instant::now();
        let simplified = simplify_graph(&graph);
        let t2 = std::time::Instant::now();
        let mermaid = render_sequence_diagram(&simplified);
        tracing::debug!(
            task_id = %task_id,
            export_ms = t0.elapsed().as_millis(),
            simplify_ms = t1.elapsed().as_millis(),
            render_ms = t2.elapsed().as_millis(),
            graph_nodes = graph.nodes.len(),
            graph_edges = graph.edges.len(),
            simplified_nodes = simplified.nodes.len(),
            simplified_edges = simplified.edges.len(),
            bytes = mermaid.len(),
            "mermaid task export"
        );
        Ok(mermaid)
    }
}
