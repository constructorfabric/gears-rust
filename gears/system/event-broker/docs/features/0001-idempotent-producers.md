<!-- Created: 2026-05-11 by Constructor Tech -->
<!-- Revised: 2026-05-12 — aligned to ADR-0003 Event Schema, ADR-0004 Idempotent Producer Protocol, and revised ADR-0002 Partition Selection -->

# Feature: Idempotent Producers

- [ ] `p1` - **ID**: `cpt-cf-evbk-featstatus-idempotent-producers-implemented`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Choosing a Producer Mode](#2-choosing-a-producer-mode)
  - [2.1 Stateless](#21-stateless)
  - [2.2 Monotonic](#22-monotonic)
  - [2.3 Chained](#23-chained)
  - [2.4 Rule of Thumb](#24-rule-of-thumb)
- [3. Wire Shapes (`meta` block)](#3-wire-shapes-meta-block)
- [4. Actor Flows (CDSL)](#4-actor-flows-cdsl)
  - [4.1 Producer Registration](#41-producer-registration)
  - [4.2 Chained Mode Publish](#42-chained-mode-publish)
  - [4.3 Monotonic Mode Publish](#43-monotonic-mode-publish)
  - [4.4 Stateless Mode Publish](#44-stateless-mode-publish)
  - [4.5 Desync Recovery (Restart / DB Restore)](#45-desync-recovery-restart--db-restore)
  - [4.6 Chain Reset (Operator-Driven)](#46-chain-reset-operator-driven)
  - [4.7 Producer Registration TTL (Natural Reset)](#47-producer-registration-ttl-natural-reset)
- [5. Processes / Business Logic (CDSL)](#5-processes--business-logic-cdsl)
  - [5.1 Producer / Outbox Contract](#51-producer--outbox-contract)
- [6. States](#6-states)
- [7. Hard-Error Catalog](#7-hard-error-catalog)
- [8. Definitions of Done](#8-definitions-of-done)
  - [Idempotent Producer Contract](#idempotent-producer-contract)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Unit Test Plan](#10-unit-test-plan)
- [11. E2E Test Plan](#11-e2e-test-plan)
- [12. Traceability](#12-traceability)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

The event broker offers three producer modes — **chained**, **monotonic**, **stateless** — that trade off strictness of idempotent publishing against per-publish overhead. **Mode is declared once at producer registration** (`POST /v1/producers`) and enforced per request by the broker. Producer-protocol fields (`producer_id`, `previous`, `sequence`) live inside the publish-time `meta` block on the event (marked `writeOnly`, stripped on read); stateless publishes omit `meta` entirely. Chained and monotonic producers maintain ingest-side dedup state in `evbk_producer_state`, keyed by `(producer_id, topic, partition)`.

### 1.2 Purpose

Idempotent-producer support lets producers retry safely after transport failures, partial commits, or network timeouts without admitting duplicates into the log. Combined with the producer-side transactional outbox (`toolkit-db`), this gives effective exactly-once semantics for in-platform event flows.

### 1.3 Actors

- **Producer SDK** (`cf-gears-event-broker-sdk`): on first publish, calls `POST /v1/producers` to obtain a `producer_id` bound to its principal; populates `meta.producer_id` / `meta.previous` / `meta.sequence` per the registered mode; computes a broker-partition hint locally from `partition_key` or `tenant_id` for SDK/outbox routing while the broker remains authoritative for final topic partition assignment; retries on transient failures.
- **IngestService** (broker): validates `meta`-shape against the registered mode of `meta.producer_id`; performs chain check; updates `evbk_producer_state` and `evbk_producer.last_seen_at` atomically with the outbox enqueue.
- **Reaper worker** (broker): purges stale `evbk_producer_state` rows (per topic-level `retention`, capped at `P14D`) and stale `evbk_producer` rows (per producer-registration TTL, default `P30D`).
- **Operator**: invokes `POST /v1/producers/{id}:reset` for chain reset on a live `producer_id`; reads `GET /v1/producers/{id}/cursors` for diagnostics.

### 1.4 References

- [ADR-0002 Partition Selection (revised)](../ADR/0002-partition-selection.md)
- [ADR-0003 Event Schema](../ADR/0003-event-schema.md)
- [ADR-0004 Idempotent Producer Protocol](../ADR/0004-idempotent-producer-protocol.md)
- DESIGN.md §3.1 Domain Model, §3.2 Producer Modes, §3.6 Two Sequences
- `schemas/event.v1.schema.json` — single canonical event schema (publish + read; per-direction semantics via `readOnly` / `writeOnly` markers)
- `migration.sql` — `evbk_producer`, `evbk_producer_state` DDL
- PRD.md FR `cpt-cf-evbk-fr-producer-modes`

## 2. Choosing a Producer Mode

**Gaps in the producer's sequence are normal.** A producer may legitimately publish `1, 2, 3, 5` — sequence 4 was rolled back in a business transaction, deleted by an operator, or simply skipped for a domain reason; re-sequencing the producer's counter is often not an option. All three modes accept gaps. The modes differ in **whether the broker can detect *unintended* gaps** (events lost in transit, out-of-order arrival, producer's view diverging from the broker's) — and, if so, how the producer recovers.

### 2.1 Stateless

**Use when** one of:

- The consumer's processing is naturally idempotent (UPSERT, set-state-to-X, idempotent task triggers — re-processing the same event is safe).
- The producer is ephemeral and cannot persist local state (lambdas, short-lived workers).
- Events are non-causal between publishes (independent emissions, no order required).

**Properties:**

- No registration required. `POST /v1/producers` is not called. No `meta.producer_id`. No `meta` block at all on the wire.
- Broker performs no dedup. A publish-retry under stateless results in two persisted events; the consumer absorbs duplicates.
- Cheapest publish path; no broker-side state.

### 2.2 Monotonic

**Use when** the publish path is **reliable / synchronous** and the producer trusts its own counter. The producer can persist `last_sequence` reliably across restarts (e.g., in a local DB, transactional with the business state).

**Properties:**

- One-time `POST /v1/producers { "mode": "monotonic" }` → broker mints `producer_id`.
- Per publish: `meta = { version, producer_id, sequence }`. The producer assigns `sequence` monotonically increasing per `(topic, partition)`.
- Broker check: `meta.sequence > evbk_producer_state.last_sequence` → accept and advance; duplicates (`meta.sequence <= last_sequence`) → `200 OK` with the original event_id.
- **Recovery on error**: `GET /v1/producers/{producer_id}/cursors` → reconcile local view → resume.
- Cheaper than chained: no `meta.previous` field; broker check is one comparison.

### 2.3 Chained

**Use when** the publish path is **async / unreliable / windowed** AND the producer needs the broker to detect unintended gaps.

**Properties:**

- One-time `POST /v1/producers { "mode": "chained" }` → broker mints `producer_id`.
- Per publish: `meta = { version, producer_id, previous, sequence }`. `meta.previous` is the broker's expected `last_sequence` at processing time (i.e., the prior accepted event's `sequence` on this `(producer_id, topic, partition)`) — **not** "sequence minus one."
- Broker check: `meta.previous == evbk_producer_state.last_sequence` AND `meta.sequence > last_sequence` → accept and advance; duplicates → `200 OK`; chain mismatch → `412 SequenceViolation` carrying the broker's known `last_sequence`.
- **Bootstrap**: the first chained-mode publish for a `(producer_id, topic, partition)` sets `meta.previous = 0` (since `last_sequence` defaults to 0 for absent rows).

### 2.4 Rule of Thumb

| Need | Mode |
|---|---|
| Consumer is idempotent; no broker dedup needed | **Stateless** |
| Synchronous publish, trust my counter; broker doesn't need to detect gaps for me | **Monotonic** |
| Async / unreliable hops; broker must detect unintended gaps and tell me where I stalled | **Chained** |

## 3. Wire Shapes (`meta` block)

The `meta` block lives on the event (`event.v1.schema.json`) and is marked `writeOnly` (publish-only). It is **optional**; absence = stateless. The broker strips `meta` from consumer-visible reads regardless of mode.

```jsonc
// Chained
"meta": { "version": 1, "producer_id": "<uuid>", "previous": 7, "sequence": 8 }

// Monotonic
"meta": { "version": 1, "producer_id": "<uuid>", "sequence": 8 }

// Stateless
// (meta omitted entirely; no producer_id on the wire)
```

Bootstrap (first publish for a chained `(producer_id, topic, partition)`):

```jsonc
"meta": { "version": 1, "producer_id": "<uuid>", "previous": 0, "sequence": 1 }
```

## 4. Actor Flows (CDSL)

### 4.1 Producer Registration

```python
# Producer fleet startup, single coordinator role (DB lock / leader election)
resp = http.POST("/v1/producers", json={
    "mode":         "chained",
    "client_agent": "myservice/1.0.0 myeventlib/2.0.0",
})
# 201 Created
# Location: /v1/producers/<uuid>
# body: { "id": "<uuid>", "mode": "chained", "client_agent": "<echoed>" }

producer_id = resp.json["id"]
distribute_to_fleet(producer_id)  # write to DB, ConfigMap, env var, secret store, etc.
```

Both `mode` and `client_agent` are required. `client_agent` is an informational diagnostic hint (RFC 9110 User-Agent grammar; ASCII; 1–256 bytes) — persisted on the producer row, surfaced in logs and metrics, immutable. Validation failure surfaces as the canonical RFC 9457 `400` problem type. The HTTP `User-Agent` request header is captured in access logs independently and is NOT a fallback. See §7 (Hard-Error Catalog).

Re-registration: any subsequent `POST /v1/producers` from the same principal mints a fresh `producer_id` (broker does NOT reuse). Old `producer_id` ages out via the producer-registration TTL Reaper (see §4.7).

### 4.2 Chained Mode Publish

```python
# Producer side — publish a window of events through an unreliable / async path
def publish_chained_window(events, last_acked_sequence):
    seq = last_acked_sequence
    for e in events:
        prev = seq
        seq  = prev + 1
        outbox.enqueue(Event(
            id=uuid4(), type="...", topic=T,
            tenant_id=current_tenant, source="...",
            subject="...", subject_type="...", occurred_at=now(),
            data=payload,
            meta={ "version": 1, "producer_id": P, "previous": prev, "sequence": seq },
        ))
    return seq  # the last sequence the producer attempted

# Ack-polling loop (runs independently of publish)
async def chained_ack_poll(producer_id):
    while True:
        cursors = await http.GET(f"/v1/producers/{producer_id}/cursors")
        # cursors = [{topic, partition, last_sequence}, ...]
        update_local_view(cursors)
        await sleep(POLL_INTERVAL)
        # If last_sequence has not advanced past the producer's window end,
        # re-publish from the broker's last_sequence + 1.

# Broker side — atomic transaction per event
row = state["evbk_producer_state"].get((meta.producer_id, topic, partition))
last = row.last_sequence if row else 0

if meta.previous == last and meta.sequence > last:
    BEGIN:
        outbox.enqueue(event)
        UPSERT evbk_producer_state(producer_id, topic, partition,
                                   last_sequence=meta.sequence,
                                   last_seen_at=now())
        UPDATE evbk_producer SET last_seen_at=now() WHERE producer_id=meta.producer_id
    COMMIT
    return 202_Accepted
elif meta.sequence <= last:
    return 200_OK(original_event_for(event.id))   # idempotent retry
else:
    return 412_SequenceViolation(known_last_sequence=last)
```

### 4.3 Monotonic Mode Publish

```python
# Producer side — synchronous publish; producer trusts its own counter
def publish_monotonic(event_data, next_sequence):
    resp = http.POST("/v1/events", json={
        "id": uuid4(), "type": "...", "topic": T,
        "tenant_id": current_tenant, "source": "...",
        "subject": "...", "subject_type": "...", "occurred_at": now(),
        "data": event_data,
        "meta": { "version": 1, "producer_id": P, "sequence": next_sequence },
    })
    if resp.status_code == 200:           # duplicate
        return resp.json["event_id"]
    elif resp.status_code == 202:         # new
        persist_local_last_sequence(next_sequence)
        return resp.json["event_id"]
    else:                                  # any error
        cursors = http.GET(f"/v1/producers/{P}/cursors")
        reconcile_local_state(cursors)
        raise PublishError(...)            # caller decides retry policy

# Broker side
row = state["evbk_producer_state"].get((meta.producer_id, topic, partition))
last = row.last_sequence if row else 0

if meta.sequence > last:
    BEGIN:
        outbox.enqueue(event)
        UPSERT evbk_producer_state(..., last_sequence=meta.sequence, last_seen_at=now())
        UPDATE evbk_producer SET last_seen_at=now() WHERE producer_id=meta.producer_id
    COMMIT
    return 202_Accepted
else:
    return 200_OK(original_event_for(event.id))   # duplicate
```

### 4.4 Stateless Mode Publish

```python
# Producer side — no registration, no meta
http.POST("/v1/events", json={
    "id": uuid4(), "type": "...", "topic": T,
    "tenant_id": current_tenant, "source": "...",
    "subject": "...", "subject_type": "...", "occurred_at": now(),
    "data": event_data,
    # meta omitted entirely
})

# Broker side — no state lookup, no producer_id resolution
BEGIN:
    outbox.enqueue(event)
COMMIT
return 202_Accepted

# Two publish attempts that include the same event.id under stateless
# yield TWO persisted events (different event-broker assigned `sequence`s).
# Consumer must be idempotent on event.id or content semantics.
```

### 4.5 Desync Recovery (Restart / DB Restore)

Scenario: producer's local `last_sequence` view is no longer trustworthy (process restarted without persistent state, local DB restored from older backup, suspected divergence).

```python
async def desync_recover(producer_id):
    cursors = await http.GET(f"/v1/producers/{producer_id}/cursors")
    # 200 OK: [{topic, partition, last_sequence}, ...]
    # 403 ProducerPrincipalMismatch — calling with a non-owning principal
    # 404 ProducerNotFound — producer aged out by TTL or never existed

    for c in cursors:
        local_state[(c.topic, c.partition)] = c.last_sequence
    # Resume publishing from local_state values
```

**Edge case: producer-ahead-of-broker.** If the producer's locally-tracked `last_sequence` is HIGHER than the broker's known `last_sequence` (e.g., broker DB restored from older backup, or state rows reaped before the producer reconnected):

- The producer SHOULD log an operator-facing warning. The local view is more advanced than the broker's; either the broker lost state or the producer has stale-but-untrusted local state.
- **Default**: register a fresh `producer_id` and start a new chain. The old `producer_id` ages out per TTL.
- **Alternative**: if the operator confirms the broker's view is authoritative (e.g., disaster-recovery scenario where the older backup is the intended state), rewind the producer's local cursor to the broker's view. Chain continuity is broken; reset semantics apply.

### 4.6 Chain Reset (Operator-Driven)

Scenario: producer's fleet is alive and well, but the chain state needs to be cleared (testing, debugging, manual recovery). The `producer_id` is preserved; the fleet does NOT redistribute a new id.

```python
# Operator (or owner-principal automation)
resp = http.POST(f"/v1/producers/{producer_id}:reset",
                 json={"topic": T, "partition": k})  # body optional; absent = reset all
# 200 OK with audit-record reference
# 403 ProducerPrincipalMismatch — non-owning principal
# 404 ProducerNotFound

# After reset, the next chained publish for the affected (producer_id, topic, partition)
# bootstraps from last_sequence=0:
#   meta = { version: 1, producer_id: P, previous: 0, sequence: 1 }
```

Audit record fields: `producer_id`, `requested_scope` (full or `(topic, partition)`), `operator_principal`, `timestamp`, `outcome`.

### 4.7 Producer Registration TTL (Natural Reset)

Producer rows track `evbk_producer.last_seen_at`, updated atomically with every accepted chained / monotonic publish. The Reaper purges rows older than the platform-wide producer-registration TTL (default `P30D`).

```python
# Reaper sweep (broker-internal)
async def reap_idle_producers():
    cutoff = now() - PRODUCER_TTL
    for row in evbk_producer.find(last_seen_at__lt=cutoff):
        BEGIN:
            DELETE FROM evbk_producer_state WHERE producer_id = row.producer_id
            DELETE FROM evbk_producer       WHERE producer_id = row.producer_id
        COMMIT
        log("reaped idle producer", producer_id=row.producer_id, last_seen_at=row.last_seen_at)

# Producer-side: next publish after age-out
resp = http.POST("/v1/events", json={..., "meta": {"producer_id": P, ...}})
# 400 UnknownProducer
# Producer re-registers, distributes new id, retires old (which is already gone).
```

The state-row retention (topic-level `retention`, capped at `P14D`) and the producer-row TTL are separate dials. State rows age out at the topic's pace; registration rows age out at the platform's pace. A registration row reap cascades to delete any orphaned state rows for the same `producer_id`.

## 5. Processes / Business Logic (CDSL)

### 5.1 Producer / Outbox Contract

The "exactly-once via idempotent producer" property is derived from a chain of guarantees:

1. **Producer business transaction → toolkit-db outbox**: the producer's business code writes both its domain state and the outbox row in one transaction. If the business txn rolls back, the outbox row is never persisted.
2. **toolkit-db outbox → SDK publish call**: the outbox pipeline reads pending rows in order and calls the SDK `publish` function. Network failures cause the SDK to retry the same outbox row.
3. **SDK publish call → broker ingest**: the SDK serializes the event with `meta.{producer_id, previous, sequence}` and POSTs to `/v1/events`. On any non-2xx, the SDK does not advance its outbox cursor.
4. **Broker ingest dedup → atomic enqueue + state update**: the broker checks the chain; if accepted, the ingest outbox enqueue, the `evbk_producer_state.last_sequence` advance, and the `evbk_producer.last_seen_at` touch happen in **one transaction**.

Failure modes and recovery:

| Failure point | What happens | Recovery |
|---|---|---|
| 1. Producer business txn / outbox enqueue split | Single transaction; rolls back together; no outbox row, no event | Producer retries (business txn re-runs) |
| 2. Outbox → ingest network failure | SDK times out; toolkit-db outbox keeps the row pending; retry on next sweep | Broker dedups via chain check on the next attempt (200 OK if first attempt had succeeded) |
| 3. Ingest crash between enqueue and state update | Single transaction; rolls back; no outbox row visible, no state advance | SDK times out (or sees 5xx); retries; broker re-applies |
| 4. Producer restart with in-flight outbox rows | Outbox pipeline resumes from its cursor; sends pending events with original `(producer_id, previous, sequence)` | Broker dedups duplicates (200 OK with original event_id); chain continues from where it left off |

The producer outbox considers an event "delivered" when the broker returns `202 Accepted` or `200 OK` (the latter for duplicates). The `202` does NOT wait for `backend.persist` — that's the broker-side ingest outbox's job.

## 6. States

`evbk_producer[producer_id]`:

| State | Meaning |
|---|---|
| Active | `last_seen_at` within the platform's producer-registration TTL window; producer in normal operation |
| Idle | `last_seen_at` approaching but not past the TTL window |
| Reaped | Row deleted by the Reaper; `producer_id` returns `400 UnknownProducer` on subsequent publish |

`evbk_producer_state[(producer_id, topic, partition)]`:

| State | Meaning |
|---|---|
| Absent | No event has been accepted for this triple yet (or the row was reaped, or `:reset` was called). Treated as `last_sequence = 0`; next chained event MUST set `meta.previous = 0` |
| Present | Row exists with `last_sequence = N`, `last_seen_at = T`. Next chained event MUST have `meta.previous = N`; next monotonic event MUST have `meta.sequence > N` |
| Reapable | `last_seen_at` older than the topic's `retention` (capped at `P14D`). Next Reaper run deletes the row |

## 7. Hard-Error Catalog

| HTTP | Code | When |
|---|---|---|
| 400 | `BadRequest` | Top-level forbidden field on publish (`producer_id`, `previous`, `sequence`, `partition`, `offset`, `offset_time`, `created_at`) |
| 400 | `InvalidMode` | `POST /v1/producers` with mode other than `chained` / `monotonic` |
| 400 | `ChainModeFieldsMissing` | Chained-mode publish missing `meta.previous` or `meta.sequence` |
| 400 | `MonotonicModeFieldsViolation` | Monotonic-mode publish with forbidden `meta.previous` or missing `meta.sequence` |
| 400 | `MetaWithoutProducerId` | `meta` has `previous` / `sequence` but no `producer_id` |
| 400 | `UnknownProducer` | `meta.producer_id` not in registry (or aged out by TTL) |
| 400 | `UnknownMetaVersion` | `meta.version > current_supported` |
| 400 | `InvalidEventFieldEncoding` | Non-ASCII bytes in any event field |
| 400 | `EventFieldTooLong` | Event string field exceeds length cap |
| 400 | `RetentionExceedsMaxSpan` | Topic created/updated with `retention > P14D` |
| 400 | `PartitionHashMismatch` | Internal SDK/broker partition hint disagrees with the broker's authoritative derivation from `partition_key` or `tenant_id` |
| 403 | `ProducerPrincipalMismatch` | Cross-principal use of `producer_id` (publish / cursor read / reset) |
| 403 | `TenantIdNotAuthorized` | Platform authz resolver denied the supplied `tenant_id` |
| 404 | `ProducerNotFound` | `GET /v1/producers/{id}/cursors` or `POST :reset` for an unknown `producer_id` |
| 412 | `SequenceViolation` | Chained mode: `meta.previous != last_sequence`; response carries broker's known `last_sequence` |

## 8. Definitions of Done

### Idempotent Producer Contract

- [ ] `p1` - **ID**: `cpt-cf-evbk-dod-idempotent-producers`

- `evbk_producer` and `evbk_producer_state` tables created per `migration.sql`.
- `POST /v1/producers` mints `producer_id`, stores `mode` + `owner_principal` + `last_seen_at`.
- `POST /v1/events` and `POST /v1/events:batch` enforce mode-shape rules per the registered mode; reject mode mismatches with the documented `400` codes; reject chain mismatches with `412 SequenceViolation`.
- `GET /v1/producers/{producer_id}/cursors` returns per-`(topic, partition)` `last_sequence`; principal-bound.
- `POST /v1/producers/{producer_id}:reset` clears state rows (full or scoped); emits audit record; principal-bound.
- Reaper deletes stale `evbk_producer_state` rows (per topic-level `retention`) and stale `evbk_producer` rows (per platform-wide producer-registration TTL); cascade purges state rows for reaped producers.
- Ingest performs the outbox-enqueue + state-update + `evbk_producer.last_seen_at` touch in one transaction.
- Producer SDK declares mode at startup, hashes partition locally for outbox routing, sends `meta` per the registered mode.
- Metrics: `evbk_producer_sequence_violation_total`, `evbk_producer_duplicate_total`, `evbk_producer_state_rows`, `evbk_producer_rows`, `evbk_producer_state_reaper_deleted_total`, `evbk_producer_reaper_deleted_total`.

## 9. Acceptance Criteria

- **AC-1**: A chained producer publishes `(previous=0, sequence=1)` then `(previous=1, sequence=2)`; both land; `evbk_producer_state.last_sequence = 2`.
- **AC-2**: A chained producer re-publishes `(previous=1, sequence=2)` after a transport timeout; broker returns `200 OK` with the original event_id; no duplicate in the log.
- **AC-3**: A chained producer publishes `(previous=5, sequence=6)` when `last_sequence = 4`; broker returns `412 SequenceViolation` carrying `last_sequence=4`; no row mutation.
- **AC-4**: A monotonic producer publishes `sequence=10` then `sequence=20`; both land; `last_sequence = 20`.
- **AC-5**: A stateless producer publishes 1000 events with no `meta`; all 1000 are persisted distinctly; `evbk_producer_state` has no rows for the principal.
- **AC-6**: A producer publishes once; waits past the topic's `retention`; Reaper deletes the state row; the next chained publish with `meta.previous=0, meta.sequence=1` is accepted.
- **AC-7**: A producer remains idle past the producer-registration TTL; Reaper deletes `evbk_producer`; the next publish from the producer's fleet using the old `meta.producer_id` returns `400 UnknownProducer`.
- **AC-8**: Operator calls `POST /v1/producers/{id}:reset` (full scope); state rows deleted; audit record created; next chained publish with `meta.previous=0` accepted.
- **AC-9**: Operator calls `POST /v1/producers/{id}:reset { "topic": T, "partition": k }`; only the matching state row is deleted; other `(producer_id, topic, partition)` rows untouched.
- **AC-10**: Principal A registers a producer; principal B publishes with A's `meta.producer_id` → `403 ProducerPrincipalMismatch`.
- **AC-11**: Producer publishes with `meta.version` greater than broker's supported → `400 UnknownMetaVersion`.
- **AC-12**: Producer publishes with `meta` containing `sequence` but no `producer_id` → `400 MetaWithoutProducerId`.
- **AC-13**: Topic is created with `retention = P30D` → `400 RetentionExceedsMaxSpan`.

## 10. Unit Test Plan

- **Mode-shape matrix**: chained / monotonic / stateless × valid / missing-required / forbidden-present / wrong-mode-for-registered-id → each cell produces the documented outcome.
- **Chain check**: parameterize over (`evbk_producer_state` row state × incoming `meta.previous` × incoming `meta.sequence`) → expected (accept / duplicate / chain-broken, row delta).
- **Monotonic gap acceptance**: `last_sequence = 10`, incoming `sequence = 15` → accepted; rows `11–14` are never delivered.
- **Stateless skip**: event with no `meta` → broker does not touch `evbk_producer_state`.
- **Atomicity**: simulated ingest crash between outbox enqueue and `evbk_producer_state` update → no half-state after recovery.
- **Principal binding**: cross-principal publish / cursor read / reset → `403 ProducerPrincipalMismatch`.
- **TTL Reaper**: producer-row TTL elapses → row deleted on next sweep; cascade purge of orphaned state rows.
- **`meta.version` accept / reject**: `version <= supported` accepted; `version > supported` → `400 UnknownMetaVersion`.
- **Event field encoding**: non-ASCII bytes in any event field → `400 InvalidEventFieldEncoding`.
- **Reset audit**: every successful reset emits an audit record.

## 11. E2E Test Plan

- **E1 — Chained happy path**: producer fleet registers; publishes a chain of 100 events; consumer receives all in order; `last_sequence = 100`; producer-row `last_seen_at` advances.
- **E2 — Retry after timeout**: producer publishes event N; simulated transport timeout; producer retries; consumer sees event N exactly once; broker returns `200 OK` on the retry.
- **E3 — Chain break recovery**: producer publishes `(previous=0, sequence=1)`, then `(previous=99, sequence=100)` → `412 SequenceViolation` carrying `last_sequence=1`; producer calls `GET /cursors`, observes `last_sequence=1`, corrects to `(previous=1, sequence=2)`, retries → accepted.
- **E4 — Monotonic recovery**: monotonic producer's local DB is restored to a prior backup; producer calls `GET /cursors`, sees `last_sequence=N` higher than its local view; operator confirms broker is authoritative; producer rewinds local cursor to `N+1` and resumes.
- **E5 — Async windowed publish (chained)**: producer fires 1000 events into an async / lossy channel; ack-poll loop calls `GET /cursors` every 5s; on detecting stalled cursor, re-publishes from stall point; eventually all 1000 land; `last_sequence = 1000`.
- **E6 — Producer-state Reaper (topic retention)**: configure topic `retention = PT5S`; chained producer publishes one event; waits 10s; Reaper deletes state row; producer's next publish with `(previous=0, sequence=1)` is accepted.
- **E7 — Producer-registration Reaper (TTL)**: configure producer-registration TTL = `PT10S`; chained producer publishes one event; waits 15s; next publish with same `meta.producer_id` → `400 UnknownProducer`; producer re-registers, distributes new id, resumes.
- **E8 — Operator reset (full)**: operator calls `POST /v1/producers/{id}:reset`; verifies all state rows gone; producer's next chained publish with `meta.previous=0` is accepted; audit record present.
- **E9 — Operator reset (scoped)**: operator calls reset with `{topic, partition}`; only matching row deleted; chains on other partitions continue normally.
- **E10 — Cross-principal rejection**: principal B attempts publish / cursor read / reset on principal A's `producer_id` → `403 ProducerPrincipalMismatch` for all three.
- **E11 — Stateless duplicate admission**: stateless producer publishes the same `event.id` payload twice (no `meta`); both land; consumer absorbs duplicates by `event.id`.
- **E12 — Tenant authz**: producer publishes with `tenant_id` outside its principal's grant → `403 TenantIdNotAuthorized` (platform resolver decision); producer with grant → accepted.

## 12. Traceability

- **PRD**: [PRD.md](../PRD.md)
  - `cpt-cf-evbk-fr-producer-modes`
  - `cpt-cf-evbk-fr-publish-single`
  - `cpt-cf-evbk-fr-publish-batch`
- **ADRs**:
  - [`0002-partition-selection`](../ADR/0002-partition-selection.md)
  - [`0002-event-schema`](../ADR/0003-event-schema.md)
  - [`0003-idempotent-producer-protocol`](../ADR/0004-idempotent-producer-protocol.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
  - §3.1 Domain Model
  - §3.2 Producer Modes (shrunk to summary + link to this feature doc)
  - §3.6 Two Sequences
  - §3.7 Database schemas — `evbk_producer`, `evbk_producer_state` rows
- **Schemas**:
  - [`schemas/event.v1.schema.json`](../schemas/event.v1.schema.json) — publish input
  - [`schemas/event.v1.schema.json`](../schemas/event.v1.schema.json) — single canonical event schema; read responses surface `readOnly` `partition`/`sequence`/`sequence_time` and strip `writeOnly` `meta`
