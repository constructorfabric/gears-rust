---
description: "Provider and model rows are read from the database on every request and never cached, in the Model Registry gear or by its consumers; only the tenant ancestor chain is cached, per replica, in a bounded LRU with TTL and jitter, with no invalidation, no change events, and no cluster gear dependency."
status: accepted
date: 2026-11-03
decision-makers: Model Registry team
---

Created:  2026-11-03 by Constructor Tech

# ADR-0003: Uncached Catalog Rows and a Per-Replica Ancestor-Chain Cache

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Cache behavior by dimension](#cache-behavior-by-dimension)
  - [Catalog rows are read from the database on every request](#catalog-rows-are-read-from-the-database-on-every-request)
  - [Resolution and list results are not cached](#resolution-and-list-results-are-not-cached)
  - [The ancestor chain is the one cached value](#the-ancestor-chain-is-the-one-cached-value)
  - [Staleness is bounded by a TTL, not removed by invalidation](#staleness-is-bounded-by-a-ttl-not-removed-by-invalidation)
  - [Why not the cluster gear](#why-not-the-cluster-gear)
  - [No consumer-side caching of resolution results](#no-consumer-side-caching-of-resolution-results)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Uncached rows, per-replica TTL chain cache](#uncached-rows-per-replica-ttl-chain-cache)
  - [No cache at all](#no-cache-at-all)
  - [Per-replica TTL cache of resolved models](#per-replica-ttl-cache-of-resolved-models)
  - [Shared cache through the cluster gear with write-through invalidation](#shared-cache-through-the-cluster-gear-with-write-through-invalidation)
  - [Owner-keyed per-replica row cache with a cluster epoch key as invalidation signal](#owner-keyed-per-replica-row-cache-with-a-cluster-epoch-key-as-invalidation-signal)
  - [Versioned per-tenant catalog with a validator](#versioned-per-tenant-catalog-with-a-validator)
  - [Change events through the event-broker outbox](#change-events-through-the-event-broker-outbox)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-model-registry-adr-caching-strategy`

## Context and Problem Statement

`resolve_model` runs on every LLM request. The PRD asks for p99 at or below 10 ms in the gear, excluding PDP and Tenant Resolver wait, at 1,000 resolutions per second sustained and 5,000 at peak, over 10,000 models and a hierarchy depth up to 10 (`cpt-cf-model-registry-nfr-resolution-latency`); LLM Gateway budgets 50 ms of total overhead around it. Each resolution needs three inputs: the caller's ancestor chain from Tenant Resolver, the provider rows of the chain's tenants, and the model rows of the winning providers.

The same catalog carries controls that must bite at once: a provider disable (`cpt-cf-model-registry-fr-provider-disable`), an approval revocation (`cpt-cf-model-registry-fr-model-approval`), a sharing change, a delete. The PRD acceptance criterion states that a revoked model stops resolving on the next call. The PRD allows hierarchy changes to take effect within a bounded, configurable delay (`cpt-cf-model-registry-contract-tenant-hierarchy`) but gives catalog changes no such allowance. The gear is a stateless in-process replica set over one platform database (`cpt-cf-model-registry-topology-in-process-gear`), has no background tasks, and consumes no event feed. The platform offers a shared coordination store through the `cluster` gear (`ClusterCacheV1`).

What is cached, where, for how long, and how do consumers learn of a change?

Scope: this ADR covers caching inside the gear and in its consumers. It does not cover PDP decisions, Types Registry schemas, or CredStore reads. It is reviewed when a revisit trigger in More Information fires; a changed decision is recorded in a superseding ADR.

## Decision Drivers

* **D1** — A disable, revocation, sharing change, or delete takes effect on the next request on every replica. These are security controls, not conveniences. D1 takes precedence over D2: a missed latency benchmark is answered by option 5 under a PRD change, not by weakening D1.
* **D2** — The resolution path makes no Tenant Resolver call in the steady state, and meets the in-gear p99 with one round of database queries.
* **D3** — No new infrastructure or coordination: no `cluster` gear dependency, no background tasks, no event bus (`cpt-cf-model-registry-constraint-local-chain-cache`).
* **D4** — Staleness exists only where the PRD allows it, and there it is bounded and configurable.
* **D5** — Memory holds tenant identifiers only: no catalog rows, no `secret_ref`, no settings in a cache.
* **D6** — Every time-dependent behavior is testable without sleeping, through an injectable clock.
* **D7** — Consistency is the same whichever replica answers; a consumer cannot observe a change on one replica and its absence on another beyond the hierarchy bound.

## Considered Options

1. Uncached rows, per-replica TTL chain cache
2. No cache at all
3. Per-replica TTL cache of resolved models
4. Shared cache through the cluster gear with write-through invalidation
5. Owner-keyed per-replica row cache with a cluster epoch key as invalidation signal
6. Versioned per-tenant catalog with a validator
7. Change events through the event-broker outbox

## Decision Outcome

Chosen option: "Uncached rows, per-replica TTL chain cache", because it is the only option that gives catalog controls next-request effect on every replica with no coordination, confines staleness to the one input the PRD allows to lag, and keeps the hot path to one set of indexed database queries (D1, D3, D4, D5, D7).

### Cache behavior by dimension

The chain cache is the only cache. Resolve and list read the same cache the same way, so the answers differ only where the paths differ.

| Dimension | `resolve_model` | `list_available_models` |
| --- | --- | --- |
| Cache key | Tenant id of the caller (chain cache). No key for rows or results. | Same. No key for result pages, filters, ordering, or PDP constraints. |
| Cached value | Ordered ancestor chain, tenant ids only. | Same. |
| Capacity | `chain_cache_max_entries`, LRU eviction. | Shared with resolve. |
| Expiry | `chain_cache_ttl_seconds` less jitter. | Same. |
| Invalidation | None. | None. |
| Approval change | Next call, read from the database. | Next call, read from the database. |
| Sharing change | Next call, read from the database. | Next call, read from the database. |
| Provider disable or delete | Next call, read from the database. | Next call, read from the database. |
| Hierarchy change (`self_managed` conversion; moves, once reparenting exists) | Chain is stale for at most one TTL plus the request that read it; stale visibility can narrow or widen. | Same. |
| Delay bound | Catalog: none, next call. Hierarchy: TTL plus one request. | Same. |
| Cold-cache behavior | One `get_ancestors` call, bounded by `tenant_resolver_deadline_ms`; concurrent misses for one tenant share one call. | Same. |
| Cache-unavailable behavior | The cache is in process and cannot be unavailable. A failed Tenant Resolver call on a miss fails closed and is not cached. | Same. |
| Tenant isolation of keys | The key is the caller's tenant id; entries hold tenant ids only; no row, `secret_ref`, or setting is held. | Same. |

### Catalog rows are read from the database on every request

Provider and model rows are never held in memory across requests, in any layer of the gear (`cpt-cf-model-registry-principle-uncached-catalog-rows`). Every `resolve_model`, `list_available_models`, and management read queries the database. The latency NFR is met by the query shape. Resolution is two indexed point queries: providers by `(tenant_id IN chain, slug)`, at most 11 rows, and the model by the unique `(provider_id, name)`. Each is budgeted at about 3 ms inside the 10 ms in-gear p99. The visibility predicates run over that bounded candidate set. The benchmark of `cpt-cf-model-registry-nfr-resolution-latency` is run against the uncached path.

Each resolution also takes one PDP decision (Policy Engine target 25 ms p95). That decision depends on the subject, so the registry cannot cache it. Row caching would therefore save at most the two queries.

Provider type schemas are not cached: they are fetched from Types Registry on writes only and never on reads.

### Resolution and list results are not cached

A cached resolution result would keep serving a revoked model or disabled provider until it expired, which contradicts the PRD acceptance criterion and `cpt-cf-model-registry-fr-provider-disable` and `cpt-cf-model-registry-fr-model-approval`. List results depend on the OData filter, order, and paging and on the caller's PDP row constraints, so result keys would multiply per subject and query. The toolkit has no in-memory OData evaluator, so a cached row set could not be filtered without the database.

### The ancestor chain is the one cached value

The chain cache (`cpt-cf-model-registry-component-chain-cache`) is an in-process map per replica from tenant id to the ordered ancestor chain read with the default `BarrierMode::Respect` (ADR-0001). It holds tenant ids only. On a miss it makes one `get_ancestors` call bounded by `tenant_resolver_deadline_ms`; concurrent misses for one tenant share one call. The empty chain of the root tenant or of a self-managed tenant is a cached value. Failures are not cached. The map is a bounded concurrent LRU of `chain_cache_max_entries` entries.

The gear uses no `ClusterCacheV1` and no other shared cache. The chain is a per-tenant fact that every replica can read for itself.

### Staleness is bounded by a TTL, not removed by invalidation

There is no invalidation API, no cache clear on hierarchy change, and no event feed. An entry lives for `chain_cache_ttl_seconds` less the jitter (values and bounds: `cpt-cf-model-registry-component-chain-cache`, DESIGN §4 Configuration). A stale chain is used for at most one TTL plus the duration of the request that read it. The changed tenant and every tenant below it keep the stale chain until their own entries expire. A stale chain can narrow or widen visibility: after a `self_managed` conversion, inheritance continues across a newly created barrier; a moved tenant (reparenting, including detaching a subtree) keeps its former ancestors' shared catalog and does not yet see the new ancestors' providers, including ones that would shadow an inherited slug. Deployments that need hierarchy changes on the next request set the TTL to 0, which turns every request into a Tenant Resolver call. Expiry is checked against the injectable clock the service already uses for audit and approval timestamps.

Account Management defers hierarchy reparenting past v1 (Account Management PRD), and no gear publishes tenant change events: Tenant Resolver keeps no cache and has no events, and resource-group publishes none in v1. In v1 only a `self_managed` conversion changes the chain of an existing tenant (a barrier is created or removed). Moves (reparenting, including detaching a subtree) arrive with reparenting, deferred past v1, and the same TTL bound covers them.

### Why not the cluster gear

Guideline `guidelines/DESIGN_REVIEW.md` C16 asks for the cluster Gear SDK where a gear needs shared state. `ClusterCacheV1` (`gears/system/cluster/cluster-sdk/src/cache/`) is a coordination key-value store:

* Values are versioned opaque bytes with per-entry TTL and compare-and-set.
* Watch events carry the key only (`CacheEvent::Changed`, `Deleted`, `Expired`), are delivered at most once, and `Lagged` or `Reset` require a full re-read.
* There is no eviction, no capacity limit, and no batch get.
* There is no tenant model: calls run under the platform security context and tenant scoping exists only through a key prefix.
* The Postgres backend has no native prefix watch; the SDK provides a polling polyfill (`PollingPrefixWatch`).

Consequences for this gear:

* A remote get is a network round trip, as is the indexed point query it would replace, so it cannot be the hot-path cache.
* At-most-once, key-only events cannot deliver exact invalidation.
* The chain cache is a per-replica memo of a per-tenant fact, not distributed coordination, so C16 does not apply to it.

event-broker uses a cluster cache watch as a long-poll wake-up signal, with the database as the source of truth for events. Option 5 uses the same pattern.

### No consumer-side caching of resolution results

The registry publishes no cache validator for resolution results and no change feed, so a consumer cache would have unbounded staleness and would defeat D1. LLM Gateway resolves on every request and keeps the record for the duration of that request only; this ADR decides it as part of the consumer contract. REST responses of the resolution routes, `GET /model-registry/v1/available-models` and `GET /model-registry/v1/available-models/{canonical_id}`, carry `Cache-Control: no-store`. The strong `ETag` of the management routes is a concurrency validator (ADR-0004), not a cache validator, and those responses carry `Cache-Control: no-store` as well. DESIGN §3.3 and the REST conventions adopt these points in a follow-up (Migration obligation).

### Consequences

* Performance: every resolution costs one round of database queries; the database is on the hot path and sized for 5,000 resolutions per second at peak. Connection-pool exhaustion and database latency are the main risks to the resolution SLO (DESIGN §4 Fault Tolerance). The uncached path must meet the NFR by itself.
* Security: revocation, disable, sharing change, and delete are immediate. A stale hierarchy is bounded by the TTL and can widen visibility, including inheritance across a newly created barrier; the DESIGN Security Assumptions control (TTL at most 300) carries the ceiling. No `secret_ref` or setting sits in a cache.
* Reliability: a Tenant Resolver outage leaves tenants with a valid entry serving and tenants without one failing closed. A cold replica depends on Tenant Resolver for its first request per tenant.
* Data: catalog writes are visible to the next read on every replica; no replication or warm-up step follows a write. No new stored data.
* Integration: no `cluster` dependency. The LLM Gateway DESIGN statement "Model Registry is cache-first" is wrong under this ADR. DESIGN §3.3 Contract Migration already drops it.
* Operations: TTL and capacity are the two knobs; hit, miss, and eviction metrics expose their effect. Memory per replica is bounded by `chain_cache_max_entries` times a chain of at most hierarchy-depth tenant ids.
* Testing: time-dependent behavior is tested with the injectable clock; no test sleeps (D6).
* Compliance, UX, business: not affected; no personal data is cached and no user-visible behavior changes beyond the stated bounds.
* Migration obligation: DESIGN §3.3 Consumer Contract adopts the no-consumer-cache point, and the REST conventions adopt `Cache-Control: no-store` on resolution and management responses.
* Reversibility: adding a row cache later (option 5) changes the PRD revocation guarantee and is a new decision; the chain cache can be removed by setting the TTL to 0.

### Confirmation

* Integration (SQLite and PostgreSQL), two service instances over one database: a provider disable, an approval revocation, a sharing change, and a delete committed through one instance change the next `resolve_model` and `list_available_models` result on the other.
* Integration with the injectable clock: an entry is served until the TTL less its jitter has elapsed and refetched after; TTL 0 calls Tenant Resolver on every request; an empty chain is cached; a failed call stores nothing; concurrent misses for one tenant make one call; inserting beyond `chain_cache_max_entries` evicts the least recently used entry. No test sleeps.
* Integration: a hierarchy change through the Tenant Resolver mock is invisible until the TTL has elapsed and visible after.
* Integration: a `self_managed` flip through the Tenant Resolver mock is invisible until the TTL has elapsed and visible after.
* Integration: resolution and management REST responses carry `Cache-Control: no-store`.
* Benchmark: the in-gear p99 of `resolve_model` on the uncached row path meets `cpt-cf-model-registry-nfr-resolution-latency` at its stated load, catalog size, and depth.
* Review: no type holding provider or model data is stored in any map or `Arc` that outlives a request; the `cluster` SDK is not a dependency of the gear crate.

## Pros and Cons of the Options

### Uncached rows, per-replica TTL chain cache

Database read per request; chain cached per replica with a bounded TTL; nothing else.

* Good, because catalog controls take effect on the next request on every replica without coordination (D1, D7).
* Good, because the only cached fact is one the PRD allows to lag, and the lag is bounded and configurable (D4).
* Good, because no new dependency, background task, or event surface is needed (D3).
* Good, because memory holds tenant ids only (D5) and expiry is testable with a clock (D6).
* Bad, because the database is on every resolution; the NFR depends on query shape and pool sizing (D2).
* Bad, because a hierarchy change lags by up to the TTL, and the lag can widen visibility for a moved or newly converted tenant.

### No cache at all

Call Tenant Resolver and the database on every request.

* Good, because nothing is ever stale (D1, D4).
* Bad, because every resolution waits on a second dependency round trip inside LLM Gateway's 50 ms budget, and Tenant Resolver becomes a hot-path dependency at 5,000 requests per second (D2).

### Per-replica TTL cache of resolved models

Cache the resolved `ModelV1` per tenant and canonical id with a TTL.

* Good, because the steady-state resolution touches neither the database nor Tenant Resolver (D2).
* Bad, because a revocation or disable keeps serving on every replica until the TTL expires, which contradicts the PRD acceptance criterion, `cpt-cf-model-registry-fr-provider-disable`, and `cpt-cf-model-registry-fr-model-approval` (D1). It would need a PRD-accepted bounded revocation delay.
* Bad, because replicas disagree for up to one TTL (D7), and memory holds `secret_ref` and provider settings (D5).

### Shared cache through the cluster gear with write-through invalidation

Keep resolved rows in `ClusterCacheV1` and delete entries on every catalog write.

* Good, because all replicas see one cache and a write can clear it.
* Bad, because a remote get is a network round trip, as is the indexed query it replaces, and the cluster gear joins the hot path as a dependency with its own latency and failure modes (D2, D3).
* Bad, because a failed invalidation after a committed write serves a revoked model until the entry's TTL (D1).
* Bad, because invalidation must cover inheritance: one ancestor write affects every descendant's entries, which the writer cannot enumerate without reading the hierarchy downward.
* Bad, because the store has no tenant model, so tenant isolation rests on key prefixes (D5).

### Owner-keyed per-replica row cache with a cluster epoch key as invalidation signal

Each replica caches provider rows keyed by owning tenant (`providers/{owner_tenant_id}`) and models keyed by `provider_id`. A resolve composes its result from at most 11 owner entries, found through the chain cache. After each committed write the writer bumps one global key (for example `model-registry/catalog-epoch`) in `ClusterCacheV1`. Every replica watches that single key and flushes its row cache on a change. A backstop TTL bounds a lost signal or a crash between commit and bump.

* Good, because an ancestor write invalidates one owner key, not every descendant's entries.
* Good, because one watched key works on every cluster backend, including Postgres.
* Good, because steady-state resolution skips the database.
* Bad, because revocation becomes "milliseconds normally, the backstop TTL in the worst case", which requires changing the PRD from "next call" to a bounded delay (D1, D4).
* Bad, because it adds a cluster dependency and a watch lifecycle (reconnect, `Lagged`, `Reset`) to every replica (D3).
* Bad, because memory holds provider settings and `secret_ref` (D5), and replicas disagree during the signal delay (D7).

This is the designated upgrade path; see More Information.

### Versioned per-tenant catalog with a validator

Keep a per-tenant catalog version in the database, cache rows per replica, and validate the cache with one version read per request.

* Good, because a validated hit costs one indexed read and a change is seen on the next request (D1).
* Bad, because inheritance makes the version non-local: an ancestor write must bump every descendant's version, or the validator must read the chain's versions, which is a query of the same order as the row read it replaces (D2).
* Bad, because it adds a write amplification path and a cached row set per replica for a saving the row read does not need at the stated scale (D5).

### Change events through the event-broker outbox

Publish catalog and hierarchy change events; replicas and consumers invalidate on receipt.

* Good, because both caches and consumer caches could be exact.
* Bad, because the gear has no background tasks and no event surface, and Tenant Resolver publishes none; building the outbox, relay, and consumers is a platform project (D3).
* Bad, because a lost event again serves a revoked model until a fallback TTL (D1, D7).

## More Information

Option 5 is the designated upgrade path. Adopting it requires a PRD change that accepts a bounded revocation and disable delay. Triggers for proposing that change:

* The benchmark of the uncached path misses `cpt-cf-model-registry-nfr-resolution-latency` at the stated load.
* The load or catalog size grows past the PRD figures.

Other revisit triggers:

* Tenant Resolver or Account Management publishes hierarchy change events that in-process gears can consume; the chain cache can then be invalidated exactly.
* A consumer needs to cache resolution results; the versioned catalog with a validator is the option to re-examine.

Related: [ADR-0001](./0001-cpt-cf-model-registry-adr-inherited-read-scope.md) decides what the cached chain is and how it is read; [ADR-0004](./0004-cpt-cf-model-registry-adr-update-semantics.md) decides the management `ETag`, which is a concurrency validator and not a cache validator.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **ADRs**: [ADR-0001](./0001-cpt-cf-model-registry-adr-inherited-read-scope.md), [ADR-0004](./0004-cpt-cf-model-registry-adr-update-semantics.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-model-registry-nfr-resolution-latency` — no Tenant Resolver call in the steady state; the uncached row path is what the benchmark measures.
* `cpt-cf-model-registry-nfr-availability` — a chain miss during a Tenant Resolver outage fails closed; a valid entry keeps serving.
* `cpt-cf-model-registry-fr-provider-disable` — a disable is visible to the next request on every replica.
* `cpt-cf-model-registry-fr-model-approval` — a revocation is visible to the next request on every replica.
* `cpt-cf-model-registry-fr-resolve-model` — resolution reads rows from the database; its results are not cached by the gear or by consumers.
* `cpt-cf-model-registry-contract-tenant-hierarchy` — hierarchy changes take effect within `chain_cache_ttl_seconds` plus one request.
* `cpt-cf-model-registry-principle-uncached-catalog-rows` — the principle this ADR decides.
* `cpt-cf-model-registry-constraint-local-chain-cache` — no shared cache, no `cluster` dependency.
* `cpt-cf-model-registry-component-chain-cache` — the one cache, its TTL, jitter, capacity, single flight, and clock.
* `cpt-cf-model-registry-topology-in-process-gear` — stateless replicas over one database; per-replica chain cache, no shared state.
