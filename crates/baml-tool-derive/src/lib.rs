//! Procedural attribute macro for BAML tool registration.
//!
//! Eliminates the boilerplate of writing metadata functions, build functions,
//! and `register_tool!` calls for each tool.
//!
//! # Mode 1 — Full tool (has `impl BamlTool`)
//!
//! Place `#[baml_tool]` on the `impl BamlTool` block. The macro reads the
//! associated types (`OpenInput`, `Input`, `Output`) from the impl and
//! generates the metadata fn, build fn, and `register_tool!` call.
//!
//! ```rust,ignore
//! #[baml_tool(
//!     name = "support/clickup",
//!     description = "Interact with ClickUp: navigate workspaces and manage tasks.",
//!     tags = ["support", "clickup"],
//!     secrets = [
//!         { name = "CLICKUP_API_KEY", description = "ClickUp personal API token", reason = "Required to authenticate" }
//!     ],
//!     baml_types = [
//!         ListTeamsInput, ListSpacesInput, ClickUpInput,
//!         ClickUpTaskSummary, ClickUpItem, ClickUpOutput,
//!     ],
//! )]
//! #[async_trait]
//! impl BamlTool for ClickUpTool {
//!     type Bundle = Support;
//!     const LOCAL_NAME: &'static str = "clickup";
//!     type OpenInput = ();
//!     type Input = ClickUpInput;
//!     type Output = ClickUpOutput;
//!
//!     fn description(&self) -> &'static str { "..." }
//!     async fn execute(&self, args: Self::Input) -> Result<Self::Output> { todo!() }
//! }
//! ```
//!
//! The tool struct must implement `Default` (used by the generated build fn to
//! construct the instance). To override construction, use `build_with = my_fn`.
//!
//! # Mode 2 — Metadata-only (no runtime handler)
//!
//! For tools where the runtime handler is provided by a host bundle (e.g.
//! memory, system, claude tools), place `#[baml_tool]` on a unit struct with
//! the `metadata_only` flag. You must explicitly specify the type parameters.
//!
//! ```rust,ignore
//! #[baml_tool(
//!     name = "memory/add",
//!     description = "Store cognitive events with optional edges.",
//!     open_input = MemoryAddOpenInput,
//!     input = MemoryAddSendInput,
//!     output = MemoryAddNextOutput,
//!     tags = ["memory"],
//!     access = Write,
//!     metadata_only,
//! )]
//! struct MemoryAddTool;
//! ```
//!
//! The generated build fn returns `Err(...)` indicating the tool is metadata-only.
//!
//! # Attribute Parameters
//!
//! **Required** (both modes):
//! - `name = "bundle/local"` — qualified tool name
//! - `description = "..."` — tool description for LLM consumption
//!
//! **Required** (Mode 2 only):
//! - `open_input = Type` — the OpenInput type
//! - `input = Type` — the Input type
//! - `output = Type` — the Output type
//!
//! **Optional** (both modes):
//! - `tags = ["tag1", "tag2"]` — tool tags
//! - `secrets = [{ name = "...", description = "...", reason = "..." }]` — secret requirements
//! - `access = Read | Write | Delete` — access level
//! - `baml_types = [Type1, Type2, ...]` — types whose `baml_decl()` forms the BAML declaration
//! - `metadata_only` — flag that switches to Mode 2
//!
//! **Optional** (Mode 1 only):
//! - `build_with = path::to::fn` — override the default build function

mod expand;
mod parse;

use proc_macro::TokenStream;
use syn::{Item, parse_macro_input};

use parse::ToolAttrs;

/// Attribute macro for registering BAML tool metadata and handlers.
///
/// See the [crate-level documentation](crate) for usage examples.
#[proc_macro_attribute]
pub fn baml_tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = parse_macro_input!(attr as ToolAttrs);

    // Determine mode based on the item kind.
    let item_clone: proc_macro2::TokenStream = item.clone().into();
    let parsed: Item = match syn::parse(item) {
        Ok(item) => item,
        Err(err) => return err.to_compile_error().into(),
    };

    let result = match parsed {
        Item::Impl(impl_block) => expand::expand_impl(&attrs, &impl_block),
        Item::Struct(struct_item) => expand::expand_struct(&attrs, &struct_item),
        _ => Err(syn::Error::new_spanned(
            item_clone,
            "baml_tool: this attribute can only be applied to `impl BamlTool` blocks \
             or structs (with `metadata_only`)",
        )),
    };

    match result {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
