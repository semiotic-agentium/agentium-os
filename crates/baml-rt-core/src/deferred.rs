//! Deferred wiring: holder for late-initialized trait objects.
//!
//! Prefer construction-time injection (e.g. typestate builder with concrete providers)
//! so dependencies are never unset. This type remains for compatibility; `get()` errors if not set.

use std::sync::{Arc, RwLock};

use crate::{BamlRtError, Result};

/// Holder for a trait object that is set after initialization.
/// `get()` returns an error if not yet set. Prefer injecting concrete implementations at build time.
#[derive(Debug, Default)]
pub struct DeferredHolder<T: ?Sized> {
    inner: RwLock<Option<Arc<T>>>,
}

impl<T: ?Sized> DeferredHolder<T> {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    pub fn set(&self, value: Arc<T>) {
        *self
            .inner
            .write()
            .expect("RwLock poison: prior panic while holding lock (unrecoverable)") = Some(value);
    }

    pub fn get(&self) -> Result<Arc<T>> {
        self.inner
            .read()
            .expect("RwLock poison: prior panic while holding lock (unrecoverable)")
            .clone()
            .ok_or_else(|| {
                BamlRtError::InvalidArgument(
                    "DeferredHolder not set (host must call set before use)".to_string(),
                )
            })
    }
}
