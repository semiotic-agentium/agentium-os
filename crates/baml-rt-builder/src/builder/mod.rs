//! Agent builder module
//!
//! Provides production-grade abstractions for building, linting, and packaging
//! BAML agent applications.
//!
//! **Tool catalogue:** Tool list composition for bootstrap is done in the binary,
//! not in the lib. See [`bootstrap`].

pub mod types;
pub mod traits;
pub mod filesystem;
pub mod linter;
pub mod compiler;
pub mod ts_gen;
pub mod baml_gen;
pub mod schema_to_baml;
pub mod packager;
pub mod service;
pub mod bootstrap;

pub use types::{AgentDir, PackagePath, FunctionName, BuildDir};
pub use traits::{
    Linter, TypeScriptCompiler, TypeGenerator, FileSystem, Packager
};
pub use filesystem::StdFileSystem;
pub use linter::OxcLinter;
pub use compiler::{OxcTypeScriptCompiler, RuntimeTypeGenerator};
pub use packager::StdPackager;
pub use service::BuilderService;
