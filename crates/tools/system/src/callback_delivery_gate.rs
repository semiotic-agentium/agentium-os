//! Process-wide registry for [`CallbackDeliveryGate`](baml_rt_core::CallbackDeliveryGate).
//!
//! The trait and [`StoredCallback`](baml_rt_core::StoredCallback) live in **`baml-rt-core`**;
//! **`baml-agent-runner`** installs a gate implementation after the agent map is built.

use std::sync::{Arc, OnceLock, RwLock};

pub use baml_rt_core::callback_store::CallbackDeliveryGate;
use tracing::{error, warn};

fn callback_delivery_gate_slot() -> &'static RwLock<Option<Arc<dyn CallbackDeliveryGate>>> {
    static GATE: OnceLock<RwLock<Option<Arc<dyn CallbackDeliveryGate>>>> = OnceLock::new();
    GATE.get_or_init(|| RwLock::new(None))
}

pub fn install_callback_delivery_gate(gate: Arc<dyn CallbackDeliveryGate>) {
    let mut guard = callback_delivery_gate_slot()
        .write()
        .unwrap_or_else(|poisoned| {
            error!("callback delivery gate registry write lock poisoned; recovering inner state");
            poisoned.into_inner()
        });
    if guard.is_some() {
        warn!("callback delivery gate replaced; in-flight callback delivery policy changed");
    }
    *guard = Some(gate);
}

pub fn callback_delivery_gate() -> Option<Arc<dyn CallbackDeliveryGate>> {
    callback_delivery_gate_slot()
        .read()
        .unwrap_or_else(|poisoned| {
            error!("callback delivery gate registry read lock poisoned; recovering inner state");
            poisoned.into_inner()
        })
        .clone()
}

pub fn clear_callback_delivery_gate() {
    let mut guard = callback_delivery_gate_slot()
        .write()
        .unwrap_or_else(|poisoned| {
            error!("callback delivery gate registry clear lock poisoned; recovering inner state");
            poisoned.into_inner()
        });
    *guard = None;
}
