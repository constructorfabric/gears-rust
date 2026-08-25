---
status: accepted
date: 2026-04-02
---

# ADR-004: Observability as a Versioned Naming Contract

**ID**: `cpt-cf-clst-adr-observability-contract`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: No explicit contract](#option-1-no-explicit-contract)
  - [Option 2: Full catalog embedded in DESIGN.md](#option-2-full-catalog-embedded-in-designmd)
  - [Option 3: Separate versioned catalog file (CHOSEN)](#option-3-separate-versioned-catalog-file-chosen)
  - [Option 4: Runtime-enumerable API](#option-4-runtime-enumerable-api)
- [More Information](#more-information)
  - [Example: consistent cross-provider alerting](#example-consistent-cross-provider-alerting)
  - [Example: cardinality accident (what we prevent)](#example-cardinality-accident-what-we-prevent)
  - [References](#references)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The cluster gears platform-tier infrastructure that every Gear depends on. When a cluster operation misbehaves — a lock that never releases, an election that flaps, a cache that times out — the first tool operators reach for is observability data: traces, metrics, and logs. The quality and consistency of that data determines how fast an incident is triaged.

Three failure modes recur when infrastructure libraries don't treat observability as a first-class contract:

1. **Per-provider inconsistency**. The Redis provider names its latency metric `cluster_redis_get_ms`, the Postgres provider names it `cluster_cache_get_duration_seconds`, and the K8s provider emits a trace span but no metric at all. An operator who built a cross-provider dashboard has to special-case every combination.
2. **Retrofitted naming**. Observability is added late in implementation, each provider picks what makes sense to its author at the time, and the names drift across crates. Renaming after rollout is a breaking change for consumer alerts.
3. **Cardinality accidents**. Lock names, cache keys, and service instance IDs end up as Prometheus labels because "it would be useful to have them there." Prometheus's time-series explodes; the whole metrics pipeline degrades; someone eventually has to ship a breaking rename.

This ADR establishes that observability signals are part of the cluster SDK's contract, on par with Rust trait signatures. Names are versioned, cardinality is bounded, and the concrete catalog is maintained as a separate reference file that evolves with the implementation.

## Decision Drivers

- Cluster is consumed by every gear — the cost of inconsistent observability is multiplied across the whole platform.
- Operators build long-lived dashboards and alerts; silent renames break them.
- Prometheus cardinality is a known production failure mode; unbounded label values must be prevented by convention, not discovered after outages.
- The design document should state the principle, not enumerate 40 span names.
- Concrete names will evolve during implementation as providers hit edge cases; the naming catalog needs to be editable without recertifying the design.

## Considered Options

1. **No explicit contract** — let each provider emit whatever it wants.
2. **Full catalog embedded in DESIGN.md** — every span and metric name listed in the design.
3. **Separate versioned catalog file** — design states the principle; the catalog lives in a reference file that evolves independently.
4. **Runtime-enumerable API** — providers implement a `fn observability_schema() -> Schema` method and tooling generates the catalog at build time.

## Decision Outcome

Chosen option: **Option 3** — the design states the principle; concrete signal names follow the conventions defined below, and per-plugin DESIGNs catalog the full set each plugin emits. Names are a versioned contract. Breaking changes (rename, removal, label change) require a major SDK version bump; additions are non-breaking.

The catalog defines three signal families:

- **OpenTelemetry spans** (dotted lowercase: `cluster.cache.get`, `cluster.lock.try_lock`, etc.) with documented attribute keys.
- **Prometheus metrics** (underscored lowercase: `cluster_cache_ops_total`, `cluster_lock_op_duration_seconds`) with low-cardinality labels only.
- **Structured log events** (via `tracing`) at defined severity levels for specific operator use cases (leader transitions at INFO, watch resets at WARN, provider errors at ERROR).

A hard cardinality rule applies: operation keys, lock names, election names, and service instance IDs **never** appear as metric labels. They may appear in span attributes (traces are sampled) and log event fields (log volume is filter-controlled). Metric labels are restricted to `provider`, `op`, `result`, `transition`, `kind`, and similar enum-like dimensions with bounded cardinality.

### Consequences

- Every provider has a concrete checklist of signals to emit — no ambiguity about "did I forget something."
- Operators can build one Grafana dashboard and Jaeger search that work across all cluster providers without provider-specific branches.
- Consumer alerts built on documented names remain stable across cluster SDK minor versions.
- New providers onboarded later (Hazelcast, Redis Enterprise) inherit the catalog — no drift.
- The catalog file becomes maintenance surface: every new span or metric must be documented there. This is the right friction — it forces the convention to be intentional.
- Cardinality rule eliminates a whole class of "we shipped, then Prometheus melted" incidents.
- Minor friction for new contributors: adding a metric now requires a doc change, not just a code change. Worth it.

### Confirmation

- CI lint (Phase 2+): a check parses each plugin's signal catalog and verifies the listed signals are actually emitted by that plugin's test harness. Drift caught at build time.
- Integration tests per provider assert that all cataloged spans/metrics/logs fire under representative workloads.
- A consumer-side assertion test registers a Prometheus scrape endpoint, runs a workload, and verifies none of the cataloged metrics have labels containing user-supplied names or keys.

## Pros and Cons of the Options

### Option 1: No explicit contract

- Good, because zero up-front investment.
- Good, because each provider gets maximum flexibility.
- Bad, because inconsistent naming across providers is the documented failure mode we are trying to avoid.
- Bad, because observability ends up retrofitted, with different crates picking different conventions.
- Bad, because cardinality accidents become likely — no mechanism prevents them.
- Bad, because consumer alerts cannot be portable across providers.

### Option 2: Full catalog embedded in DESIGN.md

- Good, because the design is self-contained.
- Good, because reviewers see every name in one place.
- Bad, because the design balloons by ~150 lines of name tables that drown the architectural decisions. Review fatigue leads to skimming; drift goes unnoticed.
- Bad, because the catalog will evolve during implementation (edge cases in providers), and every tweak becomes a design-level change requiring re-review.
- Bad, because the design document stops being an architectural narrative and becomes a reference manual. Two different audiences, poorly served by one document.

### Option 3: Separate versioned catalog file (CHOSEN)

- Good, because the design stays focused on architecture; the catalog stays focused on naming.
- Good, because the catalog can evolve during implementation without churning the design.
- Good, because operators and implementers have a single authoritative reference for names without wading through architectural prose.
- Good, because the pattern matches prior reference files in the openspec change (per-backend safety analysis, watch-backpressure analysis) — established convention.
- Good, because a lint check (Phase 2+) can parse the catalog and verify emissions, preventing drift.
- Bad, because the catalog file is an additional artifact to maintain. Mitigated by CI lint enforcing it remains current.
- Neutral, because the "stable names" contract is enforced by review discipline, not the type system. True of every documentation-level contract.

### Option 4: Runtime-enumerable API

- Good, because providers self-document — no separate catalog.
- Good, because tooling can generate Grafana dashboards from the runtime schema.
- Bad, because it adds a trait method (`observability_schema`) purely for reflection, growing the plugin-facing trait surface for a non-functional concern.
- Bad, because runtime self-reporting does not protect against naming drift at design time — providers can still pick inconsistent names, the schema method just reports whatever they picked.
- Bad, because the lint-against-static-catalog approach of Option 3 achieves the same drift-prevention at build time, with less runtime machinery.
- Neutral, because reflection-based schemas are a nice-to-have for dashboard generation; it can be added later on top of Option 3's static catalog.

## More Information

### Example: consistent cross-provider alerting

With the catalog contract in place, an operator writing a Prometheus alert "any cluster provider's error rate exceeds 1% over 5 minutes" can write:

```yaml
- alert: ClusterProviderErrorRate
  expr: rate(cluster_provider_errors_total[5m]) / rate(cluster_cache_ops_total[5m]) > 0.01
  labels:
    severity: warning
```

No provider-specific branches. No renaming when the Redis provider ships before K8s. The alert is portable.

### Example: cardinality accident (what we prevent)

```prometheus
# NOT ALLOWED — key as label, cardinality unbounded.
cluster_cache_ops_total{provider="redis", key="session/abc-123", op="get"} 1
cluster_cache_ops_total{provider="redis", key="session/def-456", op="get"} 1
# ... millions of time series, one per unique key.
```

vs.

```prometheus
# ALLOWED — bounded labels only.
cluster_cache_ops_total{provider="redis", op="get", result="ok"} 12345
```

Per-key detail goes to spans (via `cluster.cache.key` attribute) and log events, where cardinality is either sampled or filtered.

### References

- [OpenTelemetry Semantic Conventions for Spans](https://opentelemetry.io/docs/specs/semconv/general/trace/) — precedent for dotted lowercase span naming.
- [Prometheus Naming Best Practices](https://prometheus.io/docs/practices/naming/) — precedent for underscored lowercase metric naming and cardinality discipline.
- [Prometheus Cardinality Explosion](https://www.robustperception.io/cardinality-is-key/) — classic writeup of the failure mode this ADR prevents.
- [Kubernetes Instrumentation Guidelines](https://github.com/kubernetes/community/blob/master/contributors/devel/sig-instrumentation/instrumentation.md) — K8s precedent for versioning observability signals as a contract.
- [Envoy Stats Naming](https://www.envoyproxy.io/docs/envoy/latest/configuration/observability/statistics) — precedent for explicit naming contract in a platform component consumed by many clients.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-clst-nfr-observability` — Spans / metrics / log events as a versioned naming contract; cardinality discipline.
- DESIGN §3.3 telemetry expectations across all three primitive contracts.

**Sibling ADRs:**

- (none — observability stands on its own)
