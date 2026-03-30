use std::sync::{Arc, OnceLock, RwLock};

use async_trait::async_trait;
use baml_rt_core::Result;
use tracing::{error, warn};

use crate::callback_store::StoredCallback;

/// Host-installed gate for deciding whether a due callback may be emitted now.
///
/// This is intentionally optional. Hosts that do not install a gate get the
/// default behavior: every due callback is eligible for delivery immediately.
#[async_trait]
pub trait CallbackDeliveryGate: Send + Sync {
    /// Return true when the callback may be emitted on this poll cycle.
    async fn can_emit_callback(&self, callback: &StoredCallback) -> Result<bool>;
}

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
