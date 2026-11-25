# Static AuthZ Plugin

> **Temporary plugin** — this is a development/testing stub that will be replaced by a production-ready AuthZ plugin in a future release.

Static authorization policy for the AuthZ Resolver gateway.

## Purpose

Provides a tenant-scoped authorization policy so that the platform can run end-to-end without an external policy engine. Useful for:

- Local development (`make quickstart`, `make example`)
- E2E / integration tests that need authorization to pass
- Demos and prototyping

**Do not use in production.**

## Behavior

| Scenario | Decision | Constraints |
|----------|----------|-------------|
| Valid tenant resolved | `true` | `in` on `owner_tenant_id` scoped to the caller's tenant, only if the PEP declares `owner_tenant_id`; with the `TenantHierarchy` capability, also `InTenantSubtree` on each declared of `owner_tenant_id` / `id`; otherwise empty |
| Valid tenant, resource has `group_ids` or `ancestor_group_ids` | `false` | none |
| Nil (`00000000-…-000`) tenant | `false` | none |
| No tenant resolvable | `false` | none |

Tenant is resolved from `TenantContext.root_id` first, then falls back to `subject.properties["tenant_id"]`.

This ensures that the Secure ORM receives the tenant scope it needs for queries, while denying access when no valid tenant can be determined.

## Registration

The `gts_publication(entities = …)` gear attribute publishes a
`PluginV1<AuthZResolverPluginSpecV1>` instance whose own segment is
`cf.builtin.static_authz_resolver.plugin.v1` (full ID
`gts.cf.toolkit.plugins.plugin.v1~cf.core.authz_resolver.plugin.v1~cf.builtin.static_authz_resolver.plugin.v1`);
the runtime admits it before `init`, and `init` only binds the client. The runtime
publishes to the types-registry linked into the same process or to a remote one; an
in-process registry without `gears.types-registry.database` fails boot. See the
authz-resolver README, *Plugin catalogue*.

## Configuration

```yaml
gears:
  static-authz-plugin:
    config:
      vendor: "constructorfabric"
      priority: 100
```

## Feature Flag

The server binary includes this plugin only when built with the `static-authz` feature:

```bash
cargo build --bin cf-gears-example-server --features static-authz
```

The `make example` target enables this feature automatically.
