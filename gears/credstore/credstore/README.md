# CredStore

Stateful credential-storage gear module. Owns per-secret metadata in its own
database, enforces authorization in SQL, resolves secrets hierarchically across
the tenant tree, and stores the secret **value** in a backend plugin discovered
via the types registry.

> Design: [`docs/DESIGN.md`](../docs/DESIGN.md) is the baseline; the shipped
> implementation is described in [`docs/DESIGN-ADDENDUM.md`](../docs/DESIGN-ADDENDUM.md)
> (stateful gear, `credstore_secrets` table, PDP-scope authz, versioning/ETag,
> write saga, tenant-isolation barriers).

## Overview

The `cf-gears-credstore` module provides:

- **Local metadata** — a gear-owned `credstore_secrets` table (SecureORM /
  sea-orm, migration `m0001`) holding sharing, owner, status, `version`, and
  the value-fingerprint fence
- **PDP authorization** — `AccessScope` enforced in SQL via SecureORM clamps;
  out-of-scope access is fail-closed (canonical 404, anti-enumeration)
- **Hierarchical resolution** — a single indexed query over the ancestor chain
  (TTL+LRU cached, barrier-aware); the backend is read once for the winner's value
- **Value-fingerprint fence** — every read verifies the backend value against a
  per-row `HMAC-SHA256` (key auto-stored in the backend, never on the wire), so
  a metadata/value desync from a concurrent write fails closed instead of
  disclosing a value under a foreign sharing label (DESIGN §4.10, ADR-0003)
- **Versioning** — strong generation-bound `ETag` (`"<id>.<version>"`) on `GET`,
  `If-Match` optimistic concurrency on `PUT`/`DELETE` (no ABA across recreation)
- **Crash-safe writes** — provisioning→backend→active saga with rollback and a reaper
- **Backend plugin** — value-only store discovered via the types registry (vendor)
- **ClientHub + REST** — registers `CredStoreClientV1`; exposes `/credstore/v1/secrets`

This module depends on `types-registry`, `tenant-resolver`, and `authz-resolver`,
and **requires a database**. The secret value is stored in a plugin (e.g.
`cf-gears-static-credstore-plugin`, or an OpenBao-backed plugin).

## Usage

Consumers obtain the client from `ClientHub`:

```rust
use credstore_sdk::CredStoreClientV1;

let credstore = ctx.client_hub().get::<dyn CredStoreClientV1>()?;

if let Some(resp) = credstore.get(&ctx, &SecretRef::new("my-api-key")?).await? {
    // resp.value, resp.sharing, resp.is_inherited
}
```

## Configuration

The module requires a `database:` section (it is stateful). Gear config:

```yaml
credstore:
  database:
    server: "sqlite_users"   # a database server template; module gets its own file
    file: "credstore.db"
  config:
    vendor: "virtuozzo"      # GTS vendor used to discover the value-store plugin
    hierarchy:
      ancestor_cache_ttl_secs: 300
      tenant_closure_colocated: false   # opt in only where the closure is co-located
    reaper:
      tick_secs: 60
      provisioning_timeout_secs: 300
```

## License

Apache-2.0
