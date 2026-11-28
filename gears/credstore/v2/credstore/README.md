Updated:  2026-10-06 by Constructor Tech

# `CredStore`

Stateful credential-storage gear module. Owns per-secret metadata in its own
database, enforces authorization in SQL, resolves secrets hierarchically across
the tenant tree, and stores the secret **value** in a backend plugin discovered
via the types registry.

> Design: the [technical design](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/DESIGN.md) is the baseline; the decision to
> ship a stateful gear (`credstore_secrets` table, PDP-scope authz,
> versioning/ETag, write protocol) instead of the original stateless design is
> recorded in [ADR-0001](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0001-cpt-cf-credstore-adr-stateful-gear.md);
> the addendum document that used to carry this history was folded into that
> ADR and removed (see git history for its prior content).

## Overview

The `cf-gears-credstore-v2` module provides:

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
  (barriers ignored — `shared` inherits through them); the backend is read once for the winner's value
- **Versioning** — strong generation-bound `ETag` (`"<id>.<version>"`) on `GET`,
  mandatory `If-Match` on `PUT`/`DELETE` (a validator, or `*` for explicit
  last-writer-wins; no ABA across recreation)
- **Crash-safe writes** — a secret write first announces itself in the
  gear's `credstore_write_intents` table (one row per attempt, with a lease on
  the database clock), then calls the plugin's `put(key, value)`, which
  returns an immutable version; one transaction then deletes the intent and
  switches the row's `value_version` pointer to the new version. Every store
  side effect is either announced before it happens or recorded as a cleanup
  debt in the transaction that learned it was needed. An ambiguous commit is
  resolved by one verification transaction (a locking read), and a
  transaction the database rolled back definitively is retried (up to 3
  attempts). No background work runs in the process: no workers or tasks, no
  timers, no database polling, no deferred tasks, no work at startup; every
  store side effect is executed by the request that caused it
- **Store cleanup debt in `PostgreSQL`** — the `credstore_store_cleanup` table
  holds one row per obligation: `purge` (`delete_key`) or `destroy` (only where
  the plugin exposes a `destroyer`). A debt is written in the same
  transaction that makes store content dead (a rotation, a secret removal, a
  lost compare-and-set, a record delete). After a **confirmed** commit the
  same request executes the debts it just recorded and deletes each row on
  success; a failure is logged and counted (`store_cleanup_failed{op}`), the
  row stays and the answer does not change. Every debt is idempotent and safe
  to execute later by any instance
- **Heal on access** — a read or write of a live record executes its pending
  debts; expired intents of a live record are deleted by the next successful
  secret write to it (its `destroy(below)` also covers a version a crashed
  writer may have landed), and a create or read of a reference whose earlier
  create failed deletes the expired intents and purges their keys. A writer
  whose intent was healed cannot commit and cleans up its own version (`503`).
  Safety never depends on the lease or on cleanup having run
- **Delete and purge** — deleting a record is one row transaction that also
  records a key purge debt; after the commit the same request calls the
  plugin's `delete_key(key)` and deletes the debt. The reference is free at
  once, so delete-then-recreate works immediately. A failed purge stays
  recorded: no later access retries it (the row is gone and the record id is
  never reused) until a possible external job
- **Audit** — every secret read and write is published to the credstore audit
  topic through the `event-broker` gear, best effort: a failure logs an error
  (never the secret) and counts `audit_publish_failed`, and the operation is
  unaffected
- **Metrics** — `read_outcome`, `walkup_depth`, dependency latency/health,
  `cross_tenant_denied`, `read_retry`, `audit_publish_failed`,
  and for the write protocol `write_intents_healed`,
  `write_commit_verified{op,outcome}`, `store_cleanup_recorded{op}` and
  `store_cleanup_failed{op}` with `op` = `purge` | `destroy`
  (`credstore_*_total` OpenTelemetry instruments); no inventory gauge, never a
  `COUNT` query
- **Backend plugin** — a versioned value store (`CredStorePluginClientV2`: `put`, `get`, `delete_key`, optional `destroy`) keyed by `(tenant_id, record_id)`, discovered via the types registry (vendor), among the plugin instances that run in the process
- **`ClientHub` + REST** — registers `CredStoreClientV2`; exposes `/credstore/v1/credentials`

This module depends on `tenant-resolver`, and consumes `types-registry` and
`authz-resolver`, and **requires a database**. It publishes its own GTS
declarations (the credential types, the permissions, the audit topic and event
type, and the SDK's plugin spec) through `gts_publication` before `init`. The secret value is stored in a plugin (e.g.
`cf-gears-static-credstore-plugin-v2`, or
[`cf-gears-vault-credstore-plugin-v2`](../plugins/vault-credstore-plugin/README.md),
backed by Vault/OpenBao).

A credential is created in one request — a `PUT` on the record address
carries the record and a tri-state `secret` together: a string writes it,
and an explicit `null` creates the record without one (`declared`), which
is also how a tenant suppresses an inherited credential without a row of
its own. A merge-`PATCH` on the same address edits metadata or
rotates/removes the secret without touching the rest of the record; and the
secret is read by naming it in `$select` on that same address or on the
collection — there is no dedicated secret address. Every value write stores a new immutable version in the backend under the
record key `(tenant_id, record_id)`, switches the row's `value_version`
pointer to it in one transaction, and the replaced versions are destroyed by
a cleanup debt recorded in that same transaction and executed by the request —
the model of Vault KV v2 and the cloud secret managers.
Expiry applies to the secret, not to the record. Nothing sweeps expired rows:
an expired record stays visible (status `expired`, normal validator) but its
secret is never served — a read of it fails `409 SECRET_EXPIRED`, without
falling through to an ancestor's value. The owner renews it in place with a
`PATCH` of `expires_at` or a replace; a create-only `PUT` over it is
`409 ALREADY_EXISTS`.

A version the backend holds but can never return (lost or rotated decryption
key, corrupt entry) — or one that is gone although the row's pointer did not
move — is permanent: the read fails `500` (canonical `Internal`, logged),
unlike the transient `503` for a version that vanished while the pointer moved.
The record must be rewritten or deleted; in a collection read with `secret` selected such
an item fails the whole request. Rotating
only the secret is a `PATCH` with only `secret`; `PUT` is a whole replace, so an
omitted expiry is cleared and `fallback` resets to `inherit` (the pre-0.3 `PUT`
preserved the expiry).

## Usage

After the gear initializes, consumers obtain its client from `ClientHub`. This
example reads a secret value (`get_secret`, the `read_secret` action) without
formatting or logging it; the record itself is read with `get_record` (`read`):

```no_run
use std::error::Error;

use credstore_sdk::{CredStoreClientV2, SecretRef};
use toolkit::ClientHub;
use toolkit_security::SecurityContext;

async fn secret_length(
    hub: &ClientHub,
    security: &SecurityContext,
) -> Result<Option<usize>, Box<dyn Error>> {
    let credstore = hub.get::<dyn CredStoreClientV2>()?;
    let key = SecretRef::new("my-api-key")?;
    let secret = credstore.get_secret(security, &key).await?;

    Ok(secret.map(|s| s.secret.as_bytes().len()))
}
```

## Configuration

The module requires a `database:` section (it is stateful). Gear config:

```yaml
credstore-v2:
  database:
    server: "sqlite_users"   # a database server template; module gets its own file
    file: "credstore.db"
  config:
    vendor: "constructorfabric" # GTS vendor used to discover the value-store plugin
    list:
      max_limit: 200             # page-size cap
    write:
      intent_lease_secs: 300     # after this, the intent of a crashed writer may be healed (>= 60);
                                 # must well exceed the longest store request (Vault: 90 s by default)
```

The config is `deny_unknown_fields`: the withdrawn `reaper:` and `gc:` blocks
are rejected at startup.

The cases this version does not handle are listed in
[`out-of-scope.md`](../../docs/out-of-scope.md); the protocol and its residual
garbage (never a dangling pointer or wrong bytes) are described in
[`DESIGN.md`](../../docs/DESIGN.md).

## License

Apache-2.0
