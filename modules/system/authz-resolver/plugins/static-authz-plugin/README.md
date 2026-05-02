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

Tenant is resolved from `TenantContext.root_id` first, then falls back to `subject.properties["tenant_id"]`.

This ensures that the Secure ORM receives the tenant scope it needs for queries, while denying access when no valid tenant can be determined.

## Configuration

```yaml
modules:
  static_authz_plugin:
    config:
      vendor: "cyberfabric"
      priority: 100
```

## Feature Flag

The server binary includes this plugin only when built with the `static-authz` feature:

```bash
cargo build --bin cf-server --features static-authz
```

The `make example` target enables this feature automatically.
