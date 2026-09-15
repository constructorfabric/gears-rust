Created:  2026-09-15 by Constructor Tech
Updated:  2026-09-15 by Constructor Tech

# One-Off Value Migration — old plugin address to `value_id` (ADR-0006)

> **Temporary document.** Delete it, together with the whole `docs/migration/`
> directory, once **all** of the following hold:
>
> - [ ] The `copy` command has run on every deployment that carried rows written
>       before ADR-0006, and its report was accepted.
> - [ ] The storage cleanup in [`storage-cleanup.md`](storage-cleanup.md) has
>       completed on those same deployments.
> - [ ] `CredStorePluginLegacyV1`, the `copy` command and the gear's start-up
>       guard have been removed from the code.
> - [ ] The `credstore_value_migration` journal table has been dropped by a
>       migration.
>
> Nothing here applies to an installation created after ADR-0006 shipped: it
> never held a value under the old address, so `m0002` is an ordinary migration
> over an empty table for it.

<!-- toc -->

## 1. What this solves

ADR-0006 changes how the backend plugin addresses a value: from
`(tenant_id, reference, owner_class)` to `(tenant_id, value_id)`. The two key
spaces cannot collide — a `value_id` is a fresh UUID that did not exist before
the migration — which also means **no value written before ADR-0006 is reachable
by the new code**. The plugin contract has no method that can name the old
address any more.

A deployment that already holds credentials therefore needs a one-off step that
reads every value at its old address and writes it back under a fresh
`value_id`. `m0002` alone cannot do it: a migration is handed a database
connection and nothing else.

## 2. Sequence

Downtime is required for steps 2–5.

| # | Step | Mechanism |
|---|---|---|
| 1 | SQL preflight | Read-only, against the running old version. Not code — see §9 |
| 2 | Stop the old version | — |
| 3 | `--migrate-only` | `m0002` in one transaction: journal, demotion, final schema. The process exits |
| 4 | `copy` | Fence key, values, row promotion, custom-type remap |
| 5 | Start the new version | Serves on the new addressing |
| 6 | Verify | The report from step 4, plus a smoke test through the API |
| 7 | Storage cleanup | A procedure, not code — [`storage-cleanup.md`](storage-cleanup.md) |

Step 3 as a separate invocation is optional — the migration would be applied
when step 4 boots anyway — but `run_migration_phases`
(`libs/toolkit/src/runtime/host_runtime.rs`) exists precisely for this, and an
explicit step separates "the schema changed" from "the values moved" in the
operator's log.

### Why the schema is migrated before the values are copied

Not a choice. The `db` phase runs before `init`
(`libs/toolkit/src/runtime/host_runtime.rs`), and the copy needs the backend
plugin, which registers itself during its own `init`
(`gears/credstore/plugins/static-credstore-plugin/src/gear.rs`). Within one
process the order is fixed.

That is also why the copy cannot rely on `value_fp` still being in the row: by
the time it runs, the migration has already nulled it. The journal preserves the
fingerprints instead (§3).

### Why the copy is a command and not a migration

`DatabaseCapability::migrations(&self)` (`libs/toolkit/src/contracts.rs`) takes
no context, so a migration object can never reach `ClientHub`, and therefore
never the plugin. Even with a context it would not help: at `db`-phase time the
plugin is not registered yet.

The converse holds too — DDL is only sanctioned through a migration. The runner
applies migrations on a privileged connection and gears never receive raw
database access (`libs/toolkit-db/src/migration_runner.rs`). Dropping the
journal table is therefore a job for some future migration, never for a command.

## 3. The journal

`m0002` creates and fills it **before** it nulls any fingerprint or deletes any
row. The journal exists so that the migration destroys nothing beyond recovery:

```sql
CREATE TABLE IF NOT EXISTS credstore_value_migration (
    secret_id     UUID PRIMARY KEY,   -- credstore_secrets.id
    tenant_id     UUID NOT NULL,
    reference     TEXT NOT NULL,
    owner_id      UUID NULL,          -- NULL = tenant key class (sharing <> private)
    old_value_fp  BYTEA NULL,         -- the fingerprint, before it is nulled
    old_fp_key_id SMALLINT NULL,
    old_status    SMALLINT NOT NULL,  -- 1/2/3, as it was
    new_value_id  UUID NULL,          -- written by `copy`
    outcome       SMALLINT NULL,      -- NULL=unprocessed 1=copied 2=missing 3=fp_mismatch 4=saga_row
    recorded_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

What it saves:

- **The fingerprints.** Without them the copy would have to recompute a
  fingerprint from whatever the backend returned, which would launder a poisoned
  entry into a valid-looking new version — exactly the hazard the fence in
  ADR-0003 was built for. With the journal the copy checks against the original
  fingerprint and refuses to carry anything that does not match.
- **The addresses of saga rows.** `m0002` deletes rows in status 1/3; without
  the journal their backend entries would be orphaned with nothing left to name
  them.
- **Precision and resumability** for both the copy and the cleanup.

On a fresh installation the journal is created empty and does nothing.

## 4. What `m0002` does

The order inside the migration matters:

1. Additive columns and indexes: `value_id`, `fallback` with
   `ck_credstore_fallback`, `uq_credstore_value_id`, `idx_credstore_type`, the
   `credstore_value_gc` table and its index.
2. `CREATE TABLE credstore_value_migration` (§3).
3. `INSERT INTO credstore_value_migration SELECT id, tenant_id, reference,
   CASE WHEN sharing = 1 THEN owner_id ELSE NULL END, value_fp, fp_key_id,
   status, … FROM credstore_secrets` — **before** anything destructive.
4. `DELETE FROM credstore_secrets WHERE status IN (1, 3)`. A `provisioning` row
   never became visible and a `deprovisioning` row was already invisible to
   resolution; their addresses are in the journal by now.
5. `UPDATE credstore_secrets SET status = 4, value_fp = NULL, fp_key_id = NULL,
   fallback = 2, version = version + 1 WHERE status = 2`.
   - `fallback = 2` (`none`) fails closed — see §8.
   - `version + 1` because the content changed: a client's cached `ETag` must
     stop matching.
6. Remap the built-in type: `UPDATE credstore_secrets SET secret_type_uuid =
   <new> WHERE secret_type_uuid = <old>`, both UUIDs being constants. Custom
   types are unknown to the migration; the copy picks them up (§5.3).
7. The final schema: `CHECK (status IN (2, 4))`, `ck_credstore_fp_with_value`,
   `DROP INDEX idx_credstore_pending`.

No branching on whether data is present: over an empty table steps 3–6 are
no-ops.

**This is one migration and one transaction.** The runner wraps `up()` together
with the history record in a transaction and rolls it back on failure
(`libs/toolkit-db/src/migration_runner.rs`), so the outcome is binary: either the
journal, the demotion and the final schema all landed, or nothing did and the
migration can simply be run again.

`down()` remains for schema symmetry but cannot restore values: they live in the
backend, which a migration cannot reach. Irreversibility of the data is a
property of the design, not an oversight.

## 5. The `copy` command

A one-off SDK trait of its own — not `CredStoreMaintenanceV1`, so that removing
it later does not touch the permanent contract. It is resolved from `ClientHub`
the same way `run_gc` is.

```rust
#[deprecated(note = "one-shot value migration; remove after the storage cleanup")]
pub trait CredStoreValueMigrationV1: Send + Sync {
    async fn run_copy(&self, ctx: &SecurityContext)
        -> Result<CopyReport, CredStoreError>;
}
```

### 5.1 The fence key, first

Read `legacy.get(nil_tenant, "cfs-internal-fence-key", None)`. If the new address
already holds something, leave it alone. If the old address is empty there are no
fingerprints either — note it in the report and carry on. Otherwise
`put(nil_tenant, FENCE_KEY_VALUE_ID, bytes)`.

Strictly before everything else: without the key the new version bootstraps a
fresh one, every fingerprint stored in the journal stops matching, and the
migrated values come back as 404.

### 5.2 Values

In batches over a journal cursor: `WHERE outcome IS NULL AND old_status = 2
ORDER BY secret_id LIMIT gc.batch_size`. For each entry:

1. `value_id = Uuid::new_v4()`
2. `legacy.get(tenant_id, reference, owner_id)`:
   - `None` — the value is already gone. Journal `outcome = missing`; the row
     stays `declared`.
   - `Some(v)` — `verify_fp(fence_key, v, old_value_fp)`:
     - mismatch: a poisoned or foreign value. **Do not carry it.** Journal
       `outcome = fp_mismatch`, the row stays `declared`, and it gets its own
       section in the report.
     - match: continue.
3. `put(tenant_id, value_id, v)` at the new address.
4. **Read back:** `plugin.get(tenant_id, value_id)` through the new path plus
   `verify_fp`. On a mismatch the row is not promoted and the run fails: this is
   a backend failure, not a property of the data.
5. One transaction promotes the row and marks the journal:
   ```sql
   UPDATE credstore_secrets
      SET status = 2, value_id = $1, value_fp = $2, fp_key_id = $3,
          fallback = 1, version = version + 1
    WHERE id = $4 AND status = 4;
   UPDATE credstore_value_migration
      SET new_value_id = $1, outcome = 1 WHERE secret_id = $4;
   ```
   The fingerprint is **restored from the journal**, never recomputed: it
   describes the value, not the address, and that is exactly what makes the check
   in step 2 meaningful. `fallback` returns to `1` (`inherit`) — the row is whole
   again.
6. The old entry is left in place. Removing it is the cleanup's job
   ([`storage-cleanup.md`](storage-cleanup.md)).

Idempotence and resumability rest on `outcome IS NULL`.

### 5.3 Remapping custom types

For every `secret_type_uuid` that is not one of the built-ins, compute the new
v5 UUID from the re-registered type id and apply the update. Whatever does not
resolve goes into the report as `unmapped_type`: those types must be registered
by hand, or their rows will fail resolution with `unknown_type`
(`gears/credstore/credstore/src/infra/types_registry.rs`).

### 5.4 The report

`migrated`, `missing`, `fp_mismatch`, `unmapped_type`, and
`fence_key: copied | already_present | absent`. No values — only `tenant_id` and
`reference`.

**The copy has no dry run.** It would only be honest before `m0002` was applied,
and `m0002` cannot be deferred: the `db` phase precedes `init`, and the runtime
has no "boot but do not migrate" mode — `RunMode` offers only `Full` and
`MigrateOnly`, and both run the `db` phase. So everything that can be checked in
advance is checked by the SQL preflight (§9), and everything else by the copy
itself, which refuses to promote what it could not verify and collects the
discrepancies in its report. The recovery path for a bad outcome is the dump plus
the old version; the old backend entries survive until the cleanup (§10).

## 6. Storage cleanup

Deleting the old entries is deliberately not code — the old key space is the
plugin's own business, with its own prefix and API. See
[`storage-cleanup.md`](storage-cleanup.md).

**It follows that the journal must survive until the cleanup is done.** It is the
only source of the old addresses: the saga rows are gone from
`credstore_secrets`, and the demoted rows have had their fingerprints nulled. A
future migration may drop the table — but only afterwards.

## 7. The legacy trait

One method: reading at the old address. Deletion is not needed, because the
cleanup script uses the plugin's own API (§6). The signature is identical to
`get` on today's `CredStorePluginClientV1`, so an existing implementation moves
over without a change to its body.

```rust
/// The superseded `(tenant, reference, owner_class)` addressing, read-only.
/// Exists for the duration of the one-off value migration (ADR-0006); removed
/// in the release that follows the storage cleanup.
#[async_trait]
#[deprecated(note = "one-shot value-migration shim; remove after the storage cleanup")]
pub trait CredStorePluginLegacyV1: Send + Sync {
    async fn get(
        &self,
        ctx: &SecurityContext,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Result<Option<SecretValue>, CredStoreError>;
}
```

The trait is optional: when it is absent from `ClientHub` the `copy` command
refuses to start with a clear error, and the gear's normal operation is
unaffected.

## 8. Guards, and one assumption

- **The gear refuses to start** while the journal still holds `outcome IS NULL`
  rows: step 4 was skipped, and the gear would otherwise serve rows that have no
  value. Five lines, removed together with the legacy trait.
- **The read-back** in the copy: a row is never promoted onto a value that has
  not been proven readable.
- **The check against the stored fingerprint**: a poisoned value is never
  carried.
- **The reconciliation before the cleanup** (`storage-cleanup.md` §4) blocks
  deletion until all five groups agree.

**The assumption.** Demoted rows get `fallback = 2` (`none`), failing closed.
`inherit` would mean resolution walks past the row and up the tenant chain:
harmless between steps 3 and 5, but if the copy failed to restore some rows, a
descendant would then quietly serve an ancestor's secret instead of returning an
honest 404. Rows the copy does promote get `fallback = 1` back.

## 9. The SQL preflight (not code)

Read-only queries, run against the live old version before it is stopped. No
risk, no deployment, and they catch what is expensive to learn late:

- inventory: how many rows in `status = 2`, how many saga rows in
  `status IN (1, 3)`;
- which `secret_type_uuid` values actually occur, and **whether each one is in
  the mapping table** — the only way to learn about `unmapped_type` in advance;
- violations of the old invariant: `value_fp IS NOT NULL AND fp_key_id IS NULL`
  and the converse;
- `sharing = 1` rows with a suspicious `owner_id`.

What this cannot tell you is whether the backend still holds a value at each old
address and whether its fingerprint matches. Only code that knows the old
addressing can read the backend, which means the old version or the copy itself.
A read-only preflight shipped in the old version was considered and dropped: the
"some values are missing from the backend" scenario costs extended downtime and a
restore, never data, because the old entries live until the manual cleanup.

## 10. Rollback

`m0002` is irreversible with respect to data: the values live in the backend,
which a migration cannot reach, so `down()` restores the shape of the schema and
nothing else.

In practice, up until the cleanup deletes anything: restore the database dump and
start the old version. The old backend entries are intact, because only the
cleanup touches them. Policies and type registrations are additive and will not
get in the way.

This covers "the copy went badly" too — a report full of `missing` or
`fp_mismatch` means: roll back with the dump, investigate, try again. Which is
precisely why the copy needs no dry run: a rehearsal would add no guarantee that
this does not already provide.

After the cleanup has deleted anything, only backups remain.

## 11. What the plugin owner changes

1. Add `impl CredStorePluginLegacyV1` — this is their current `get`, body
   unchanged. **Keep the old `delete` and the old key format**; the cleanup
   script needs them.
2. Rewrite `impl CredStorePluginClientV1` over `(tenant_id, value_id)` — a
   separate key space. A `put` to a `value_id` the store already holds must
   return `Conflict` (the fence-key bootstrap relies on that to settle a race
   between replicas); a `delete` of something absent is success.
3. Register both:
   ```rust
   ctx.client_hub().register_scoped::<dyn CredStorePluginClientV1>(scope.clone(), new_api);
   ctx.client_hub().register_scoped::<dyn CredStorePluginLegacyV1>(scope, legacy_api);
   ```
4. Drop the legacy implementation in the release that follows the cleanup.

## 12. Runbook

**Before the stop, while the old version runs:**

1. Register the new credential types (base, derived, custom) in the types
   registry. Additive; the old version does not see them.
2. Issue PDP policies for the new resource type `credential.v1~` and its six
   actions (`list`, `read`, `write`, `delete`, `read_secret`, `write_secret`).
   They are inert under the old version, which asks about the old type — which is
   what keeps authorization from breaking the moment the new version starts.
3. Back up: a database dump and whatever snapshot the backend supports.
   **Mandatory** — `m0002` is irreversible with respect to data.
4. Remove any `static-credstore-plugin.config.secrets` block from deployment
   configuration, or the new version will not start (`deny_unknown_fields`).
5. Run the SQL preflight (§9).

**The migration (downtime):**

6. Stop the old version.
7. `--migrate-only` → `m0002`. **The point of no return for the database:** once
   it succeeds, the values in the rows are gone, and only the copy or the dump
   brings them back.
8. `copy` → read the report. Expect the fence key found, `fp_mismatch` at zero,
   `missing` at zero.
9. Start the new version.
10. Verify: the report from step 8 plus a smoke test — read a value, read an
    inherited value, list, rotate.

**The cleanup, afterwards:** [`storage-cleanup.md`](storage-cleanup.md).

**Later:** retire the old PDP policies and type registrations; in a following
release remove the legacy trait, the `copy` command and the start-up guard, and
drop the journal with a migration — but only once the cleanup has finished.
