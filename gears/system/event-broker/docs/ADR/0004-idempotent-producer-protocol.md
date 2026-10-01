---
status: accepted
date: 2026-05-12
decision-makers: Event Broker Team
---

# Idempotent Producer Protocol — Mode Declared At Registration, Enforced Per Request

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Three Modes, Wire Shapes](#three-modes-wire-shapes)
  - [Registration: `POST /v1/producers`](#registration-post-v1producers)
  - [Mode-Shape Enforcement at Publish Time](#mode-shape-enforcement-at-publish-time)
  - [Mode Immutability](#mode-immutability)
  - [Chain Scope](#chain-scope)
  - [Bootstrap Chain Value](#bootstrap-chain-value)
  - [Chain Reset — Two Levers](#chain-reset--two-levers)
  - [Producer Registration TTL](#producer-registration-ttl)
  - [Stateless Safety Floor](#stateless-safety-floor)
  - [Producer Concurrency](#producer-concurrency)
  - [Producer Identity Principal Binding](#producer-identity-principal-binding)
  - [Atomicity: Outbox Enqueue + State Update](#atomicity-outbox-enqueue--state-update)
  - [Batch Publish](#batch-publish)
  - [Hard-Error Catalog](#hard-error-catalog)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Mode Declared At Registration (chosen — Option B)](#mode-declared-at-registration-chosen--option-b)
  - [Inferred Per Request From `meta` Fields](#inferred-per-request-from-meta-fields)
  - [Per-Request `meta.mode` Discriminator](#per-request-metamode-discriminator)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-evbk-adr-idempotent-producer-protocol`

## Context and Problem Statement

The event broker offers three producer modes for ingest-side idempotent publishing:

- **chained**: per-event `previous` + `sequence`; broker enforces chain continuity; gap detection via the link.
- **monotonic**: per-event `sequence`; broker enforces strict advancement of `last_sequence`; gaps detected by the producer via cursor reconciliation.
- **stateless**: no broker-side dedup; consumer carries the idempotency burden.

The initial design (DESIGN.md §3.2 Producer Modes) **inferred mode per request** from which fields the producer happened to set:

| Mode | Producer sets |
|---|---|
| chained | `producer_id`, `previous`, `sequence` |
| monotonic | `producer_id`, `sequence` (no `previous`) |
| stateless | none |

This mirrors the partition-override problem in [ADR-0002](0002-partition-selection.md): a producer refactor that drops `meta.previous` silently downgrades the producer from chained-mode dedup to monotonic-mode dedup with **no error and no observable signal** until a duplicate slips through. Same silent-switch hazard, different field.

This ADR locks in **mode declared at registration**, with broker-side per-request enforcement, and resolves the satellite questions: bootstrap chain value, chain reset, producer-registration TTL, stateless safety floor, producer concurrency, principal binding, and the publish-time atomicity contract.

The broker is unshipped — no production data, no live producers — so this ADR lands as a single coherent surface before implementation begins.

## Decision Drivers

* Explicit contract: producer mode must be a declared property, not an emergent property of which fields a producer happened to set
* Hard errors at the wire boundary: mode-shape violations reject the publish loudly, not silently weaken dedup
* Symmetry with partition selection: same principle ([ADR-0002 revised](0002-partition-selection.md)) - the producer never chooses a partition; the backend assigns it ([feature 0007](../features/0007-storage-backend-api.md)), and producer freedom is constrained to the choice the producer intended to make
* Principal binding: a `producer_id` is owned by the principal that created it; cross-principal use is rejected
* Per-event chain values in batches: every event carries its own `meta`, so one batch carries events of several producer ids and topics, and each `(producer_id, topic)` run in it is validated against its own chain
* Future-mode extensibility: new modes (e.g., post-MVP recent-`event.id` LRU stateless variant) ship as new registration values without changing the event schema
* Operator-driven reset must exist but must be auditable
* Idle producers should age out automatically so producer-state storage doesn't grow without bound

## Considered Options

* **Option A** — Inferred per request from `meta` fields (status quo of the initial design)
* **Option B** — Mode declared at `POST /v1/producers`, enforced per request (chosen)
* **Option C** — Per-request `meta.mode` discriminator (`meta.mode: "chained" | "monotonic" | "stateless"`)

## Decision Outcome

Adopt **Option B - mode declared at producer registration, enforced per request**. A producer registers once with `POST /v1/producers { "mode": "chained" | "monotonic" }`, the broker stores `mode` on the producer row, and every subsequent publish referencing that `producer_id` is validated against the stored mode. Mode-shape mismatches reject with `400`. Stateless mode does **not** register: no row, no `meta.producer_id` on publish, no broker-side dedup. A producer id is not bound to a topic: it publishes to any number of topics, and the broker keeps one chain per `(producer_id, topic)` (see [Chain Scope](#chain-scope)).

### Three Modes, Wire Shapes

The `meta` block (per [ADR-0003 Event Schema](0003-event-schema.md)) is the carrier for all producer-protocol fields. Mode determines its shape:

```jsonc
// Chained — registered with POST /v1/producers { "mode": "chained" }
"meta": { "version": 1, "producer_id": "<uuid>", "previous": 7, "sequence": 8 }

// Monotonic — registered with POST /v1/producers { "mode": "monotonic" }
"meta": { "version": 1, "producer_id": "<uuid>", "sequence": 8 }

// Stateless — no registration, meta omitted entirely
// (no producer_id, no chain state, no broker dedup)
```

Within `meta`, `previous` and `sequence` are the producer-chain pair (no naming collision with the body-level `sequence` — the `meta.` qualifier disambiguates per [ADR-0003 § Terminology Cleanup](0003-event-schema.md#terminology-cleanup-offset--sequence)).

#### When to Choose Each Mode

For the purposes of this ADR, a **gap** is any missing value in a producer's sequence stream — intentional (a business-txn rollback, an operator deletion, a deliberate skip; re-sequencing is often not an option) or unintentional (lost in transit, out-of-order arrival, producer view diverging from broker view). All three modes accept intentional gaps. The modes differ in whether the broker can detect unintended ones.

- **Stateless** — use when the consumer's processing is naturally idempotent (UPSERT, set-state-to-X, idempotent task triggers), or the producer is ephemeral (lambdas, short-lived workers), or events are non-causal. Broker holds no state, performs no dedup.
- **Monotonic** — use when the publish path is reliable / synchronous and the producer trusts its own counter. Broker accepts any `meta.sequence > last_sequence`; cannot distinguish intentional gaps from unintentional ones. Recovery on error: `GET /v1/producers/{producer_id}/cursors` → reconcile → resume. Cheaper than chained.
- **Chained** — use when the publish path is async / unreliable / windowed AND the producer needs the broker to detect unintended gaps. Producer fires a window, later polls `GET /v1/producers/{producer_id}/cursors`; re-publishes from the stall point if cursor didn't advance. Out-of-order arrival or transit loss surfaces as `412 SequenceViolation` carrying the broker's `last_sequence`.

Rule of thumb:

- "Consumer is idempotent; no broker dedup needed" → **stateless**
- "Synchronous publish, trust my counter; broker doesn't need to detect gaps for me" → **monotonic**
- "Async / unreliable hops; broker must detect unintended gaps and tell me where I stalled" → **chained**

### Registration: `POST /v1/producers`

- **Request body**: `{ "mode": "chained" | "monotonic", "client_agent": "<rfc-9110-user-agent-string>" }`. Both fields are required. The body MUST NOT contain `producer_id` or `id` — broker-minted.
- **Response**: `201 Created`, body `{ "id": "<uuid>", "mode": "<mode>", "client_agent": "<echoed>" }`, header `Location: /v1/producers/<id>`.
- **Authn**: requires an authenticated principal. The producer row records the principal as `owner_principal`.
- **Modes other than `chained` / `monotonic`** (including `stateless`) → `400 InvalidMode`. Stateless does not register.
- **`client_agent`** is an informational diagnostic hint — purely observational. The broker persists it on the producer row, echoes it on the registration and cursor responses, and surfaces it in operational logs and metric labels. It does **not** participate in deduplication, authorization, ownership, or any other load-bearing decision; collisions across producers are allowed. Validation: ASCII, 1–256 bytes, conforms to RFC 9110 User-Agent grammar (`product *( RWS ( product / comment ) )`). Failure surfaces as the canonical RFC 9457 `400` problem type; no broker-specific error code. Immutable after create — no mutation endpoint exists. The HTTP `User-Agent` request header continues to be captured in access logs on every request independently of `client_agent` and is NOT a fallback for a missing `client_agent`.

The caller persists the returned `id` (as `producer_id`) and distributes it to its producer fleet (DB, ConfigMap, env var, secret store — caller's choice of coordination mechanism).

### Mode-Shape Enforcement at Publish Time

On every publish, after authn but before any storage write:

1. If `meta` is absent → stateless publish path. Broker does not consult the producer registry. Accept at face value (subject to event-schema validation).
2. If `meta` is present:
   - If `meta.producer_id` is absent but other producer-protocol fields are present (`meta.previous` or `meta.sequence`) → reject `400 MetaWithoutProducerId`.
   - If `meta.producer_id` is absent and no other producer-protocol fields → stateless publish path (treat as if `meta` were absent).
   - If `meta.producer_id` is present:
     - Look up the producer row.
     - If not found → reject `404 ProducerNotFound`.
     - If owner principal does not match the calling principal → reject `403 ProducerPrincipalMismatch`.
     - Validate shape against the stored mode:

| Stored mode | Required in `meta` | Forbidden in `meta` | Error on mismatch |
|---|---|---|---|
| `chained` | `producer_id`, `previous`, `sequence` | — | `400 ChainModeFieldsMissing` |
| `monotonic` | `producer_id`, `sequence` | `previous` | `400 MonotonicModeFieldsViolation` |

3. If `meta.version` exceeds the broker's supported version → reject `400 UnknownMetaVersion` (per [ADR-0003 § Optional Versioned `meta` Block](0003-event-schema.md#optional-versioned-meta-block)).

After validation passes, mode-specific business rules apply:

- **Chained**: accept iff `meta.previous == evbk_producer_state.last_sequence` AND `meta.sequence > last_sequence`; gaps between sequences are accepted. Duplicate (`meta.sequence == last_sequence`) -> `200 OK`, status only. Anything else -> `412 SequenceViolation` carrying the broker's `last_sequence`.
- **Monotonic**: accept iff `meta.sequence > evbk_producer_state.last_sequence`. Duplicate (`meta.sequence <= last_sequence`) -> `200 OK`. Gaps between sequences are accepted.

`last_sequence` is read from the `(producer_id, topic)` row of the event's topic. A batch applies the same rules per run (see [Batch Publish](#batch-publish)).

### Mode Immutability

`mode` is immutable for the lifetime of a `producer_id`. The broker exposes no operation that changes the mode of an existing producer row. To switch modes, a producer:

1. Registers a fresh `producer_id` via `POST /v1/producers` with the new mode.
2. Distributes the new `producer_id` across its fleet.
3. Retires the old `producer_id` (its state ages out via the Reaper, see [Producer Registration TTL](#producer-registration-ttl)).

Why immutable: reusing `evbk_producer_state` rows across modes is unsafe. The chained invariant (`previous` == `last_sequence`) does not hold against the monotonic gap-accepting rule, and switching mid-stream would cause both modes to misbehave on in-flight events.

### Chain Scope

Chain state is kept per `(producer_id, topic)`: `evbk_producer_state` has `PRIMARY KEY (producer_id, topic)` and holds the pair's `last_sequence`.

- **No topic binding.** A producer id is not bound to a topic; registration names none (`[mode, client_agent]`). One id publishes to any number of topics, each topic its own chain, ordered and checked independently of the others.
- **No partition.** The partition is not part of the chain. The backend assigns it after ingest admits the event ([feature 0007](../features/0007-storage-backend-api.md)); neither the producer nor the broker computes one for the chain, and `meta` carries no partition.
- **Gaps across topics.** A counter shared across topics (for example a producer-outbox partition's sequence) is valid: each topic's chain sees that counter with gaps, and both modes accept gaps.

#### Producer Partitions

A producer without partitions uses one producer id. A partitioned producer - one whose own work is split into partitions that number their events independently, such as the outbox partitions of the SDK's `DbProducer` - registers one producer id per producer partition, so each id has exactly one sequence space.

The rule is required, not advisory. A sequence space shared across producer partitions interleaves on a topic's chain: producer partition 3 sends `sequence = 7` after producer partition 9 sent `sequence = 40`. Chained mode answers `412`; monotonic mode treats `7` as a duplicate, answers `200 OK` and drops the event silently. The broker cannot tell such an interleave from a retry, so the producer guarantees one sequence space per producer id.

### Bootstrap Chain Value

The first accepted publish for a `(producer_id, topic)` pair creates its `evbk_producer_state` row and takes its numbers. The contract:

- A missing row admits the first publish without a chain check; the row is inserted with `last_sequence = meta.sequence`.
- Chained mode accepts any `meta.previous` on that first publish (the shape rule `meta.sequence > meta.previous` still applies); the conventional first step is `meta.previous = 0, meta.sequence = 1`.
- Monotonic mode accepts any `meta.sequence` on that first publish.
- The same bootstrap rule applies after a `:reset` (see below).

### Chain Reset — Two Levers

Two distinct paths exist for chain reset, serving different scenarios:

#### Operator-driven reset: `POST /v1/producers/{producer_id}:reset`

- **Request body** (optional): `{ "topic": "..." }` to scope the reset to the `(producer_id, topic)` row. Body absent -> reset all `evbk_producer_state` rows for the `producer_id`.
- **Authz**: owning principal only. Cross-principal → `403 ProducerPrincipalMismatch`.
- **Audit**: every reset emits an audit record (operator-driven destructive operation).
- **Effect**: deletes `evbk_producer_state` rows; next publish bootstraps fresh (see [Bootstrap Chain Value](#bootstrap-chain-value)).
- **`producer_id` is preserved**. The fleet does not need to redistribute a new id.

Use case: the producer's fleet is alive and well, but the chain state on the broker side is wrong (or needs to be cleared for testing / debugging) and the producer resumes from any sequence: the next accepted publish takes its numbers.

#### Natural reset: Producer Registration TTL

A producer's registration row carries `last_seen_at`, updated on every publish of the producer id, on any topic. The Reaper purges `evbk_producer` rows whose `last_seen_at` is older than the platform's producer-registration TTL, together with their chain rows (see [Producer Registration TTL](#producer-registration-ttl)). After purge, the `producer_id` is gone - the next publish referencing it gets `404 ProducerNotFound`, the producer re-registers, distributes the new id, and continues.

Use case: long-quiet producers (monthly batch job that hasn't run in 6 months) shouldn't keep their identity forever. The TTL forces a natural re-registration cycle.

### Producer Registration TTL

- Default TTL: platform-wide setting (initial proposed value `P30D` — 30 days). Configurable per-deployment.
- TTL is **per producer registration row**. A `(producer_id, topic)` chain row lives as long as its producer registration and has no retention of its own; a quiet topic's row stays while the producer publishes to other topics.
- A producer's `evbk_producer.last_seen_at` is updated with every publish of the producer id, on any topic.
- Reaper sweep cadence: bounded (default `PT5M`); exact cadence is implementation detail, not spec.
- Purge cascade: an `evbk_producer` row and every `evbk_producer_state` row of the same `producer_id` are deleted together, in one transaction.
- Post-purge publish: `404 ProducerNotFound`. Producer must re-register and obtain a new `producer_id`.

### Stateless Safety Floor

In MVP, stateless mode performs **no broker-side dedup**. A publish-retry under stateless yields two persisted events; the consumer absorbs duplicates via idempotent processing.

Documented loudly in the producer-author guidance. Stateless = "consumer carries idempotency."

**Post-MVP extension** (not in this ADR): an opt-in recent-`event.id` LRU at registration time, e.g.:

```jsonc
// Hypothetical future shape — NOT shipped at MVP
POST /v1/producers
{ "mode": "stateless", "dedup_window": "PT5M" }
```

Captured here so the registration body's future evolution is anticipated. If/when this lands, it adds a new mode value or a new registration field; the event schema is unaffected.

### Producer Concurrency

The broker supports **single-writer per `producer_id`**. HA producer topologies use external leader election (Raft, etcd, Consul, DB row lock — caller's choice). The broker does NOT provide:

- Producer epochs / leases / fencing
- Cluster-wide active/standby coordination

Concurrent writers sharing a `producer_id` will see chain failures (`412 SequenceViolation` for chained mode) or monotonic regressions (the lower-`sequence` writer gets `200 OK` duplicate, then catches up against the higher-sequence writer's state — natural divergence detector). This is documented as **misuse, not a bug**.

This keeps the broker simple: sequence-ordering itself is the only guard. Adding epoch fields to `meta` was considered and rejected — see [More Information](#more-information).

### Producer Identity Principal Binding

The `owner_principal` is recorded on the producer row at `POST /v1/producers` time. Every endpoint that touches the producer's state (`POST /v1/events`, `GET /v1/producers/{id}/cursors`, `POST /v1/producers/{id}:reset`) validates the calling principal against `owner_principal`:

- Match → proceed.
- Mismatch → `403 ProducerPrincipalMismatch`.

`producer_id` is not a secret — it is distributed across the producer's fleet via the caller's coordination mechanism. The `403` is explicit ("you don't own this") rather than `404` (info-protective); concealing existence offers no real security benefit because the id is intentionally widely distributed.

Cursor-endpoint authz: ownership IS the authz check. No separate permission constant beyond the principal-ownership rule.

### Atomicity: Outbox Enqueue + State Update

For accepted chained / monotonic publishes, the broker performs the ingest-outbox enqueue and the `evbk_producer_state` update **in one transaction**:

```sql
BEGIN;
  INSERT INTO outbox(...) VALUES (...);                     -- enqueue for the dispatcher
  UPDATE evbk_producer_state                                -- INSERT for the pair's first accepted publish
     SET last_sequence = $meta_sequence
   WHERE producer_id = $pid AND topic = $topic;
  UPDATE evbk_producer
     SET last_seen_at = now()
   WHERE producer_id = $pid;
COMMIT;
```

A duplicate-only publish writes nothing to the outbox or the chain row, but still refreshes `evbk_producer.last_seen_at`: a producer that keeps retrying is alive and must not age out. Every answer - `200`, `201`, `202` - is status only.

Outcomes by failure point:

1. **Producer business txn commit / outbox enqueue split** — handled by `toolkit-db`'s transactional outbox at the producer side; not a broker concern.
2. **Outbox → ingest network failure mid-publish** - producer SDK retries; broker dedups via chain check (chained) or `sequence` check (monotonic) and returns `200 OK`, status only.
3. **Ingest crash between enqueue and state update** — single transaction; commits all-or-nothing; producer sees publish failure and retries.
4. **Producer restart with in-flight outbox rows** — producer SDK resumes from its outbox; broker dedups via mode-specific check.

This atomicity is the central invariant of the "exactly-once via idempotent producer" claim. A publish answered `201 Created` or `202 Accepted` guarantees BOTH the outbox row is persisted AND the chain state has advanced (or, in stateless, the event has been accepted for storage without chain state); `200 OK` means every event of the request is a duplicate.

A publish is answered `202 Accepted` once the enqueue commits. A producer that needs the events stored asks with `Prefer: wait=N` (RFC 7240; `N` in seconds, capped by broker config). Ingest mints a trace internally (a UUID, never on the wire), subscribes to it and enqueues under it in the same transaction, then waits up to `N`:

- every event stored by the backend -> `201 Created`
- `N` elapsed first -> `202 Accepted` (the events stay enqueued and are stored later)
- the backend reports a terminal failure -> `5xx`
- every event a duplicate -> `200 OK`, with no wait

### Batch Publish

`POST /v1/events:batch` carries events of any producer ids and topics, and stateless events, in one request. Each event's `meta` passes mode-shape enforcement on its own. The broker then splits the chained and monotonic events into runs, one run per `(producer_id, topic)`, each keeping request order; stateless events belong to no run. A single-event publish is a run of one.

Request-shape rules, checked before any chain state is read:

- **R1** - an empty batch -> `400 empty_batch`.
- **R3** - in-run shape: in a chained run every event has `sequence > previous`, and every event after the first has `previous` equal to the prior event's `sequence`; a monotonic run's sequences strictly increase. A violation -> `400 malformed_run { index }`, naming the first offending event.
- **R13** - an `event.id` repeated within the batch -> `400 duplicate_event_id { index }`, naming the repeat.

Chain rules, per run against the pair's `last_sequence`:

- **R4 (chained)** - the run advances if its first event's `previous == last`. The whole run is a duplicate if its last event's `sequence == last`. If an event `e[k]` has `sequence == last`, events through `e[k]` are a duplicate prefix and the rest advances. Anything else -> `412`.
- **R5 (monotonic)** - the leading events with `sequence <= last` are a duplicate prefix; the rest exceeds `last` and advances.

Outcome:

- **All-or-nothing.** The first violation rejects the whole batch with `412 SequenceViolation`, whose `sequence_mismatch` violation carries `{index, producer_id, topic, last_sequence}` (`index` of the violating run's first event); nothing is enqueued.
- **Duplicates skipped.** Duplicate events are not enqueued; the rest of the batch is. A batch of only duplicates is answered `200 OK`.
- **No cross-run order.** Runs are checked independently; a batch promises no order between events of different runs beyond per-key order (ADR-0002).

### Hard-Error Catalog

| HTTP | Code | When | Recovery |
|---|---|---|---|
| 400 | `BadRequest` | Top-level forbidden field (producer-protocol, backend-assigned, or explicit `partition`) | Fix request shape |
| 400 | `InvalidMode` | Registration with mode other than `chained` / `monotonic` | Use a valid mode (stateless = no registration) |
| 400 | `ChainModeFieldsMissing` | Chained-mode publish missing `meta.previous` or `meta.sequence` | Fix request shape |
| 400 | `MonotonicModeFieldsViolation` | Monotonic-mode publish with forbidden `meta.previous` or missing `meta.sequence` | Fix request shape |
| 400 | `MetaWithoutProducerId` | `meta` carries `previous` / `sequence` but no `producer_id` | Either omit `meta` entirely (stateless) or include `meta.producer_id` |
| 404 | `ProducerNotFound` | `meta.producer_id` not found in registry (or aged out by TTL); names the producer resource | Re-register and distribute new id |
| 400 | `UnknownMetaVersion` | `meta.version` exceeds broker's supported version | SDK rolls back to a supported version |
| 400 | `InvalidEventFieldEncoding` | Non-ASCII bytes in event field | Sanitize input |
| 400 | `EventFieldTooLong` | Event string field exceeds length cap | Sanitize input |
| 403 | `ProducerPrincipalMismatch` | Cross-principal publish / cursor read / reset | Use the owning principal |
| 403 | `TenantIdNotAuthorized` | Platform authz resolver denied the `tenant_id` | Acquire grant (platform-side) |
| 400 | `BadRequest` (`empty_batch`) | Batch with no events (R1) | Send at least one event |
| 400 | `BadRequest` (`malformed_run`) | A run's events do not link or increase (R3); names the event `index` | Fix the run's numbering |
| 400 | `BadRequest` (`duplicate_event_id`) | An `event.id` repeated within one batch (R13); names the event `index` | Send each event once |
| 412 | `SequenceViolation` | Chained run neither advances nor duplicates the chain head (R4); body `{index, producer_id, topic, last_sequence}` | `GET /v1/producers/{id}/cursors` -> reconcile -> resume |

### Consequences

- Good, because mode-switch bugs become hard errors at the wire boundary instead of silent dedup degradation
- Good, because the wire `meta` block has three crisp shapes, one per mode, each enforced by JSON Schema `oneOf` + broker-side mode lookup
- Good, because the `:reset` lever + producer-TTL lever cover both operator-driven and natural reset scenarios without forcing one paradigm
- Good, because future modes (post-MVP stateless-with-LRU, or future "monotonic-with-gap-rejection") slot in as new registration mode values without changing the event schema
- Good, because principal binding is enforced uniformly across publish, cursor read, and reset
- Bad / accepted, because registration is now a required step for chained / monotonic producers (already true today; one extra HTTP call at first-publish)
- Bad / accepted, because mode immutability forces re-registration to switch modes (deliberate friction; mode-switching is rare and dangerous)
- Bad / accepted, because the no-fencing concurrency model means concurrent writers on the same `producer_id` cause data anomalies that surface as `412` / monotonic regressions, not as a clean error. Mitigation: producer-author guidance ("single-writer per `producer_id`").
- Neutral, because `POST /v1/producers/{id}:reset` is destructive and authz-fenced; operators must explicitly choose to call it, and every call is audited.

### Confirmation

The decision is verified by:

- **Wire-shape rejection tests** for each hard-error code listed above, with sample valid + invalid payloads.
- **Mode-shape enforcement test matrix**: chained / monotonic / stateless × valid / missing-field / forbidden-field / wrong-mode-for-registered-id. Every cell produces the documented outcome.
- **Atomicity test**: simulate ingest crash between outbox enqueue and state update; verify no half-state visible after recovery.
- **Reset audit test**: every successful `:reset` call produces an audit record with operator principal + timestamp + scope.
- **TTL reap test**: idle producer's row is reaped after the TTL window; next publish gets `404 ProducerNotFound`.
- **Principal binding test**: cross-principal calls to publish / cursor read / reset all return `403 ProducerPrincipalMismatch`.
- **Batch run test**: one batch with events of two producer ids, two topics and stateless events; each of R1, R3, R4, R5 and R13 produces its documented outcome, and a `412` leaves nothing enqueued.
- **Concurrent writer test**: two writers sharing a `producer_id` produce `412 SequenceViolation` (chained) or monotonic regression (monotonic) without broker error — documented behavior.

## Pros and Cons of the Options

### Mode Declared At Registration (chosen — Option B)

* Good, because mode is an explicit declared property, not an emergent one
* Good, because mode-shape violations are hard `400`s, not silent dedup degradation
* Good, because future modes plug in as new registration values without event-schema churn
* Good, because principal binding has a natural home (the producer row)
* Bad, because chained / monotonic publish requires registration (one-time setup); stateless does not
* Bad, because switching modes requires re-registration (deliberate friction)

### Inferred Per Request From `meta` Fields

**Description**: The status-quo of the initial design. Mode is per-request, inferred from which fields the producer happens to set. No registration required for any mode.

* Good, because no registration round-trip for chained / monotonic
* Good, because the wire is self-describing per event
* Bad / decisive against, because **a producer refactor that drops `meta.previous` silently downgrades chained → monotonic** with no error and no signal; same silent-switch hazard as the partition override removed in [ADR-0002 (revised)](0002-partition-selection.md)
* Bad, because principal binding has no natural home; ad-hoc per-publish lookup of "who owns this `producer_id`?" against an implicit registry
* Bad, because future modes require adding new per-request fields or discriminators

### Per-Request `meta.mode` Discriminator

**Description**: `meta.mode: "chained" | "monotonic" | "stateless"` on every event; broker validates required fields per declared mode.

* Good, because mode is explicit per request — no hidden inference
* Bad, because per-event mode-switching is not a real use case; producers stay in one mode for their lifetime
* Bad, because it adds yet another inconsistency vector: a producer that sets the right fields but the wrong `meta.mode` (or vice versa) gets a confusing error
* Bad, because it duplicates information that the registration model captures once
* Bad, because principal binding is still unsolved (need a separate registry mechanism anyway)
* Captured for completeness; the registration model dominates on every axis

## More Information

- **Producer epochs / leases** (rejected): adding a `meta.producer_epoch` field plus broker-side epoch tracking would enable clean active/standby failover (standby bumps epoch on takeover; broker rejects stragglers). Rejected for MVP — adds wire-field surface and broker complexity for an HA scenario that the platform already handles via external leader election. Captured as a potential future extension if real concurrent-writer pain emerges.
- **Stateless LRU** (post-MVP): an opt-in recent-`event.id` cache at registration time (`{ mode: "stateless", dedup_window: "PT5M" }`) is captured as a future addition. Ships as a new mode value or a new registration field; no event-schema change.
- **Mode evolution**: new modes after MVP (e.g., `monotonic-strict` that rejects gaps) are added as new registration values. The mode-shape table grows; existing producers continue to work unchanged.
- **Reset audit log destination**: the `:reset` audit record's storage location is a platform concern (per existing audit infrastructure), not specified here.
- **`evbk_producer` row shape**: minimum fields are `producer_id` (PK, broker-minted UUID), `owner_principal`, `mode`, `client_agent`, `created_at`, `last_seen_at`. `client_agent` is required, persisted, and immutable. Implementation may add observability columns; not constrained by this ADR.

External references:

- Apache Kafka — idempotent producer protocol (producer-id + sequence + epoch model): <https://kafka.apache.org/documentation/#producerconfigs_enable.idempotence>
- RFC 2119 / RFC 8174 — keyword definitions (MUST, SHOULD, MAY)
- RFC 9457 — Problem Details for HTTP APIs
- W3C Trace Context (`traceparent`) — referenced by [ADR-0003 § Field-Level Changes](0003-event-schema.md#field-level-changes)

## Traceability

- **PRD**: [PRD.md](../PRD.md)
  - `cpt-cf-evbk-fr-producer-modes` — three producer modes; chained / monotonic dedup; principal binding
  - `cpt-cf-evbk-fr-publish-single` — single-event publish carries `meta` per this ADR's wire shapes
  - `cpt-cf-evbk-fr-publish-batch` - batch publish carries per-event `meta`; validated per `(producer_id, topic)` run (see [Batch Publish](#batch-publish) and [ADR-0003](0003-event-schema.md))
- **DESIGN**: [DESIGN.md](../DESIGN.md)
  - §3.2 Producer Modes — shrunk to summary + link to `docs/features/0001-idempotent-producers.md`
  - §3.6 Two Sequences — producer chain in `meta` (per this ADR); server-assigned `sequence` (per [ADR-0003](0003-event-schema.md))
  - §3.7 Database schemas — `evbk_producer` row shape (this ADR); `evbk_producer_state` row shape (existing); both governed by [ADR-0003](0003-event-schema.md) field-level changes
- **Related ADRs**:
  - [`0002-partition-selection`](0002-partition-selection.md) - partition key contract; the chain does not depend on the partition
  - [`0003-event-schema`](0003-event-schema.md) — canonical event shape; `meta` block placement (`writeOnly`); `tenant_id` flips to producer-supplied; `subject_type` stays; ASCII encoding rule
- **Feature doc**: [`docs/features/0001-idempotent-producers.md`](../features/0001-idempotent-producers.md) — CDSL flows, mode-choice producer-author guidance, acceptance criteria, test plan
