# Technical Design — Settings Activation

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Goals / Non-Goals](#2-goals--non-goals)
  - [2.1 Goals](#21-goals)
  - [2.2 Non-Goals](#22-non-goals)
- [3. Principles & Constraints](#3-principles--constraints)
  - [3.1 Design Principles](#31-design-principles)
  - [3.2 Constraints](#32-constraints)
- [4. Technical Architecture](#4-technical-architecture)
  - [4.1 Domain Model](#41-domain-model)
  - [4.2 Component Model](#42-component-model)
  - [4.3 API Contracts](#43-api-contracts)
  - [4.4 External Interfaces & Protocols](#44-external-interfaces--protocols)
  - [4.5 Service-to-Service Pattern](#45-service-to-service-pattern)
  - [4.6 Interactions & Sequences](#46-interactions--sequences)
  - [4.7 Database schemas & tables](#47-database-schemas--tables)
  - [4.8 Security & Authorization](#48-security--authorization)
  - [4.9 Technology Stack](#49-technology-stack)
- [5. Risks / Trade-offs](#5-risks--trade-offs)
  - [5.1 Architectural Trade-offs](#51-architectural-trade-offs)
  - [5.2 Security and Performance Risks](#52-security-and-performance-risks)
- [6. Open Questions](#6-open-questions)
  - [6.1 From PRD (Cross-Reference)](#61-from-prd-cross-reference)
  - [6.2 Design-Specific Questions](#62-design-specific-questions)
- [7. Additional context](#7-additional-context)
  - [Feature Metrics](#feature-metrics)
  - [NFR Mapping](#nfr-mapping)
  - [Testing Architecture](#testing-architecture)
- [8. Traceability](#8-traceability)

<!-- /toc -->

<!--
=============================================================================
TECHNICAL DESIGN DOCUMENT
=============================================================================
PURPOSE: Define HOW the system is built — architecture, components, APIs,
data models, and technical decisions that realize the requirements.

DESIGN IS PRIMARY: DESIGN defines the "what" (architecture and behavior).
ADRs record the "why" (rationale and trade-offs) for selected design
decisions; ADRs are not a parallel spec, it's a traceability artifact.

SCOPE:
  ✓ Architecture overview and vision
  ✓ Design principles and constraints
  ✓ Component model and interactions
  ✓ API contracts and interfaces
  ✓ Data models and database schemas
  ✓ Technology stack choices

NOT IN THIS DOCUMENT (see other templates):
  ✗ Requirements → PRD.md
  ✗ Storage, resolution, staging and apply commit → DESIGN.md
  ✗ Detailed rationale for decisions → ADR/

STANDARDS ALIGNMENT:
  - IEEE 1016-2009 (Software Design Description)
  - IEEE 42010 (Architecture Description — viewpoints, views, concerns)
  - ISO/IEC 15288 / 12207 (Architecture & Design Definition processes)

DESIGN LANGUAGE:
  - Be specific and clear; no fluff, bloat, or emoji
  - Reference PRD requirements using `cpt-cf-settings-service-fr-{slug}`,
    `cpt-cf-settings-service-nfr-{slug}` IDs
  - Cross-document references name the target file, e.g. "DESIGN.md §4.2"
=============================================================================
-->

- [ ] `p1` - **ID**: `cpt-cf-settings-service-design-settings-activation`

## 1. Architecture Overview

### 1.1 Architectural Vision

Design **Settings Activation** — the part of the **Settings Service** that provides **guaranteed, acknowledged delivery** of settings-change notifications. It tells a settings consumer **which settings changed** in an apply, so the consumer can re-read the new values and apply them its own way, and it **tracks each consumer's acknowledgement per await-record (one per apply × subscription)** until the change is confirmed activated. It builds on the [Settings Service](./DESIGN.md) value store, which serves values **on read** (pull): that path assumes a consumer reads a value **when it needs it**. This design adds the **push** signal with **back-response tracking**, so a consumer that has already materialized values at startup (connection pools, listener sockets, rendered config files) learns exactly which changed and can confirm successful activation back to the service. Settings Activation is **not a separate component** — the Settings Service owns it.

The service also keeps its own **cache coherence** from the same apply: replicas evict cached effective values. This is a **separate, internal broadcast** (§4.2 *Cache Invalidation Broadcast*) — not the consumer signal — so the two concerns (consumer activation vs. replica invalidation) do not leak into each other.

#### Two distinct distributions

An apply drives **two independent kinds of distribution**, with deliberately different delivery and trust models:

| | **Consumer activation** (§4.2 *Apply Publisher*, §4.2 *Consumer Activation SDK*) | **Replica cache invalidation** (§4.2 *Cache Invalidation Broadcast*) |
|---|---|---|
| **Who receives** | Services that **subscribed** to the specific settings — only they know which settings need active re-application beyond a plain pull | **Every** Settings Service replica (the service's own instances) |
| **Subscription** | Per-setting, opt-in (§4.2 *Subscription Manager*) | **None** — always sent, always processed |
| **Payload** | Filtered to **only the subscriber's own subscribed keys** | The full changed-key set (trusted, internal) |
| **Acknowledged** | Yes — per await-record (apply × subscription), wait-for-all (§4.2 *Apply Outcome Tracker*) | No — fire-and-forget eviction |
| **Event** | `settings.apply_notification` (per subscriber) | `settings.cache_invalidate` (broadcast) |

Splitting them is **least-privilege / blast-radius reduction** (§4.8): a consumer receives only the keys it subscribed to, not the full "everything that changed, in every tenant" set — which stays inside the trusted replica set. Under the **trusted-caller** model (DESIGN.md §6) the subscriber identity is taken on trust, so this is a best-effort default, **not** an identity-enforced access control; revisit if consumers are ever untrusted.

#### Why a distinct design

Reading a value and being told a value changed are different concerns with different owners, so they are documented separately — even though both live inside the Settings Service:

- The **Settings Service** owns the value and answers reads (pull) — [DESIGN-settings-service](./DESIGN.md).
- The **owning consumer** owns *how* to react to a change — re-read a variable, rebuild a pool, drain and rebind, restart itself. That knowledge lives in the consumer, not in a central orchestrator, and a static per-setting effect enum cannot express it.

This design owns the **signal** and the **reaction contract**. It does not own value storage, resolution, or validation.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-settings-service-fr-apply-effect-resolution` | Two-signal split: a per-subscriber filtered `apply_notification` for consumer activation and an unfiltered `cache_invalidate` broadcast for replica cache coherence; consumers re-read and self-react |
| `cpt-cf-settings-service-nfr-efficiency-live-read` | Activation never reloads or restarts a consumer; a heavier reaction is the consumer's own, performed in its handler on the signal |
| `cpt-cf-settings-service-nfr-reliability-fail-safe-staged` | Durable `apply_await_records` are the delivery queue — deliver-until-ack, with wait-for-all outcome resolution and no deadline |
| `cpt-cf-settings-service-nfr-performance-read-cache` | `cache_invalidate` is published inline and best-effort; a missed broadcast self-heals inside `cache_ttl_seconds`, so coherence never depends on durable delivery |
| `cpt-cf-settings-service-nfr-ops-apply-monitoring` | Per-administrator failure surfaces through the back-response and `event_apply_failed`; the aggregate operator signal stays with DESIGN.md |

#### NFR Allocation

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|--------|-------------|--------------|-----------------|----------------------|
| `cpt-cf-settings-service-nfr-reliability-fail-safe-staged` | No consumer activation is lost | Apply Publisher + await-records | Deliver-until-ack from durable records; idempotent per `(apply_id, subscriber, key)`; terminal states immutable | Integration redelivery and restart tests |
| `cpt-cf-settings-service-nfr-performance-read-cache` | Replica staleness is bounded | Cache Invalidation Broadcast | Inline best-effort broadcast plus the `cache_ttl_seconds` backstop owned by the cache components in DESIGN.md | Timed multi-replica convergence test |
| `cpt-cf-settings-service-nfr-security-baseline` | No secret leaves in the signal stream | Event payloads | Notification events carry identifiers only; back-responses carry a hash for secret-valued settings | Subscribed-observer test asserting no plaintext |

### 1.3 Architecture Layers

```
┌─────────────────────────────────────────────────────────────┐
│   Consumers (gears subscribing to the settings they read)   │
├─────────────────────────────────────────────────────────────┤
│  Consumer Activation SDK │ subscribe · handler · ack        │
├─────────────────────────────────────────────────────────────┤
│  Settings Activation (inside the settings-service gear)     │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ Apply Publisher · Subscription Manager ·             │   │
│  │ Apply Outcome Tracker · Cache Invalidation Broadcast │   │
│  └──────────────────────────────────────────────────────┘   │
├─────────────────────────────────────────────────────────────┤
│  Transport │ Event Broker (at-least-once, repeat-safe)      │
├─────────────────────────────────────────────────────────────┤
│  Storage   │ PostgreSQL (await-records, subscriptions)      │
└─────────────────────────────────────────────────────────────┘
```

| Layer | Responsibility | Technology |
|-------|---------------|------------|
| Consumer SDK | Subscribe to exact setting keys, receive the change signal, report the applied outcome | Rust trait in `settings-service-sdk` |
| Activation | Publish the two signals, track one await-record per `(apply, subscriber, key)`, resolve bundle outcome | In-process Rust modules in the `settings-service` gear |
| Transport | At-least-once event delivery; a repeated notification is re-applied and re-acknowledged | Event Broker |
| Storage | Durable await-records and the subscription registry | PostgreSQL via `toolkit-db` |

#### Context View

```mermaid
C4Context
 title Settings Activation (part of the Settings Service) - Context
 Enterprise_Boundary(b0, "the platform OSS") {
 System(settings, "Settings Service", "Stores settings and serves effective values on read (pull). Settings Activation is part of this service: it publishes per-subscriber apply_notification, tracks back-responses per await-record, and broadcasts cache_invalidate to its own replicas")
 System(consumer, "Settings Consumer Gear", "Subscribes to specific settings; reacts to change signals; emits apply_success/apply_failed per setting")
 System_Ext(broker, "Platform Event Broker", "Durable pub/sub transport for notifications, cache-invalidation broadcast, and responses")
 }
 Rel(settings, broker, "publish apply_notification (per subscriber, filtered); publish cache_invalidate (broadcast); receive per-setting back-responses")
 Rel(consumer, broker, "receive apply_notification (only own keys); publish apply_success/apply_failed")
 Rel(consumer, settings, "read effective values for changed keys (pull); re-subscribe after restart")
 Rel(settings, broker, "replicas consume cache_invalidate and evict cached values (cache coherence)")
```

#### Container View

```mermaid
C4Container
 title Settings Activation (within the Settings Service) - Containers
 System_Boundary(s, "Settings Service") {
 Container(pub, "Apply Publisher", "in Settings Service", "On apply commit: co-commit one await-record per (apply, subscription) with its expected value; publish cache_invalidate broadcast (best-effort)")
 Container(broker_client, "Event Broker Client", "Transport", "Durable pub/sub via Event Broker (at-least-once); delivers apply_notification from awaiting records until acked; broadcasts cache_invalidate (best-effort); receives back-response events")
 Container(sub, "Subscription Manager", "service", "Per-setting subscriptions registered explicitly by consumers; triggers re-publish on re-subscribe")
 Container(tracker, "Apply Outcome Tracker", "service", "Consumes back-response events; verifies applied value/hash; per-record outcome; computes the bundle outcome on demand (wait-for-all); serves read-only bundle status")
 Container(cache, "Cache Invalidation Broadcast", "service", "Every replica consumes cache_invalidate unconditionally and evicts cached effective values")
 ContainerDb(db, "Notification State", "PostgreSQL", "subscriptions, per-record await-records (delivery queue + tracking)")
 }
 System_Boundary(cg, "Settings Consumer Gear") {
 Container(sdk, "Consumer Activation SDK", "Rust SDK (ClientHub)", "subscribe(keys); on filtered apply_notification re-read + invoke handler; report_outcome per setting")
 }
 System_Ext(broker, "Platform Event Broker", "Durable pub/sub")
 Rel(pub, db, "co-commit await-records")
 Rel(pub, broker_client, "enqueue apply_notification (per subscriber) + cache_invalidate")
 Rel(broker_client, broker, "publish / subscribe")
 Rel(broker, sdk, "deliver filtered notification (only subscriber's keys)")
 Rel(broker, cache, "deliver cache_invalidate (broadcast to every replica)")
 Rel(sdk, broker, "publish apply_success/apply_failed (per setting)")
 Rel(broker, tracker, "deliver back-response events")
 Rel(tracker, db, "resolve await-records (status + response)")
 Rel(sub, db, "persist subscriptions")
```

## 2. Goals / Non-Goals

### 2.1 Goals

- **Consumer notification per apply (filtered)** — `apply_notification { apply_id, tenant, changed_keys: [key] }`, delivered **per subscriber**, carrying **only the changed keys that subscriber is subscribed to** (never the full apply). One message per apply per subscriber, so a consumer batch-reacts without re-subscribing or polling. `tenant` absent ⇒ platform-wide. Keys are the settings' GTS **instance** ids `<value-type>~<instance-id>` — referenceable by construction; **only the value type (left half) is registered**, the setting itself is not (DESIGN.md §4.6). No `change_kind` — consumers re-read anyway.
- **Replica cache invalidation (broadcast)** — `cache_invalidate { apply_id, tenant, changed_keys: [key] }`, published once per apply to **all** Settings Service replicas (no subscription, no ack), carrying the full changed-key set so every replica evicts its cached `(key, tenant)` entries (§4.2 *Cache Invalidation Broadcast*).
- **Immutable change bundle** — the set of settings changed by a single apply is written to the DB immediately (effective on read) and then reconciled as one unit. The bundle's expected values are **fixed at apply time**; to change a value the administrator applies again, producing a **new** apply/bundle.
- **Per-setting subscription** — a consumer subscribes, in its own name, to the **specific setting keys** it must actively activate (not merely pull). Subscription implies **acknowledged delivery** for those keys (§4.2 *Subscription Manager*).
- **Back-response contract** — consumers emit `apply_success` (or `apply_failed { detail }`) **per changed setting** after reacting, echoing the `tenant` and **the value they applied (a hash for secret-valued settings)**, so the Settings Service tracks activation **per await-record** and verifies the applied value against the expected value **snapshotted at apply time** (§4.2 *Apply Publisher*). A `success` back-response carrying a value that does **not** match is treated as a **failure**.
- **Settings Service apply-outcome visibility** — the Settings Service tracks and exposes via API the state of each apply: a **wait-for-all** overall status `awaiting` → `success` / `failed` / `superseded` / `cancelled`, plus **succeeded / failed / superseded / cancelled / awaiting counts** over the await-records.
- **Consumer re-read-and-react** obligation: on an `apply_notification` the consumer re-reads the affected keys and applies them. A restart-only consumer is handled by **re-publish on re-subscribe** (§4.2 *Apply Outcome Tracker*) — no missed activation is stranded.
- **Event Broker transport only** — durable pub/sub via the platform event broker. At-least-once delivery; the consumer's reaction is idempotent, so a repeated notification is re-applied and re-acknowledged.

### 2.2 Non-Goals

- **Setting storage, effective-value resolution, validation, staging, and the apply commit** — owned by the [Settings Service](./DESIGN.md) value path. Settings Activation is triggered *by* that apply, and does not duplicate it.
- **Carrying the value in a notification** — neither **notification** event (`apply_notification` / `cache_invalidate`) contains the value or a secret (§4.1); consumers re-read. (Back-responses **do** echo the applied value — a hash for secrets — §4.1/§4.4.)
- **Exactly-once delivery** — the model tolerates at-least-once (Event Broker durability): on every notification the consumer re-reads the effective value and converges to it, so a repeat is **re-applied and re-acknowledged**, never suppressed (§4.2 *Consumer Activation SDK*). No global ordering guarantee across applies.
- **Central execution of heavier reactions — not in the model.** Activation never centrally reloads/restarts a consumer, nor classifies a per-setting "effect." Heavier reactions (rebuild a pool, re-render a config file, restart) are the consumer's **own**, done in its handler on the signal — a consumer that cannot apply in place restarts itself (exit → supervisor restarts it → reads the current value on boot, §4.6). See §4.2 *Heavier consumer reactions*. (Coordinated **rolling** restart across replicas is a deployment/rollout concern — RMS — not activation.)
- **A response deadline / time-boxed wait** — the Settings Service does **not** impose a deadline on a bundle; it waits **unboundedly** for every await-record to resolve (how long to keep waiting is an administrator decision, §6). A restart-only consumer leaves its await-records **unanswered**; on **re-subscribe after boot** the service **re-publishes** the notification and the consumer acknowledges then (§4.2 *Apply Outcome Tracker*).
- **Namespace/prefix subscriptions** — not supported. Subscription (and the consumer-facing `watch`, DESIGN.md §4.5) is per **exact** setting key (§4.2 *Subscription Manager*), never a namespace prefix or category.

## 3. Principles & Constraints

### 3.1 Design Principles

#### Notify and React, Never Centrally Execute

- [ ] `p1` - **ID**: `cpt-cf-settings-service-principle-notify-not-execute`

Activation signals *which* keys changed; it never performs a consumer's reaction. A consumer that must rebuild a pool, re-render a config file, or restart does so itself in its own handler. There is no central per-setting effect and no orchestrator.

#### Two Signals, Two Audiences

- [ ] `p1` - **ID**: `cpt-cf-settings-service-principle-two-signal-split`

Consumer activation and replica cache coherence are separate concerns with different delivery requirements, so they are separate events: `apply_notification` is per-subscriber, filtered, and acknowledged; `cache_invalidate` is an unfiltered broadcast to the service's own trusted replicas with no subscription and no ack.

#### Identifiers in Notifications, Values Only in Back-Responses

- [ ] `p1` - **ID**: `cpt-cf-settings-service-principle-identifiers-only`

Neither notification event carries a settings value or a secret — consumers re-read under their own identity. The back-responses are the deliberate exception: they echo the value the consumer applied, hashed when the setting is secret-valued, so the service can verify what was activated.

#### Verify Against the Snapshot, Not a Recomputation

- [ ] `p1` - **ID**: `cpt-cf-settings-service-principle-snapshot-verification`

An acknowledgement is compared against the effective value snapshotted for that await-record at apply time, never against a value recomputed at receive. A later apply must not make an earlier acknowledgement look correct.

#### The Wait Is Unbounded and Admin-Driven

- [ ] `p1` - **ID**: `cpt-cf-settings-service-principle-unbounded-wait`

A bundle stays open until every await-record is terminal. There is no automatic timeout: the service cannot distinguish a slow consumer from a gone one, so it does not guess. A clean retire resolves its records as `cancelled`; anything else is an operator decision.

### 3.2 Constraints

| Constraint/Assumption | Description |
|----------------------|-------------|
| Delivered as part of the Settings Service Gear | Settings Activation is a mechanism **inside** the Settings Service; the publisher and state tables ship in the Settings Service Cyber Fabric Gear (ToolKit runtime); consumers reach the subscription contract through the settings SDK registered in `ClientHub`. |
| Event Broker transport only | Durable pub/sub via the platform event broker. No broker-less fallback — the broker is a platform dependency. |
| Two events per apply | Each apply emits (1) a `cache_invalidate` broadcast to all replicas (published inline at apply, best-effort, cache-TTL backstop) and (2) one filtered `apply_notification` per subscriber (delivered from the durable await-records until acked). Both come from the same settled apply (§4.2 *Apply Publisher*). |
| Consumer notification is filtered | An `apply_notification` carries **only the subscriber's own subscribed changed keys** — never the full apply. This is **key-scoped, best-effort least-privilege** (§4.8), **not** a cross-tenant isolation guarantee: filtering has no tenant dimension, so a subscriber to key K is notified of K's change in **any** tenant (the notification carries which). |
| Tenant in payload | Both events include the `tenant` the change applies to (absent ⇒ platform-wide), so consumers can correctly resolve tenant lineage if they care about cascading, and replicas evict the right scope. |
| No settings value in a notification payload | The two **notification** events (`apply_notification`, `cache_invalidate`) carry **identifiers only** (`changed_keys`, `tenant`) — no settings value, no secret; the tenant-scoped effective value resolves correctly on re-read. The **back-responses** (`apply_success` / `apply_failed`) are the deliberate exception: they carry the **applied value** — plaintext for a non-secret setting, a **hash** for a secret value (the secret plaintext never enters the stream); see §4.8, §4.1/§4.4. |
| Consumers read effective values on demand, respond with outcomes | The pull path (`SettingsReaderClient`, DESIGN.md §4.5) is the source of truth; this system signals *which* of a subscriber's keys changed and tracks *what* happened via back-response (`apply_success` / `apply_failed`, per setting, carrying the applied value or a secret hash). |
| Wait-for-all, unbounded (restart scenario) | Reconciliation waits until **every** await-record resolves; there is **no TTL**. A restart-only consumer does not acknowledge before rebooting; its await-records stay **awaiting**. On **re-subscribe after boot**, the Settings Service **re-publishes** the notification for the unanswered await-records and the consumer acknowledges then (§4.2 *Apply Outcome Tracker*, §6). |

## 4. Technical Architecture

### 4.1 Domain Model

#### Entity: `ApplyNotification` (per-subscriber consumer event body)

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `apply_id` | UUID | Yes | The apply operation (Settings Service `ApplyOperation`) that produced the changes — correlates the signal to its cause and to response reports. |
| `tenant` | string | No | The tenant the change applies to (`/tenants/{id}`). **Absent ⇒ the change is platform-wide.** The consumer re-reads and applies for this tenant; it is responsible for resolving affected descendant tenants if it cares about cascading. |
| `changed_keys` | `[key]` | Yes | **Only the changed setting keys that the receiving subscriber is subscribed to** (GTS instance ids; no operation type) — never the full apply. Delivered per subscriber, so each consumer sees a bundle scoped to its own subscriptions. Consumer already re-reads; operation type is not needed. |

**Invariant:** the payload never contains values or secrets, and never keys the subscriber did not subscribe to. A consumer that needs values re-reads them via `SettingsReaderClient` (DESIGN.md §4.5), which resolves effective values for the given `tenant`.

#### Entity: `CacheInvalidation` (internal broadcast event body)

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `apply_id` | UUID | Yes | The apply that changed the values. |
| `tenant` | string | No | The tenant of the change (absent ⇒ platform-wide). Replicas evict this scope and, for `cascading` keys, affected descendant tenants. |
| `changed_keys` | `[key]` | Yes | The **full** set of changed keys for the apply. Sent only to the trusted Settings Service replica set (§4.2 *Cache Invalidation Broadcast*) — never to external consumers. |

**Invariant:** carries identifiers only (no values/secrets). Consumed unconditionally by every replica; there is no subscription and no acknowledgement.

#### Entity: `BackResponse` (consumer-emitted responses)

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `apply_id` | UUID | Yes | The apply this response answers. |
| `subscriber` | string | Yes | Consumer identity (gear/module namespace) that reacted. |
| `key` | string | Yes | The changed setting (GTS instance id) this response answers. Together with `subscriber` and `apply_id` it identifies the **await-record**; a consumer emits one response per changed setting it is subscribed to. |
| `tenant` | string | No | The tenant the consumer applied the change for (echoed from the notification; absent ⇒ platform-wide). Lets the system verify against the tenant-scoped effective value. |
| `status` | `ResponseStatus` | Yes | `success` / `failed`. |
| `applied_value` | string | No | The value the consumer applied for `key` at `tenant` — the **plaintext value** for non-secret settings, or a **hash** for secret-valued settings (plaintext never leaves the consumer). The Settings Service verifies it against the expected value **snapshotted at apply time** for the await-record (§4.2 *Apply Publisher*; comparing hashes for secrets); a value that does **not** match makes the record **failed** even when `status = success`. |
| `detail` | string | No | Failure detail when `status = failed` (e.g., which specific reason the setting failed to apply). |
| `received_at` | `timestamptz` | Yes | UTC timestamp when response was received (allows delayed responses post-restart; the wait is unbounded). |

**Verification is tenant-scoped:** the system verifies an acknowledgement against the expected value **snapshotted for that await-record at apply time** (§4.2 *Apply Publisher*) — the tenant-scoped effective value captured for the acknowledged `tenant`, not a single global value, and **not** a value recomputed at receive. For secret-valued settings the consumer returns a **hash** of the applied value rather than the plaintext, and the system compares hashes. A mismatch (or a `failed` status) resolves the record as **failed**.

#### Entity: `AwaitRecord` (per-(apply, subscription) tracking state — the load-bearing state)

One row per **(apply, subscriber, key)** the apply must hear back on. Its tenant is **not stored here** — it is an attribute of the apply, read via `apply_id` from the per-apply row (`apply_bundle_tracker.tenant`). Created at publish (§4.2 *Apply Publisher*), the record holds the value **snapshot** the back-response is verified against and the per-record terminal outcome. Verification and supersession are scoped to `(subscriber, key, tenant)` — the same key in a different tenant (a different apply's scope) is an independent record. Persisted as `apply_await_records` (§4.7); the `ApplyOutcomeTracker` counts are **computed on demand** from these rows (count by status), never stored, incremented, or decremented.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `apply_id` | UUID | Yes | The apply this record belongs to. `(apply_id, subscriber, key)` is the record's **unique identity** — `tenant` is **not** in the key (it is determined by the apply). Applies are **totally ordered by `apply_id`** (settle-ordered UUIDv7), so "older / newer" within a `(subscriber, key, tenant)` supersession group is a comparison of `apply_id`. |
| `subscriber` | string | Yes | Consumer identity that owes an acknowledgement. |
| `key` | string | Yes | The changed setting (GTS instance id). |
| `expected_value` | string | Yes | The **snapshot fixed at apply time** (§4.2 *Apply Publisher*) — the tenant-scoped effective value, or a **hash** for secret-valued settings. The back-response's applied value is compared against this snapshot, never a recomputed value. |
| `status` | `RecordStatus` | Yes | `awaiting` → `succeeded` / `failed` / `superseded` / `cancelled` (§4.2 *Apply Outcome Tracker*; `cancelled` = the owing consumer retired, §4.2 *Subscription Manager*). Terminal states are **immutable**, so redelivered acks are idempotent no-ops. |
| `answered_at` | `timestamptz` | No | When the record reached a terminal state; `NULL` while `awaiting`. |

#### Entity: `ApplyOutcomeTracker` (Settings Service tracking apply activation state)

**Computed view, not a stored aggregate.** The counts below are **computed on demand** from the `AwaitRecord` rows (count by status) whenever bundle status is read — never stored, incremented, or decremented. This is what keeps them correct under at-least-once redelivery: a duplicate ack cannot double-count, because the numbers are a function of the (idempotent, terminal) record statuses, not a running total of events.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | UUID | Yes | Tracker id (UUIDv7). |
| `apply_id` | UUID | Yes | The apply being tracked. |
| `tenant` | string | No | The tenant of the apply (absent ⇒ platform-wide). |
| `subscribers_at_publish_time` | `[subscriber]` | Yes | List of subscribers to the changed keys (resolved at publish time; captured for auditability). |
| `expected_records` | integer | Yes | Count of **await-records** the apply must hear back on (changed keys × subscribers subscribed to those keys). Computed on demand. |
| `resolved_records` | integer | Yes | Count of await-records in a **terminal** state (`succeeded` / `failed` / `superseded` / `cancelled`) — a `superseded` record (like a `cancelled` one) is resolved without its own response. Computed on demand (see the note above), not stored. |
| `succeeded_records` | integer | Yes | Await-records answered `success` with a value matching their **snapshot** (§4.2 *Apply Publisher*). |
| `failed_records` | integer | Yes | Await-records answered `fail` status **or** `success` with a mismatching value. |
| `superseded_records` | integer | Yes | Await-records whose `(key, tenant)` was overtaken by a **later, confirmed** apply before this one was acknowledged (§4.2 *Apply Outcome Tracker*) — a terminal **non-failure** outcome. |
| `cancelled_records` | integer | Yes | Await-records resolved `cancelled` because the owing consumer **retired** before answering (§4.2 *Subscription Manager*) — a terminal **non-failure** outcome. |
| `overall_status` | `ApplyOutcomeStatus` | Yes | `awaiting` → `success` / `failed` / `superseded` / `cancelled` — **wait-for-all** (see transitions in §4.2 *Apply Outcome Tracker*). |

#### Entity: `Subscription`

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | UUID | Yes | Subscription id (UUIDv7). |
| `subscriber` | string | Yes | The consumer identity that registered this subscription — **any trusted in-platform consumer**, not necessarily the setting's owner/contributor. |
| `key` | string | Yes | The **exact setting key** (GTS instance id) the subscriber watches — one row per setting the consumer must actively activate. Not a namespace/prefix pattern. Registered by the consumer (trusted-caller, §4.2 *Subscription Manager*) for any key it can read — **not** tied to contribution, and stored by this system, **not** on the settings declaration (which stays activation-agnostic). |
| `created_at` | `timestamptz` | Yes | UTC timestamp. |

> A subscription **implies acknowledged delivery** for its key — there is no non-acking subscription. Replica cache invalidation is *not* modelled as a subscription (§4.2 *Cache Invalidation Broadcast*): it is an unconditional broadcast with no registry row.

#### Enums

| Enum | Values |
|------|--------|
| `ResponseStatus` | `success`, `failed` |
| `RecordStatus` | `awaiting`, `succeeded`, `failed`, `superseded`, `cancelled` |
| `ApplyOutcomeStatus` | `awaiting`, `success`, `failed`, `superseded`, `cancelled` |

> **`ApplyOutcomeStatus`** tracks the aggregate activation outcome of an apply as a **wait-for-all** result: it stays `awaiting` until **every** await-record is terminal, then resolves to `failed` (any record failed), else `superseded` (no failure, but ≥1 key overtaken by a later confirmed apply), else `cancelled` (no failure/supersession, but ≥1 record `cancelled` because its consumer retired — §4.2 *Subscription Manager*), else `success` (every record confirmed with a matching value). Partial progress is visible through the per-record **succeeded / failed / superseded / cancelled / awaiting counts**, not as a distinct status. There is **no deadline** — the wait is unbounded (§4.2 *Apply Outcome Tracker*, §6). The Settings Service exposes this state via API (§4.3).

#### Entity Relationships

```mermaid
erDiagram
 APPLY ||--o{ APPLY_BUNDLE_TRACKER : "recorded by"
 APPLY ||--o{ APPLY_AWAIT_RECORD : "awaits"
 SUBSCRIPTION ||--o{ APPLY_AWAIT_RECORD : "owes"
 SUBSCRIPTION {
 uuid id PK
 string subscriber
 string key
 }
 APPLY_BUNDLE_TRACKER {
 uuid id PK
 uuid apply_id "correlation (no DB FK)"
 string tenant
 jsonb subscribers_at_publish_time
 string note "counts + overall_status computed on demand"
 }
 APPLY_AWAIT_RECORD {
 uuid id PK
 uuid apply_id "identity: (apply_id, subscriber, key)"
 string subscriber
 string key
 string expected_value "snapshot at publish (hash for secrets)"
 string status "awaiting|succeeded|failed|superseded|cancelled"
 string applied_value "received response; null until answered"
 string detail
 timestamptz received_at
 timestamptz answered_at
 timestamptz last_notified_at "delivery-loop backoff"
 }
```

### 4.2 Component Model

#### Component: Apply Publisher

- [ ] `p1` - **ID**: `cpt-cf-settings-service-component-apply-publisher`

Runs inside the Settings Service; invoked by the apply commit (DESIGN.md §4.2 *Apply Orchestrator*) after all values are written.

**Dependencies:** Event Broker Client, Notification State (PostgreSQL), Settings Service apply path, Subscription Manager.

**Operations:**

| Operation | Input | Output | Key Behavior |
|-----------|-------|--------|--------------|
| `publish_apply` | `apply_id`, `tenant`, `changed_keys` | — | In a **final bookkeeping transaction once the apply has settled** (the settings apply commits **per change**, so there is no single whole-apply transaction to ride — DESIGN.md §4.2 *Apply Orchestrator*), **co-commit one await-record per (apply, subscription)** (changed keys × their subscribers, resolved via `resolve_subscribers`), each holding the **expected tenant-scoped effective value** — a **snapshot fixed at apply time** that each back-response is later compared against (read back verbatim at receive, §4.2 *Apply Outcome Tracker*, **never recomputed**); `expected_records` is then just the count of those rows (computed on demand). These durable await-records **are** the delivery queue: the delivery loop (§4.2 *Event Broker Client*) publishes a **filtered** `apply_notification` per subscriber from its `awaiting` records and keeps re-publishing until each is acked — **no separate outbox**. Separately, publish a `cache_invalidate` **broadcast** with the full committed changed-key set (§4.2 *Cache Invalidation Broadcast*) **inline, best-effort** — replicas evict; a missed broadcast self-heals within `cache_ttl_seconds` (§7 *NFR Mapping*). Idempotent per `apply_id`. |

**Publish-after-settle & guarantee.** The await-records are the durable record of what must be delivered; they are written in one bookkeeping transaction after the apply settles. A crash **before** that commit leaves nothing recorded — the committed values are still pull-readable and any stale replica cache self-heals within `cache_ttl_seconds`, never silent drift. **After** the commit, delivery is guaranteed: the delivery loop (§4.2 *Event Broker Client*) publishes `apply_notification` for every `awaiting` record and retries **until it is acked** (not merely until the broker accepts a publish), so a notification that is accepted-but-never-handled is re-delivered without a separate re-publish path. `cache_invalidate` is the one best-effort element — published inline, not persisted — and its correctness is bounded by `cache_ttl_seconds` (§7 *NFR Mapping*), so it needs no durable queue. No values are read or transported here.

#### Component: Event Broker Client

- [ ] `p1` - **ID**: `cpt-cf-settings-service-component-event-broker-client`

Transport abstraction for sending and receiving notifications.

**Configuration:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `redeliver_interval_seconds` | integer | `30` | Minimum interval before the delivery loop re-publishes an `apply_notification` for a still-`awaiting` record (backoff via `last_notified_at`), so a slow-to-ack consumer is not re-notified on every scan. **Not** a deadline — the wait is unbounded (§4.2 *Apply Outcome Tracker*). |

> **`cache_ttl_seconds` is not configured here.** The cache TTL backstop is **owned by the cache components** — the replica effective-value cache (DESIGN.md §4.2 *Cache & Invalidation*) and the reader-SDK consumer cache (DESIGN.md §4.5). Activation only **references** it: it is the sole correctness backstop for the best-effort `cache_invalidate` broadcast (§4.2 *Cache Invalidation Broadcast*), so a missed broadcast self-heals within that TTL (§7 *NFR Mapping*).

**Operations:**

| Operation | Input | Output | Key Behavior |
|-----------|-------|--------|--------------|
| `publish_notification` | `subscriber`, `ApplyNotification` | — | Deliver a subscriber's **filtered** notification (only its subscribed changed keys), built from its `awaiting` records. At-least-once; invoked by the delivery loop (below). |
| `publish_cache_invalidation` | `CacheInvalidation` | — | **Broadcast** the full changed-key set to every Settings Service replica (§4.2 *Cache Invalidation Broadcast*). At-least-once; no acknowledgement. |
| `subscribe_notifications` | `keys`, handler | subscription handle | Ensure the subscriber's **durable per-subscriber consumer queue** on the broker and **update the registry** (§4.2 *Subscription Manager*) with its `(subscriber, key)` interests. It does **not** create per-key broker subscriptions — broker addressing is **per-subscriber identity**; the delivery loop filters by the registry and publishes each subscriber's bundle to its queue. |
| `deliver_awaiting` | — | — | The **delivery loop**: publishes a filtered `apply_notification` per subscriber from its `awaiting` records — immediately after the co-commit (prompt) and on a periodic reconcile scan (retry, broker-outage recovery, and post-restart re-delivery). Honours `redeliver_interval_seconds` via `last_notified_at`. Stops for a record once it is terminal (§4.2 *Apply Outcome Tracker*). |
| `publish_response` | `BackResponse` | — | Consumer emits per-setting success/failed response via Event Broker. Settings Service receives and tracks. |

**Broker topology — per-subscriber addressing.** The broker is addressed by **subscriber identity**: each subscriber has **one durable consumer queue**, and the delivery loop publishes its filtered bundle there. There is **no per-key broker subscription** — `subscribe`/`subscribe_notifications` only update the **registry** (§4.2 *Subscription Manager*) recording which keys a subscriber wants; per-key filtering is done by the service from that registry, not by broker routing. (Assumes the platform Event Broker's durable pub/sub supports a durable per-consumer queue, so a notification published while a subscriber is down is delivered on reconnect.)

#### Component: Subscription Manager

- [ ] `p1` - **ID**: `cpt-cf-settings-service-component-subscription-manager`

**Dependencies:** Notification State (PostgreSQL). Subscription is **self-contained**: a consumer registers explicitly (`register_subscription` / SDK `subscribe`). There is **no dependency on the Settings Service Contribution path** and **no auto-subscribe** — contribution and subscription are fully independent.

**Operations:**

| Operation | Input | Output | Key Behavior |
|-----------|-------|--------|--------------|
| `register_subscription` | `subscriber`, `key`, `Context` | `Subscription` | **Any trusted in-platform consumer** registers a per-`key` subscription for **any key it can read** — trusted-caller, the same posture as the reader SDK (DESIGN.md §6); subscription is **decoupled from owning/contributing** the setting, so shared and admin-authored keys are subscribable. Subscription is **always explicit** — there is **no auto-subscribe from contribution**; a subscription's ack obligation (§4.2 *Apply Outcome Tracker*) is only ever created by a deliberate subscribe, never as a side effect of declaring a setting. Idempotent per `(subscriber, key)`. **On re-subscribe** (e.g. after a consumer restart) the subscriber's still-`awaiting` records are re-delivered automatically by the delivery loop (§4.2 *Event Broker Client*) — no explicit re-publish call — so the consumer receives the change again and can acknowledge (§4.2 *Apply Outcome Tracker*). |
| `resolve_subscribers` | `key` | `Subscription[]` | Return subscriptions for the exact changed `key` (exact match, no prefix); used to build each subscriber's filtered `apply_notification` and to know which subscribers owe an ack. |
| `retire_subscription` | `subscriber`, `key` | — | Remove the subscription (Settings Service register/retire lifecycle). **MUST also resolve the subscriber's still-`awaiting` await-records for this `key` to `cancelled`** (a terminal non-failure, §4.1) — retire is a deterministic lifecycle event, so the service knows the consumer will never answer and closes the records instead of leaving bundles `awaiting` forever. Affected bundle outcomes recompute on demand (§4.2 *Apply Outcome Tracker*); the delivery loop stops re-delivering `cancelled` records. |

#### Component: Consumer Activation SDK

- [ ] `p1` - **ID**: `cpt-cf-settings-service-component-consumer-activation-sdk`

The consumer-facing contract. Subscribes to specific setting keys, receives filtered `apply_notification` bundles, re-reads the changed settings, and emits back-responses.

**Operations:**

| Operation | Input | Output | Key Behavior |
|-----------|-------|--------|--------------|
| `subscribe` | `keys`, handler | subscription handle | Subscribe to the **specific setting keys** the consumer must actively activate — **any keys it can read**, not tied to ownership (§4.2 *Subscription Manager*). On each apply, the SDK **receives a bundle of only those keys that changed** (+ the `tenant`) and **re-reads** their effective values (`SettingsReaderClient.get_effective`) for that `tenant`. Invokes the handler with the bundle + fresh values. Handler decides *how* to apply. |
| `report_outcome` | `apply_id`, `tenant`, `key`, `status`, `applied_value`, `detail?` | — | After reacting to a change, emit `BackResponse` (success/failed) **for the given `key`** (and `tenant`), carrying the **value applied** (a **hash** for secret-valued settings), so the Settings Service resolves the await-record and verifies against the expected value **snapshotted at apply time** (§4.2 *Apply Publisher*). Idempotent per `(apply_id, subscriber, key)`. |

**Consumer obligation (normative):** a consumer that materializes settings at startup (connection pool, listener socket, rendered file) and needs *active* re-application MUST `subscribe` to those setting keys, re-read on signal, and emit a per-setting back-response — **on every delivery, including a repeat of one already handled**. Re-reading is what makes the reaction idempotent: a consumer already at the current value reports `success` with that value and does no further work. A repeat MUST NOT be suppressed — suppressing it withholds its acknowledgement and leaves the await-record `awaiting` (§4.2 *Apply Outcome Tracker*). A consumer that reads settings fresh on every use needs no subscription — the pull path already gives it the current value. On boot a consumer reads current values via the pull path, and any activation left pending across a restart is re-published on re-subscribe (§4.2 *Apply Outcome Tracker*).

#### Component: Apply Outcome Tracker

- [ ] `p1` - **ID**: `cpt-cf-settings-service-component-apply-outcome-tracker`

Tracks and exposes bundle state based on consumer responses. Runs in the Settings Service.

**Dependencies:** Notification State (PostgreSQL), Event Broker Client (receives `BackResponse` events). *(No dependency on the Settings Service apply path for the expected value: it is read from the await-record snapshotted at publish (§4.2 *Apply Publisher*), not recomputed here.)*

**Operations:**

| Operation | Input | Output | Key Behavior |
|-----------|-------|--------|--------------|
| `receive_response` | `BackResponse` | — | Consume back-response event from Event Broker. Let `R` = the named await-record `(apply_id, subscriber, key)`, within its `(subscriber, key, tenant)` supersession group. **`status = failed`** → resolve `R` **failed** (a failure supersedes nothing). **`status = success`** → resolve **by value**: find the still-`awaiting` record `M` in that group whose **snapshot** (§4.2 *Apply Publisher*; hash compare for secrets) equals `applied_value` — and when **several** awaiting records hold that same snapshot (a re-apply of an identical value), `M` is the **newest** of them (highest settle-ordered `apply_id`, §4.1). If `M` exists → `M` **succeeded**, every still-`awaiting` record **older than `M`** (lower `apply_id`) → **superseded** (equal-snapshot ones included — a later confirmed apply exists, so they are overtaken, not independently confirmed), records **newer than `M`** stay `awaiting`. If no awaiting record matches but the value equals an already-`succeeded` record's snapshot → **no-op** (idempotent redelivery). Otherwise (a `success` carrying a value the service never committed) → resolve `R` **failed**. Only the record's `status` transitions here; the bundle counts and `overall_status` are **computed on demand** on read (§`bundle_status`; transitions below), never stored. Terminal states are immutable; idempotent per `(apply_id, subscriber, key)`. Late responses are **always** accepted — the wait is unbounded, no deadline. |
| `bundle_status` | `apply_id` | `ApplyOutcomeTracker` | Return the current state of the apply: `overall_status` (`awaiting`/`success`/`failed`/`superseded`/`cancelled`), `expected_records` and `succeeded_records`/`failed_records`/`superseded_records`/`cancelled_records`/awaiting counts, per-subscriber responses, per-setting confirmation. Surfaced through Settings Service API (§4.3). |

**Status transitions (wait-for-all):** let `terminal = succeeded_records + failed_records + superseded_records + cancelled_records`.
- **awaiting** → *while `terminal < expected_records`* — even if some records have already `failed`, been `superseded`, or been `cancelled`, the bundle stays `awaiting` until **every** record is terminal. A single failure does **not** finalize the bundle.
- **failed** → *`terminal == expected_records` AND `failed_records > 0`* — failure dominates the terminal outcome.
- **superseded** → *`terminal == expected_records` AND `failed_records == 0` AND `superseded_records > 0`* — no failure; at least one key was overtaken by a later, confirmed apply.
- **cancelled** → *`terminal == expected_records` AND `failed_records == 0` AND `superseded_records == 0` AND `cancelled_records > 0`* — no failure or supersession; at least one record's consumer retired before answering (§4.2 *Subscription Manager*).
- **success** → *`succeeded_records == expected_records`* — every record confirmed with a matching value.

There is **no deadline-driven transition**: the tracker never moves to a terminal state on a timer. How long to keep waiting for still-`awaiting` records is an administrator decision (§6).

**Supersession requires a confirmed, successful successor (later in time); it is decided at receive, not at publish.** A new apply to an already-awaited `(key, tenant)` does **not** touch existing records at publish (§4.2 *Apply Publisher*) — it only adds its own. An older record is resolved `superseded` **only when a later-in-time apply for the same `(key, tenant)` has actually `succeeded`** (a value-matched ack), never on the mere existence of a later apply. So if the later value fails to activate, no `succeeded` successor exists, nothing is superseded, the older record stays `awaiting` (the newer state was never confirmed) and the failing record is `failed` — supersession can never mask a failed newer apply.

**Back-response lateness (restart scenario):** a restart-only consumer leaves its await-records **awaiting** (no ack before reboot). The delivery loop (§4.2 *Event Broker Client*) keeps re-publishing those records; after the consumer re-subscribes on boot it receives them and acknowledges; value-matching then resolves the latest record `succeeded` and older awaiting records for that key `superseded`. Records stay `awaiting` until then — there is no TTL (§4.1, §6).

#### Heavier consumer reactions (self-react)

Activation is **self-react**: the signal (§4.2 *Apply Publisher*/§4.2 *Consumer Activation SDK*) tells a consumer *what* changed; the consumer decides *how*. A consumer that must do more than re-read a variable — rebuild a connection pool, drain and rebind, **re-render a config file**, or **restart** — performs that reaction **itself in its handler**. A consumer that cannot apply in place simply **restarts**: it exits, its supervisor (k8s / systemd) brings it back at the same replica count, and it reads the current value on boot via pull — this is exactly the restart path in §4.6.

Consequently the Settings Service needs **no central orchestrator and no per-setting "effect" enum** to drive reloads/restarts — consistent with §1 (reaction knowledge lives in the consumer, not a central orchestrator). The only thing self-react does **not** cover is **coordinated rolling restart** across a service's replicas (so they don't all drop at once); that is a **deployment/rollout concern** (RMS / the orchestrator), out of scope for activation. A consumer that cares can also stagger its own restart.

#### Component: Cache Invalidation Broadcast

- [ ] `p1` - **ID**: `cpt-cf-settings-service-component-cache-invalidation-broadcast`

**Dependencies:** Event Broker Client, Settings Service effective-value cache (DESIGN.md §4.2 *Cache & Invalidation* of that design).

Replica cache coherence is **not** a subscription. Every Settings Service replica **unconditionally** consumes the `cache_invalidate` broadcast for every apply and evicts — there is no registry row, no filtering, and no acknowledgement. Because the recipients are the service's own trusted replicas, the broadcast may carry the full changed-key set (unlike the consumer `apply_notification`, which is filtered, §4.8). It is published **inline at apply time, best-effort** (§4.2 *Apply Publisher*) — **not** persisted in a queue; a missed broadcast self-heals within `cache_ttl_seconds` (§7 *NFR Mapping*), so replica coherence never depends on durable delivery.

**Operations:**

| Operation | Input | Output | Key Behavior |
|-----------|-------|--------|--------------|
| `on_cache_invalidate` | `CacheInvalidation` | — | Each replica evicts its cached `(key, tenant)` entries — and, for `cascading` keys, affected descendant tenants — so peers re-resolve on next read. Always sent, always processed; idempotent eviction. |

**Apply-driven invalidation is not the only trigger.** This broadcast covers a **value** change. A cached `cascading` effective value also depends on the tenant's **ancestor chain**, so a hierarchy change — a re-parent, or a tenant inserted mid-chain — invalidates it with **no apply and therefore no broadcast**. That trigger is owned by the settings-service cache (DESIGN.md §4.2 *Cache & Invalidation* of that design), which evicts on a Tenant Resolver hierarchy-change signal; the Tenant Resolver does not publish one today, so `cache_ttl_seconds` bounds the window. Stated here so the broadcast is not mistaken for the complete invalidation story.

**Ordering — the broadcast follows the commit, never precedes it.** The publish is inline at apply but strictly **after** the value is durably committed (DESIGN.md §4.2 *Apply Orchestrator*: commit → local evict → publish), so no replica or consumer can receive an invalidation, or an `apply_notification`, for a value that is not yet stored. Best-effort delivery relaxes *whether* the signal arrives, never *when* it may be sent.

### 4.3 API Contracts

Most of this system is an in-process SDK contract. The **only** REST surface is the **read-only apply activation facet** (§4.3). Subscription and acknowledgement are SDK + Event Broker (§4.5); replica cache invalidation is the `cache_invalidate` broadcast (§4.2 *Cache Invalidation Broadcast*) — neither is REST. `{key}` is the setting's GTS instance id (URL-encoded).

#### REST — Apply Activation Facet (read-only)

| Method | Endpoint | Description | Idempotency |
|--------|----------|-------------|-------------|
| `GET` | `/v1/applies/{apply_id}/activation` | Activation-facet state: `overall_status` (`awaiting`/`success`/`failed`/`superseded`/`cancelled`), `expected_records`, `succeeded_records`/`failed_records`/`superseded_records`/`cancelled_records`/awaiting counts, per-subscriber responses, per-setting confirmation | Yes |
| `GET` | `/v1/applies/{apply_id}/activation/responses` | List all `BackResponse` entries for this apply (who responded, for which key, when, what status) | Yes |

**The `activation` facet of `/v1/applies/{apply_id}`.** This lives at **`/v1/applies/{apply_id}/activation`** — a facet of the apply namespace, keyed by `apply_id` (a **correlation id**, §4.7), **beside** the settings execution facet `GET /v1/applies/{apply_id}/commit` (the apply **execution** record, `value.v1~`, **deleted on settle** — §4.3 / §4.2 *Apply Orchestrator*/DESIGN.md §4.6). Two facets, **opposite lifecycles**: `commit` answers "did the values commit" and self-deletes on settle; `activation` answers "did consumers activate" and **outlives** the apply (unbounded wait, §4.2 *Apply Outcome Tracker*). Both hang off the **neutral `apply_id` namespace** — the activation facet is **not** nested under the delete-on-settle execution record — which is what avoids the orphaned-sub-resource trap (the `commit` facet 404s independently while this `activation` facet lives on). One gear serves both (Settings Activation is part of the Settings Service, §1).

**Pagination.** `GET /v1/applies/{apply_id}/activation/responses` uses **cursor pagination** (`cursor`/`limit` → `{ items, page_info }`, no `total_count`) per the shared REST DNA (DESIGN.md §4.3 / guideline §5) — its row count is **changed-keys × subscribers**, unbounded for a platform-wide apply, so it MUST paginate. The `activation` bundle read itself (`/activation`) is a single resource, not a list.

**These two reads are the entire REST surface.** Everything else is deliberately not REST:
- **Subscribe / acknowledge** — SDK `subscribe` / `report_outcome` (§4.5), carried as Event Broker events (`apply_success` / `apply_failed`). There is **no REST back-response ingest**.
- **Replica cache invalidation** — the `cache_invalidate` broadcast event (§4.2 *Cache Invalidation Broadcast*); no endpoint.
- **Publishing** — the Apply Publisher publishes `cache_invalidate` inline and the delivery loop publishes `apply_notification` from the await-records, both in-process to the broker (§4.2 *Apply Publisher*/§4.2 *Event Broker Client*); no endpoint, hence **no platform service-token surface**.

### 4.4 External Interfaces & Protocols

Canonical definitions; the [Settings Service](./DESIGN.md) publishes and consumes these. CloudEvents envelope.

| Event | Attributes | Notes |
|-------|-----------|-------|
| `settings.apply_notification` | `apply_id`, `tenant`, `changed_keys: [key]` | Published **per subscriber** with **only that subscriber's subscribed changed keys** — never the full apply. `tenant` absent ⇒ platform-wide. **No values, no secrets.** (Re-)delivered from a subscriber's `awaiting` await-records until acked by the delivery loop (§4.2 *Event Broker Client*), which subsumes restart re-publish. Drives consumer re-read + per-setting ack. |
| `settings.cache_invalidate` | `apply_id`, `tenant`, `changed_keys: [key]` | Published **inline at apply, best-effort**, as a broadcast to every Settings Service replica, carrying the **full** changed-key set. **No values, no secrets.** No subscription, no acknowledgement, **no durable queue** — every replica evicts; a miss self-heals within `cache_ttl_seconds` (§4.2 *Cache Invalidation Broadcast*, §7 *NFR Mapping*). Internal/trusted recipients only. |
| `settings.apply_success` / `settings.apply_failed` | `apply_id`, `tenant`, `subscriber`, `key`, `applied_value`, `detail?` | Emitted by consumers **per changed setting** after reacting, carrying the **applied value (hash for secrets)**. Resolves the await-record and drives the `success`/`failed`/`superseded` bundle outcome (wait-for-all); verified against the expected value **snapshotted at apply time** (§4.2 *Apply Publisher*) (a mismatch is a failure even on `apply_success`). |

**Two events, two trust domains:** consumer activation (`apply_notification`) is per-subscriber and filtered; cache coherence (`cache_invalidate`) is a full-set broadcast confined to the service's own replicas. The full change set never reaches an external consumer (§4.8).

### 4.5 Service-to-Service Pattern

#### In-Process SDK — Consumer Activation

| Operation | Signature | Description |
|-----------|-----------|-------------|
| `subscribe` | `subscribe(keys, handler)` | Subscribe to specific setting keys; on each apply, receive a filtered bundle of the changed subscribed keys, re-read, and invoke the handler. |
| `report_outcome` | `report_outcome(apply_id, tenant, key, status, applied_value, detail?)` | Emit a per-setting `BackResponse` after reacting (for the given `key`/tenant), carrying the applied value (hash for secrets). |

### 4.6 Interactions & Sequences

The two sequences below are the authoritative activation protocol, expressed in this design's terms (per-subscriber filtered `apply_notification`, broadcast `cache_invalidate`, per-await-record tracking, wait-for-all outcome). All service-side actions originate from the **Settings Service** — Settings Activation is part of it, not a separate component.

#### Main interaction protocol

```mermaid
sequenceDiagram
 autonumber
 actor Admin
 participant SS as Settings Service
 participant DB as Settings DB
 participant Broker as Event Broker
 participant R as Service Replicas
 participant C as Subscribed Consumer

 Note over C,SS: (prerequisite) consumer subscribed to specific setting keys

 Admin->>SS: Apply(change bundle)
 SS->>DB: commit changes per-change (effective on next read), then co-commit await-records (bookkeeping tx after settle)
 DB-->>SS: committed
 Note over SS: enter reconciliation(apply_id, tenant?)
 loop per await-record (apply × subscription)
 SS->>SS: create await-record (expected tenant-scoped value snapshot)
 end
 SS->>Broker: publish cache_invalidate (inline, best-effort) — apply_id, tenant, ALL changed_keys
 Broker->>R: deliver cache_invalidate (every replica, unconditional)
 R->>R: evict cached (key, tenant) entries
 SS->>Broker: delivery loop publishes apply_notification per subscriber from awaiting records (retries until acked)
 Broker->>C: deliver apply_notification (filtered bundle)

 C->>SS: re-read effective value(s) for changed keys @ tenant
 SS-->>C: current value(s)
 Note over C: apply locally

 C->>SS: report_outcome per setting — apply_success/apply_failed(apply_id, tenant, key, applied value/hash)
 SS->>SS: match applied value to a still-awaiting record's snapshot @ (subscriber, key, tenant)
 SS->>SS: matched record → SUCCEEDED; older awaiting records → SUPERSEDED<br/>failed status → that record FAILED (supersedes nothing)

 Admin->>SS: GET /v1/applies/{apply_id}/activation
 SS-->>Admin: overall_status + succeeded / failed / superseded / cancelled / awaiting record counts

 Note over SS: AWAITING until every record is terminal (late acks waited for, unbounded).<br/>Then FAILED if any failed; else SUPERSEDED if any superseded; else CANCELLED if any cancelled; else SUCCESS.
```

#### Apply via consumer restart

```mermaid
sequenceDiagram
 autonumber
 participant SS as Settings Service
 participant Broker as Event Broker
 participant C as Restart-Only Consumer

 Broker->>C: deliver apply_notification (its subscribed changed_keys + tenant)
 Note over C: cannot apply in place — needs a restart
 Note over C,SS: consumer sends NO back-response → await-record stays UNANSWERED
 Note over C: consumer restarts (reads current values via pull on boot)

 C->>SS: re-subscribe for those setting keys (after boot)
 SS->>SS: delivery loop: this consumer's still-awaiting records remain queued
 SS->>Broker: (re-)publish apply_notification from awaiting records (latest apply per key)
 Broker->>C: deliver apply_notification
 C->>SS: apply_success(apply_id, tenant, key, applied value/hash)
 SS->>SS: value-match → latest record SUCCEEDED; older awaiting records SUPERSEDED
```

### 4.7 Database schemas & tables

**Apply linkage is a correlation id, not a DB foreign key.** Every `apply_id` below references the Settings Service `ApplyOperation` **by value only** — there is **no enforced FK**. The settings apply row is **deleted on settle** (delete-on-settle, §4.2 *Apply Orchestrator* / §4.6) while activation tracking can outlive it (the reconciliation wait is unbounded, §4.2 *Apply Outcome Tracker*); activation therefore owns its own row lifecycle and treats `apply_id` as a bare correlation key.

#### Table: `subscriptions`

| Column | Type | Nullable | Default | Constraints |
|--------|------|----------|---------|-------------|
| `id` | UUID | No | auto | **PK** |
| `subscriber` | text | No | — | |
| `key` | text | No | — | Exact setting key (GTS instance id); one row per subscribed setting |

**Indexes:** unique `uq_sub_subscriber_key` on `(subscriber, key)`; `idx_sub_key` on `key` (fan-out lookup by changed key).

#### Table: `apply_bundle_tracker`

Per-apply metadata only. **No stored counters and no stored `overall_status`**: the counts (`expected_records` / `resolved_records` / `succeeded_records` / `failed_records` / `superseded_records` / `cancelled_records`) and `overall_status` are **computed on demand** from `apply_await_records` (§4.1, §4.2 *Apply Outcome Tracker*), so there is no per-event increment to corrupt.

| Column | Type | Nullable | Default | Constraints |
|--------|------|----------|---------|-------------|
| `id` | UUID | No | auto | **PK** |
| `apply_id` | UUID | No | — | Correlation id → settings apply (no DB FK) |
| `tenant` | text | Yes | — | Null ⇒ platform-wide |
| `subscribers_at_publish_time` | JSONB | No | — | Snapshot of the subscribers resolved at publish (audit) |
| `created_at` | timestamptz | No | now | |

**Indexes:** unique `uq_tracker_apply` on `apply_id`.

#### Table: `apply_await_records`

The load-bearing per-`(apply_id, subscriber, key)` state. Created at publish for every await-record the apply must hear back on; it holds the value **snapshot** and the terminal outcome, and absorbs the received back-response. It is **also the delivery queue**: rows with `status = 'awaiting'` are exactly what the delivery loop (§4.2 *Event Broker Client*) publishes and re-publishes until acked — there is no separate outbox. `GET /v1/applies/{apply_id}/activation/responses` reads the rows with `received_at IS NOT NULL`.

| Column | Type | Nullable | Default | Constraints |
|--------|------|----------|---------|-------------|
| `id` | UUID | No | auto | **PK** |
| `apply_id` | UUID | No | — | Correlation id → settings apply (no DB FK) |
| `subscriber` | text | No | — | Owing consumer identity |
| `key` | text | No | — | Changed setting (GTS instance id) |
| `expected_value` | text | No | — | Snapshot of the tenant-scoped effective value at publish (a **hash** for secret-valued settings); the applied value is compared against this (§4.2 *Apply Publisher*/§4.2 *Apply Outcome Tracker*) |
| `status` | text | No | `'awaiting'` | Check: `awaiting`, `succeeded`, `failed`, `superseded`, `cancelled` |
| `applied_value` | text | Yes | — | The received back-response value (plaintext for non-secret, hash for secret); `NULL` until a response arrives (and for `superseded`, which resolves without one) |
| `detail` | text | Yes | — | Failure detail when the response is `failed` |
| `received_at` | timestamptz | Yes | — | When the back-response was received; `NULL` while `awaiting` or when `superseded` without a response |
| `answered_at` | timestamptz | Yes | — | When the record reached a terminal state; `NULL` while `awaiting` |
| `last_notified_at` | timestamptz | Yes | — | When the delivery loop last published `apply_notification` for this record; drives the `redeliver_interval_seconds` backoff (§4.2 *Event Broker Client*). `NULL` until first delivery |
| `created_at` | timestamptz | No | now | |

**Indexes:** unique `uq_await_record` on **`(apply_id, subscriber, key)`** — `tenant` is **not a column here** (it lives on the per-apply row), so there is no nullable column in the unique key and PostgreSQL's NULL-distinct duplicate-row trap cannot occur; `idx_await_apply` on `apply_id`; partial `idx_await_awaiting` on `(subscriber, key)` `WHERE status = 'awaiting'` (supersession / value-match lookup **and the delivery-loop scan**; `tenant` is applied by joining `apply_bundle_tracker` on `apply_id`).

### 4.8 Security & Authorization

#### Authorization model

Mirrors the Settings Service model (design DESIGN.md §4.7); enforced server-side via the RBAC `PolicyEnforcer` (fail-closed). Activation is part of the Settings Service, so its control-plane resources sit under `gts.cf.toolkit.settings.*`.

| Operation | Required permission | Scope | Unauthorized |
|-----------|---------------------|-------|--------------|
| Read activation facet / responses (`GET /v1/applies/{apply_id}/activation`, `…/responses`) | `read` on `gts.cf.toolkit.settings.apply_bundle.v1~` | Apply's tenant subtree | `404` if not visible |
| Emit back-response (SDK `report_outcome` → `apply_success`/`apply_failed` broker event — **not REST**) | **Trusted subscriber** — attributed to the subscriber identity, **not** independently verified (DESIGN.md §6); valid within the deployment trust boundary | — | — |

- **AuthN:** the two read endpoints (§4.3) take an ordinary user/session bearer, gated by RBAC as above. There are **no internal REST endpoints** and **no effects/ack REST** — the publisher publishes in-process (`cache_invalidate` inline; `apply_notification` from await-records, §4.2 *Apply Publisher*/§4.2 *Event Broker Client*), and delivery + acknowledgement are Event-Broker events (§4.2 *Apply Publisher*/§4.2 *Cache Invalidation Broadcast*), so this design introduces **no platform service-token surface**.
- **Least-privilege consumer notifications (best-effort):** a consumer's `apply_notification` is filtered to **only the keys it subscribed to** — it is not sent the full "everything that changed" set, which stays inside the trusted replica set (the two-event split, §4.4). This reduces over-sharing / blast radius. It is **not** an identity-enforced isolation guarantee: subscriber identity is **caller-supplied and trusted, not verified** (DESIGN.md §6), so filtering rests on the trusted-caller assumption. **Revisit if consumers are ever exposed as untrusted** — verified subscriber identity + per-tenant read-entitlement would then be required (in settings too).
- **Subscription is trusted-caller, not ownership-bound:** any trusted in-platform consumer may subscribe to any setting key it can read — subscription is **decoupled from contribution/ownership** (§4.2 *Subscription Manager*), so shared and admin-authored keys (owned by no module) are subscribable. Subscriber identity is **caller-supplied and trusted, not verified** (DESIGN.md §6); there is **no ownership/namespace reject**. **Revisit if consumers are ever exposed as untrusted** — verified subscriber identity + per-key read-entitlement would then be required.
- **No sensitive data in the notification stream:** the two **notification** events (`apply_notification`, `cache_invalidate`) carry **identifiers only** — no value, no secret. Back-responses (`apply_success` / `apply_failed`) deliberately echo the **applied value** — plaintext for non-secret settings, a **hash** for secret-valued ones (plaintext never leaves the consumer); tenant scope resolves on re-read under the consumer's own identity.
- **Reaction acks** are attributed to the subscriber identity (`Context`) and are not privileged operations on the value.
- **At-least-once delivery:** Event Broker guarantees at-least-once, so the consumer's reaction must be idempotent — a repeat is re-applied and re-acknowledged. Cache TTL bounds staleness. No security decision depends on delivery.
- **Audit & correlation:** `apply_id` propagates from the Settings Service apply through both events and the reaction report for end-to-end correlation.

### 4.9 Technology Stack

| Concern | Choice | Notes |
|---------|--------|-------|
| Runtime | Part of the `settings-service` ToolKit gear | Not a separate gear; it shares the gear's process, database, and lifecycle |
| Transport | Event Broker | At-least-once; consumers re-apply and re-acknowledge a repeat; no broker-less fallback |
| Durability | PostgreSQL via `toolkit-db` | Await-records double as the delivery queue — no separate outbox |
| Consumer contract | Rust trait in `settings-service-sdk` | `subscribe(keys)` plus `report_outcome(...)`; exact keys only, no prefix subscriptions |
| Observability | Prometheus scrape targets | Metrics enumerated in §7 *Feature Metrics* |

## 5. Risks / Trade-offs

### 5.1 Architectural Trade-offs

Decisions taken during design, with the alternative rejected and the residual cost.

- **Delivery is await-record-driven; `cache_invalidate` is best-effort.** Guaranteed `apply_notification` delivery comes from the durable `apply_await_records` (deliver-until-ack, §4.2 *Event Broker Client*) — no separate outbox. `cache_invalidate` is published inline and **not** persisted; a miss is bounded by `cache_ttl_seconds` (§4.2 *Cache Invalidation Broadcast*, §7 *NFR Mapping*), so replica coherence never depends on its delivery. *Residual:* first-delivery promptness relies on the immediate post-commit publish; the periodic reconcile scan is the backstop.
- **"Applied" is multi-terminal (wait-for-all):** `awaiting` → `success` / `failed` / `superseded` / `cancelled` (§4.2 *Apply Outcome Tracker*). The bundle stays `awaiting` until **every** await-record is terminal; an early failure does not end the wait. *Mitigation:* per-record succeeded/failed/superseded/cancelled/awaiting counts give partial visibility during the wait. A subscription implies an ack; settings nobody subscribed to need no activation (pure pull) and are not part of any bundle's outcome.
- **Heavier reactions are self-react — no central orchestrator (§4.2 *Heavier consumer reactions*).** A consumer that must re-render config or restart does it itself on the signal (re-render in its handler; or restart = exit → supervisor → read current value on boot, §4.6). Nothing is centrally orchestrated and there is no per-setting effect enum. *Residual:* coordinated **rolling** restart across a service's replicas (to avoid simultaneous downtime) is a deployment/rollout concern (RMS), out of scope for activation.
- **Ordering.** No global ordering across applies; a consumer that needs a consistent multi-key snapshot re-reads all its subscribed keys on any signal (resync semantics), not per-key deltas. *Assumption:* most consumers care about eventual consistency, not strict ordering.
- **Subscriber discovery — DECIDED: option (a), the Subscription Manager registry.** The per-subscriber `apply_notification` fan-out resolves subscribers from the registry (§4.2 *Subscription Manager*), populated by explicit subscription — simple, explicit state. Alternatives not taken: (b) query the broker's subscriber API (adds latency per apply); (c) broker broadcasts a subscriber-list feed (eventual consistency). (The `cache_invalidate` broadcast has no such question — it goes to all replicas.)

### 5.2 Security and Performance Risks

- **Consumer discipline.** Reaction depends on the consumer subscribing and reacting. *Mitigation:* consumers **explicitly** subscribe to every setting they must actively activate (§4.2 *Subscription Manager*); authoring guidance; a **seam test** asserting a subscription exists for every setting declared as requiring active activation.
- **Event Broker required.** No broker-less fallback. *Mitigation:* Event Broker is a platform dependency; at-least-once durability with an idempotent consumer reaction is the contract. `cache_ttl_seconds` backstop handles a missed `cache_invalidate`.
- **Repeated delivery.** At-least-once means a consumer may receive the same `(apply_id, key)` more than once (the delivery loop re-publishes an `awaiting` record until acked). It MUST re-apply and re-acknowledge the repeat rather than suppress it — a suppressed repeat withholds its acknowledgement and strands the await-record. *Mitigation:* the reaction is idempotent by construction (re-read → converge → ack, §4.2 *Consumer Activation SDK*); the delivery loop rebuilds each bundle from still-`awaiting` records, so a repeat carries only unacked keys; `redeliver_interval_seconds` backoff bounds re-notify volume; testing covers redelivery.
- **Restart handling.** A restart-only consumer leaves its await-records `awaiting`; on re-subscribe the service re-publishes the notification and the consumer acks after boot (§4.2 *Subscription Manager*, §4.2 *Apply Outcome Tracker*).
- **Hierarchy-change invalidation is not covered by the broadcast** — a tenant re-parent or mid-chain insert changes a `cascading` effective value with no apply to broadcast about (§4.2 *Cache Invalidation Broadcast*). The eviction trigger belongs to the settings-service cache, but the Tenant Resolver publishes no hierarchy-change signal today, so after a re-parent a replica may serve the pre-move value for up to `cache_ttl_seconds`. *Mitigation:* the TTL backstop; the durable fix is a Tenant Resolver hierarchy-change event (§4.2 *Cache & Invalidation* / §4.4/DESIGN.md §6).

## 6. Open Questions

### 6.1 From PRD (Cross-Reference)

No PRD open question is owned by this design. The activation-relevant items are tracked in [PRD.md](./PRD.md) and in [DESIGN.md](./DESIGN.md) DESIGN.md §6.1.

### 6.2 Design-Specific Questions

- **Unbounded wait / stuck bundle (OPEN).** With wait-for-all and no deadline, a bundle with a permanently unresponsive await-record never reaches a terminal outcome — it stays `awaiting` until an administrator stops waiting. There is **no automatic timeout** (by design). A clean **gear retire** is already handled deterministically — its await-records resolve to `cancelled` (§4.2 *Subscription Manager*) — so this open question covers **only** a consumer that vanishes **without** retiring. **Open:** should there be an **admin-initiated** "give up / close as failed" action for such a consumer (and how is one that is gone-for-good distinguished from a slow/restarting one)?
- **Replica registration (OPEN).** A consumer with a unique identity is straightforward; a consumer running as **several replicas** is unclear — how are replicas registered and accounted for so acknowledgement is tracked correctly per await-record? (Note: this concerns *acking consumers* running as replicas — Settings Service cache-invalidation replicas need no registration, §4.2 *Cache Invalidation Broadcast*.)
- **Activation-model rationale is recorded in this design.** The decisions behind the model — notify-and-react vs central execution, the two-event split for isolation, Event Broker durability, wait-for-all with an unbounded wait, restart handling, and subscriber discovery — are stated in §3.1 *Design Principles*, §3.2 *Constraints*, and §5.1 *Architectural Trade-offs*.

## 7. Additional context

### Feature Metrics

All metrics exposed as Prometheus scrape targets.

| Vector | Metric | Description | Target Threshold |
|--------|--------|-------------|------------------|
| **Performance** | `settings_notify_publish_to_deliver_seconds{event}` | Publish→deliver latency by event (`apply_notification`, `cache_invalidate`) | p95 ≤ 2s |
| **Performance** | `settings_notify_response_latency_seconds` | apply_notification → apply_success back-response latency (observation only; the wait itself is unbounded) | p95 ≤ 30s |
| **Reliability** | `settings_notify_redeliveries_total` | `apply_notification` re-publications by the delivery loop (retries beyond first send) — rising = consumers slow/failing to ack | — |
| **Reliability** | `settings_notify_awaiting_records` | Await-records still `awaiting` across open bundles — surfaces stuck/long-running reconciliations | — |
| **Reliability** | `settings_notify_cache_invalidations_total` | `cache_invalidate` broadcasts published (one per apply) | — |
| **Reliability** | `settings_notify_reactions_total{outcome}` | Reaction outcomes (`succeeded`/`failed`) | — |
| **Versatility** | `settings_notify_subscriptions` | Active per-setting subscriptions | — |

### NFR Mapping

- **Delivery:** publish→deliver p95 ≤ 2s (Event Broker backend); no consumer activation lost — the delivery loop re-publishes an `awaiting` await-record until it is acked (§4.2 *Event Broker Client*).
- **Consumer activation latency:** reaction p95 ≤ 30s for subscribed settings (a latency *target* for observation; the reconciliation wait itself is unbounded and admin-driven, §6).
- **Availability:** notification availability follows the Settings Service SLO; a transport outage degrades to pull + cache-TTL convergence, not to stale-forever.
- **Cache staleness bound:** ≤ `cache_ttl_seconds` (owned by the cache components — DESIGN.md §4.2 *Cache & Invalidation* / DESIGN.md §4.5 — not by this design; default 30s) even if a `cache_invalidate` broadcast is missed.
- **Multi-replica coherence:** no replica serves a value staler than TTL after an apply.
- **Signal ordering:** every signal is published **after** the value's durable commit (§4.2 *Cache Invalidation Broadcast*; DESIGN.md §4.2 *Apply Orchestrator* commit → evict → publish), so no recipient ever observes a signal for an unstored value.
- **Apply-failure visibility:** per-administrator failure notification is this design's back-response/`event_apply_failed` path; the **aggregate** operator-facing signal — apply-failure-rate on the shared platform dashboards plus an alert-routing rule for platform-wide conditions — is owned by the settings-service design (§7 *Feature Metrics* / DESIGN.md §7 *NFR Mapping & Scale Model* there), since it derives from apply outcomes rather than activation outcomes.

### Testing Architecture

#### Testing Levels

| Level | Database | Network | What is real | What is mocked |
|---|---|---|---|---|
| **Unit** | No DB — in-memory trait mocks | No network | Publisher await-record co-commit + delivery-loop logic, per-subscriber notification filtering, per-record outcome aggregation, wait-for-all status computation, subscription matching | `InMemoryNotificationRepo`, `MockEventBrokerClient` |
| **Integration** | Real PostgreSQL (testcontainers, per-test tx) | No network — direct calls; Event Broker client mocked | Await-record co-commit, delivery from awaiting records, subscriptions, tracking | — |
| **API** | Real PostgreSQL | In-process HTTP (`Router::oneshot`) | Activation-facet REST, domain services, DB | `PolicyEnforcer` (Allow/Deny), Event Broker |
| **E2E** | Real PostgreSQL + running broker | Real HTTP + real pub/sub | Full publish→deliver→react→outcome loop across two processes, incl. filtered delivery, cache_invalidate broadcast, restart re-publish | Nothing (broker + a sample consumer gear are real) |

#### Level 1: Unit Tests (Domain Layer)

**Mock boundaries:**

| Mock | Purpose | Pattern |
|------|---------|---------|
| `InMemoryNotificationRepo` | HashMap-backed subscriptions/await-records/responses | `with_subscriptions(vec![...])` |
| `MockEventBrokerClient` | Capture published notifications + broadcasts; inject delivery + back-responses | `.published`, `.broadcast`, `.deliver(event)` |

| What to test | What is mocked | Verification target |
|---|---|---|
| Publish builds identifier-only payloads | `MockEventBrokerClient` | No value/secret in `apply_notification` or `cache_invalidate`; idempotent per `apply_id` |
| Per-subscriber filtering | `InMemoryNotificationRepo` | A subscriber's `apply_notification` contains **only** its subscribed changed keys; no other keys/tenants leak |
| cache_invalidate carries full set | `MockEventBrokerClient` | Broadcast contains all changed keys; one broadcast per apply |
| Publish creates one await-record per (apply, subscription) | `InMemoryNotificationRepo` | `expected_records` == record count (changed keys × their subscribers), each with its expected value |
| Outcome status — wait-for-all | `InMemoryNotificationRepo` | `awaiting` until every record terminal (even if some already failed/superseded); then `failed`/`superseded`/`success` |
| Outcome status — value-mismatch success ack | repo | `success`-status ack with non-matching value → record `failed` → bundle `failed` |
| Outcome status — supersession | repo | a later confirmed apply for a key → older `awaiting` records for `(subscriber, key, tenant)` `superseded`; a **failed** later apply supersedes nothing |
| Outcome status — secret hash compare | repo | Hash of applied value compared to hash of expected value; match → succeeded, mismatch → failed |
| Delivery loop re-publish | `MockEventBrokerClient` | The delivery loop re-publishes `awaiting` records (respecting `last_notified_at` / `redeliver_interval_seconds` backoff); terminal records are not re-sent |
| Subscription exact-key matching | repo | Correct subscribers resolved for a key; non-subscribed keys excluded |
| Error mapping — all domain → API variants | none | 100% variant coverage |

#### Level 2: Integration Tests (Persistence + Transport)

**Infrastructure:** PostgreSQL via testcontainers (Event Broker is the transport; no Postgres `LISTEN/NOTIFY`).

| What to test | Setup | Verification target |
|---|---|---|
| Delivery for a committed apply | Seed apply + await-records | `cache_invalidate` published inline; delivery loop publishes a filtered `apply_notification` per subscriber from awaiting records; none lost |
| Publish-after-commit boundary | Abort apply tx | No await-records, no delivery, no `cache_invalidate` |
| Delivery backoff | Re-scan before `redeliver_interval_seconds` | Record not re-published until the interval elapses (`last_notified_at`) |
| Bundle tracker creation | Apply published | One await-record per (apply, subscription); `expected_records` = record count |
| Per-record response tracking | Seed apply + responses | `succeeded_records` / `failed_records` / `superseded_records` reflect the record statuses; `overall_status` computed on demand (wait-for-all) |
| Response idempotency | Duplicate back-response (incl. platform-wide `tenant = NULL`) | `uq_await_record` on `(apply_id, subscriber, key)` — at-most-once; NULL-tenant duplicates blocked |
| Subscription idempotency | Re-register same `(subscriber, key)` | `uq_sub_subscriber_key` — no duplicate |

#### Level 3: API Tests (REST Layer)

**Mock boundaries:** `PolicyEnforcer` (Allow/Deny), Event Broker mocked; DB + domain real.

| What to test | Method | Verification target |
|---|---|---|
| Activation status | `GET /v1/applies/{apply_id}/activation` | Correct `overall_status` + succeeded/failed/superseded/cancelled/awaiting record counts |
| List responses | `GET /v1/applies/{apply_id}/activation/responses` | All back-responses for the apply (per subscriber/key) |
| AuthZ deny | `GET …/activation` + `DenyingAuthZClient` | 403 |
| RFC 9457 errors | trigger each category | `type`/`title`/`status`/`detail` present |

#### Level 4: E2E Tests (Python / pytest)

**Infrastructure:** running Settings Service + broker + a sample consumer gear.

| What to test | Marker | Verification target |
|---|---|---|
| Full loop: apply → apply_notification → consumer re-read → apply_success → bundle `success` | `@pytest.mark.smoke` | End-to-end across two processes; bundle tracks outcome; `success` only after every record terminal |
| Consumer isolation (filtered delivery) | — | A consumer subscribed to key A, on an apply changing A and B, receives **only** A — never B or another tenant |
| Replica cache coherence (broadcast) | — | Every replica evicts on `cache_invalidate`; a second replica serves fresh value within TTL after apply |
| Value mismatch → failed | — | A consumer that applies a wrong value → its record `failed` → bundle `failed` |
| Restart re-publish | — | Restart-only consumer leaves await-record awaiting; on re-subscribe the service re-publishes; consumer acks; record resolves (no TTL) |
| No secret in stream | — | Subscribed observer never sees a secret value in any event or back-response (hash only) |

#### What Must NOT Be Mocked

| Component | Why |
|---|---|
| Await-record co-commit + delivery (integration + E2E) | Correctness depends on the real transactional co-commit of await-records and deliver-until-ack from them |
| Event Broker at-least-once behavior | At-least-once redelivery semantics must be tested; a repeated notification must be re-applied and re-acknowledged |
| Per-subscriber filtering on real broker (E2E) | Key-scoped least-privilege (best-effort, §4.8): a subscriber receives only its own keys — must be proven against real delivery |
| Unique indexes (await-record, subscription, tracker) | DB-level at-most-once for await-records (`uq_await_record` on `(apply_id, subscriber, key)`) |
| Replica cache eviction on real Event Broker (E2E) | Multi-replica coherence is the headline guarantee |

#### Concurrency Testing

Concurrent applies and concurrent back-responses exist. Test: seed N applies, spawn back-responses with barrier-synchronized start, assert a single `apply_await_records` row per `(apply_id, subscriber, key)` (via `uq_await_record`) and a deterministic final `overall_status` (wait-for-all). Assert that under concurrent applies each apply's `cache_invalidate` is published once and its `apply_notification`s are delivered at-least-once (each delivery re-applied and re-acked by the consumer).

#### NFR Verification Mapping

| NFR | Test level | How verified |
|---|---|---|
| Publish→deliver p95 ≤ 2s | Integration + load | Timed publish/deliver via Event Broker (both events) |
| Response p95 ≤ 30s (target) | E2E | Timed apply_notification → apply_success |
| Cache staleness ≤ TTL | E2E | Replica read after apply within TTL |
| No activation lost | Integration + E2E | Unanswered await-record re-published on re-subscribe (no TTL) |
| Multi-replica coherence | E2E | Two-replica fresh-read after cache_invalidate broadcast |

## 8. Traceability

- **PRD**: [PRD.md](./PRD.md)
- **Settings Service design**: [DESIGN.md](./DESIGN.md) — storage, resolution, the pull reader, and the apply commit this design publishes from
- **Features**: [features/](./features/) — TBD, not yet authored for this gear
