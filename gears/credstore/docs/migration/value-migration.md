Created:  2026-09-15 by Constructor Tech
Updated:  2026-10-02 by Constructor Tech

# Moving existing values into the immutable-versions store

> **Temporary document.** Delete it, together with the `docs/migration/`
> directory, once every deployment that held credentials written before ADR-0006
> has completed the procedure below and removed its superseded store entries.
>
> None of it applies to an installation created after ADR-0006 shipped: it never
> wrote a value at the old address, so `m0002` runs over an empty table and there
> is nothing to move. An installation whose old backend was the in-memory plugin
> has nothing to copy either (its values did not survive a restart): after `m0002`
> its rows are simply `declared`.

ADR-0006 changes how a value is addressed in the value store:

| | Old | New |
|---|---|---|
| Store key | `tenant_id` + `reference` + key class (owner or tenant) | `(tenant_id, record_id)`, where `record_id` is the row's `id` |
| Which value is current | the one value under the key | the immutable version the row points at: column `value_version` of `credstore_secrets` |
| Integrity check | fingerprint columns `value_fp` / `fp_key_id` with a fence key kept in the store | none; PostgreSQL alone decides which version is current |

A store key holds many immutable versions. A write is a `put` that returns an
opaque `value_version`; the row is then switched to that version in PostgreSQL.
A row with `value_version IS NULL` is `declared` (`status = 4`); a row with a
`value_version` is `active` (`status = 2`). The table enforces this pairing with a
`CHECK`, so the two columns are always set together.

No value written before ADR-0006 is reachable by the new contract: nothing was
ever written under the new key. `m0002` therefore cannot carry values; it
reshapes the schema and leaves every surviving row `declared`:

- adds `value_version` (`NULL`) and `fallback` (default `1`, `inherit`);
- narrows the `status` check to `(2, 4)`; codes `1` and `3` are retired;
- rows in `status IN (1, 3)` (unfinished writes and deletes) are deleted;
- every `active` row becomes `declared`;
- drops `value_fp`, `fp_key_id` and the index that swept unfinished rows.

`m0002` moves no store bytes and mints no `value_version`. There is **no gc
table, no maintenance job and no fingerprint fence** after it. Moving the values
is a separate, one-off job: **scripts run by the operator**, outside the gear,
against the database and the store's own API. The gear ships no such job.

**Part 1** is what the operator runs. **Part 2** is a prompt that generates the
three scripts Part 1 refers to.

---

# Part 1 — Running the migration

## The order, and why it is this order

```
1. copy.py       before m0002: copy every active value into the new store, verify by value_fp
2. m0002         the schema migration; drops value_fp, so it cannot come earlier than 1
3. activate.py   set value_version for every migrated row from the results of 1
4. test          verify reads
5. cleanup.py    only now, optionally: retire the old store entries
```

The migration is stop-the-world. The old and new key shapes and plugin contracts
cannot both serve the same reference, so a rolling or mixed-version rollout is not
supported.

**The result file of step 1 is the only link between a row and its new version.**
It maps each row id to the `value_version` the store returned. `m0002` demotes
every row to `declared`, so nothing in the database remembers which version
belongs to which row. Keep the file safe until step 3 has finished, and until the
test has passed.

`value_fp` is available only until `m0002` runs. That is why copy and
verification happen first, and why `m0002` is applied only after step 1 has
succeeded.

## Prerequisites

1. The new store is in place and is a versioned backend with ordered versions.
   For Vault / OpenBao that is a KV v2 mount with **`max_versions = 0`**,
   **`delete_version_after = 0`** and **`cas_required = false`**. These are the
   operator's obligation: the plugin does not check them at startup or later. A
   mount that evicts versions on its own would lose values the database still
   points at.
2. Register the new credential types in the types registry — the base type, every
   derived type, and any custom types of your own, under
   `gts.cf.core.credstore.credential.v1~`. This is additive; the old version does
   not see them.
3. Issue PDP policies for the new resource type and its six actions (`list`,
   `read`, `write`, `delete`, `read_secret`, `write_secret`). They are inert
   while the old version runs, **and that is what keeps authorization from
   failing the moment the new version starts.** See the type-scoped authorization
   ADR (0010) for who reissues what. Skipping this breaks every call regardless
   of how well the values migrate.
4. Remove any `static-credstore-plugin.config.secrets` block from deployment
   configuration. Config seeding was withdrawn and the field is now rejected, so
   the new version will refuse to start with it present.
5. **Take a database dump and a snapshot of the old store** (and of the new one,
   if it is not empty). Not optional: `m0002` is irreversible with respect to
   data.

## Before stopping the old version

6. Run `copy.py` in its show-only mode (read-only) and read its reconciliation
   report. Investigate anything it flags *before* going further — this is the
   last moment at which looking costs nothing.

## The migration itself (downtime)

7. Stop the old version.
8. Run `copy.py` for real. Read its report: `fp_mismatch` and `missing` should be
   zero, or each entry understood. The step must succeed before `m0002`.
9. Apply `m0002`, either as a `--migrate-only` run or by letting the new binary
   boot. **Point of no return for the database:** once it succeeds the fingerprints
   are gone, saga rows are deleted, and every row is `declared`; only
   `activate.py` or the dump brings the values back.
10. Run `activate.py`. Read its report: promoted, left `declared`.
11. Start the new version.

## Testing

12. Read a value, read an inherited value, list, rotate. A row left `declared`
    (see below) returns no value; that is expected and reported by `activate.py`.
    If anything else goes badly, roll back (below). The old store entries are still
    there, because only step 13 touches them.

## Afterwards

13. Optionally run `cleanup.py`. Nothing forces it: the superseded entries harm
    nothing beyond occupying space and holding a second copy of every secret.
    **Point of no return for the old store.** The old fence-key entry goes last.

## Rows that did not get a value

A row whose value was missing from the old store, or failed the fingerprint check,
is **not** copied and stays `declared`. `activate.py` marks it `fallback = 2`
(`none`). Reason: `fallback = 1` (`inherit`) would make resolution walk past the
row and up the tenant chain, so the moment an ancestor is given a value the
descendant would quietly serve *that* secret instead of returning an honest
"no value". `none` fails closed. These records need their secret provisioned
again by their owner.

## Verification

- After step 8: every `active` row is either in the result file with a
  `value_version`, or listed as `missing` / `fp_mismatch`.
- After step 10: no row has `status = 2` with `value_version IS NULL` (the table
  rejects that anyway), and the number of `status = 2` rows equals the number of
  promoted entries in the report.
- After step 11: reads through the gear return the expected values for a sample
  of records of each sharing mode, including an inherited one.

## Rollback

Restore the database dump and start the old version. This works at any point
before `cleanup.py` deletes anything, because the old entries are exactly what
the old version reads. The values written to the new store by `copy.py` are
inert: no restored row points at them; remove them with the store's own tooling
if they are unwanted. Policies and type registrations are additive and do not
interfere. Do **not** use a schema `down` migration against live data: it cannot
bring back fingerprints or dropped rows.

After `cleanup.py` has run, only backups remain — and a list of key names is not
a backup, so take a store snapshot if the backend offers one.

---

# Part 2 — The prompt

Everything below is meant to be handed to an assistant together with the material
listed under **Inputs**. It produces three scripts: `copy.py`, `activate.py`,
`cleanup.py`.

## Context

A credential store keeps metadata rows in PostgreSQL (`credstore_secrets`) and the
secret values themselves in a separate store, behind a small key-value API. The
address of a value is changing from `(tenant_id, reference, key_class)` to
`(tenant_id, record_id)`, where `record_id` is the row's `id`. Writing to the new
store is `put(key, value)`, which returns an opaque `value_version`; the row then
records that version in the column `value_version`. A SQL migration (`m0002`)
runs between your first and second script; it adds `value_version`, drops the
fingerprint columns and leaves every row `declared`. Your scripts move the values
across before it and wire the rows up after it.

### Constants

```
nil tenant          00000000-0000-0000-0000-000000000000
old fence key ref   cfs-internal-fence-key     (nil tenant, tenant key class)
```

### Column encodings in `credstore_secrets`

```
status    1 provisioning   2 active   3 deprovisioning   4 declared
sharing   1 private        2, 3 non-private
fallback  1 inherit        2 none          (column exists only after m0002)
```

### Deriving the old store address from a row

| `sharing` | Key class | Owner component of the address |
|---|---|---|
| `1` | owner | the row's `owner_id` |
| `2`, `3` | tenant | absent |

This is exactly how the old version built the key it passed to the plugin. Use
the key-formatting code from the old implementation supplied in **Inputs**; do not
reconstruct the format from this description.

## Script 1 — `copy.py`

Runs **before** `m0002`, on the shipped schema, with the old version stopped for
the real run (the show-only run may happen while it still serves).

Read from the database:

```sql
SELECT id, tenant_id, reference, sharing, owner_id, status, value_fp, fp_key_id
  FROM credstore_secrets
 ORDER BY tenant_id, reference;
```

Read the old fence key from the old store at the reserved entry above. If it is
absent, stop: no fingerprint can be verified.

Reconciliation report (show-only mode, and the start of the real run):

| Group | Expectation | If it does not hold |
|---|---|---|
| rows with `status = 2` | the old key **is** present in the store | Absent means the value is already gone; the row will end up without a value. Record it as `missing` |
| rows with `status IN (1, 3)` | may or may not be present | Not a discrepancy: unfinished writes or deletes. `m0002` deletes these rows; nothing is copied for them |

For every row with `status = 2`:

1. Read the value at the old key.
2. Verify the fingerprint: `HMAC-SHA256(fence_key, value)` must equal `value_fp`.
   On a mismatch **do not copy the value**; record it as `fp_mismatch`. This check
   is the point: a mismatch means the store entry is not the one this row is
   supposed to name, and copying it anyway would turn a stale or foreign value
   into a legitimate-looking current one.
3. `put` the value at the new key `(tenant_id, record_id = row id)`; keep the
   returned `value_version`.
4. Read it back by that `value_version` and compare it with the value read in
   step 1. A mismatch here is a store failure rather than a property of the data
   — stop.
5. Append `row id, tenant_id, value_version` to the result file. Leave the old key
   in place.

The result file also lists `missing` and `fp_mismatch` rows, each with its
reason, so `activate.py` knows exactly which rows to leave alone. It never
contains values.

**Idempotence.** The result file is the progress file. On a restart, skip rows
already in it. A `put` whose result was not recorded leaves an unreferenced
version of that key; it is harmless, and the next successful run supersedes it.

Exit non-zero if any step failed. The operator decides whether `missing` and
`fp_mismatch` entries are acceptable before running `m0002`.

## Script 2 — `activate.py`

Runs **after** `m0002`, before the new version starts.

For every entry in the result file with a `value_version`:

```sql
UPDATE credstore_secrets
   SET status = 2, value_version = %s, version = version + 1
 WHERE id = %s AND status = 4 AND value_version IS NULL;
```

`status` and `value_version` are set together because the table accepts only
`(declared, NULL)` or `(active, not NULL)`. Take `value_version` **from the result
file**; never mint or recompute it.

For every row left in `status = 4` because its value was missing or failed
verification (that is, listed as `missing` or `fp_mismatch`):

```sql
UPDATE credstore_secrets SET fallback = 2 WHERE id = %s AND status = 4;
```

Rows that were `declared` before the migration and are not in the result file are
left untouched.

**Idempotence.** The `AND status = 4 AND value_version IS NULL` predicate makes a
re-run safe: rows already activated are not touched. Before changing anything, check
that every id in the result file still exists; report ids that do not (the
record was deleted in the meantime).

Report: promoted, left `declared` with `fallback = 2`, unknown ids. No values.

## Script 3 — `cleanup.py`

Runs only after testing has passed. Deletes from the **old** store, driven by the
old keys recomputed from the result file and the pre-migration snapshot of the
rows:

| Entry | Action |
|---|---|
| copied and activated | delete the old key |
| row was in `status` `1` or `3` | delete the old key — the row is gone, nothing names it |
| `fp_mismatch` | **keep** — evidence, to be investigated separately |
| the old fence key | delete **last**, behind its own confirmation |

**The principal risk is touching a new key.** Delete strictly by the old keys the
result file produces, never by enumerating the store. If a candidate key has the
shape of a new key (`(tenant_id, record_id)`, with a UUID as its second
component), stop. If the old and new stores share a mount, this check is
mandatory.

## Requirements for all three scripts

- **Never log, print or persist a secret value.** Addresses, ids, fingerprints and
  `value_version`s only. The result file must not contain values.
- Two modes wherever anything is written or deleted: show first, act second.
- Restartable: an interruption must not prevent a clean re-run.
- One log line per processed entry, naming the address.
- Non-zero exit on any reconciliation failure, without proceeding.
- Deleting a key that is not there counts as success.

## Inputs

Supply alongside this prompt:

1. The old store API: how to read, write and delete, and what deleting an absent
   key returns.
2. The old implementation's key-formatting code — how
   `(tenant_id, reference, owner_id)` became a store key.
3. The new store API: `put` returning a `value_version`, read by `value_version`,
   and the new key formatting, so the scripts can prove they never touch a new key
   when deleting.
4. Database connection parameters.

Expected output: `copy.py`, `activate.py`, `cleanup.py`.
