//! Agent builder module
//!
//! Provides production-grade abstractions for building and packaging
//! BAML agent applications. TypeScript compilation and type checking
//! are delegated to `tsc`.
//!
//! **Tool catalogue:** Tool list composition for bootstrap is done in the binary,
//! not in the lib. See [`bootstrap`].

pub mod a2a_shim_gen;
pub mod baml_gen;
pub mod baml_signature_gen;
pub mod bootstrap;
pub mod compiler;
pub mod error;
pub mod filesystem;
pub mod ir_to_ts;
pub mod packager;
pub mod schema_to_baml;
pub mod service;
pub mod traits;
pub mod ts_gen;
pub mod types;

pub use compiler::{RuntimeTypeGenerator, TSCONFIG_JSON, TscCompiler, write_canonical_tsconfig};
pub use filesystem::StdFileSystem;
pub use packager::StdPackager;
pub use service::BuilderService;
pub use traits::{FileSystem, Packager, TypeGenerator, TypeScriptCompiler};
pub use types::{AgentDir, BuildDir, FunctionName, PackagePath};
