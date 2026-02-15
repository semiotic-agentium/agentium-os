//! Agent builder module
//!
//! Provides production-grade abstractions for building, linting, and packaging
//! BAML agent applications.
//!
//! **Tool catalogue:** Tool list composition for bootstrap is done in the binary,
//! not in the lib. See [`bootstrap`].

pub mod a2a_shim_gen;
pub mod baml_gen;
pub mod baml_signature_gen;
pub mod bootstrap;
pub mod compiler;
pub mod filesystem;
pub mod ir_to_ts;
pub mod linter;
pub mod packager;
pub mod schema_to_baml;
pub mod service;
pub mod traits;
pub mod ts_gen;
pub mod types;

pub use compiler::{OxcTypeScriptCompiler, RuntimeTypeGenerator};
pub use filesystem::StdFileSystem;
pub use linter::OxcLinter;
pub use packager::StdPackager;
pub use service::BuilderService;
pub use traits::{FileSystem, Linter, Packager, TypeGenerator, TypeScriptCompiler};
pub use types::{AgentDir, BuildDir, FunctionName, PackagePath};
