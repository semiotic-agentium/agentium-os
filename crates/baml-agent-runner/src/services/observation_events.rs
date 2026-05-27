<<<<<<< HEAD:crates/baml-agent-runner/src/services/conversation_history_events.rs
// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Conversation-history event service: notifies SSE subscribers when the transcript may have changed.
//!
//! Notifications fire (1) **after successful provenance commits** (runner wraps the graph writer)
//! and (2) on **A2A task
//! updates** (e.g. task-only status transitions that affect resume hints without new graph rows).
=======
//! Observation event service: SSE subscribers for unified operator bundles.
>>>>>>> 1fb3d596 (feat: unify operator observation with typed ops and planning index):crates/baml-agent-runner/src/services/observation_events.rs

use std::sync::Arc;

use baml_rt_core::{ObservationUpdate, observation::kinds};
use tokio::{sync::broadcast, task::JoinHandle};

use crate::runner::AgentRunner;

pub(crate) struct ObservationEventServiceImpl {
    tx: broadcast::Sender<ObservationUpdate>,
    _tasks: Vec<JoinHandle<()>>,
}

impl ObservationEventServiceImpl {
    pub(crate) fn new(runner: Arc<AgentRunner>, tx: broadcast::Sender<ObservationUpdate>) -> Self {
        let mut handles = Vec::new();
        for mut updates in runner.subscribe_task_update_receivers() {
            let tx_clone = tx.clone();
            handles.push(tokio::spawn(async move {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(620);
                loop {
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    match updates.recv().await {
                        Ok(event) => {
                            let context_id = event
                                .context_id()
                                .map(|id| id.as_str().to_string())
                                .unwrap_or_default();
                            if context_id.is_empty() {
                                continue;
                            }
                            let update = ObservationUpdate {
                                context_id,
                                task_id: event.task_id().map(|id| id.to_string()),
                                kinds: kinds::TRANSCRIPT | kinds::OPS,
                            };
                            if let Err(e) = tx_clone.send(update.clone()) {
                                tracing::warn!(
                                    error = ?e,
                                    context_id = %update.context_id,
                                    "observation notify send failed"
                                );
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }));
        }
        Self {
            tx,
            _tasks: handles,
        }
    }
}

impl baml_rt_api::ObservationEventService for ObservationEventServiceImpl {
    fn subscribe_updates(&self) -> broadcast::Receiver<ObservationUpdate> {
        self.tx.subscribe()
    }
}
