---
status: accepted
date: 2026-09-10
---

Created:  2026-09-10 by Constructor Tech
Updated:  2026-09-18 by Constructor Tech

# ADR-0006: Immutable Value Versions with a Pointer in the Row

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The protocol](#the-protocol)
  - [The gc table is the intent log](#the-gc-table-is-the-intent-log)
  - [Statuses, and what the fence still does](#statuses-and-what-the-fence-still-does)
  - [One ordering, no name retention, and the backend key shape](#one-ordering-no-name-retention-and-the-backend-key-shape)
  - [The pattern, and why the reaper goes with the saga](#the-pattern-and-why-the-reaper-goes-with-the-saga)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Revisit Triggers](#revisit-triggers)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-immutable-value-versions`

## Context and Problem Statement

Shipped ([ADR-0001](0001-cpt-cf-credstore-adr-stateful-gear.md)–[0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md)): the backend key `(tenant_id, reference, key-class)` is reused, so rotation overwrites in place; a write is a saga tracked by `status` (`provisioning`, `active`, `deprovisioning`, `declared`); `value_fp = HMAC(fence_key, value)` detects a row/backend mismatch and fails the read closed until a healing `If-Match: *` re-write; guarded and `If-Match: *` writes use opposite orderings; `deprovisioning` holds a name until the backend delete completes. Four hazards — a torn write reading as absence, a stuck `provisioning` row wedging a name, `deprovisioning` holding a name hostage, two write orderings — share one cause: row and backend can be observed mid-write, disagreeing about which bytes belong to a reference. Fence, saga, reaper and name retention manage that disagreement after the fact. Can it be made impossible?

## Decision Drivers

- **D1** — a row never points at bytes other than the ones it describes; unreachable, not detected and healed.
- **D2** — a failure between the stores costs garbage, never a wrong value, a closed read or a wedged name.
- **D3** — one write ordering, whatever the precondition.
- **D4** — no name retention; no statuses beyond "has a value" and "has none".
- **D5** — the plugin stays a dumb kv store: no list, no transactions.
- **D6** — a read stays one SQL query plus one backend read.
- **D7** — no resident background loop; hygiene is a periodic job on an operator-chosen cadence.

## Considered Options

- **O1** — in-place overwrite, saga and fence (shipped).
- **O2** — immutable value versions: every write lands under a fresh key, the row holds a pointer, a gc table tracks unreferenced versions.
- **O3** — backend-native versioning (Vault KV v2, GCP Secret Manager `SecretVersion`).
- **O4** — two-phase commit or an outbox coordinating row and backend as one distributed transaction.

## Decision Outcome

**Chosen: O2.** Every value write mints a fresh `value_id` (UUID v4) and writes the bytes under `(tenant_id, value_id)`; a backend entry is never overwritten, and the row alone knows which value belongs to which reference. `credstore_secrets` gains `value_id UUID NULL` (`NULL` = `declared`) with a unique partial index. `credstore_value_gc(value_id, tenant_id, reason, enqueued_at)` records every version a write creates or retires (`reason`: pending / superseded / removed / aborted). O1 detects instead of preventing and meets none of D1–D7; O3 and O4 fail on grounds unrelated to correctness (Pros and Cons).

### The protocol

**Write** (`PUT` create or replace, `PATCH` carrying `secret`):

1. Validate, authorize, resolve the type, read the row for the precondition.
2. Insert the intent `credstore_value_gc(new_id, pending)` — the durable trace a crash needs, since the plugin cannot list.
3. `plugin.put(tenant_id, new_id, value)`.
4. One transaction: CAS the row to `new_id` (insert on create, update on replace), `DELETE gc(new_id)`, and `INSERT gc(old_id, superseded)` if an old version existed.
5. Best effort after commit: `plugin.delete(old_id)`, then `DELETE gc(old_id)`.

| Failure | State after | Who cleans up |
|---|---|---|
| Steps 1–2 fail | Nothing written or changed | Nothing to clean up |
| Step 3 fails | gc row `pending`; row unchanged | Best-effort `DELETE gc`; caller gets 503 |
| Step 4 fails — DB unavailable | Backend holds orphan bytes; gc row `pending` | Job's pending-reclaim pass once older than `gc.pending_max_age_secs` |
| Step 4 fails — CAS lost | Orphan bytes, unreferenced; winner's row already updated | Best-effort mark `aborted` + delete; job's gc drain as backstop |
| Step 5 fails | Row points at `new_id`; old bytes remain, gc row `superseded` | Job's gc drain, next run |

A crash costs at most one orphaned version and its gc row. Two guarded writers: the loser's CAS matches nothing → 409, its bytes are marked `aborted` and cleaned up. Two `If-Match: *` writers: both complete under distinct ids; the last commit owns the pointer, the other version is enqueued `superseded`.

**Read**: row → `value_id` → `plugin.get` → verify `value_fp` → serve. A `NotFound` from a racing pointer switch is retried once against the row's current `value_id`; a second miss is an error. `value_id IS NULL` (`declared`) never resolves.

**Remove secret** (`PATCH {"secret": null}`, including suppression's `{"fallback": "none", "secret": null}`): one transaction clears `value_id`/`value_fp`, sets `declared` and enqueues the old version `removed`; then step-5 cleanup. **Delete record**: one transaction deletes the row and enqueues its version; the name is free at once, and a successor's fresh `value_id` cannot be touched by a lagging delete. **Expiry** is still checked on every read; removing expired rows is the job's second pass, one transaction per row, batched.

### The gc table is the intent log

`pending` makes a crash between steps 2 and 4 discoverable without scanning the backend (D5). There is no resident watcher (D7): the host calls the SDK trait `CredStoreMaintenanceV1::run_gc` on an operator cadence (daily default). Two passes: **gc drain** — every `reason ≠ pending` row: delete the backend entry, then the row; **pending reclaim** — every `pending` row older than `gc.pending_max_age_secs`, deleting the backend entry only if no row references it (unreachable by protocol, since `DELETE gc(new_id)` commits with the pointer switch; kept as a backstop). A duplicate backend delete is harmless and a duplicate `DELETE gc` is a no-op, so the job never conflicts with step 5.

### Statuses, and what the fence still does

`CHECK (status IN (2, 4))` admits `active` and `declared` only; `provisioning`/`deprovisioning` are retired, their codes reserved. A row is either consistent — it points at existing bytes or at nothing — or not yet visible. `declared` is reached only on purpose: `PATCH {"secret": null}` on an existing record, or a creating `PUT` with an explicit `"secret": null` (no intent, no backend entry). `value_fp` is stamped in step 4 and verified on every read as before (ADR-0003), but a torn write can no longer happen, so a mismatch now means out-of-band tampering; the healing re-write is withdrawn, and recovery is a new write under a fresh `value_id`. `CHECK ((value_id IS NULL) = (value_fp IS NULL) = (fp_key_id IS NULL))` ties the fence to the pointer; out-of-band seeding (a value served on trust without a fingerprint) is withdrawn with it.

### One ordering, no name retention, and the backend key shape

Backend first under a fresh id, then the row transaction, whatever the precondition; `If-Match: *` is ordinary RFC 9110 last-writer-wins. `deprovisioning` (ADR-0002) existed only because the key was deterministic; with `value_id` in the key, `DELETE` releases the row and the name in one transaction. The plugin composes `tenant_id/value_id` — no `reference`, key-class or `owner_id`; the SPI is `get`/`put`/`delete(ctx, tenant_id, value_id)`. The key is opaque: an operator cannot map a raw entry back to a reference without the row. The fence key moves to a fixed, SDK-declared `value_id` (`FENCE_KEY_VALUE_ID`) under the nil tenant, so a random v4 id cannot collide with it.

### The pattern, and why the reaper goes with the saga

Nothing here is novel. Immutable versioned secrets with a pointer to the current one is how Vault KV v2, GCP Secret Manager, AWS Secrets Manager and Azure Key Vault work; credstore keeps the pointer in the metadata row so any dumb kv store qualifies (D5). The atomic pointer switch is copy-on-write / shadow paging (Lorie, 1977). `credstore_value_gc` plus the maintenance job is reachability-based garbage collection, Git's model. The `pending` intent row is a transactional outbox reduced to one row.

The reaper is removed, not reshaped. A saga has intermediate states that are wrong until repaired, and repairing them was the reaper's job. Immutable versions leave nothing to repair — a row points at fully written bytes or at nothing — so what remains is unreachable garbage: the textbook case for a lazy periodic job, not a resident loop.

### Consequences

- D1 is structural; a failure costs at most an orphaned version (D2); one ordering (D3); two statuses, no name retention (D4); three dumb plugin methods (D5); one query plus one backend read, with a rare bounded retry (D6); no resident loop (D7).
- Cost: garbage between a write and its cleanup or the next job run, bounded by the cadence and `gc.pending_max_age_secs`; one extra small DB write per value write; an opaque key; no seeding; an expired credential lingers until the job or the owner removes it.
- Breaking for plugin authors only; no REST or SDK-consumer change — this ADR sits below [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md)'s surface. The reaper, its config and metrics are withdrawn; the job emits `gc_deleted`, `gc_pending_reclaimed`, `expired_deleted` (DESIGN §10).
- **Data migration** is not the gear's work: `m0002` leaves every row `declared`; an external script moves the fence key, carries each fingerprint-checked value to a fresh `value_id`, and marks what it could not carry `fallback: none`. Runbook: [`docs/migration/value-migration.md`](../migration/value-migration.md); DESIGN §8.

### Confirmation

- E2E: a write killed between steps 3–4 leaves the prior value serving and a `pending` gc row the job reclaims after `gc.pending_max_age_secs`; a CAS lost in step 4 leaves the winner serving and the loser `aborted`; two concurrent `If-Match: *` writers both succeed and the row points at the last commit.
- E2E: `DELETE` immediately followed by `PUT` on the same reference succeeds without 409; a read racing a pointer switch retries once and succeeds; an entry corrupted out of band fails closed and is recovered only by a fresh `value_id`.
- Contract: `status` never holds a retired code; `value_fp`/`fp_key_id` are non-null iff `value_id` is; no timer task exists in the gear process.

## Revisit Triggers

- A plugin wants its backend's native versioning directly (O3); today that stays an implementation choice behind the three-method SPI.
- One job period of garbage or expired-row lifetime hurts a high-rotation reference: shorten the cadence or add an eager in-request drain.
- Out-of-band seeding is wanted back for bootstrap; `ck_credstore_fp_with_value` is now load-bearing, so it needs a narrower story than ADR-0003's.

## Pros and Cons of the Options

- **O1 (shipped)** — Good: no migration, no SPI change, no backend garbage. Bad: a torn write serves 404 until a re-put (D1, D2); a stuck `provisioning` row wedges creation (D2, D4); `deprovisioning` holds a name hostage to a race its own key scheme creates (D4); two write orderings (D3).
- **O2 (chosen)** — see Decision Outcome and Consequences.
- **O3** — Good: a backend with native history needs no `value_id`. Bad as the contract: ADR-0001 promises any dumb kv store qualifies, and native versioning excludes every backend without it, including the shipped in-memory plugin. A plugin may still map `value_id` onto its own version number underneath the SPI.
- **O4** — Good: the textbook answer for a write across two stores. Bad: a coordinator or outbox dispatcher is a component and a failure mode of its own, and remote I/O inside a coordinated critical section is what the platform's cluster primitives avoid. The intent row plus gc table is the outbox idea at minimum weight.

## More Information

Precedents for [the pattern](#the-pattern-and-why-the-reaper-goes-with-the-saga): [Vault KV v2](https://developer.hashicorp.com/vault/docs/secrets/kv/kv-v2) · [GCP Secret Manager](https://cloud.google.com/secret-manager/docs/overview) · [AWS Secrets Manager](https://docs.aws.amazon.com/secretsmanager/latest/userguide/whats-in-a-secret.html) · [Azure Key Vault](https://learn.microsoft.com/en-us/azure/key-vault/general/about-keys-secrets-certificates) · [Shadow paging](https://en.wikipedia.org/wiki/Shadow_paging) (Lorie, 1977) · [Copy-on-write](https://en.wikipedia.org/wiki/Copy-on-write) · [Git Internals](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects) · [`git gc`](https://git-scm.com/docs/git-gc) · [Transactional outbox](https://microservices.io/patterns/data/transactional-outbox.html) · [Saga](https://microservices.io/patterns/data/saga.html). Protocol walk-through, failure map and a three-tenant scenario: DESIGN §6.2–§6.4.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.7, §6.1–§6.4, §8
- `cpt-cf-credstore-fr-immutable-value-versions` (introduced); `cpt-cf-credstore-fr-write-secret`, `cpt-cf-credstore-fr-write-credential-record`, `cpt-cf-credstore-fr-delete-secret`, `cpt-cf-credstore-fr-optimistic-concurrency` (now run on this protocol); `cpt-cf-credstore-fr-deprovisioning` (superseded).
- Builds on [ADR-0001](0001-cpt-cf-credstore-adr-stateful-gear.md); supersedes [ADR-0002](0002-cpt-cf-credstore-adr-deprovisioning-saga.md); amends [ADR-0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md); underlies [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md); leaves [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md) untouched.
