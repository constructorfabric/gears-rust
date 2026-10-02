---
status: accepted
date: 2026-09-10
---

Created:  2026-09-10 by Constructor Tech
Updated:  2026-10-02 by Constructor Tech

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

Shipped ([ADR-0001](0001-cpt-cf-credstore-adr-stateful-gear.md)–[0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md)): the backend key `(tenant_id, reference, key-class)` is reused, so rotation overwrites in place; a write is a saga tracked by `status` (`provisioning`, `active`, `deprovisioning`, `declared`); `value_fp = HMAC(fence_key, value)` detects a row/backend mismatch and fails the read closed until a healing `If-Match: *` re-write; guarded and `If-Match: *` writes use opposite orderings; `deprovisioning` holds a name until the backend delete completes. Four hazards — a torn write reading as absence, a stuck `provisioning` row wedging a name, `deprovisioning` holding a name hostage, two write orderings — share one cause: row and backend can be observed mid-write, disagreeing about which bytes belong to a reference. Fence, saga, reaper and name retention manage that disagreement after the fact. Can it be made impossible, without a gc table, a maintenance job or any cleanup that correctness depends on?

## Decision Drivers

- **D1** — a row never points at bytes other than the ones it describes; unreachable, not detected and healed.
- **D2** — a failure between the stores costs garbage, never a wrong value, a closed read or a wedged name.
- **D3** — one write ordering, whatever the precondition.
- **D4** — no name retention; no statuses beyond "has a value" and "has none".
- **D5** — the plugin is a versioned kv store: `put` returns the provider's version of the stored value; no list, no CAS, no transactions.
- **D6** — a read stays one SQL query plus one backend read.
- **D7** — no resident loop and no maintenance job; the only asynchronous work is the platform outbox's purge on delete.

## Considered Options

- **O1** — in-place overwrite, saga and fence (shipped).
- **O2** — gear-minted `value_id` per write: every write lands under a fresh random key `(tenant_id, value_id)`, the row holds a pointer, a `credstore_value_gc` table with a `pending` intent tracks unreferenced versions, and a periodic maintenance job (`run_gc`) drains it (the previous version of this ADR).
- **O3** — backend-native versioning (Vault KV v2, GCP Secret Manager `SecretVersion`) used directly as the plugin contract.
- **O3'** — plugin-minted ordered versions under one key per record: the provider assigns a value version on `put` (ordered, where `destroy` is supported), the row holds it as a pointer, on a backend that supports `destroy` a successful write destroys older versions inline, and record deletion purges the key through the platform transactional outbox.
- **O4** — two-phase commit or an outbox coordinating row and backend as one distributed transaction for every write.

## Decision Outcome

**Chosen: O3'.** The row stays authoritative for metadata and holds `value_version`, the value version the provider returned from `put` (`NULL` = `declared`; not the row's own `version` counter). All versions of a record live under one store key `(tenant_id, record_id)`; a version is never overwritten. A write puts the bytes first, then switches the pointer in one PG transaction, then, where the backend supports `destroy`, destroys older versions best effort. Deleting a record deletes the row and enqueues a key purge in the same transaction. O1 detects instead of preventing and meets none of D1–D7. O2 meets D1–D4 but needs a gc table, an intent row, a maintenance job and a fence key address just to find its own garbage; O3' finds its garbage by position (below the pointer) instead of by bookkeeping. O4 fails on grounds unrelated to correctness (Pros and Cons).

### Data

`credstore_secrets` (authoritative for metadata) holds:

- `id` — the record identity, minted at create and never reused: a re-create mints a new `id`. It is also the record part of the store key.
- `version` — the row version for optimistic concurrency, +1 on every change, metadata or secret.
- `value_version` — the value version of the current secret in the store, as returned by `put` (TEXT, `NULL` means `declared`, no secret). It is distinct from `version`, the row's optimistic-concurrency counter (the **row version**, bumped on every change of the row and used in the `ETag` `(id, version)` and the CAS); the **value version** identifies the stored secret bytes.

The client validator is `(id, version)`. A new `id` on re-create means a validator from before a delete never matches: no ABA, no tombstones. The store holds, under `(tenant_id, record_id)`, immutable versions identified by the value versions the provider assigned.

### The plugin contract

Three required operations and one optional. Every operation takes the record key explicitly, `key = (tenant_id, record_id)`, where `record_id` is the row's `id` (minted at create, never reused); the gear chooses the key and the plugin only maps it to a physical location under its installation prefix, so the reference, type and sharing are not part of the key. Every call also carries the request context, used only for correlation, never for authorization. Required: `put(key, value) -> version` (durable; the provider chooses the value version and `put` returns it, and the gear stores it in `value_version`), `get(key, version) -> Option<bytes>` (exactly the bytes written by the `put` that returned `version`, not found, or a permanent "unreadable" outcome meaning the version exists but can never be read, e.g. a lost decryption key — never different bytes) and `delete_key(key)` (delete the key with all its versions; idempotent). Optional: `destroy(key, selector)`, idempotent, where the selector is `Below(version)` (every version older than `version`) or `Exactly(version)` (that one version). The plugin declares whether it supports `destroy`. A backend that supports `destroy` **MUST** return **ordered versions per key** (a `put` that starts after another `put` on the same key has returned gets a greater version); a backend without `destroy` does not need ordered versions, because `Below(vv)` is safe only when every version older than a committed version belongs to a writer with a stale base. Every backend must also provide **durability** (`put` returns only after the bytes are durable) and **exact bytes**. Not required: CAS, listing, cross-key transactions, linearizability beyond the version ordering, server-side logic.

Backends with `destroy` and ordered versions: Vault and OpenBao KV v2 (the KV version number; the mount must never evict a version on its own, so unlimited retention is required), Google Cloud Secret Manager (version numbers), a PostgreSQL plugin (a sequence-backed row id in an insert-only table), the in-memory plugin (a per-key counter) and a file plugin (a per-key monotonic counter, not a random id). Backends without `destroy`: AWS Secrets Manager (a version cannot be deleted; version ids are UUIDs, so unordered) and Azure Key Vault (a version can only be disabled; ids are opaque). They provide the three required operations only and accept the residual below: a rotated or removed secret stays in the backend until the record is deleted. The per-provider mapping is in DESIGN §4.3 and the requirement is `cpt-cf-credstore-fr-backend-compatibility`.

### The protocol

**Write** (`PUT` create or replace, `PATCH` carrying `secret`):

1. **Read and check.** Read the row (none for create). Check the client precondition (`Matches` = `(id, version)`, or `Exists`), the PDP on the row's type (create: the request's type) before any store call, and the type traits. Create over an own row is 409, an expired one included (the expired record is visible: renew or delete it). A replace or `PATCH` finds an expired own row like a live one and updates it in place (renewal, same id).
2. **Put the value.** `vv = plugin.put(key, value)` under the record's key (create: a freshly minted record id); `vv` is the value version the provider assigned. A store error is 503; nothing changed in PG; an orphan version may exist.
3. **Switch the pointer in one PG transaction.** Replace/patch: `UPDATE … SET value_version = :vv, <fields>, version = version + 1 WHERE id = :id AND version = :v1`. Create: `INSERT` the row with the new id, `version = 1`, `value_version = :vv`; a unique violation on `(tenant, reference, class)` is a concurrent create. Outcomes: **committed** → step 4; **definite loss** (0 rows, or a unique violation) → when `destroy` is supported, best-effort `destroy(Exactly(vv))`, then 409 (`OPTIMISTIC_LOCK_FAILURE` for an update, `ALREADY_EXISTS` for a create) — for an `Exists` (last-writer-wins) update: re-read the row and retry once from step 1 before returning 409; **ambiguous** (PG unavailable or timed out: the commit may or may not have happened) → do not delete `vv`, return 503.
4. **Destroy older versions.** Only when the backend supports `destroy`; best effort after commit: `plugin.destroy(Below(vv))`. A failure is ignored; older versions stay until the next successful write destroys them. Without `destroy` this step does nothing.
5. **Reply** with the new validator: create 201 + `Location` + `ETag`; replace/patch 204 + `ETag`.
6. **No create over an expired own row**: an expired record still holds the reference, so a create-only write over it ends at step 1 with 409. Only a delete enqueues a purge.

**Metadata-only patch** (no `secret` member): one PG CAS (`version + 1`); no store call at all.

**Remove the secret** (`PATCH {"secret": null}`, including suppression `{"fallback": "none", "secret": null}`): one PG CAS sets `value_version = NULL` (`declared`); then, where `destroy` is supported, best effort `destroy(Below(old_vv))` and `destroy(Exactly(old_vv))`.

**Read**: one PG query resolves the hierarchy and picks the decisive row; the PDP is evaluated on its type before any store call. A metadata-only read or a listing replies from PG and never touches the store. When the secret is selected: `get(key, value_version)`; found → reply; not found (a concurrent write switched the pointer and destroyed the old version) → re-read the row once and, if `value_version` changed, `get` again; a second miss after the pointer moved is 503. If the pointer did not move and the version is gone, or the plugin reports the version permanently unreadable, the read answers `SECRET_UNREADABLE` (409, canonical `FailedPrecondition` with the HTTP status overridden, like `SECRET_EXPIRED`) at once, with no retry: retrying cannot help, and the record must be rewritten (`PUT`/`PATCH` with a secret) or deleted. In secret mode (bulk collection read) such an item is returned with its metadata and without the secret, like an expired one. `value_version IS NULL` (`declared`) is never a store call. Expiry applies to the secret, not to the record: it is evaluated from the row at read time, and when the decisive row is `active` with `expires_at` in the past the read answers `SECRET_EXPIRED` (409) before any store call, never continuing to an ancestor's value; the record's metadata stays readable with status `expired`.

**Delete record**: read the row, check the precondition and the PDP (no row: 404). One PG transaction deletes the row (CAS on `version` for `Matches`) and enqueues `purge(tenant_id, record_id)` in the platform transactional outbox. The reference is free at once; a re-create mints a new id and a new key, which a lagging purge cannot touch. The outbox handler calls `delete_key` and retries on failure — at least once, and `delete_key` is idempotent. Reply 204.

| Failure | State after | Who cleans up |
|---|---|---|
| Step 1 fails or the precondition is refused | Nothing written or changed | Nothing to clean up |
| Step 2 fails | PG unchanged; an orphan version may exist (the failure can be ambiguous: the bytes can have persisted despite it); 503 | The next successful write's `destroy(Below)` (backend with `destroy`) |
| Step 3 loses (0 rows, unique violation) | Winner's row stands; this writer's version is unreferenced; 409 | Best-effort `destroy(Exactly(vv))`; if that fails or is unsupported, the next successful write's `destroy(Below)` (backend with `destroy`) |
| Step 3 ambiguous (PG unavailable or timeout) | Commit may or may not have happened; `vv` is kept because the row may point at it; 503 | Nothing deletes `vv`; if the commit did not happen, the next successful write's `destroy(Below)` (backend with `destroy`) |
| Step 4 fails, or no `destroy` | Row points at `vv`; older versions remain below the pointer | The next successful write's `destroy(Below)`; without `destroy`, record delete or tenant offboarding |
| Writer crashes after `put`, before commit | Orphan version above the pointer; unreachable | The next successful write's `destroy(Below)` (backend with `destroy`), else record delete or tenant offboarding |
| Writer crashes after commit, before destroy | Old versions below the pointer; unreachable | The next successful write's `destroy(Below)` (backend with `destroy`) |
| Delete: PG transaction fails | Row and reference exactly as they were | Nothing to clean up |
| Delete: outbox `delete_key` fails | Row already gone, reference already free; key still holds versions | The outbox retries until it succeeds |

A crash costs at most orphaned versions under one record key. Two guarded writers: the loser's CAS matches nothing → 409, and it removes its own version. Two `If-Match: *` writers: both put; the first to commit owns the pointer, the other re-reads, retries once from step 1 and commits a later version, or returns 409.

### Why destroy is safe, and why `delete_key` is not used on the remove-secret path

`destroy(Below(vv))` deletes versions by position, so it is safe only because versions are ordered; this argument applies to `Below` only, and a backend without `destroy` needs neither it nor ordered versions. Let writer W commit `vv = r` and destroy below `r`. Another writer X can be in one of two situations. If X's `put` is **older** than `r` (X put, or read its base row, before W's commit), X holds a stale base: W's commit bumped `version`, so X's CAS matches nothing and X destroys its own version — the version W destroys was never going to be a pointer. If X's `put` is **newer** than `r`, X read the row after W's commit, which was after W's `put` returned, so by the ordering guarantee X's version is greater than `r` and `destroy(Below(r))` never touches it. Hence committed versions strictly increase in commit order, the row's current `value_version` is never destroyed, and a pointer never dangles. Without ordered versions neither half of the argument holds, which is why a backend without them cannot support `destroy`. `destroy(Exactly(vv))` needs no ordering: it names a version no row references.

`delete_key` is never used on the remove-secret path. Removing the secret leaves the record alive as `declared`, and its key will be reused by the next write. A concurrent writer may already have put a newer version under that key and be about to commit, or may do so right after the removal commit; `delete_key` would delete that version before its pointer switch commits and leave a dangling pointer. The remove-secret path therefore deletes by position and by exact version (`destroy(Below(old_vv))`, `destroy(Exactly(old_vv))`), which can only touch versions the removal itself made unreachable. `delete_key` runs only after the row is gone, from the outbox, against a key no future record will use (record ids are never reused).

### What remains

There is no gc table, no `pending` intent, no maintenance job and no fence. What is left is bounded garbage:

- **Orphan versions above the pointer** (a `put` whose commit failed ambiguously, or a writer that crashed after `put`) are unreachable and destroyed by the next successful write's `destroy(Below)` on a backend with `destroy`. A record that is never written again, or any record on a backend without `destroy`, keeps them until the record is deleted (outbox `delete_key`) or the tenant is offboarded.
- **Old versions below the pointer** after a failed destroy are unreachable and removed by the next successful write.
- **Without `destroy`** (AWS Secrets Manager, Azure Key Vault), rotated, removed and orphaned versions stay in the backend, unreachable through the gear, until the record is deleted (`delete_key`) or the tenant is offboarded. A rotated or removed secret is retained by such a backend until record deletion; this is the accepted behaviour.
- **A writer that read the row before a delete and put after it** lands its version under the deleted key. Its CAS fails (the row is gone), so it deletes its version. If it crashed in between, that version stays unreachable under a key nobody will write again — a documented residual, bounded at one version per such crash.
- **Integrity** comes from immutable versions and the exact-bytes `get`; a backend that returns different bytes violates its contract. Out-of-band tampering detection is not provided by the gear.
- **The PG database is the system of record for metadata** and must be backed up together with the store; the index is not rebuildable from the store (no listing). Index rebuild is out of scope.
- **Audit** events for secret reads and writes go through the platform event-broker gear and are best-effort: if publishing fails, the operation continues, an error is logged without the secret and the failure is counted in a metric.

### Statuses, and what is withdrawn

`CHECK (status IN (2, 4))` admits `active` and `declared` only; `provisioning`/`deprovisioning` are retired, their codes reserved. A row is either consistent — `value_version` names an existing version — or `declared` with no value version. `declared` is reached only on purpose: `PATCH {"secret": null}` on an existing record, or a creating `PUT` with an explicit `"secret": null` (no store call at all). `value_version IS NULL` holds exactly for `declared`.

Withdrawn by this ADR, relative to the shipped design and to O2: the `credstore_value_gc` table and its `pending` / `superseded` / `removed` / `aborted` reasons; `CredStoreMaintenanceV1::run_gc` and the maintenance job with its `gc` config and counters; the fingerprint fence — `value_fp`, `fp_key_id`, the stored fence key and its reserved address ([ADR-0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md)): a row/backend mismatch is impossible by construction, so there is nothing for it to detect; the random per-write `value_id`. Kept: no name retention, two statuses, the generation-bound validator.

### The pattern

Nothing here is novel. Immutable versioned secrets with a pointer to the current one is how Vault KV v2, GCP Secret Manager, AWS Secrets Manager and Azure Key Vault work; credstore keeps the pointer in the metadata row so a versioned kv store qualifies (D5). The atomic pointer switch is copy-on-write / shadow paging (Lorie, 1977). Destroying below the committed pointer is garbage collection by position, which ordered versions make safe without a reachability scan. Deleting a record through the transactional outbox is the textbook pattern for a side effect that must follow a commit.

The reaper is removed, not reshaped. A saga has intermediate states that are wrong until repaired, and repairing them was the reaper's job. Immutable versions leave nothing to repair, and the leftover garbage is reclaimed by the next write or by the delete, not by a timer.

### Consequences

- D1 is structural; a failure costs at most an orphaned version (D2); one ordering (D3); two statuses, no name retention (D4); a versioned kv plugin (three required operations plus an optional `destroy`) with no list and no CAS (D5); one query plus one backend read, with a rare bounded retry (D6); no resident loop and no job, only the outbox purge (D7).
- Cost: garbage between a failed cleanup and the next write to that record; unreferenced versions of a never-rewritten record survive until its delete; one `destroy` call per successful secret write on a backend with `destroy`; a backend without `destroy` retains rotated and removed versions until record deletion; a backend with `destroy` must provide ordered versions; the PG database is a system of record that needs a backup consistent with the store; an expired credential lingers (its metadata visible, its secret withheld) until it is renewed in place or deleted by its owner.
- Breaking for plugin authors only (`CredStorePluginClientV2`); no REST or SDK-consumer change — this ADR sits below [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md)'s surface. The reaper, its config and metrics are withdrawn; counters for destroy failures, outbox purge failures, read retries and unreadable versions (`secret_unreadable`) replace them (DESIGN §10). The one consumer-visible addition is the `SECRET_UNREADABLE` answer above.
- **Data migration** is not the gear's work: the schema migration leaves every row `declared`; a one-off operator tool shipped with the gear's sources but not part of the gear — linked by the operator with the old and the new store implementations, so values an encrypting plugin keeps behind its own code are read in-process — carries each value to the record's key through `put`, sets `value_version`, and marks what it could not carry `fallback: none`; the copy runs first, on the shipped schema and verified against the fingerprints, and the schema migration (which drops the fingerprint columns) is applied only after the copy has succeeded; the old fence key entry is removed afterwards (DESIGN §8).

### Confirmation

- E2E: a write killed after `put` and before the commit leaves the prior value serving and an unreachable version that the next successful write destroys (backend with `destroy`); a CAS lost in step 3 leaves the winner serving and, on a backend with `destroy`, the loser's version destroyed; two concurrent `If-Match: *` writers both complete or one returns 409, and the row points at a committed version (J1, J3).
- E2E: a read racing a pointer switch plus destroy retries once and succeeds; a second miss after the pointer moved is 503, never a stale or empty value (J6). A row whose pointer did not move but whose version the store lacks, or which the plugin reports unreadable, answers 409 `SECRET_UNREADABLE` without a retry and is counted by `secret_unreadable`; in secret mode the item is returned without its secret. `DELETE` immediately followed by `PUT` on the same reference succeeds without 409, the new record has a new id and key, and the lagging outbox purge does not remove the new value (J7).
- E2E: a failed `destroy` is ignored and the next write cleans up (backend with `destroy`); on a backend without `destroy`, writes, reads and record deletion behave identically and no cleanup is attempted; a failed outbox `delete_key` is retried until the key is gone; a PG timeout at the pointer switch returns 503 without deleting the version.
- Contract: versions under one key strictly increase in real-time `put` order (J4); the bytes at the row's `value_version` are the bytes of the write that committed it (J2); metadata-only reads, listings and metadata-only patches make no store call (J5); `status` never holds a retired code and `value_version IS NULL` exactly for `declared`; no timer task and no maintenance entry point exist in the gear process.

## Revisit Triggers

- A target backend that must reclaim superseded versions cannot provide ordered versions (`destroy(Below)` requires them; such a backend needs its own ordered counter or must run without `destroy` and accept retention until record deletion).
- Orphan retention for never-rewritten records hurts: orphan versions of a record that is never written again survive until its delete. A bounded sweep could be added then; it is deliberately not part of this decision.
- The PG database must become rebuildable from the store: that needs a listing capability on the plugin or a metadata copy in the store, neither of which is provided.
- Out-of-band tampering detection becomes a requirement: that needs a mechanism the fence's removal gave up (ADR-0003), and a story for key custody.

## Pros and Cons of the Options

- **O1 (shipped)** — Good: no migration, no SPI change, no backend garbage. Bad: a torn write serves 404 until a re-put (D1, D2); a stuck `provisioning` row wedges creation (D2, D4); `deprovisioning` holds a name hostage to a race its own key scheme creates (D4); two write orderings (D3).
- **O2 (previous version of this ADR)** — Good: D1–D4 are met; plugin needs only `get`/`put`/`delete`. Bad: the gear mints ids the store cannot order, so it must record every intent in a `credstore_value_gc` table, run a maintenance job to drain it and reclaim stale `pending` rows, keep a fixed fence-key id, and still carries a narrow race between the job's backend delete and its gc delete; more moving parts (a table, a job, a config block, a trait, counters) than the problem needs.
- **O3' (chosen)** — Good: no extra table, no intent row, no job; garbage is found by position; the backend is dumb (D5); one protocol for every precondition (D3). Bad: a backend with `destroy` must provide ordered versions, and one without it retains superseded versions until record deletion; orphan versions of a never-rewritten record linger until its delete; the PG database becomes a system of record that must be backed up with the store.
- **O3** — Good: a backend with native history needs no gear-side versioning. The earlier critique — that native versioning excludes every backend without it, including the in-memory plugin — was wrong: an in-memory plugin mints versions with a counter, and so can a file or PostgreSQL plugin. What matters is not native versioning but the version returned by `put` (ordered, where `destroy` is supported). O3' is O3 stated as a contract on the plugin (versions from `put`) instead of as a property of particular products.
- **O4** — Good: the textbook answer for a write across two stores. Bad: a coordinator or outbox dispatcher on every write is a component and a failure mode of its own, and remote I/O inside a coordinated critical section is what the platform's cluster primitives avoid. O3' uses the outbox only for the one side effect that must follow a commit and is naturally idempotent (the key purge).

## More Information

Precedents for [the pattern](#the-pattern): [Vault KV v2](https://developer.hashicorp.com/vault/docs/secrets/kv/kv-v2) · [GCP Secret Manager](https://cloud.google.com/secret-manager/docs/overview) · [AWS Secrets Manager](https://docs.aws.amazon.com/secretsmanager/latest/userguide/whats-in-a-secret.html) · [Azure Key Vault](https://learn.microsoft.com/en-us/azure/key-vault/general/about-keys-secrets-certificates) · [Shadow paging](https://en.wikipedia.org/wiki/Shadow_paging) (Lorie, 1977) · [Copy-on-write](https://en.wikipedia.org/wiki/Copy-on-write) · [Transactional outbox](https://microservices.io/patterns/data/transactional-outbox.html) · [Saga](https://microservices.io/patterns/data/saga.html). Protocol walk-through, failure map and a three-tenant scenario: DESIGN §6.1–§6.5.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.3, §4.7, §4.10, §6.1–§6.5, §8
- `cpt-cf-credstore-fr-immutable-value-versions` (introduced); `cpt-cf-credstore-fr-write-secret`, `cpt-cf-credstore-fr-write-credential-record`, `cpt-cf-credstore-fr-delete-secret`, `cpt-cf-credstore-fr-optimistic-concurrency` (now run on this protocol); `cpt-cf-credstore-fr-deprovisioning` (superseded).
- Builds on [ADR-0001](0001-cpt-cf-credstore-adr-stateful-gear.md); supersedes [ADR-0002](0002-cpt-cf-credstore-adr-deprovisioning-saga.md) and withdraws the fence of [ADR-0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md); underlies [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md); leaves [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md) untouched.
