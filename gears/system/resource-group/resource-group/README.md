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

Types Registry owns the RG type definitions. The SDK declares the RG base
schemas. Consumer gears publish derived group schemas and member resource types
through managed GTS publication (`gts_publication`) before application initialization.
Dynamic publishers use Types Registry's platform API. See the
[type contracts](../docs/features/0002-type-management.md) and
[migration guidance](../docs/DESIGN.md).

Group types derive from `gts.cf.core.rg.type.v1~`, which requires
a `parent_type` payload field and a `membership_type` trait on concrete types. Publish a concrete membership schema
under `gts.cf.core.rg.group_membership.v1~` and the group schema that binds to it.
Several group types can share a membership schema.

- The group schema constrains `parent_type` in its body. At write time RG
  supplies the actual parent's GTS type, or `null` for a root. For a parent
  family, use a string branch with `x-gts-type-ref`.
- The group type sets `x-gts-traits.membership_type` and locks it with
  `const` in `x-gts-traits-schema.properties.membership_type`. RG reads the
  value from `effective_traits` and checks that the selected schema is active
  and concrete before creating the group.
- When adding a member, RG reads the group's current type declaration and
  batches the membership schema and member type lookup. It validates `{resource_type}`
  against the active membership schema. Use `anyOf` with `x-gts-type-ref` to
  allow type families, or `enum` to allow exact type IDs.

Types Registry checks same-ID revisions using its normal schema compatibility
rules. Widening placement or membership constraints can take effect for
existing groups. The [schema revision E2E tests](../../../../testing/e2e/suites/resource_group/test_type_revisions.py)
publish revisions through Types Registry and verify that existing groups can
move or accept members under the widened rules. They also check that the
registry refuses incompatible payload revisions and rejects a same-ID revision
that replaces the `membership_type` constant with a different schema ID.

Publish group and membership schemas for existing GTS identifiers before
upgrading deployments that use custom types. Adding a member to any group
uses the current membership rule published for that group type.

Registries that already admitted the earlier v1 schema with a required
`membership_type` payload field need a schema migration. Moving that field
to traits changes the accepted payloads and can fail the registry's same-ID
compatibility check.

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
