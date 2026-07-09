#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! `ToolKit` GTS integration.
//!
//! This crate bridges Rust types to the [Global Type System] (GTS) used by
//! Gears. It provides three things:
//!
//! 1. **Link-time inventory** of GTS Type Schemas and well-known Instances —
//!    collectors populated at link time via the `inventory` crate. Any crate
//!    in the process that uses the macros below contributes to the same
//!    global inventory. `types-registry` consumes the inventory at startup,
//!    so there is no per-gear registration code for entries known at
//!    compile time.
//! 2. **Thin proc-macro wrappers** that delegate to upstream `gts-macros`
//!    and additionally submit the corresponding inventory entry:
//!    - `#[gts_type_schema(...)]` — wraps `gts_macros::struct_to_gts_schema`.
//!    - `gts_instance!` — wraps `gts_macros::gts_instance!`.
//!    - `gts_instance_raw!` — wraps `gts_macros::gts_instance_raw!`.
//! 3. **Platform base types** — a small shipped set of GTS base Type Schemas
//!    ([`PluginV1`], [`AuthzPermissionV1`]) used across the platform.
//!
//! ## Adding a new entry
//!
//! For a new platform base Type Schema, put a `#[gts_type_schema(...)]`-annotated
//! struct into this crate and add `mod your_gear;` below. For a Type Schema
//! or Instance owned by a specific gear, use the same macros from that
//! gear's crate — the inventory is process-global, so entries land in
//! `types-registry` regardless of which crate declares them.
//!
//! [Global Type System]: https://github.com/hypernetix/gts-spec

pub mod permission;
pub mod plugin;

pub use permission::AuthzPermissionV1;
pub use plugin::PluginV1;

// Re-export GTS primitives used by the wrapper macros and downstream
// callers. Keeps `toolkit_gts::*` self-sufficient at the top level.
pub use gts::{GtsInstanceId, GtsSchema};

// Re-export `inventory` so the macro expansions can emit
// `<crate>::inventory::submit!` without requiring consumer crates to add
// `inventory` as a direct dep.
#[doc(hidden)]
pub use inventory;

// Re-export the companion proc-macros so consumers need only one crate dep.
pub use toolkit_gts_macros::{gts_instance, gts_instance_raw, gts_type_schema};

/// Hidden re-exports used by the `cf-gears-toolkit-gts-macros` proc-macro
/// expansions to reach the upstream construction macros without forcing
/// consumers to take a direct dependency on `gts-macros`.
#[doc(hidden)]
pub mod __private {
    pub use ::gts_macros::{
        gts_instance as upstream_gts_instance, gts_instance_raw as upstream_gts_instance_raw,
        struct_to_gts_schema as upstream_struct_to_gts_schema,
    };
}

/// Registration record for a GTS Type Schema contributed to the process-wide
/// inventory.
///
/// Each `#[gts_type_schema(...)]`-annotated type submits one of these via
/// `inventory::submit!` at macro-expansion time. The `schema_fn` lazily
/// invokes the macro-generated accessor (`gts_schema_with_refs_as_string`)
/// to produce the JSON Schema document on demand.
#[derive(Clone)]
pub struct InventoryTypeSchema {
    /// GTS Type Identifier (e.g. `gts.cf.toolkit.authz.permission.v1~`).
    pub type_id: &'static str,
    /// Lazy accessor returning the GTS Type Schema as a JSON string.
    pub schema_fn: fn() -> String,
}

/// Registration record for a well-known GTS Instance contributed to the
/// process-wide inventory.
///
/// Submitted by the `gts_instance! { ... }` macro. `type_id` is derived at
/// macro-expansion time from the last `~` in the full instance id.
#[derive(Clone)]
pub struct InventoryInstance {
    /// GTS Type Identifier the Instance conforms to (prefix of the full
    /// Instance Identifier up to and including the last `~`).
    pub type_id: &'static str,
    /// Full GTS Instance Identifier.
    pub instance_id: &'static str,
    /// Lazy accessor returning the Instance payload as JSON (with `id`
    /// auto-injected by the macro).
    ///
    /// Two emission paths populate this:
    /// - `gts_instance! { ... }` (typed) expands to
    ///   `serde_json::to_value(&Struct { ... }).expect(...)`, which panics
    ///   only if a custom `Serialize` impl in the struct's transitive
    ///   field types fails (e.g., a map with non-string keys). In practice
    ///   our base types — `AuthzPermissionV1`, `PluginV1<P>` — use only
    ///   primitive / string / `GtsInstanceId` fields, so the panic path is
    ///   unreachable.
    /// - `gts_instance_raw! { ... }` expands to `serde_json::json!(...)`
    ///   on a literal JSON object, which cannot panic at runtime.
    pub payload_fn: fn() -> serde_json::Value,
}

inventory::collect!(InventoryTypeSchema);
inventory::collect!(InventoryInstance);

/// Returns every GTS Type Schema declared via `#[gts_type_schema(...)]` in
/// any crate linked into the current process.
///
/// Source of truth for each Type Schema is its Rust struct via the
/// macro-generated `gts_schema_with_refs_as_string` accessor — no
/// hand-written JSON.
///
/// # Errors
///
/// Returns an error if any registered Type Schema accessor produces invalid
/// JSON. This should be impossible with a correctly-applied `#[gts_type_schema]`
/// macro and signals a macro regression.
pub fn all_inventory_type_schemas() -> anyhow::Result<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    for entry in inventory::iter::<InventoryTypeSchema> {
        let schema_str = (entry.schema_fn)();
        let value: serde_json::Value = serde_json::from_str(&schema_str).map_err(|e| {
            anyhow::anyhow!(
                "invalid GTS Type Schema JSON emitted by GTS type {}: {e}",
                entry.type_id
            )
        })?;
        out.push(value);
    }
    Ok(out)
}

/// Returns every well-known GTS Instance declared via `gts_instance!` in
/// any crate linked into the current process.
///
/// # Errors
///
/// Currently never returns `Err`. Typed `gts_instance!` declarations use
/// `serde_json::to_value` internally, whose only failure mode is a custom
/// `Serialize` impl that fails — practically unreachable for our base
/// types (primitive / string / `GtsInstanceId` fields only). Raw
/// declarations use `serde_json::json!` on a literal object, which cannot
/// fail. The `Result` return type is kept for symmetry with
/// [`all_inventory_type_schemas`] and to leave room for surfacing
/// per-instance serialization errors without an API break if a future
/// base type ever introduces a fallible `Serialize` path.
pub fn all_inventory_instances() -> anyhow::Result<Vec<serde_json::Value>> {
    Ok(inventory::iter::<InventoryInstance>
        .into_iter()
        .map(|entry| (entry.payload_fn)())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        InventoryInstance, InventoryTypeSchema, all_inventory_instances, all_inventory_type_schemas,
    };

    #[test]
    fn platform_base_schemas_are_registered_and_valid() {
        let schemas = all_inventory_type_schemas().expect("schemas collect cleanly");

        // Both platform base types shipped by this crate must be present.
        let ids: Vec<&str> = inventory::iter::<InventoryTypeSchema>
            .into_iter()
            .map(|e| e.type_id)
            .collect();
        assert!(
            ids.contains(&"gts.cf.toolkit.plugins.plugin.v1~"),
            "PluginV1 not registered; got ids: {ids:?}"
        );
        assert!(
            ids.contains(&"gts.cf.toolkit.authz.permission.v1~"),
            "AuthzPermissionV1 not registered; got ids: {ids:?}"
        );
        assert_eq!(
            schemas.len(),
            ids.len(),
            "iter vs aggregated count mismatch (did all entries collect cleanly?)"
        );

        for (idx, s) in schemas.iter().enumerate() {
            assert!(s.is_object(), "schema #{idx} is not a JSON object: {s}");
            assert!(s.get("$id").is_some(), "schema #{idx} missing $id: {s}");
            assert!(
                s.get("type").is_some(),
                "schema #{idx} missing top-level type: {s}"
            );
        }
    }

    #[test]
    fn inventory_instances_registry_is_consistent() {
        // No instances ship from this crate, but the collector path must
        // still run. (External crates may contribute instances; this crate
        // only checks self-consistency.)
        let instances = all_inventory_instances().expect("instances collect cleanly");
        let ids: Vec<&str> = inventory::iter::<InventoryInstance>
            .into_iter()
            .map(|e| e.instance_id)
            .collect();
        assert_eq!(
            instances.len(),
            ids.len(),
            "iter vs aggregated count mismatch"
        );
    }
}
