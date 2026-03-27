//! Episode snapshot service.

use std::sync::Arc;

pub(crate) struct EpisodeServiceImpl {
    store: Arc<baml_rt_provenance::SurrealProvenanceStore>,
}

impl EpisodeServiceImpl {
    pub(crate) fn new(store: Arc<baml_rt_provenance::SurrealProvenanceStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl baml_rt_api::EpisodeService for EpisodeServiceImpl {
    async fn episode_snapshot(
        &self,
        task_id: &str,
    ) -> std::result::Result<baml_rt_api::EpisodeSnapshotDto, baml_rt_api::EpisodeError> {
        use baml_rt_core::ids::{ExternalId, TaskId};
        use baml_rt_provenance::EpisodeReader;

        let task_id_typed = TaskId::from_external(ExternalId::new(task_id.to_string()));
        let reader = EpisodeReader::new(Arc::clone(&self.store));
        match reader.read_snapshot_by_task_id(&task_id_typed).await {
            Ok(episode) => Ok(episode.into()),
            Err(baml_rt_provenance::ProvenanceError::InvalidEvent { .. }) => {
                Err(baml_rt_api::EpisodeError::NotFound)
            }
            Err(e) => Err(baml_rt_api::EpisodeError::Other(Box::new(e))),
        }
    }

    async fn episode_text(
        &self,
        task_id: &str,
    ) -> std::result::Result<String, baml_rt_api::EpisodeError> {
        use baml_rt_core::ids::{ExternalId, TaskId};
        use baml_rt_provenance::{EpisodeReader, render_episode};

        let task_id_typed = TaskId::from_external(ExternalId::new(task_id.to_string()));
        let reader = EpisodeReader::new(Arc::clone(&self.store));
        match reader.read_snapshot_by_task_id(&task_id_typed).await {
            Ok(episode) => Ok(render_episode(&episode)),
            Err(baml_rt_provenance::ProvenanceError::InvalidEvent { .. }) => {
                Err(baml_rt_api::EpisodeError::NotFound)
            }
            Err(e) => Err(baml_rt_api::EpisodeError::Other(Box::new(e))),
        }
    }
}
