Created:  2026-09-15 by Constructor Tech
Updated:  2026-10-03 by Constructor Tech

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
- drops `value_fp`, `fp_key_id` and the index that swept unfinished rows;
- creates the empty write-intent table `credstore_write_intents` (internal
  bookkeeping of in-flight secret writes).

`m0002` moves no store bytes and mints no `value_version`. There is **no gc
table, no maintenance job and no fingerprint fence** after it. The tool runs
stop-the-world with no concurrent writer, so it records no write intents.
Moving the values is a separate, one-off job: the **value-migration tool**
(`gears/credstore/credstore-value-migration`, package
`cf-gears-credstore-value-migration`), run by the operator, outside the gear,
against the database and the stores. The gear does not contain or run it.

The tool is a library plus a command line, with three subcommands: `copy`,
`activate` and `cleanup`. Every one is a **dry run unless `--apply` is given**.
It cannot read your values by itself: you link it with your old and your new
store implementations into a small binary (Part 2).

**Part 1** is what the operator runs. **Part 2** is how to build the binary the
operator runs.

---

# Part 1 — Running the migration

Command line (`migrate` is whatever you named your binary):

```text
migrate --results r.jsonl --database-url postgres://... copy             # dry run
migrate --results r.jsonl --database-url postgres://... copy --apply
migrate --results r.jsonl --database-url postgres://... activate --apply
migrate --results r.jsonl cleanup --apply [--include-fence-key]
```

- `--database-url` is the credstore gear's database (`postgres://` or
  `sqlite://`); it can also be given in `CREDSTORE_MIGRATION_DATABASE_URL`.
  `cleanup` does not need it.
- `--results` is the results file (default `credstore-value-migration.jsonl` in
  the current directory), see below.
- Exit codes: `0` success; `1` the stage aborted (nothing proceeds; the reason is
  on stderr); `2` the stage needs a decision from you (`copy`: some rows end up
  without a value; `activate`: some rows are in a state it must not touch).

**The results file is the only link between a row and its new version.** It is
JSON Lines: a header, then one entry per row with the row id, tenant, reference,
sharing, the old owner (private rows), the status before, the outcome
(`copied`, `unverified`, `missing`, `fp_mismatch`, `unknown_fence_key`,
`unfinished`) and, for copied rows, the `value_version` the store returned. It
never holds a secret value or a fingerprint. `m0002` demotes every row to
`declared`, so nothing in the database remembers which version belongs to which
row. Keep the file safe (on durable storage, outside the host being rebuilt)
until `activate` has finished and the migration is tested; `cleanup` needs it
too.

## The order, and why it is this order

```
1. copy       before m0002: copy every active value into the new store, verify by value_fp
2. m0002      the schema migration; drops value_fp, so it cannot come earlier than 1
3. activate   set value_version for every migrated row from the results file of 1
4. test       verify reads
5. cleanup    only now, optionally: retire the old store entries
```

The migration is stop-the-world. The old and new key shapes and plugin contracts
cannot both serve the same reference, so a rolling or mixed-version rollout is not
supported.

`value_fp` is available only until `m0002` runs. That is why copy and
verification happen first, and why `m0002` is applied only after step 1 has
succeeded. `copy` and `activate` refuse to run against the wrong schema (`copy`
only before `m0002`, `activate` only after it).

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
   configuration. Config seeding was withdrawn and the field is now rejected
   (config validation fails at boot), so the new version will refuse to start
   with it present.
5. Build the migration binary (Part 2) and rehearse it, dry runs and `--apply`,
   against a restored copy of the database and a scratch store. Do not meet the
   tool for the first time inside the downtime window.
6. **Take a database dump and a snapshot of the old store** (and of the new one,
   if it is not empty). Not optional: `m0002` is irreversible with respect to
   data.

## Before stopping the old version

7. Run `copy` without `--apply` (read-only: it reads the database and the old
   store, and writes nothing to the new store, the database, the old store or the
   results file) and read its report. Investigate anything it flags *before*
   going further — this is the last moment at which looking costs nothing:
   - `MISSING`: an `active` row with no value in the old store;
   - `FP_MISMATCH` and `UNKNOWN_FENCE_KEY`: a value that failed the fingerprint
     check, or a row whose fingerprint names a fence key other than the one the
     shipped gear used. Neither is copied;
   - the count of rows **copied unverified**: rows with no fingerprint at all
     (seeded out of band), which the shipped gear served on trust. They are
     copied, but nothing can prove they are right; check the list;
   - the **type-divergent pairs** (below).

   If the old fence key is absent from the old store while any row carries a
   fingerprint, the stage aborts: no fingerprint can be verified.

## The migration itself (downtime)

8. Stop the old version.
9. Run `copy --apply`. It reads each `active` row's value from the old store,
   verifies the fingerprint, `put`s the value under
   `(tenant_id, record_id = row id)`, reads it back by the returned version and
   compares, and appends the outcome to the results file (flushed before the next
   row). Rows in the retired statuses `1` and `3` are recorded as `unfinished`
   and not copied. The old entries stay in place. If rows end up without a value
   the command exits `2` after printing them: understand each entry, then run the
   same command again with `--accept-losses` to proceed (it resumes from the
   results file, so it is quick). If it is interrupted, run it again: ids already
   in the file are skipped. A read-back that returns different bytes aborts the
   stage; that is a store failure, not a property of the data. The step must
   succeed before `m0002`.
10. Apply `m0002`, either as a `--migrate-only` run or by letting the new binary
    boot. **Point of no return for the database:** once it succeeds the
    fingerprints are gone, saga rows are deleted, and every row is `declared`;
    only `activate` or the dump brings the values back.
11. Run `activate` (first without `--apply` if you want to see the counts, then
    with it). It reads only the results file and the database. Read its report:
    promoted, left `declared` with `fallback = none`, already done, unknown ids
    (rows deleted in the meantime), ignored `unfinished` entries. Exit `2` lists
    rows in a state it left untouched (for example `active` with a different
    version): look at each of them before starting the new version.
12. Start the new version.

## Testing

13. Read a value, read an inherited value, list, rotate. A row left `declared`
    (see below) returns no value; that is expected and reported by `activate`.
    If anything else goes badly, roll back (below). The old store entries are still
    there, because only step 14 touches them.

## Afterwards

14. Optionally run `cleanup` (dry run first). Nothing forces it: the superseded
    entries harm nothing beyond occupying space and holding a second copy of every
    secret. **Point of no return for the old store.** It deletes strictly the old
    addresses recorded in the results file, never by enumerating the store; it
    keeps the `fp_mismatch` and `unknown_fence_key` entries as evidence; and it
    deletes the old fence-key entry last, **only** with `--include-fence-key`
    (after which the kept entries can no longer be verified). It refuses, before
    deleting anything, if a recorded old reference looks like a UUID equal to a
    record id of the results file, i.e. the shape of a new key (the old and new
    stores may share a mount). Deleting an absent entry counts as success, so a
    re-run is safe.

## Rows that did not get a value

A row whose value was missing from the old store, or failed the fingerprint check
(`missing`, `fp_mismatch`, `unknown_fence_key`), is **not** copied and stays
`declared`. `activate` marks it `fallback = 2` (`none`). Reason: `fallback = 1`
(`inherit`) would make resolution walk past the row and up the tenant chain, so
the moment an ancestor is given a value the descendant would quietly serve *that*
secret instead of returning an honest "no value". `none` fails closed. These
records need their secret provisioned again by their owner.

## Type-divergent pairs

`copy` also reports, read-only, every **same-tenant** pair of a private and a
non-private row with one reference whose secret types differ. Both rows keep
working after the cutover. What changes is that a **new** private override whose
type differs from the type it overrides is rejected with
`TYPE_MISMATCH_WITH_INHERITED`; records that already exist are not touched. The
report is information for the owners of those references (they may want to
align the types before writing new overrides). A divergence between a tenant and
an **ancestor** needs the tenant hierarchy and is not computed by the tool.

## Verification

- After step 9: every row of the table has an entry in the results file. Every
  `active` row is either `copied` / `unverified` with a `value_version`, or listed
  as `missing` / `fp_mismatch` / `unknown_fence_key`.
- After step 11: no row has `status = 2` with `value_version IS NULL` (the table
  rejects that anyway), and the number of `status = 2` rows equals the number of
  promoted entries in the report.
- After step 12: reads through the gear return the expected values for a sample
  of records of each sharing mode, including an inherited one.

## Rollback

Restore the database dump and start the old version. This works at any point
before `cleanup --apply` deletes anything, because the old entries are exactly
what the old version reads. The values written to the new store by `copy` are
inert: no restored row points at them; remove them with the store's own tooling
if they are unwanted. Policies and type registrations are additive and do not
interfere. Do **not** use a schema `down` migration against live data: it cannot
bring back fingerprints or dropped rows.

After `cleanup` has run, only backups remain — and a list of key names is not
a backup, so take a store snapshot if the backend offers one.

---

# Part 2 — Building the migration binary

## Who needs it

Any installation that holds values written before ADR-0006.

- An installation whose old backend was the **in-tree in-memory plugin** has
  nothing to copy; it needs neither the binary nor Part 1's `copy` / `activate` /
  `cleanup`.
- An installation on an **out-of-tree plugin** — in particular one that encrypts
  in-process, so that a value can only be read through the plugin's own code —
  needs the binary. The tool never talks to a store's wire format itself: it
  calls your old plugin code (to read and delete old entries) and your new plugin
  code (to write new versions), in the same process.

## The shape of the binary

A small Rust crate (your own, not part of the gears workspace) that depends on

- the tool's library, `cf-gears-credstore-value-migration` (library name
  `credstore_value_migration`);
- the current `cf-gears-credstore-sdk` (the V2 plugin contract);
- your **old** plugin code, under a renamed dependency for the old SDK it was
  written against, so the two SDK versions coexist, for example
  `credstore-sdk-v02 = { package = "cf-gears-credstore-sdk", version = "0.2" }`;
- your **new** plugin implementation.

It implements one trait and calls one function:

```rust
use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use credstore_value_migration::{LegacyStoreError, LegacyValueStore, run_cli};
use uuid::Uuid;

/// Adapter over the pre-0.3 plugin: addresses it exactly as the shipped gear did.
struct MyLegacy(OldPlugin);

#[async_trait]
impl LegacyValueStore for MyLegacy {
    /// `owner_id` is `Some` only for a private record; `None` otherwise.
    async fn get(
        &self,
        tenant_id: Uuid,
        reference: &str,
        owner_id: Option<Uuid>,
    ) -> Result<Option<Vec<u8>>, LegacyStoreError> {
        // Call the old plugin's `get` (it decrypts in-process); `Ok(None)` when absent.
        todo!()
    }

    async fn delete(
        &self,
        tenant_id: Uuid,
        reference: &str,
        owner_id: Option<Uuid>,
    ) -> Result<(), LegacyStoreError> {
        // Call the old plugin's `delete`; an absent entry is success.
        todo!()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    let legacy = Arc::new(MyLegacy(build_old_plugin().await?));
    let target = build_new_plugin().await?; // Arc<dyn CredStorePluginClientV2>
    run_cli(legacy, target).await
}
```

`gears/credstore/credstore-value-migration/examples/migrate.rs` is a compilable
skeleton of the same shape: copy it into your crate and fill in the placeholders.

What the adapter must get right:

- **Address the old plugin exactly as the shipped gear did.** The tool passes
  `(tenant_id, reference, owner_id)`, with `owner_id = Some(..)` for a private row
  and `None` for a tenant or shared row; turn it into whatever the old gear
  handed to the plugin (key class included), using the old gear's own code, not a
  reconstruction from a description.
- The **old fence key** is read through the same adapter, at the nil tenant
  (`00000000-0000-0000-0000-000000000000`), reference `cfs-internal-fence-key`,
  tenant key class (no owner); it is raw bytes. `cleanup --include-fence-key`
  deletes it through `delete` the same way.
- Never put a value into a `LegacyStoreError` message: the message is printed.

## Constructing the new store outside ClientHub

The tool calls the target through the V2 contract
(`CredStorePluginClientV2`: `put`, `get` by version, `delete_key`) on an
`Arc<dyn CredStorePluginClientV2>` you pass to `run_cli`. Build your new plugin
there directly, with the deployment's own configuration (endpoint, credentials,
encryption key), as the plugin's `init` would, but without registering anything
in the ClientHub and without starting the gear. It is called with a fixed
service identity of the tool in the `SecurityContext`; a plugin may use it for
correlation only and must not authorise on it.

Use the **same configuration the new gear version will use**, so that the
versions the tool writes are readable by the running gear: a different mount,
key or namespace makes the whole copy useless.

## What the plugin author ships before the cutover

1. A **`CredStorePluginClientV2` implementation**:
   - `put` stores a new immutable version under `(tenant_id, record_id)` and
     returns the provider's version; `get` returns exactly the bytes of that
     `put` or `None` when that version is gone;
   - if it supports `destroy` (and `supports_destroy` returns `true`), it must
     provide **ordered versions** per key; without `destroy` there is no cleanup
     of old versions (they stay until `delete_key`);
   - `get` may report `SecretUnreadable` for a version it holds but can never
     return (lost or rotated decryption key, corrupt entry), as opposed to a
     transient `ServiceUnavailable`. The tool reads each copied value back and
     aborts the stage on any error or difference, so a plugin that cannot read
     back what it just wrote is found during the rehearsal, not in production.
2. The **old (V1) code kept compiling**, for the migration binary only: the old
   `get` and `delete` over the old addresses, against the old SDK under a renamed
   dependency. It is not shipped with, or loaded by, the new gear.
3. A **built and rehearsed migration binary**, or the means for the operator to
   build one (the adapter and the two constructors above), before the downtime
   window opens (Part 1, prerequisite 5).

The binary, the old plugin code and the results file all handle plaintext
secrets or the means to read them: treat the host that runs them accordingly, and
remove the binary and the results file when the migration is verified. The
results file holds no values, but it is the key to which version belongs to which
row until `activate` has run.
