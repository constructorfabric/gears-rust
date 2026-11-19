# Resource Group

Main gear for hierarchical resource group management in Gears. Backed by `cf-gears-resource-group-sdk`, persisted via SeaORM, exposed over REST, and integrated with the AuthZ resolver and types registry.

## Overview

The `cf-gears-resource-group` gear provides:

- **GTS-typed groups** — every group is bound to a GTS type (validated against the types registry)
- **Hierarchy** — parent/child relationships with ancestor/descendant traversal and depth tracking
- **Memberships** — `(resource_type, resource_id)` links to groups
- **OData listing** — filterable, cursor-paginated reads for groups and memberships
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
- `rest` — REST API for groups, hierarchy traversal, and memberships (OpenAPI-described)
- Gear dependencies: `authz-resolver`, `types-registry`

## Type Publication and Upgrade

Types Registry owns the RG type definitions. The SDK declares the RG base schema;
consumer gears publish derived group schemas and membership resource types through
managed GTS publication (`gts_publication`) before application initialization.
Dynamic publishers use Types Registry's platform API. See the
[type contracts](../docs/features/0002-type-management.md) and
[migration guidance](../docs/DESIGN.md).

Export custom type definitions before upgrading: the migration removes the old
local definitions (metadata schemas, placement rules) irreversibly and keeps
`gts_type` only as an identity dictionary of type keys. Existing groups and
memberships keep their keys and remain readable and removable after their types
are retired. Membership additions limit `resource_id` to 2600 UTF-8 bytes.
Upgrade preflight rejects oversized legacy identifiers before altering tables;
reconcile those resource identifiers and their links before retrying the upgrade.

## Configuration

```yaml
gears:
  resource-group:
    config:
      # GTS type-path prefixes that make an RG type a tenant type
      # (`tenant_id = group.id`, tenant-root uniqueness). A type is a tenant
      # type when its code starts with ANY of the entries.
      # Default: [TENANT_RG_TYPE_PATH] = ["gts.cf.core.rg.type.v1~cf.core._.tenant.v1~"]
      tenant_type_paths:
        - "gts.cf.core.rg.type.v1~acme.core._.tenant.v1~"
```

Entries must be canonical (trimmed, lowercase). `rg-tr-plugin`'s `tenant_types`
takes exact type paths, not prefixes: list there every concrete tenant type
that falls under these prefixes.

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

The gear registers REST routes for groups, hierarchy traversal, and memberships. See the generated OpenAPI document for the full surface, including force-cascade deletion.

## Testing

```bash
cargo test -p cf-gears-resource-group
```

## License

Apache-2.0
