---
description: "Feature specification of the Settings Service gear's replica cache coherence: the end-of-request cache_invalidate broadcast over the Event Broker, each replica's consumer and eviction, the broker as a soft dependency, and the cache_ttl_seconds backstop."
---

<!-- Created: 2026-10-28 by Virtuozzo International GmbH -->
<!-- Updated: 2026-10-28 by Virtuozzo International GmbH -->

# Feature: Replica Cache Coherence

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-replica-coherence`

- [ ] `p1` - `cpt-cf-settings-service-feature-replica-coherence`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Publish the Broadcast at the End of a Request](#publish-the-broadcast-at-the-end-of-a-request)
  - [Consume a Broadcast and Evict](#consume-a-broadcast-and-evict)
  - [Run the Replica's Consumer](#run-the-replicas-consumer)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Touched-Tenant Accumulation](#touched-tenant-accumulation)
  - [Key-Wide Eviction](#key-wide-eviction)
  - [Broadcast Outcome](#broadcast-outcome)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [End-of-Request Publish](#end-of-request-publish)
  - [Replica Consumer](#replica-consumer)
  - [The Broker as a Soft Dependency](#the-broker-as-a-soft-dependency)
  - [Publishing Identity and Grants](#publishing-identity-and-grants)
  - [Time-to-Live Backstop](#time-to-live-backstop)
  - [Coherence Is Observable](#coherence-is-observable)
  - [Coherence Tests](#coherence-tests)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Makes every replica of the gear converge on a value another replica set. At the end of each request that committed value changes, the Value Writer publishes one `cache_invalidate` broadcast per tenant the request touched on the cache topic of the platform Event Broker, and a request or reconcile pass that retired, reactivated, evolved or reclassified declarations — by the administrative path or by the module path — publishes one under the root tenant naming every key it changed; every replica, the writing one included, consumes it through an anonymous consumer group of its own and evicts every cached scope of each named key through its local cache's key-wide `invalidate`. The broker is a soft dependency resolved at first use, and `cache_ttl_seconds` bounds a replica's staleness whether or not a broadcast reaches it.

### 1.2 Purpose

The writing replica evicts its own cache inside the write (entry 2.8); the other replicas hold their own copies of the same effective values and keep serving them until something tells them to drop them. A single-replica deployment is coherent by construction, and R2 is the first release in which several replicas are the normal case, so convergence between them ships with it.

Three properties shape the feature.

**The signal is best-effort and the bound is not.** The broadcast is published inline at the end of the request, the request's whole publish phase bounded by one `publish_timeout_seconds`, never persisted, and never acknowledged. A broadcast that is refused, times out, or never arrives postpones convergence; it cannot leave a replica stale for longer than `cache_ttl_seconds`, the time-to-live the cache of entry 2.5 owns. The broker's absence or loss never fails gear init and never fails a write.

**The broadcast is not the consumer signal.** Replica coherence and consumer activation are separate concerns with separate audiences. `cache_invalidate` carries the full changed-key set of a tenant to the service's own trusted replicas, on a topic no consumer gear is granted anything on, with no subscription and no acknowledgement; the filtered, acknowledged `change_notification` is entry 2.13's and travels on a topic of its own.

**Nothing in it is an operation.** No actor can trigger, target or observe coherence: there is no endpoint, no SDK call and no parameter that reaches it. The publish is a step of the writing or declaration request, or of the reconcile pass, itself, and the eviction is a step of each replica's own lifecycle.

**Requirements**: `cpt-cf-settings-service-fr-replica-coherence`, `cpt-cf-settings-service-nfr-performance-read-cache`, `cpt-cf-settings-service-nfr-availability`

**Principles**: `cpt-cf-settings-service-principle-two-signal-split`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-event-broker` | Carries each `cache_invalidate` broadcast from the publishing replica to every replica's anonymous consumer group on the cache topic; its presence is not required for a write to commit |
| `cpt-cf-settings-service-actor-types-registry` | Holds the activation subject type the gear registers at init, and the cache topic and the `cache_invalidate` event type the consumer task registers once the broker resolves |
| `cpt-cf-settings-service-actor-internal-caller` | Reads through any replica and receives the value another replica set once that replica has evicted, or once `cache_ttl_seconds` has passed |
| `cpt-cf-settings-service-actor-platform-admin` | Makes the writes and the declaration retires, reactivations, evolutions and reclassifications whose requests end with a broadcast; holds no operation on coherence itself |
| `cpt-cf-settings-service-actor-tenant-admin` | Makes writes within its own subtree whose requests end with a broadcast; holds no operation on coherence itself |
| `cpt-cf-settings-service-actor-contributing-module` | Runs the reconcile passes that retire, revive, upgrade or reclassify in place its contributed declarations, each ending with a broadcast; holds no operation on coherence itself |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.5 Replica Coherence After a Set, §6.1 (Performance: Read-Path Caching; Availability), §2.2 System Actors (Event Broker), §8 Resolve Effective Value via Inheritance (an ancestor changes the value), §9 Acceptance Criteria (replica convergence)
- **Design**: [DESIGN.md](../DESIGN.md) — §2.3 (R2), §4.2 (Component: Cache & Invalidation, its `invalidate` operation and key-wide eviction; Component: Declaration Management and Component: Module Contribution Reconciler, the paragraphs on the end-of-request publish hook they invoke; Component: Value Writer, the `set` row), §4.3 (REST API — Setting Declarations: the *Evolve by re-declaring* sentence that an evolve publishes one broadcast under the root tenant naming the predecessor and the successor key, the *Re-declare to revive* sentence that a revive publishes the `cache_invalidate` broadcast under the root tenant, and the sentence that a `PATCH` changing classification or domain affinity publishes the same broadcast as a retire, whose `DELETE` publishes it under the root tenant; *Activation tracking*, the `change_set_id` definition), §4.4 (Events Consumed, the `cache_invalidate` row; Activation signals on the broker), §4.7 (GTS Type & Schema Identifiers, the activation signal subject), §4.8 (Publishing identity), §4.9 (Gear declaration, Capabilities; Lifecycle hooks, Gear init and Gear lifecycle; The Event Broker as a soft dependency), §7 (Feature Metrics, `settings_cache_broadcast_total`; Testing Architecture)
- **Design (activation)**: [DESIGN-activation.md](../DESIGN-activation.md) — §1.1 (Two distinct distributions), §3.2 (Constraints), §4.1 (Entity `CacheInvalidation`), §4.2 (Component: Event Broker Client, its configuration, `publish_cache_invalidation` and *Broker topology*; Component: Cache Invalidation Broadcast), §4.4 (the `cache_invalidate` row and the subject pair), §5.2 (Broker limits in v1), §7 (NFR Mapping, Fan-out bounds, Testing Architecture, NFR Verification Mapping)
- **ADR**: [ADR-003](../ADR/ADR-003-thin-event-pull-report-activation.md) — signals carry identifiers only
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.12
- **Dependencies**: entry 2.8 for the Value Writer whose request this feature ends — the per-change local eviction `inst-vw-commit-8` of `cpt-cf-settings-service-algo-value-writes-commit`, and the batch's `inst-vw-batch-8` of `cpt-cf-settings-service-flow-value-writes-batch`, after which the publish runs; entry 2.5 for the cache whose `invalidate` the peers call and whose `cache_ttl_seconds` (`cpt-cf-settings-service-dod-value-resolution-cache-ttl`) bounds a lost broadcast; entry 2.3 (`cpt-cf-settings-service-feature-setting-declarations`) for the retire, reactivate, evolve and `PATCH` paths of a declaration, the second hook after which the publish runs; entry 2.10 (`cpt-cf-settings-service-feature-module-contributions`) for the reconcile pass that retires, revives, upgrades or reclassifies in place contributed declarations, the third hook after which the publish runs; entry 2.9 for the principal and token scope of the Secret Manager (`cpt-cf-settings-service-algo-secret-values-reference`), under which this feature publishes and consumes — the broker context with the root tenant as subject tenant is built here from the same principal builder, parameterised by subject tenant (DESIGN §4.8 *Publishing identity*), and this feature defines no principal of its own.
- **Design components**: `cpt-cf-settings-service-component-cache-invalidation-broadcast`, `cpt-cf-settings-service-component-event-broker-client` (its cache half: `publish_cache_invalidation`, the cache topic, the `cache_invalidate` type and the replica's consumer); extends `cpt-cf-settings-service-component-value-writer` (the end-of-request publish hook), `cpt-cf-settings-service-component-cache-and-invalidation` (eviction on receipt), and `cpt-cf-settings-service-component-declaration-management` and `cpt-cf-settings-service-component-module-contribution-reconciler` (they invoke the end-of-request publish hook)
- **Sequences**: the broadcast step of `cpt-cf-settings-service-seq-activation-change-set-notify`; the sequence itself is entry 2.13's
- **Not applicable**: Subscriptions, the `change_notification` signal, the activation state and the key's mutation lock are entry 2.13; this feature introduces the end-of-request publish hook 2.13 extends, and the broadcast precedes the notifications there. Invalidation on a hierarchy change — a re-parent or a mid-chain insertion — is a stated non-goal of the broadcast, owned by the cache of entry 2.5 and bounded by `cache_ttl_seconds` while the Tenant Resolver publishes no hierarchy-change signal. A restriction change of entry 2.7 publishes no broadcast: the cache holds values, never access decisions, which every replica decides afresh on each read (DESIGN §4.2 *Cache & Invalidation*). Any request by which an actor triggers, targets or observes coherence is prohibited by the PRD and does not exist. The domain events of DESIGN §4.4 *Events Emitted* on the broker, and consuming `tenant_deleted`, are carried by no entry of this wave (DECOMPOSITION §1). The optional subject pair `subject_type` / `subject_id` in the payload stays absent until subject-scoped writes exist. The publish-to-deliver latency histogram, which also observes `cache_invalidate`, ships with the activation metrics of entry 2.13. No table, no migration, no endpoint and no SDK type is added — the `CacheInvalidation` body and its schemas live in the gear, and only the three identifiers (the activation subject type, the cache topic and the `cache_invalidate` type) live in the SDK's `gts` module — so there is no data to roll back and no compile-time gate beyond the gear's own build: removing the broker from a deployment returns every replica to `cache_ttl_seconds` convergence with writes unaffected. No feature-local deviation from a shared baseline is taken.

## 2. Actor Flows (CDSL)

Coherence has no request surface of its own, and the three flows name one actor by exception: the publish flow is triggered by an administrator's write or declaration request, or by a contributing module's reconcile pass, and the lifecycle flow by the runtime, while the actor of each flow is the Event Broker, the system actor the PRD names for the signal.

**Use cases**: `cpt-cf-settings-service-usecase-resolve-effective-value`

Throughout, a tenant in a broadcast is a bare lowercase UUID string, and platform scope is the root tenant's id, an ordinary tenant id and never an absent one. "The cache topic" is `gts.cf.core.events.topic.v1~cf.core.settings.cache.v1` and "the `cache_invalidate` type" is `gts.cf.core.events.event.v1~cf.core.settings.cache_invalidate.v1~`.

### Publish the Broadcast at the End of a Request

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-replica-coherence-publish`

**Actor**: `cpt-cf-settings-service-actor-event-broker`

**Success Scenarios**:
- A request that committed changes at one or more tenants hands the broker one `cache_invalidate` per tenant it touched, each carrying the request's `change_set_id` and that tenant's full changed-key set, after every commit of the request
- A request or reconcile pass that retired, reactivated, evolved or reclassified declarations, by the administrative path or by the module path, hands the broker `cache_invalidate` under the root tenant naming every key it changed — for an evolve or an upgrade the predecessor and the successor — with the request's correlation id, or the id the pass minted, as `change_set_id`
- A tenant whose changed-key set exceeds the broker's 60 KiB event limit is covered by several broadcasts that together carry every key once
- The caller's response carries the outcomes of its changes whatever happened to the broadcasts

**Error Scenarios**:
- No broker resolvable, the cache topic and the `cache_invalidate` type not yet registered, or the root tenant's id not yet known: every broadcast of the request is counted `skipped`, and the write stands
- The broker refuses a publish — a missing grant included — or does not accept it within the request's publish budget of one `publish_timeout_seconds`: that publish is counted `failed`, the request's remaining publishes are counted `skipped`, and the peers converge within `cache_ttl_seconds`; a publish the budget cuts short after the broker accepted an earlier one of the request is counted `skipped`, not `failed`
- A publish failed or exceeded its own timeout a few seconds ago — a refusal of the event or of a grant excepted; a request that merely ran out of its budget does not mute: the broker client answers no broker for the request, every broadcast of it is counted `skipped`, and the peers converge within `cache_ttl_seconds`

**Steps**:
1. [x] - `p1` - A request of the Value Writer — a single `set`, a `set` batch with its `set` and `revert` entries, a `revert`, a `remove_value` or a `clone` — reaches its end with every change committed or refused and the local cache already evicted for each committed change (`inst-vw-commit-8`, `inst-vw-batch-8`); a single `set`, `revert`, `remove_value` or `clone` is a one-change request that ends after `cpt-cf-settings-service-algo-value-writes-commit` (`inst-vw-commit-8`) and reaches the same hook - `inst-rc-publish-1`
2. [x] - `p1` - A request or reconcile pass that retired, reactivated, evolved or reclassified declarations — by the administrative path of entry 2.3 (`cpt-cf-settings-service-feature-setting-declarations`) or by the module path of entry 2.10 (`cpt-cf-settings-service-feature-module-contributions`: retire, revive, upgrade and in-place reclassification of contributed declarations); a reclassification is a `PATCH` that changed a declaration's classification or domain affinity, or a reconcile pass that changed a contributed declaration's classification or domain affinity at the same major — reaches its end the same way, with the declarations committed and their keys evicted locally; it hands accumulation one committed change at the root tenant, whose id it takes from the broker client's cache (`inst-rc-lifecycle-4`) and never resolves itself, for every key it changed — for an evolve or an upgrade, the retired predecessor and the new successor — and carries as its `change_set_id` the request's correlation id, or the id a reconcile pass mints once, so it publishes under the root tenant with every key it changed in `changed_keys` (DESIGN §4.3, §4.4 *Events Consumed*) - `inst-rc-publish-9`
3. [x] - `p1` - Invoke touched-tenant accumulation over the request's change outcomes; **IF** no change committed → **RETURN** with nothing published and nothing counted - `inst-rc-publish-2`
4. [x] - `p1` - Resolve `EventBrokerApi` from the `ClientHub` with a lookup that answers "none" rather than failing, once for the whole request, and hold that handle with the publishing context for every publish of it; read whether the consumer task has registered the cache topic and the `cache_invalidate` type and has cached the root tenant's id (`inst-rc-lifecycle-4`); **IF** the root tenant's id is not yet known → build nothing, discard what accumulation gathered, count one `skipped` per tenant the request touched, then continue to the return step - `inst-rc-publish-5`
5. [x] - `p1` - **FOR EACH** touched tenant, in the order the request first touched it → build one `CacheInvalidation` body carrying the request's `change_set_id`, the tenant in `tenant` — for a change at the root tenant, the root tenant's id read in `inst-rc-publish-5` — and every key the request changed there in `changed_keys`, with the optional `subject_type` and `subject_id` absent - `inst-rc-publish-3`
   1. [x] - `p1` - Wrap the body in the broker-native envelope: `type_id` the `cache_invalidate` type, `tenant_id` the root tenant's id read in `inst-rc-publish-5` — the tenant every replica's single interest names, and the event type's default partition key — `subject` the tenant in `data.tenant`, `subject_type` `gts.cf.core.settings.activation.v1~`, `source` the gear name `settings-service`, an `id` unique to the event, `occurred_at` the time of publishing, `trace_parent` the request's trace context when one is present, and the body as `data`; the event is published by the broadcast outcome (`cpt-cf-settings-service-algo-replica-coherence-outcome`) in step 6, under the publishing identity - `inst-rc-publish-7`
   2. [x] - `p1` - **IF** the serialised event, envelope included, would exceed the broker's 60 KiB event size limit → split `changed_keys` across several events, packing the keys in order until the next key would exceed the limit, each event with the same `change_set_id` and `tenant`, an `id` of its own, and each under the limit, together carrying every key exactly once - `inst-rc-publish-4`
6. [x] - `p1` - **FOR EACH** built event, in order → invoke the broadcast outcome with it, the broker's availability, the registration state, whether the root tenant's id is known, and whether an earlier publish of this request has failed - `inst-rc-publish-6`
7. [x] - `p1` - **RETURN** to the request — the Value Writer, the declaration path or the reconcile pass — which answers the caller with the outcomes of its changes whatever the broadcasts' outcomes; no broadcast carries a value, a secret, a secret reference or an actor, only the change set id, the tenant and setting keys - `inst-rc-publish-8`

### Consume a Broadcast and Evict

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-replica-coherence-consume`

**Actor**: `cpt-cf-settings-service-actor-event-broker`

**Success Scenarios**:
- Every replica, the writing one included, drops every cached scope of each key the broadcast names, and its next read of those keys at that tenant, or at a descendant a `cascading` value covers, resolves the committed value
- A repeated delivery evicts again and changes nothing else

**Error Scenarios**:
- A delivery whose `data` carries no usable `tenant` or `changed_keys` evicts nothing; the replica's entries expire within `cache_ttl_seconds`
- A key whose declaration is missing, retired or unreadable on this replica is evicted key-wide like any other, since the consume path reads no declaration

**Steps**:
1. [x] - `p1` - The Event Broker delivers an event of the cache topic to this replica's anonymous consumer group through the replica's single interest, and the consumer continues the publisher's trace from `trace_parent` when the event carries one. The observation of `settings_notify_publish_to_deliver_seconds` at this receipt lands with entry 2.13 (`cpt-cf-settings-service-flow-consumer-activation-lifecycle`) - `inst-rc-consume-1`
2. [x] - `p1` - **IF** the event's `type_id` is not the `cache_invalidate` type → ignore it and **RETURN** - `inst-rc-consume-2`
3. [x] - `p1` - **IF** `data.tenant` is not a UUID, **OR** `data.changed_keys` is absent or not a list of strings → log at `warn` with the event's `id`, evict nothing, and **RETURN**; `cache_ttl_seconds` bounds what this replica serves meanwhile - `inst-rc-consume-3`
4. [x] - `p1` - Ignore any attribute of `data` this version does not know, so an additive attribute of the same event type is harmless - `inst-rc-consume-4`
5. [x] - `p1` - Invoke key-wide eviction with the keys in `data.changed_keys` and the tenant in `data.tenant`; the deliveries already waiting on the stream behind this one are taken with it, and the union of their keys is evicted in one call - `inst-rc-consume-5`
6. [x] - `p1` - **RETURN** completed: there is no acknowledgement to send, the group's offset advances in memory, a repeated delivery takes the same path and evicts what is already gone, and the broadcast the writing replica published itself is processed like any other - `inst-rc-consume-6`

### Run the Replica's Consumer

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-replica-coherence-lifecycle`

**Actor**: `cpt-cf-settings-service-actor-event-broker`

**Success Scenarios**:
- Gear init completes whether or not a broker exists; once one resolves, the cache topic and the `cache_invalidate` type are registered and the replica consumes broadcasts through an anonymous group of its own
- On `stop` the replica leaves its group and deletes it within the runtime's shutdown deadline

**Error Scenarios**:
- No broker resolvable: the consumer is idle and the resolution is retried; writes commit and broadcasts count `skipped`
- Registration refused, or the group's creation or join refused for a missing `define` or `consume` grant: the consumer stays idle, the refusal is logged, and the step is retried with backoff — a refused join against the same group, so the broker holds at most one group for the replica
- The broker lost mid-life: the consumer pauses, deletes its group best-effort, and on the next successful resolution creates a new group and joins again
- A crash leaves the group behind in the broker, which does not reap consumer groups

**Steps**:
1. [x] - `p1` - Register the activation subject type `gts.cf.core.settings.activation.v1~` through the link-time inventory the Types Registry drains when it initializes, ahead of the gear's init and without resolving the broker, and at gear init refuse the configuration when `publish_timeout_seconds` is `0` or above `10`; init neither resolves the broker nor waits for it, and the broker is never in the gear's `deps` - `inst-rc-lifecycle-1`
2. [x] - `p1` - On `start`, run the consumer as a `stateful` background task of the gear's lifecycle under the runtime's cancellation token, and log its start at `info`; every wait with backoff in this flow is exponential from 1 s to a 30 s cap with jitter, and is not configurable - `inst-rc-lifecycle-2`
3. [x] - `p1` - **WHILE** the cancellation token has not fired - `inst-rc-lifecycle-3`
   1. [x] - `p1` - Resolve `EventBrokerApi` from the `ClientHub` with a lookup that answers "none" rather than failing; **IF** none → stay idle, wait with backoff, and repeat this step; on the first successful resolution resolve the root tenant's id through the Tenant Resolver once and cache it for the process — never during gear init, a reconcile pass or a transaction — so that the consumer's interest and every end-of-request publish take it from that cache; retry with the same backoff while it fails - `inst-rc-lifecycle-4`
   2. [x] - `p1` - **IF** the cache topic and the `cache_invalidate` type are not yet registered → register the topic, and the type as a derived event schema of the broker's event base that names `gts.cf.core.settings.activation.v1~` as its allowed subject type and keeps the broker's default partition key `tenant_id`, idempotently; **IF** the registration is refused or fails → log at `warn`, stay idle, wait with backoff, and repeat; on success record the registration so the publish flow stops counting `skipped` for that reason; a publish or a join refused within the broker's specification-refresh interval after the first registration is expected and is retried like any refusal - `inst-rc-lifecycle-5`
   3. [x] - `p1` - **IF** the replica holds no consumer group → create its anonymous consumer group once, under the publishing identity, owned by the root tenant; **IF** the broker refuses the creation → log at `warn`, stay idle, wait with backoff, and repeat this step - `inst-rc-lifecycle-12`
   4. [x] - `p1` - Join the cache topic through that group with one interest — the `cache_invalidate` type with `tenant_id` the root tenant's id cached in `inst-rc-lifecycle-4` — keeping offsets in memory and starting from the latest position; **IF** the broker refuses the join → log at `warn`, stay idle, wait with backoff, and retry the join against the same group, never creating a second one - `inst-rc-lifecycle-6`
   5. [x] - `p1` - Take deliveries and hand each to the consume flow, repeating the `ClientHub` lookup before each wait for the next delivery, until the cancellation token fires or the broker is lost - `inst-rc-lifecycle-7`
   6. [x] - `p1` - **IF** the broker is lost — the delivery stream ended or errored, or the `ClientHub` lookup returns no broker → log at `warn`, pause taking deliveries, make one best-effort attempt to leave and delete the group — whether or not the deletion succeeded, the replica holds no group afterwards; a failed deletion is logged at `warn` and the group is left as a crashed replica's is — and return to resolving the broker; only after that attempt does the resume create a new group and join again; a broadcast published while the consumer is paused is never delivered to it, and `cache_ttl_seconds` bounds that gap - `inst-rc-lifecycle-8`
4. [x] - `p1` - On `stop`, stop taking deliveries, leave the group, and delete it best-effort with `delete_consumer_group`, within the runtime's shutdown deadline - `inst-rc-lifecycle-9`
5. [x] - `p1` - **IF** the leave or the deletion fails or the deadline passes → log at `warn`; the group stays in the broker exactly as a crashed replica's does, holding no delivery any live replica needs - `inst-rc-lifecycle-10`
6. [x] - `p1` - **RETURN** with the consumer stopped, logged at `info` - `inst-rc-lifecycle-11`

## 3. Processes / Business Logic (CDSL)

### Touched-Tenant Accumulation

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-replica-coherence-accumulate`

**Input**: The outcomes of one request's changes, in the order the request evaluated them, each with its key, the tenant it targeted and whether it committed

**Output**: The tenants the request changed values at, in the order it first touched them, each with the set of keys it changed there; for a declaration retire, reactivate, evolve or reclassification, by the administrative path of entry 2.3 or by a reconcile pass of entry 2.10 that retired, revived, upgraded or reclassified in place (classification or domain affinity changed at the same major) contributed declarations, the root tenant with every key it changed — for an evolve or an upgrade, the retired predecessor and the new successor

**Steps**:
1. [x] - `p1` - Start an empty, insertion-ordered collection of tenants, each with an empty key set - `inst-rc-accumulate-1`
2. [x] - `p1` - **FOR EACH** change outcome, in order - `inst-rc-accumulate-2`
   1. [x] - `p1` - **IF** the change did not commit — refused at a gate, at validation, on a stale tag, for an unavailable dependency, or at the commit → skip it; a refused change stored nothing and has nothing to evict - `inst-rc-accumulate-3`
   2. [x] - `p1` - Take the tenant the change wrote at — the target of a `clone`, never its source, and the root tenant's id for a platform-scope write, which every `global` write is, and for a declaration retire, reactivate, evolve or reclassification by either path, the reconcile pass's retire, revive, upgrade and in-place reclassification (classification or domain affinity changed at the same major) included, the root tenant's id the broker client cached (`inst-rc-lifecycle-4`) — as a bare lowercase UUID string - `inst-rc-accumulate-4`
   3. [x] - `p1` - Add the change's key to that tenant's key set; a key changed more than once at one tenant appears once - `inst-rc-accumulate-5`
3. [x] - `p1` - **RETURN** the collection; it is empty when no change committed - `inst-rc-accumulate-6`

### Key-Wide Eviction

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-replica-coherence-evict`

**Input**: A set of setting keys and the tenant a broadcast names

**Output**: Every cached scope of those keys evicted on this replica, key-wide for every key

**Steps**:
1. [x] - `p1` - Take the keys as the broadcast names them and read no declaration: the eviction is the same for every key whatever its Scope Class, so a missing, retired or unreadable declaration changes nothing, and the named tenant does not narrow it - `inst-rc-evict-1`
2. [x] - `p1` - **FOR EACH** key - `inst-rc-evict-2`
   1. [x] - `p1` - Evict every cached scope of the key through the cache's key-wide `invalidate` - `inst-rc-evict-3`
   2. [x] - `p1` - Every cached scope of the key is gone, so the descendant scopes a `cascading` value covers, and every tenant that cached a `global` value under its own scope, re-resolve on their next read - `inst-rc-evict-4`
   3. [x] - `p1` - A `local` key's other tenants, evicted with the named one, re-resolve on their next read; that extra read is the cost of a consume path that reads no declaration - `inst-rc-evict-5`
   4. [x] - `p1` - Leave the cached entries of every key the broadcast does not name in place - `inst-rc-evict-6`
3. [x] - `p1` - **RETURN** having evicted on this replica only; each eviction moves the key's generation as the cache's `invalidate` does, so a read in flight on this replica cannot store what it read before the eviction - `inst-rc-evict-7`

### Broadcast Outcome

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-replica-coherence-outcome`

**Input**: One built broadcast, whether a broker resolved, whether the cache topic and the `cache_invalidate` type are registered, whether the root tenant's id is known, whether an earlier publish of the same request has failed, and whether the request's publish budget is spent

**Output**: The broadcast's result — `sent`, `skipped` or `failed` — counted once on `settings_cache_broadcast_total{result}`, and whether the request's remaining publishes are to be skipped

**Steps**:
1. [x] - `p1` - **IF** no broker resolved — the broker client also answers none for a short cooling window of a few seconds after a publish that failed or exceeded its own timeout, a refusal of the event or of a grant excepted; a request that merely ran out of its budget does not mute — **OR** the cache topic and the `cache_invalidate` type are not yet registered, **OR** the root tenant's id is not yet known → count `skipped` and **RETURN** `skipped` - `inst-rc-outcome-1`
2. [x] - `p1` - **IF** an earlier publish of the same request has failed, **OR** the request's publish budget is spent → count `skipped` and **RETURN** `skipped`, so a request pays at most one `publish_timeout_seconds` for its whole publish phase - `inst-rc-outcome-2`
3. [x] - `p1` - Publish the broadcast through `publish_cache_invalidation`, waiting for the broker to accept it at most until the request's publish budget runs out: one `publish_timeout_seconds` (default `2`) from the start of the request's publish phase, shared by its broadcasts and the `change_notification`s that follow them on the same session - `inst-rc-outcome-3`
4. [x] - `p1` - **IF** the broker accepted it within that bound → count `sent` and **RETURN** `sent` - `inst-rc-outcome-4`
5. [x] - `p1` - **ELSE** — refused, a missing `produce` grant included, or not accepted within the bound → drop the publish, count `failed`, log at `warn` with the change set id, the tenant and the broker's reason, mark the request so its remaining publishes are skipped, and **RETURN** `failed`; a publish the budget cut short after the broker accepted an earlier publish of the request is dropped and counted `skipped` instead, nothing logged at `warn` — the request ran out of time, the broker did not fail - `inst-rc-outcome-5`

## 4. States (CDSL)

Not applicable. A broadcast carries no state: it is built, published once and forgotten, and nothing records whether it arrived. The consumer's running, idle and paused conditions are conditions of the gear's lifecycle task, decided afresh by each resolution of the broker, not states of an entity this feature stores; the cache entries it evicts are removed rather than transitioned, as entry 2.5 models them.

## 5. Definitions of Done

### End-of-Request Publish

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-replica-coherence-publish`

At the end of every request of the Value Writer that committed at least one change — a single `set`, a batch, a `revert`, a `remove_value` or a `clone` — the system **MUST** publish, after every commit of the request and after the local eviction of each committed change, one `cache_invalidate` per tenant the request touched, carrying the request's `change_set_id`, that tenant in `data.tenant` (the root tenant's id for a platform-scope write) and every key the request changed at that tenant, on the cache topic under the root tenant's `tenant_id`, with `subject` the tenant of the change and `subject_type` `gts.cf.core.settings.activation.v1~`. At the end of a request or reconcile pass that retired, reactivated, evolved or reclassified declarations — by the administrative path of entry 2.3, a reclassification being a `PATCH` that changed a declaration's classification or domain affinity, or by the module path of entry 2.10, a reconcile pass that retired, revived, upgraded or reclassified in place (classification or domain affinity changed at the same major) contributed declarations — the system **MUST** publish `cache_invalidate` under the root tenant, with the root tenant's id in `data.tenant`, every key the request or pass changed in `changed_keys` — for an evolve or an upgrade, the retired predecessor and the new successor — and as `change_set_id` the request's correlation id, or the id the reconcile pass minted once. A key set whose serialised event, envelope included, exceeds the broker's 60 KiB event limit **MUST** be split into several broadcasts, the keys packed in order until the next would exceed the limit, that together carry every key once. The request's whole publish phase **MUST** be bounded by one `publish_timeout_seconds` (default `2`), and after the first refused or timed-out publish, or once that budget is spent, the request's remaining publishes **MUST** be skipped; for a few seconds after a publish that failed or exceeded its own timeout, later requests **MUST** publish nothing and count `skipped`; a request that merely ran out of its budget **MUST NOT** mute them. A request that committed nothing **MUST** publish nothing. The caller's response **MUST NOT** depend on any broadcast's outcome, and no broadcast **MUST** carry a value, a secret or anything other than identifiers.

**Implements**:
- `cpt-cf-settings-service-flow-replica-coherence-publish`
- `cpt-cf-settings-service-algo-replica-coherence-accumulate`
- `cpt-cf-settings-service-algo-replica-coherence-outcome`

**Constraints**: `cpt-cf-settings-service-constraint-activation-two-events-per-change-set`, `cpt-cf-settings-service-constraint-activation-tenant-in-payload`, `cpt-cf-settings-service-constraint-activation-identifiers-only-payload` — the `cache_invalidate` half of each

**Touches**:
- Entities: `CacheInvalidation`, Value Writer end-of-request publish hook, the declaration retire, reactivate, evolve and `PATCH` paths, the contribution reconcile pass, Event Broker Client `publish_cache_invalidation`

### Replica Consumer

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-replica-coherence-consumer`

Every replica **MUST** consume `cache_invalidate` through an anonymous consumer group of its own on the cache topic, with one interest on the root tenant for the `cache_invalidate` type, offsets in memory from the latest position, and **MUST** evict every cached scope of each named key through its cache's key-wide `invalidate`, whatever the key's Scope Class, reading no declaration on the consume path. The group **MUST** be created once and kept across a refused join, the join retried with backoff against the same group, so that after any number of retries the broker holds at most one group for the replica; on `stop` and on a lost broker the group **MUST** be deleted best-effort before any new one is created. The writing replica **MUST** process its own broadcast like any other, a repeated delivery **MUST** be harmless, and the handler **MUST** complete for every delivery, a malformed one included, without an acknowledgement protocol.

**Implements**:
- `cpt-cf-settings-service-flow-replica-coherence-consume`
- `cpt-cf-settings-service-algo-replica-coherence-evict`
- `cpt-cf-settings-service-flow-replica-coherence-lifecycle`

**Constraints**: `cpt-cf-settings-service-constraint-activation-tenant-in-payload`

**Touches**:
- Entities: `CacheInvalidation`, effective-value cache `invalidate`, the replica's anonymous consumer group

### The Broker as a Soft Dependency

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-replica-coherence-soft-dependency`

The Event Broker **MUST** be fetched from the `ClientHub` at first use (`try_get`, as the tenant resolver is while the broker SDK carries no REST projection for a `#[toolkit::consumes]` declaration) and never listed in `deps`; gear init **MUST NOT** resolve it, wait for it or fail for its absence. The gear **MUST** register the activation subject type at init with its other schemas, and the consumer task **MUST** register the cache topic and the `cache_invalidate` type once the broker first resolves, idempotently and retried until it succeeds. The consumer task **MUST** retry the broker's resolution until it succeeds, so a broker that appears later is picked up, and on its first successful resolution **MUST** resolve the root tenant's id through the Tenant Resolver once and cache it for the process, retried with the same backoff while it fails, and a broker lost mid-life **MUST** pause the consumer until the next successful resolution. On `stop` the consumer **MUST** stop taking deliveries, leave its group and delete it within the runtime's shutdown deadline. Without a broker, before the registration succeeds, or before the root tenant's id is known, a write **MUST** commit and its broadcasts **MUST** count `skipped`.

**Implements**:
- `cpt-cf-settings-service-flow-replica-coherence-lifecycle`
- `cpt-cf-settings-service-algo-replica-coherence-outcome`

**Constraints**: `cpt-cf-settings-service-constraint-activation-broker-only`, `cpt-cf-settings-service-constraint-activation-in-settings-gear` (the lives-in-the-gear half; the session-pooling clause belongs to consumer activation)

**Touches**:
- Entities: gear declaration (`deps` unchanged, `stateful` capability), Gear init, Gear lifecycle, Types Registry registration of the cache topic, the `cache_invalidate` type and the activation subject type

### Publishing Identity and Grants

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-replica-coherence-identity`

The gear **MUST** publish and consume on the broker under the principal and token scope of the Secret Manager (`cpt-cf-settings-service-algo-secret-values-reference`) — the service principal it uses toward the Credential Store, with the service's own subject type and the first-party token scope `*` — in a broker context with the root tenant as subject tenant, which this feature builds from the same principal builder, parameterised by subject tenant (DESIGN §4.8 *Publishing identity*), so the replica's anonymous groups are owned by the root tenant; this feature **MUST NOT** define a principal of its own. The root tenant's id the context, the consumer interest and every broadcast envelope carry **MUST** be resolved lazily by the consumer task, the broker client's background task, through the Tenant Resolver on its first successful resolution and cached for the process, and **MUST NOT** be resolved during gear init, during a reconcile pass or inside a transaction; every end-of-request publish — of a value write, a declaration path or a reconcile pass — **MUST** take it from that cache, and a publish that runs before it is known **MUST** be skipped and counted `skipped`, `cache_ttl_seconds` bounding the staleness. The deployment's grants for that identity are `produce` on the cache topic, the `cache_invalidate` type and the activation subject type, `consume` on the cache topic, `cache_invalidate` and the activation subject type through the replicas' own groups, and `define` for those groups, each covering events of every tenant. A missing grant **MUST** leave the gear up: a missing `produce` grant counts the broadcast `failed`, a missing `consume` or `define` grant or a refused registration leaves the consumer idle with `cache_ttl_seconds` bounding staleness, and each case **MUST** be observable through the counter or a `warn` log.

**Implements**:
- `cpt-cf-settings-service-flow-replica-coherence-publish`
- `cpt-cf-settings-service-flow-replica-coherence-lifecycle`
- `cpt-cf-settings-service-algo-replica-coherence-outcome`

**Constraints**: `cpt-cf-settings-service-constraint-activation-broker-only`

**Touches**:
- Entities: Secret Manager service principal, broker grants `produce` / `consume` / `define`

### Time-to-Live Backstop

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-replica-coherence-ttl-backstop`

`cache_ttl_seconds` (default 30 s, also its ceiling) **MUST** stay the cache's own knob, unchanged by this feature, and **MUST** be the bound on a replica's staleness for every broadcast that does not reach it: one counted `skipped` or `failed`, one published while its consumer is idle, paused or not yet joined, and one lost in delivery. No replica **MUST** serve a value staler than `cache_ttl_seconds` after a change, whether or not the broadcast arrives. This feature takes no lock; the requirement that the settings database be reached through session pooling or a direct connection belongs to the lock-based features of entry 2.13 and is noted here, not owned. This definition of done is verified by the *Dropped broadcast* integration test and by `cpt-cf-settings-service-dod-value-resolution-cache-ttl`, not by code of its own; its `@cpt-dod` marker lives on the *Dropped broadcast* integration test.

**Implements**:
- `cpt-cf-settings-service-dod-replica-coherence-tests` — its *Dropped broadcast* integration test

**Constraints**: `cpt-cf-settings-service-constraint-activation-broker-only`

**Touches**:
- Entities: effective-value cache time-to-live

### Coherence Is Observable

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-replica-coherence-observability`

The system **MUST** count every broadcast on `settings_cache_broadcast_total{result}`: `sent` for one the broker accepted; `skipped` for no broker resolvable — a publish that recently failed or exceeded its own timeout included — the signal types not yet registered, the root tenant's id not yet known, or a publish skipped after an earlier refusal or timeout in the same request or once its publish budget was spent, the one the budget cut short after the broker accepted an earlier publish of the request included; `failed` for a refusal, a missing grant included, or no acceptance within `publish_timeout_seconds`. The consumer **MUST** log its start and stop at `info`, and a refused publish, a refused registration, a refused group creation or join, a lost broker, a malformed delivery and a group it could not delete at `warn`. No log line or metric label **MUST** carry a value or a secret. Each delivery **MUST** run inside a `cache_invalidate.consume` span carrying the `change_set_id` and `tenant` of its body, a child of the publisher's trace when the event carries `trace_parent` and a fresh root when it does not; deliveries taken off the stream together evict the union of their keys inside one `cache_invalidate.evict` span that follows from each of theirs.

**Implements**:
- `cpt-cf-settings-service-algo-replica-coherence-outcome`
- `cpt-cf-settings-service-flow-replica-coherence-lifecycle`
- `cpt-cf-settings-service-flow-replica-coherence-consume`

**Constraints**: `cpt-cf-settings-service-constraint-activation-identifiers-only-payload`

**Touches**:
- Entities: metrics, logs

### Coherence Tests

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-replica-coherence-tests`

The feature **MUST** ship with these tests, each implementing or extending a row of DESIGN §7 or DESIGN-activation §7, or a rule of DESIGN-activation §4.2 where no row exists:

- Unit, with a broker mock capturing broadcasts: payloads carry identifiers only — no value or secret in `cache_invalidate`; one broadcast per tenant the request touched, published under the root tenant with the change's tenant in `data.tenant`, the tenant's full key set and the request's `change_set_id`; a tenant's key set over 60 KiB, measured on the serialised event including the envelope, published as several broadcasts, each under the limit, together carrying every key once; one publish stalled past `publish_timeout_seconds` counted `failed`, the request's remaining publishes counted `skipped`, and the request paying one timeout; a `publish_timeout_seconds` of `0` or above `10` refused at init; no broker resolvable counted `skipped` with the write committed; a refused publish counted `failed`; key-wide eviction for every key, a `local` one and one with no declaration included, with no declaration read; a repeated delivery; retire, reactivate, evolve and reclassification by the administrative path, and retire, revive, upgrade and in-place reclassification by a reconcile pass, each publishing under the root tenant naming every key it changed, the predecessor and the successor for an evolve or an upgrade (DESIGN §7 Level 1 *Declaration change broadcast*); a restriction change publishing nothing (DESIGN §7 Level 1 *Restriction change publishes nothing*); the broadcast halves of DESIGN §7 Level 1 *Set — commit + publish* — the value written, the local cache evicted, then `cache_invalidate` published — and *Set — no broker resolvable* — the value committed, `settings_cache_broadcast_total{result="skipped"}` incremented, no error to the caller; and the Level 1 rows of DESIGN-activation §7 — *Refused join leaks no group*, *Malformed delivery* (logged at `warn`, nothing evicted; a delivery with an unknown `data` attribute evicts its keys), *Group deleted on stop*, *Lost broker pauses and resumes*, *Registration refused*, *Publish accepted counts `sent`* and *Root tenant unknown → `skipped`*.
- Integration, **required**, DESIGN-activation §7 Level 2 *Two-replica convergence*: two `SettingsService` gears over one database and a delivering broker double hosted by the test, which fans each `cache_invalidate` out to every joined group and exposes a join barrier and a drop switch, with a hierarchy fixture holding a descendant tenant for the cascading case; the test waits on the join barrier until both replicas are ready before the write, makes a warm read on replica B, writes on replica A, and polls B with a deadline well below `cache_ttl_seconds` until it serves the new value, for a `local` key at the written tenant and for a `cascading` key at the descendant too.
- Integration, **required**, DESIGN-activation §7 Level 2 *Dropped broadcast*: the same pair with the drop switch on and `cache_ttl_seconds` set to 1–2 s; B is asserted to serve the previous value before the TTL elapses and the new one after the TTL plus a stated margin.
- E2E: the replica-coherence rows of both designs — *Cascading override + cross-instance re-resolution* and *Replica cache coherence (broadcast)* — in an environment that runs a broker and a second replica.

**Implements**:
- `cpt-cf-settings-service-flow-replica-coherence-publish`
- `cpt-cf-settings-service-flow-replica-coherence-consume`
- `cpt-cf-settings-service-flow-replica-coherence-lifecycle`
- `cpt-cf-settings-service-algo-replica-coherence-accumulate`
- `cpt-cf-settings-service-algo-replica-coherence-evict`
- `cpt-cf-settings-service-algo-replica-coherence-outcome`

**Constraints**: `cpt-cf-settings-service-constraint-activation-identifiers-only-payload`, `cpt-cf-settings-service-constraint-activation-two-events-per-change-set`

**Touches**:
- Entities: `CacheInvalidation`, broker mock, delivering broker double, `settings_cache_broadcast_total`

## 6. Acceptance Criteria

- [x] A `set` batch changing two keys at one tenant and one key at another publishes exactly two `cache_invalidate` broadcasts after its commits, each under the root tenant's `tenant_id`, each with the request's `change_set_id`, the tenant in `data.tenant` and every key changed there; the value is written and the local cache evicted before the broadcast is published (unit)
- [x] A single `set`, `revert`, `remove_value` and `clone` each publish one broadcast for the tenant they wrote at — for a `clone`, the target — and a request that committed no change publishes nothing (unit)
- [x] A platform-scope write, a `global` one included, publishes under `data.tenant` the root tenant's id (unit)
- [x] A declaration retire, reactivate, evolve or reclassification by the administrative path of entry 2.3, and a reconcile pass of entry 2.10 that retired, revived, upgraded or reclassified in place (classification or domain affinity changed at the same major) contributed declarations, publishes under the root tenant naming every key it changed — for an evolve or an upgrade, the retired predecessor and the new successor — with the request's correlation id, or the id the pass minted once, as `change_set_id` (unit)
- [x] A restriction change (`set_restriction`, `clear_restriction`) evicts locally and publishes no broadcast (unit)
- [x] No broadcast carries a setting value, a secret, a secret reference or an actor; `data` holds `change_set_id`, `tenant` and `changed_keys` only (unit)
- [x] A tenant's key set whose serialised event, envelope included, exceeds 60 KiB is published as several broadcasts, each under the limit, together carrying every key once (unit)
- [x] A publish the broker does not accept within `publish_timeout_seconds` is counted `failed`, the request's remaining publishes are counted `skipped`, the request pays one timeout, and the write's response is unchanged (unit)
- [x] Gear init refuses a `publish_timeout_seconds` of `0` or above `10` (unit)
- [x] With no broker resolvable, a write commits, answers as it would with one, and its broadcast is counted `skipped`; gear init completes with no broker in the `ClientHub` (unit)
- [x] A publish before the root tenant's id is known is counted `skipped` and the write still commits (unit)
- [x] The Tenant Resolver is called for the root tenant's id once, on the consumer task's first broker resolution, retried with backoff while it fails, and never during gear init, a reconcile pass or a transaction (unit)
- [x] With the cache topic or the `cache_invalidate` type refused by the Types Registry, the consumer stays idle, a `warn` log names the refusal, the registration is retried with backoff, and a write meanwhile commits with its broadcast counted `skipped` (unit)
- [x] The activation subject type `gts.cf.core.settings.activation.v1~` is registered through the link-time inventory, without resolving the broker (unit)
- [x] A broadcast the broker accepts within `publish_timeout_seconds` is counted `sent` (unit)
- [x] With the `produce` grant withheld, the broadcast is counted `failed`, a `warn` log names the refusal, and the gear keeps serving reads and writes (unit)
- [x] With the `consume` or `define` grant withheld, the consumer stays idle, a `warn` log names the refusal, and the peer serves the new value within `cache_ttl_seconds`; after any number of retries the broker holds at most one group for the replica (unit)
- [x] Two `SettingsService` gears over one database and a delivering broker double hosted by the test: once the join barrier reports both replicas ready and replica B has made a warm read, a write on replica A is followed by B serving the new value within a poll deadline well below `cache_ttl_seconds`, at the written tenant for a `local` key and at a descendant for a `cascading` key (integration)
- [x] With the drop switch on and `cache_ttl_seconds` set to 1–2 s, replica B serves the previous value before the TTL elapses and the new one after the TTL plus a stated margin (integration)
- [x] A replica receiving its own broadcast, or a broadcast twice, evicts and serves the committed value; a malformed delivery is logged at `warn`, evicts nothing, and the consumer keeps running; a delivery carrying an unknown `data` attribute evicts its keys (unit)
- [x] A broadcast evicts every cached scope of each key it names, key-wide for every key — a `local` key and a key whose declaration the replica cannot read included — and the consume path reads no declaration, so a peer's next read of a named key re-resolves against the current declaration and, after a declaration change, masks or refuses as that declaration requires (unit)
- [x] A broker that becomes resolvable after gear start registers the cache topic and the `cache_invalidate` type once, and the replica's consumer joins and starts evicting; a broker lost mid-life — the delivery stream ended or errored, or the lookup returning no broker — pauses the consumer, which deletes its group best-effort, retries with exponential backoff from 1 s to a 30 s cap with jitter, and on the next resolution creates a new group and joins again (unit)
- [x] On `stop`, the replica leaves its anonymous group and deletes it within the shutdown deadline, and the start and the stop are logged at `info` (unit)
- [ ] In an environment that runs a broker and a second replica, the E2E rows *Cascading override + cross-instance re-resolution* (DESIGN §7) and *Replica cache coherence (broadcast)* (DESIGN-activation §7) pass: after an ancestor write on one replica the other serves the new value within `cache_ttl_seconds`, and its descendants re-resolve on their next read (E2E)
- [x] No endpoint, SDK call or request parameter triggers, targets or reports coherence (unit)
