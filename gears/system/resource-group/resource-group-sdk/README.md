# Resource Group SDK

SDK crate for the Resource Group gear, providing public API contracts for hierarchical resource group management with the GTS type system in Gears.

## Overview

This crate defines the transport-agnostic interface for the Resource Group gear:

- **`ResourceGroupClient`** — Async trait for group and membership lifecycle
- **`ResourceGroupReadHierarchy`** — Narrow read-only trait for in-process plugin consumers (e.g. AuthZ resolver, tenant-resolver RG plugin) that need ancestor/descendant walks plus flat OData listing
- Models: `ResourceGroup`, `ResourceGroupMembership`, `ResourceGroupWithDepth`, `GroupHierarchy`, etc.
- **`ResourceGroupError`** — Optional typed projection of the `CanonicalError` returned by client methods
- OData filter field definitions (behind the `odata` feature)

## Usage

### Getting the Client

Consumers obtain the client from `ClientHub`:

```rust
use resource_group_sdk::ResourceGroupClient;

let rg = hub.get::<dyn ResourceGroupClient>()?;
```

### Publishing Types

RG publishes the SDK's abstract group and membership schemas. Owners publish
derived schemas through `gts_publication` before application initialization
or use Types Registry's platform API for dynamic publication. Publish group
and member resource types before creating links. SDK models expose GTS type
identifiers, and RG fetches definitions for type-dependent writes. See the
[type workflow](../docs/features/0002-type-management.md) and
[upgrade steps](../docs/DESIGN.md#upgrading-resource-group).

### Group Lifecycle

```rust
use resource_group_sdk::CreateGroupRequest;

let req = CreateGroupRequest::new(type_code, "Engineering").with_parent_id(Some(parent_id));
let group = rg.create_group(&ctx, req).await?;
let same = rg.get_group(&ctx, group.id).await?;
```

`CreateGroupRequest` is `#[non_exhaustive]`: build it through `::new` and the
`with_*` setters rather than a struct literal. This lets the gear add new
optional fields (as `tenant_id` was, below) without breaking existing
callers.

### Hierarchy Traversal

```rust
use toolkit_odata::ODataQuery;

let descendants = rg.get_group_descendants(&ctx, group.id, &ODataQuery::default()).await?;
let ancestors   = rg.get_group_ancestors(&ctx, group.id, &ODataQuery::default()).await?;
```

### Memberships

```rust
rg.add_membership(&ctx, group.id, &resource_type, &resource_id).await?;
rg.remove_membership(&ctx, group.id, &resource_type, &resource_id).await?;
```

`resource_type` is a registered concrete GTS type identifier. On addition, the
UTF-8 length of `resource_id` must not exceed `MAX_MEMBERSHIP_RESOURCE_ID_BYTES`
(2600 bytes), on either supported database backend. Removal matches the stored
type without consulting the registry and works after type retirement.

## Read-Only Hierarchy Trait

Plugin consumers that need only read access can depend on the narrower
`ResourceGroupReadHierarchy` trait. It exposes ancestor/descendant walks and
OData-filtered listing — enough to support batch lookups like
`id in (id1, id2, …)` without pulling in the full client surface.

```rust
use resource_group_sdk::ResourceGroupReadHierarchy;

let read = hub.get::<dyn ResourceGroupReadHierarchy>()?;
let page = read.list_groups(&ctx, &query).await?;
```

## Error Handling

```rust
use resource_group_sdk::ResourceGroupError;

match rg.get_group(&ctx, id).await.map_err(ResourceGroupError::from) {
    Ok(group) => { /* ... */ }
    Err(ResourceGroupError::NotFound { .. }) => { /* ... */ }
    Err(e) => return Err(e.into()),
}
```

## Features

- `odata` (default) — enables OData filter field definitions and typed query
  helpers (depends on `toolkit-odata-macros` and `toolkit-sdk`).

## Versioning notes

This crate has no separate `CHANGELOG` — notable pre-1.0 API changes are
recorded here.

- **0.3.0**: `CreateGroupRequest` gained an optional `tenant_id` field and
  became `#[non_exhaustive]` with a `::new` constructor plus `with_*`
  setters (see Group Lifecycle above). Per pre-1.0 SemVer, adding a field to
  a public struct is a breaking change for any consumer still constructing
  it with a struct literal, hence the minor bump rather than a patch.

## License

Apache-2.0
