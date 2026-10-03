# Performance Testing — Cluster

This document defines **what** cluster performance tests measure and **how** they
measure it. Correctness is owned by [TESTING-STRATEGY.md](./TESTING-STRATEGY.md);
the targets come from [ADR-001](./ADR/001-provider-compatibility-and-performance.md);
server-side signals come from the [observability contract](./OBSERVABILITY.md).

<!-- toc -->

- [1. Scope](#1-scope)
- [2. System under test](#2-system-under-test)
- [3. Metrics](#3-metrics)
- [4. Workloads](#4-workloads)
- [5. Run parameters](#5-run-parameters)
- [6. Targets](#6-targets)
- [7. Measurement rules](#7-measurement-rules)
- [8. Harness](#8-harness)
- [9. Reporting](#9-reporting)
- [10. Cadence & gating](#10-cadence--gating)
- [11. Diagnosis](#11-diagnosis)
- [12. Prerequisites & open items](#12-prerequisites--open-items)

<!-- /toc -->

## 1. Scope

**We test:** client-observed throughput and latency of the cluster primitives
(cache, lock, leader election) per backend, **end-to-end over gRPC in a deployed
Kubernetes cluster**, and how much of that latency is transport versus backend.

We also test that coordination stays **safe under load**: leaders and leases
survive contention, and every run's results are verified (§7).

**We do not test:** correctness or fault tolerance in general (L1–L4 in
TESTING-STRATEGY), absolute production capacity, or micro-benchmarks of SDK
internals (use `criterion` for those).

**Why a dedicated suite.** Every other cluster test runs the backend in-process.
In production, cluster is out-of-process and every operation is a network round
trip. That round trip is the cost a consumer pays, and only a deployed test can
measure it. Cluster has no REST surface, so gateway-level tools (k6 at the edge)
cannot reach it; the measuring client must be an in-cluster gRPC consumer (see
§8 for why that is a purpose-built driver rather than an off-the-shelf tool).

## 2. System under test

Two deployment profiles, driven by the **same consumer code**:

| Profile | Path | What it measures |
|---|---|---|
| **Profile 3 — OoP** (headline) | driver pod ─gRPC─▶ grpc-hub (TokenReview auth) ─gRPC─▶ cluster gear (N replicas) ─▶ backend | Full consumer round trip |
| **Profile 1 — embedded** (control) | driver process links cluster + plugin, `LocalClusterClient` ─▶ backend | Backend + plugin cost, zero hops |

`Profile 3 − Profile 1` is the transport cost with everything else held constant.

**Backends:** `standalone`, `postgres`, `k8s` (native), `redis`. NATS/etcd are
future config values; the harness is backend-agnostic.

**Fixed conditions:**

- **In-cluster only.** The consumer's cluster endpoint is derived from Kubernetes
  DNS (`cluster.{POD_NAMESPACE}.svc.cluster.local:50051`, invariant I9) and is not
  configurable, so the driver runs as a pod. Loopback cannot carry this suite.
- **Auth enforced.** TokenReview (`internal_auth_enforcement=Required`) stays on —
  it is the production posture. Disabling it is a diagnostic-only knob.
- **Pinned resources.** Explicit CPU/memory requests and limits on driver, cluster,
  and backend pods.
- **Node separation.** On the load-test cluster, anti-affinity places driver pods,
  cluster replicas, and the backend on distinct nodes, so every hop crosses the
  network. Co-located pods under-report the transport delta.

**Environments** — the suite always runs in a Kubernetes cluster:

| Environment | Use | Results |
|---|---|---|
| **Local** (minikube) | Developing workloads, smoke-checking the harness | Not recorded; not comparable |
| **Load-test cluster** (dedicated, multi-node Kubernetes) | Nightly and pre-release runs | Numbers of record |

## 3. Metrics

Per (backend, profile, workload, operating point):

| Metric | Definition | Source |
|---|---|---|
| **Throughput** | Attempts/sec over the measured window | Driver counter |
| **Latency** | p50 / p95 / p99 / max per op (no means) | Driver HdrHistogram |
| **Contention** | CAS conflicts / lock-already-held — counted separately, not errors | Driver |
| **Error rate** | Timeouts and `Provider{..}` errors / attempts | Driver + `cluster_provider_errors_total` |
| **Server latency** | `cluster_<primitive>_op_duration_seconds{provider,op}`, merged across all replicas | Cluster gear via OTLP |
| **Transport delta** | `client pXX − server pXX` = network + hub + auth + (de)serialization | Derived |
| **Saturation knee** | Point in a concurrency/rate sweep where p99 rises while throughput flattens | Derived from sweep |
| **Cost** | CPU / memory of cluster and backend pods | `kubectl top` |
| **Subscription health** | `cluster_subscriptions_active`, `cluster_subscriptions_reaped_total`, `cluster_watch_resets_total` | Cluster gear via OTLP |
| **Safety margin** | Renewal p99 ÷ renewal interval (leader, lease, lock); unintended leadership changes; leases or locks lost while their holder was renewing | Driver |
| **Integrity** | Pass/fail of the post-run invariant checks (§7) | Driver |
| **Drift** | Slope over time of p99, gear RSS, backend storage size, open connections, active subscriptions | Soak runs (§10) |

Client-side numbers are authoritative. Server-side numbers exist only for
attribution.

## 4. Workloads

Workloads are derived from the public SDK facades (`ClusterCacheV1`,
`DistributedLockV1`, `LeaderElectionV1`). Each one is a call sequence the API
is designed for, so together they cover every I/O method on the surface. Each
workload runs on its own so results are per-pattern; the mixed profile combines
them.

**Cache — `ClusterCacheV1`**

| `operation` | Call sequence | Load shape | Headline metric | Tier |
|---|---|---|---|---|
| `read` | `get` / `contains`, with `put` / `delete` at `read_ratio` | Seeded keys; read-mostly by default | Latency, throughput — the cleanest transport-floor probe | Core |
| `compare_and_swap` | `get` → `compare_and_swap(expected_version)`, retry on `CasConflict` | Few hot keys, read-modify-write loop | Update latency including retries, conflict rate | Core |
| `lease` | `put_if_absent(Ttl::Of)` → `compare_and_swap` on the held version every renewal interval → `delete` | `key_cardinality` leases held across the window | Renewal latency ÷ renewal interval, leases lapsed | Core |
| `watch` | `watch(key)`, then `get` on each event (events carry no value) | `participants` watchers per key while a writer updates | Delivery latency (commit → event) and read-back latency; `Lagged` / `Reset` counts | Core |
| `write` | `put` (`Ttl::Of` and `Ttl::Indefinite`) / `delete` | Write-only, spread keys | Write latency, throughput | On demand |
| `ttl_expiry` | `put(Ttl::Of)`, left to expire, observed via `watch` | Many short-TTL entries | Expiry lateness (`Expired` event time − deadline), backend sweep cost | On demand |
| `watch_prefix` | `watch_prefix(prefix)` + `get`; `watch_prefix_polling` where `features().prefix_watch` is false | Writers across keys under one prefix | Delivery latency, polling overhead | On demand |
| `scan_prefix` | `scan_prefix(prefix)` | Seeded key space, swept size | Latency vs key count (O(keys) path) | On demand |

**Lock — `DistributedLockV1`**

| `operation` | Call sequence | Load shape | Headline metric | Tier |
|---|---|---|---|---|
| `try_lock` | `try_lock(name, ttl)` → hold `hold_ms` → `release` | Non-blocking contention | Throughput, latency, contention rate | Core |
| `lock` | `lock(name, ttl, timeout)` → hold `hold_ms` → `release` | `participants` waiters on few locks (server-side wait) | Acquire latency, fairness (Jain's index), `LockTimeout` rate | Core |
| `lock_renew` | `lock` → `LockGuard::renew` every renewal interval → `release` | Many long-held locks | Renewal latency ÷ renewal interval, locks lost | On demand |

**Leader — `LeaderElectionV1`**

The SDK derives the renewal interval as `ttl / (max_missed_renewals + 1)`
(`ElectionConfig`; default 30 s / 3 = 10 s) and renews automatically.

| `operation` | Call sequence | Load shape | Headline metric | Tier |
|---|---|---|---|---|
| `elect` | `elect` / `elect_with_config` → `changed()` until `Leader` → `resign`; successor observes via `changed()` | Per round: `participants` contenders on one fresh name | Election latency, failover latency, convergence time as contenders grow | Core |
| `leader_hold` | `elect` → leadership held with auto-renew; followers observe `status` / `changed()` | `key_cardinality` elections × `participants` each | Renewal latency ÷ renewal interval, unintended leadership changes, propagation to followers, K8s Lease PUT rate; ADR-001 "50 concurrent elections" | Core |

**Capability gating.** Before running, the driver reads `features()` and
`consistency()`. A workload the backend does not support is reported as
`unsupported`, not failed, and the consistency level is recorded with the
result — an `EventuallyConsistent` cache is not comparable with a
`Linearizable` one.

**Not measured separately:** `scoped()` (key prefixing, no I/O), and
`auto_restart` / `run_while_leader` (client-side wrappers over the calls above).

**Mixed** — the noisy-neighbour profile. Production backends serve every
primitive and many consumers at once, so single-pattern runs are not enough.
The sweep CLI starts concurrent runs on separate driver pods over one shared
measured window:

| Role | Driver pod | Load |
|---|---|---|
| **Aggressor** | A | `compare_and_swap`, zipfian keys, swept from nominal to the knee |
| **Victims** | B | `leader_hold`, `lease`, `watch` at nominal rate |

Result: each victim's metrics compared with its isolated baseline. Pass criteria
are the safety targets in §6 — a victim may slow down, but must not lose
leadership, leases, or locks.

## 5. Run parameters

Parameters split by cost to change.

**Run-time** — sent to the standing driver's control API; no redeploy.

*Load and window* (all workloads):

| Parameter | Meaning |
|---|---|
| `operation` | Workload from §4 |
| `rate_per_sec` | Open-model arrival rate of the workload's primary call (for `watch` / `watch_prefix`, the writer's update rate); unset ⇒ closed model. Not used by `elect` (round-based) or `lease` / `lock_renew` / `leader_hold` (cadence comes from the renewal interval) |
| `concurrency` | In-flight cap (open model) or pool size (closed model) |
| `duration_secs` / `warmup_secs` | Measured window / discarded warm-up |

*Keyspace*:

| Parameter | Meaning |
|---|---|
| `key_cardinality` | Distinct names — keys, locks, leases, or elections; for `scan_prefix`, the key-space size |
| `key_distribution` | `uniform` \| `zipfian`; applies to `read`, `compare_and_swap`, `write`, `try_lock`, `lock` |
| `payload_bytes` | Value size for cache writes |

*API arguments* — each maps to an argument of the SDK call, so a result records
exactly what a consumer would have passed:

| Parameter | SDK argument | Workloads |
|---|---|---|
| `ttl_ms` | `Ttl::Of`; `try_lock` / `lock` `ttl`; `ElectionConfig` `ttl` | `write`, `ttl_expiry`, `lease`, lock workloads, leader workloads |
| `max_missed_renewals` | `ElectionConfig` `max_missed_renewals`; the same rule sets the renewal interval `ttl / (max_missed_renewals + 1)` for `lease` and `lock_renew` | `lease`, `lock_renew`, `leader_hold` |
| `lock_timeout_ms` | `lock` `timeout` | `lock`, `lock_renew` |
| `hold_ms` | Time between acquire and `release` | `try_lock`, `lock` |
| `read_ratio` | Fraction of ops that are `get` / `contains` | `read` |
| `poll_interval_ms` | `watch_prefix_polling` `interval` | `watch_prefix` on backends without native prefix watch |
| `participants` | Watchers per key, contenders per election, participants per held election, or waiters per lock | `watch`, `watch_prefix`, `elect`, `leader_hold`, `lock` |

Leader workloads default to the SDK's `ElectionConfig` values (`ttl_ms` 30 000,
`max_missed_renewals` 2). Every other workload that takes `ttl_ms` requires it
explicitly, since the SDK has no default there either.

A sweep file has `defaults` plus a list of `sweeps`. Within a sweep, list-valued
parameters form the cross-product; a `mixed` entry runs its roles concurrently on
separate driver pods (§4). `defaults` apply only to workloads that use them;
a parameter set explicitly in a sweep that its workload does not use is
rejected, so a typo or misplaced argument cannot silently become a no-op run.

```yaml
defaults:
  duration_secs:    60
  warmup_secs:      10
  key_distribution: uniform
  payload_bytes:    64

sweeps:
  # Saturation curves for the cache hot path (open model).
  - operation:        read
    rate_per_sec:     [500, 1000, 2000, 5000]
    concurrency:      256
    key_cardinality:  1024
    key_distribution: [uniform, zipfian]
    read_ratio:       [1.0, 0.9, 0.5]

  - operation:        compare_and_swap
    rate_per_sec:     [500, 1000, 2000, 5000]
    concurrency:      256
    key_cardinality:  [16, 1024]
    key_distribution: [uniform, zipfian]

  # Blocking-lock contention: waiters × hold time.
  - operation:        lock
    key_cardinality:  8
    participants:     [16, 64]
    hold_ms:          [1, 10]
    ttl_ms:           5000
    lock_timeout_ms:  2000

  # Leader safety margin at shrinking TTLs.
  - operation:           leader_hold
    key_cardinality:     50
    participants:        3
    ttl_ms:              [30000, 10000, 3000]
    max_missed_renewals: 2

  # Noisy neighbour: victims held at nominal load while the aggressor climbs.
  - mixed:
      aggressor:
        operation:        compare_and_swap
        rate_per_sec:     [1000, 5000, 10000]
        concurrency:      256
        key_cardinality:  16
        key_distribution: zipfian
      victims:
        - { operation: leader_hold, key_cardinality: 50, participants: 3, ttl_ms: 10000, max_missed_renewals: 2 }
        - { operation: lease, key_cardinality: 1000, ttl_ms: 15000, max_missed_renewals: 2 }
        - { operation: watch, key_cardinality: 10, participants: 32, rate_per_sec: 100 }
```

**Deploy-time** — Helm values; requires a rollout:

| Parameter | Values |
|---|---|
| Backend binding | Per-primitive provider in the cluster profile |
| Cluster replicas | 1 / 3 / 5 |
| Driver pods | 1 / 4 / 16 — each pod is one gRPC channel, i.e. one consumer process |
| Backend sizing | Postgres pool size; Redis maxmemory |
| Redis topology | `single-node` \| `sentinel` \| `cluster` |
| Auth enforcement | `required` (default) \| `disabled` (diagnostic only) |

**Standard sweeps:** backend × workload; `concurrency` or `rate_per_sec`
(saturation curve); `key_distribution`; cluster replicas (expect flat once the
backend is the ceiling); driver pods at fixed total load (exposes per-connection
cost in hub routing, auth, and replica balancing that one high-concurrency pod
hides). On demand: `payload_bytes`, `key_cardinality`, `participants`, and
`ttl_ms` downward on the renewal workloads under the mixed profile — this finds
the smallest TTL each backend can hold safely under load.

## 6. Targets

From ADR-001. Asserted only on the load-test cluster (§2).

| Backend | Envelope | Confirmation target |
|---|---|---|
| Standalone | In-process | Harness baseline — isolates transport from backend cost |
| Postgres | ~10k–50k cache ops/s; advisory lock ~0.01–0.05 ms server-side | 1000 concurrent advisory locks per connection (`try_lock` / `lock`); `compare_and_swap` sustains the band |
| K8s (native) | 2–10 ms/op; etcd ~3k–5k writes/s | 50 concurrent Lease elections (`leader_hold`); 100-entry CRD cache; write ceiling is a plateau, not a cliff |
| Redis | 100k–200k ops/s; ~0.15 ms p50 / ~0.5 ms p99 | 10,000 concurrent leases (`lease`) |

**Safety targets** — all backends, in isolation and under the mixed profile (§4).
These are hard gates in every load-test tier; a miss is a correctness failure,
not a slowdown:

| Target | Threshold |
|---|---|
| Renewal p99 (`leader_hold`, `lease`, `lock_renew`) | < renewal interval, `ttl / (max_missed_renewals + 1)` |
| Unintended leadership changes | 0 |
| Leases or locks lost while their holder was renewing | 0 |
| Integrity checks (§7) | All pass |

**Redis consistency.** Redis cache is `EventuallyConsistent` except on a verified
single-node topology (`Linearizable`). Lock and leader runs on redis require
single-node or an explicit opt-in. Cache runs work on any topology.

## 7. Measurement rules

1. **Open model by default.** Saturation and tail-latency runs of workloads
   that take a rate (§5) use `rate_per_sec` (rate-scheduled,
   coordinated-omission corrected). Closed model
   only for pure max-throughput runs. Closed-model tails are optimistic at the knee.
2. **Percentiles from full histograms.** Never report means.
3. **Warm-up is discarded.** It fills connection pools, primes the TokenReview
   cache, and lets informers sync.
4. **Seeded, fixed key sets** so cardinality is constant run to run.
5. **Repeat ≥3×** and report variance.
6. **Measure the noise floor first.** Run an unchanged build repeatedly; a
   regression threshold below the noise floor is invalid.
7. **Prove the driver is not the bottleneck.** Check driver CPU and the gRPC
   channel's HTTP/2 concurrent-stream limit; a plateau is only the system's if
   adding a second driver pod does not raise throughput.
8. **Aggregate server metrics across all replicas.** A single replica's histogram
   corrupts the transport delta.
9. **Record the environment.** Git SHA, node allocatable, and the full run config
   go into every result.
10. **Only load-test cluster results count.** Local runs validate the harness,
    not performance. Load-test results are valid for backend-vs-backend ranking,
    release-vs-release regression, and curve shape. Not valid as absolute
    capacity.
11. **Reset backend state before every run.** Truncate cluster tables, flush
    Redis, delete cluster CRDs and Leases, then seed the workload's key set.
    Leftover data from a previous run invalidates the next.
12. **Check integrity after every run.** A run whose results are wrong is invalid,
    whatever its throughput:

    | Workload | Invariant |
    |---|---|
    | `compare_and_swap` | Final counter values = number of successful CAS ops |
    | `try_lock`, `lock`, `lock_renew` | At most one holder per lock at any instant |
    | `elect`, `leader_hold` | Exactly one leader per name; no two participants in `Leader` status at once |
    | `lease` | No lease with two holders |
    | `watch`, `watch_prefix` | Per-key events in version order; none lost unless `Lagged` / `Reset` was reported |

## 8. Harness

A dedicated, unpublished `cluster-perf` crate under `gears/system/cluster/`,
with four components:

| Component | Role |
|---|---|
| **Load driver** | Long-lived in-cluster consumer gear (Profile 3). Accepts a run spec over a control API (submit run, poll result), drives one operation, records an HdrHistogram |
| **Embedded control** | The same engine with cluster + plugin linked in-process (Profile 1) |
| **Sweep CLI** | Reads the sweep file, fires each point at the standing driver pods — one run, or concurrent runs over a shared window for the mixed profile — resets backend state between points, captures `kubectl top` around each window |
| **Delta reporter** | Joins the client histogram with server histograms from the OTLP collector; computes the transport delta |

**Why not k6, ghz, or another off-the-shelf tool.** What we measure is the cost
a consumer pays, and a consumer reaches cluster only through the cluster SDK. A
generic tool calling the gRPC services directly would skip the SDK's client:
its channel handling, serialization, and service-account token attachment. It
would also have to re-implement the stateful parts in scripts: holding and
renewing lock guards, leader auto-renewal and watches, TTL lease renewal. Some
things a generic tool cannot do at all:

- run the Profile 1 embedded control, which needs the same Rust code linked
  in-process;
- check invariants that depend on SDK types, such as CAS versions, lock fencing
  tokens, and `LeaderStatus` transitions (§7).

So the load generator is the SDK itself, inside a thin driver. We still build on
standard parts (HdrHistogram, the OTLP collector, `kubectl`), and k6 keeps its
role as an optional REST smoke test (§10).

Requirements:

- **The driver wires to cluster exactly like any consumer**: cluster SDK with the
  `grpc-client` feature, profile marker + `register_cluster_profile!`, facades
  resolved from the ClientHub. It links no cluster gear code in Profile 3, so
  every call is a real round trip. It carries no custom retry, deadline, or
  connection-reuse behavior.
- **The driver is not an example gear's REST wrapper.** A wrapper that bundles
  several primitives per call cannot give per-primitive numbers; at most it is a
  k6 liveness smoke.
- **No substrate assumptions in code.** The driver reaches cluster over
  in-cluster DNS; the CLIs depend only on `kubectl`. Local vs load-test cluster is
  a Helm values preset, not a code change.
- **Deployment** reuses the platform Helm charts (platform-host/grpc-hub, cluster,
  backend, otel-collector) with one perf preset per backend.

## 9. Reporting

One artifact per run: JSON (for comparison) plus Markdown summary, containing:

- run config, git SHA, node profile;
- throughput, p50/p95/p99/max, contention and error rates (client);
- server latency per `(provider, op)` and the transport delta;
- workload breakdowns (leader, watch, lease, lock-wait);
- safety margins and integrity check results;
- cluster and backend pod CPU/memory;
- for soak runs, time series of the drift metrics (§3).

Results append to a time series keyed by run id and git SHA.

## 10. Cadence & gating

| Tier | Runs | Gate |
|---|---|---|
| **Per-PR** (optional) | k6 smoke against a consumer gear's REST endpoint | Never gates |
| **Nightly** | Standalone + postgres + k8s; all core workloads; mixed profile at one aggressor level; short sweep. Budget < 45 min including bring-up | Safety targets (§6): hard gate. Regression vs rolling baseline (e.g. p99 +20%, throughput −15%) — armed only once the noise floor is below the threshold. Record-only until then |
| **Pre-release** | All backends, core + on-demand workloads, full mixed-profile sweep, wide sweeps | Safety targets and ADR-001 confirmation targets asserted |
| **Soak** (pre-release) | Mixed profile at ~70% of the measured knee for 4–8 h, per backend | Safety targets hold for the whole run; drift slopes (§3) flat within noise; p99 in the final hour within the regression threshold of the first |

Bootstrap: ≥5 nightlies on an unchanged build establish the noise floor before
the nightly regression gate is armed. The safety gate needs no baseline and is
armed from the first run.

Soak exists because cluster's main risks build up over time and a minute-long
window cannot see them: TTL expiry and lease renewal, subscription reaping,
Postgres table bloat and vacuum, etcd database growth and compaction, CRD
accumulation, and memory or connection leaks in the gear.

## 11. Diagnosis

When a result misses its target, decompose the latency:

| Layer | Measured as | Owner |
|---|---|---|
| Backend | Raw client (`sqlx` / `fred` / `kube`) with no cluster code | Backend (not ours) |
| Plugin | `Profile 1 − raw client` | Us |
| Transport | `Profile 3 − Profile 1`, and `client − server` | Us |

Tools, used on diagnosis runs only:

- **Gear:** `tokio-console`, `pprof-rs` / flamegraph, tokio runtime metrics;
  gRPC/h2 stream counts.
- **Postgres:** `pg_stat_statements`, `pg_stat_activity`, `pg_locks`,
  `EXPLAIN (ANALYZE, BUFFERS)`, LISTEN/NOTIFY commit-lock contention.
- **Redis:** `SLOWLOG`, `INFO commandstats`/`latencystats`, `LATENCY HISTORY`;
  `MOVED`/`ASK` counts in Cluster mode.
- **K8s:** `apiserver_request_duration_seconds`, `apiserver_flowcontrol_*` (429s),
  etcd `wal_fsync_duration`, Lease PUT rate.

In-gear attribution needs child spans (pool-wait, backend call, serialization);
current spans are flat.

Loop: measure → localize → change → re-run the identical spec → keep only if it
improves and L1–L3 stay green.

Backend-specific scenarios (Postgres pool sweep and LISTEN/NOTIFY, K8s APF onset
and etcd cliff, Redis topology and Sentinel failover) live in each plugin's own
performance addendum, which references this document for method.

## 12. Prerequisites & open items

| Item | Needed for |
|---|---|
| Dedicated, multi-node load-test Kubernetes cluster with pinned node sizing | Nightly and pre-release tiers |
| Child spans in the gear (pool-wait, backend call, serialization) | In-gear attribution (§11) |
| Consumer gear with a REST endpoint | Optional k6 smoke tier |
| Fault-injection composition (Toxiproxy / DST) | Latency during failover — deferred, joint with L4 |
