# Static AuthZ Plugin

> **Temporary plugin** — this is a development/testing stub that will be replaced by a production-ready AuthZ plugin in a future release.

Static authorization policy for the AuthZ Resolver gateway.

## Purpose

Provides a permissive authorization policy so that the platform can run end-to-end without an external policy engine. Useful for:

- Local development (`make quickstart`, `make example`)
- E2E / integration tests that need authorization to pass
- Demos and prototyping

**Do not use in production.**

## Behavior

| Scenario | Decision | Constraints |
|----------|----------|-------------|
| Valid tenant resolved | `true` | `in` predicate on `owner_tenant_id` scoped to the caller's tenant |
| Nil (`00000000-…-000`) tenant | `false` | none |
| No tenant resolvable | `false` | none |
| Matching `property_grants` rule, PEP declares the property | `true` | every emitted constraint also carries `in(property, values)` (a constraint with only that predicate when the PEP declares no tenant property) |
| Matching `property_grants` rule, PEP does not declare the property | `false` | none (fail closed, a warning is logged) |

Tenant is resolved from `TenantContext.root_id` first, then falls back to `subject.properties["tenant_id"]`.

This ensures that the Secure ORM receives the tenant scope it needs for queries, while denying access when no valid tenant can be determined.

## Configuration

```yaml
gears:
  static_authz_plugin:
    config:
      vendor: "constructorfabric"
      priority: 100
```

### Property grants

`property_grants` (default: empty, behaviour unchanged) restricts which values of a resource property a caller gets, in one evaluation. A rule matches when `resource_type` equals the request's resource type exactly, the action is in `actions` (empty or absent = every action) and the caller is in `subjects` (empty or absent = every subject). Rules on the same property are unioned. Every value is parsed at startup: a UUID is used as is, a string starting with `gts.` must be a valid GTS id and is converted to its v5 UUID (an invalid one fails startup), anything else is a plain string. A rule with `values: []` admits nothing. If the PEP does not declare the property, the request is denied rather than widened.

Type grant: every subject may only touch the `api_key` credential type or one given by UUID.

```yaml
gears:
  static_authz_plugin:
    config:
      property_grants:
        - resource_type: "gts.cf.core.credstore.credential.v1~"
          property: "secret_type"
          actions: ["read", "read_secret"]
          values:
            - "gts.cf.core.credstore.credential.v1~cf.core.credstore.api_key.v1~"
            - "0f9c3a52-6a1e-5c3e-9d55-1f2c4b7a8e10"
```

Reference grant: the application `email-sender` may read the value of the `smtp-password` credential only.

```yaml
gears:
  static_authz_plugin:
    config:
      property_grants:
        - resource_type: "gts.cf.core.credstore.credential.v1~"
          property: "reference"
          actions: ["read_secret"]
          subjects: ["6f1b7c1e-3c52-4c5e-8d0a-5b2f9a4e7c31"]   # email-sender
          values: ["smtp-password"]
```

## Feature Flag

The server binary includes this plugin only when built with the `static-authz` feature:

```bash
cargo build --bin cf-gears-server --features static-authz
```

The `make example` target enables this feature automatically.
