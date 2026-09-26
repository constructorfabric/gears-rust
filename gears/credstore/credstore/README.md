# `CredStore`

Stateful credential-storage gear module. Owns per-secret metadata, enforces
authorization in SQL, resolves secrets hierarchically across the tenant
tree, and stores the secret **value** in a backend plugin discovered via the
types registry.

> Design: [technical design](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/DESIGN.md) is the baseline; the decision to
> ship a stateful gear (`credstore_secrets` table, PDP-scope authz,
> versioning/ETag, write saga) instead of the original stateless design is
> in [ADR-0001](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0001-cpt-cf-credstore-adr-stateful-gear.md).

## Overview

The `cf-gears-credstore` module provides:

- **Local metadata** — a gear-owned `credstore_secrets` table (`SecureORM` /
  sea-orm, migration `m0001`) holding sharing, owner, status, `version`;
  planned (ADR-0006) adds `value_id`, pointing to its current immutable
  backend version
- **One item shape for record and secret** (planned, ADR-0004) — point read
  and collection share one shape; `secret` appears only when `$select`
  names it, under `read_secret`; `null` creates a credential with no
  secret — also how a tenant suppresses an inherited credential with no
  row of its own (planned, ADR-0007/ADR-0008)
- **PDP authorization** — `AccessScope` enforced in SQL via `SecureORM`
  clamps; out-of-scope access fails closed (canonical 404, anti-enumeration)
- **Hierarchical resolution** — one indexed query over the ancestor chain
  (TTL+LRU cached; `shared` inherits across barriers); backend read once,
  for the winner only
- **Value-fingerprint fence** — every read verifies the backend value against a
  per-row `HMAC-SHA256` (key stored in the backend, never on the wire); a
  desync fails closed instead of disclosing under a foreign sharing label
  (DESIGN §4.10, ADR-0003; no healing path under ADR-0006)
- **Versioning** — strong `ETag` (`"<id>.<version>"`) on `GET`; mandatory
  `If-Match` on `PUT`/`DELETE` (validator, or `*` for last-writer-wins; no
  ABA across recreation)
- **Crash-safe writes** — each value is a new immutable version under a fresh
  id; the pointer switches to it in one transaction; old versions are
  deleted right after; leftovers and expired records are swept by a
  periodic maintenance job, not a reaper (planned, ADR-0006; until then:
  provisioning→backend→active, with rollback and a reaper)
- **Backend plugin** — value-only store discovered via types registry (vendor)
- **`ClientHub` + REST** — registers `CredStoreClientV1`; exposes `/credstore/v1/secrets`

Depends on `types-registry`, `tenant-resolver` and `authz-resolver`;
**requires a database**. Secret values live in a plugin (e.g.
`cf-gears-static-credstore-plugin`, or OpenBao-backed).

**Planned direction (not yet implemented).** [ADR-0004](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md),
[ADR-0005](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md),
[ADR-0007](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0007-cpt-cf-credstore-adr-record-write-verbs.md)
and [ADR-0010](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0010-cpt-cf-credstore-adr-type-scoped-authorization.md)
keep the record and its optional secret as one item: the point read and the
upward-rooted listing share it, carrying a secret only when `$select` names
it under `read_secret` (serving as the capped, non-paginated bulk secret
read), and rename the PDP resource type to
`gts.cf.core.credstore.credential.v1~`, replacing the three shipped actions
with six (`list`, `read`, `write`, `delete` on the record; `read_secret`,
`write_secret` on the secret). Creation becomes one request: `PUT` carries
the record and a tri-state `secret` — a string writes it, `null` creates
the record without one (`declared`), also how a tenant suppresses an
inherited credential with no row of its own (ADR-0008); a merge-`PATCH`
edits metadata or rotates/removes the secret; no dedicated secret address.
[ADR-0006](https://github.com/constructorfabric/gears-rust/blob/main/gears/credstore/docs/ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md)
replaces in-place overwrite with immutable value versions — the model of Vault KV v2 and the cloud secret managers, shadow paging with a `git gc`-style collector: every write
mints a fresh version id, switches the row's pointer to it atomically, and
deletes the replaced version; a periodic maintenance job outside the gear
collects what that delete missed — so the saga and its reaper go away.

## Usage

After the gear initializes, consumers obtain its client from `ClientHub`;
this retrieves a secret without logging its value:

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
    let response = credstore.get(security, &key).await?;

    Ok(response.map(|secret| secret.value.as_bytes().len()))
}
```

## Configuration

Requires a `database:` section (stateful gear). Config:

```yaml
credstore:
  database:
    server: "sqlite_users"   # a database server template; module gets its own file
    file: "credstore.db"
  config:
    vendor: "constructorfabric" # GTS vendor used to discover the value-store plugin
    hierarchy:
      ancestor_cache_ttl_secs: 300
    reaper:
      tick_secs: 60
      provisioning_timeout_secs: 300
      # planned, ADR-0006: removed — replaced by the `gc` job
      # (`gc.pending_max_age_secs: 3600`, `gc.batch_size: 256`), run as
      # `credstore gc` on an operator-chosen schedule, not inside the gear
```

## License

Apache-2.0
