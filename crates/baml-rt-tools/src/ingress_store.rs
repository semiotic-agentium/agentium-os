//! Process-wide registry for [`IngressStore`](baml_rt_core::IngressStore).
//!
//! Types and the trait live in **`baml-rt-core`** (`ingress_store` module). This file only holds
//! the `OnceLock` slot and install/clear helpers. The **`baml-agent-runner`** installs an
//! `Arc<dyn IngressStore>` backed by `DeploymentStateStore` at startup.

use std::sync::{Arc, OnceLock, RwLock};

pub use baml_rt_core::ingress_store::{IngressId, IngressItem, IngressStore};
use tracing::{error, warn};

fn ingress_store_slot() -> &'static RwLock<Option<Arc<dyn IngressStore>>> {
    static STORE: OnceLock<RwLock<Option<Arc<dyn IngressStore>>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(None))
}

pub fn install_ingress_store(store: Arc<dyn IngressStore>) {
    let mut guard = ingress_store_slot().write().unwrap_or_else(|poisoned| {
        error!("ingress store registry write lock poisoned; recovering inner state");
        poisoned.into_inner()
    });
    if guard.is_some() {
        warn!(
            "ingress store replaced; pending ingress rows in the previous store may be unreachable"
        );
    }
    *guard = Some(store);
}

pub fn ingress_store() -> Option<Arc<dyn IngressStore>> {
    ingress_store_slot()
        .read()
        .unwrap_or_else(|poisoned| {
            error!("ingress store registry read lock poisoned; recovering inner state");
            poisoned.into_inner()
        })
        .clone()
}

pub fn require_ingress_store() -> baml_rt_core::Result<Arc<dyn IngressStore>> {
    ingress_store().ok_or_else(baml_rt_core::ingress_store_not_installed)
}

pub fn clear_ingress_store() {
    let mut guard = ingress_store_slot().write().unwrap_or_else(|poisoned| {
        error!("ingress store registry clear lock poisoned; recovering inner state");
        poisoned.into_inner()
    });
    *guard = None;
}
