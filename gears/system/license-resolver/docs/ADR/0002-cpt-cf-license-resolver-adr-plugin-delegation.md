---
status: accepted
date: 2026-06-08
---

# ADR-0002: Pure Plugin-Delegating Gateway with No Grant Store

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Plugin-delegating gateway, no own store](#a-plugin-delegating-gateway-no-own-store)
  - [(b) Resolver-owned grant store](#b-resolver-owned-grant-store)
  - [(c) Hybrid built-in store + plugins](#c-hybrid-built-in-store--plugins)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-license-resolver-adr-plugin-delegation`

## Context and Problem Statement

License resolver must answer `is_licensed(request)` for grants that physically live in many heterogeneous,
vendor-specific backends (entitlement services, SaaS licensing APIs, on-prem stores). Where should grant facts live, and
how should the resolver reach them — should the module own a grant store, delegate to pluggable backends, or do both?
The Global Type System (GTS) types registry already provides versioned plugin discovery and vendor/priority selection
used by sibling resolvers.

## Decision Drivers

* Grant data is owned by external authorities and varies per vendor; the resolver is a read-only evaluator, not a system
  of record.
* New backends must be addable or swappable without changing callers or the public contract.
* Behavior must fail closed: when no backend can be reached, the resolver must never grant by default.
* Consistency with `tenant-resolver` / `authz-resolver`, which already discover and select plugins via the types
  registry by vendor + priority.
* Avoid duplicating or caching authoritative grant state that the resolver does not own.

## Considered Options

* (a) Pure plugin-delegating gateway with no resolver-owned grant store; backends discovered via the types registry (
  `LicenseResolverPluginSpecV1`) and selected by vendor + priority
* (b) A resolver-owned grant store as the **sole** source: the resolver persists grant records and answers from its own
  store, with **no** plugin delegation
* (c) A **hybrid**: the resolver keeps its own store **and** delegates to plugins, so a check may be answered from either
  — **two** sources of truth

## Decision Outcome

Chosen option: (a) pure plugin-delegating gateway with no grant store, because grant facts are externally owned and
heterogeneous, delegation keeps the resolver read-only and authoritative-by-reference, and reusing the types-registry
discovery + vendor/priority selection already proven in `tenant-resolver` / `authz-resolver` lets backends evolve
independently of callers while making fail-closed behavior natural when no plugin matches.

### Consequences

* The main module is a gateway/selector only: it discovers plugins via the types registry (GTS spec
  `LicenseResolverPluginSpecV1`), selects by vendor + priority, delegates the check, and maps plugin errors to
  `LicenseResolverError`.
* No persistence, schema, or migrations are owned by this module; there is no grant table to back up or reconcile.
* When no plugin matches, the gateway returns `NoPluginAvailable`; when a selected backend is unreachable, it returns
  `ServiceUnavailable` — neither path can yield a granted decision.
* Plugins implement an identical check signature (`LicenseResolverPluginClient`), so adding a vendor backend requires
  registering its plugin spec, with no caller or contract change.

### Confirmation

Confirmed by design and code review (no persistence layer in the main module; discovery/selection routed through the
types registry) and by tests asserting that vendor/priority selection picks the expected plugin and that no-plugin and
unreachable-backend conditions produce zero grant-by-default outcomes.

## Pros and Cons of the Options

### (a) Plugin-delegating gateway, no own store

The module discovers and delegates to backend plugins; it stores nothing.

* Good, because authoritative grant data stays with its owners and is never duplicated.
* Good, because backends are added/swapped via plugin registration with no caller change.
* Good, because it reuses the proven `tenant-resolver` / `authz-resolver` registry discovery + vendor/priority
  selection.
* Good, because fail-closed is natural: no plugin or unreachable backend cannot produce a grant.
* Neutral, because per-check latency depends on the selected backend rather than a local store.
* Bad, because availability of a check is bounded by the backend's availability (no local fallback).

### (b) Resolver-owned grant store

The resolver is the **single** source of truth: it persists grant records and answers checks from its own store, with no
delegation to backends.

* Good, because checks can be served with low, predictable local latency.
* Bad, because it makes the resolver a system of record for data it does not own, requiring ingestion, reconciliation,
  and migrations.
* Bad, because heterogeneous vendor models do not fit one schema, and stale local copies risk incorrect grant/deny
  decisions.
* Bad, because it contradicts the read-only / delegate-don't-store design intent.

### (c) Hybrid built-in store + plugins

The resolver keeps its own store **and** also delegates to plugins — a check may be answered from the local store or
from a backend, so there are **two** sources of truth (unlike (b), which has only the store, and (a), which has only
plugins).

* Good, because it could offer a local fast path alongside delegation.
* Bad, because it carries the store's ownership and staleness costs plus the delegation machinery — the worst of both.
* Bad, because dual sources of truth complicate fail-closed guarantees and auditing of which source decided a check.

## More Information

Mirrors `tenant-resolver` (`TenantResolverPluginSpecV1`) and `authz-resolver` (`AuthZResolverPluginSpecV1`) discovery
and selection. The plugin spec `LicenseResolverPluginSpecV1` is registered under the toolkit plugin base /
`gts.cf.core.lic.*`. See `guidelines/GTS.md` for the types-registry and plugin-spec model.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-license-resolver-fr-plugin-delegation` — defines the discover-and-delegate gateway behavior this ADR records.
* `cpt-cf-license-resolver-fr-read-only` — no store and no write paths follow directly from this decision.
* `cpt-cf-license-resolver-fr-is-licensed-check` — the single check operation is served by delegating to the selected
  plugin.
* `cpt-cf-license-resolver-nfr-fail-closed` — no-plugin / unreachable-backend paths yield no grant by default.
* `cpt-cf-license-resolver-nfr-read-latency` — per-check latency is bounded by the delegated backend, not a local store.
* `cpt-cf-license-resolver-principle-delegate-dont-store` — this ADR is the rationale for that DESIGN principle.
* `cpt-cf-license-resolver-principle-fail-closed-no-plugin` — fail-closed behavior is a direct consequence of pure
  delegation.
* `cpt-cf-license-resolver-constraint-gts-via-types-registry` — discovery/selection go exclusively through the GTS types
  registry.
