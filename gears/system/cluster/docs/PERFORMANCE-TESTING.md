# Performance Testing Strategy — Cluster

> **Companion documents.** [TESTING-STRATEGY.md](./TESTING-STRATEGY.md) owns
> *correctness* across the L1–L4 pyramid (unit, conformance, per-backend
> integration, fault injection / DST). This document is the fifth, orthogonal
> concern — **how fast, at what scale, and how does it degrade** — and reuses that
> document's vocabulary (backends, primitives, profiles). The numeric envelopes
> this suite validates come from
> [ADR-001](./ADR/001-provider-compatibility-and-performance.md); the signals it
> scrapes come from the [observability contract](./OBSERVABILITY.md).

<!-- toc -->

- [1. Why cluster needs its own performance suite](#1-why-cluster-needs-its-own-performance-suite)
- [2. Goals & non-goals](#2-goals--non-goals)
- [3. What we measure — the SLIs](#3-what-we-measure--the-slis)
- [4. The envelopes to validate](#4-the-envelopes-to-validate)
- [5. Architecture](#5-architecture)
  - [5.1 Components](#51-components)
  - [5.2 The load driver *is* the consumer](#52-the-load-driver-is-the-consumer)
  - [5.3 One driver, two profiles — the built-in control](#53-one-driver-two-profiles--the-built-in-control)
  - [5.4 Two control planes — so a sweep isn't N redeploys](#54-two-control-planes--so-a-sweep-isnt-n-redeploys)
  - [5.5 System diagram](#55-system-diagram)
  - [5.6 Crate layout](#56-crate-layout)
- [6. The network path, and why it dominates](#6-the-network-path-and-why-it-dominates)
- [7. Workload models](#7-workload-models)
- [8. The load generator](#8-the-load-generator)
- [9. Configurability — the run matrix](#9-configurability--the-run-matrix)
- [10. Metrics collection & reporting](#10-metrics-collection--reporting)
- [11. Environment fidelity & repeatability](#11-environment-fidelity--repeatability)
- [12. CI cadence & regression gating](#12-ci-cadence--regression-gating)
- [13. Diagnosis & bottleneck investigation](#13-diagnosis--bottleneck-investigation)
  - [13.1 Attributing where the time goes](#131-attributing-where-the-time-goes)
  - [13.2 Profiling the gear](#132-profiling-the-gear)
  - [13.3 Backend-side introspection](#133-backend-side-introspection)
  - [13.4 The investigation loop](#134-the-investigation-loop)
- [14. Per-plugin performance addendum](#14-per-plugin-performance-addendum)
- [15. Phasing](#15-phasing)
- [16. Decisions & open questions](#16-decisions--open-questions)

<!-- /toc -->

## 1. Why cluster needs its own performance suite

Every other layer of cluster testing runs the primitives **in-process**. Unit and
smoke tests drive an in-process stub; the conformance suite (L2) and even the
per-backend integration tests (L3) instantiate a backend *inside the test binary*
and call the trait directly. None of them cross a network boundary, and that is
exactly the property that makes them unsuitable for performance work.

In production the cluster gear is deployed **out-of-process**. A consumer does not
hold an `Arc<dyn ClusterCacheBackend>` — it holds a gRPC client and every
operation is a round trip:

```
consumer gear ──gRPC──▶ grpc-hub ──gRPC──▶ cluster gear ──native──▶ backend
     (client)          (auth + route)      (facade + provider)     (pg / k8s / redis)
```

A `get` that takes 40µs against an in-process `HashMap` becomes a multi-hop
network call with TokenReview auth, protobuf (de)serialization, and a backend
round trip on top. **The number that matters to a consumer author is the one
measured end-to-end, over the wire, in a deployed cluster** — and nothing we have
today measures it. This is also the only setting where the ADR-001 claims
("Postgres ~10k–50k cache ops/sec", "K8s 2–10ms per operation") can be confirmed
against the *deployed gear*, not just the client library.

**Why k6-at-the-gateway may not be the best fit for cluster.** The direction other
gears are leaning toward is k6 pointed at the edge/API gateway — an external tool
measuring what an external API caller sees. That approach can't reach cluster: it is
an internal `system` gear whose entire surface is the four gRPC
coordination services behind grpc-hub — its `register_rest` adds no routes, so there
is nothing at the edge for k6 to target. This also moves the *vantage point*. A
normal gear's meaningful number is the external caller's view through the gateway;
cluster has no external callers — its callers are other gears calling it over gRPC
**in-cluster**. So the number that matters is the **consumer gear's** view, which is
exactly what the in-cluster load driver embodies (§5.2) and what k6 — even as the
smoke tier (§8) — can only reach indirectly, by driving a *consumer* gear's REST
wrapper rather than cluster itself. Pointing k6 at the edge would measure a path
cluster does not have.

A note on what this is **not**: this is not a micro-benchmark suite. `criterion`
(already used elsewhere in the workspace) is the right tool for measuring the SDK
default backends' CAS/version arithmetic or a provider's serialization cost in
isolation, and we may add a thin `criterion` floor to attribute overhead (§10).
But the headline deliverable is a *macro* suite that measures the real,
deployed, over-the-wire system — because the network trip is the cost we are
trying to characterize.

## 2. Goals & non-goals

**Goals**

- **G1 — End-to-end throughput & latency per primitive, per backend, in a real
  cluster.** Client-observed ops/sec and p50/p95/p99 for cache, lock, and leader
  operations against standalone / postgres / k8s-native / redis.
- **G2 — Configurable across deployment profiles.** A single run is parametrized
  by backend, primitive, operation mix, concurrency, payload size, and cluster
  replica count — so we can sweep, not hand-craft, scenarios.
- **G3 — Attribute the round trip.** Separate *client-observed* latency from
  *server-observed* latency (the observability histograms) so we can quantify the
  network/hub/serialization tax versus the backend's own cost.
- **G4 — Detect regressions.** Repeatable enough that a run-to-run delta is
  signal, so a PR that doubles a hot-path allocation is caught.
- **G5 — Validate the ADR-001 envelopes and confirmation targets** (§4) against
  the deployed gear, converting them from documented claims into asserted ones.

**Non-goals**

- **Absolute production capacity planning.** minikube on a laptop is not a
  production node; see §11. The suite produces *relative* and *comparative*
  numbers (backend-vs-backend, release-vs-release), not a datasheet.
- **Correctness under fault.** Split-brain, partition tolerance, and watch
  recovery belong to L4 (DST + Toxiproxy) in the testing strategy. Where
  performance and fault overlap — latency *during* a backend failover — we note
  the seam (§15) but do not own it here.
- **Replacing the conformance suite.** This suite assumes correctness is already
  established by L1–L3; it measures speed, not truth.

## 3. What we measure — the SLIs

Four families, per (primitive, operation, backend, profile):

| SLI | Definition | Source |
|---|---|---|
| **Throughput** | Sustained successful ops/sec at a fixed concurrency | client-side counter |
| **Latency** | p50 / p95 / p99 / max of per-op wall-clock | client-side HdrHistogram |
| **Saturation** | Throughput and p99 as concurrency climbs (the knee) | swept runs |
| **Error rate** | Fraction returning `Provider{...}` / timeouts under load | client + `cluster_provider_errors_total` |

The suite records **two latency views** and the delta between them is a
first-class output:

- **Client-observed** — wall-clock at the load generator, the full round trip of
  §6. This is the consumer's truth.
- **Server-observed** — `cluster_<primitive>_op_duration_seconds` scraped from the
  cluster gear (already emitted per the [observability contract](./OBSERVABILITY.md)
  §5), the histogram labeled `(provider, op)`.

`client_p99 − server_p99` ≈ the network + grpc-hub + auth + (de)serialization
overhead. Making that number visible is half the value of running OoP at all —
it is the cost the in-process suites structurally cannot see.

There is a second, cleaner attribution lever the SDK hands us for free: **consumer
source is identical across Profile 1 (embedded, in-process, zero hops — the
`LocalClusterClient` returns the backend `Arc` directly) and Profile 3 (OoP, one
gRPC hop — `RemoteClusterClient`).** Running the *same* load driver against the
*same* backend in both profiles isolates the entire transport tax as a subtraction:
`profile3_latency − profile1_latency` is the round trip, measured with everything
else held constant. Profile 1 also gives the **backend's own ceiling** with no
network in the way — the floor the Profile-3 number can never beat. We should run
both wherever the backend supports embedding (standalone, postgres, and redis do —
each links in-process and talks to its store directly, skipping the cluster-gear
gRPC hop; k8s-native is deployment-shaped either way).

Every run also captures the **cost side** so a throughput number is never
reported naked: cluster-gear and backend pod CPU/memory (via `kubectl top` /
cAdvisor), and for the coordination primitives the two gauges the gear owns —
`cluster_subscriptions_active` and `cluster_subscriptions_reaped_total`
(OBSERVABILITY §5.2) — so we can see whether the abandoned-subscription sweep is
keeping up under a churny leader/watch workload.

## 4. The envelopes to validate

[ADR-001](./ADR/001-provider-compatibility-and-performance.md) already commits to
per-backend performance envelopes and a "Confirmation" list of throughput targets.
This suite is the machinery that confirms them. Restated as acceptance targets:

| Backend | Documented envelope (ADR-001) | Confirmation target to assert |
|---|---|---|
| **Standalone** | in-process, no network from backend's view | Baseline/ceiling for the *harness itself* — isolates gRPC+hub cost from backend cost |
| **Postgres** | ~10k–50k cache ops/sec; advisory lock ~0.01–0.05ms server-side | 1000 concurrent advisory locks per connection; cache CAS hot-loop sustains documented band |
| **K8s (native)** | 2–10ms per op; etcd ~3k–5k sustained writes/sec ceiling | 50 concurrent Lease elections; 100-entry CRD cache; confirm the write ceiling is a *ceiling*, not a cliff |
| **Redis** | native cache (hash-per-entry, Lua CAS, `PX` TTL) + native lock (`SET NX PX`, token-fenced Lua); 100k–200k ops/sec, ~0.15ms p50 / ~0.5ms p99 (the plugin's own DESIGN restates this envelope) | 10,000 concurrent subscriber leases (the event-broker workload) — the highest-throughput target in the suite |
| **NATS / etcd** *(future)* | 100k+ / 3k–5k ops/sec resp. | deferred until plugins exist |

> **Backend availability.** Shipped/near-shipped backends are `standalone`,
> `postgres`, the native `k8s` plugin, and **`redis`** — the redis plugin is
> implemented on `feat/cluster-redis`, awaiting merge, and is already an
> unconditional dependency of `cf-gears-cluster`, so a merged `cluster-oop` image
> resolves `provider: redis` in every build (no cargo feature to flip). The
> motivating example ("cache throughput of postgres or redis") is therefore fully
> serviceable — both are real backends. `NATS` and `etcd` remain future work;
> keep the suite backend-agnostic so they slot in as config values when their
> plugins land.
>
> ⚠️ **Redis is `EventuallyConsistent` by default — this is a first-class perf
> *and* correctness dimension, not a footnote.** Its cache declares
> `Linearizable` only under a verified single-node durable topology; a Redis-only
> profile that omits `lock` / `leader_election` **fails startup** because the
> strict CAS-based default constructor rejects an eventually-consistent cache
> (the operator must opt in explicitly). For this suite that means: redis **cache**
> throughput is testable on any topology (single-node / Sentinel / Cluster), but
> redis-backed **lock** and **leader** runs must either use the single-node
> Linearizable topology or the explicit opt-in — and the *topology itself*
> (single-node vs Sentinel vs Cluster) is a performance variable worth sweeping
> (§9), since Cluster-mode sharding and Sentinel failover change both the ceiling
> and the tail.

The ADR is explicit that these span orders of magnitude (leader election <10
ops/sec; subscriber leases 10,000+ concurrent), so the suite must be able to
express both a *low-count-correctness-critical* workload and a
*high-count-throughput* workload without reshaping the harness — see §7.

> **Measured results — cleared 2026-09-17, pending a fresh run.** The prior
> Phase-2 numbers (postgres + single-node `redis`, minikube) have been removed;
> a new performance run will repopulate this section. Until then, treat the
> envelopes and confirmation targets above as *documented but unvalidated*
> against the current build.

## 5. Architecture

The suite reuses the demo's OoP deploy harness for **deployment** — the helm charts
under `deploy/helm/{cluster,cluster-consumer}`, `deploy/oop-smoke.sh`, the Docker
images, the minikube/kind bring-up, and the proven grpc-hub auth + DNS wiring that
already carries a **verified** cross-pod cache/lock/leader round trip on shared
Postgres. On top of that deployment it adds three components of its own, all in the
`cluster-perf` crate (§16): a **load driver**, an **orchestrator**, and a
**reporter**. It does not reinvent deployment, and it puts no business consumer on
the load path — the driver is the consumer (§5.2).

> **Reality check on where that harness lives — resolved.** The deploy substrate
> has since landed on this branch (§15, phase 0): the reused umbrella charts live at
> the **repo root** under `deploy/helm/` (`toolkit-platform`, `platform-host`,
> `cluster`, `cluster-perf`, `postgres`, `redis`), *not* under the gear tree — the
> only manifest committed *inside* the gear remains the k8s plugin's
> `ClusterCacheEntry` CRD (`plugins/k8s-cluster-plugin/deploy/crd.yaml`). The
> perf-specific presets are `deploy/helm/toolkit-platform/values-perf.yaml`
> (postgres system-under-test) and `values-perf-redis.yaml` (single-node redis). So
> the earlier "land the deploy manifests first" dependency is met for minikube; the
> **remaining** deploy gap is a real-cluster preset (`values-perf-prod.yaml`) — see
> §5.6 and §11. The harness *code* is already cluster-portable (§5.6).

### 5.1 Components

| Component | Role |
|---|---|
| **Load driver** | An in-cluster consumer gear whose "workload" is the benchmark loop. Resolves `ClusterCacheV1` / `DistributedLockV1` / `LeaderElectionV1` from the ClientHub and drives one configured operation from a pool of in-flight tasks; records client latency into an HdrHistogram exposed via its control API (`GET /runs/{id}`). It does **not** serve a `/metrics` scrape route — like every OoP gear it pushes its telemetry over OTLP (§10). |
| **Orchestrator** | The `cluster-perf-sweep` CLI: reads `perf-run.yaml` (§9), drives the **run-time** plane — firing each swept point at the *already-deployed* driver's control API (no redeploy) — and scrapes pod CPU/mem via `kubectl top` around each window. It links no cluster/toolkit code and does no infra: the **deploy-time** plane (Helm values, replicas, backend) is operated separately via `helm` + the `values-perf*.yaml` presets (§5.4/§5.6), not by this CLI. |
| **Reporter** | The `cluster-perf-delta` CLI: joins the driver's client-side HdrHistogram with the cluster gear's server-side metrics (pushed via OTLP to the collector, §10), computes the `client − server` delta, and emits JSON + Markdown. |

### 5.2 The load driver *is* the consumer

The single most important architectural idea: **there is no separate business
consumer on the load path — the driver plays that role itself.** It is a
purpose-built consumer gear that reuses the *wiring pattern* of the demo's
`cluster-consumer` (a typed profile marker + `register_cluster_profile!`, a
`cluster-sdk` dependency with the `grpc-client` feature, k8s service-account token
attachment, and resolving the `*V1` facades from the ClientHub in `start`) but
replaces its REST endpoint with the load engine. Because it wires to cluster
exactly as any real consumer does, its numbers *are* a real consumer's numbers. To
keep that a *tested* invariant rather than a prose claim, the driver and
`cluster-consumer` must **share the wiring code path** (the same profile marker +
`register_cluster_profile!` + facade resolution), so a change that makes the example
diverge from a real consumer fails a test — the driver cannot silently drift into a
non-representative client (retry policy, connection reuse, deadlines) and keep
reporting numbers we trust.

`cluster-consumer` **is kept** — but for a different job. It is the canonical,
minimal **integration example** a gear developer copies to learn how to wire their
own gear to cluster; it stays deliberately simple. It is *not* on the perf load
path (its `/roundtrip` bundles all three primitives into one call, §8), though
because it is deployed anyway it remains available as a trivial k6 liveness smoke if
we want one (option A, §8). Keeping the two apart is the point: the example
optimizes for *clarity*, the driver for *measurement*.

### 5.3 One driver, two profiles — the built-in control

The same driver, built two ways, yields the network-tax subtraction of §3, because
consumer source is identical across profiles (invariant I1):

- **Profile 3 (oop)** — the `grpc-client` build; the driver pod calls cluster over
  gRPC through grpc-hub. Measures the full round trip — the headline number.
- **Profile 1 (embedded)** — the driver links the cluster gear + plugin in-process
  and resolves via `LocalClusterClient` (an `Arc`, zero hops), talking to the
  backend directly. **No hub and no cluster pod exist in this shape** — the driver
  process *is* the system under test. This is the experimental control; the
  isolated cost of the OoP path is `profile3 − profile1`.

### 5.4 Two control planes — so a sweep isn't N redeploys

The run knobs of §9 split cleanly by *what changing them costs*, and the suite is
built around that split:

- **Deploy-time plane** (Helm, i.e. a redeploy — currently operated manually via
  `helm` + the `values-perf*.yaml` presets, not wired into the sweep CLI). Mostly
  Helm values already present in the demo charts or trivially added:
  - **cluster replica count** — the demo runs 3; sweep 1 / 3 / 5 to watch the gear
    scale horizontally against a shared backend (the backend, not the gear, is
    usually the ceiling).
  - **backend selection & profile** — values-only: the `demo` profile binds
    `cache` / `lock` / `leader` to a provider per primitive.
  - **backend sizing & topology** — Postgres pool size; Redis maxmemory *and*
    topology (single-node / Sentinel / Cluster, §4), which sets both the ceiling and
    whether lock/leader can bind at all.
  - **auth enforcement** and **resource limits** — the TokenReview posture (§6) and
    pinned CPU/memory requests+limits so runs are comparable (§11).
- **Run-time plane** (orchestrator → the driver's control API, *no* redeploy).
  `primitive`, `operation`, `concurrency`, `payload_bytes`, `key_cardinality`,
  `duration`, `warmup`. The driver is long-lived and takes a run spec per call, so
  the `concurrency: [1, 8, 32, 128]` saturation sweep is **four control calls
  against one standing deployment**, not four deploys. Only the deploy-time plane
  pays for a rollout.

### 5.5 System diagram

```
                cluster-perf crate  (gears/system/cluster/cluster-perf)
      ┌────────────────────────────────────────────────────────────────────┐
      │  cluster-perf-sweep (CLI)        cluster-perf-delta (reporter)     │
      │  • read perf-run.yaml            • join client HdrHistogram        │
      │  • drive standing driver         + server metrics (OTLP)           │
      │  • run-time plane, no deploy     • compute client−server delta     │
      │  • scrape kubectl top            • emit JSON + Markdown            │
      └───┬───────────────────────────────▲─────────────────┬──────────────┘
   run(spec) │ control API                 │ scrape collector  │ run-time
            ▼ (run-time plane, §5.4)        │ (OTLP metrics)    ▼ plane (§5.4)
 ┌───────────────── in-cluster (minikube / kind / real k8s) ──────────────────┐
 │  ┌───────────────────┐ gRPC+token ┌──────────┐  gRPC  ┌────────────────┐   │
 │  │ load-driver gear  │───────────▶│ grpc-hub │───────▶│ cluster gear   │   │
 │  │ (cluster-perf-oop)│            │ TokenRev │        │ (N replicas)   │   │
 │  │ • cluster-sdk     │◀───────────│  (warm)  │◀───────│ facade+provider│   │
 │  │ • grpc-client     │            └──────────┘        └───────┬────────┘   │
 │  │ • task pool       │                                        │ native     │
 │  │ • HdrHistogram    │◀─ run(spec) ── sweep CLI               ▼            │
 │  │ • control API     │                              ┌─────────────────┐    │
 │  │ • OTLP push ──────┼─▶ otel-collector ─▶ delta    │ backend         │    │
 │  └───────────────────┘                              │ pg / k8s / redis│    │
 │                                                     └─────────────────┘    │
 └────────────────────────────────────────────────────────────────────────────┘
   Profile 3 (oop)      = the path above (measures the full round trip).
   Profile 1 (embedded) = cluster-perf-embedded links cluster+plugin in-proc →
                          LocalClusterClient (Arc) → backend. No hub, no cluster pod.

   cluster-consumer (the integration EXAMPLE, §5.2) is deployed alongside but is
   NOT on the load path — developers copy it to wire their own gear to cluster.
```

### 5.6 Crate layout

The suite is one crate — `cf-gears-cluster-perf` (`publish = false`) at
`gears/system/cluster/cluster-perf/`. It is **one library plus four
feature-gated binaries**: the library is always linkable in-process (that is what
lets Profile 1 embed it, §5.3), and each binary is gated behind a Cargo feature so
the OoP gear image, the client-side CLIs, and the embedded control never drag in
each other's dependencies.

**Binaries** — each maps to a §5.1 component (plus the Profile-1 control):

| Binary | Path | Feature | Role |
|---|---|---|---|
| `cluster-perf-oop` | `src/main.rs` | `oop_module` | The **load driver** (§5.1) — the Profile-3 in-cluster gear image. Adds the bootstrap entrypoint, CLI, and mimalloc allocator. |
| `cluster-perf-sweep` | `src/orchestrator_main.rs` | `orchestrator` | The **orchestrator** (§5.1/§9) — a client-side sweep CLI; reads `perf-run.yaml`, drives the *standing* driver's control API (run-time plane, no redeploy), emits JSON + Markdown. |
| `cluster-perf-delta` | `src/delta_main.rs` | `orchestrator` | The **reporter** (§10) — runs one workload, then joins its client SLI with the cluster gear's server histogram scraped from the OTLP collector to compute the `client − server` delta. |
| `cluster-perf-embedded` | `src/embedded_main.rs` | `embedded` | The **Profile-1 zero-hop control** (§5.3) — links the cluster gear + standalone plugin in-process via `LocalClusterClient`. No Kubernetes, no gRPC. |

**Features.** `default = []` (bare library); `oop_module` (OoP bootstrap: `clap`,
`mimalloc`, `toolkit/bootstrap`); `k8s-auth` (TokenReview platform-plane auth for
Profile 3 / real cluster — `toolkit/k8s-auth`); `orchestrator` (client-side CLIs:
`reqwest`, `serde_json`, `serde-saphyr`, `tracing-subscriber`); `embedded`
(Profile-1 control — links `cluster` + `cf-gears-standalone-cluster-plugin`). The
cluster contract is an unconditional dep with `grpc-client` on, since the
framework's proxy-wiring replays the SDK's `RemoteClusterClient` to place a remote
`dyn ClusterClient` in the hub.

**Library modules** (`src/`):

| Module | Responsibility |
|---|---|
| `lib.rs` | Crate root; exposes the gear + engine, gates `orchestrator` / `embedded` modules by feature. |
| `gear.rs` | Gear definition, cluster profile marker, and control-API wiring — the **shared wiring code path** the driver co-owns with `cluster-consumer` (§5.2). |
| `config.rs` | Gear configuration, mounted under `gears.cluster-perf.config`. |
| `rest.rs` | The control surface: `POST /cluster-perf/v1/runs` (submit), `GET /runs/{id}` (poll result), and an anonymous ping. |
| `engine.rs` | The load engine: resolves a primitive's facade, runs the configured workload — open-model (`rate_per_sec`, the coordinated-omission fix, §8) or closed-model — and records latency into an HdrHistogram. |
| `orchestrator.rs` | Client-side sweep driver behind `cluster-perf-sweep` (feature `orchestrator`). |
| `embedded.rs` | Profile-1 embedded control behind `cluster-perf-embedded` (feature `embedded`). |
| `main.rs`, `orchestrator_main.rs`, `delta_main.rs`, `embedded_main.rs` | The four binary entrypoints above. |
| `registered_gears.rs` | Links **only this crate's own** `#[toolkit::gear]` inventory into the OoP binary — deliberately *not* the cluster gear (Profile 3 resolves it over gRPC via the DirectoryService, which is what makes the round trip genuinely OoP). |

**Supporting files.** `Cargo.toml`, `README.md`, and `perf-run.example.yaml` (the
§9 run-matrix example the sweep CLI consumes).

**Deploy artifacts** live at the repo root, not under the crate (they reuse the
demo umbrella charts, §5): the driver chart is `deploy/helm/cluster-perf`, and the
perf presets are `deploy/helm/toolkit-platform/values-perf.yaml` (postgres) and
`values-perf-redis.yaml` (single-node redis). The harness *code* carries no
minikube assumption — the driver reaches cluster over in-cluster DNS (invariant I9,
§6) and the sweep/delta CLIs shell out only to `git` and `kubectl`
(`top pods`, `port-forward`), both context-agnostic — so it can be pushed at a real
multi-node cluster **unchanged**; the only outstanding deploy delta is a
real-registry preset (`values-perf-prod.yaml`: `pullPolicy: IfNotPresent` +
imagePullSecret, pinned `resources:`, enabled persistence/`storageClass`), a
values-only change the charts already support (§11, §16).

## 6. The network path, and why it dominates

A **hard invariant** shapes everything here. From `cluster-sdk/src/wiring.rs`
(invariant I9): a consumer's cluster endpoint is **derived from Kubernetes DNS** —
`cluster.{POD_NAMESPACE}.svc.cluster.local:50051` — and is **non-configurable by
design**. There is no cluster-side endpoint key in any form. The consequence for
this suite is decisive:

> **A real cluster round trip requires real Kubernetes DNS.** On loopback the name
> does not resolve and every call fails with a typed `Provider{ConnectionLost}`.
> Therefore the performance suite is a **minikube/kind suite, not a loopback
> suite** — the seam-only loopback tier used for functional smoke cannot carry a
> throughput workload.

The path also fixes what the latency breakdown *is*. Each op crosses: load-gen →
edge/hub gRPC, grpc-hub **TokenReview auth** (a real cost under the
`internal_auth_enforcement=Required` posture the demo runs), routing to a cluster
replica, the facade's protobuf decode, the provider's native backend call, and the
whole thing back. §3's `client − server` delta is precisely the sum of everything
that is *not* the last hop. Two design implications:

- **Auth is on the hot path — and stays there.** TokenReview enforcement is the
  production posture and is not optional (§16). The framework caches validated
  tokens, so the steady-state per-op auth cost is low *once warm* — but the
  measured window must start after the warm-up primes that cache (§11), or the
  first-call validation cost smears the p99. The `auth_enforcement: disabled`
  toggle exists only to attribute the residual warm cost, never as a shipped
  shape.
- **Replica fan-out matters.** With N cluster replicas behind the hub, requests
  spread across replicas that all talk to the *same* backend — so the gear scales
  but the backend is shared. This is the intended place to observe the ADR-001
  "backend is the ceiling" claim directly.
- **Profile 1 is the control.** Because the same consumer runs embedded, the
  zero-hop Profile-1 measurement (§3) is the experimental control for every
  Profile-3 number — the isolated cost of *this exact path* is the difference
  between them.

Two client behaviors the load driver must account for (from the remote client
seam): a blocking `lock()` waits **server-side** with no per-call client deadline
(the wait time is a workload input, not a hang), and watches are long-lived
streams — so a watch-heavy workload measures stream setup + event-delivery
latency, not request/response throughput.

## 7. Workload models

ADR-001 names three real consumer workloads spanning several orders of magnitude.
The suite models those — **plus the per-facade read, blocking, and at-scale
baselines the original three leave uncovered** (a review of the cache / lock /
leader facades against what the driver actually drives, 2026-09-17) — as named,
parametrized profiles rather than ad-hoc scripts:

| Workload | Primitive & op | Shape | Derived from |
|---|---|---|---|
| **Subscriber leases** | `cache.put_if_absent` + periodic renewal + TTL expiry | *High count* — up to 1000 leases/instance, ×N instances = 10k concurrent, renewed every few seconds | Event broker |
| **Rate-limit counters** | `cache.compare_and_swap` hot loop | *High frequency* — small key set, read-modify-write churn, CAS-conflict rate is itself a metric | OAGW |
| **Config locks** | `lock.try_lock` / `lock` / `release` | *Contention* — many waiters on few locks; measure acquire latency and fairness | OAGW |
| **Leader election** | `leader.elect` / renew / resign | *Low count, correctness-critical* — few elections, but measure failover/renewal latency and that renewal cost stays flat | Scheduler |
| **Watch fan-out** | `cache.watch` / `watch_prefix` (long-lived streams) | *Broadcast* — M subscribers on a key while a writer drives it; measure event-delivery latency and lag/drop as write-rate × subscriber-count climbs | Any reactive consumer |
| **Cache read / RW-mix** *(new)* | `cache.get` (+ `cache.put` / `delete` at a `read_ratio`) | *Read-mostly* — seeded key set, configurable read:write and hit ratio; the **cleanest transport-floor probe** (§3) — a `get` carries no CAS amplification, lock, or notify, so `client − server` on it is the purest round-trip tax | Config / feature-flag / session reads |
| **Blocking-lock contention** *(new)* | `lock.lock` (blocking, `timeout`) + `release` | *Wait* — many waiters block **server-side** (§6) on few locks; measure acquire-under-wait latency, timeout rate, and blocking-queue fairness — a distinct path from the `try_lock` config-locks workload | OAGW singletons |
| **Leader election at scale** *(new)* | `leader.elect` × N candidates on **one** name | *At-scale correctness* — N contenders converge on one leader; measure convergence time, the exactly-one-leader invariant, and renewal PUT-rate — asserts the ADR-001 "50 concurrent Lease elections" target (§4) | Scheduler HA |

**Further workloads (diagnosis & native-path coverage, §13/§14).** The set above is
the headline shapes; four more fill in facade paths the headline set cannot reach,
run **on demand** (the diagnosis/pre-release tiers) rather than every nightly:

- **Leader renewal steady-state** — `leader.elect` auto-renew across many concurrent
  elections. Asserts §7's own "renewal cost stays flat" claim and drives the K8s
  Lease-PUT-rate-vs-etcd-write-ceiling pressure (§14) that the cache lease workload
  never touches (leases are cache `put_if_absent`, not leader renewals).
- **Prefix scan** — `cache.scan_prefix` over a seeded key space. An O(keys) range
  read on a distinct backend path (postgres prefix scan, redis `SCAN` cursor, k8s
  list) — a known footgun the point-op workloads cannot surface.
- **Leader-observer fan-out** — M followers on `LeaderWatch::{status, recv}` while
  the leader churns (resign → re-elect). Leadership-change propagation latency to
  many watchers — the leader-primitive analogue of cache watch fan-out.
- **Lock-renewal churn** — `LockGuard::renew` on many long-held locks. Heartbeat
  write pressure for held locks; lowest priority, as its write-ceiling signal
  overlaps the leader-renewal workload above.

**Explicitly not modeled:** `cache.contains` (get-shaped — covered by the read
workload) and `compare_and_swap_value` / `compare_and_delete` (semantic variants of
the CAS already covered by rate-limit).

Each model declares its **operation mix** (e.g. rate-limit = 100% CAS after an
initial seed; subscriber-lease = put_if_absent burst then a renewal steady-state),
its **key cardinality** (rate-limit is a few hot keys; leases are many cold keys),
and its **value payload size** (ADR-001 flags the Postgres 8KB LISTEN/NOTIFY limit
and that a `u64` version is 8 bytes — payload size changes the wire cost and, on
Postgres, the notification path); a clean way to declare it is **named size
classes** (empty / small / large payloads selected per workload) rather than only
raw byte counts, which keeps the fixed key/payload set constant run-to-run (§11).
The renewal cadence is the subtle one: it turns
a "count" into a sustained ops/sec (1000 leases renewed every 3s = ~333 ops/sec of
background load *before* any new claims), which is exactly the etcd-write-ceiling
pressure ADR-001 warns about for K8s.

**Why watch fan-out earns its own model.** ADR-001 flags watch/notification as the
sharpest point of divergence between backends, and it is request/response's blind
spot: Postgres `LISTEN/NOTIFY` serializes on a global commit lock, Redis Cluster's
pre-7.0 `PUBLISH` broadcasts to every node (~12,500 publishes/sec ceiling) with
keyspace notifications off by default, and the redis plugin publishes its *own*
events rather than trusting keyspace notifications at all. None of that surfaces
under a request/response workload — only a fan-out (M subscribers on a key while a
writer drives it) does, and it reads the `cluster_watch_resets_total` and
`cluster_subscriptions_active` signals (§10) to see lag and reaping under load.
This is the highest-value workload for the redis + k8s bottleneck hunt.

## 8. The load generator

No load tooling exists in the repo today (no k6/vegeta/hdrhistogram/criterion-for-
this) — this is a greenfield choice. Three candidate approaches, not mutually
exclusive:

**A. Drive the existing consumer REST endpoint with an HTTP load tool (k6 /
vegeta).** The `cluster-consumer` demo already exposes
`POST /cluster-consumer/v1/roundtrip` which exercises all three primitives
(lock acquire+release, cache put+get, leader join+settle+resign), scoped, through
the edge. Cheapest to stand up; reuses a verified path. *But* each HTTP call does a
fixed *bundle* of primitive ops, so it can't isolate "cache put throughput" from
"lock throughput", and the consumer's own REST+handler overhead sits between the
load tool and the gRPC call we care about. Good for a **first smoke / trend line**,
poor for **per-primitive precision**.

**B. A purpose-built Rust load driver (recommended primary).** A binary in the
`cluster-perf` crate (§16) that depends on `cluster-sdk` with the `grpc-client` feature,
resolves `ClusterCacheV1` / `DistributedLockV1` / `LeaderElectionV1` from the
ClientHub exactly as a real consumer does, and hammers a *single* configured
operation from a tunable pool of in-flight tasks. This exercises the **real gRPC
client path** (the thing we are measuring) with none of the REST/bundle noise, and
gives us direct access to per-op timing for an HdrHistogram. It reads a workload
config (§9) and reports structured results. This is where per-primitive,
per-backend precision comes from. It runs **in-cluster** (as a pod, so DNS resolves
and it shares the network fabric with production) — an external driver would need
`minikube tunnel` and would measure laptop↔VM latency instead of pod↔pod.

**C. A `criterion` micro-floor (optional, in-process).** Not over the wire —
measures the SDK/provider cost with zero network, to establish the *floor* that
option B's client-observed number sits above. Purely for attribution (§10); a
"nice to have", not the deliverable.

Recommendation: **build B, keep A as a smoke tier, add C only if attribution needs
it.** B is the artifact that answers "what throughput does a consumer actually
get."

> **Decision (2026-09-17): adopt k6 as the smoke tier — and only the smoke tier.**
> k6 (option A) is the cheap, no-Rust liveness/trend check and an *independent*
> cross-check on the Rust driver's self-reported numbers; it drives the consumer's
> bundled `POST /cluster-consumer/v1/roundtrip` and runs in the per-PR tier (§12),
> never as a hard gate. It is **not** a substitute for the Rust driver (B): k6 can
> only fire the bundled round trip over REST, so it cannot isolate a per-primitive
> number, cannot run the Profile-1 embedded control, and cannot produce the clean
> `client − server` transport delta — the deliverables B exists for. Its former
> unique selling point (open-model) is no longer unique now that the Rust driver
> defaults to open-model. **Prerequisite:** k6's target does not exist on this
> branch — `cluster-consumer` is `enabled: false` in `values-perf.yaml` and its
> crate + `/roundtrip` live on `feat/cluster-live-demo` (unmerged), so the smoke
> tier is blocked on landing that consumer first.

**Closed vs open model — mind coordinated omission.** Option B as described is
*closed-model*: a fixed pool of in-flight tasks, each issuing the next request only
after the previous completes. When the system stalls, the pool stops issuing — so a
naive HdrHistogram under-counts exactly the latency you care about at the knee (the
classic coordinated-omission trap: the tail looks *better* precisely when the system
is worst). For trustworthy tails the driver must either run *open-model* (schedule
requests at a fixed arrival rate, independent of completions) or apply
coordinated-omission correction. k6's `constant-arrival-rate` executor is
open-model — a second reason it is the right smoke tool (§16) — and the Rust driver
**defaults to** rate-scheduled (open-model) load for the saturation and tail-latency
runs, the runs that carry most of the suite's value, keeping closed-model only for
pure max-throughput. Open-model is the default posture here, not an opt-in: a
closed-model p99 at the knee is optimistic by construction — and reporting a
*mean* latency compounds the lie, since the average smears the very tail the knee
is about. The driver therefore records a full per-op histogram (§10), never a
running average, and keeps open-model the default for the runs where the tail is
the point.

## 9. Configurability — the run matrix

A run is one point in a cross-product, driven by a single config file (YAML,
consistent with the gear config style) that both the Helm deploy and the load
driver read. Sketch:

```yaml
# perf-run.yaml — one scenario
backend:        postgres          # standalone | postgres | k8s | redis  (nats/etcd future)
deployment:     oop               # oop (Profile 3, gRPC hop) | embedded (Profile 1, zero hop, the control)
profile:        demo              # which cluster profile binds cache/lock/leader
primitive:      cache             # cache | lock | leader
workload:       rate-limit        # subscriber-leases | rate-limit | config-locks | leader-election
                                  #  | watch-fanout | cache-read | blocking-lock | election-at-scale
                                  #  | leader-renewal | prefix-scan | leader-observer | lock-renewal
operation:      compare_and_swap  # narrows the workload to one op: get | put | delete | put_if_absent
                                  #  | compare_and_swap | try_lock | lock | scan_prefix | elect
concurrency:    [1, 8, 32, 128]   # swept — produces a saturation curve; doubles as the candidate
                                  #  count for election-at-scale and the waiter count for blocking-lock
key_cardinality: 16               # hot-key count for CAS; high for leases
key_distribution: uniform         # uniform | zipfian — skew concentrates load on hot keys (contention)
read_ratio:     1.0               # cache-read only: fraction of ops that are get (rest are put/delete)
payload_bytes:  64
duration:       60s               # steady-state window after warm-up
warmup:         10s
subscribers:    0                 # streams per key: cache watch-fanout, or followers for leader-observer
cluster_replicas: 3
backend_sizing: { pg_pool: 32 }   # backend-specific (pg_pool | redis_topology | redis_maxmemory | ...)
redis_topology: single-node       # single-node | sentinel | cluster — only for backend: redis (see §4)
auth_enforcement: required        # required | disabled — is TokenReview on the hot path
```

The dimensions worth sweeping automatically:

- **backend** × **primitive/workload** — the headline comparison matrix.
- **concurrency** — the saturation sweep; the output is a curve, and the
  interesting number is the *knee* (where p99 turns up while throughput flattens).
- **cluster_replicas** — horizontal-scale check (expect flat past the backend's
  ceiling).
- **key_distribution** — uniform vs zipfian; skew is where cache-CAS and lock
  contention bottlenecks actually appear, so a diagnosis run (§13) wants it. It is
  most meaningful on the **cache-read** workload — hot-key read caching is exactly
  where skew bites.
- **read_ratio** — cache-read only; sweep `1.0 → 0.5 → 0.0` (pure-read → mixed →
  write-heavy) to contrast the read path against `put`/CAS without a new workload.
- **payload_bytes** and **key_cardinality** — second-order, run on demand.

Keeping the config declarative means the run matrix is data, and CI can pick a
representative subset (§12) while a human can sweep exhaustively on demand.

## 10. Metrics collection & reporting

Two independent measurement channels, deliberately:

1. **Client-side (authoritative for the SLI).** The load driver records every
   op's latency into an HdrHistogram and counts successes/errors. This is the
   consumer's-eye number and needs no cluster cooperation.
2. **Server-side (for attribution).** The cluster gear's histograms and counters
   from the [observability contract](./OBSERVABILITY.md) —
   `cluster_cache_op_duration_seconds`, `cluster_lock_op_duration_seconds`,
   `cluster_*_ops_total`, `cluster_provider_errors_total`,
   `cluster_leader_transitions_total`, and the subscription gauges. Because these
   are a *versioned contract*, the collector can depend on the names. In the OoP
   deploy this is **not** a direct per-pod `/metrics` scrape: every OoP gear
   **pushes OTLP** to an `otel-collector` (`values-perf.yaml` wires
   `otlp_grpc → otel-collector:4317`), and the reporter (`cluster-perf-delta`, §5.6)
   scrapes the collector's Prometheus endpoint — so a short-lived collector is
   enough and we need no standing monitoring stack. With N cluster replicas (§5.4)
   the histograms arrive tagged per pod (`service.instance.id`), so the reporter
   query must still **aggregate across every replica** (sum the counters, merge the
   histograms) — reading a single instance measures only a fraction of the server
   work and silently corrupts the `client − server` delta. Two mechanics matter for
   that query: the **per-replica sum** itself, and an **adaptive query step** sized
   from the window length (clamped to a floor, e.g. ≥10s) so a short run and a long
   run both render a readable series.

The **report** joins the two into a single artifact per run — JSON for
machine/regression comparison, plus a rendered Markdown/CSV summary:

- throughput, p50/p95/p99/max (client), error rate;
- the **`client − server` latency delta** (the network/hub/auth tax, §3);
- backend-attributed server latency and error kinds;
- pod CPU/mem for cluster + backend;
- the run config echoed back, so a result is self-describing and reproducible;
- a stable **run id + git SHA + node profile**, appended to a results time series
  so an optimization campaign or a regression hunt can be tracked across releases,
  not just diffed pairwise.

Regression comparison is then a diff of two JSON reports against a threshold
(§12).

## 11. Environment fidelity & repeatability

The single most important disclaimer: **minikube on a developer laptop yields
relative, not absolute, numbers.** Shared CPU, a single-node etcd, the docker
network fabric, and no real disk isolation mean the Postgres/etcd envelopes in
ADR-001 (measured on server-class hardware) will *not* be reproduced in absolute
terms. What the suite gives reliably is: **backend-vs-backend ranking**,
**release-vs-release regression**, and **shape** (does throughput plateau where the
ADR says it should; is the saturation knee where we expect). Treat absolute
ops/sec as directional.

Practices to make runs comparable:

- **Warm-up window** discarded before the measured steady state (JIT-free Rust
  still has connection-pool fill, cache priming, and K8s informer sync to settle).
- **Pinned resources** — explicit CPU/mem requests+limits on cluster and backend
  pods; a run records the node's allocatable so cross-machine results are at least
  annotated.
- **Seeded, fixed key sets** per workload so cardinality is constant run to run.
- **Repeat count** (e.g. 3×) with reported variance; a single number from a noisy
  laptop is a lie.
- **kind as the CI substrate** where minikube's driver quirks bite — notably the
  **macOS docker-driver caveat** already documented for the demo: `minikube ip` is
  not host-routable, so an *external* load driver would need `minikube tunnel` /
  `kubectl port-forward`. Running the driver **in-cluster** (§8) sidesteps this
  entirely and is another reason to prefer option B.
- **Establish the noise floor first.** Before trusting any regression gate, measure
  run-to-run variance on an *unchanged* build. If it exceeds the §12 threshold, the
  gate is a false-positive generator and must be widened (or the substrate
  stabilized). The noise floor is a published number, not an assumption.
- **Prove the driver isn't the bottleneck.** A saturated or under-provisioned load
  driver caps measured throughput and masquerades as a backend ceiling. Give the
  driver pod headroom, watch its own CPU, and confirm a second driver pod raises
  throughput before believing a plateau belongs to the system under test.

## 12. CI cadence & regression gating

Performance runs are **not per-PR** — they are too slow and too noisy to gate a
merge on absolute numbers. Cadence mirrors the L3/L4 nightly tier in
TESTING-STRATEGY §9:

| Trigger | Runs |
|---|---|
| **Nightly** | The representative matrix subset: standalone (harness baseline) + postgres + k8s-native, one workload per primitive, a short concurrency sweep. Publish reports; compare to the previous night. |
| **On-demand / pre-release** | Full sweep — all backends, all workloads, wide concurrency, larger payloads. Owns the "does the release still hit the ADR-001 envelope" question. |
| **Per-PR (optional, cheap)** | Only the standalone smoke via option A against a single-node kind, as a *trend* line — never a hard gate. |

**Gating philosophy:** gate on **regression against a rolling baseline** (e.g.
p99 regressed >20% or throughput dropped >15% vs the last green nightly) — but only
once the measured noise floor (§11) sits *below* that threshold, or the gate fires
on noise, not regressions. Never gate on an absolute threshold that a busy CI runner
will trip randomly. The ADR-001
confirmation targets (§4) are asserted in the **on-demand pre-release** tier where
the environment can be controlled, not nightly.

**Bootstrapping the baseline.** The gate needs history it does not have on day one.
Seed it with a run of consecutive nightlies (e.g. 5) on an unchanged build, publish
the resulting noise floor (§11) as a number, and arm the gate only once that floor
sits *below* the regression threshold. Until then nightly is record-only — a
regression gate with no trustworthy baseline is a false-positive generator.

**Nightly runs to a wall-clock budget, and the budget picks the subset.** Fix a
nightly ceiling (target < 45 min end to end, substrate bring-up included) and derive
the representative matrix subset *top-down* from that budget rather than hand-picking
scenarios and hoping they fit; the cross-product of §9 is far too large to run whole
nightly. On-demand / pre-release carries no such ceiling.

**Where the confirmation targets can actually be asserted.** On minikube every
ADR-001 target is record-and-flag-drift only (§11) — the hard assertions of §4 need a
pinned, controlled, multi-node substrate that does **not** exist in this repo today.
Treat that substrate as a tracked prerequisite of the pre-release tier, not a tier
that can run now.

## 13. Diagnosis & bottleneck investigation

The sections above measure *whether* a configuration is fast and catch regressions.
This section is the other half of the goal: when a number is worse than the
envelope, **localizing the bottleneck and deciding whether it is ours to fix.**
Validation is pass/fail; diagnosis is a hunt, and it needs finer tools than a single
op-duration number.

### 13.1 Attributing where the time goes

Two coarse levers already exist: the `client − server` delta (§3/§10) splits
network+hub+auth from the last hop, and the Profile 1 vs Profile 3 A/B (§5.3)
isolates the whole OoP transport tax. Both stop at "network vs gear vs backend."
Going finer needs two additions:

- **Nested spans in the gear.** The observability spans (OBSERVABILITY §4) are
  *flat* — `cluster.cache.get` is one span over facade-decode + pool-acquire +
  backend-call + encode. To see *inside* the gear those need child spans (or timing
  fields): pool-wait, backend round trip, (de)serialization. This is a small
  gear/plugin instrumentation change and a prerequisite for any real in-gear
  diagnosis — without it, "the gear is slow" is the deepest answer available.
- **A raw-backend baseline — the true floor.** Profile 1 still runs through the
  SDK + plugin. A third baseline that drives the raw client (`fred` for redis,
  `sqlx` for postgres, `kube` for k8s) with *no cluster abstraction at all* bounds
  the physics. Then the decomposition is clean: `raw-client` is the backend's own
  ceiling (not ours to fix); `Profile 1 − raw-client` is the plugin's overhead
  (ours); `Profile 3 − Profile 1` is the transport's (also ours). That is what
  turns "it's slow" into "slow *here*, and here is who owns it."

### 13.2 Profiling the gear

Run against the cluster-gear pod while the driver holds a steady load; gated to
diagnosis runs (the instrumentation has overhead), not every run:

- **`tokio-console` / `console-subscriber`** — the first stop for an async gear:
  task stalls, poll durations, and resource waits surface lock-held-too-long,
  connection-pool starvation, and accidental blocking-in-async directly.
- **`pprof-rs` / `cargo flamegraph`** — CPU hotspots: protobuf (de)serialization,
  the redis plugin's Lua rendering, k8s CRD JSON, scoping/prefix-scan work.
- **tokio runtime metrics** — worker-thread saturation and injection-queue depth,
  to separate "backend is slow" from "the gear's own runtime is saturated."

The *orchestration* of profile capture mirrors the metrics scrape: a background
collector task **per replica** on a fixed ticker pulls a fixed-duration CPU
profile (`pprof-rs`) plus a heap snapshot from each pod into a per-pod results
directory, torn down with the run's cancellation context.

### 13.3 Backend-side introspection

The gear's own OTLP metrics are the gear's view; a bottleneck is often only visible in the
backend's own instrumentation, which the orchestrator captures around the measured
window:

| Backend | What to capture |
|---|---|
| **Postgres** | `pg_stat_statements` (per-query time), `pg_stat_activity` (pool saturation, wait events), `pg_locks`, `EXPLAIN (ANALYZE, BUFFERS)` on the hot statements, checkpoint/WAL rate, and the `LISTEN/NOTIFY` commit-lock contention under concurrent writers |
| **Redis** | `SLOWLOG`, `INFO commandstats`/`latencystats`, `LATENCY HISTORY`, `redis-cli --latency`; in Cluster mode cross-slot and `MOVED`/`ASK` counts; keyspace-notification cost vs the plugin's own-published watch |
| **K8s** | apiserver `apiserver_request_duration_seconds` and `apiserver_flowcontrol_*` (APF throttling → 429s), etcd `wal_fsync_duration` / `server_slow_apply_total` / db size, Lease PUT rate vs the etcd write ceiling, watch-cache behavior |

### 13.4 The investigation loop

Diagnosis is a forward loop, not a gate: **measure → profile (13.1–13.3 localize) →
hypothesize → change → re-measure with the identical run spec → keep the change iff
it improves the A/B without regressing correctness** (L1–L3 in
[TESTING-STRATEGY](./TESTING-STRATEGY.md) stay green). The "did my optimization help"
A/B is the §12 regression tooling run in reverse — two gear builds through the same
driver, wanting an *improvement*, not merely no regression.

The output of a hunt is a classification, and only one class is an optimization
target:

- **Fixable by us** — plugin overhead over raw-client (13.1), connection-pool
  sizing, (de)serialization cost, and the **gRPC/tonic transport** (one channel per
  process, HTTP/2 `max_concurrent_streams` and flow-control stalls, message-size
  limits, compression). The transport is literally the "network trip" this suite is
  about, and under high concurrency the channel can be the ceiling before the
  backend is — observe h2 stream counts to catch it.
- **Backend-inherent** — etcd fsync latency, Redis single-threaded execution,
  Postgres per-connection cost. Not ours to fix; the finding feeds a *routing /
  deployment recommendation* instead — exactly how ADR-001 arrives at "add Redis
  when you exceed etcd's ~5k-write ceiling."

## 14. Per-plugin performance addendum

The generic matrix (§7/§9) proves the cross-backend baseline; it deliberately
cannot express a backend's *native-path* bottlenecks. Those get a per-plugin
performance addendum — a short section in each plugin's own docs (sibling to the
`TESTING.md` each already ships for correctness) that references *this* document for
the shared method (driver, config, reporting, diagnosis) and adds only its
backend-specific scenarios. This mirrors what
[TESTING-STRATEGY](./TESTING-STRATEGY.md) §5 does for correctness: the shared suite
lives here, the backend specifics live there.

Each addendum also reconciles its measured numbers against the envelope the plugin's
*own* DESIGN already states, so the two never drift:

| Backend | Native-path scenarios beyond the generic matrix | Reconcile against |
|---|---|---|
| **Postgres** | pool-size vs throughput sweep; advisory-lock acquire under ~1000 holders/connection; `LISTEN/NOTIFY` throughput and the global-commit-lock degradation under concurrent writer + notify | pg plugin DESIGN/TESTING; ADR-001 §PostgreSQL |
| **K8s** | Lease PUT rate vs the etcd write ceiling (find the *cliff*, not just the plateau); CRD cache at 100+ entries; APF-throttling onset (429s); informer resync cost; watch-stream fan-out | k8s plugin DESIGN (~10k counter-updates/sec envelope); ADR-001 §K8s |
| **Redis** | single-node vs Sentinel vs Cluster throughput; **Sentinel failover latency** (ops in flight during failover — where the `EventuallyConsistent` declaration must hold); Cluster cross-slot penalty; Lua CAS script cost; own-published watch vs keyspace-notification cost | redis plugin DESIGN (100k–200k ops/sec, ~0.15ms p50); ADR-001 §Redis |

These addenda are where the redis + k8s bottleneck hunt actually lives: the shared
suite gets both backends onto the same axes, and the addendum is where a native path
that underperforms its own envelope becomes a tracked, profile-backed finding rather
than a hunch.

## 15. Phasing

> **Start with the real suite, not the demo's load path.** An earlier draft made
> phase 1 a k6 test against the demo's `/roundtrip` endpoint. That conflates
> reusing the demo's *deployment* (worth doing — we don't rebuild minikube
> bring-up) with reusing its *load path* (not worth doing — `/roundtrip` bundles
> all three primitives per call and buries the gRPC hop under a REST handler, so it
> can never produce the per-primitive number that is the actual deliverable). We
> reuse the deployment and build the real driver from the start.

0. **Prerequisite — deployment substrate. ✅ Done.** The perf preset
   (`deploy/helm/toolkit-platform` + `values-perf.yaml`) stands up a known-good
   `cluster` (×3) + shared-postgres + platform-host/grpc-hub + otel-collector
   deployment on minikube for the driver to point at. This is bring-up, not
   measurement — reused as-is, not rebuilt.
1. **Thin vertical slice of the real suite. ✅ Done.** The first `cluster-perf` deliverable:
   the in-cluster load driver (§8 option B) hitting **one** primitive (cache
   `compare_and_swap`) against **one** backend (postgres, with standalone as the
   zero-backend-cost control), at a fixed concurrency, in-cluster. Output: an
   HdrHistogram of client latency plus one metrics scrape → the first real
   client-vs-server delta (§3/§10). This proves the whole path end to end — driver
   → gRPC → hub (warm auth) → cluster → backend → metrics — and *is* the initial
   design of the actual suite, kept deliberately minimal. It answers the postgres
   question with a per-primitive number, not a bundled round trip. *Verified
   2026-09-16: CAS on postgres p50 4.66 ms / p99 45.5 ms over the wire vs 14 µs /
   127 µs embedded (Profile 1); client−server delta p50 +1.5 ms, p99 +2.0 ms — the
   p99 tail is backend-contention-bound, not transport.*
2. **Generalize the driver. ✅ Done.** All primitives, the workload models (§7), the
   swept config matrix (§9), and the full two-channel reporting (§10). This is the
   bulk of the suite. *Verified live 2026-09-16 (git `113ffda68`, minikube,
   directional §11): all six workloads (compare_and_swap, try_lock, elect,
   watch_fanout, subscriber_leases, config_locks) driven end-to-end over uniform
   and zipfian on **both** postgres and single-node redis, with the Profile-1
   embedded control and the client−server delta captured. Headline confirmations
   were tabulated in §4 (that results table has since been cleared pending a
   re-run, 2026-09-17). Two harness notes surfaced: (a) the orchestrator's
   `report.json`/`timeseries.jsonl` mirror only the flat SLIs plus the leader
   breakdown — the watch/lease/lock_wait breakdowns must be read from the driver's
   own `GET /runs/{id}`; (b) a client-side h2 concurrent-stream ceiling caps every
   workload at ≥~64–128 concurrent streams, so the stable measurement band is
   conc≤32.*
3. **Native K8s backend + its bottleneck hunt.** Add the `k8s` plugin as a backend
   under test; assert the 50-election / 100-CRD-entry confirmation targets and
   observe the etcd write ceiling as a real plateau. This is the first backend where
   the diagnosis layer (§13) and a per-plugin addendum (§14) earn their keep — APF
   throttling, Lease-vs-CRD cost, and the watch fan-out (§7) are all native-path
   findings the generic matrix can't reach.
4. **Redis + its bottleneck hunt** (from `feat/cluster-redis`, once merged — no
   plugin work needed). Add it as a backend value and make the 10k-subscriber-lease
   target assertable. The distinctive work is *topology*: sweep single-node vs
   Sentinel vs Cluster, exercise the `EventuallyConsistent`-cache startup gate for
   lock/leader (§4), and run the §14 addendum scenarios (Sentinel failover latency,
   Cluster cross-slot, Lua CAS cost, own-published watch), profiling per §13 wherever
   a number misses its envelope. *Single-node pass done 2026-09-16 (git `113ffda68`,
   Option A — a values swap on the same OoP image, cluster pods roll; no rebuild):
   the `EventuallyConsistent` startup gate was exercised (cluster bound redis without
   a CrashLoop, `Linearizable` via the `AssertedSingleNode` hint), the 10k-subscriber-lease
   target confirmed (0 expiries at 800/s; 7 501/10 000 lapse below the sustain line),
   the own-published watch delivery latency measured (~91 ms p50, within noise of
   postgres LISTEN/NOTIFY at 16 subs), and CAS shown to absorb zipfian hot-key
   contention far better than postgres — see §4. **Gap surfaced:** the redis native
   lock (try_lock + config_locks) returns `Unsupported { feature }` for the scoped-lock
   path, so redis lock throughput/fairness is not yet measurable. Still Phase-4 work:
   the Sentinel/Cluster topology sweep and the rest of the §14 addendum.*
5. **Perf-under-fault (bridge to L4).** Latency *during* a backend failover /
   partition, composing this suite's measurement with Toxiproxy/DST fault
   injection. Owned jointly with the testing strategy's L4.

## 16. Decisions & open questions

### Resolved (2026-09-03)

| Question | Decision |
|---|---|
| **Where does the suite live?** | A dedicated **`cluster-perf` crate under `gears/system/cluster/`**. Performance testing belongs at the gear level because cluster is the one gear that *requires* OoP deployment — it acts as a proxy for the real backends, so its performance is inseparable from that deployment, and no other gear needs this today. Common tooling can be extracted later *if* the pattern recurs across gears; it is not generalized prematurely now. |
| **Repo-wide standard now, or cluster-first?** *(2026-09-15)* | **Cluster-first.** Build the cluster perf tier as a self-contained `cluster-perf` crate and do **not** extract a repo-wide performance standard or shared tooling yet. Generalize only once a second gear grows its own performance crate and a genuinely common pattern emerges — at which point the existing `criterion` micro-bench convention (`libs/toolkit-db/benches`, `oidc-authn-plugin/benches`, on the workspace `criterion` dep) is the *micro* tier to harmonize against. This doc stays the reference implementation of the *macro* (deployed, over-the-wire) tier in the meantime. |
| **Production-representative absolute numbers?** | **Comparative signal is enough for now** (backend-vs-backend, release-vs-release). But the harness must be built so it can be pushed at a real multi-node k8s cluster *unchanged* if datasheet numbers are ever needed — portable manifests, no minikube-only assumptions baked into the driver or config (this is a hard design constraint, not an aspiration). |
| **Auth on the hot path** | **Enforcement stays ON** — it is unavoidable and is the production-representative posture (§6). The framework caches validated tokens, so steady-state per-op cost is low once warm; the warm-up window (§11) must prime that cache before the measured window. `auth_enforcement: disabled` is retained as a *diagnostic-only* knob to attribute the residual warm cost, never a shipped shape. |
| **Load tool for option A** | **k6 for the MVP.** Deliberately reversible; revisit only if we later want open-model constant-*rate* load instead of closed-model constant-concurrency. |
| **In-cluster vs external load driver** | **In-cluster** — the driver runs as a pod (§8/§11) so k8s DNS resolves, it shares the production network fabric, and it dodges the macOS docker-driver routing caveat. Consistent with the portability constraint above. An external driver measures laptop↔VM latency instead of pod↔pod, so it is not used. |
| **Assert envelopes, or just record?** | **Record-and-flag-drift by default.** Nightly/comparative runs compare against the ADR-001 targets (§4) and flag regressions; they do not hard-fail on absolute numbers. Hard assertions of the confirmation targets run only in the controlled on-demand / pre-release tier (§12), where the environment is pinned. |

### Still open

| Question | Notes / leaning |
|---|---|
| minikube vs kind as the substrate | **Deferred by decision — not load-bearing.** Design the measurement primitives first (load driver, config, reporting), then pick the substrate when the harness is built: kind is likely easier in CI and dodges the macOS driver quirk; minikube is what the demo is proven on. Either is acceptable. |
