Created:  2026-09-15 by Constructor Tech
Updated:  2026-09-15 by Constructor Tech

# Removing the superseded secrets from the backend (ADR-0006)

> **Temporary document.** Delete it, together with the whole `docs/migration/`
> directory, once **all** of the following hold:
>
> - [ ] This cleanup has completed on every deployment that carried rows written
>       before ADR-0006.
> - [ ] The entries deliberately retained as `fp_mismatch` evidence (§4, group D)
>       have been investigated and resolved.
> - [ ] The `credstore_value_migration` journal table has been dropped by a
>       migration.
> - [ ] [`value-migration.md`](value-migration.md) is being deleted in the same
>       change.
>
> Nothing here applies to an installation created after ADR-0006 shipped: it
> never wrote a value at an old address.

For the team running a backend plugin of their own. It doubles as a prompt: hand
it to an assistant together with the material listed in §8 and get a Python
script over your storage API.

**When to run it:** after the value migration has copied everything
([`value-migration.md`](value-migration.md)) and the new behaviour has been
tested. This is the last step, and it is irreversible.

<!-- toc -->

## 1. What happened, and why a cleanup is needed

ADR-0006 changes how a value is addressed in the backend:

| | Old address | New address |
|---|---|---|
| Composed of | `tenant_id` + `reference` + key class (owner or tenant) | `tenant_id` + `value_id` |
| Who knows the format | your plugin's old `get`/`put`/`delete` | your plugin's new implementation |

The two key spaces **cannot overlap**: a `value_id` is a fresh UUID that did not
exist before the migration.

The migration copies every value from its old address to a new one and **deletes
nothing**. So afterwards each value is in the store twice. This cleanup removes
the old copy.

## 2. Preconditions

Do not start until all of these hold:

- [ ] The `copy` command has run and its report has been read.
- [ ] The report says `fence_key` is `copied` or `already_present`. If it says
      `absent`, **stop** and investigate: without the fence key no fingerprint
      can be verified.
- [ ] The new version is up and tested: reading a value, reading an inherited
      value, listing, rotation.
- [ ] A database dump taken before the migration exists.
- [ ] The `credstore_value_migration` table is still present — nobody has dropped
      it.

## 3. Step 1 — export the expected list from the database

The source of truth is the migration journal. It was filled **before** the
migration destroyed anything, so it also covers entries whose rows no longer
exist.

```sql
SELECT secret_id,
       tenant_id,
       reference,
       owner_id,     -- NULL = tenant key class; non-NULL = owner key class
       outcome       -- 1=copied 2=missing 3=fp_mismatch 4=saga_row
  FROM credstore_value_migration
 ORDER BY tenant_id, reference;
```

One more address, which is not and cannot be in the journal — the **old fence
key**:

```
tenant_id = 00000000-0000-0000-0000-000000000000   (nil UUID)
reference = cfs-internal-fence-key
owner_id  = NULL
```

**Turning a triple into an address.** Use exactly the function your old
`CredStorePluginClientV1::get(ctx, tenant_id, key, owner_id)` used. Reuse that
formatting rather than reconstructing it from memory. The mapping rule is:
`owner_id` is non-`NULL` if and only if the row was private (`sharing = 1`),
which is precisely how the old gear passed it.

## 4. Step 2 — enumerate the old key space

Enumerate **the old prefix only**. New entries (`tenant_id` + `value_id`) must
not appear in the listing. If your API cannot filter them out by key shape,
filter on format: the second component of a new address is always a UUID, while
an old one is a `reference`, i.e. a string of `[A-Za-z0-9_-]` that is almost
never a UUID.

If a new address slips into the list and is deleted, you erase a live value that
a row in the database already points at. **This is the principal risk of the
whole procedure**, so verify the filter on its own: run it in listing-only mode
first and confirm by eye that no second component is a UUID.

## 5. Step 3 — reconcile

There is no literal one-to-one match here, and that is expected. What must agree
is something else: five groups, each with a known expectation.

| Group | Criterion | Expected in the backend | If it disagrees |
|---|---|---|---|
| **A** | `outcome = copied` | **must be present** | Missing → either the enumeration filter is wrong or somebody has already deleted things. Stop and investigate |
| **B** | `outcome = saga_row` | may or may not be present | Not a discrepancy: these are rows the migration deleted, and their values were never checked |
| **C** | `outcome = missing` | **must be absent** | Present → the copy wrongly concluded the value was lost and demoted the row. Stop: the row may need to be restored by hand |
| **D** | `outcome = fp_mismatch` | **must be present** | Present is correct. Missing → somebody has already deleted the evidence; record the fact |
| **E** | in the backend, not in the journal | **exactly one: the fence key** | Anything else is an orphan from a past failure or a hand-written entry. Stop: orphans are reviewed separately and never deleted automatically |

**The "100%" criterion**: all five groups match their expectation, and set E
consists of exactly one address — the old fence key. Only then proceed.

The script must print a verdict per group and, on any disagreement, exit
non-zero **without proceeding to deletion**.

## 6. Step 4 — back up

Before deleting anything, save:

1. **The deletion list** — groups A and B plus the fence key. A dated file, kept
   alongside the database dump.
2. **The retention list** — group D (`fp_mismatch`), so that what was left behind
   on purpose is visible later.

Worth weighing: a list of names **does not enable recovery**. If something turns
out to have been needed, names will not bring it back. Only a value snapshot or a
store snapshot will. If your backend can snapshot, take one; if it cannot, accept
deliberately that there is no way back after this step.

## 7. Step 5 — delete

The order matters.

1. **Groups A and B** — in batches, logging every address. Deleting an absent key
   must count as success, so that the procedure stays repeatable after an
   interruption.
2. **Leave group D alone.**
3. **The old fence key last**, as a separate step with its own confirmation. It
   has already been copied to the new address and is no longer read, but it is
   live key material; leaving it in a production secret store is worse than
   removing it. This step is what finally closes the door on rolling back, which
   is why it comes last.

## 8. Requirements on the script

- **Never read or print values.** The procedure needs addresses only. If the API
  returns a value alongside a key, do not log it, do not write it to a file, and
  do not hold it longer than necessary.
- **Two modes:** listing-only first (deletes nothing, prints the reconciliation
  and the lists), then deletion. The second runs only after the first reports a
  match.
- **Repeatable:** an interruption partway through must not prevent a re-run.
- **Batches and a log:** one line per deleted address, so that what went is
  auditable afterwards.
- **Non-zero exit** on any disagreement in step 3.

## 9. What not to touch

- Entries at new addresses (`tenant_id` + `value_id`) — those hold the live
  values.
- Rows in `credstore_secrets`.
- The `credstore_value_migration` table — a future migration drops it; until then
  it is the only record of what was removed and why.
- Group D (`fp_mismatch`) — evidence, not garbage.

## 10. Using this as a prompt

Supply, along with this text:

1. A description of your storage API: how to list keys under a prefix, how to
   delete a key, and what a delete of an absent key returns.
2. The old key format — the code of your old `get`, where the triple
   `(tenant_id, reference, owner_id)` becomes a storage key.
3. The new key format — the same for the new implementation, so that the filter
   in §4 provably cannot touch it.
4. Database connection parameters for exporting the journal.

Expected result: a single Python script with two modes (list / delete)
implementing §3–§7 and the requirements in §8.
