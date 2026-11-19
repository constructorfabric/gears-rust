# Resource Group Rust Interfaces

The public contracts live in
[`resource-group-sdk/src/api.rs`](../resource-group-sdk/src/api.rs).
Both are async, `Send + Sync` interfaces registered in ClientHub. Every fallible
method returns `CanonicalError`; consumers can project it into the SDK's
`ResourceGroupError` when they need typed dispatch.

## Authorized client: `ResourceGroupClient`

This interface is for application consumers and the REST adapter. Operations use
the caller's `SecurityContext` and enforce PolicyEnforcer constraints.

| Method | Purpose |
| --- | --- |
| `create_group` | Create a group from `CreateGroupRequest` |
| `get_group` | Read a scoped group by UUID |
| `list_groups` | List groups using `&ODataQuery` |
| `update_group` | Replace name, parent and metadata; type and tenant are immutable |
| `delete_group` | Delete an empty group |
| `delete_group_cascade` | Delete a subtree and its memberships |
| `get_group_descendants` | Read descendants with relative depth |
| `get_group_ancestors` | Read ancestors with relative depth |
| `add_membership` | Add `(group_id, resource_type, resource_id)` |
| `remove_membership` | Remove a scoped membership by that key |
| `list_memberships` | List memberships through the owning groups' scope |

List and hierarchy methods return `toolkit_odata::Page<T>`. The concrete service
implements cascade deletion; the trait's default `delete_group_cascade` delegates
to non-cascade deletion, so another implementation must override it to provide
cascade semantics.

Publish the group's GTS type through Types Registry before creating a group:

```rust
use resource_group_sdk::{CreateGroupRequest, ResourceGroup, ResourceGroupClient};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

async fn create_team(
    client: &dyn ResourceGroupClient,
    ctx: &SecurityContext,
    type_code: &str,
) -> Result<ResourceGroup, CanonicalError> {
    client
        .create_group(ctx, CreateGroupRequest::new(type_code, "Engineering"))
        .await
}
```

A caller outside a group's scope observes the same not-found result as for a
missing group. Membership reads and removal enforce scope through the owning
group. An idempotent cleanup caller may treat not found as already removed only
when its enumeration scope is no wider than its deletion scope.

## Trusted reads: `ResourceGroupReadHierarchy`

This in-process interface serves AuthZ and tenant resolver consumers. Its methods
bypass PolicyEnforcer: a PDP reading RG as a policy information source must not
re-enter itself. The caller owns tenant scoping and must supply the appropriate
filters or check the returned `tenant_id`.

| Method | Return type |
| --- | --- |
| `get_group_descendants(ctx, group_id, &query)` | `Page<ResourceGroupWithDepth>` |
| `get_group_ancestors(ctx, group_id, &query)` | `Page<ResourceGroupWithDepth>` |
| `list_groups(ctx, &query)` | `Page<ResourceGroup>` |
| `get_group(ctx, id)` | `ResourceGroup` |
| `list_memberships(ctx, &query)` | `Page<ResourceGroupMembership>` |

Each result is wrapped in `Result<_, CanonicalError>`. A subject-membership
lookup must filter by `resource_id` and, where appropriate, `resource_type`;
an unfiltered call returns all membership rows. This trusted interface has no
separate unscoped REST endpoint.

## Models and query fields

`ResourceGroup` contains `id`, GTS `code`, `name`, `hierarchy` and optional
`metadata`. The hierarchy contains `parent_id` and `tenant_id`;
`ResourceGroupWithDepth` adds relative `depth`. REST serializes `code` as `type`.

`CreateGroupRequest::new(code, name)` supplies the required values; its `with_*`
setters add the optional ID, parent, target tenant and metadata.
`UpdateGroupRequest` contains `name`, `parent_id` and `metadata`.
`ResourceGroupMembership` contains `group_id`, `resource_type` and `resource_id`.

`ODataQuery` carries typed filters and pagination. The SDK's
[`odata` modules](../resource-group-sdk/src/odata/) define the supported fields.

## Wiring and type ownership

[`ResourceGroup::init`](../resource-group/src/gear.rs) builds `RgService` for
`ResourceGroupClient` and `RgReadService` for `ResourceGroupReadHierarchy`, then
registers each under its own ClientHub trait key. Both share the underlying group
and membership services.

RG publishes its base GTS schema from the SDK's crate-local collector. Each
consumer publishes its derived types through its own managed publication. Group
and membership writes obtain materialized type rules from `TypesRegistryApi`
with the caller's context; payload validation uses `types-registry-sdk`.

See [DESIGN.md](DESIGN.md) for transaction boundaries, lifecycle behavior and the
three-table persistence model.
