# Resource Group

Main gear for hierarchical resource group management in Gears. Backed by `cf-gears-resource-group-sdk`, persisted via SeaORM, exposed over REST, and integrated with the AuthZ resolver and types registry.

## Overview

The `cf-gears-resource-group` gear provides:

- **GTS-typed groups** — every group is bound to a GTS type (validated against the types registry)
- **Hierarchy** — parent/child relationships with ancestor/descendant traversal and depth tracking
- **Memberships** — `(resource_type, resource_id)` links to groups
- **OData listing** — filterable, cursor-paginated reads for types, groups, and memberships
- **AuthZ enforcement** — every mutation and read goes through `PolicyEnforcer` from `cf-gears-authz-resolver-sdk`
- **ClientHub integration** — registers `ResourceGroupClient` (full surface) and `ResourceGroupReadHierarchy` (narrow read-only) for in-process consumers

The gear owns its database schema via `DatabaseCapability` and exposes a REST surface via `RestApiCapability`.

## Architecture

```
Consumer Gear
    │
    ▼
ResourceGroupClient / ResourceGroupReadHierarchy  (SDK traits, ClientHub)
    │
    ▼
cf-gears-resource-group  (this crate — services, repos, REST handlers)
    │
    ├──▶ AuthZ resolver  (PolicyEnforcer)
    ├──▶ Types registry  (GTS schema validation)
    └──▶ Database         (SeaORM, sqlite + pg)
```

## Capabilities

- `db` — SeaORM-backed storage with gear-owned migrations
- `rest` — REST API for types, groups, and memberships (OpenAPI-described)
- Gear dependencies: `authz-resolver`, `types-registry`

## Usage (in-process)

Consumers use the SDK trait from `cf-gears-resource-group-sdk`:

```rust
use resource_group_sdk::ResourceGroupClient;

let rg = hub.get::<dyn ResourceGroupClient>()?;
let group = rg.get_group(&ctx, group_id).await?;
```

For read-only consumers (e.g. AuthZ plugin, tenant-resolver RG plugin):

```rust
use resource_group_sdk::ResourceGroupReadHierarchy;

let read = hub.get::<dyn ResourceGroupReadHierarchy>()?;
let descendants = read.get_group_descendants(&ctx, group_id, &query).await?;
```

## REST API

The gear registers REST routes for types, groups, hierarchy traversal, and memberships. See the generated OpenAPI document for the full surface, including cascade-delete endpoints that are intentionally not exposed via the SDK.

## Testing

```bash
cargo test -p cf-gears-resource-group
```

## License

Apache-2.0

## Group identifier ownership

Ordinary REST and SDK creation allocate UUIDv7 IDs server-side; supplying `id`
returns `InvalidArgument` (HTTP 400). Deterministic tenant defaults use the
separate authenticated `ResourceGroupDefaults` SDK capability. See the
[migration guide](../docs/explicit-id-migration.md) before upgrading consumers
that currently supply explicit IDs.
