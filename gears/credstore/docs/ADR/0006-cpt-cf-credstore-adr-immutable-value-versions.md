---
status: accepted
date: 2026-09-10
---

Created:  2026-09-10 by Constructor Tech
Updated:  2026-10-03 by Constructor Tech

# ADR-0006: Immutable Value Versions with a Pointer in the Row

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Data](#data)
  - [The plugin contract](#the-plugin-contract)
  - [The protocol](#the-protocol)
  - [Why destroy is safe, and why `delete_key` is not used on the remove-secret path](#why-destroy-is-safe-and-why-delete_key-is-not-used-on-the-remove-secret-path)
  - [Why reclaim is safe, and what the lease is for](#why-reclaim-is-safe-and-what-the-lease-is-for)
  - [What remains](#what-remains)
  - [Statuses, and what is withdrawn](#statuses-and-what-is-withdrawn)
  - [The pattern](#the-pattern)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Revisit Triggers](#revisit-triggers)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-immutable-value-versions`

## Context and Problem Statement

Shipped ([ADR-0001](0001-cpt-cf-credstore-adr-stateful-gear.md)–[0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md)): the backend key `(tenant_id, reference, key-class)` is reused, so rotation overwrites in place; a write is a saga tracked by `status` (`provisioning`, `active`, `deprovisioning`, `declared`); `value_fp = HMAC(fence_key, value)` detects a row/backend mismatch and fails the read closed until a healing `If-Match: *` re-write; guarded and `If-Match: *` writes use opposite orderings; `deprovisioning` holds a name until the backend delete completes. Four hazards — a torn write reading as absence, a stuck `provisioning` row wedging a name, `deprovisioning` holding a name hostage, two write orderings — share one cause: row and backend can be observed mid-write, disagreeing about which bytes belong to a reference. Fence, saga, reaper and name retention manage that disagreement after the fact. Can it be made impossible, without a gc table, a maintenance job or any cleanup that correctness depends on? And, once it is, can the garbage a failure leaves behind be accounted for — every unreachable secret version covered by a durable obligation to remove it — instead of merely hoped to be removed by a later write?

## Decision Drivers

- **D1** — a row never points at bytes other than the ones it describes; unreachable, not detected and healed.
- **D2** — a failure between the stores costs garbage, never a wrong value, a closed read or a wedged name.
- **D3** — one write ordering, whatever the precondition.
- **D4** — no name retention; no statuses beyond "has a value" and "has none".
- **D5** — the plugin is a versioned kv store: `put` returns the provider's version of the stored value; no list, no CAS, no transactions.
- **D6** — a read stays one SQL query plus one backend read.
- **D7** — no resident loop and no maintenance job; every store side effect after a commit is an outbox task enqueued in that commit, and every `put` is announced in PG first (a write intent); reclaim runs at startup and piggybacks on writes.
- **D8** — hygiene: an unreachable secret version is always covered by a durable obligation (an outbox task or an open write intent), except the documented residuals. Safety (D1) never depends on it.

## Considered Options

- **O1** — in-place overwrite, saga and fence (shipped).
- **O2** — gear-minted `value_id` per write: every write lands under a fresh random key `(tenant_id, value_id)`, the row holds a pointer, a `credstore_value_gc` table with a `pending` intent tracks unreferenced versions, and a periodic maintenance job (`run_gc`) drains it (withdrawn).
- **O3** — backend-native versioning (Vault KV v2, GCP Secret Manager `SecretVersion`) used directly as the plugin contract.
- **O3'** — plugin-minted ordered versions under one key per record, with **inline best-effort cleanup** and no intents: the provider assigns a value version on `put` (ordered, where `destroy` is supported), the row holds it as a pointer, on a backend that supports `destroy` the writer destroys older versions inline after the commit (and its own version after a lost CAS, and the old version after a secret removal), and record deletion purges the key through the platform transactional outbox. This is what this ADR said before the review of aviator5 showed that rotated, removed and orphaned secrets can be retained forever when an inline call is skipped or the writer stops (four scenarios). Withdrawn.
- **O3'+** — O3' with **write intents and outbox-driven cleanup**: every `put` is announced in a PG table before it happens, every store side effect after a commit (destroy of older versions, of a lost version, of a removed version, purge of a key) is an outbox task enqueued in the very transaction that learned it is needed, and a reclaim of expired intents (at startup and piggybacked on writes) enqueues the purge of what a dead writer may have left.
- **O4** — two-phase commit or an outbox coordinating row and backend as one distributed transaction for every write.

## Decision Outcome

**Chosen: O3'+.** The row stays authoritative for metadata and holds `value_version`, the value version the provider returned from `put` (`NULL` = `declared`; not the row's own `version` counter). All versions of a record live under one store key `(tenant_id, record_id)`; a version is never overwritten. The principle: **every side effect on the value store is either announced in PostgreSQL before it happens (a write intent before each `put`) or executed by an outbox task enqueued in the same PG transaction that learned it is needed; no best-effort store call is ever relied on for cleanup.** A write announces itself, puts the bytes, then in one PG transaction retires its intent, switches the pointer and enqueues the destroy of older versions (where the backend supports `destroy`). A secret removal switches the pointer to `NULL` and enqueues the destroys in the same transaction. Deleting a record deletes the row and enqueues a key purge in the same transaction. An intent whose writer is gone is reclaimed after its lease, which enqueues the purge of the key when the record has no row. O1 detects instead of preventing and meets none of D1–D8. O2 meets D1–D4 but needs a gc table, an intent row, a maintenance job and a fence key address just to find its own garbage. O3' finds its garbage by position (below the pointer) and meets D1–D6, but its cleanup is an inline best-effort call, not an outbox task (D7), and a stopped or failing writer never repeats it, so D8 fails whenever that call is lost. O3'+ keeps the position-based cleanup and makes the obligation durable; its price is one small intent table and one extra short PG transaction per secret write. O4 fails on grounds unrelated to correctness (Pros and Cons).

### Data

`credstore_secrets` (authoritative for metadata) holds:

- `id` — the record identity, minted at create and never reused: a re-create mints a new `id`. It is also the record part of the store key.
- `version` — the row version for optimistic concurrency, +1 on every change, metadata or secret.
- `value_version` — the value version of the current secret in the store, as returned by `put` (TEXT, `NULL` means `declared`, no secret). It is distinct from `version`, the row's optimistic-concurrency counter (the **row version**, bumped on every change of the row and used in the `ETag` `(id, version)` and the CAS); the **value version** identifies the stored secret bytes.

The client validator is `(id, version)`. A new `id` on re-create means a validator from before a delete never matches: no ABA, no tombstones. The store holds, under `(tenant_id, record_id)`, immutable versions identified by the value versions the provider assigned.

`credstore_write_intents` (internal bookkeeping, not exposed and not tenant-scoped through the PDP) holds one row per secret write attempt between its announcement and its commit:

- `attempt_id` — primary key, minted per attempt (random, never reused);
- `tenant_id`, `record_id` — the store key the attempt will `put` under;
- `lease_until` — database clock, `now() + lease`; nobody may reclaim the intent before it. Indexed.

It is a before-the-fact log, not a task queue. The lease and the reclaim batch are gear configuration, the block `write`: `intent_lease_secs` (default 300, at least 60 — the lease must comfortably exceed a plugin `put`, whose own timeout the gear cannot see) and `reclaim_batch` (default 16, `> 0`).

Store cleanup runs on the platform transactional outbox, one queue `credstore.store_cleanup`, partitioned by store key so that the tasks of one key are delivered in order. Two kinds of task, both idempotent and delivered at least once: `purge` (call `delete_key`) and `destroy` (a selector, `Below` or `Exactly`, and a version; call `destroy`, or acknowledge without a call when the plugin does not support it). A transient error is retried, a malformed payload is rejected, and a `destroy` on a purged key succeeds. `destroy` tasks are enqueued only when the selected plugin supports `destroy`; `purge` tasks always.

### The plugin contract

Three required operations and one optional. Every operation takes the record key explicitly, `key = (tenant_id, record_id)`, where `record_id` is the row's `id` (minted at create, never reused); the gear chooses the key and the plugin only maps it to a physical location under its installation prefix, so the reference, type and sharing are not part of the key. Every call also carries the request context, used only for correlation, never for authorization. Required: `put(key, value) -> version` (durable; the provider chooses the value version and `put` returns it, and the gear stores it in `value_version`), `get(key, version) -> Option<bytes>` (exactly the bytes written by the `put` that returned `version`, not found, or a permanent "unreadable" outcome meaning the version exists but can never be read, e.g. a lost decryption key — never different bytes) and `delete_key(key)` (delete the key with all its versions; idempotent). Optional: `destroy(key, selector)`, idempotent, where the selector is `Below(version)` (every version older than `version`) or `Exactly(version)` (that one version). The plugin declares whether it supports `destroy`. A backend that supports `destroy` **MUST** return **ordered versions per key** (a `put` that starts after another `put` on the same key has returned gets a greater version); a backend without `destroy` does not need ordered versions, because `Below(vv)` is safe only when every version older than a committed version belongs to a writer with a stale base. Every backend must also provide **durability** (`put` returns only after the bytes are durable) and **exact bytes**. Not required: CAS, listing, cross-key transactions, linearizability beyond the version ordering, server-side logic.

Backends with `destroy` and ordered versions: Vault and OpenBao KV v2 (the KV version number; the mount must never evict a version on its own, so unlimited retention is required), Google Cloud Secret Manager (version numbers), a PostgreSQL plugin (a sequence-backed row id in an insert-only table), the in-memory plugin (a per-key counter) and a file plugin (a per-key monotonic counter, not a random id). Backends without `destroy`: AWS Secrets Manager (a version cannot be deleted; version ids are UUIDs, so unordered) and Azure Key Vault (a version can only be disabled; ids are opaque). They provide the three required operations only and accept the residual below: a rotated or removed secret stays in the backend until the record is deleted. The per-provider mapping is in DESIGN §4.3 and the requirement is `cpt-cf-credstore-fr-backend-compatibility`.

### The protocol

**Write** (`PUT` create or replace, `PATCH` carrying `secret`):

1. **Read and check.** Read the row (none for create). Check the client precondition (`Matches` = `(id, version)`, or `Exists`), the PDP on the row's type (create: the request's type) before any store call, and the type traits. Create over an own row is 409, an expired one included (the expired record is visible: renew or delete it). A replace or `PATCH` finds an expired own row like a live one and updates it in place (renewal, same id). A create mints its record id here.
2. **Announce (tx0).** In its own transaction, insert the write intent `(attempt_id, tenant_id, record_id, lease_until = now() + lease)`, with a fresh `attempt_id`. A failure is 503 and nothing was written to the store. The writer remembers a process-monotonic start instant.
3. **Lease guard.** If more than half of the lease has elapsed since tx0 committed (monotonic clock), the writer does **not** put: it deletes its own intent, best effort, and returns 503. This bounds the window in which a stalled writer can still land a version after its intent could have been reclaimed (reclaim never claims an intent before `lease_until`).
4. **Put.** `vv = put(key, value)` under the record's key; `vv` is the value version the provider assigned. A store error is 503; the intent stays and reclaim handles it; an orphan version may exist (the failure can be ambiguous: the bytes can have persisted despite it).
5. **Commit (tx1), one PG transaction.** First delete the intent by `attempt_id`; it must affect exactly one row. Zero rows means the intent was reclaimed: roll the whole transaction back and go to 5c. Otherwise, in the same transaction:
   - *create*: insert the row `active` with `value_version = vv`. A unique violation (the reference is taken) is a definite loss: the intent deletion still commits together with an enqueued `purge` of the key (the freshly minted record id can never get a row), and the answer is 409 `ALREADY_EXISTS`;
   - *replace / patch*: the CAS on the row version read in step 1. Success: enqueue `destroy(Below(vv))` (backend with `destroy`). Zero rows is a definite loss: in the same transaction enqueue `destroy(Exactly(vv))` if a row with this record id still exists, otherwise `purge` of the key; answer 409 `OPTIMISTIC_LOCK_FAILURE`. An `Exists` (last-writer-wins) update re-reads and retries once from step 1 with a **new** attempt before it returns 409;
   - tx1 fails ambiguously (PG unavailable or timed out: the commit may or may not have happened): return 503 and do nothing more. If the commit happened, its tasks committed with it; if not, the intent is still open and reclaim takes it.

   Committed: reply with the new validator (create 201 + `Location` + `ETag`; replace/patch 204 + `ETag`).

   **5c. Intent lost.** The writer is still alive and knows `vv`: in a new transaction it enqueues `destroy(Exactly(vv))` if the row with its record id exists, otherwise `purge` of the key, and returns 503. If this transaction also fails, the failure is logged and counted (`write_intent_lost_total` counts the loss itself); what remains is residual 2 below.
6. **Piggyback reclaim.** After step 5, whatever its outcome, run a reclaim pass in its own transaction, best effort: a failure is logged and counted and never changes the reply.

There is no inline `destroy` anywhere in the write path. **No create over an expired own row**: an expired record still holds the reference, so a create-only write over it ends at step 1 with 409. Only a delete enqueues a purge for a record that existed.

**Metadata-only patch** (no `secret` member): one PG CAS (`version + 1`); no store call and no intent.

**Remove the secret** (`PATCH {"secret": null}`, including suppression `{"fallback": "none", "secret": null}`, and a `PUT` with `null` over an `active` row): one PG transaction does the CAS that sets `value_version = NULL` (`declared`) and, where `destroy` is supported, enqueues `destroy(Below(old_vv))` and `destroy(Exactly(old_vv))`, `old_vv` being the pointer that transaction nulled. No intent (nothing is put) and no inline destroy.

**Read**: one PG query resolves the hierarchy and picks the decisive row; the PDP is evaluated on its type before any store call. A metadata-only read or a listing replies from PG and never touches the store. When the secret is selected: `get(key, value_version)`; found → reply; not found (a concurrent write switched the pointer and a destroy task removed the old version) → re-read the row once and, if `value_version` changed, `get` again; a second miss after the pointer moved is 503. If the pointer did not move and the version is gone, or the plugin reports the version permanently unreadable, the read answers `SECRET_UNREADABLE` (409, canonical `FailedPrecondition` with the HTTP status overridden, like `SECRET_EXPIRED`) at once, with no retry: retrying cannot help, and the record must be rewritten (`PUT`/`PATCH` with a secret) or deleted. In secret mode (bulk collection read) such an item is returned with its metadata and without the secret, like an expired one. `value_version IS NULL` (`declared`) is never a store call. Expiry applies to the secret, not to the record: it is evaluated from the row at read time, and when the decisive row is `active` with `expires_at` in the past the read answers `SECRET_EXPIRED` (409) before any store call, never continuing to an ancestor's value; the record's metadata stays readable with status `expired`.

**Delete record**: read the row, check the precondition and the PDP (no row: 404). One PG transaction deletes the row (CAS on `version` for `Matches`) and enqueues `purge` of `(tenant_id, record_id)`. The reference is free at once; a re-create mints a new id and a new key, which a lagging purge cannot touch. Reply 204.

**Reclaim** is one transaction: take up to `reclaim_batch` intents whose `lease_until` has passed (skipping rows locked by a concurrent pass), delete them, and for each one enqueue `purge` of its key if **no row** with its record id exists (an existence probe, never a count); if the row exists enqueue nothing (the orphan above the pointer of a live record is residual 1). It runs at startup before the gear reports ready, repeated until a pass returns fewer than the batch (a bounded number of passes; an error is logged and the gear still starts), and after every secret write (step 6). There is no resident loop and no timer.

**Outbox handlers.** `purge` calls `delete_key`; `destroy` calls `destroy` with the task's selector and version (no call when the plugin does not support it). Both are idempotent, so at-least-once delivery is harmless, and both may run at any time after their commit.

| Failure | State after | Who cleans up |
|---|---|---|
| Step 1 fails or the precondition is refused | Nothing written or changed | Nothing to clean up |
| Step 2 fails (announce) | Nothing written to the store; 503 | Nothing to clean up |
| Step 3 trips (lease guard) | Nothing put; own intent deleted (or left, if that delete fails); 503 | Nothing; a left intent is reclaimed (it enqueues a purge of an empty key at most) |
| Step 4 fails, or the writer crashes after `put` before step 5 | PG unchanged; intent open; an orphan version may exist; 503 | Reclaim after the lease: `purge` if the record has no row (a crashed create, a record deleted meanwhile); if the row exists the orphan sits above the pointer (residual 1): the next successful write's `destroy(Below)` or the delete's `purge` |
| Step 5 loses (0 rows, unique violation) | Winner's row stands; this writer's version is unreferenced; 409 | The task enqueued in tx1: `destroy(Exactly(vv))`, or `purge` when no row with this record id exists |
| Step 5 ambiguous (PG unavailable or timeout) | Commit may or may not have happened; `vv` is kept because the row may point at it; 503 | If it committed, its tasks are durable; if not, the intent is open and reclaim takes it. Nothing deletes `vv` inline |
| 5c: intent found reclaimed | tx1 rolled back; 503 | The writer's own task (`destroy(Exactly(vv))` or `purge`); if that transaction also fails: residual 2 |
| Writer crashes after commit | Row points at `vv`; older versions remain below the pointer | The `destroy(Below(vv))` task, already durable; without `destroy`, record delete or tenant offboarding |
| A `destroy` or `purge` task fails | Versions remain until it succeeds | The outbox retries; counted in `store_cleanup_failed_total` |
| Remove-secret: PG transaction fails | Row exactly as it was | Nothing to clean up |
| Remove-secret: committed | Row `declared`; old versions below and at `old_vv` remain until delivery | The two tasks committed with it |
| Delete: PG transaction fails | Row and reference exactly as they were | Nothing to clean up |
| Delete: committed, `delete_key` fails | Row already gone, reference already free; key still holds versions | The outbox retries until it succeeds |
| A reclaim pass fails | The intents stay | The next pass (a later write or the next startup); logged and counted in `write_intent_reclaim_failed_total` |

A crash costs at most orphaned versions under one record key, and, outside the residuals below, each is covered by an open intent or a task until it is gone. Two guarded writers: the loser's CAS matches nothing → 409, and its tx1 enqueues the removal of its own version. Two `If-Match: *` writers: both announce and put; the first to commit owns the pointer, the other re-reads, retries once from step 1 with a new attempt and commits a later version, or returns 409.

### Why destroy is safe, and why `delete_key` is not used on the remove-secret path

`destroy(Below(vv))` deletes versions by position, so it is safe only because versions are ordered; this argument applies to `Below` only, and a backend without `destroy` needs neither it nor ordered versions. Let writer W commit `vv = r` and destroy below `r`. Another writer X can be in one of two situations. If X's `put` is **older** than `r` (X put, or read its base row, before W's commit), X holds a stale base: W's commit bumped `version`, so X's CAS matches nothing and X destroys its own version — the version W destroys was never going to be a pointer. If X's `put` is **newer** than `r`, X read the row after W's commit, which was after W's `put` returned, so by the ordering guarantee X's version is greater than `r` and `destroy(Below(r))` never touches it. Hence committed versions strictly increase in commit order, the row's current `value_version` is never destroyed, and a pointer never dangles. Without ordered versions neither half of the argument holds, which is why a backend without them cannot support `destroy`. `destroy(Exactly(vv))` needs no ordering: it names a version no row references.

The argument does not depend on **when** the destroy runs. The outbox executes `destroy(Below(vv))` after the commit, possibly much later and after further writes: a version committed later was put after `vv`'s commit and is therefore greater than `vv`, so a `Below(vv)` that runs later still only touches versions that are older than a committed pointer and were never going to become one. The same holds for the pair `Below(old_vv)` and `Exactly(old_vv)` of a secret removal, which names only versions the removal itself made unreachable.

`delete_key` is never used on the remove-secret path. Removing the secret leaves the record alive as `declared`, and its key will be reused by the next write. A concurrent writer may already have put a newer version under that key and be about to commit, or may do so right after the removal commit; `delete_key` would delete that version before its pointer switch commits and leave a dangling pointer. The remove-secret path therefore deletes by position and by exact version (`destroy(Below(old_vv))`, `destroy(Exactly(old_vv))`), which can only touch versions the removal itself made unreachable. `delete_key` runs only after the row is gone, from the outbox, against a key no future record will use (record ids are never reused).

### Why reclaim is safe, and what the lease is for

Reclaim deletes an intent and may enqueue a `purge` of the key. It cannot dangle a pointer, whenever it runs and even against a writer that is still alive, for two reasons. First, tx1 can commit only while its intent exists: deleting it is the first thing tx1 does and it must affect exactly one row, so once a reclaim has taken the intent no pointer to the attempt's version can ever be committed; the writer rolls back and cleans up its own version (5c). Second, reclaim purges only a key whose record has no row, and record ids are never reused, so nobody will read that key again. **Safety therefore never depends on the lease.**

The lease decides *hygiene*: when reclaim may act. An intent is protected until `lease_until`, and a writer that is past the lease guard (step 3) has at least half a lease left; the lease is chosen far above the plugin's `put` timeout, so a live writer finishes its `put` before its intent can be reclaimed. If that assumption fails (a writer paused between the guard and the end of `put` for longer than the remaining lease), the `put` can land after reclaim purged the key; the writer's tx1 then finds its intent gone and 5c enqueues the cleanup, unless the writer also stops before 5c: that is residual 2.

### What remains

Residual garbage — exactly these four, each documented and bounded:

1. **Orphan version above the highest committed pointer of a live record** (a crash, or an ambiguous tx1, after `put` on replace or patch; it also survives a later secret removal): never served; removed by the record's next secret write (`destroy(Below)`) or by its delete (`purge`). It cannot be found without a plugin listing, which the contract does not have.
2. **The lease window.** A writer paused between the lease guard and the end of `put` for longer than the remaining lease may land a version after reclaim purged the key; if it then also stops before 5c, that version leaks. Closing it needs a conditional put in the plugin contract (for example Vault KV v2 `cas`): not done.
3. **Backends without `destroy`** (AWS Secrets Manager, Azure Key Vault) keep rotated and removed versions, and orphans under a live key, until the record is deleted (`delete_key`) or the tenant is offboarded. A rotated or removed secret is retained by such a backend until record deletion; this is the accepted behaviour. A key whose record has no row is still purged.
4. **Intents of a crashed instance whose lease has not expired at the next startup** are reclaimed by later writes (the piggyback pass) or by the next startup.

Not garbage, stated for completeness:

- **Integrity** comes from immutable versions and the exact-bytes `get`; a backend that returns different bytes violates its contract. Out-of-band tampering detection is not provided by the gear.
- **The PG database is the system of record for metadata** and must be backed up together with the store; the index is not rebuildable from the store (no listing). Index rebuild is out of scope.
- **Audit** events for secret reads and writes go through the platform event-broker gear and are best-effort: if publishing fails, the operation continues, an error is logged without the secret and the failure is counted in a metric.

### Statuses, and what is withdrawn

`CHECK (status IN (2, 4))` admits `active` and `declared` only; `provisioning`/`deprovisioning` are retired, their codes reserved. A row is either consistent — `value_version` names an existing version — or `declared` with no value version. `declared` is reached only on purpose: `PATCH {"secret": null}` on an existing record, or a creating `PUT` with an explicit `"secret": null` (no store call at all). `value_version IS NULL` holds exactly for `declared`.

Withdrawn by this ADR, relative to the shipped design and to O2: the `credstore_value_gc` table and its `pending` / `superseded` / `removed` / `aborted` reasons; the maintenance job, its entry point with its `gc` config and counters; the fingerprint fence — `value_fp`, `fp_key_id`, the stored fence key and its reserved address ([ADR-0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md)): a row/backend mismatch is impossible by construction, so there is nothing for it to detect; the random per-write `value_id`; the **inline best-effort `destroy`** after a commit, after a lost CAS and after a secret removal, with the counters that described it (`destroy_failed`, `outbox_purge_failed`) and the queue `credstore.key_purge`, replaced by the intent journal, the queue `credstore.store_cleanup` and its counters. Kept: no name retention, two statuses, the generation-bound validator.

### The pattern

Nothing here is novel. Immutable versioned secrets with a pointer to the current one is how Vault KV v2, GCP Secret Manager, AWS Secrets Manager and Azure Key Vault work; credstore keeps the pointer in the metadata row so a versioned kv store qualifies (D5). The atomic pointer switch is copy-on-write / shadow paging (Lorie, 1977). Destroying below the committed pointer is garbage collection by position, which ordered versions make safe without a reachability scan. Every cleanup and every delete through the transactional outbox is the textbook pattern for a side effect that must follow a commit; the write intent is the matching pattern for a side effect that must be recorded before it happens (an intent log, as in write-ahead logging).

The reaper is removed, not reshaped. A saga has intermediate states that are wrong until repaired, and repairing them was the reaper's job. Immutable versions leave nothing to repair, and the leftover garbage is covered by a durable obligation, executed by the outbox or enqueued by the next reclaim, not by a timer.

### Consequences

- D1 is structural; a failure costs at most an orphaned version (D2); one ordering (D3); two statuses, no name retention (D4); a versioned kv plugin (three required operations plus an optional `destroy`) with no list and no CAS (D5); one query plus one backend read, with a rare bounded retry (D6); no resident loop and no job: the asynchronous work is the outbox's `purge` and `destroy` tasks, and reclaim runs at startup and after writes (D7); every unreachable version is covered by a task or an open intent, except four documented residuals (D8).
- Cost: one small intent table and one extra short PG transaction (tx0) per secret write; the hygiene guarantee rests on a lease assumption (a live writer finishes its `put` before its lease expires) that safety does not; an outbox task per successful secret write on a backend with `destroy`; a backend without `destroy` retains rotated and removed versions until record deletion; a backend with `destroy` must provide ordered versions; orphans above the pointer of a never-rewritten live record survive until its delete; the PG database is a system of record that needs a backup consistent with the store; an expired credential lingers (its metadata visible, its secret withheld) until it is renewed in place or deleted by its owner.
- Breaking for plugin authors only (`CredStorePluginClientV2`); no REST or SDK-consumer change — this ADR sits below [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md)'s surface. The reaper, its config and metrics are withdrawn; the counters `write_intents_reclaimed_total`, `write_intent_lost_total`, `write_intent_reclaim_failed_total`, `store_cleanup_enqueued_total` and `store_cleanup_failed_total` (the last two per operation, `purge` or `destroy`), together with the read counters (`read_retry`, `secret_unreadable`), replace them (DESIGN §10). The one consumer-visible addition is the `SECRET_UNREADABLE` answer above.
- **Data migration** is not the gear's work: the schema migration leaves every row `declared`; a one-off operator tool shipped with the gear's sources but not part of the gear — linked by the operator with the old and the new store implementations, so values an encrypting plugin keeps behind its own code are read in-process — carries each value to the record's key through `put`, sets `value_version`, and marks what it could not carry `fallback: none`; the copy runs first, on the shipped schema and verified against the fingerprints, and the schema migration (which drops the fingerprint columns) is applied only after the copy has succeeded; the old fence key entry is removed afterwards (DESIGN §8). The tool runs stop-the-world with no concurrent writer, so it records no write intents.

### Confirmation

- E2E, the five scenarios of the review: (1) a replace commits and the process stops before any cleanup runs: the `destroy(Below)` task is in the outbox and, once delivered, the old version is gone; (2) a secret removal likewise: both destroy tasks are in the outbox with the commit; (3) a replace killed after `put` leaves an orphan above the pointer and an open intent, the prior value keeps serving, and the orphan is removed by the record's next successful write or by its delete (residual 1); (4) a create killed after `put` leaves an open intent, and after the lease the next reclaim enqueues the purge of the key; (5) a writer that put after a delete's purge ran finds its CAS lost and enqueues the removal of its own version (`purge`, the row being gone).
- E2E: a CAS lost at the commit leaves the winner serving and, on a backend with `destroy`, enqueues the removal of the loser's version in the same transaction; a create that loses to a concurrent create enqueues a `purge`; two concurrent `If-Match: *` writers both complete or one returns 409, and the row points at a committed version (J1, J3); a PG timeout at the commit returns 503 and the version is not deleted inline.
- E2E, reclaim versus a live writer: an intent reclaimed while its writer is between `put` and the commit makes tx1 roll back, the answer is 503, the writer enqueues the cleanup of its own version and no pointer to it exists; a reclaim never takes an intent whose lease has not expired; startup reclaim runs before the gear reports ready, and a failing reclaim does not stop the gear.
- E2E, lease guard: a writer that finds more than half of the lease spent after tx0 does not call `put`, deletes its own intent and answers 503.
- E2E: a read racing a pointer switch plus a delivered destroy retries once and succeeds; a second miss after the pointer moved is 503, never a stale or empty value (J6). A row whose pointer did not move but whose version the store lacks, or which the plugin reports unreadable, answers 409 `SECRET_UNREADABLE` without a retry and is counted by `secret_unreadable`; in secret mode the item is returned without its secret. `DELETE` immediately followed by `PUT` on the same reference succeeds without 409, the new record has a new id and key, and the lagging outbox purge does not remove the new value (J7).
- E2E: on a backend without `destroy`, writes, reads and record deletion behave identically and no `destroy` task is enqueued or executed (`purge` tasks still are); a failed outbox task is retried until it succeeds and is counted in `store_cleanup_failed_total`.
- Contract: versions under one key strictly increase in real-time `put` order (J4); the bytes at the row's `value_version` are the bytes of the write that committed it (J2); metadata-only reads, listings and metadata-only patches make no store call and write no intent (J5); `status` never holds a retired code and `value_version IS NULL` exactly for `declared`; the write path makes no `destroy` call itself (the plugin sees `destroy` only from the outbox handler); no timer task and no maintenance entry point exist in the gear process.

## Revisit Triggers

- A target backend that must reclaim superseded versions cannot provide ordered versions (`destroy(Below)` requires them; such a backend needs its own ordered counter or must run without `destroy` and accept retention until record deletion).
- Orphan retention for never-rewritten records hurts (residual 1): orphan versions of a live record that is never written again survive until its delete. A bounded sweep could be added then; it is deliberately not part of this decision.
- The lease window (residual 2) must be closed: a conditional put in the plugin contract (for example Vault KV v2 `cas`) would let a late `put` fail instead of landing after a purge.
- The platform outbox gains delayed delivery and transactional cancel: a delayed cleanup message cancelled by the commit could replace the intent table.
- The PG database must become rebuildable from the store: that needs a listing capability on the plugin or a metadata copy in the store, neither of which is provided.
- Out-of-band tampering detection becomes a requirement: that needs a mechanism the fence's removal gave up (ADR-0003), and a story for key custody.

## Pros and Cons of the Options

- **O1 (shipped)** — Good: no migration, no SPI change, no backend garbage. Bad: a torn write serves 404 until a re-put (D1, D2); a stuck `provisioning` row wedges creation (D2, D4); `deprovisioning` holds a name hostage to a race its own key scheme creates (D4); two write orderings (D3).
- **O2 (withdrawn)** — Good: D1–D4 are met; plugin needs only `get`/`put`/`delete`. Bad: the gear mints ids the store cannot order, so it must record every intent in a `credstore_value_gc` table, run a maintenance job to drain it and reclaim stale `pending` rows, keep a fixed fence-key id, and still carries a narrow race between the job's backend delete and its gc delete; more moving parts (a table, a job, a config block, a trait, counters) than the problem needs.
- **O3' (inline best-effort cleanup, withdrawn)** — Good: no extra table, no intent row, no job; garbage is found by position; the backend is dumb (D5); one protocol for every precondition (D3); one PG transaction per write. Bad: the cleanup is a best-effort call after the commit, so a rotation whose destroy was skipped or whose writer stopped leaves the old version below the pointer, a secret removal whose destroy was lost leaves the removed secret in the store, a crashed create leaves a version under an id that never became a row, and a writer that put after a delete's purge leaves a version under a dead key; in every case nobody is responsible for the version and it can be retained forever (four scenarios). A backend with `destroy` must provide ordered versions, and one without it retains superseded versions until record deletion.
- **O3'+ (chosen)** — Good: every unreachable version is covered by a durable obligation, an outbox task or an open intent, except four documented residuals (D8); garbage is still found by position, with no listing and no gc table; the backend stays dumb (D5); one protocol for every precondition (D3); safety does not depend on the lease or on any cleanup. Bad: one small intent table and one extra short PG transaction per secret write; the hygiene guarantee rests on a lease assumption (a live writer finishes its `put` before its lease expires), closable only with a conditional put in the plugin contract; a backend with `destroy` must provide ordered versions, and one without it retains superseded versions until record deletion; orphans above the pointer of a never-rewritten live record linger until its delete; the PG database becomes a system of record that must be backed up with the store. Rejected variants: delayed destruction (a `not_before` on the task) lengthens the retention of old secrets, and reads already tolerate a destroy through one re-read; a separate cleanup-task table duplicates the outbox, which already is the durable executor, and the intent journal is a before-the-fact log, not a task queue; recovery of plugin-minted versions through the plugin contract is not needed, because dead keys are purged whole (`delete_key`) and live-key orphans are the documented residual; a delayed outbox message with a transactional cancel instead of the intent table needs changes in the platform outbox (not planned).
- **O3** — Good: a backend with native history needs no gear-side versioning. The earlier critique — that native versioning excludes every backend without it, including the in-memory plugin — was wrong: an in-memory plugin mints versions with a counter, and so can a file or PostgreSQL plugin. What matters is not native versioning but the version returned by `put` (ordered, where `destroy` is supported). O3'+ is O3 stated as a contract on the plugin (versions from `put`) instead of as a property of particular products.
- **O4** — Good: the textbook answer for a write across two stores. Bad: a coordinator or outbox dispatcher on every write is a component and a failure mode of its own, and remote I/O inside a coordinated critical section is what the platform's cluster primitives avoid. O3'+ uses the outbox only for side effects that must follow a commit and are naturally idempotent (the key purge and the destroys) and the intent journal only for the one effect that precedes it (the `put`).

## More Information

Precedents for [the pattern](#the-pattern): [Vault KV v2](https://developer.hashicorp.com/vault/docs/secrets/kv/kv-v2) · [GCP Secret Manager](https://cloud.google.com/secret-manager/docs/overview) · [AWS Secrets Manager](https://docs.aws.amazon.com/secretsmanager/latest/userguide/whats-in-a-secret.html) · [Azure Key Vault](https://learn.microsoft.com/en-us/azure/key-vault/general/about-keys-secrets-certificates) · [Shadow paging](https://en.wikipedia.org/wiki/Shadow_paging) (Lorie, 1977) · [Copy-on-write](https://en.wikipedia.org/wiki/Copy-on-write) · [Transactional outbox](https://microservices.io/patterns/data/transactional-outbox.html) · [Saga](https://microservices.io/patterns/data/saga.html). Protocol walk-through, failure map and a three-tenant scenario: DESIGN §6.1–§6.5.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3, §4.7, §4.10, §6.1–§6.5, §8
- `cpt-cf-credstore-fr-immutable-value-versions` (introduced); `cpt-cf-credstore-fr-write-secret`, `cpt-cf-credstore-fr-write-credential-record`, `cpt-cf-credstore-fr-delete-secret`, `cpt-cf-credstore-fr-optimistic-concurrency` (now run on this protocol); `cpt-cf-credstore-fr-deprovisioning` (superseded).
- Builds on [ADR-0001](0001-cpt-cf-credstore-adr-stateful-gear.md); supersedes [ADR-0002](0002-cpt-cf-credstore-adr-deprovisioning-saga.md) and withdraws the fence of [ADR-0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md); underlies [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md); leaves [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md) untouched.
