//! Process-wide registry for [`CallbackStore`](baml_rt_core::CallbackStore).
//!
//! Types and the trait live in **`baml-rt-core`** (`callback_store` module). This file only holds
//! the `OnceLock` slot and install/clear helpers. The **`baml-agent-runner`** installs an
//! `Arc<dyn CallbackStore>` backed by `DeploymentStateStore` at startup.

use std::sync::{Arc, OnceLock, RwLock};

pub use baml_rt_core::callback_store::{
    CallbackStore, CancelCallbackSelector, ScheduleCallbackRequest, ScheduleCallbackResult,
    StoredCallback,
};
use tracing::{error, warn};

fn callback_store_slot() -> &'static RwLock<Option<Arc<dyn CallbackStore>>> {
    static STORE: OnceLock<RwLock<Option<Arc<dyn CallbackStore>>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(None))
}

pub fn install_callback_store(store: Arc<dyn CallbackStore>) {
    let mut guard = callback_store_slot().write().unwrap_or_else(|poisoned| {
        error!("callback store registry write lock poisoned; recovering inner state");
        poisoned.into_inner()
    });
    if guard.is_some() {
        warn!(
            "callback store replaced; pending callbacks in the previous store may be unreachable"
        );
    }
    *guard = Some(store);
}

pub fn callback_store() -> Option<Arc<dyn CallbackStore>> {
    callback_store_slot()
        .read()
        .unwrap_or_else(|poisoned| {
            error!("callback store registry read lock poisoned; recovering inner state");
            poisoned.into_inner()
        })
        .clone()
}

pub fn require_callback_store() -> baml_rt_core::Result<Arc<dyn CallbackStore>> {
    callback_store().ok_or_else(baml_rt_core::callback_store_not_installed)
}

pub fn clear_callback_store() {
    let mut guard = callback_store_slot().write().unwrap_or_else(|poisoned| {
        error!("callback store registry clear lock poisoned; recovering inner state");
        poisoned.into_inner()
    });
    *guard = None;
}
