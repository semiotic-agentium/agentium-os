// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tool configuration storage and resolution.
//!
//! Provides ConfigReader/ConfigWriter traits and SurrealDB-backed implementation
//! for versioned tool config with provenance linkage.

mod error;
mod store;
mod traits;

pub use error::ConfigStoreError;
pub use store::SurrealConfigStore;
pub use traits::{
    ConfigReader, ConfigService, ConfigVersion, ConfigVersionNumber, ConfigWriter,
    InternalConfigReader, InternalConfigWriter, StoredConfig, UnixMs,
};
