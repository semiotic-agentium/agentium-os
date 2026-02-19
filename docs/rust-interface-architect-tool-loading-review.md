# Rust Interface Architect — Tool Loading & Discovery Review (rev. 2)

*Re-invocation after design changes: single inventory, access policy, manifest boundary, and **global discovery**.*

---

## 1. Current state (implemented)

**Access policy (DU, no unrestricted path)**
- `ToolAccessPolicy` enum: `PermitOnly(HashSet<ToolAccess>)`. No bare `Option`; all tools are gated.
- `parse_access_allowlist()` → `ToolAccessPolicy` (default when env unset: permit all).
- `enforce_tool_access(tool_name, policy: &ToolAccessPolicy)` always runs the check.

**Manifest boundary**
- `ManifestToolNames(Vec<ToolName>)` with `ManifestToolNames::parse(&[String]) -> Result<Self>`.
- `register_manifest_tools(registry, tool_names: &ManifestToolNames, policy: &ToolAccessPolicy)`.

**System-tool predicate**
- `is_system_host_tool(name: &ToolName) -> bool` (bundle `"system"` + local in `["internal_a2a", "discover_agents", "discover_tools"]`).
- Used in `register_manifest_tools` to skip building from inventory; host registers `SystemBundle` separately.

**Build error preservation**
- `(provider.build)().map_err(|e| BamlRtError::InvalidArgumentWithSource { message, source: Box::new(e) })`.

**Discovery is global**
- `ToolRegistry::search_tools(query, limit)` always searches the **whole catalog** (`tool_catalog::all_tool_metadata()`).
- Discovery lists **globally available tools** only; there is no per-agent invokability flag (tool lists are fixed in compiled BAML prompts).
- Single implementation: `tool_discovery::search_tools(metadata_list, query, limit)`.

**Runner / builder**
- Runner and builder use `ToolAccessPolicy`, `ManifestToolNames`, and `register_manifest_tools` at the boundary.

---

## 2. Findings (boundaries and remaining tension)

**Catalog vs. registration coupling (unchanged)**
`ToolProvider` still bundles type-level metadata and a runtime build. System tools use a no-op build that returns `Err`; they remain in the inventory for catalog/metadata but are never built from it. The type does not distinguish “metadata-only” from “metadata + build.” Acceptable as-is; the predicate `is_system_host_tool` cleanly skips them at registration.

**Registry allowlist remains `Option<HashSet<ToolName>>`**
The registry’s internal `allowlist: Option<HashSet<ToolName>>` still means “if `Some`, only these tools may be registered/invoked.” This is the **manifest** allowlist (which tools this agent may use), not the access policy. Semantics: `None` = no allowlist (any registered tool may be invoked); `Some(set)` = only tools in the set. If the design intent is “every agent must have an explicit allowlist,” this could be a newtype or DU (e.g. `ManifestAllowlist(HashSet<ToolName>)`) and `None` removed so that “no tools” is `ManifestAllowlist(HashSet::new())`. **Recommendation:** document current meaning; consider a DU only if product requires “no implicit allow-all.”

**Discovery vs. registry**
Discovery is global: the registry’s `search_tools` reads from the inventory (whole catalog). “What exists” = inventory; “what this agent may call” = registry + allowlist at invocation time. No invokability in discovery output.

**ToolCatalog iterator**
`ToolCatalog::iter` still returns `Box<dyn Iterator<...>>`. Optional refinement: use an associated type for the iterator to avoid boxing if the trait is only used generically. Low priority.

**Public surface**
- `ToolProvider` and its fields remain `pub` for the macro used from other crates.
- `search_tools(metadata_list, query, limit)` is the single public discovery API.

---

## 3. Proposed traits / types (optional next steps)

**Manifest allowlist DU (optional)**
If “no implicit allow-all” is required:

```rust
/// Which tools this agent may register/invoke. No "unrestricted" path.
pub enum ManifestAllowlist {
    Only(HashSet<ToolName>),
}
```

- Registry would take `ManifestAllowlist` instead of `Option<HashSet<ToolName>>`; “allow all” would be explicit (e.g. a variant or a full set from the catalog). Not implemented; document as optional.

**ToolCatalog::Iter associated type (optional)**
- `type Iter<'a>: Iterator<Item = &'a ToolFunctionMetadata> where Self: 'a;`
- `fn iter(&self) -> Self::Iter<'_>`
- Removes boxing for `InventoryCatalog`. Small, non-urgent.

---

## 4. Rationale (why the current design is stronger)

- **Access policy:** “All tools gated” is in the type; no silent skip.
- **ManifestToolNames:** Invalid names fail at parse; host registration receives only `ToolName`s.
- **System predicate:** One named rule; adding a system tool is “extend predicate or bundle” in one place.
- **Build errors:** Failures keep a chain via `InvalidArgumentWithSource`.
- **Global discovery:** “What can exist” (inventory) is separate from “what this agent can call” (registry + allowlist); discovery always shows the whole catalog; `invocable_in_session` encodes current agent capability.

---

## 5. Risks (breaking changes, feature regressions)

- **Registry allowlist:** If a DU is introduced for the manifest allowlist, all call sites of `set_allowlist` / `set_allowlist_from_strings` and any code that checks `if allowlist.is_some()` must be updated.
- **ToolCatalog::Iter:** Changing the trait breaks implementors; currently only `InventoryCatalog` implements it.

---

## 6. Test adjustments (if optional steps are taken)

- **ManifestAllowlist:** Tests that set an allowlist would construct `ManifestAllowlist::Only(set)`; “no allowlist” would no longer exist unless encoded as an explicit variant/set.
- **ToolCatalog::Iter:** Any code that types the iterator would use the new associated type.

---

*End of review (rev. 2). Design reflects: single inventory, ToolAccessPolicy, ManifestToolNames, system predicate, preserved build error, and global discovery with invocable flag. Implement optional steps only if the Fabricator sanctions.*
