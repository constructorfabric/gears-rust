# cf-gears-toolkit-gts

ToolKit integration with the [Global Type System](https://github.com/hypernetix/gts-spec) (GTS). Provides a process-wide link-time **inventory** of GTS Type Schemas and well-known Instances, proc-macros that populate it, and a small set of shipped platform base types (`PluginV1<P>`, `AuthzPermissionV1`).

## What this crate is for

- **Link-time inventory** — any `#[gts_type_schema(...)]`-annotated struct or `gts_instance!` declaration, in any crate linked into the process, lands in a shared `inventory`-based collector. `types-registry::init()` reads the collector and registers everything before publishing its client. No per-gear registration code is required for entries known at compile time.
- **Platform base types** — this crate also ships the handful of shipped base Type Schemas that many gears need directly: `PluginV1<P>` (base of every toolkit plugin instance) and `AuthzPermissionV1` (base of every authorization permission).

Gear-specific Type Schemas (e.g. plugin specs or gear-local permission extensions) can use either the wrapped `#[gts_type_schema(...)]` macro from this crate (and join the inventory automatically) or the raw upstream `#[gts_macros::struct_to_gts_schema(...)]` macro plus runtime registration in the gear's `init()`. Both paths end up in `types-registry`; the first has zero boilerplate.

## Adding a platform base Type Schema (inside this crate)

1. Create a new file, e.g. `src/role.rs`.
2. Annotate a struct with `#[gts_type_schema(type_id = "...", ...)]`:

   ```rust
   use crate::gts_type_schema;

   #[gts_type_schema(
       type_id = "gts.cf.core.authz.role.v1~",
       description = "Authorization role",
       properties = "name,permissions,display_name"
   )]
   pub struct RoleV1 {
       pub name: String,
       pub permissions: Vec<String>,
       pub display_name: String,
   }
   ```
3. Add `mod role;` (and optional `pub use role::RoleV1;`) to `lib.rs`.

`all_inventory_type_schemas()` picks it up automatically — no edits to central lists.

## Declaring a well-known GTS instance

Two separate macros by payload shape:

- **`gts_instance!`** — **typed**, Rust struct literal. Compile-time field/type checking. Preferred.
- **`gts_instance_raw!`** — **raw JSON** object literal containing the full Instance Identifier. Use when the instance has no Rust struct counterpart.

### `gts_instance!` — typed

Pass a struct literal of the conforming Rust type; the compiler checks every field name and type against it. The full Instance Identifier goes in the `id` field as a string literal.

```rust
use toolkit_gts::{AuthzPermissionV1, gts_instance};

gts_instance! {
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.am._.tenant_create.v1",
        resource_type: "gts.cf.core.am.tenant.*".to_owned(),
        action: "create".to_owned(),
        display_name: "Tenant creation".to_owned(),
    }
}
```

The struct literal must contain exactly one of `id` / `gts_id` / `gtsId` as a string literal. Upstream `gts_macros::gts_instance!` emits a compile-time assertion that the literal's prefix matches `<AuthzPermissionV1 as GtsSchema>::SCHEMA_ID` exactly (typo in the prefix → build error, not silent runtime mismatch) and rewrites the literal into a `GtsInstanceId` value before constructing the struct. The struct is then serialised via `serde_json::to_value`. Typos in field names, wrong field types, missing required fields — all compile errors.

**Optional — typed runtime accessor via `#[gts_static(NAME)]`:**

```rust
gts_instance! {
    #[gts_static(CHAT_READ_PERM)]
    AuthzPermissionV1 {
        id: "gts.cf.toolkit.authz.permission.v1~cf.mini_chat._.chat_read.v1",
        resource_type: /* ... */,
        action: /* ... */,
        display_name: /* ... */,
    }
}

// Elsewhere:
let p: &AuthzPermissionV1 = &CHAT_READ_PERM;
println!("{}", p.resource_type);
```

`pub static CHAT_READ_PERM: LazyLock<AuthzPermissionV1>` is emitted alongside the normal inventory submission. Opt-in: omit `#[gts_static(...)]` and only the inventory entry is emitted.

### `gts_instance_raw!` — raw JSON

Use when the payload does not correspond to a single canonical Rust struct. Pass a single brace-delimited JSON object literal — its top-level `"id"` key holds the full Instance Identifier.

```rust
use toolkit_gts::gts_instance_raw;

gts_instance_raw!({
    "id": "gts.cf.core.events.topic.v1~cf.core._.audit.v1",
    "name": "audit",
    "description": "Audit log events",
});
```

No compile-time field checking — validation happens at `types-registry::switch_to_ready()` via full JSON Schema validation.

### Generic base types

Some base types (e.g. `PluginV1<P>`) are generic over a derived-spec `P: GtsSchema`. Spell such instances with turbofish — `PluginV1::<DerivedSpec> { …, properties: DerivedSpec }`. The derived spec is declared with `#[gts_macros::struct_to_gts_schema(base = PluginV1, ...)]` in the owning gear and, when its `properties` depend on config, registered with `types-registry` at gear `init()` (in-process: it already has a reference to itself; OoP: via an SDK-level publish helper).

## Crate-local collectors (publishing your own declarations)

A gear that publishes its declarations to a remote `types-registry` needs to know which are
its own. Such a crate opts in, without changing any other crate:

```rust
// lib.rs
toolkit_gts::declare_gts_inventory!();

// anywhere in the crate: the same input as the crate-root macros
#[toolkit_gts::local::gts_type_schema(/* ... */)]
pub struct MySpecV1;
toolkit_gts::local::gts_instance_raw!({ "id": "...", /* ... */ });
```

The `local` macros submit each declaration to the crate's own collector and to the
process-wide inventory, so an embedded registry still seeds it. The crate then exports
`gts_declarations() -> Vec<(String, String)>` — `(GTS id, JSON text)`, std types only. A gear
merges its crates' declarations with `toolkit_gts::local::merge_declarations`, which sorts them,
keeps one of canonically equal duplicates (`same_canonical_content`: key order aside, number
spelling kept, so `-0.0` ≠ `0.0`) and reports every conflicting id with both origins. The
crate-root macros keep their process-wide-only behaviour; crates that do not opt in need no
change. A `local` macro without the declaration fails to compile ("could not find
`__toolkit_gts_inventory` in the crate root").

## Boundary with `types-registry`

- `types-registry` is a **content-agnostic** aggregator. It calls `all_inventory_type_schemas()` / `all_inventory_instances()` and registers whatever it finds. It never names specific types, so adding a new GTS Type requires zero edits in `types-registry`.
- The inventory is process-global. In in-process runs, `types-registry::init()` already sees every contributing crate's entries. In the future OoP world, each process publishes its own inventory to the remote registry via the SDK; gears are not involved in that either way.
