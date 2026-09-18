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
  - [O1: in-place overwrite, saga, fence (shipped)](#o1-in-place-overwrite-saga-fence-shipped)
  - [O2: immutable versions, pointer, gc table (CHOSEN)](#o2-immutable-versions-pointer-gc-table-chosen)
  - [O3 and O4: backend-native versioning, or two-phase commit/outbox](#o3-and-o4-backend-native-versioning-or-two-phase-commitoutbox)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-immutable-value-versions`

## Context and Problem Statement

**What ships today** ([ADR-0001](0001-cpt-cf-credstore-adr-stateful-gear.md)–[0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md)). The backend key for a value is `(tenant_id, reference, key-class)`, deterministic and reused: rotation overwrites it in place. A write is a saga with no shared transaction, tracked by a lifecycle `status` — `provisioning`, `active`, `deprovisioning`, `declared`. `value_fp = HMAC(fence_key, value)` detects a row/backend mismatch and fails the read closed until a healing `If-Match: *` re-write. Two overwrite orderings coexist (CAS-then-backend for guarded writes, backend-then-CAS for `If-Match: *`), and `deprovisioning` retains a reference's name until its backend delete completes, to stop a lagging delete from erasing a successor's value under the identical key. All four hazards — a torn write reading as absence, a stuck `provisioning` row wedging a name, `deprovisioning` holding a name hostage to a race the key scheme itself creates, and two mental models for one write — share one root cause: the row and the backend can each be observed independently, mid-write, disagreeing about which bytes belong to a reference. Detection (the fence) and containment (the saga, the reaper, name retention) manage that disagreement after the fact. **Can it be made impossible instead?**

## Decision Drivers

- **D1** — no state in which the row points at bytes that are not the bytes it describes; impossible to reach, not detected and healed.
- **D2** — a failure between the stores costs garbage, never a wrong value, a closed read, or a wedged name.
- **D3** — one write ordering, regardless of precondition.
- **D4** — no name retention, no lifecycle statuses beyond "has a value" and "has none".
- **D5** — the plugin stays a dumb kv store: no list, no transactions.
- **D6** — reads keep one SQL query plus one backend read.
- **D7** — no resident background loop; hygiene is a periodic job on an operator-chosen cadence.

## Considered Options

- **O1** — in-place overwrite, saga, and fence (the shipped design; baseline).
- **O2** — immutable value versions: every write lands under a fresh, backend-unique key; the row holds a pointer; a gc table tracks versions no row references.
- **O3** — backend-native versioning (Vault KV v2, GCP Secret Manager `SecretVersion`) instead of a credstore-built protocol.
- **O4** — two-phase commit or an outbox pattern coordinating the row and the backend as one distributed transaction.

## Decision Outcome

**Chosen: O2.** Every value write mints a fresh `value_id` (UUID v4) and writes the bytes to the backend under `(tenant_id, value_id)` — no `reference`, no key-class; the row alone knows which value belongs to which reference. A backend entry, once written, is never overwritten. `credstore_secrets` gains `value_id UUID NULL` (`NULL` = `declared`) with a unique partial index; a new table `credstore_value_gc(value_id, tenant_id, reason, enqueued_at)` records every version a write creates or retires (`reason`: pending / superseded / removed / aborted). The deciding arguments are D1–D7 together: O1 satisfies none of them by construction (a detector, not a preventer); O3 and O4 are rejected on grounds independent of correctness (Pros and Cons).

### The protocol

**Write** (`PUT` create, `PUT` replace, `PATCH` carrying `secret`): (1) validate, authorize, resolve type, read the row for the precondition; (2) insert the intent — `credstore_value_gc(new_id, pending)`, the durable trace a crash needs since the plugin offers no listing; (3) `plugin.put(tenant_id, new_id, value)`; (4) one transaction — CAS the row to point at `new_id` (insert on create, update on replace) plus `DELETE gc(new_id)` and, if an old version existed, `INSERT gc(old_id, superseded)`; (5) best-effort `plugin.delete(old_id)` then `DELETE gc(old_id)`, immediately after commit — the cleanup path for the common case, so most gc rows never survive to a job run.

| Failure | State after | Who cleans up |
|---|---|---|
| Steps 1–2 fail | Nothing written or changed | Nothing to clean up |
| Step 3 fails | gc row `pending`; row unchanged | Best-effort `DELETE gc`; caller gets 503 |
| Step 4 fails — DB unavailable | Backend holds orphan bytes; gc row `pending` | Job's pending-reclaim pass once older than `gc.pending_max_age_secs` |
| Step 4 fails — CAS lost | Orphan bytes, unreferenced; winner's row already updated | Best-effort mark `aborted` + delete; job's gc drain as backstop |
| Step 5 fails | Row points at `new_id`; old bytes remain, gc row `superseded` | Job's gc drain, next run |

Crash at any point costs at most one orphaned version and its gc row — never a wrong value or a wedged name. Two guarded writers racing: the loser's CAS matches nothing, 409, its bytes marked `aborted` and cleaned up. Two `If-Match: *` writers racing: both complete independently under distinct ids; whichever commits last wins the pointer, the loser's version is enqueued `superseded` — no clobbering, because each version lives under its own key.

**Read**: row → `value_id` → `plugin.get` → verify `value_fp` → serve. A `NotFound` from a pointer switch that raced the read retries once against the row's now-current `value_id`; a second miss is a real error. `value_id IS NULL` (`declared`) is never a resolution candidate.

**Remove secret** (`PATCH {"secret": null}`, including suppression's `{"fallback": "none", "secret": null}`): one transaction — clear `value_id`/`value_fp`, set `declared`, enqueue the old version `removed` — then a best-effort backend delete, same cleanup path as step 5. **Delete record**: one transaction — delete the row, enqueue its version if any — releasing the name at once; a successor mints its own fresh `value_id`, so a lagging delete can never touch it. **Expiry** is checked on every read, unchanged; removing an expired row is the periodic job's other pass, one transaction per row, batched.

### The gc table is the intent log

The `pending` reason exists because a crash between steps 2 and 4 must be discoverable without scanning the backend (D5). There is no resident watcher (D7): the SDK trait `CredStoreMaintenanceV1::run_gc`, invoked by the host on an operator cadence (daily default), runs two passes — **gc drain** (any `reason ≠ pending` row: delete backend + row) and **pending reclaim** (any `pending` row older than `gc.pending_max_age_secs`, backend deleted only if no row still references it — unreachable by protocol, since `DELETE gc(new_id)` sits inside the same transaction as the pointer switch, kept as a defensive backstop). With a daily-or-weekly cadence the job never races step 5's in-line cleanup: a duplicate delete is harmless, a duplicate `DELETE gc` is a no-op.

### Statuses, and what the fence still does

`CHECK (status IN (2, 4))` admits only `active` and `declared`; `provisioning`/`deprovisioning` are retired, their codes reserved, never reassigned — under this protocol a row is either fully consistent (points at bytes that exist, or at nothing) or not yet visible at all, with no third state to name. `declared` is reached exactly two ways, never by omission: `PATCH {"secret": null}` on an existing record, or a creating `PUT` with an explicit `"secret": null` (no intent, no backend entry, since there is no version to supersede). `value_fp` is stamped in step 4 and verified on every read, unchanged in mechanism (ADR-0003), but its role narrows: a torn write can no longer occur, so a mismatch now means only out-of-band tampering. The saga's healing re-write is withdrawn — there is no "same key" to re-inject into; recovery is an ordinary new write under a fresh `value_id`. `CHECK ((value_id IS NULL) = (value_fp IS NULL) = (fp_key_id IS NULL))` ties the fence to the pointer; out-of-band seeding (a value with no fingerprint, served on trust) is withdrawn with it.

### One ordering, no name retention, and the backend key shape

The two shipped overwrite orderings collapse into one: backend first under a fresh id, then the row transaction, regardless of precondition; `If-Match: *` is now ordinary RFC 9110 last-writer-wins, with no healing role. ADR-0002's `deprovisioning` existed only because the backend key was deterministic, so a lagging delete could land after a successor wrote under the identical key — under immutable versions the key includes `value_id`, so an old and a new version never share one, `DELETE` releases the row and the name in the same transaction, and a successor's fresh id is untouched by a predecessor's lagging delete. A plugin composes `tenant_id/value_id` — no `reference`, no key-class, no `owner_id`; the SPI drops to `get`/`put`/`delete(ctx, tenant_id, value_id)`. The key is shorter than today's `tenant/reference/class` but opaque: an operator can no longer map a raw entry back to a reference without the row that currently names it. The fence key's own reserved entry moves to a fixed, SDK-declared `value_id` (`FENCE_KEY_VALUE_ID`) under the nil tenant, so a random v4 id can never collide with it.

### The pattern, and why the reaper goes with the saga

None of this is novel: immutable versioned secrets with a pointer to the current one (Vault KV v2, GCP Secret Manager, AWS Secrets Manager, Azure Key Vault) is the same idea `value_id` implements, with the pointer in the metadata row rather than the backend, so any dumb kv store can participate (D5); the atomic pointer switch is copy-on-write / shadow paging (Lorie, 1977), the mechanism that gives crash consistency without a redo log; `credstore_value_gc` plus the maintenance job is reachability-based garbage collection, Git's model; and the `pending` intent row is a transactional outbox reduced to one row. The reaper is *removed*, not reshaped, because a saga has intermediate states that are wrong until repaired — that was the reaper's job, on a timer. Immutable versions leave nothing to repair: a row either points at fully written bytes or at nothing, so what remains is unreachable garbage, the textbook case for a lazy periodic job, not a resident loop.

### Consequences

- A row can never point at bytes it does not describe — D1 is structural; a failure costs at most an orphaned version, never a torn read, a wedged name, or a wrong value (D2); one write ordering, one code path (D3); two statuses, no name retention (D4).
- The plugin contract stays three dumb methods, no list, no transactions (D5); a read stays one query plus one backend read, with a rare bounded retry (D6); no resident loop (D7).
- Cost: the backend accumulates garbage between a write and its own cleanup or the job's next run, bounded by the job's cadence and `gc.pending_max_age_secs`; one extra small DB write per value write; the key is opaque, not reconstructable from the reference alone; out-of-band seeding is gone; an expired credential lingers until the job clears it or the owner acts directly.
- Breaking for plugin authors only (`get`/`put`/`delete` reshape to `(tenant_id, value_id)`); no REST or SDK-consumer contract change — this ADR sits entirely below [ADR-0004](0004-cpt-cf-credstore-adr-secret-value-exposure.md)'s surface. The in-process reaper, its config, and its metrics are withdrawn outright; the maintenance job emits `gc_deleted`, `gc_pending_reclaimed`, `expired_deleted` instead (DESIGN §10).
- **Data migration** is not the gear's own work: `m0002` is an ordinary, irreversible migration leaving every row `declared`; an external script (outside the gear, which cannot reach the backend from inside a migration) moves the fence key first, carries each fingerprint-checked value to a freshly minted `value_id`, and marks any value that could not be carried `fallback: none`. Runbook: [`docs/migration/value-migration.md`](../migration/value-migration.md); see DESIGN §8.

### Confirmation

- E2E: a write killed between steps 3–4 leaves the prior value serving and a `pending` gc row the job reclaims after `gc.pending_max_age_secs`; a CAS lost in step 4 leaves the winner serving and the loser `aborted`, collected by cleanup or the next job run; two concurrent `If-Match: *` writers both succeed, the row pointing at whichever committed last.
- E2E: `DELETE` immediately followed by `PUT` on the same reference succeeds with no wait, no 409; a read racing a pointer switch retries once and succeeds; a backend entry corrupted out of band fails closed on its next read and is recovered only by a fresh `value_id`.
- Contract: `status` never holds a retired code; `value_fp`/`fp_key_id` are non-null iff `value_id` is; no timer task exists in the gear process.

## Revisit Triggers

- A plugin wants to use its backend's own native versioning directly instead of through the `value_id`-keyed contract (O3) — today that stays an implementation choice behind the three-method SPI; garbage or expired-row lifetime of one job period becomes a problem for a high-rotation reference, arguing for a shorter cadence or an eager in-request drain.
- Out-of-band seeding is wanted back for bootstrap; it needs a narrower story than ADR-0003's, since `ck_credstore_fp_with_value` is now load-bearing elsewhere.

## Pros and Cons of the Options

### O1: in-place overwrite, saga, fence (shipped)

- Good: ships today; no migration, no plugin SPI change, no new table; no backend garbage.
- Bad: a torn write serves 404 until a caller happens to re-put (D1, D2); a stuck `provisioning` row wedges creation (D2, D4); `deprovisioning` holds a name hostage to a race its own key scheme creates (D4); two overwrite orderings for one operation (D3).
- Rejected as the ongoing model, superseded by O2.

### O2: immutable versions, pointer, gc table (CHOSEN)

- Good: satisfies D1–D7 by construction, per Decision Outcome.
- Bad: backend garbage between a write and the job's next run, an extra small DB write per write, an opaque key, the loss of out-of-band seeding, an expired credential that lingers until the job clears it — named plainly in Consequences.

### O3 and O4: backend-native versioning, or two-phase commit/outbox

- Good (O3): a backend with its own version history (Vault KV v2, GCP Secret Manager) needs no `value_id` at all. Bad as the *contract*: ADR-0001 promises any dumb kv store qualifies as a backend, and native versioning excludes every backend without it, including the shipped in-memory plugin — rejected as the contract, open to a plugin mapping `value_id` onto its own version number underneath the SPI.
- Good (O4): a textbook answer to coordinating a write across two stores without a shared transaction. Bad: heavier than the guarantee needs — a coordinator or outbox-with-dispatcher adds a component and a failure mode of its own, and remote I/O inside a coordinated critical section is what this platform's cluster primitives steer away from; the intent-row/gc-table pair already is the outbox idea, at the minimum weight this problem needs.

## More Information

Precedents for [the pattern](#the-pattern-and-why-the-reaper-goes-with-the-saga): [Vault KV v2](https://developer.hashicorp.com/vault/docs/secrets/kv/kv-v2) · [GCP Secret Manager](https://cloud.google.com/secret-manager/docs/overview) · [AWS Secrets Manager](https://docs.aws.amazon.com/secretsmanager/latest/userguide/whats-in-a-secret.html) · [Azure Key Vault](https://learn.microsoft.com/en-us/azure/key-vault/general/about-keys-secrets-certificates) · [Shadow paging](https://en.wikipedia.org/wiki/Shadow_paging) (Lorie, 1977) · [Copy-on-write](https://en.wikipedia.org/wiki/Copy-on-write) · [Git Internals](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects) · [`git gc`](https://git-scm.com/docs/git-gc) · [Transactional outbox](https://microservices.io/patterns/data/transactional-outbox.html) · [Saga](https://microservices.io/patterns/data/saga.html). Write protocol walked step by step, with its failure map and a three-tenant scenario: DESIGN §6.2–§6.4.

## Traceability

- **PRD**: [PRD.md](../PRD.md) · **DESIGN**: [DESIGN.md](../DESIGN.md) §4.7 (schema), §6.1–§6.4 (lifecycle), §8 (migration)
- `cpt-cf-credstore-fr-immutable-value-versions` — the requirement this ADR introduces.
- `cpt-cf-credstore-fr-write-secret`, `-fr-write-credential-record`, `-fr-delete-secret`, `-fr-optimistic-concurrency` — the protocol these requirements now run on.
- `cpt-cf-credstore-fr-deprovisioning` — superseded: no status, no name retention remain.
- Builds on [ADR-0001](0001-cpt-cf-credstore-adr-stateful-gear.md); supersedes [ADR-0002](0002-cpt-cf-credstore-adr-deprovisioning-saga.md); amends [ADR-0003](0003-cpt-cf-credstore-adr-value-fingerprint-fence.md); underlies the write verbs of [ADR-0007](0007-cpt-cf-credstore-adr-record-write-verbs.md); leaves [ADR-0005](0005-cpt-cf-credstore-adr-upward-collection-read.md) untouched.
