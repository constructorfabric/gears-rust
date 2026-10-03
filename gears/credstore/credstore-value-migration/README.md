# `CredStore` value migration

One-off, stop-the-world operator tool that moves the secret values of an
existing `CredStore` installation into the immutable-versions store introduced by
ADR-0006 (`CredStorePluginClientV2`), as described in the migration runbook
(`gears/credstore/docs/migration/value-migration.md`). It works for plugins whose
values can only be read through their own Rust code (for example in-process
AES-GCM encryption): you link **your old and your new plugin implementations**
into a small binary, and this crate provides the three stages and the command
line.

Read the runbook for the procedure, the order and the safety rules. This crate
implements its three stages:

| Stage | When | What |
|---|---|---|
| `copy` | before `m0002`, old version stopped | reads each `active` row's value from the old store, verifies the fence fingerprint, `put`s it under `(tenant_id, record_id = row id)`, reads it back, records the `value_version` in the results file |
| `activate` | after `m0002`, before the new version starts | sets `status = 2, value_version` for the copied rows; `fallback = 2` for rows left without a value |
| `cleanup` | after testing | deletes the superseded entries of the old store by their recorded addresses |

Every stage is a dry run unless `--apply` is given.

## Wiring

Implement [`LegacyValueStore`] as a thin adapter over the pre-0.3 plugin
(depend on the old SDK under a renamed dependency, for example
`credstore-sdk-v02 = { package = "cf-gears-credstore-sdk", version = "0.2" }`),
construct the new plugin directly (no `ClientHub`), and call [`run_cli`]:

```no_run
use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::CredStorePluginClientV2;
use credstore_value_migration::{LegacyStoreError, LegacyValueStore, run_cli};
use uuid::Uuid;

struct MyLegacy;

#[async_trait]
impl LegacyValueStore for MyLegacy {
    async fn get(
        &self,
        _tenant_id: Uuid,
        _reference: &str,
        _owner_id: Option<Uuid>,
    ) -> Result<Option<Vec<u8>>, LegacyStoreError> {
        // Call the old plugin's `get` (it decrypts in-process).
        Ok(None)
    }

    async fn delete(
        &self,
        _tenant_id: Uuid,
        _reference: &str,
        _owner_id: Option<Uuid>,
    ) -> Result<(), LegacyStoreError> {
        // Call the old plugin's `delete`; an absent entry is success.
        Ok(())
    }
}

# async fn build_new_plugin() -> Arc<dyn CredStorePluginClientV2> { unimplemented!() }
#[tokio::main]
async fn main() -> anyhow::Result<std::process::ExitCode> {
    let target = build_new_plugin().await;
    run_cli(Arc::new(MyLegacy), target).await
}
```

See `examples/migrate.rs` for a compilable skeleton.

```text
migrate --results r.jsonl --database-url postgres://... copy             # dry run
migrate --results r.jsonl --database-url postgres://... copy --apply
migrate --results r.jsonl --database-url postgres://... activate --apply
migrate --results r.jsonl cleanup --apply [--include-fence-key]
```

`--database-url` can also come from `CREDSTORE_MIGRATION_DATABASE_URL`.
`PostgreSQL` and `SQLite` are supported.

Exit codes: `0` success; `1` aborted; `2` a decision is needed. `copy` exits
`2` when rows end up without a value (`missing`, `fp_mismatch`,
`unknown_fence_key`): inspect the report and re-run the same command with
`--accept-losses` to proceed (the re-run resumes from the results file, so it
is quick). `activate` exits `2` for rows in a state it must not touch.

## What `copy` verifies (and mirrors from the shipped gear)

- The old fence key is read from the old store at nil tenant,
  `cfs-internal-fence-key`, tenant key class; it is stored as raw bytes. If it
  is absent and any row carries a fingerprint, the stage aborts.
- The old address of a row is `(tenant_id, reference, owner)` with
  `owner = Some(owner_id)` for `sharing = 1` (private) and `None` for
  `sharing = 2, 3` - exactly what the shipped gear passed to the plugin.
- `value_fp IS NOT NULL`: `fp_key_id` must be `1` (else `unknown_fence_key`),
  and `HMAC-SHA256(fence_key, value)` must match in constant time (else
  `fp_mismatch`). Neither is copied.
- `value_fp IS NULL`: the shipped gear served such out-of-band seeded rows on
  trust (and backfilled the fingerprint on read). They are copied and recorded
  as `unverified`, since nothing can be verified. Check the list.
- Statuses `1` and `3` are not copied; they are recorded as `unfinished` so
  `cleanup` can remove their legacy entries.
- After `put`, the value is read back by the returned version and compared;
  a difference aborts (a store failure, not a property of the data).
- Read-only report of *type-divergent pairs*: a private row whose type differs
  from the non-private row of the same `(tenant_id, reference)`. Both keep
  working; after the cutover a NEW private override with a differing type is
  rejected (`TYPE_MISMATCH_WITH_INHERITED`). Divergence between a tenant and an
  ancestor cannot be computed without the tenant hierarchy and is not reported.

## The results file

JSON Lines: a header
`{"format":"credstore-value-migration","version":1,"created_at":...}`, then one
entry per row: `id`, `tenant_id`, `reference`, `sharing`, `owner_id` (private
rows only), `status_before`, `outcome` (`copied`, `unverified`, `missing`,
`fp_mismatch`, `unknown_fence_key`, `unfinished`) and `value_version` (copied
outcomes). It never holds a secret value or fingerprint. Each entry is
appended and fsynced before the next row is touched. It is the **only** link
between a row and its new version: keep it until `activate` has run and the
migration is tested.

A re-run of `copy --apply` skips ids already in the file. A `put` whose entry
was not written (the process was killed in between) leaves an unreferenced
version of that key; it is harmless, nothing points at it and the next write
to that key reclaims it.

## Safety

- No secret value is logged, printed or persisted; one log line per processed
  entry names id, tenant, reference and outcome.
- `cleanup` deletes strictly by the old addresses in the results file, never by
  enumerating the store, keeps `fp_mismatch`/`unknown_fence_key` entries as
  evidence, deletes the fence key last and only with `--include-fence-key`, and
  aborts before deleting anything if a legacy reference parses as a UUID equal
  to a record id of the results file (a new-key shape).
- Take a database dump and a store snapshot first; `m0002` is irreversible with
  respect to data.
