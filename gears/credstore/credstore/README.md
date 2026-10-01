# `CredStore`

Stateful credential-storage gear module. Owns per-secret metadata in its own
database, enforces authorization in SQL, resolves secrets hierarchically across
the tenant tree, and stores the secret **value** in a backend plugin discovered
via the types registry.

> Design: the [technical design](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/DESIGN.md) is the baseline; the decision to
> ship a stateful gear (`credstore_secrets` table, PDP-scope authz,
> versioning/ETag, write saga) instead of the original stateless design is
> recorded in [ADR-0001](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0001-cpt-cf-credstore-adr-stateful-gear.md);
> the addendum document that used to carry this history was folded into that
> ADR and removed (see git history for its prior content).

## Overview

The `cf-gears-credstore` module provides:

- **Local metadata** — a gear-owned `credstore_secrets` table (`SecureORM` /
  sea-orm, migrations `m0001` + `m0002`) holding sharing, owner, status,
  `version`, `fallback`, and a `value_version` pointer to the row's current
  immutable backend version (ADR-0006)
- **One item shape for the record and its secret** — the point read and the
  collection return the same shape; `secret` is present only when the
  caller's `$select` names it, under `read_secret`; a credential can be
  created without a secret, by an explicit `null`, which is also how a
  tenant suppresses an inherited credential with no row of its own
  ([ADR-0004](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md),
  [ADR-0007](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0007-cpt-cf-credstore-adr-record-write-verbs.md),
  [ADR-0008](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0008-cpt-cf-credstore-adr-suppression-fallback.md))
- **PDP authorization** — six actions on the credential type (`list`,
  `read`, `write`, `delete` on the record; `read_secret`, `write_secret` on
  the secret, [ADR-0010](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0010-cpt-cf-credstore-adr-type-scoped-authorization.md)),
  `AccessScope` enforced in SQL via `SecureORM` clamps; out-of-scope access
  is fail-closed (canonical 404, anti-enumeration)
- **Hierarchical resolution** — a single indexed query over the ancestor chain
  (TTL+LRU cached, barriers ignored — `shared` inherits through them); the backend is read once for the winner's value
- **Versioning** — strong generation-bound `ETag` (`"<id>.<version>"`) on `GET`,
  mandatory `If-Match` on `PUT`/`DELETE` (a validator, or `*` for explicit
  last-writer-wins; no ABA across recreation)
- **Crash-safe writes** — each value is written with `put(key, value)`, which
  returns an immutable version; the row's `value_version` pointer switches to
  it in one transaction after the bytes are durable, and older versions are
  destroyed best effort afterwards (only if the plugin declares
  `supports_destroy`; a failure is counted in `destroy_failed` and retried
  implicitly by the next write). No resident reaper, no maintenance job
- **Delete and purge** — deleting a record is one row transaction that also
  enqueues a key purge on the platform transactional outbox; the outbox
  handler calls the plugin's `delete_key(key)` (retried until it succeeds,
  `outbox_purge_failed` counts failed attempts). The reference is free at
  once, so delete-then-recreate works immediately
- **Audit** — every secret read and write is published to the credstore audit
  topic through the `event-broker` gear, best effort: a failure logs an error
  (never the secret) and counts `audit_publish_failed`, and the operation is
  unaffected
- **Metrics** — `read_outcome`, `walkup_depth`, dependency latency/health,
  `cross_tenant_denied`, `destroy_failed`, `outbox_purge_failed`, `read_retry`,
  `list_type_invariant_violation`, `audit_publish_failed` (`credstore_*`
  OpenTelemetry instruments); no inventory gauge
- **Backend plugin** — a versioned value store (`CredStorePluginClientV2`: `put`, `get`, `delete_key`, optional `destroy`) keyed by `(tenant_id, record_id)`, discovered via the types registry (vendor)
- **`ClientHub` + REST** — registers `CredStoreClientV1`; exposes `/credstore/v1/credentials`

This module depends on `types-registry`, `tenant-resolver`, and `authz-resolver`,
and **requires a database**. The secret value is stored in a plugin (e.g.
`cf-gears-static-credstore-plugin`, or
[`cf-gears-vault-credstore-plugin`](../plugins/vault-credstore-plugin/README.md)
(prototype), backed by Vault/OpenBao).

A credential is created in one request — a `PUT` on the record address
carries the record and a tri-state `secret` together: a string writes it,
and an explicit `null` creates the record without one (`declared`), which
is also how a tenant suppresses an inherited credential without a row of
its own. A merge-`PATCH` on the same address edits metadata or
rotates/removes the secret without touching the rest of the record; and the
secret is read by naming it in `$select` on that same address or on the
collection — there is no dedicated secret address. Every value write stores a new immutable version in the backend under the
record key `(tenant_id, record_id)`, switches the row's `value_version`
pointer to it in one transaction, and destroys the replaced versions best
effort afterwards — the model of Vault KV v2 and the cloud secret managers.
Expiry applies to the secret, not to the record. Nothing sweeps expired rows:
an expired record stays visible (status `expired`, normal validator) but its
secret is never served — a read of it fails `409 SECRET_EXPIRED`, without
falling through to an ancestor's value. The owner renews it in place with a
`PATCH` of `expires_at` or a replace; a create-only `PUT` over it is
`409 ALREADY_EXISTS`.

## Usage

After the gear initializes, consumers obtain its client from `ClientHub`. This
example reads a secret value (`get_secret`, the `read_secret` action) without
formatting or logging it; the record itself is read with `get` (`read`):

```no_run
use std::error::Error;

use credstore_sdk::{CredStoreClientV1, SecretRef};
use toolkit::ClientHub;
use toolkit_security::SecurityContext;

async fn secret_length(
    hub: &ClientHub,
    security: &SecurityContext,
) -> Result<Option<usize>, Box<dyn Error>> {
    let credstore = hub.get::<dyn CredStoreClientV1>()?;
    let key = SecretRef::new("my-api-key")?;
    let secret = credstore.get_secret(security, &key).await?;

    Ok(secret.map(|s| s.secret.as_bytes().len()))
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
    vendor: "constructorfabric" # GTS vendor used to discover the value-store plugin
    hierarchy:
      ancestor_cache_ttl_secs: 300
    list:
      max_limit: 200             # metadata-mode page-size cap
      secret_mode_cap: 25        # secret-mode ($select=…,secret) match-set cap
```

The config is `deny_unknown_fields`: the withdrawn `reaper:` and `gc:` blocks
are rejected at startup.

## License

Apache-2.0
