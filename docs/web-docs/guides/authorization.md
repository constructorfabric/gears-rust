---
title: Authorization
description: The PDP/PEP flow — PolicyEnforcer, AccessScope, constraint predicates, and route-level auth.
sidebar:
  label: Authorization
  order: 3
---

Authentication happens at the edge (the API Gateway validates the token and injects a
`SecurityContext`). **Authorization happens in your gear**: each operation asks the
`PolicyEnforcer` for an `AccessScope` before touching data. This guide shows that flow.

## Declare the route's auth posture

Every route states whether it requires a token. `OperationBuilder::authenticated()` requires
a valid bearer token; `.public()` opts out (rare):

```rust
OperationBuilder::get("/users-info/v1/users")
    .operation_id("users_info.list_users")
    .authenticated()                 // require a token
    .handler(handlers::list_users)
    // …
```

The handler then receives the `SecurityContext` as an Axum extension:

```rust
pub async fn list_users(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteAppServices>>,
    OData(query): OData,
) -> ApiResult<JsonPage<serde_json::Value>> {
    let page = svc.users.list_users_page(&ctx, &query).await?;
    // …
}
```

## Declare resource types

A gear declares the resource types it protects and which PEP properties the PDP may use to
constrain them. Declare these once:

```rust title="domain/service/mod.rs"
pub const USER: ResourceType = ResourceType::from_static(
    "users_info.user",
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub mod actions {
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    pub const CREATE: &str = "create";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
}
```

An entity with per-user ownership would also list `pep_properties::OWNER_ID` (mapping to its
owner column) so the PDP can return "only your own rows" constraints.

## Construct the enforcer

The `PolicyEnforcer` is built once in `init` from the AuthZ resolver client (resolved through
`ClientHub`), then shared by all services. The gear must declare `deps = ["authz-resolver"]`.

```rust
let authz = ctx.client_hub().get::<dyn AuthZResolverClient>()?;
let enforcer = PolicyEnforcer::new(authz);
```

## Ask for a scope, then query

In each service method, request the scope and pass it to the repository:

```rust title="domain/service/users.rs"
// List: no resource id, constraints come entirely from the PDP
let scope = self.policy_enforcer
    .access_scope(ctx, &resources::USER, actions::LIST, None)
    .await?;
let page = self.repo.list_page(&conn, &scope, query).await?;

// Create/Update/Delete: supply resource properties via AccessRequest
let scope = self.policy_enforcer
    .access_scope_with(
        ctx, &resources::USER, actions::CREATE, None,
        &AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, new_user.tenant_id),
    )
    .await?;
let created = self.repo.create(&conn, &scope, user).await?;
```

For point operations (get/update/delete by id), the example **prefetches** the row with
`AccessScope::allow_all()` to read its `owner_tenant_id`, passes that as a resource property,
and lets the PDP return a narrow `eq` constraint — which also gives TOCTOU protection on the
subsequent scoped write.

## Constraint predicates

The PDP returns a decision plus row-level constraints, which the PEP compiles into the
`AccessScope`. Predicate kinds include:

| Predicate | Meaning |
| --- | --- |
| `eq(prop, value)` | exact match |
| `in(prop, [values])` | one of a set |
| `in_tenant_subtree(prop, root)` | within a tenant subtree |
| `in_group(prop, [members])` | group membership |

## Fail-closed by design

Authorization denies on anything uncertain:

- PDP **denies** → permission denied (HTTP 403);
- PDP is **unreachable** or evaluation fails → internal error (HTTP 500);
- a constraint **can't be compiled** to a column → denied.

Map the enforcer error into your domain error so it renders as the right canonical problem:

```rust
DomainError::Forbidden =>
    UserResourceError::permission_denied().with_reason("ACCESS_DENIED").create(),
```

## See also

- [Security & multi-tenancy](../../concepts/security-and-tenancy/) — the model behind this flow.
- [Database patterns](../database/) — how the `AccessScope` becomes SQL.
- Full code: `examples/toolkit/users-info/users-info/src/domain/service/`.
