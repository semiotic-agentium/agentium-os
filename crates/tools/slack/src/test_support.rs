use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use baml_rt_core::{
    Result,
    ingress_store::{IngressId, IngressItem, IngressStore},
};
use baml_rt_tools::ingress_store::{clear_ingress_store, install_ingress_store};
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub struct MemoryIngressStore {
    rows: Arc<Mutex<HashMap<IngressId, MemoryIngressRow>>>,
}

#[derive(Clone)]
struct MemoryIngressRow {
    item: IngressItem,
    delivered: bool,
    emitted_at_unix_ms: Option<u64>,
}

#[async_trait]
impl IngressStore for MemoryIngressStore {
    async fn enqueue(&self, item: &IngressItem) -> Result<bool> {
        let mut rows = self.rows.lock().await;
        if rows.contains_key(&item.ingress_id) {
            return Ok(false);
        }
        rows.insert(
            item.ingress_id.clone(),
            MemoryIngressRow {
                item: item.clone(),
                delivered: false,
                emitted_at_unix_ms: None,
            },
        );
        Ok(true)
    }

    async fn list_pending(&self, limit: usize) -> Result<Vec<IngressItem>> {
        let mut rows = self
            .rows
            .lock()
            .await
            .values()
            .filter(|row| !row.delivered && row.emitted_at_unix_ms.is_none())
            .cloned()
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            left.item
                .enqueued_at_unix_ms
                .cmp(&right.item.enqueued_at_unix_ms)
                .then_with(|| left.item.ingress_id.cmp(&right.item.ingress_id))
        });
        Ok(rows.into_iter().take(limit).map(|row| row.item).collect())
    }

    async fn requeue_stale(&self, emitted_before_unix_ms: u64) -> Result<usize> {
        let mut rows = self.rows.lock().await;
        let mut reclaimed = 0;
        for row in rows.values_mut() {
            if row.delivered {
                continue;
            }
            if row
                .emitted_at_unix_ms
                .is_some_and(|emitted_at| emitted_at <= emitted_before_unix_ms)
            {
                row.emitted_at_unix_ms = None;
                reclaimed += 1;
            }
        }
        Ok(reclaimed)
    }

    async fn mark_emitted(
        &self,
        ingress_ids: &[IngressId],
        emitted_at_unix_ms: u64,
    ) -> Result<Vec<IngressId>> {
        let mut rows = self.rows.lock().await;
        let mut eligible = Vec::new();
        for ingress_id in ingress_ids {
            let Some(row) = rows.get_mut(ingress_id) else {
                continue;
            };
            if row.delivered || row.emitted_at_unix_ms.is_some() {
                continue;
            }
            row.emitted_at_unix_ms = Some(emitted_at_unix_ms);
            eligible.push(ingress_id.clone());
        }
        Ok(eligible)
    }

    async fn mark_delivered(
        &self,
        ingress_ids: &[IngressId],
        _delivered_at_unix_ms: u64,
    ) -> Result<()> {
        let mut rows = self.rows.lock().await;
        for ingress_id in ingress_ids {
            if let Some(row) = rows.get_mut(ingress_id) {
                row.delivered = true;
            }
        }
        Ok(())
    }
}

impl MemoryIngressStore {
    pub async fn pending_count(&self) -> usize {
        self.rows
            .lock()
            .await
            .values()
            .filter(|row| !row.delivered)
            .count()
    }

    pub async fn pending_items(&self) -> Vec<IngressItem> {
        let rows = self.rows.lock().await;
        rows.values()
            .filter(|row| !row.delivered)
            .map(|row| row.item.clone())
            .collect()
    }

    pub async fn set_emitted_at(&self, ingress_id: &IngressId, emitted_at_unix_ms: u64) {
        let mut rows = self.rows.lock().await;
        let row = rows
            .get_mut(ingress_id)
            .expect("ingress row should exist in test store");
        row.emitted_at_unix_ms = Some(emitted_at_unix_ms);
    }
}

pub struct IngressStoreGuard;

impl Drop for IngressStoreGuard {
    fn drop(&mut self) {
        clear_ingress_store();
    }
}

pub fn install_memory_ingress_store() -> (IngressStoreGuard, Arc<MemoryIngressStore>) {
    clear_ingress_store();
    let store = Arc::new(MemoryIngressStore::default());
    install_ingress_store(store.clone() as Arc<dyn IngressStore>);
    (IngressStoreGuard, store)
}
