# Cluster Observability Contract — v1

**ID**: `cpt-cf-clst-ref-observability-catalog`

This is the versioned naming catalog mandated by [ADR-004](./ADR/004-observability-contract.md).
It is the authoritative reference for the span, metric, and log-event names every
cluster backend/plugin emits. The names are mirrored in code as constants in
[`cluster_sdk::observability`](../cluster-sdk/src/observability.rs); this document
catalogs them with their attributes, labels, and severities.

**Stability**: names are a contract on par with the Rust trait signatures.
Renaming, removing, or relabeling a signal is a **breaking change** requiring a
major SDK version bump. Adding a new signal is non-breaking.

<!-- toc -->

- [1. Conventions](#1-conventions)
- [2. Cardinality rule](#2-cardinality-rule)
- [3. Field keys](#3-field-keys)
- [4. Spans](#4-spans)
- [5. Metrics](#5-metrics)
- [6. Log events](#6-log-events)

<!-- /toc -->

## 1. Conventions

| Signal family | Style | Pattern | Example |
|---|---|---|---|
| OpenTelemetry spans | dotted lowercase | `cluster.<primitive>.<op>` | `cluster.cache.get` |
| Prometheus metrics | underscored lowercase | `cluster_<primitive>_<subject>_<unit>` | `cluster_cache_ops_total` |
| Structured log events | dotted lowercase | `cluster.<primitive>.<event>` | `cluster.leader.transition` |

## 2. Cardinality rule

Operation keys, lock names, and election names **never**
appear as metric labels — they are unbounded and would explode Prometheus
cardinality. They may appear only as **span attributes** (traces are sampled)
and **log-event fields** (log volume is filter-controlled).

Metric labels are restricted to the bounded, enum-like keys enumerated by
`cluster_sdk::observability::METRIC_LABEL_ALLOWLIST`: `provider`, `op`, `result`,
`transition`, `kind`, `primitive`, `profile`.

`profile` joined that list with item `S2`. It was classified as high-cardinality
here and in `fields::attr`, which contradicted DESIGN.md §3.18.3
("`profile` and `provider` are bounded and allowed as labels") and its invariant
I15. The design is right: a profile name comes from the operator's configured
profile set, fixed at `start` and changed only by an explicit reload (DESIGN.md §3.18.5).

## 3. Field keys

### Label keys (bounded — allowed on metrics, spans, and logs)

| Key | Meaning | Example values |
|---|---|---|
| `provider` | Concrete backend/provider name | `postgres`, `redis`, `k8s` |
| `op` | Facade operation | `get`, `put`, `try_lock` |
| `result` | Bounded operation outcome | `ok`, `conflict`, `timeout`, `contended` |
| `transition` | Leadership transition kind | `acquired`, `lost`, `resigned` |
| `kind` | Provider-error retryability class | `connection_lost`, `timeout`, `auth_failure` |
| `primitive` | Primitive name | `cache`, `lock`, `leader` |
| `profile` | Cluster profile | `default`, `orders` |

### Attribute keys (high-cardinality — spans and logs only, never metric labels)

| Key | Meaning |
|---|---|
| `key` | Cache key |
| `name` | Coordination name |
| `lock` | Lock name |
| `election` | Election name |

## 4. Spans

| Span name | Covers | Attributes |
|---|---|---|
| `cluster.cache.get` | `ClusterCacheV1::get` | `provider`, `key` |
| `cluster.cache.put` | `ClusterCacheV1::put` | `provider`, `key` |
| `cluster.cache.delete` | `ClusterCacheV1::delete` | `provider`, `key` |
| `cluster.cache.contains` | `ClusterCacheV1::contains` | `provider`, `key` |
| `cluster.cache.put_if_absent` | `ClusterCacheV1::put_if_absent` | `provider`, `key` |
| `cluster.cache.compare_and_swap` | `ClusterCacheV1::compare_and_swap` | `provider`, `key` |
| `cluster.cache.watch` | `ClusterCacheV1::watch` | `provider`, `key` |
| `cluster.cache.watch_prefix` | `ClusterCacheV1::watch_prefix` | `provider`, `key` |
| `cluster.leader.elect` | `LeaderElectionV1::elect` / `elect_with_config` | `provider`, `election` |
| `cluster.leader.renew` | Background claim renewal | `provider`, `election` |
| `cluster.leader.resign` | `LeaderWatch::resign` | `provider`, `election` |
| `cluster.lock.try_lock` | `DistributedLockV1::try_lock` | `provider`, `lock` |
| `cluster.lock.lock` | `DistributedLockV1::lock` | `provider`, `lock` |
| `cluster.lock.renew` | `LockGuard::renew` | `provider`, `lock` |
| `cluster.lock.release` | `LockGuard::release` | `provider`, `lock` |

## 5. Metrics

| Metric name | Type | Unit | Labels |
|---|---|---|---|
| `cluster_cache_ops_total` | counter | — | `provider`, `op`, `result` |
| `cluster_cache_op_duration_seconds` | histogram | seconds | `provider`, `op` |
| `cluster_lock_ops_total` | counter | — | `provider`, `op`, `result` |
| `cluster_lock_op_duration_seconds` | histogram | seconds | `provider`, `op` |
| `cluster_leader_transitions_total` | counter | — | `provider`, `transition` |
| `cluster_watch_resets_total` | counter | — | `provider`, `primitive` |
| `cluster_provider_errors_total` | counter | — | `provider`, `kind` |
| `cluster_subscriptions_reaped_total` | counter | — | `profile`, `primitive` |
| `cluster_subscriptions_active` | gauge | — | `profile`, `primitive` |

The `op` label is a bounded set of facade operations: cache —
`get`/`put`/`delete`/`contains`/`put_if_absent`/`compare_and_swap`/`watch`/`watch_prefix`
plus the backend-internal `compare_and_delete`/`scan_prefix`; lock —
`try_lock`/`lock`/`renew`/`release`.

### 5.1 OpenTelemetry instrument names and the `_total` suffix

The names above are the **scraped Prometheus** series names — the contract. The
SDK's OpenTelemetry adapter
([`cluster_sdk::observability::otel::OtelClusterMetrics`](../cluster-sdk/src/observability/otel.rs))
creates each counter instrument **without** the `_total` suffix
(`cluster_cache_ops`, not `cluster_cache_ops_total`): the
`opentelemetry-prometheus` exporter appends `_total` to counters when it renders
them, so the scraped series matches the catalog. Including `_total` on the
instrument would double it (`…_total_total`). Histograms (`…_duration_seconds`)
carry no such suffix and use the catalog name verbatim. The adapter derives the instrument name from
the catalog constant by stripping `_total`, so the constant stays the single
source of truth.

### 5.2 Emission status

The metrics sink is the OTel-agnostic `cluster_sdk::observability::ClusterMetrics`
port (with `NoopMetrics` as the default and the feature-gated `OtelClusterMetrics`
as the concrete adapter). The cache primitive emits via the
`cluster_sdk::InstrumentedCache` decorator; the SDK default lock and leader
backends emit via their `with_observability(provider, metrics)` builder. The
**standalone** plugin wires all three (`provider = "standalone"`).

`cluster_watch_resets_total` (and the `cluster.watch.reset` WARN log) is emitted
by the watch auto-restart combinator (`RestartingWatch`) on each transparent
reconnect: the backend stamps a `(provider, metrics)` context onto the watch it
hands out, which the combinator captures and reports against the watch's
`primitive` label (`cache` / `leader`). The full contract is now
emitted.

`cluster_subscriptions_reaped_total` and `cluster_subscriptions_active` are the
two exceptions to that port. They are emitted by the **cluster gear** — not by any
backend — through a meter it owns directly
(`cluster/src/api/grpc/sweep.rs`), on the same precedent as the Postgres plugin's
reaper gauges: `ClusterMetrics` is the sink a *provider* reports through, it
exposes neither a gauge nor a free-form counter, and a server-side session index
is not a provider signal. Both come from the abandoned-subscription sweep
(DESIGN.md §3.18.3) on its 5 s cadence, and both carry exactly
`(profile, primitive)` — an election name reaches the reap log and never the
metric. `primitive` is `leader` today, since election subscriptions are the only
kind this table holds; the label is emitted anyway so a second watch table slots
in without the series changing shape.

## 6. Log events

| Event name | Severity | Emitted when | Fields |
|---|---|---|---|
| `cluster.leader.transition` | INFO | Leadership is acquired, lost, or resigned | `provider`, `election`, `transition` |
| `cluster.watch.reset` | WARN | A watch terminally closed and was resubscribed | `provider`, `primitive` |
| `cluster.provider.error` | ERROR | A backend/provider operation failed | `provider`, `kind`, `op`, one of `key`/`lock`/`election`/`name` (the resource, per primitive), `message` |
