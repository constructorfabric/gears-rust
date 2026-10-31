---
description: "Feature specification of the Settings Service gear's consumer activation: subscriptions, the key's mutation lock, the activation state written in each change's transaction, the filtered change_notification and its delivery loop, the outcome recorder with its activation audit record, and the consumer SDK."
---

<!-- Created: 2026-10-31 by Virtuozzo International GmbH -->
<!-- Updated: 2026-10-31 by Virtuozzo International GmbH -->

# Feature: Consumer Activation

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-consumer-activation`

- [ ] `p1` - `cpt-cf-settings-service-feature-consumer-activation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Subscribe to Settings](#subscribe-to-settings)
  - [Record the Activation State with a Change](#record-the-activation-state-with-a-change)
  - [Publish Notifications at the End of a Request](#publish-notifications-at-the-end-of-a-request)
  - [Deliver the Awaiting States](#deliver-the-awaiting-states)
  - [React to a Notification](#react-to-a-notification)
  - [Report an Outcome](#report-an-outcome)
  - [Retire a Subscription](#retire-a-subscription)
  - [Run the Activation Half of the Broker Lifecycle](#run-the-activation-half-of-the-broker-lifecycle)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Key Mutation Lock](#key-mutation-lock)
  - [Affected Subscriptions](#affected-subscriptions)
  - [Expected Value Encoding](#expected-value-encoding)
  - [Outcome Rules](#outcome-rules)
  - [Notification Bundle](#notification-bundle)
  - [Subscribe Per-Key State](#subscribe-per-key-state)
  - [Fresh Read](#fresh-read)
- [4. States (CDSL)](#4-states-cdsl)
  - [Activation State Machine](#activation-state-machine)
  - [Subscription Key State Machine](#subscription-key-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Activation Schema](#activation-schema)
  - [Subscriptions](#subscriptions)
  - [Every Mutation of a Key Takes Its Lock](#every-mutation-of-a-key-takes-its-lock)
  - [Activation State in the Change's Transaction](#activation-state-in-the-changes-transaction)
  - [Canonical Encoding and Secret Hash](#canonical-encoding-and-secret-hash)
  - [Canonical Encoding in the SDK](#canonical-encoding-in-the-sdk)
  - [Notification Delivery](#notification-delivery)
  - [Outcome Recording](#outcome-recording)
  - [Consumer Activation SDK](#consumer-activation-sdk)
  - [Activation Is Observable](#activation-is-observable)
  - [Activation Is Observable in the SDK](#activation-is-observable-in-the-sdk)
  - [Activation Tests](#activation-tests)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Lets a consuming gear subscribe to the settings it materializes, learn which of them changed at the tenant it serves, re-read them and report what it applied, and keeps, per subscription, whether the current value is confirmed. Every value change records, inside its own transaction and under the key's mutation lock, one activation state per subscription it affects — `awaiting`, with the expected value at the subscription's tenant and the change's audit record id as `change_ref`; the end of the request publishes one filtered `change_notification` per affected subscriber and tenant on the activation topic of the platform Event Broker; a delivery loop on every replica re-publishes what stays `awaiting`; and the consumer's report, an SDK call, settles the state against the expectation recorded when the change committed and appends an `activation` audit record.

### 1.2 Purpose

A consumer that has already materialized a setting — a connection pool sized by it, a socket bound from it, a rendered file — never re-reads it unless something says it changed, and an administrator who set a value cannot tell from the `set` response whether the platform runs on it. This feature carries the protocol of DESIGN-activation under ADR-003: a thin event carrying identifiers only, a pull of the current value through the reader, and an outcome reported through the SDK and verified against a snapshot.

Two distributions follow a committed change, and this feature owns the second of them (DESIGN-activation §1.1 *Two distinct distributions*):

| | Replica cache invalidation (entry 2.12) | Consumer activation (this feature) |
|---|---|---|
| Who receives | Every Settings Service replica | Only subscribers of the changed key at a tenant whose effective value changed |
| Subscription | None | Per exact key and tenant, explicit, through the SDK |
| Payload | The tenant's full changed-key set | The subscriber's own `awaiting` keys at that tenant, each with its `change_ref` |
| Acknowledged | No | Yes — one activation state per subscription, no deadline |
| Event | `cache_invalidate`, cache topic | `change_notification`, activation topic |

Six properties shape the feature.

**One activation state per subscription.** A subscription is `(subscriber, key, tenant)` and holds at most one activation state. The latest change that affects it sets what the subscriber is expected to apply; a later change overwrites the state and resets it to `awaiting`, and that overwrite is the whole of supersession. `change_ref` is the id of the change's audit record, which correlates the change, the notification and the report.

**The state commits with the value.** The activation state is written in the change's own transaction, under the key's mutation lock held from before the candidate step until the commit and the local eviction, so no committed change can lack its expectation and no registration or retire lands between the subscriptions a change saw and its commit.

**Delivery repeats until reported, and the wait is unbounded.** The `awaiting` states are the delivery queue: the end of the request publishes them promptly, and the delivery loop re-publishes each one every `redeliver_interval_seconds` until the consumer reports or a later change replaces it. No state reaches a terminal status on a timer.

**No value in any notification.** A `change_notification` carries the tenant, the subscriber and the keys with their `change_ref`, nothing else; the consumer re-reads under its own identity with the freshness option. The report is the deliberate exception: it echoes the applied value, a salted hash for a `secret`-trait setting.

**The outcome is an SDK call.** `report_outcome` reaches the Activation Outcome Recorder directly — not a broker event, not a REST route — and answers `applied` or `ignored`.

**The broker is a soft dependency.** Without a broker a write commits, its states stay `awaiting`, the delivery loop idles, and delivery resumes once the broker resolves.

**Requirements**: `cpt-cf-settings-service-fr-consumer-activation`, `cpt-cf-settings-service-contract-consumer-activation`, `cpt-cf-settings-service-fr-live-read-activation` (the change signal a consumer self-reacts to), `cpt-cf-settings-service-nfr-reliability-validated-set` (a refused or stale write stores no activation state), `cpt-cf-settings-service-nfr-security-baseline` (identifiers only in the notification stream, a hash in place of a secret in a report), `cpt-cf-settings-service-nfr-ops-set-monitoring` (the activation and mutation-lock metrics)

**Principles**: `cpt-cf-settings-service-principle-notify-not-execute`, `cpt-cf-settings-service-principle-identifiers-only`, `cpt-cf-settings-service-principle-snapshot-verification`, `cpt-cf-settings-service-principle-unbounded-wait`, `cpt-cf-settings-service-principle-two-signal-split` (extends, covered by entry 2.12: this feature carries the `change_notification` half)

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-internal-caller` | The Settings Consumer Gear: through the SDK it subscribes per key and tenant, receives its filtered notifications, re-reads the notified keys fresh, reports a per-setting outcome and retires its subscriptions |
| `cpt-cf-settings-service-actor-platform-admin` | Makes the value writes and restriction changes whose commits record activation states and take the key's mutation lock; holds no operation on activation itself |
| `cpt-cf-settings-service-actor-tenant-admin` | Makes value writes and restriction changes within its subtree, with the same effect; holds no operation on activation itself |
| `cpt-cf-settings-service-actor-event-broker` | Carries each `change_notification` from the publishing replica to the consumer gear's anonymous consumer group on the activation topic; its absence never fails a write and loses no activation |
| `cpt-cf-settings-service-actor-tenant-resolver` | Answers, under the key's mutation lock and before the change's transaction, the ancestor chain of each subscription tenant of a `cascading` key, which decides whether the written scope covers it |
| `cpt-cf-settings-service-actor-authz-resolver` | Decides, through the `PolicyEnforcer`, the registration read decision — `read` on the key at the subscription's tenant — before the lock, and the per-setting decision `resolve_plaintext` takes for the service's own identity before a fallback or staged plaintext fetch |
| `cpt-cf-settings-service-actor-types-registry` | Holds the activation topic and the `change_notification` event type the delivery loop registers once the broker resolves |
| Credential Store, reached through the Secret Manager | Yields the plaintext of an inherited fallback secret, or of a staged secret a batch adopts, once per change under the service's own identity, so the expected value can be hashed; it has no PRD actor of its own and is reached only through `cpt-cf-settings-service-component-secret-manager` |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.5 Consumer Activation: Notify, Confirm, and Account; §5.5 A Value Activates via Live-Read; Consumers Self-React; §6.1 (Reliability: Validate, and Refuse a Stale Set; Security; Operational Visibility); §7.2 Consuming Gear Activation Contract; §2.2 System Actors (Event Broker, Internal Service Caller); §9 Acceptance Criteria (the consuming-gear notification and confirmation criterion)
- **Design**: [DESIGN.md](../DESIGN.md) — §4.2 (Component: Value Resolver, its two uncached forms of `resolve`; Component: Tenant Access, the `toolkit-db` `LockManager` dependency and the lock on `set_restriction` and `clear_restriction`; Component: Value Writer, the `set`, `revert` and `remove_value` rows and the end-of-request publish hook; Component: Secret Manager, `resolve_plaintext` with its two in-process uses and `secret_use`; Component: Cache & Invalidation; Component: Audit Emitter), §4.3 (the *Lock wait exhausted* `503` rows; the history read paragraph, whose attachment of reports is entry 2.14's), §4.4 (*Activation signals on the broker*), §4.5 (`get_effective` with `freshness?`), §4.7 (`audit_records`: `operation = activation`, the `detail` and `change_ref` columns, `idx_audit_change_ref`), §4.8 (*Access binds the writer, not the target*; *Publishing identity*), §4.9 (*The Event Broker as a soft dependency*; Gear lifecycle), §7 (*Set — value write, audit record and activation state are one transaction*, *Set — commit + publish*, *Set — no broker resolvable*, Concurrency Testing item 5)
- **Design (activation)**: [DESIGN-activation.md](../DESIGN-activation.md) — the authoritative design of this feature: §1.1 (*Two distinct distributions*), §2.1, §3.1, §3.2, §4.1 (every entity, *Canonical value encoding*, *Secret-value hash*, *File-reference values*, the enums), §4.2 (Activation Publisher with *Which subscriptions a change affects*, *Registration and writes*, *The expected value of a secret* and *Cost*; Event Broker Client with its configuration, `publish_notification`, `subscribe_notifications`, `deliver_awaiting` and *Broker topology*; Subscription Manager with *Why only one replica scans at a time*; Consumer Activation SDK with the consumer obligation and *Fresh re-read*; Activation Outcome Recorder with *Reports in the history*, *Supersession* and *Concurrency of reports*), §4.3 (what is and is not REST), §4.4 (the `change_notification` row, the subject pair), §4.5 (the SDK table, *Results and errors*), §4.6, §4.7, §4.8, §5.2, §6.2, §7
- **ADR**: [ADR-003](../ADR/ADR-003-thin-event-pull-report-activation.md) (`cpt-cf-settings-service-adr-thin-event-pull-report-activation`) — the notification carries no value; the consumer pulls and reports
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.13, and §1 *Accepted narrowing — entitlement is decided once, at registration*
- **Dependencies**: as DECOMPOSITION entry 2.13 lists them — entry 2.12 (`cpt-cf-settings-service-feature-replica-coherence`) for the Event Broker Client, the publishing identity, the root tenant's id its consumer task caches (`inst-rc-lifecycle-4` of `cpt-cf-settings-service-flow-replica-coherence-lifecycle`) and the end-of-request publish hook, whose broadcast (`cpt-cf-settings-service-flow-replica-coherence-publish`) runs before the notifications; entry 2.8 (`cpt-cf-settings-service-feature-value-writes`) for the per-change commit `cpt-cf-settings-service-algo-value-writes-commit`, whose transaction carries the activation state, and the batch `cpt-cf-settings-service-flow-value-writes-batch`; entry 2.6 (`cpt-cf-settings-service-feature-audit-store`) for the append `cpt-cf-settings-service-algo-audit-store-append`, whose record id is the `change_ref` and which takes the `activation` record, and the resource formatter `cpt-cf-settings-service-algo-audit-store-resource-id`, the history read `cpt-cf-settings-service-flow-audit-store-history`, to whose `operation` vocabulary entry 2.13 adds only the `activation` value while entry 2.14 adds the attachment of reports to their change; entry 2.9 (`cpt-cf-settings-service-feature-secret-values`) for the plaintext resolution and its `secret_use` record (`cpt-cf-settings-service-flow-secret-values-resolve`, `cpt-cf-settings-service-dod-secret-values-machine-path`, DESIGN.md §4.2 *Secret Manager* `resolve_plaintext`), the service principal (`cpt-cf-settings-service-algo-secret-values-reference`) and the staged entry a batch adopts (`cpt-cf-settings-service-flow-secret-values-stage`). Touched, not depended on: entry 2.7 (`cpt-cf-settings-service-feature-tenant-access`), whose restriction changes `cpt-cf-settings-service-flow-tenant-access-set` and `cpt-cf-settings-service-flow-tenant-access-clear` take the key's mutation lock, and entry 2.5, whose resolver and cache the fresh reads bypass.
- **The in-process uses of `resolve_plaintext`**: this feature's two in-process uses of the Secret Manager's `resolve_plaintext` — the inherited fallback fetch of a `revert` or `remove_value`, and the staged-secret adoption fetch of a batch `set` — extend the machine-path contract of `cpt-cf-settings-service-dod-secret-values-machine-path`: each takes the same per-setting authorization decision, for the service's own identity instead of a calling gear's, and writes the same `secret_use` record, and the plaintext never leaves the process. Both uses are recorded in the shared design, not a local deviation: DESIGN.md §4.2 *Secret Manager* names them as the two in-process uses of `resolve_plaintext`, and DESIGN-activation §4.2 *The expected value of a secret* specifies each fetch; this feature carries them as their consequence. The staged-secret fetch exists because a batch `set` adopting a staged secret holds no plaintext at the commit, since the stage flow put it in the Credential Store earlier, while the expected value must be the hash of the value the consumer will read, so the plaintext is fetched once per change under the lock, by the staged reference, exactly as the fallback is.
- **Entitlement**: entitlement to a setting is decided once, at registration, by the registration read decision; no delivery re-decides it, and `hidden` never filters a notification (DECOMPOSITION §1 *Accepted narrowing — entitlement is decided once, at registration*).
- **Performance**: a change holds the key's mutation lock through its candidate step, any plaintext fetch and its transaction, a time bounded by the key's subscriptions (DESIGN-activation §4.2 *Cost*); a change at the per-key bound of DESIGN-activation §7 *Fan-out bounds* exceeds the write's REST latency target (DESIGN.md §7) and queues the key's other mutations meanwhile, while the typical change of a few candidates stays within it. The metrics observe the targets of DESIGN-activation §7 — publish-to-deliver p95 at most 2 s, report latency p95 at most 30 s — and the lock's wait; the two p95 targets are observed through the metrics only and no test asserts them.
- **Not applicable**: The per-setting activation read of DESIGN-activation §4.3, the Recorder's `activation_status` and the reports attached to the history read are entry 2.14 (`cpt-cf-settings-service-feature-activation-read`); this feature adds no endpoint. User experience is not applicable because the feature has no user interface: its surfaces are an SDK contract, an event and a background loop. Compliance is not applicable beyond the baselines the feature inherits, because an activation state holds a value the store already holds under its classification, a secret only as a salted hash, and an `activation` audit record follows the audit table's retention. Activation of subject-scoped values is not tracked in v1 — the state's identity has no subject component and the optional subject pair stays absent in a notification. The subtree subscription mode is deferred (DESIGN-activation §6.2). Signalling a hierarchy change — a re-parent or a mid-chain insertion — to subscribers is a non-goal: no activation state is written for it and the pull path covers it. Dependency groups and domain-affinity filtering are R3. An administrator action that closes an outstanding state as failed, a secret hash keyed from the Credential Store, and accounting for a consumer that runs as several replicas are open questions (DESIGN-activation §6.2) and are not built. The broker binding of the domain events of DESIGN §4.4 *Events Emitted* belongs to no entry of this wave (DECOMPOSITION §1). Verified subscriber identity is not built: the subscriber and the subscription tenant are taken on trust under the trusted-caller model (DESIGN §6), as DECOMPOSITION §1 *Accepted narrowing* records. The `cache_invalidate` broadcast, its consumer and its metric are entry 2.12's.
- **Free-text `detail`**: the consumer's failure detail is written into the append-only audit table; the SDK documents it as free of values and secrets, that contract and the 1 KiB cap are the only controls, and a value leaked into it cannot be redacted (DESIGN-activation §5.2).
- **Rollout, rollback and compile-time gate**: the feature adds one migration — the two tables and the `audit_records` additions (`operation = activation`, the nullable `change_ref` and `detail` columns, `idx_audit_change_ref`). Writes by an earlier build, or by a replica still on the earlier build during a rolling upgrade, take no mutation lock and record no activation state, so consumers subscribe only once every replica runs this build. An earlier build reads neither table, but its history read treats an `operation` it does not know as an integrity error (`cpt-cf-settings-service-dod-audit-store-table`, DESIGN.md §4.7), so a rollback is clean only while no `activation` record exists; once a report has been recorded, the accepted window is that the history read of each setting holding an `activation` record fails until this build is restored. Review owner: the settings-service maintainers. Until entry 2.14 lands, an `activation` record appears in the history of its subscription tenant as an ordinary record, resource `cf.settings:{key}@{tenant}`, with its `change_ref` and `detail` not yet rendered; entry 2.14 attaches it to its change. Removing the broker from a deployment leaves writes committing and states `awaiting`. The compile-time gate is the SDK's breaking contract: the consumer activation contract `settings-service-sdk` declares today — a change set id per notification, `subscribe` without a tenant, `report_outcome` naming a change set — is replaced by the contract this feature specifies (§2 *Subscribe to Settings*, *Report an Outcome*; DESIGN-activation §4.5), no gear in this repository compiles against the replaced one, the SDK takes a breaking version bump, and a consumer gear built against the replaced contract no longer compiles, which is the intended signal.
- **Design components**: `cpt-cf-settings-service-component-subscription-manager`, `cpt-cf-settings-service-component-change-set-publisher` (the Activation Publisher), `cpt-cf-settings-service-component-change-set-outcome-tracker` (the Activation Outcome Recorder, `receive_response` only), `cpt-cf-settings-service-component-consumer-activation-sdk`; extends `cpt-cf-settings-service-component-event-broker-client` (the activation topic, `publish_notification`, `subscribe_notifications`, `deliver_awaiting`), `cpt-cf-settings-service-component-value-writer` (the lock, `record_change` in each change's transaction, the plaintext fetches, the notification half of the end-of-request hook), `cpt-cf-settings-service-component-tenant-permission` (the lock on restriction changes), `cpt-cf-settings-service-component-value-resolver` (the two uncached forms of `resolve`), `cpt-cf-settings-service-component-cache-and-invalidation` (the fresh read bypasses it) and `cpt-cf-settings-service-component-secret-manager` (the two in-process uses of `resolve_plaintext` — the fallback fetch and the staged-secret adoption fetch — audited as `secret_use`)
- **Sequences**: `cpt-cf-settings-service-seq-activation-change-set-notify` (its broadcast step is entry 2.12's), `cpt-cf-settings-service-seq-activation-restart-resubscribe`

## 2. Actor Flows (CDSL)

The flows name their actor by exception where no human acts: the record flow is triggered by an administrator's write — named as the platform administrator, a tenant administrator's write within its subtree taking the same path — and the publish, deliver and lifecycle flows by the gear itself, named after the Event Broker, the system actor the PRD gives the signal. The subscribe, react, report and retire flows are the consuming gear's, through the SDK. The lifecycle flow is the delivery loop's one control loop and owns its tick; the deliver flow is the body of one pass of it.

**Use cases**: `cpt-cf-settings-service-usecase-review-and-set`, `cpt-cf-settings-service-usecase-resolve-effective-value`

Throughout, a tenant is a bare lowercase UUID string, and platform scope is the root tenant's id, an ordinary tenant id and never an absent one. "The activation topic" is `gts.cf.core.events.topic.v1~cf.core.settings.activation.v1`, "the `change_notification` type" is `gts.cf.core.events.event.v1~cf.core.settings.change_notification.v1~`, and "the activation subject type" is `gts.cf.core.settings.activation.v1~`. "The subscriber" is the identity of the calling gear — the subject of the security context the SDK `Context` carries — never an argument the caller passes. A subscription handle is live from the moment `subscribe` returns it until the caller drops it or `retire()` retires it. Every wait with backoff on the broker or on a registration — the SDK's creation of its consumer group, its join, and its retries of a `pending` key — is exponential from 1 s to a 30 s cap with jitter, as in entry 2.12, and is not configurable; the delivery loop retries its broker lookup and the activation types' registration on its own tick (`inst-ca-lifecycle-7`), and the key's mutation lock waits on a schedule of its own (`inst-ca-lock-3`).

### Subscribe to Settings

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-consumer-activation-subscribe`

**Actor**: `cpt-cf-settings-service-actor-internal-caller`

**Success Scenarios**:
- `subscribe` returns its handle at once; each key the gear may read becomes `registered` with the fresh effective value at the tenant, read after its registration committed, and the gear's process holds one anonymous consumer group on the activation topic with an interest for that tenant
- A key that cannot be registered yet — not declared until a later boot of its owner — stays `pending`, is retried, and on its registration hands the handler its fresh value in a bundle without `change_ref`, which owes no report
- A retired key is registered and reported `registered` with its value absent; its next change after a reactivation writes the subscription's state

**Error Scenarios**:
- Empty `keys` or a malformed tenant: an error, no handle, nothing registered
- A 65th tenant interest of the gear in the process: every key `refused`, nothing registered
- The subscriber may not read the key at the tenant (`403`): that key `refused`, terminal
- A key refused `NotFound` before the lock, a lock wait exhausted, the authorization service unavailable, a database error before the commit, a fresh read failing after the commit, or a missing broker grant: that key `pending`, retried while the handle is live, a warning each minute

**Steps**:
1. [ ] - `p1` - The consuming gear calls the SDK's `subscribe` with `keys`, a `tenant` and a handler, under its own security context - `inst-ca-subscribe-1`
2. [ ] - `p1` - **IF** `keys` is empty **OR** `tenant` is not a UUID → **RETURN** an invalid-input error, with nothing registered and no handle - `inst-ca-subscribe-2`
3. [x] - `p1` - Normalise `tenant` to a bare lowercase UUID string and take the subscriber from the caller's security context - `inst-ca-subscribe-3`
4. [ ] - `p1` - **IF** the gear's consumer group in this process holds no interest for `tenant` **AND** already holds 64 tenant interests → **RETURN** the handle with every key `refused` and nothing registered, logged at `warn` - `inst-ca-subscribe-4`
5. [ ] - `p1` - Hand the caller the handle at once, with every key `pending` - `inst-ca-subscribe-5`
6. [ ] - `p1` - Run the steps below in the background for as long as the handle is live - `inst-ca-subscribe-6`
7. [ ] - `p1` - Move each key's state as the per-key state rules say (`cpt-cf-settings-service-algo-consumer-activation-subscribe-state`) - `inst-ca-subscribe-7`
8. [ ] - `p1` - **FOR EACH** key on the handle, in order and one at a time - `inst-ca-subscribe-8`
   1. [x] - `p1` - Take the registration read decision — `read` on the key at `tenant` through the `PolicyEnforcer` — before any lock is taken - `inst-ca-subscribe-9`
   2. [x] - `p1` - **IF** the decision is a denial → mark the key `refused`, log at `warn`, and take no further step for it - `inst-ca-subscribe-10`
   3. [x] - `p1` - **IF** the decision cannot be obtained → leave the key `pending` for a retry and take no further step for it now - `inst-ca-subscribe-11`
   4. [x] - `p1` - Invoke `register_subscription` for the subscriber, the key and `tenant` - `inst-ca-subscribe-12`
   5. [x] - `p1` - DB: SELECT the declaration by key; **IF** none exists, **OR** it is hidden from the subscriber at `tenant`, **OR** `tenant` lies outside the subscriber's subtree → refuse `NotFound` with nothing registered and no lock taken, leave the key `pending`, and take no further step for it now - `inst-ca-subscribe-13`
   6. [x] - `p1` - Take the key's mutation lock (`cpt-cf-settings-service-algo-consumer-activation-lock`) around the transaction that inserts the subscription - `inst-ca-subscribe-14`
   7. [x] - `p1` - **IF** the lock wait is exhausted → nothing is registered, the key stays `pending`, and take no further step for it now - `inst-ca-subscribe-15`
   8. [x] - `p1` - DB: INSERT into `subscriptions` a row with a UUIDv7 `id`, the subscriber, the key, `tenant` and `created_at` now, idempotent on `uq_sub_subscriber_key_tenant` so an existing row is kept; a retired key whose declaration still exists is registered like a live one - `inst-ca-subscribe-16`
   9. [x] - `p1` - Commit the transaction - `inst-ca-subscribe-17`
   10. [x] - `p1` - Release the lock and await the release, whether the transaction committed or failed - `inst-ca-subscribe-31`
   11. [x] - `p1` - **IF** the insert or the commit failed → nothing is registered and the key stays `pending`; take no further step for it now - `inst-ca-subscribe-18`
   12. [x] - `p1` - Log the registration at `info` with the subscriber, the key and the tenant - `inst-ca-subscribe-19`
   13. [ ] - `p1` - After the commit, read the key at `tenant` through the fresh read (`cpt-cf-settings-service-algo-consumer-activation-fresh-read`) - `inst-ca-subscribe-20`
   14. [ ] - `p1` - **IF** the read returns a value → mark the key `registered` with that value, a `SecretHandle` for a `secret`-trait key; **IF** it returns `Retired` → mark it `registered` with the value absent - `inst-ca-subscribe-21`
   15. [ ] - `p1` - **IF** the read fails → keep the registration and leave the key `pending`, its retry resuming at the fresh read - `inst-ca-subscribe-22`
9. [ ] - `p1` - Ensure the gear's one anonymous consumer group in this process on the activation topic, created under the subscriber's identity with the broker's `define` grant and shared by every handle of the gear, keeping offsets in memory from the latest position - `inst-ca-subscribe-23`
10. [ ] - `p1` - Join that group with one interest for `tenant` unless it holds one — the `change_notification` type with `tenant_id` exactly `tenant`, since the broker matches interests by exact tenant - `inst-ca-subscribe-24`
11. [ ] - `p1` - **IF** no broker is resolvable → the registered keys stay `registered`, and the group and the join are retried with backoff; the delivery loop catches up from the `awaiting` states once they succeed - `inst-ca-subscribe-25`
12. [ ] - `p1` - **IF** the broker refuses the group or the join for a missing `define` or `consume` grant → move the handle's registered keys to `pending` with their subscriptions kept, log at `warn`, and retry the join with backoff against the same group until an operator grants, the keys returning to `registered` once it succeeds - `inst-ca-subscribe-26`
13. [ ] - `p1` - **WHILE** the handle is live **AND** a key is `pending` → retry that key's outstanding step with backoff, and log at `warn` once each minute it stays `pending`, naming the key and the reason - `inst-ca-subscribe-27`
14. [ ] - `p1` - **IF** a retry commits a pending registration → read the key fresh as in `inst-ca-subscribe-20`, mark it `registered`, and invoke the handler with the fresh values as a bundle without `change_ref`, which owes no report - `inst-ca-subscribe-28`
15. [ ] - `p1` - **IF** the caller drops the handle → the handle is no longer live: stop its pending retries and retire nothing; a `subscribe` the caller repeats returns a new handle over the same subscriptions - `inst-ca-subscribe-29`
16. [ ] - `p1` - **IF** the handle was the gear's last live handle in the process, whether dropped or retired → leave the gear's consumer group and delete it best-effort, as the retire flow does (`inst-ca-retire-11`) - `inst-ca-subscribe-30`

### Record the Activation State with a Change

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-consumer-activation-record`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- Each committed value change writes, in its own transaction, one `awaiting` activation state per subscription it affects, with the expected value at the subscription's tenant and the change's audit record id as `change_ref`; a subscription it does not affect is untouched
- A registration, a retire or a restriction change of the same key waits for the change in flight, or the change waits for it; changes of different keys never wait for each other
- The pairs of subscriber and tenant the request touched are handed to the end-of-request publish

**Error Scenarios**:
- The key's mutation lock is not acquired within the budget: the change answers `Unavailable` — `503` on a single-value route, per item in `SetResult[]` — with nothing stored
- The Tenant Resolver cannot answer a subscription tenant's chain: the write fails closed with nothing stored
- A fallback or staged plaintext fetch fails — its per-setting authorization decision denied or undecidable, the store read or its `secret_use` record failing: the change is refused `503` with nothing of it stored; a `secret_use` record already written stays
- The transaction rolls back — a stale tag, a retired declaration, a failed audit append — or its commit fails: nothing is stored, no activation state is written, and the lock is released

**Steps**: steps 18 and 19 (`inst-ca-record-27`, `inst-ca-record-28`) are standing rules for other triggers — restriction changes and declaration lifecycle changes — not actions of this pass.

1. [x] - `p1` - A value change of entry 2.8 — a single `set`, a `set` or `revert` entry of a batch, a `revert`, a `remove_value` or a `clone` — that passed its write gates reaches the commit of `cpt-cf-settings-service-algo-value-writes-commit` - `inst-ca-record-1`
2. [x] - `p1` - **IF** its validation (`inst-vw-commit-1`), its early `If-Match` judgement, which `inst-vw-commit-4` makes for every change, or, for an inline secret, its Credential Store put of the same step refused it → **RETURN** that refusal as entry 2.8 defines it, without contending for the lock - `inst-ca-record-2`
3. [x] - `p1` - Take the key's mutation lock (`cpt-cf-settings-service-algo-consumer-activation-lock`) within `mutation_lock_wait_seconds`, in a batch further capped by the remainder of the request deadline - `inst-ca-record-3`
4. [x] - `p1` - **IF** the wait is exhausted → **RETURN** the change refused `Unavailable` with nothing stored; a batch still attempts its later changes while time remains, and an inline secret's stored entry is released at once, as any refused commit's is, the `pending_secrets` sweep remaining the backstop - `inst-ca-record-4`
5. [x] - `p1` - Invoke `resolve_subscribers` for the key and the written scope `X`: DB: SELECT the key's subscriptions once, through `idx_sub_key`, and keep that read set for the rest of the change - `inst-ca-record-5`
6. [x] - `p1` - Qualify the read set by the key's Scope Class: every subscription for `global`; for `local`, those whose tenant `T` is `X`; for `cascading`, those whose tenant `T` is `X` or a descendant of `X` - `inst-ca-record-6`
   1. [x] - `p1` - For `cascading`, ask the Tenant Resolver for each subscription tenant's ancestor chain, which answers whether `X` is `T` or an ancestor of `T`, barriers ignored as runtime resolution ignores them, with no subtree enumerated; resolving the distinct subscription tenants concurrently, at most eight at a time; keep each qualifying tenant's chain for the transaction, and skip, with one `warn` naming the subscriber, key and tenant, a subscription whose tenant the Tenant Resolver reports as not existing, which cannot qualify - `inst-ca-record-7`
   2. [x] - `p1` - **IF** the Tenant Resolver cannot answer for a tenant (any failure other than "the tenant does not exist") → release the lock and **RETURN** the change refused, failing closed with nothing stored - `inst-ca-record-8`
7. [x] - `p1` - **IF** the change is a `revert` or a `remove_value` of a `secret`-trait key **AND** at least one subscription qualified → read the value `X` falls back to once its override is gone through the Value Resolver's freshness form outside a transaction, never from the effective-value cache - `inst-ca-record-9`
   1. [x] - `p1` - **IF** that fallback is a stored secret → have the Secret Manager fetch its plaintext once through `resolve_plaintext` under the service's own identity — the per-setting authorization decision for that identity first, then the store read — writing one `secret_use` record attributed to that identity, which stays if the change later rolls back - `inst-ca-record-10`
   2. [x] - `p1` - **IF** that fallback is the declaration's default → take the default's value, with no fetch - `inst-ca-record-11`
8. [x] - `p1` - **IF** the change is a batch `set` adopting a staged secret **AND** at least one subscription qualified → have the Secret Manager fetch the staged entry's plaintext once by its reference, under the same identity, with the same per-setting authorization decision and one `secret_use` record, exactly as for a fallback - `inst-ca-record-12`
9. [x] - `p1` - **IF** a fetch's per-setting authorization decision is a denial or cannot be obtained, **OR** its store read fails, **OR** its `secret_use` record cannot be written → release the lock and **RETURN** the change refused `503`, nothing of the change stored - `inst-ca-record-13`
10. [x] - `p1` - Hold any plaintext — an inline secret's from the request, a fetched one from its fetch — only in the Secret Manager's redacting, zeroizing wrapper until it is hashed, and never log it - `inst-ca-record-14`
11. [x] - `p1` - Mint the id of the change's audit record in its audit draft before the append, so the append writes the record under it and `record_change` receives it as the change's `change_ref` - `inst-ca-record-15`
12. [x] - `p1` - Open the change's transaction at read committed and run entry 2.8's steps that write the value row and append its audit record, in this order: `inst-vw-commit-2`, `inst-vw-commit-3`, `inst-vw-commit-5`, `inst-vw-commit-6` - `inst-ca-record-16`
13. [x] - `p1` - Invoke `record_change` in that transaction with the change, its pre-image, its audit record's id, the qualified subscriptions with their chains and any plaintext - `inst-ca-record-17`
   1. [x] - `p1` - Keep the affected subscriptions (`cpt-cf-settings-service-algo-consumer-activation-affected`), each with the effective value at its tenant after the change, resolved through the transaction and bypassing the cache - `inst-ca-record-18`
   2. [x] - `p1` - Encode each affected subscription's expected value (`cpt-cf-settings-service-algo-consumer-activation-expected`) - `inst-ca-record-19`
   3. [x] - `p1` - DB: replace the `activation_state` rows of the affected, still-subscribed triples — a DELETE of them followed by one INSERT … SELECT from `subscriptions` restricted to the ids of the affected subscriptions in the read set, in the change's transaction, the key's mutation lock being the only serialisation of the two statements (any future writer of `activation_state` must take it), and the guarded UPDATEs of a report or a stamp skipping a replaced tuple exactly as they skip a changed `change_ref` — with status `awaiting`, the encoded `expected_value`, `change_ref` the audit record's id, `applied_value`, `detail`, `reported_at` and `last_notified_at` cleared, `updated_at` now, whatever the previous status - `inst-ca-record-20`
   4. [x] - `p1` - A subscription of the read set retired before the state write gets no row, since the insert-select finds no subscription for it - `inst-ca-record-21`
   5. [x] - `p1` - Remember, for the request, each pair of subscriber and tenant the state write wrote - `inst-ca-record-22`
14. [x] - `p1` - Commit the change's transaction (`inst-vw-commit-7`) - `inst-ca-record-23`
15. [x] - `p1` - **IF** the transaction rolled back at any step above **OR** the commit failed → nothing of the change and no activation state is stored, and the pairs it remembered are forgotten; release the lock and **RETURN** the refusal entry 2.8 defines - `inst-ca-record-24`
16. [x] - `p1` - Evict the local cache for the change (`inst-vw-commit-8`) - `inst-ca-record-25`
17. [x] - `p1` - Release the lock and await the release before the change answers - `inst-ca-record-26`
18. [x] - `p1` - Standing rule for restriction changes: a restriction change of entry 2.7 takes the same lock after its authorization, target and visibility checks and before its transaction, holds it around the transaction that upserts or deletes the restriction row, and writes no activation state; its exhausted wait answers `503` with nothing stored - `inst-ca-record-27`
19. [x] - `p1` - Standing rule for declaration lifecycle changes: a retire, reactivate, evolve or update of a declaration, and a reconcile pass of contributed declarations, takes no mutation lock, since it changes no stored value - `inst-ca-record-28`
20. [x] - `p1` - **RETURN** the change's outcome to its request; the pairs it remembered feed the end-of-request publish - `inst-ca-record-29`

### Publish Notifications at the End of a Request

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-consumer-activation-publish`

**Actor**: `cpt-cf-settings-service-actor-event-broker`

**Success Scenarios**:
- A request whose changes affected subscriptions hands the broker one `change_notification` per pair of subscriber and tenant it touched, after its commits and after its `cache_invalidate` broadcasts, each carrying every `awaiting` key of that pair with its `change_ref`
- Each bundled state's `last_notified_at` is stamped once the broker accepts its notification
- The caller's response is the same whatever happened to the notifications

**Error Scenarios**:
- No broker resolvable, the activation types not registered, or the root tenant's id unknown: nothing is published and the delivery loop covers every state
- A publish refused — a missing `produce` grant included — or not accepted within `publish_timeout_seconds`: it is dropped, logged at `warn`, the request's remaining publishes are skipped, and the states stay `awaiting` for the delivery loop

**Steps**:
1. [x] - `p1` - The Value Writer's end-of-request publish hook reaches this flow after every change of the request committed or was refused, and after the replica coherence publish (`cpt-cf-settings-service-flow-replica-coherence-publish`) handed its broadcasts to the broker - `inst-ca-publish-1`
2. [x] - `p1` - **IF** the request remembered no pair of subscriber and tenant → **RETURN** with nothing published - `inst-ca-publish-2`
3. [x] - `p1` - **IF** the hook resolved no broker for the request, **OR** the activation topic and the `change_notification` type are not yet registered (`cpt-cf-settings-service-flow-consumer-activation-lifecycle`), **OR** the root tenant's id is not yet known → publish nothing and **RETURN**; the states stay `awaiting` for the delivery loop - `inst-ca-publish-3`
4. [x] - `p1` - **IF** an earlier publish of the same request — a `cache_invalidate` broadcast included — was refused or timed out → publish nothing and **RETURN**, so the request pays at most one `publish_timeout_seconds` - `inst-ca-publish-4`
5. [x] - `p1` - **FOR EACH** remembered pair, in the order the request first touched it - `inst-ca-publish-5`
   1. [x] - `p1` - DB: SELECT the pair's `activation_state` rows with status `awaiting` through `idx_activation_awaiting` — every awaiting key of the pair, not only those this request changed - `inst-ca-publish-6`
   2. [x] - `p1` - Build the pair's notifications (`cpt-cf-settings-service-algo-consumer-activation-bundle`) - `inst-ca-publish-7`
   3. [x] - `p1` - Wrap each in the broker-native envelope: `type_id` the `change_notification` type, `tenant_id` the subscription's tenant, `subject` that tenant, `subject_type` the activation subject type, `source` the gear name `settings-service`, an `id` unique to the event, `occurred_at` the time of publishing, `trace_parent` from the current span when one is present, and the body as `data` - `inst-ca-publish-8`
   4. [x] - `p1` - Publish each through `publish_notification` under the publishing identity of entry 2.12, waiting at most `publish_timeout_seconds` for the broker to accept it - `inst-ca-publish-9`
   5. [x] - `p1` - **IF** the broker accepted it → DB: UPDATE `activation_state` setting `last_notified_at` now on each bundled row that still names the `change_ref` it was bundled with, so a row a later change overwrote meanwhile stays unstamped - `inst-ca-publish-10`
   6. [x] - `p1` - **ELSE** → drop the publish, log at `warn` with the subscriber, the tenant and the broker's reason, and skip the request's remaining publishes; the rows stay `awaiting` - `inst-ca-publish-11`
6. [x] - `p1` - **RETURN** to the request, which answers its caller with the outcomes of its changes whatever the notifications' outcomes - `inst-ca-publish-12`

### Deliver the Awaiting States

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-consumer-activation-deliver`

**Actor**: `cpt-cf-settings-service-actor-event-broker`

**Success Scenarios**:
- One pass re-publishes every `awaiting` state whose notification was lost, refused, skipped or not yet answered once `redeliver_interval_seconds` has passed since `last_notified_at`, and at once when an overwrite cleared it
- Exactly one replica scans per tick; the others skip the pass without waiting
- States whose subscription no longer exists are removed by one conditional delete, whatever their status

**Error Scenarios**:
- The scan lock held by another replica: this replica's pass ends at once
- A publish refused or timed out: the rows stay `awaiting` and are due again at the next interval
- The broker lost during the pass: the rest of the pass is abandoned and the states are kept
- `stop` during a pass: the pass finishes or is abandoned within the shutdown deadline, and every `awaiting` state is kept

**Steps**:
1. [x] - `p1` - The delivery loop (`cpt-cf-settings-service-flow-consumer-activation-lifecycle`) starts one pass on a tick on which the broker resolved, the root tenant's id is known and the activation topic and the `change_notification` type are registered - `inst-ca-deliver-1`
2. [x] - `p1` - Make one non-blocking `try_lock` on the gear's scan lock through `toolkit-db`'s `LockManager` - `inst-ca-deliver-2`
3. [x] - `p1` - **IF** the scan lock is not acquired → **RETURN** from the pass without waiting: another replica scans this tick - `inst-ca-deliver-3`
4. [x] - `p1` - DB: DELETE in one conditional statement every `activation_state` row whose subscription no longer exists, whatever its status, leaving every row whose subscription exists untouched - `inst-ca-deliver-4`
5. [x] - `p1` - DB: SELECT the `activation_state` rows with status `awaiting` that are due — `last_notified_at` absent or older than `redeliver_interval_seconds` — through `idx_activation_awaiting`, grouped per pair of subscriber and tenant - `inst-ca-deliver-5`
6. [x] - `p1` - **FOR EACH** pair - `inst-ca-deliver-6`
   1. [x] - `p1` - Build the pair's notifications (`cpt-cf-settings-service-algo-consumer-activation-bundle`) - `inst-ca-deliver-7`
   2. [x] - `p1` - Wrap each in the envelope of `inst-ca-publish-8` - `inst-ca-deliver-8`
   3. [x] - `p1` - Publish each through `publish_notification` under the publishing identity of entry 2.12, waiting at most `publish_timeout_seconds` for the broker to accept it - `inst-ca-deliver-9`
   4. [x] - `p1` - **IF** the broker accepted it → DB: UPDATE `last_notified_at` to now on each bundled row that still names the `change_ref` the scan read, so a row overwritten during the scan keeps it absent - `inst-ca-deliver-10`
   5. [x] - `p1` - **IF** the broker accepted it → count one on `settings_notify_redeliveries_total` for the notification - `inst-ca-deliver-16`
   6. [x] - `p1` - **ELSE** → log at `warn` with the subscriber, the tenant and the broker's reason (the first refusal of the pass; later ones at `debug`), and leave the rows `awaiting` until the next due tick; a publish that timed out, or the third refusal in a row, ends the pass as in the broker-lost step but reporting the broker stalled - `inst-ca-deliver-11`
   7. [x] - `p1` - **IF** the broker is lost during the pass — a publish fails because the broker is gone → abandon the rest of the pass, release the scan lock, and **RETURN** reporting the broker lost - `inst-ca-deliver-12`
   8. [x] - `p1` - **IF** the cancellation token fires during the pass → finish the publish in flight or abandon it within the runtime's shutdown deadline, keep every `awaiting` state, release the scan lock, and **RETURN** - `inst-ca-deliver-13`
7. [x] - `p1` - Release the scan lock and await the release - `inst-ca-deliver-14`
8. [x] - `p1` - **RETURN** with the pass complete - `inst-ca-deliver-15`

### React to a Notification

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-consumer-activation-react`

**Actor**: `cpt-cf-settings-service-actor-internal-caller`

**Success Scenarios**:
- The handler receives only its own subscriber's notifications, with every notified key re-read fresh at the notification's tenant, and the outcomes it returns are reported
- A repeated notification is re-applied and re-reported like the first

**Error Scenarios**:
- A notification for another subscriber of the same tenant is dropped silently
- A malformed delivery is dropped and logged at `warn`
- A notified key whose fresh re-read fails, or answers `Retired` or `NotFound`, is left out of the handler's bundle and reports nothing; its state stays `awaiting`, delivered again, until the next change of the key or a retire

**Steps**:
1. [ ] - `p1` - The Event Broker delivers an event of the activation topic to the gear's consumer group in this process through one of its tenant interests - `inst-ca-react-1`
2. [ ] - `p1` - Continue the publisher's trace from `trace_parent` when the event carries one - `inst-ca-react-14`
3. [ ] - `p1` - At receipt, before the malformed and foreign-subscriber drops below, observe `settings_notify_publish_to_deliver_seconds` with `event` `change_notification` from the envelope's `occurred_at` to the delivery, so a dropped delivery is observed too - `inst-ca-react-2`
4. [ ] - `p1` - **IF** the event's `type_id` is not the `change_notification` type, **OR** `data.tenant` is not a UUID, **OR** `data.subscriber` is absent, **OR** `data.changes` is not a list of `key` and `change_ref` pairs → log at `warn` with the event's `id`, drop the delivery and **RETURN** - `inst-ca-react-3`
5. [ ] - `p1` - **IF** `data.subscriber` is not this gear's subscriber → drop the delivery without a log line and **RETURN**, since every process with an interest in a tenant receives that tenant's notifications for its other subscribers - `inst-ca-react-4`
6. [ ] - `p1` - Ignore any attribute of `data` this version does not know, and the optional subject pair, absent in v1 - `inst-ca-react-5`
7. [ ] - `p1` - **FOR EACH** notified key that a live handle of the gear for `data.tenant` holds → re-read it at `data.tenant` through the fresh read (`cpt-cf-settings-service-algo-consumer-activation-fresh-read`), one store read per key - `inst-ca-react-6`
   1. [ ] - `p1` - **IF** the re-read fails, **OR** it answers `Retired` or `NotFound` → treat it as a failed re-read: leave the key out of the bundle and report nothing for it; the state stays `awaiting`, delivered again, until the next change of the key or a retire - `inst-ca-react-7`
8. [ ] - `p1` - A notified key that no live handle of the gear for `data.tenant` holds is outside that loop: it is not re-read, owes nothing until a handle for it exists, and its state stays `awaiting` - `inst-ca-react-8`
9. [ ] - `p1` - Invoke the handler with the bundle — `data.tenant` and each re-read key with its `change_ref` — and the fresh values, a `SecretHandle` for a `secret`-trait key - `inst-ca-react-9`
10. [ ] - `p1` - **FOR EACH** outcome the handler returns → report it through `cpt-cf-settings-service-flow-consumer-activation-report`, which encodes its applied value - `inst-ca-react-10`
11. [ ] - `p1` - A key the handler returns no outcome for is reported later by the consumer through `report_outcome`, still naming the `change_ref` it was notified with - `inst-ca-react-11`
12. [ ] - `p1` - A repeat of a notification already handled takes the same path — re-read, re-apply, re-report — and is never suppressed - `inst-ca-react-12`
13. [ ] - `p1` - **RETURN** completed: the group's offset advances in memory and nothing is acknowledged to the broker - `inst-ca-react-13`

### Report an Outcome

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-consumer-activation-report`

**Actor**: `cpt-cf-settings-service-actor-internal-caller`

**Success Scenarios**:
- A report on an `awaiting` state settles it `succeeded` or `failed`, appends one `activation` audit record in the same transaction, and answers `applied`
- A report for no state, a terminal state, a stale `change_ref`, or a state settled or replaced concurrently answers `ignored`, changes nothing and writes no audit record

**Error Scenarios**:
- The audit append or the commit fails: the transaction rolls back, nothing changes, and the call answers an error; the consumer may repeat it, since a report is idempotent per subscriber, key, tenant and `change_ref`, and the SDK never retries it
- Malformed input — a tenant or a `change_ref` that is not a UUID, a status that is neither `success` nor `failed`, a `key` holding a NUL byte or longer than 1 KiB, or a `detail` or `applied_value` holding a NUL byte: an invalid-argument error, nothing changed

**Steps**:
1. [ ] - `p1` - The consuming gear calls `report_outcome` with `change_ref`, `tenant`, `key`, `status`, `applied_value` and an optional `detail` — directly, or through the outcomes its handler returned (`inst-ca-react-10`) - `inst-ca-report-1`
2. [ ] - `p1` - On both entry paths, before calling the Recorder, the SDK encodes `applied_value` as the expected-value algorithm does (`cpt-cf-settings-service-algo-consumer-activation-expected`): RFC 8785 canonical JSON, or the secret-value hash computed in the consumer's process for a `secret`-trait key - `inst-ca-report-2`
3. [ ] - `p1` - The SDK reaches the Activation Outcome Recorder's `receive_response` by a direct call in process, and through its remote binding under the out-of-process profiles (DESIGN §4.5) — never a REST route or a broker event - `inst-ca-report-3`
4. [x] - `p1` - Take the subscriber from the caller's security context - `inst-ca-report-4`
5. [x] - `p1` - **IF** `tenant` or `change_ref` is not a UUID, **OR** `status` is neither `success` nor `failed`, **OR** `key` holds a NUL byte or exceeds 1 KiB, **OR** `detail` or `applied_value` holds a NUL byte → **RETURN** an invalid-argument error with nothing changed, counted on no metric of this flow - `inst-ca-report-5`
6. [x] - `p1` - Normalise `tenant` to a bare lowercase UUID string, truncate `detail` to 1 KiB, and treat an `applied_value` above the value size cap as a value that does not match, storing none - `inst-ca-report-6`
7. [x] - `p1` - Open one transaction at read committed; DB: SELECT the `activation_state` row of the subscriber, `key` and `tenant`; every `ignored` return below ends this transaction with nothing written - `inst-ca-report-7`
8. [x] - `p1` - **IF** no row exists → count `ignored` on `settings_notify_reports_total` and **RETURN** `ignored`: the subscription was retired, or never registered - `inst-ca-report-8`
9. [x] - `p1` - **IF** the row is `succeeded` or `failed` → count `ignored` on `settings_notify_reports_total` and **RETURN** `ignored`: a report never moves a terminal row, and only the next change resets it - `inst-ca-report-9`
10. [x] - `p1` - Apply the outcome rules (`cpt-cf-settings-service-algo-consumer-activation-outcome`) to the `awaiting` row; **IF** they answer `ignored` → count `ignored` on `settings_notify_reports_total` and **RETURN** `ignored` - `inst-ca-report-10`
11. [x] - `p1` - DB: UPDATE the row's `status`, `applied_value`, `detail`, `reported_at` now and `updated_at` now, guarded on the row still naming the `change_ref` the decision read and still being `awaiting` - `inst-ca-report-11`
12. [x] - `p1` - **IF** the update matched no row → count `ignored` on `settings_notify_reports_total` and **RETURN** `ignored` with no audit record: a concurrent report settled the row, or a concurrent change replaced its expectation and its own notification follows - `inst-ca-report-12`
13. [x] - `p1` - Append one `activation` audit record through `cpt-cf-settings-service-algo-audit-store-append` in the same transaction: `resource` the key at the row's tenant from the shared formatter, `declaration_key` the key, `tenant_id` the row's tenant as a UUID, `actor` the subscriber with `actor_classification` `public`, `outcome` `success` or `failure` matching the new status, `detail` the truncated consumer detail on failure, a freshly minted `request_id`, `change_ref` the row's, and no pre-value or post-value - `inst-ca-report-13`
14. [x] - `p1` - **IF** the append fails → roll back and **RETURN** an `Unavailable` error with nothing changed, counted on no metric of this flow - `inst-ca-report-14`
15. [x] - `p1` - Commit the transaction - `inst-ca-report-15`
16. [x] - `p1` - **IF** the commit fails → **RETURN** an `Unavailable` error with nothing changed, counted on no metric of this flow - `inst-ca-report-16`
17. [x] - `p1` - Count `settings_notify_reactions_total` with the new status as `outcome` - `inst-ca-report-17`
18. [x] - `p1` - Observe `settings_notify_response_latency_seconds` from the row's `updated_at` as the change wrote it - `inst-ca-report-20`
19. [x] - `p1` - Count `applied` on `settings_notify_reports_total` - `inst-ca-report-18`
20. [x] - `p1` - **RETURN** `applied` - `inst-ca-report-19`

### Retire a Subscription

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-consumer-activation-retire`

**Actor**: `cpt-cf-settings-service-actor-internal-caller`

**Success Scenarios**:
- `retire()` on a handle deletes each of its keys' subscriptions together with their activation states, reports nothing for them, and a late report for a deleted state is `ignored`
- A retire racing a value write of the key waits for the write's commit, then deletes the state the write committed
- When the gear's last live handle in the process is gone — dropped or retired — the process's consumer group is left and deleted

**Error Scenarios**:
- A key with no subscription of the caller's at that tenant — another subscriber's included: `404` for that key, nothing of the other subscriber's deleted
- The key's lock not acquired within the budget: that key answers `Unavailable`, stays subscribed and keeps its state on the handle and its pending retries; the SDK does not retry the retire, and the caller may call `retire()` again
- The group cannot be deleted: logged at `warn`, the group stays in the broker as a crashed process's does

**Steps**:
1. [ ] - `p1` - The consuming gear calls `retire()` on a handle explicitly; dropping a handle never retires anything - `inst-ca-retire-1`
2. [ ] - `p1` - Stop the pending retries only of the keys whose retire below answers `retired` or `404`, each as it is answered; a key answered `Unavailable` keeps its state on the handle and its pending retries - `inst-ca-retire-2`
3. [x] - `p1` - **FOR EACH** key of the handle → invoke `retire_subscription` with the subscriber from the SDK `Context`, the key and the handle's tenant - `inst-ca-retire-3`
   1. [x] - `p1` - Take the key's mutation lock (`cpt-cf-settings-service-algo-consumer-activation-lock`) - `inst-ca-retire-4`
   2. [x] - `p1` - **IF** the wait is exhausted → answer `Unavailable` for the key with nothing deleted, the key staying subscribed, and continue with the next key - `inst-ca-retire-5`
   3. [x] - `p1` - DB: SELECT the subscription of the subscriber, the key and the tenant; **IF** none → release the lock, answer `404` for the key, and continue with the next key - `inst-ca-retire-6`
   4. [x] - `p1` - DB: DELETE that subscription and its `activation_state` row in one transaction; nothing is reported for the deleted row - `inst-ca-retire-7`
   5. [x] - `p1` - Commit the transaction - `inst-ca-retire-8`
   6. [x] - `p1` - Release the lock and await the release - `inst-ca-retire-15`
   7. [x] - `p1` - Log the retire at `info` with the subscriber, the key and the tenant - `inst-ca-retire-16`
   8. [x] - `p1` - Answer `retired` for the key - `inst-ca-retire-9`
4. [ ] - `p1` - **IF** every key was answered `retired` or `404` → the handle is retired and no longer live; otherwise the handle stays live for each key answered `Unavailable`, which keeps its state and its pending retries, so the caller may call `retire()` again - `inst-ca-retire-10`
5. [ ] - `p1` - **IF** no live handle of the gear remains in the process — every handle it was given has been dropped or retired → leave the gear's consumer group and delete it best-effort; until then the group keeps every tenant interest it joined - `inst-ca-retire-11`
6. [ ] - `p1` - On the gear's `stop`, leave and delete the group best-effort within the runtime's shutdown deadline - `inst-ca-retire-12`
7. [ ] - `p1` - **IF** the leave or the deletion fails, or the deadline passes → log at `warn`; the group stays in the broker exactly as a crashed process's does, holding no delivery a live process needs - `inst-ca-retire-13`
8. [x] - `p1` - **RETURN** per key `retired`, `404` or `Unavailable`; the SDK retries none of them - `inst-ca-retire-14`

### Run the Activation Half of the Broker Lifecycle

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-consumer-activation-lifecycle`

**Actor**: `cpt-cf-settings-service-actor-event-broker`

**Success Scenarios**:
- Gear init completes whether or not a broker exists; the delivery loop ticks on every replica, refreshing the activation gauges on every tick, and once a broker resolves registers the activation topic and the `change_notification` type and runs one delivery pass per tick
- A broker that appears later, or returns after a loss, is picked up on a later tick, and delivery catches up from the `awaiting` states

**Error Scenarios**:
- A `redeliver_interval_seconds` or a `mutation_lock_wait_seconds` of `0` or above its ceiling (86400 and 3600): gear init refuses the configuration
- No broker resolvable, or the root tenant's id unknown: the tick idles after refreshing the gauges; writes commit and their states stay `awaiting`
- The registration refused: logged at `warn` and retried on the next tick; publishes and the scan stay off meanwhile
- The gear's `produce` grant missing: each publish is refused and logged at `warn`, the states stay `awaiting`, and the gear keeps serving
- The broker lost during a pass, or `stop` during one: the pass is abandoned or finished within the shutdown deadline, and every `awaiting` state is kept

**Steps**: steps 9 and 10 (`inst-ca-lifecycle-16`, `inst-ca-lifecycle-17`) are standing rules — for every publish of this feature and for the consumer of entry 2.12 — not actions of this loop's pass. Every publish of this feature runs under the publishing identity of entry 2.12, which holds the `produce` grants this feature adds on the activation topic.

1. [x] - `p1` - At gear init, read `redeliver_interval_seconds`, a whole number of seconds from `1` to `86400` (one day), default `30` - `inst-ca-lifecycle-1`
2. [x] - `p1` - Read `mutation_lock_wait_seconds`, a whole number of seconds from `1` to `3600` (one hour), default `10`; a budget above the platform request deadline is accepted and documented as a misconfiguration, since the batch cap by the remainder of the request deadline enforces the deadline - `inst-ca-lifecycle-2`
3. [x] - `p1` - **IF** either is `0` or above its ceiling → refuse the configuration and fail init; the shared `publish_timeout_seconds` is read and validated by entry 2.12 (`inst-rc-lifecycle-1`) - `inst-ca-lifecycle-3`
4. [x] - `p1` - **IF** the gear runs the single-process SQLite profile → at init remove the lock markers a crash may have left, through `remove_lock_marker_at_startup`, for the scan lock and for the mutation lock of every declared key; in every other profile never - `inst-ca-lifecycle-4`
5. [x] - `p1` - Init neither resolves the broker nor waits for it, and the broker is never in the gear's `deps` - `inst-ca-lifecycle-5`
6. [x] - `p1` - On `start`, run the delivery loop on every replica as a `stateful` background task of the gear's lifecycle under the runtime's cancellation token, and log its start at `info` - `inst-ca-lifecycle-6`
7. [x] - `p1` - **WHILE** the cancellation token has not fired, run a pass at start and then once every `redeliver_interval_seconds` — this loop owns the tick, and its retries ride on it - `inst-ca-lifecycle-7`
   1. [x] - `p1` - DB: count the `subscriptions` rows and the `activation_state` rows with status `awaiting`, and set `settings_notify_subscriptions` and `settings_activation_awaiting` from them, whether or not a broker resolves - `inst-ca-lifecycle-8`
   2. [x] - `p1` - Resolve `EventBrokerApi` from the `ClientHub` with a lookup that answers "none" rather than failing; **IF** none → idle: end this tick; the next tick starts after the interval - `inst-ca-lifecycle-9`
   3. [x] - `p1` - Read the root tenant's id from the cache the replica's consumer task fills (`inst-rc-lifecycle-4`); **IF** it is not yet known → idle: end this tick; the next tick starts after the interval; the delivery loop never resolves the root tenant itself - `inst-ca-lifecycle-10`
   4. [x] - `p1` - **IF** the activation topic and the `change_notification` type are not yet registered → register the topic, and the type as a derived event schema of the broker's event base that names the activation subject type as its allowed subject type and keeps the default partition key `tenant_id`, idempotently - `inst-ca-lifecycle-11`
   5. [x] - `p1` - **IF** the registration is refused or fails → log at `warn` and end this tick; the next tick starts after the interval and registers again - `inst-ca-lifecycle-12`
   6. [x] - `p1` - **IF** the registration succeeds → record it, so the publish and the scan stop skipping for that reason; a publish refused within the broker's specification-refresh interval after the first registration is retried like any refusal - `inst-ca-lifecycle-19`
   7. [x] - `p1` - Run one pass of `cpt-cf-settings-service-flow-consumer-activation-deliver` - `inst-ca-lifecycle-13`
   8. [x] - `p1` - **IF** the pass reports the broker lost → log at `warn`, keep every `awaiting` state, and end this tick; the next tick starts after the interval and resolves the broker again - `inst-ca-lifecycle-14`
8. [x] - `p1` - On `stop`, the token having fired, let the pass in flight finish or abandon it within the runtime's shutdown deadline (`inst-ca-deliver-13`), keeping every `awaiting` state - `inst-ca-lifecycle-15`
9. [x] - `p1` - Standing rule for every publish of this feature: it runs under the publishing identity of entry 2.12, to which this feature adds the grants needed on the activation topic — `produce` on the activation topic, the `change_notification` type and the activation subject type for events of every tenant; a missing grant refuses each publish, logged at `warn`, leaving the gear up and the states `awaiting` - `inst-ca-lifecycle-16`
10. [x] - `p1` - Standing rule for the consumer of entry 2.12 (`cpt-cf-settings-service-flow-replica-coherence-consume`, at `inst-rc-consume-1`): on each `cache_invalidate` delivery it observes `settings_notify_publish_to_deliver_seconds` with `event` `cache_invalidate` from the envelope's `occurred_at` to the delivery, at receipt and before its malformed drop, the one addition this feature makes to that flow - `inst-ca-lifecycle-17`
11. [x] - `p1` - **RETURN** with the loop stopped, logged at `info` - `inst-ca-lifecycle-18`

## 3. Processes / Business Logic (CDSL)

### Key Mutation Lock

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-consumer-activation-lock`

**Input**: A setting key, the mutation to run under its lock — a value change, a restriction change, a subscription registration or a subscription retire — and the wait budget: `mutation_lock_wait_seconds`, in a batch further capped by the remainder of the request deadline

**Output**: The mutation's own result with the lock released, or `Unavailable` with nothing of the mutation done

**Steps**:
1. [x] - `p1` - Name the lock from the gear name and the setting key, so one key has one lock across every replica and two keys never share one - `inst-ca-lock-1`
2. [x] - `p1` - Take it only after the mutation's authorization and validation passed, so a refused mutation never contends for it - `inst-ca-lock-2`
3. [x] - `p1` - Attempt `try_lock` through `toolkit-db`'s `LockManager`; **WHILE** it is not acquired **AND** the budget lasts → wait on the lock's own retry schedule — a first wait of tens of milliseconds, doubling, capped at one second, with jitter, bounded by the budget and injectable in tests, separate from the 1 s to 30 s backoff of the broker — and attempt again; waiters are not served in order - `inst-ca-lock-3`
4. [x] - `p1` - **IF** the budget is exhausted → count `exhausted` on `settings_mutation_lock_total` and observe the wait on `settings_mutation_lock_wait_seconds` - `inst-ca-lock-4`
5. [x] - `p1` - **IF** the budget is exhausted → **RETURN** `Unavailable` with nothing of the mutation done - `inst-ca-lock-11`
6. [x] - `p1` - Count `acquired` on `settings_mutation_lock_total` and observe the wait on `settings_mutation_lock_wait_seconds` - `inst-ca-lock-5`
7. [x] - `p1` - Hold the lock of one key at a time: a mutation never takes a second key's lock while it holds one, so the locks cannot deadlock - `inst-ca-lock-6`
8. [x] - `p1` - Run the mutation under it — a value change from its candidate step until its transaction rolled back, or committed and the local cache is evicted; a restriction change, a registration or a retire around its transaction - `inst-ca-lock-7`
9. [x] - `p1` - Release the lock and await the release before the mutation returns - `inst-ca-lock-8`
10. [x] - `p1` - **IF** the unlock fails (`NotHeld`, a database error) → log at `warn` with the key, and count `error` on `settings_mutation_lock_total`; a lost lock session is silent to the holder (`toolkit-db` reports it as already released) — the residual DESIGN-activation §6 records - `inst-ca-lock-9`
11. [x] - `p1` - **RETURN** the mutation's result; the guarantee rests on the lock, not on the isolation level, and it holds only while the settings database is reached through session pooling or a direct connection - `inst-ca-lock-10`

### Affected Subscriptions

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-consumer-activation-affected`

**Input**: One change — its key, Scope Class and `secret` trait, the written scope `X`, the operation and its pre-image — the subscriptions qualified under the lock with, for a `cascading` key, each subscription tenant's ancestor chain, and the change's open transaction

**Output**: The affected subscriptions, each with the effective value at its tenant after the change

**Steps**:
1. [x] - `p1` - **FOR EACH** qualified subscription of tenant `T` - `inst-ca-affected-1`
   1. [x] - `p1` - **IF** the key is `cascading` **AND** `T` is not `X` → DB: SELECT, in the transaction, the overrides of the key at the tenants of `T`'s chain from `T` up to but excluding `X`; **IF** one exists → exclude the subscription, since a nearer override shadows the change - `inst-ca-affected-2`
   2. [x] - `p1` - Resolve the effective value at `T` after the change through the Value Resolver's in-transaction form — reading through the transaction over `T`'s chain for a `cascading` key, bypassing and never populating the effective-value cache, calling no resolver; a `global` key resolves at the root and a `local` key at `X` - `inst-ca-affected-3`
   3. [x] - `p1` - Take the value before the change at `T` from the change's pre-image when `X` held an override, or from the value `X` inherited when the change creates a new override, read in the same transaction over the same chain - `inst-ca-affected-4`
   4. [x] - `p1` - **IF** the key has the `secret` trait → the subscription is affected, since the pre-image holds only a reference and every write counts - `inst-ca-affected-5`
   5. [x] - `p1` - **ELSE IF** the value after the change differs from the value before it in their canonical encoding → the subscription is affected - `inst-ca-affected-6`
   6. [x] - `p1` - **ELSE** → the subscription is not affected and gets no row: a write of an identical value affects nothing - `inst-ca-affected-7`
2. [x] - `p1` - The changes of one request are evaluated in order, each under its own lock and transaction, so the last change to a key at a tenant wins the row - `inst-ca-affected-8`
3. [x] - `p1` - **RETURN** the affected subscriptions with their values after the change - `inst-ca-affected-9`

### Expected Value Encoding

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-consumer-activation-expected`

**Input**: The effective value at a subscription's tenant after the change — for a `secret`-trait key the plaintext the change holds — the key, the subscription's tenant and whether the key has the `secret` trait

**Output**: The `expected_value` text the row stores, and the same encoding the SDK applies to `applied_value`

**Steps**:
1. [x] - `p1` - **IF** the key has no `secret` trait → encode the value as RFC 8785 canonical JSON and **RETURN** those bytes as text, compared later byte for byte - `inst-ca-expected-1`
2. [x] - `p1` - Rely on the number domain the Type Validator enforces on write (`inst-vw-commit-1`, numeric canonicality): every number survives a round trip through IEEE-754 binary64; the encoder never normalizes a number, and one outside that domain fails the change with nothing stored - `inst-ca-expected-2`
3. [x] - `p1` - Encode a `file-reference`-trait value as an ordinary structured value: it carries a version-pinned reference, never the file, and needs no hash - `inst-ca-expected-3`
4. [x] - `p1` - **IF** the key has the `secret` trait → build the array of the literal `cf.settings.activation.secret.v1`, the key, the subscription's tenant as a bare lowercase UUID string and the plaintext value, encode it as RFC 8785 canonical JSON, and take its SHA-256 as lowercase hex - `inst-ca-expected-4`
   1. [x] - `p1` - Salt with the key and the tenant only, never with anything of the change, so one value hashes equal across the changes that replace one another on a row - `inst-ca-expected-5`
   2. [x] - `p1` - Take the plaintext from the request for an inline secret `set`, from the fetch for an inherited fallback or an adopted staged entry, or the default's value when the fallback is not a stored secret - `inst-ca-expected-6`
5. [ ] - `p1` - The SDK applies this same encoding and hash to `applied_value` in the consumer's process; agreement of the two canonicalizers on a shared corpus is a conformance requirement - `inst-ca-expected-7`
6. [x] - `p1` - **RETURN** the encoded text - `inst-ca-expected-8`

### Outcome Rules

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-consumer-activation-outcome`

**Input**: An `awaiting` activation state — its `change_ref`, `expected_value` and whether its key has the `secret` trait — and a report's `change_ref`, `status`, `applied_value` and `detail`

**Output**: `succeeded`, `failed` or `ignored`, and for a transition the fields it writes

**Steps**:
1. [x] - `p1` - Treat `applied_value` as matching only when it is present, well formed — valid RFC 8785 canonical JSON, or 64 lowercase hex characters for a `secret`-trait row — and byte-equal to `expected_value` - `inst-ca-outcome-1`
2. [x] - `p1` - **IF** `status` is `success` **AND** the value matches → **RETURN** `succeeded`, whatever `change_ref` the report names: the consumer runs the value currently expected - `inst-ca-outcome-2`
3. [x] - `p1` - **IF** `status` is `success` **AND** the value is different, missing or malformed → **RETURN** `failed` when the report names the row's `change_ref`, `ignored` as stale when it names another - `inst-ca-outcome-3`
4. [x] - `p1` - **IF** `status` is `failed` → **RETURN** `failed` when the report names the row's `change_ref`, `ignored` when it names another - `inst-ca-outcome-4`
5. [x] - `p1` - A transition writes only `status`, `applied_value` as reported (absent when the report carried none), `detail` (the consumer's detail on a `failed` status, absent otherwise), `reported_at` and `updated_at`; `expected_value` and `change_ref` are never touched by a report - `inst-ca-outcome-5`

### Notification Bundle

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-consumer-activation-bundle`

**Input**: One pair of subscriber and tenant and its `awaiting` rows, each with its key and `change_ref`

**Output**: One `ChangeNotification` body, or several when one would exceed the broker's event size limit

**Steps**:
1. [x] - `p1` - Build one body: `tenant` the pair's tenant (the root tenant's id for a platform-wide subscription), `subscriber` the pair's subscriber, and `changes`, the list of every row's `key` with its `change_ref`, in key order - `inst-ca-bundle-1`
2. [x] - `p1` - Carry identifiers only: no value, no expected value, no hash, no actor, and no key outside the pair's `awaiting` rows - `inst-ca-bundle-2`
3. [x] - `p1` - Leave the optional `subject_type` and `subject_id` absent in v1 - `inst-ca-bundle-3`
4. [x] - `p1` - **IF** the serialised event, envelope included, would exceed the broker's 64 KiB event size limit (`cpt-cf-evbk-nfr-event-size`) → split `changes` across several bodies, packing entries in order until the next would exceed the limit, each body with the same `tenant` and `subscriber` and an event `id` of its own, each entry kept whole, together carrying every entry once - `inst-ca-bundle-4`
5. [x] - `p1` - **RETURN** the bodies in order - `inst-ca-bundle-5`

### Subscribe Per-Key State

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-consumer-activation-subscribe-state`

**Input**: The outcome of a `subscribe` call, or of one step of one key's registration on a handle

**Output**: The key's state on the handle and whether the SDK retries it

**Steps**:
1. [ ] - `p1` - **MATCH** the outcome of a `subscribe` call or of one key's registration step - `inst-ca-substate-1`
   - [ ] - `p1` - **CASE** empty `keys` or a malformed tenant: an error for the call, no handle, nothing registered, no retry - `inst-ca-substate-2`
   - [ ] - `p1` - **CASE** the registration committed and the fresh read returned a value: `registered` with that value, no retry - `inst-ca-substate-3`
   - [ ] - `p1` - **CASE** the registration committed and the fresh read returned `Retired`: `registered` with the value absent, no retry; a reactivation re-arms the next change - `inst-ca-substate-4`
   - [ ] - `p1` - **CASE** the registration committed and no broker is resolvable: `registered`; the SDK retries joining the activation topic and the delivery loop catches up - `inst-ca-substate-5`
   - [ ] - `p1` - **CASE** the key refused `NotFound` before the lock — undeclared, hidden, or a tenant outside the subscriber's subtree: `pending`, retried with backoff while the handle is live - `inst-ca-substate-6`
   - [ ] - `p1` - **CASE** the lock wait exhausted, the authorization service unavailable, or a database error before the commit: `pending`, nothing registered, retried - `inst-ca-substate-7`
   - [ ] - `p1` - **CASE** the fresh read failed after the commit: `pending` with the registration kept, retried from the read - `inst-ca-substate-8`
   - [ ] - `p1` - **CASE** a broker grant missing (`consume` or `define`): `pending` with the registration kept, the join retried until an operator grants - `inst-ca-substate-9`
   - [ ] - `p1` - **CASE** the join retried after a missing grant succeeds: `registered` again, the subscription kept, no retry - `inst-ca-substate-10`
   - [ ] - `p1` - **CASE** a `403` registration read decision: `refused`, terminal, nothing registered - `inst-ca-substate-11`
   - [ ] - `p1` - **CASE** a 65th tenant interest of the gear in the process: `refused` for every key of the call, terminal, nothing registered - `inst-ca-substate-12`
2. [ ] - `p1` - A multi-key `subscribe` registers each key under its own lock in its own transaction; the handle reports each key's state, and the call is not atomic across keys - `inst-ca-substate-13`
3. [ ] - `p1` - A `pending` key logs a `warn` once each minute it stays `pending`; dropping the handle stops its retries and retires nothing - `inst-ca-substate-14`
4. [ ] - `p1` - **RETURN** the key's state and whether the SDK retries it - `inst-ca-substate-15`

### Fresh Read

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-consumer-activation-fresh-read`

**Input**: A setting key, a tenant and the caller's security context — a reader call of `get_effective` with the freshness option set

**Output**: The effective value at the tenant as `get_effective` returns it — a `SecretHandle` for a `secret`-trait key, or `Retired` or `NotFound` as any read answers — or the reader's error

**Steps**:
1. [ ] - `p1` - Take the read as an ordinary `get_effective` of the key at the tenant under the caller's identity, with the same authorization, visibility and retirement rules as any read of that key at that scope - `inst-ca-fresh-1`
2. [ ] - `p1` - Bypass the effective-value cache on the replica that serves the read: no cached entry is consulted - `inst-ca-fresh-2`
3. [ ] - `p1` - Resolve the effective value from the store over the requested tenant's ancestor chain, as the Value Resolver's freshness form resolves it (DESIGN §4.5, §4.2 *Value Resolver*) - `inst-ca-fresh-3`
4. [ ] - `p1` - Leave the cache unpopulated: the result is not stored, so the read leaves the cache as it found it, as both uncached forms of `resolve` do (DESIGN.md §4.2 *Value Resolver*, §4.5 `get_effective`) - `inst-ca-fresh-4`
5. [ ] - `p1` - Make one store read per key; the bulk read has no freshness option and is never used for a fresh read - `inst-ca-fresh-5`
6. [ ] - `p1` - **RETURN** the result as `get_effective` returns it - `inst-ca-fresh-6`

## 4. States (CDSL)

### Activation State Machine

- [x] `p1` - **ID**: `cpt-cf-settings-service-state-consumer-activation-row`

**States**: `awaiting`, `succeeded`, `failed`

**Initial State**: `awaiting`, written by the first committed change that affects the subscription

**Transitions**:
1. [x] - `p1` - **FROM** no row **TO** `awaiting` **WHEN** a committed change affects the subscription (`cpt-cf-settings-service-flow-consumer-activation-record`) - `inst-ca-row-1`
2. [x] - `p1` - **FROM** `awaiting` **TO** `succeeded` **WHEN** a `success` report carries the expected value, whatever `change_ref` it names - `inst-ca-row-2`
3. [x] - `p1` - **FROM** `awaiting` **TO** `failed` **WHEN** a report naming the row's `change_ref` is `failed`, or is a `success` whose value differs, is missing or is malformed - `inst-ca-row-3`
4. [x] - `p1` - **FROM** `awaiting` **TO** `awaiting` **WHEN** a later committed change affects the subscription, replacing `expected_value` and `change_ref` and clearing `last_notified_at` - `inst-ca-row-4`
5. [x] - `p1` - **FROM** `succeeded` or `failed` **TO** `awaiting` **WHEN** a later committed change affects the subscription, clearing `applied_value`, `detail`, `reported_at` and `last_notified_at` - `inst-ca-row-5`
6. [x] - `p1` - **FROM** any state **TO** deleted **WHEN** the subscription is retired, or the delivery loop's sweep finds its subscription gone - `inst-ca-row-6`

No transition happens on a timer: a state stays `awaiting` until a report or a later change, and a report never moves a terminal state.

### Subscription Key State Machine

- [ ] `p1` - **ID**: `cpt-cf-settings-service-state-consumer-activation-handle-key`

**States**: `pending`, `registered`, `refused`

**Initial State**: `pending`, the state of every key of a handle `subscribe` returns, unless the interest limit refused the call

**End States**: `refused` is terminal; a `registered` or `pending` key leaves the handle when `retire()` answers it `retired` or `404` — gone from the handle, which is not a state, and neither is retired

**Transitions**:
1. [ ] - `p1` - **FROM** `pending` **TO** `registered` **WHEN** the key's registration committed and its fresh read returned a value or `Retired` - `inst-ca-hk-1`
2. [ ] - `p1` - **FROM** `pending` **TO** `pending` **WHEN** a retryable outcome repeats; the SDK retries with backoff and logs once each minute - `inst-ca-hk-2`
3. [ ] - `p1` - **FROM** `pending` **TO** `refused` **WHEN** the registration read decision is a `403` - `inst-ca-hk-3`
4. [ ] - `p1` - **FROM** `registered` **TO** `pending` **WHEN** the broker refuses the gear's group or join for a missing `define` or `consume` grant, the subscription kept - `inst-ca-hk-4`
5. [ ] - `p1` - **FROM** `pending` **TO** `registered` **WHEN** the join retried after a missing `define` or `consume` grant succeeds, the subscription kept - `inst-ca-hk-5`
6. [ ] - `p1` - **FROM** `registered` **TO** gone from the handle **WHEN** `retire()` deleted the key's subscription - `inst-ca-hk-6`
7. [ ] - `p1` - **FROM** `pending` **TO** gone from the handle **WHEN** `retire()` deleted the key's subscription or answered `404` for it - `inst-ca-hk-7`

A `Subscription` has no state machine: it exists, or it does not. Dropping a handle ends its keys' states in the process without touching any subscription. The answers of `retire()` and `report_outcome` belong to their flows, `cpt-cf-settings-service-flow-consumer-activation-retire` and `cpt-cf-settings-service-flow-consumer-activation-report`.

## 5. Definitions of Done

### Activation Schema

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-schema`

The system **MUST** add, in one migration for PostgreSQL and SQLite, the `subscriptions` table — `id` UUID primary key, `subscriber`, `key`, `tenant` as a bare lowercase UUID string never `NULL`, `created_at` defaulting to now — with the unique `uq_sub_subscriber_key_tenant` on subscriber, key and tenant and `idx_sub_key` on key; the `activation_state` table — `subscriber`, `key`, `tenant`, `change_ref` UUID with no foreign key, `expected_value`, `status` checked to `awaiting`, `succeeded` and `failed` with default `awaiting`, nullable `applied_value`, `detail`, `reported_at` and `last_notified_at`, `updated_at` defaulting to now — with the unique `uq_activation_state` on subscriber, key and tenant, `idx_activation_key_tenant` on key and tenant, and the partial `idx_activation_awaiting` on subscriber and tenant over the `awaiting` rows; and on `audit_records` the `activation` value of `operation`, the nullable `change_ref` UUID and `detail` text columns, and the partial `idx_audit_change_ref` over the rows that carry a `change_ref`. The history read's `operation` vocabulary **MUST** gain `activation`, so that until entry 2.14 attaches reports to their change an `activation` record is listed in the history of its subscription tenant as an ordinary record, resource `cf.settings:{key}@{tenant}`, its `change_ref` and `detail` not yet rendered. `change_ref` **MUST** stay a reference by value, since audit records leave by retention while an `awaiting` state has no deadline. Because writes by an earlier build, or by a replica still on it during a rolling upgrade, take no mutation lock and record no activation state, consumer gears **MUST** be deployed to subscribe only once every replica runs this build; a rollback is clean only while no `activation` record exists, since an earlier build's history read refuses an `operation` it does not know (`cpt-cf-settings-service-dod-audit-store-table`), and once one exists the history of each setting holding one fails until this build is restored.

**Implements**:
- `cpt-cf-settings-service-flow-consumer-activation-record`
- `cpt-cf-settings-service-flow-consumer-activation-subscribe`
- `cpt-cf-settings-service-flow-consumer-activation-report`
- `cpt-cf-settings-service-state-consumer-activation-row`

**Constraints**: `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded` (`change_ref` a reference by value, outliving the audit record's retention)

**Touches**:
- DB: `subscriptions`, `activation_state`, `audit_records` (`operation`, `change_ref`, `detail`, `idx_audit_change_ref`)
- Entities: `Subscription`, `ActivationState`, `ActivationStatus`

### Subscriptions

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-subscriptions`

The Subscription Manager **MUST** register a subscription per subscriber, exact key and tenant, idempotent on `uq_sub_subscriber_key_tenant`, with the tenant normalised to a bare lowercase UUID string and the subscriber taken from the caller's security context. Before the lock it **MUST** take the registration read decision and refuse a `403` with nothing registered, then refuse `NotFound` for a key with no declaration, a key hidden from the subscriber or a tenant outside its subtree, so a refused registration never contends for the lock; it **MUST** register a retired key, and hold the key's mutation lock around the inserting transaction. `resolve_subscribers` **MUST** read the key's subscriptions once under the lock and qualify them by Scope Class — every one for `global`, the written tenant for `local`, for `cascading` those whose tenant the written scope is or is an ancestor of, decided per subscription tenant from its ancestor chain with no subtree enumeration — and a Tenant Resolver failure **MUST** fail the write closed. `retire_subscription` **MUST** take the subscriber from `Context`, hold the key's lock, delete the subscription and its activation state in one transaction, report nothing for them, answer `404` for a key with no subscription of the caller's — another subscriber's included, of which nothing is deleted — and answer `Unavailable` with nothing deleted when the lock wait is exhausted.

**Implements**:
- `cpt-cf-settings-service-flow-consumer-activation-subscribe`
- `cpt-cf-settings-service-flow-consumer-activation-retire`
- `cpt-cf-settings-service-flow-consumer-activation-record`
- `cpt-cf-settings-service-component-subscription-manager` — `register_subscription`, `resolve_subscribers`, `retire_subscription`

**Constraints**: `cpt-cf-settings-service-constraint-activation-filtered-notification`, `cpt-cf-settings-service-constraint-activation-tenant-in-payload`

**Touches**:
- DB: `subscriptions`, `activation_state`
- Entities: `Subscription`, Tenant Resolver ancestor chain, `PolicyEnforcer` registration read decision

### Every Mutation of a Key Takes Its Lock

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-mutation-lock`

Every value change (`set`, `revert`, `remove_value`, `clone`, each change of a batch), every restriction change (`set_restriction`, `clear_restriction`) and every subscription registration and retire **MUST** take the key's mutation lock — a named advisory lock through `toolkit-db`'s `LockManager`, keyed on the gear and the setting key — after its authorization and validation, waiting through `try_lock` on the lock's own retry schedule — a first wait of tens of milliseconds, doubling, capped at one second, with jitter — within `mutation_lock_wait_seconds` (default `10`, refused at init when `0` or above `3600`), in a batch further capped by the remainder of the request deadline, the schedule and the budget injectable in tests. An exhausted wait **MUST** answer `Unavailable` with nothing stored — `503` on the single-value and restriction routes, per item in `SetResult[]`, with the batch's later changes still attempted — and **MUST** leave the lock to its owner. A value change **MUST** hold the lock from before its candidate step until its transaction rolled back, or committed and the local cache is evicted, then release it and await the release before answering, so the next mutation of the key acquires it without waiting after a rollback; a restriction change, a registration and a retire **MUST** hold it around their transactions. Declaration lifecycle changes **MUST NOT** take it, and no mutation **MUST** hold two keys' locks at once. Acquisitions **MUST** be counted `acquired`, `exhausted` or `error` and their waits observed; a failed unlock (`NotHeld`, a database error) **MUST** be logged at `warn` and counted `error`; a lost lock session is silent to the holder (`toolkit-db` reports it as already released) — the residual DESIGN-activation §6 records. In the single-process SQLite profile the gear **MUST** remove the lock markers at init for the scan lock and every declared key's lock, and in no other profile. The settings database **MUST** be reached through session pooling or a direct connection; a transaction-pooling proxy voids the lock and is not supported.

**Implements**:
- `cpt-cf-settings-service-algo-consumer-activation-lock`
- `cpt-cf-settings-service-flow-consumer-activation-record`
- `cpt-cf-settings-service-flow-consumer-activation-lifecycle`
- `cpt-cf-settings-service-component-value-writer` — the lock around each change
- `cpt-cf-settings-service-component-tenant-permission` — the lock on `set_restriction` and `clear_restriction`

**Constraints**: `cpt-cf-settings-service-constraint-activation-in-settings-gear` (extends, listed under entry 2.12: its session-pooling clause is covered here, by the key's mutation lock and the scan lock)

**Touches**:
- Config: `mutation_lock_wait_seconds` (default 10 s)
- Entities: `toolkit-db` `LockManager`, Value Writer commit, Tenant Access restriction changes, Subscription Manager registration and retire

### Activation State in the Change's Transaction

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-record`

Every committed value change **MUST** write, in its own transaction and under the key's lock, one activation state per subscription it affects — the effective value at the subscription's tenant after the change differing from the value before it, every write of a `secret`-trait key counting, a candidate shadowed by a nearer override excluded — by one insert-select from `subscriptions` restricted to the ids of the read set made under the lock, setting `awaiting`, the encoded expected value resolved through the transaction over the handed chain while bypassing the cache, `change_ref` the change's audit record id — minted in the audit draft before the append and handed to `record_change` — the report fields and `last_notified_at` cleared and `updated_at` now, overwriting an existing state whatever its status. For a `revert` or a `remove_value` of a `secret`-trait key with qualified candidates, and for a batch change adopting a staged secret with qualified candidates, the plaintext **MUST** be fetched once per change through `resolve_plaintext`, under the lock and before the transaction, under the service's own identity with the same per-setting authorization decision, with one `secret_use` record that stays when the change rolls back, the fallback's reference read through the resolver's freshness form; a denied or undecidable decision, a failed store read or an unwritable `secret_use` record **MUST** refuse the change `503` with nothing of it stored and the lock released; the plaintext **MUST** live only in the redacting, zeroizing wrapper until hashed. A change refused at a gate, by validation, by the lock budget, by the Tenant Resolver, by a plaintext fetch or at the commit **MUST** store no activation state, and a rolled-back transaction **MUST** release the lock.

**Implements**:
- `cpt-cf-settings-service-flow-consumer-activation-record`
- `cpt-cf-settings-service-algo-consumer-activation-affected`
- `cpt-cf-settings-service-state-consumer-activation-row`
- `cpt-cf-settings-service-component-change-set-publisher` — `record_change`
- `cpt-cf-settings-service-component-value-resolver` — the two uncached forms of `resolve`
- `cpt-cf-settings-service-component-secret-manager` — the fallback and staged plaintext fetch, audited as `secret_use`

**Constraints**: `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded`, `cpt-cf-settings-service-constraint-activation-two-events-per-change-set` (the `change_notification` half: the state it is delivered from)

**Touches**:
- DB: `activation_state`, `subscriptions`, `audit_records` (`secret_use`)
- Entities: `ActivationState`, Value Writer commit (`inst-vw-commit-2`, `inst-vw-commit-3`, `inst-vw-commit-5`, `inst-vw-commit-6`, `inst-vw-commit-7`, `inst-vw-commit-8`), Secret Manager `resolve_plaintext`

### Canonical Encoding and Secret Hash

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-canonical`

`expected_value` **MUST** carry the value in RFC 8785 canonical JSON, to be compared byte for byte with the SDK's `applied_value`; a `secret`-trait value **MUST** be carried as the lowercase hex SHA-256 over the canonical encoding of the array of `cf.settings.activation.secret.v1`, the key, the subscription's tenant and the value, salted by key and tenant only; a `file-reference`-trait value **MUST** be encoded as an ordinary structured value. The encoder **MUST NOT** normalize a number; values reach it within the binary64-lossless domain the Type Validator enforces on write. The service-side canonicalizer **MUST** produce the known bytes for a corpus of values — member order, whitespace, number formatting and Unicode escaping varied — and the secret hash its known answer, computed outside the crate. The SDK side of the encoding and the conformance of the two canonicalizers on a shared corpus are `cpt-cf-settings-service-dod-consumer-activation-canonical-sdk`.

**Implements**:
- `cpt-cf-settings-service-algo-consumer-activation-expected`
- `cpt-cf-settings-service-algo-consumer-activation-outcome`

**Constraints**: `cpt-cf-settings-service-constraint-activation-identifiers-only-payload` (the back-response exception: a hash for a secret), `cpt-cf-settings-service-constraint-activation-pull-plus-back-response`

**Touches**:
- Entities: `ActivationState.expected_value`, the service-side canonicalizer

### Canonical Encoding in the SDK

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-canonical-sdk`

The SDK's encoder of the consumer's `applied_value` **MUST** apply the same RFC 8785 encoding and, for a `secret`-trait row, the same secret hash as the service, and the service-side and SDK-side canonicalizers **MUST** produce identical bytes for a shared corpus of values — member order, whitespace, number formatting and Unicode escaping varied — and for the secret hashes of that corpus.

**Implements**:
- `cpt-cf-settings-service-algo-consumer-activation-expected`

**Constraints**: `cpt-cf-settings-service-constraint-activation-identifiers-only-payload` (the back-response exception: a hash for a secret)

**Touches**:
- Entities: `BackResponse.applied_value`, the shared canonicalizer of the gear and the SDK

### Notification Delivery

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-delivery`

At the end of every request that wrote activation states, after its `cache_invalidate` broadcasts, the end-of-request hook **MUST** publish, per pair of subscriber and tenant it touched, the pair's `awaiting` keys with their `change_ref` as `change_notification` on the activation topic, with envelope `tenant_id` and `subject` the subscription's tenant, `subject_type` the activation subject type, `source` `settings-service` and `trace_parent` from the current span; a bundle over 64 KiB **MUST** be split with keys kept whole; each publish **MUST** be bounded by `publish_timeout_seconds`, the request's remaining publishes skipped after the first refusal or timeout of either signal, and an accepted publish **MUST** stamp `last_notified_at` on the rows still naming the bundled `change_ref`. Without a broker, before the activation types are registered or before the root tenant's id is known, nothing **MUST** be published and the write **MUST** commit. Every replica **MUST** run the delivery loop as one control loop ticking every `redeliver_interval_seconds` (default `30`, refused at init when `0`): each tick refreshes the activation gauges whether or not a broker resolves, and a tick on which the broker resolves, the root tenant's id is known and the activation types are registered runs one pass — one non-blocking attempt on the scan lock, the pass skipped when another replica holds it, an orphan sweep in one conditional delete, a re-publish of the due `awaiting` rows only, a stamp guarded by the row's `change_ref`, the rest of the pass abandoned when the broker is lost, and on `stop` the pass finished or abandoned within the shutdown deadline with every `awaiting` state kept. The activation topic and the `change_notification` type **MUST** be registered by the delivery loop's own task once the broker resolves, idempotently, and retried on the next tick after a refusal. The deployment **MUST** grant the publishing identity of entry 2.12 `produce` on the activation topic, the `change_notification` type and the activation subject type for events of every tenant; a missing grant **MUST** leave the gear up and the states `awaiting`, each refused publish logged at `warn` and retried by a later pass.

**Implements**:
- `cpt-cf-settings-service-flow-consumer-activation-publish`
- `cpt-cf-settings-service-flow-consumer-activation-deliver`
- `cpt-cf-settings-service-flow-consumer-activation-lifecycle`
- `cpt-cf-settings-service-algo-consumer-activation-bundle`
- `cpt-cf-settings-service-component-change-set-publisher` — `publish_request`, the notification half
- `cpt-cf-settings-service-component-event-broker-client` — the activation topic, `publish_notification`, `deliver_awaiting`

**Constraints**: `cpt-cf-settings-service-constraint-activation-two-events-per-change-set` (the `change_notification` half), `cpt-cf-settings-service-constraint-activation-filtered-notification`, `cpt-cf-settings-service-constraint-activation-tenant-in-payload`, `cpt-cf-settings-service-constraint-activation-identifiers-only-payload`, `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded`, `cpt-cf-settings-service-constraint-activation-broker-only` (extends, covered by entry 2.12)

**Touches**:
- Config: `redeliver_interval_seconds` (default 30 s), the shared `publish_timeout_seconds` (default 2 s)
- DB: `activation_state` (`idx_activation_awaiting`)
- Entities: `ChangeNotification`, the scan lock, the Types Registry registration of the activation topic and the `change_notification` type, the publishing identity's `produce` grant on the activation topic

### Outcome Recording

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-outcomes`

The Activation Outcome Recorder's `receive_response` **MUST** answer `ignored` for no state and for a terminal one; for an `awaiting` state it **MUST** settle `succeeded` a `success` whose value matches the expected value whatever `change_ref` it names, settle `failed` a `success` with a different, missing or malformed value or a `failed` status when the report names the state's `change_ref`, and answer `ignored` for either when it names another. A transition **MUST** write only `status`, `applied_value`, `detail`, `reported_at` and `updated_at`, guarded on the state still naming the decided `change_ref` and still `awaiting`, a guard matching no row answering `ignored` with nothing written; it **MUST** append, in the same transaction, exactly one `activation` audit record with the key's canonical resource, `declaration_key`, the state's tenant, the subscriber as `actor` with `actor_classification` `public`, `outcome`, the consumer's `detail` truncated to 1 KiB on failure, a minted `request_id`, `change_ref` and no pre-value or post-value. A failed append or commit **MUST** roll back and answer an error; a report **MUST** be idempotent per subscriber, key, tenant and `change_ref`, so a repeated report reaches the same end state as the first; late reports **MUST** be accepted while the state is `awaiting`.

**Implements**:
- `cpt-cf-settings-service-flow-consumer-activation-report`
- `cpt-cf-settings-service-algo-consumer-activation-outcome`
- `cpt-cf-settings-service-state-consumer-activation-row`
- `cpt-cf-settings-service-component-change-set-outcome-tracker` — `receive_response`

**Constraints**: `cpt-cf-settings-service-constraint-activation-pull-plus-back-response`, `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded`

**Touches**:
- DB: `activation_state`, `audit_records` (`operation` `activation`, `change_ref`, `detail`)
- Entities: `BackResponse`, `ResponseStatus`, `ActivationStatus`, the Audit Emitter's append

### Consumer Activation SDK

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-sdk`

`settings-service-sdk` **MUST** replace its consumer activation contract with `subscribe(keys, tenant, handler)`, the handle's explicit `retire()` and `report_outcome(change_ref, tenant, key, status, applied_value, detail?)`. `subscribe` **MUST** return an error only for invalid input and otherwise a handle at once, with the per-key states `registered` (with the fresh value read after the registration committed), `pending` (retried with backoff while the handle is live, a `warn` each minute) and `refused` (terminal: a `403`, or a 65th tenant interest checked before registering); a repeated `subscribe` **MUST** return a new handle, and dropping a handle **MUST** stop its retries and retire nothing. The SDK **MUST** keep one anonymous consumer group per subscribing gear per process, with one interest per subscription tenant, offsets in memory from the latest position, left and deleted best-effort with the gear's last handle or on `stop`. It **MUST** drop notifications for another subscriber; re-read each notified key through the fresh read (`cpt-cf-settings-service-algo-consumer-activation-fresh-read`) — `get_effective` with the freshness option, which bypasses the effective-value cache on the serving replica and does not populate it, and is otherwise an ordinary read of the key at the scope under the caller's identity; invoke the handler with the bundle and the fresh values; report its outcomes with `applied_value` encoded before the call, on both entry paths, as the service encodes `expected_value`; re-apply and re-report a repeat; and hand the handler a bundle without `change_ref`, owing no report, when a pending registration commits. `report_outcome` **MUST** answer `applied` or `ignored`, and the SDK **MUST NOT** retry it. `SettingsReaderClient.get_effective` **MUST** take the optional freshness option. A subscribing gear's deployment **MUST** hold `consume` on the activation topic, the `change_notification` type and the activation subject type for its subscription tenants, and `define` for its own group; a missing grant **MUST** leave the gear up, its keys `pending` and their states `awaiting`, logged at `warn` and retried.

**Implements**:
- `cpt-cf-settings-service-flow-consumer-activation-subscribe`
- `cpt-cf-settings-service-flow-consumer-activation-react`
- `cpt-cf-settings-service-flow-consumer-activation-report`
- `cpt-cf-settings-service-flow-consumer-activation-retire`
- `cpt-cf-settings-service-algo-consumer-activation-subscribe-state`
- `cpt-cf-settings-service-algo-consumer-activation-fresh-read`
- `cpt-cf-settings-service-state-consumer-activation-handle-key`
- `cpt-cf-settings-service-component-consumer-activation-sdk` — `subscribe`, `report_outcome`
- `cpt-cf-settings-service-component-event-broker-client` — `subscribe_notifications`
- `cpt-cf-settings-service-component-cache-and-invalidation` — the fresh read bypasses it

**Constraints**: `cpt-cf-settings-service-constraint-activation-pull-plus-back-response`, `cpt-cf-settings-service-constraint-activation-filtered-notification`, `cpt-cf-settings-service-constraint-activation-broker-only` (extends, covered by entry 2.12)

**Touches**:
- API: SDK `subscribe`, `retire()`, `report_outcome`; SDK `SettingsReaderClient.get_effective` with the freshness option; no REST route
- Entities: `ChangeNotification`, `BackResponse`, the subscription handle and its per-key state, the gear's anonymous consumer group, the subscribing gear's `consume` and `define` grants

### Activation Is Observable

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-observability`

The system **MUST** expose the metrics of DESIGN-activation §7 that the service itself observes: `settings_notify_publish_to_deliver_seconds{event}` observed at receipt from the envelope's `occurred_at`, before any drop, for `cache_invalidate` in the replica's consumer of entry 2.12 (the `change_notification` observation is the SDK's, `cpt-cf-settings-service-dod-consumer-activation-observability-sdk`); `settings_notify_response_latency_seconds` from the state's `updated_at` to the report that settles it; `settings_notify_redeliveries_total` per notification the scan publishes; `settings_activation_awaiting` and `settings_notify_subscriptions`, refreshed by the delivery loop on every tick of every replica, broker or not; `settings_notify_reactions_total{outcome}`; `settings_notify_reports_total{result}`, counting every `applied` and every `ignored` answer and no error; `settings_mutation_lock_wait_seconds`; and `settings_mutation_lock_total{result}`. It **MUST** log at `info` a subscription registered or retired and the delivery loop's start and stop, and at `warn` a refused publish, a refused registration of the activation types, a refused registration read decision, a lock error and a lost broker. Every notification **MUST** carry `trace_parent` from the publishing span when one is present. No log line, metric label or span attribute of the service **MUST** carry a value, a hash or a secret.

**Implements**:
- `cpt-cf-settings-service-flow-consumer-activation-deliver`
- `cpt-cf-settings-service-flow-consumer-activation-report`
- `cpt-cf-settings-service-flow-consumer-activation-lifecycle`
- `cpt-cf-settings-service-algo-consumer-activation-lock`

**Constraints**: `cpt-cf-settings-service-constraint-activation-identifiers-only-payload`

**Touches**:
- Entities: metrics, logs, spans

### Activation Is Observable in the SDK

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-observability-sdk`

The SDK **MUST** observe `settings_notify_publish_to_deliver_seconds` with `event` `change_notification` at receipt, from the envelope's `occurred_at`, before any malformed or foreign-subscriber drop, so that a dropped delivery is observed too. It **MUST** log at `warn` a refused group or join, a malformed delivery, and each minute a key stays `pending`, naming the key and the reason. No log line, metric label or span attribute of the SDK **MUST** carry a value, a hash or a secret.

**Implements**:
- `cpt-cf-settings-service-flow-consumer-activation-react`
- `cpt-cf-settings-service-flow-consumer-activation-subscribe`
- `cpt-cf-settings-service-component-consumer-activation-sdk`

**Constraints**: `cpt-cf-settings-service-constraint-activation-identifiers-only-payload`

**Touches**:
- Entities: metrics, logs, spans

### Activation Tests

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-consumer-activation-tests`

The feature **MUST** ship with these tests, each implementing a row of DESIGN-activation §7 or DESIGN §7, or a rule of DESIGN-activation §4.2 or of this document where no row exists. The publish-to-deliver p95 and response p95 targets of DESIGN-activation §7 are observed through `settings_notify_publish_to_deliver_seconds` and `settings_notify_response_latency_seconds` only, and no test asserts them:

- Unit, with `InMemoryNotificationRepo` and `MockEventBrokerApi`: every Level 1 row of DESIGN-activation §7 not owned by entry 2.12 or 2.14 — *Publish builds identifier-only payloads* (the `change_notification` half), *Per-subscriber filtering*, *A change writes one row per affected subscription*, *Overwrite on a newer change*, *Matching value succeeds*, *Wrong value with the current `change_ref`*, *Stale report ignored*, *Repeat report*, *No row*, *Activation audit record once per transition*, *Outcome status — secret hash compare*, *Canonical encoding — number domain*, *Canonical encoding — serialization-independent match* (the service-side corpus; the corpus shared with the SDK is `cpt-cf-settings-service-dod-consumer-activation-canonical-sdk`), *Delivery loop re-publish*, *Subscription matching*, *Affectedness by Scope Class*, *One notification per subscriber and tenant*, *Split notification*, *Fresh re-read*, *Secret fallback hash*, *Tenant Resolver failure*, *Foreign notifications dropped*, *Publish timeout and skip* (the notification half), *No broker* (the activation-state half), *Orphan row swept*, *Retire deletes rows*, *Registration read decision*, *Subscription group lifecycle*, *Subscribe states* and *Error mapping — all domain → API variants*; the DESIGN §7 rows *Set — value write, audit record and activation state are one transaction*, the notification half of *Set — commit + publish* and the activation-state half of *Set — no broker resolvable*; and, where no row exists: a `file-reference` value encoded as an ordinary object; the staged-secret adoption fetch; a fetch whose per-setting decision is denied or undecidable, or whose store read or `secret_use` record fails, refusing the change `503` with nothing stored; a `subscribe` key left `pending` by an exhausted lock wait, an unavailable authorization service or a database error with nothing registered, and by a fresh read failing after the commit with the registration kept; a missing subscriber-side `consume` or `define` grant moving the handle's keys from `registered` to `pending` with their subscriptions kept and back once the join succeeds; no broker at `subscribe` keeping the keys `registered` while the group and the join are retried; the 1 s to 30 s backoff with jitter of a `pending` key and its `warn` once each minute; a notified key whose fresh re-read fails or answers `Retired` or `NotFound` left out of the bundle with nothing reported; `report_outcome` answering an invalid-argument error for malformed input, and an error with nothing changed when its audit append or its commit fails; a consumer group that cannot be left or deleted logged at `warn` with nothing else changed; a malformed `change_notification` dropped with a `warn`; the activation types' registration refused and retried; delivery idle while the root tenant's id is unknown; a missing `produce` grant leaving the states `awaiting`; the broker lost mid-pass; `stop` during a pass; a repeated notification reaching the same end state as the first; registration of a retired key; `retire()` answering `404` for another subscriber's row and `Unavailable` on an exhausted wait; declaration lifecycle changes taking no lock; the SQLite marker removal at init in the single-process profile only; gear init refusing a `0` interval or budget; the lock's retry schedule; `report_outcome` never retried by the SDK; the fresh read leaving the cache unpopulated; and no value, hash or secret in a log line, metric label or span attribute.
- Integration, on real PostgreSQL through testcontainers: every Level 2 row of DESIGN-activation §7 except *Two-replica convergence* and *Dropped broadcast* — *Delivery for committed changes* (its notification half), *State commits with the value*, *Delivery backoff*, *Concurrent replicas do not duplicate*, *Subscribe during a write*, *Retire during a write*, *Two writes, one key*, *Lock wait exhausted*, *Restriction change against a write*, *Scan during an overwrite*, *Different keys do not block*, *Retired subscription gets no row*, *Per-item `Unavailable`*, *Lock released on rollback*, *`secret_use` kept on rollback*, *Lock metrics*, *Overwrite clears the delivery stamp*, *Concurrent report and overwrite*, *Report recorded in history*, *Report idempotency* and *Subscription idempotency* — plus the *Lock hold time at the per-key bound* row of the NFR verification mapping, DESIGN §7 Concurrency Testing item 5, the staged-secret adoption fetch under the lock, and a `retire()` answering `Unavailable` against a held lock and deleting the subscription on a second call. The concurrent rows run on separate committed connections with explicit cleanup, a write held in flight by a pause point exposed to tests only between the candidate step and the commit, the scan held by a callback in the mocked publish, the lock's retry schedule and budget injected, and `pg_stat_database.deadlocks` asserted unchanged.
- Integration, on PostgreSQL through testcontainers and on SQLite: the migration creates `subscriptions` and `activation_state` with their uniqueness, `status` check and indexes, the partial `idx_activation_awaiting` included, and adds to `audit_records` the `activation` value of `operation`, the `change_ref` and `detail` columns and the partial `idx_audit_change_ref`; and an `activation` record in a setting's history is listed as an ordinary record under the canonical resource id until entry 2.14 attaches it.
- Integration, end to end, on real PostgreSQL with the delivering broker double of entry 2.12 and an in-process SDK consumer: set → `change_notification` → fresh re-read → `report_outcome` → state `succeeded` with one `activation` audit record carrying the change's `change_ref`; a wrong value on the current `change_ref` → `failed`; a report on a replaced `change_ref` → `ignored`; `retire()` deleting the subscription and its state; redelivery after a consumer restart that re-subscribes; and a write without a broker whose state stays `awaiting` until the broker appears and the loop delivers it.
- E2E, in an environment that runs a broker and a sample consumer gear: the Level 4 rows of DESIGN-activation §7 owned here — *Full loop*, its write side (set, `change_notification`, fresh re-read, `report_outcome`, state `succeeded`), the activation read and the history assertions being entry 2.14's; *Consumer isolation*; *Value mismatch → failed*, limited to the state `failed` and its `activation` audit record; *Restart re-publish*; and *No secret in stream*.

**Implements**:
- `cpt-cf-settings-service-flow-consumer-activation-subscribe`
- `cpt-cf-settings-service-flow-consumer-activation-record`
- `cpt-cf-settings-service-flow-consumer-activation-publish`
- `cpt-cf-settings-service-flow-consumer-activation-deliver`
- `cpt-cf-settings-service-flow-consumer-activation-react`
- `cpt-cf-settings-service-flow-consumer-activation-report`
- `cpt-cf-settings-service-flow-consumer-activation-retire`
- `cpt-cf-settings-service-flow-consumer-activation-lifecycle`
- `cpt-cf-settings-service-algo-consumer-activation-lock`
- `cpt-cf-settings-service-algo-consumer-activation-affected`
- `cpt-cf-settings-service-algo-consumer-activation-expected`
- `cpt-cf-settings-service-algo-consumer-activation-outcome`
- `cpt-cf-settings-service-algo-consumer-activation-bundle`
- `cpt-cf-settings-service-algo-consumer-activation-subscribe-state`
- `cpt-cf-settings-service-algo-consumer-activation-fresh-read`

**Constraints**: `cpt-cf-settings-service-constraint-activation-two-events-per-change-set`, `cpt-cf-settings-service-constraint-activation-filtered-notification`, `cpt-cf-settings-service-constraint-activation-tenant-in-payload`, `cpt-cf-settings-service-constraint-activation-identifiers-only-payload`, `cpt-cf-settings-service-constraint-activation-pull-plus-back-response`, `cpt-cf-settings-service-constraint-activation-wait-for-all-unbounded`

**Touches**:
- Entities: `InMemoryNotificationRepo`, `MockEventBrokerApi`, the delivering broker double, the test-only pause point, the in-process SDK consumer, the sample consumer gear

## 6. Acceptance Criteria

- [ ] On PostgreSQL and on SQLite the migration creates `subscriptions` with the unique `uq_sub_subscriber_key_tenant` and `idx_sub_key`, and `activation_state` with the unique `uq_activation_state`, the `status` check to `awaiting`, `succeeded` and `failed`, `idx_activation_key_tenant` and the partial `idx_activation_awaiting` over the `awaiting` rows, and adds to `audit_records` the `activation` value of `operation`, the nullable `change_ref` and `detail` columns and the partial `idx_audit_change_ref` over the rows carrying a `change_ref`; a second subscription or activation state for one subscriber, key and tenant and a `status` outside the three values are refused; and an `activation` record in a setting's history is listed as an ordinary record under the canonical resource id `cf.settings:{key}@{tenant}` until entry 2.14 attaches it to its change (integration)
- [x] A `set` at `X` of a `cascading` key with subscriptions at `X`, at a descendant `D` and at a descendant `E` shadowed by its own override writes, in the change's transaction, one `awaiting` state for `X` and one for `D`, each with the expected value at its tenant and the change's audit record id as `change_ref`, and none for `E` (unit)
- [x] A write of an identical value writes no activation state, while every write of a `secret`-trait key writes one per qualified subscription; a `local` key affects only the subscription at the written tenant and a `global` key every subscription of the key (unit)
- [x] A later change to the same key and tenant resets a `succeeded`, `failed` or `awaiting` state to `awaiting` with the new expected value and `change_ref`, clearing `applied_value`, `detail`, `reported_at` and `last_notified_at`; within one request the last change to a key at a tenant wins (unit)
- [x] A transaction aborted after the value write stores no value, no audit record and no activation state; a crash after a change commits and before the end of its request leaves its states `awaiting`, which the delivery loop publishes (integration)
- [x] Two concurrent writes of one key commit one after the other and the later one's expected value equals the value in force at its commit; two writes of different keys do not wait for each other; `pg_stat_database.deadlocks` is unchanged (integration)
- [x] With the key's lock held and a short injected budget, a single write answers `503` `Unavailable` with nothing stored and the lock still held by its owner; in a batch that item alone answers `Unavailable` and the changes before and after it commit (integration)
- [x] The lock's retry schedule starts at tens of milliseconds, doubles, is capped at one second with jitter and stops at the budget, and the schedule and the budget are injectable in tests (unit)
- [x] After a change whose transaction rolled back on a stale tag, the next mutation of the key acquires its lock without waiting (integration)
- [ ] With the per-key bound of subscriptions (DESIGN-activation §7 *Fan-out bounds*), the time a change holds the key's lock is measured through `settings_mutation_lock_wait_seconds` of a mutation waiting on it and reported; the publish-to-deliver and response p95 targets are observed through their metrics and asserted by no test (integration)
- [x] A declaration retire, reactivate, evolve, update or reconcile pass takes no mutation lock (unit)
- [ ] A registration and a retire issued while a write of the key is in flight wait for its commit: the registration's fresh read returns the committed value and the next write writes its state; the retire deletes the subscription with the state the write committed, and the write's publish delivers nothing for it (integration)
- [x] A restriction change racing a write of the key takes the same lock: the write commits before the restriction, or sees it and is refused (integration)
- [x] A `revert` of a secret override with subscribers inheriting the fallback fetches the fallback plaintext once, under the lock and before the transaction, writes exactly one `secret_use` record attributed to the service's identity, records per state the hash salted by that state's tenant, and the `secret_use` record stays when the change's transaction rolls back (unit, integration)
- [x] A batch `set` adopting a staged secret with subscribers fetches the staged plaintext once by its reference, under the lock and before the transaction, writes exactly one `secret_use` record attributed to the service's identity, and records per state the hash salted by that state's tenant (unit, integration)
- [x] A fallback or staged plaintext fetch whose per-setting authorization decision for the service's identity is denied or cannot be obtained, whose store read fails, or whose `secret_use` record cannot be written refuses the change `503` with nothing of it stored and the key's lock released (unit)
- [x] A `cascading` write whose subscription tenants' chains the Tenant Resolver cannot answer fails closed with nothing stored (unit)
- [x] A subscription of the change's read set deleted before the state write gets no state, while the other affected subscriptions get theirs (integration)
- [x] A `change_notification` carries `tenant`, `subscriber` and `changes` of `key` and `change_ref` only — no value, hash, secret or actor — and only the pair's `awaiting` keys, never another subscriber's or tenant's (unit)
- [x] A `change_notification` is published under `tenant_id` the subscription's tenant, with `subject` that tenant, `subject_type` `gts.cf.core.settings.activation.v1~` and `source` `settings-service`, after the request's `cache_invalidate` broadcasts (unit)
- [x] A bundle whose serialised event exceeds 64 KiB is published as several notifications, each under the limit, together carrying every `awaiting` key once with its `change_ref` (unit)
- [x] An accepted end-of-request publish stamps `last_notified_at` on the bundled rows; a publish stalled past `publish_timeout_seconds` is dropped, the request's remaining publishes are skipped, the request pays one timeout, and the delivery loop publishes the rows later (unit)
- [x] The delivery loop re-publishes an `awaiting` row once `redeliver_interval_seconds` has passed since `last_notified_at` and not before, publishes a row with no stamp at the next tick, and never re-publishes a terminal row (unit, integration)
- [x] Two delivery loops attempting the same tick never both scan: one acquires the scan lock, the other's single attempt fails and it skips the pass without waiting (integration)
- [x] The scan deletes, in one conditional statement, every state whose subscription no longer exists, `awaiting` or terminal, and leaves every state whose subscription exists untouched (unit)
- [x] A change overwriting a row while the scan publishes it leaves `last_notified_at` absent after the scan's stamp (integration)
- [x] Gear init refuses a `redeliver_interval_seconds` or a `mutation_lock_wait_seconds` of `0` or above its ceiling (86400 and 3600) (unit)
- [x] In the single-process SQLite profile gear init removes the lock markers of the scan lock and of every declared key's mutation lock, and in every other profile it removes none (unit)
- [x] With the activation topic or the `change_notification` type refused by the Types Registry, the refusal is logged at `warn` and retried on the next tick, and meanwhile no notification is published and no scan runs, the states staying `awaiting` (unit)
- [x] While the root tenant's id is not yet known, the delivery loop runs no pass and publishes nothing, the gauges are still refreshed on every tick, and the states stay `awaiting` (unit)
- [x] With the `produce` grant withheld, each notification publish is refused and logged at `warn`, the states stay `awaiting`, and the gear keeps serving reads and writes (unit)
- [x] A broker lost during a pass abandons the rest of the pass, releases the scan lock and keeps every `awaiting` state, and the next tick on which a broker resolves delivers them (unit)
- [x] `stop` during a pass finishes or abandons it within the shutdown deadline, releases the scan lock, keeps every `awaiting` state, and the loop's stop is logged at `info` (unit)
- [x] A `success` report whose value equals the expected value settles `succeeded` whether it names the state's `change_ref` or an earlier one; a `success` naming the state's `change_ref` with a different, missing or non-canonical value settles `failed`; the same reports naming an earlier `change_ref`, and a `failed` naming one, answer `ignored` and leave the state `awaiting` (unit)
- [x] A report for no state or for a terminal state answers `ignored`; a duplicate report makes one transition and one audit record (unit, integration)
- [x] Each transition appends exactly one `activation` audit record — `resource` the key at the state's tenant, `declaration_key`, `tenant_id`, `actor` the subscriber with `actor_classification` `public`, `outcome`, `detail` truncated to 1 KiB on failure, a minted `request_id`, `change_ref`, no pre-value or post-value — in the update's transaction; ignored and repeated reports append none (unit)
- [x] A report and a newer change on one state at once: the report applies only against the expectation it answered, a report on the replaced expectation answers `ignored` with no audit record, and the state ends naming the newer change (integration)
- [ ] A `report_outcome` answering an error is not retried by the SDK, and the consumer's repeat of the same report applies once (unit)
- [x] A `report_outcome` with a tenant or a `change_ref` that is not a UUID, or a status other than `success` or `failed`, answers an invalid-argument error, and one whose audit append or commit fails answers an error; in both cases the state, the audit table and `settings_notify_reports_total` are unchanged (unit)
- [ ] A secret hash is the lowercase hex SHA-256 over the canonical array of `cf.settings.activation.secret.v1`, key, tenant and value; the same value under another key or tenant hashes differently and under the same key and tenant equally across changes, and it matches a known answer computed outside the crate (unit)
- [ ] The service and the SDK canonicalizers agree on a shared corpus of re-serialized values and on the secret hashes of that corpus (unit; `cpt-cf-settings-service-dod-consumer-activation-canonical-sdk`)
- [x] A value with an integer beyond 2^53 in magnitude, or a decimal with more precision than binary64 resolves, is rejected on write and never canonicalized, and a `file-reference` value is encoded and compared as an ordinary object with no hash (unit)
- [ ] `subscribe` with empty keys or a malformed tenant answers an error with nothing registered; a key refused `NotFound` is `pending` without contending for the lock, and once declared its registration commits and the handler receives a bundle without `change_ref` that owes no report; a `403` key is `refused`; a 65th tenant interest refuses every key with nothing registered; dropping a handle stops its retries and retires nothing (unit)
- [ ] A `subscribe` to a retired key registers it and reports it `registered` with the value absent, and after a reactivation the key's next change writes the subscription's state (unit)
- [ ] A `subscribe` key whose registration meets an exhausted lock wait, an unavailable authorization service or a database error before the commit is `pending` with nothing registered, a key whose fresh read fails after the commit is `pending` with its registration kept, and each becomes `registered` once its retry succeeds (unit)
- [ ] A group or a join refused for a missing `consume` or `define` grant of the subscribing gear moves the handle's `registered` keys to `pending` with their subscriptions kept and logs a `warn`, and once the retried join succeeds the keys are `registered` again (unit)
- [ ] With no broker resolvable at `subscribe`, the registered keys stay `registered` while the group creation and the join are retried, and the join succeeds once a broker resolves (unit)
- [ ] A `pending` key's retries wait exponentially from 1 s to a 30 s cap with jitter, and a `warn` naming the key and the reason is logged once each minute the key stays `pending`, not more often (unit)
- [ ] Registering the same subscriber, key and tenant twice keeps one `subscriptions` row through `uq_sub_subscriber_key_tenant`, and a repeated `subscribe` returns a new handle over it (integration)
- [ ] `retire()` answers `404` for a key whose subscription at the handle's tenant is another subscriber's, deleting nothing of it, and `Unavailable` with nothing deleted while the key's lock is held past the budget; the SDK retries neither, and a second `retire()` deletes the subscription once the lock is free (unit, integration)
- [ ] One consumer group serves every handle of a gear in a process and is deleted when the last handle goes; a notification whose `subscriber` is another gear's is never handed to the handler (unit)
- [ ] When the gear's consumer group cannot be left or deleted, after its last handle goes or on `stop`, a `warn` is logged and nothing else changes: no subscription, state or handle is touched (unit)
- [ ] A delivery whose `type_id` is not the `change_notification` type, whose `data.tenant` is not a UUID, whose `data.subscriber` is absent or whose `data.changes` is malformed is dropped with a `warn` naming the event's `id`, the handler is not invoked, and `settings_notify_publish_to_deliver_seconds` still observes it (unit)
- [ ] A notified key is re-read with the freshness option, bypassing a cache that still holds the previous value and leaving the cache unpopulated, and the report of the committed value settles the state `succeeded` (unit)
- [ ] A notified key whose fresh re-read fails, or answers `Retired` or `NotFound`, is left out of the handler's bundle and nothing is reported for it, its state staying `awaiting`, while the other keys of the notification are handled (unit)
- [ ] A repeated `change_notification` for a key the consumer already handled is re-read, re-applied and re-reported, and the state ends as it did after the first report — the repeat's report answers `ignored` once the first settled the state — with no second audit record (unit)
- [x] With no broker resolvable, a write commits and answers as with one, its states stay `awaiting`, the delivery loop idles, `settings_activation_awaiting` counts them, and once the broker resolves the loop registers the activation types and delivers them (unit, integration)
- [x] `settings_mutation_lock_total` counts `acquired`, `exhausted` and `error` for an acquisition, an exhausted wait and an injected failing unlock, `settings_mutation_lock_wait_seconds` observes each wait, `settings_notify_reports_total` counts every `applied` and `ignored` answer and no error, and `settings_notify_reactions_total`, `settings_notify_response_latency_seconds`, `settings_notify_redeliveries_total` and `settings_notify_subscriptions` move as their flows state (unit, integration)
- [ ] No log line, metric label or span attribute of this feature carries a setting value, an expected or applied value, a hash or a secret (unit)
- [ ] A set, its notification, the consumer's fresh re-read and its report end with the state `succeeded` and one `activation` record under the change, over real PostgreSQL with the delivering broker double and an in-process SDK consumer; the failed, stale-ignored, retire, redelivery-after-restart and no-broker-then-resume paths reach the outcomes their flows state (integration)
- [ ] In an environment that runs a broker and a sample consumer gear, the DESIGN-activation §7 Level 4 rows pass for what this feature owns: *Full loop* through its write side — set, `change_notification`, fresh re-read, `report_outcome`, state `succeeded` — *Consumer isolation*, *Value mismatch → failed* through the state `failed` and its `activation` audit record, *Restart re-publish* and *No secret in stream*; the activation read and the history assertions of those rows are entry 2.14's (E2E)
- [ ] `cfs validate --artifact` reports zero errors on this document, and every `@cpt-*` marker the implementation adds resolves to an id defined here (gate)
