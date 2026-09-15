Created:  2026-09-15 by Constructor Tech
Updated:  2026-09-15 by Constructor Tech

# Moving existing values to the ADR-0006 address

> **Temporary document.** Delete it, together with the `docs/migration/`
> directory, once every deployment that held credentials written before ADR-0006
> has completed the procedure below and removed its superseded backend entries.
>
> None of it applies to an installation created after ADR-0006 shipped: it never
> wrote a value at the old address, so `m0002` runs over an empty table and there
> is nothing to move.

ADR-0006 changes the address a value is stored under in the backend plugin:

| | Old address | New address |
|---|---|---|
| Composed of | `tenant_id` + `reference` + key class (owner or tenant) | `tenant_id` + `value_id`, a fresh UUID4 |

The two key spaces cannot overlap, which is another way of saying that **no value
written before ADR-0006 is reachable by the new contract** — the plugin has no
method left that can name the old address.

`m0002` brings the schema to its new shape and, in doing so, empties the rows:
`status` becomes `4` (`declared`), and `value_id`, `value_fp` and `fp_key_id`
become `NULL`. Rows in `status IN (1, 3)` are deleted outright. Moving the values
across is therefore a separate, one-off job that runs outside the gear, against
the database and the backend's own API.

**Part 1** is what an operator runs. **Part 2** is a prompt that generates the
three scripts Part 1 refers to.

---

# Part 1 — Running the migration

## The order, and why it is this order

```
1. export.py      snapshot everything to CSV, old version still serving   ← the backup
2. m0002          the ordinary schema migration
3. migrate.py     copy values in the backend, promote the rows
4. test
5. cleanup.py     optional, later: delete the superseded backend entries
```

**The CSV from step 1 is the only source of truth for steps 3 and 5.** It holds
the fingerprints `m0002` is about to null and the addresses of the rows it is
about to delete. Without it the migration is not recoverable, and step 5 has
nothing to work from.

## Before stopping the old version

1. Register the new credential types in the types registry — the base type, every
   derived type, and any custom types of your own, under
   `gts.cf.core.credstore.credential.v1~`. This is additive; the old version does
   not see them.
2. Issue PDP policies for the new resource type and its six actions (`list`,
   `read`, `write`, `delete`, `read_secret`, `write_secret`). They are inert
   while the old version runs, which asks about the old type — **and that is
   what keeps authorization from failing the moment the new version starts.**
   Skipping this breaks every call regardless of how well the values migrate.
3. Remove any `static-credstore-plugin.config.secrets` block from deployment
   configuration. ADR-0006 withdrew config seeding and the field is now rejected,
   so the new version will refuse to start with it present.
4. **Take a database dump and a backend snapshot.** Not optional: step 2 is
   irreversible with respect to data.
5. Run `export.py` (read-only) and read its reconciliation report. Investigate
   anything it flags *before* going further — this is the last moment at which
   looking costs nothing.

## The migration itself (downtime)

6. Stop the old version.
7. Apply `m0002`, either as a `--migrate-only` run or by letting the new binary
   boot. **Point of no return for the database:** once it succeeds the rows hold
   no values, and only `migrate.py` or the dump brings them back.
8. Run `migrate.py`. Read its report: the fence key must be found, and both
   `fp_mismatch` and `missing` should be zero.
9. Start the new version.

## Testing

10. Read a value, read an inherited value, list, rotate. If any of it goes badly,
    restore the dump and bring the old version back — the old backend entries are
    still there, because only step 5 touches them.

## Afterwards

11. Optionally run `cleanup.py`. Nothing forces it: the superseded entries harm
    nothing beyond occupying space and holding a second copy of every secret.
    **Point of no return for the backend**, and the old fence key goes last.

## Rollback

Restore the database dump and start the old version. This works at any point
before `cleanup.py` deletes anything, because the values it would delete are
exactly what the old version reads. Policies and type registrations are additive
and do not interfere.

After `cleanup.py` has run, only backups remain — and a list of key names is not
a backup, so take a snapshot if the backend offers one.

---

# Part 2 — The prompt

Everything below is meant to be handed to an assistant together with the material
listed under **Inputs**. It produces three scripts: `export.py`, `migrate.py`,
`cleanup.py`.

## Context

A credential store keeps metadata rows in PostgreSQL (`credstore_secrets`) and
the secret values themselves in a separate backend, behind a small key-value API.
The address of a value in that backend is changing from
`(tenant_id, reference, key_class)` to `(tenant_id, value_id)`, where `value_id`
is a fresh UUID4 minted per value. A SQL migration (`m0002`) has already been
written; it updates the schema and empties the rows. Your scripts move the values
across and put the rows back together.

### Constants

```
nil tenant             00000000-0000-0000-0000-000000000000
old fence key ref      cfs-internal-fence-key
new fence key value_id f7252add-b079-558f-81e1-7a03b14a9cc9
old generic type uuid  2a8aac98-cf09-58ed-acd6-f599f35cb5bf
new generic type uuid  c57822de-3aae-58b7-b712-71d907c999e2
```

### Column encodings in `credstore_secrets`

```
status    1 provisioning   2 active   3 deprovisioning   4 declared
sharing   1 private        2, 3 non-private
fallback  1 inherit        2 none
```

### Deriving the old backend address from a row

| `sharing` | Key class | Owner component of the address |
|---|---|---|
| `1` | owner | the row's `owner_id` |
| `2`, `3` | tenant | absent |

This is exactly how the old gear built the argument it passed to the plugin. Use
the key-formatting code from the old implementation supplied in **Inputs**; do
not reconstruct the format from this description.

## Script 1 — `export.py`

Runs before the migration, while the old version is still serving. Read-only.

Read from the database:

```sql
SELECT id, tenant_id, reference, sharing, owner_id,
       status, value_fp, fp_key_id, secret_type_uuid, version, expires_at
  FROM credstore_secrets
 ORDER BY tenant_id, reference;
```

Enumerate the old key space in the backend. Add one more entry that exists in the
backend but can never have a row: the old fence key, at the nil tenant under the
reference `cfs-internal-fence-key`, tenant key class.

Write a CSV with one line per entry:

```
secret_id, tenant_id, reference, sharing, owner_id, status,
value_fp_hex, fp_key_id, secret_type_uuid, old_key, present_in_backend, kind
```

`value_fp_hex` is the `bytea` fingerprint rendered as hex; `old_key` is the
assembled backend address; `kind` is `row` or `fence_key`.

Print a reconciliation report over three groups:

| Group | Expectation | If it does not hold |
|---|---|---|
| rows with `status = 2` | the key **is** present in the backend | Absent means the value is already gone; those rows will end up without a value. Record them |
| rows with `status IN (1, 3)` | may or may not be present | Not a discrepancy: these are unfinished sagas |
| present in the backend, absent from the CSV | **the fence key and nothing else** | Anything else is an orphan from a past failure. Stop and investigate before migrating |

## Script 2 — `migrate.py`

Runs after `m0002`, before the new version starts.

**Step 0 — the fence key, strictly first.** Read it at the old address and write
it at `(nil tenant, f7252add-b079-558f-81e1-7a03b14a9cc9)`. If the new address
already holds something, leave it alone. If the old address is empty, carry on
but say so in the report.

This must come first because the fence key is what every fingerprint was computed
under. If the new version boots without finding it there, it generates a fresh
one, every fingerprint in the CSV stops matching, and the migrated values come
back as 404.

**Step 1 — the values.** For every CSV line with `kind = row` and `status = 2`:

1. Mint `value_id = uuid4()`.
2. Read the value at `old_key`.
3. Verify the fingerprint: `HMAC-SHA256(fence_key, value)` must equal
   `bytes.fromhex(value_fp_hex)`. On a mismatch **do not carry the value**;
   record it as `fp_mismatch` and leave the row without one. This check is the
   point: a mismatch means the backend entry is not the one this row is supposed
   to name, and copying it anyway would turn a stale or foreign value into a
   legitimate-looking new version.
4. Write the value at the new address `(tenant_id, value_id)`.
5. Read it back from the new address and verify the fingerprint again. A mismatch
   here is a backend failure rather than a property of the data — stop.
6. Promote the row:
   ```sql
   UPDATE credstore_secrets
      SET status = 2, value_id = %s, value_fp = %s, fp_key_id = %s,
          version = version + 1
    WHERE id = %s AND status = 4;
   ```
   The fingerprint comes **from the CSV**; never recompute it.
7. Leave the old key in place.

**Step 2 — rows that did not get a value.** For every row left in `status = 4`
because its value was missing or its fingerprint did not match:

```sql
UPDATE credstore_secrets SET fallback = 2 WHERE id = %s AND status = 4;
```

`fallback = 1` (`inherit`) would make resolution walk past the row and up the
tenant chain, so the moment an ancestor is given a value the descendant would
quietly serve **that** secret instead of returning an honest 404. `2` (`none`)
fails closed.

**Step 3 — type ids.** The type UUID changed:

```sql
UPDATE credstore_secrets SET secret_type_uuid = %s WHERE secret_type_uuid = %s;
```

Apply it for the generic type using the constants above, and for every custom
type pair supplied in **Inputs**. Any `secret_type_uuid` left unmapped will fail
resolution with `unknown_type`, so report what remains unmapped.

**Idempotence.** The `AND status = 4` predicate makes a re-run safe: rows already
promoted are not touched. Track processed lines in a progress file so a restart
does not re-read the backend from the beginning.

Report: promoted, missing from the backend, `fp_mismatch`, unmapped types. No
values.

## Script 3 — `cleanup.py`

Runs only after testing has passed. Deletes from the backend, driven by the CSV:

| CSV line | Action |
|---|---|
| `kind = row`, promoted successfully | delete |
| `kind = row`, `status` was `1` or `3` | delete — the row is gone, nothing names the key |
| `kind = row`, `fp_mismatch` | **keep** — evidence, to be investigated separately |
| `kind = fence_key` | delete **last**, behind its own confirmation |

**The principal risk is touching a new address.** Delete strictly by the
`old_key` column of the CSV, never by enumerating the backend. The second
component of a new address is always a UUID; if a UUID turns up in the deletion
list, stop.

## Requirements for all three scripts

- **Never log, print or persist a secret value.** Addresses and fingerprints
  only. The CSV must not contain values.
- Two modes wherever anything is written or deleted: show first, act second.
- Restartable: an interruption must not prevent a clean re-run.
- One log line per processed entry, naming the address.
- Non-zero exit on any reconciliation failure, without proceeding.
- Deleting a key that is not there counts as success.

## Inputs

Supply alongside this prompt:

1. The backend API: how to list keys under a prefix, read, write and delete, and
   what deleting an absent key returns.
2. The old implementation's key-formatting code — how
   `(tenant_id, reference, owner_id)` became a backend key.
3. The new implementation's key-formatting code, so the scripts can prove they
   never touch a new address.
4. Database connection parameters.
5. Custom credential types, if any, as old/new UUID pairs.

Expected output: `export.py`, `migrate.py`, `cleanup.py`.
