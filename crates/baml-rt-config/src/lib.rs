//! Tool configuration storage and resolution.
//!
//! Provides ConfigReader/ConfigWriter traits and SQLite-backed implementation
//! for versioned tool config with provenance linkage.

mod error;
mod store;
mod traits;

pub use error::ConfigStoreError;
pub use store::SqliteConfigStore;
pub use traits::{
    ConfigReader, ConfigService, ConfigVersion, ConfigVersionNumber, ConfigWriter,
    InternalConfigReader, InternalConfigWriter, StoredConfig, UnixMs,
};
