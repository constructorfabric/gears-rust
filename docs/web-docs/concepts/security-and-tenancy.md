---
title: Security & multi-tenancy
description: The secure-by-default data path — SecurityContext, PDP/PEP, AccessScope, SecureConn, and the tenant tree.
sidebar:
  label: Security & multi-tenancy
  order: 4
---

Security in Gears is not a library you remember to call — it is a layered path you cannot
accidentally bypass.

## The secure-by-default data path

1. **Static checks** — custom lints catch violations at build time (e.g. raw SQL outside
   migrations, the domain layer importing infrastructure).
2. **Authentication** — the API Gateway validates tokens and injects a `SecurityContext`
   (subject id, subject tenant, token scopes). Gears never parse tokens.
3. **Authorization** — handlers/services call the `PolicyEnforcer` (the PEP), which queries
   the AuthZ resolver (the PDP). The decision includes row-level constraints compiled into an
   `AccessScope`.
4. **Database scoping** — gears query through `SecureConn`, which applies the `AccessScope`
   as automatic `WHERE` clauses for tenant isolation and ABAC. Raw connections are not
   exposed.

The `SecurityContext` flows through the system as explicit data — passed as the first
argument to SDK methods and service calls — rather than hidden in thread-local state.

## PDP / PEP

Gears define only the **PDP–PEP contract**, not a policy language:

- The **PEP** (Policy Enforcement Point) is the `PolicyEnforcer` your service calls. It
  builds an `AccessRequest`, asks the PDP, and compiles the answer into an `AccessScope`.
- The **PDP** (Policy Decision Point) is a vendor plugin behind the AuthZ resolver. It may
  implement RBAC, ABAC, ReBAC, or anything else, and returns a decision plus row-level
  constraint predicates (e.g. `eq`, `in`, `in_tenant_subtree`, `in_group`).

```rust
let scope = self.policy_enforcer
    .access_scope_with(
        ctx, &resources::USER, actions::CREATE, None,
        &AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, new_user.tenant_id),
    )
    .await?;
```

Authorization is **fail-closed**: a denied decision, an unreachable PDP, or a constraint that
can't be compiled all result in access being denied.

## SecureConn & Scopable

Entities opt into scoping with `#[derive(Scopable)]`, declaring which columns map to the
tenant / owner / resource / type dimensions:

```rust
#[derive(DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "users")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model { /* … */ }
```

Repositories then apply the scope to every query — `.secure().scope_with(&scope)` for reads,
`secure_insert`/`secure_update_with_scope` for writes — so the `AccessScope` becomes SQL.

:::danger[Never bypass SecureConn]
Reaching for a raw database connection skips `AccessScope` and can leak data across tenants.
There is intentionally no unscoped escape hatch — raw SQL outside migrations is rejected by
the workspace lints. Always go through `SecureConn` with a scope obtained from the
`PolicyEnforcer`.
:::

The [authorization guide](../../guides/authorization/) and [database guide](../../guides/database/)
show the full flow against real code.

## Multi-tenancy

Tenants form a **single-root tree**. Every resource belongs to exactly one tenant
(`owner_tenant_id`) — the primary isolation boundary. A materialized **closure table** makes
ancestor/descendant queries cheap.

- **Parent → child visibility** is the default: parents can read child data.
- A child can raise a **barrier** (`self_managed = true`) to hide its subtree from ancestors,
  configurable per resource type (e.g. business data is hidden, but usage/billing data may
  still roll up).
- **Resource groups** add optional, tenant-scoped grouping used as an input to authorization
  decisions.

Three tenant notions appear in requests: the **subject tenant** (who the caller belongs to),
the **resource/owner tenant** (`owner_tenant_id`, the partition key), and the **context
tenant** (the scope root for the operation, which may differ in cross-tenant scenarios).
