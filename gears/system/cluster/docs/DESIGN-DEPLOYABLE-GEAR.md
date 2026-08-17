# Technical Design — Cluster as a Deployable (Out-of-Process) Gear

**Status**: Proposed — design only, no implementation.

**Amends**: [cluster DESIGN.md](./DESIGN.md) §3.15 ("Cluster is an in-process Rust library SDK; it has no deployment topology of its own") and extends [cluster PRD.md](./PRD.md) §3.1 with the platform's OoP deployment profiles.

**Conforms to**: [`docs/arch/toolkit-oop/PRD.md`](../../../../docs/arch/toolkit-oop/PRD.md) (ToolKit Distributed Gears) and its ADRs — chiefly [ADR-0002 REST-first OoP](../../../../docs/arch/toolkit-oop/ADR/0002-cpt-cf-adr-rest-first-oop.md), [ADR-0005 Eventual Readiness](../../../../docs/arch/toolkit-oop/ADR/0005-cpt-cf-adr-eventual-readiness.md), [ADR-0006 Platform-Plane Auth](../../../../docs/arch/toolkit-oop/ADR/0006-cpt-cf-adr-platform-plane-auth.md), [ADR-0008 Two-Plane Auth](../../../../docs/arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md).

**Change driver**: the team decided cluster must be a gear of its own — a deployable process/pod that other gears reach over a network — rather than a library every consumer links and hosts in-process.

<!-- toc -->

- [0. Reading Guide](#0-reading-guide)
- [1. Problem Statement & Goals](#1-problem-statement--goals)
  - [Goals](#goals)
  - [Non-goals](#non-goals)
  - [The primitive set: cache, distributed lock, leader election](#the-primitive-set-cache-distributed-lock-leader-election)
- [2. Alignment with the Platform OoP Model](#2-alignment-with-the-platform-oop-model)
  - [2.0 Framework state as of this writing](#20-framework-state-as-of-this-writing)
  - [2.1 What the platform PRD dictates](#21-what-the-platform-prd-dictates)
  - [2.2 Transport decision — contract-first, gRPC data plane, REST lifecycle plane](#22-transport-decision--contract-first-grpc-data-plane-rest-lifecycle-plane)
    - [2.2.1 Live option — split the transport by primitive](#221-live-option--split-the-transport-by-primitive)
    - [2.2.2 What cluster reuses, and what it adds](#222-what-cluster-reuses-and-what-it-adds)
  - [2.3 Where cluster does not fit the standard mould](#23-where-cluster-does-not-fit-the-standard-mould)
- [3. The Central Architectural Decision — the Remote Backend Seam](#3-the-central-architectural-decision--the-remote-backend-seam)
  - [3.1 Where the boundary goes](#31-where-the-boundary-goes)
  - [3.2 Deployment profiles, one consumer API](#32-deployment-profiles-one-consumer-api)
  - [3.3 Revised layer diagram](#33-revised-layer-diagram)
  - [3.4 Crate layout changes](#34-crate-layout-changes)
  - [3.5 Rejected alternatives](#35-rejected-alternatives)
- [4. Making Cluster a Deployable Gear](#4-making-cluster-a-deployable-gear)
  - [4.1 Current state audit](#41-current-state-audit)
  - [4.2 Capability set changes](#42-capability-set-changes)
  - [4.3 Binary target and OoP bootstrap](#43-binary-target-and-oop-bootstrap)
  - [4.4 Readiness — the four-state model](#44-readiness--the-four-state-model)
  - [4.5 Discovery: name resolution vs. finding the name](#45-discovery-name-resolution-vs-finding-the-name)
  - [4.6 Platform-plane authentication](#46-platform-plane-authentication)
  - [4.7 Startup: eventual readiness vs. loud capability validation](#47-startup-eventual-readiness-vs-loud-capability-validation)
    - [4.7.1 `resolve()` is async, and validates inline whenever it can](#471-resolve-is-async-and-validates-inline-whenever-it-can)
  - [4.8 Shutdown and the reverse-dependency drain rule](#48-shutdown-and-the-reverse-dependency-drain-rule)
  - [4.9 Consumer-side wiring](#49-consumer-side-wiring)
    - [4.9.1 What is wired, and what is resolved per call](#491-what-is-wired-and-what-is-resolved-per-call)
    - [4.9.2 But it must not be hand-written per consumer](#492-but-it-must-not-be-hand-written-per-consumer)
    - [4.9.3 The registration itself](#493-the-registration-itself)
  - [4.10 Deployment artifacts and operator config](#410-deployment-artifacts-and-operator-config)
    - [4.10.1 Schema migrations](#4101-schema-migrations)
  - [4.11 Modification checklist](#411-modification-checklist)
- [5. Runtime Profile Management](#5-runtime-profile-management)
  - [5.1 What must become runtime state](#51-what-must-become-runtime-state)
  - [5.2 The `ProfileRegistry`](#52-the-profileregistry)
  - [5.3 Backend instance sharing — the N-connections problem](#53-backend-instance-sharing--the-n-connections-problem)
  - [5.4 Per-client profile binding and session tracking](#54-per-client-profile-binding-and-session-tracking)
    - [5.4.1 The watch-subscription sweep (ask `A6`, decision 18)](#541-the-watch-subscription-sweep-ask-a6-decision-18)
  - [5.5 Profile discovery and capability validation over the wire](#55-profile-discovery-and-capability-validation-over-the-wire)
  - [5.6 Dynamic profile reload](#56-dynamic-profile-reload)
  - [5.7 Capacity, isolation and failure modes](#57-capacity-isolation-and-failure-modes)
  - [5.8 Replication — leases live in the store, not in a replica](#58-replication--leases-live-in-the-store-not-in-a-replica)
    - [5.8.1 The model](#581-the-model)
    - [5.8.2 Restarts and upgrades revoke nothing](#582-restarts-and-upgrades-revoke-nothing)
    - [5.8.3 Replica count: the model now, the count after conformance](#583-replica-count-the-model-now-the-count-after-conformance)
- [6. API Surface](#6-api-surface)
  - [6.1 Contract-first, three projections](#61-contract-first-three-projections)
  - [6.2 The contract traits](#62-the-contract-traits)
  - [6.3 Complete facade → wire mapping](#63-complete-facade--wire-mapping)
  - [6.4 Cache contract](#64-cache-contract)
  - [6.5 Lock contract — unary against a store-owned lease](#65-lock-contract--unary-against-a-store-owned-lease)
  - [6.6 Leader-election contract — unary join/resign plus one watch](#66-leader-election-contract--unary-joinresign-plus-one-watch)
  - [6.7 Profile / admin contract](#67-profile--admin-contract)
  - [6.8 Watch stream semantics](#68-watch-stream-semantics)
  - [6.9 Error model over the wire](#69-error-model-over-the-wire)
  - [6.10 Idempotency and retry](#610-idempotency-and-retry)
  - [6.11 Versioning](#611-versioning)
- [7. Cross-Cutting Consequences](#7-cross-cutting-consequences)
  - [7.1 Security](#71-security)
  - [7.2 Performance — the primary consequence](#72-performance--the-primary-consequence)
    - [7.2.1 The hop arithmetic](#721-the-hop-arithmetic)
    - [7.2.2 The cost is concentrated, not spread](#722-the-cost-is-concentrated-not-spread)
    - [7.2.3 The hot pattern, priced](#723-the-hot-pattern-priced)
    - [7.2.4 The binding constraint is pool fan-in, not gRPC](#724-the-binding-constraint-is-pool-fan-in-not-grpc)
    - [7.2.5 Other per-operation costs](#725-other-per-operation-costs)
    - [7.2.6 What gets better](#726-what-gets-better)
    - [7.2.7 Escape hatches, in order of preference](#727-escape-hatches-in-order-of-preference)
    - [7.2.8 The critical-section rule — ADR-002 is amended](#728-the-critical-section-rule--adr-002-is-amended)
  - [7.3 Leadership liveness tracks the consumer, not the transport](#73-leadership-liveness-tracks-the-consumer-not-the-transport)
  - [7.4 Scoping across the boundary](#74-scoping-across-the-boundary)
  - [7.5 Observability](#75-observability)
  - [7.6 Testing strategy](#76-testing-strategy)
- [8. Phasing](#8-phasing)
- [9. Open Questions and Decisions Needed](#9-open-questions-and-decisions-needed)
- [10. Documentation Deltas](#10-documentation-deltas)
- [11. Appendix — A Consumer and the Gear, End to End](#11-appendix--a-consumer-and-the-gear-end-to-end)
  - [11.1 Consumer side](#111-consumer-side)
  - [11.2 Gear side](#112-gear-side)
  - [11.3 Operator config](#113-operator-config)
  - [11.4 One call, end to end](#114-one-call-end-to-end)
  - [11.5 What actually differs between the profiles](#115-what-actually-differs-between-the-profiles)
- [12. Appendix — Implementation Sketch](#12-appendix--implementation-sketch)
  - [12.1 The contract and its DTOs — `cluster-sdk/src/{contract,dto}.rs`](#121-the-contract-and-its-dtos--cluster-sdksrccontractdtors)
  - [12.2 The error codec — `cluster-sdk/src/convert.rs`](#122-the-error-codec--cluster-sdksrcconvertrs)
  - [12.3 Gear: the profile registry — `cluster/src/registry.rs`](#123-gear-the-profile-registry--clustersrcregistryrs)
  - [12.4 Gear: `LocalClusterClient` — `cluster/src/local_client.rs`](#124-gear-localclusterclient--clustersrclocal_clientrs)
  - [12.5 Gear: the session index — `cluster/src/api/grpc/subscriptions.rs`](#125-gear-the-session-index--clustersrcapigrpcsubscriptionsrs)
  - [12.6 Gear: the service impls — `cluster/src/api/grpc/`](#126-gear-the-service-impls--clustersrcapigrpc)
  - [12.7 Gear: the gear — `cluster/src/gear.rs`](#127-gear-the-gear--clustersrcgearrs)
  - [12.8 Gear: the binary — `cluster/src/{main,registered_gears}.rs`](#128-gear-the-binary--clustersrcmainregistered_gearsrs)
  - [12.9 Client: `RemoteClusterClient` — `cluster-sdk/src/client/remote.rs`](#129-client-remoteclusterclient--cluster-sdksrcclientremoters)
  - [12.10 Client: the cache backend — the simple case](#1210-client-the-cache-backend--the-simple-case)
  - [12.11 Client: the lock backend](#1211-client-the-lock-backend)
  - [12.12 Client: leader election — event pump plus resign](#1212-client-leader-election--event-pump-plus-resign)
  - [12.13 Client: the requirement registry and readiness — `cluster-sdk/src/requirements.rs`](#1213-client-the-requirement-registry-and-readiness--cluster-sdksrcrequirementsrs)
  - [12.14 Client: the resolver — `cluster-sdk/src/cache/resolver.rs`](#1214-client-the-resolver--cluster-sdksrccacheresolverrs)
  - [12.15 Client: the registration — `cluster-sdk/src/wiring.rs`](#1215-client-the-registration--cluster-sdksrcwiringrs)
  - [12.16 Consumer: the whole surface, once](#1216-consumer-the-whole-surface-once)
  - [12.17 Constraints the SDK's existing types impose](#1217-constraints-the-sdks-existing-types-impose)

<!-- /toc -->

## 0. Reading Guide

| Question | Section |
|---|---|
| How does this fit the platform's OoP model, and which transport? | §2 |
| What must change so cluster is a deployable gear? | §4 (premise in §3) |
| How are deployment profiles read and managed at runtime, and how does a request route to the right backend connection? | §5 |
| What is the API surface, and how does every facade method map onto it? | §6 |

§2 and §3 come first because two decisions govern everything else: which transport the platform allows, and where the process boundary is cut. §7 collects consequences, including three that change committed contracts (§7.1 last row, §7.2, §7.3).

## 1. Problem Statement & Goals

Today the cluster gear (`cf-gears-cluster`, crate `cluster`) is a `RunnableCapability` that, during `start`, instantiates each profile's backends from operator config and registers them in the **in-process** `ClientHub` under the scope `cluster:{profile}`. Consumer gears resolve `Arc<dyn ClusterCacheBackend>` (and the three siblings) straight out of that same hub. The Postgres pool, the TTL reapers, the leader-renewal loops and the watch fan-out all live in the consumer's own process.

That model cannot serve Profile 3 (K8s Native):

- A gear in its own pod has its own `ClientHub`. Nothing is registered under `cluster:{profile}` there, so every resolve fails with `ProfileNotBound`.
- Every OoP consumer would need its own Postgres/Redis credentials and its own pool, multiplying infrastructure connections by replica count and pushing credential distribution out to every gear.
- Coordination state that ought to be shared — a renewal loop, a watch subscription, a lock reaper — would be duplicated per consumer process.

### Goals

1. **`cluster` runs as its own pod/process** and serves the three coordination primitives to gears in other processes, within the platform's OoP model (§2).
2. **Consumer code does not change *between deployment profiles*** — one source file compiles and behaves identically whether cluster is in-process or in another pod. The four facades, the typed-profile resolver, capability requirements, scoping, watch-union events and the auto-restart combinator all keep their shapes and semantics. This is the platform's own `cpt-cf-fr-developer-transparency` and `cpt-cf-fr-client-transparency` applied to cluster.

   One **one-time SDK change** is required to get there, and it is deliberate: `resolve()` becomes `async` (§4.7.1). A remote resolution must be able to fetch a profile descriptor before it can validate declared capabilities, and a sync signature cannot. The migration is `resolve()` → `resolve().await` at each call site, inside `init`/`start`, which are already `async fn` (`libs/toolkit/src/contracts.rs:38,162`). All 73 current call sites are inside the cluster tree — tests, examples and SDK internals — with none in any consuming gear, so this is the cheapest it will ever be. Profile *transparency* is the invariant worth protecting here; never touching the SDK signature is not.
3. **Profile 1 (Embedded) stays fully supported.** Cluster PRD §1.3 requires zero-infrastructure dev/test; platform PRD Profile 1 requires the same code to work in-process.
4. **Backend connections are owned once, by the cluster gear**, and shared across profiles pointing at the same infrastructure.
5. **Profiles are runtime state**, queryable over the wire, so a client can be told what it is bound to and capability validation still fails loudly.
6. **Internal-only.** Cluster has no externally-visible API. Under `cpt-cf-fr-api-visibility` — as rewritten by [PR #4403](https://github.com/constructorfabric/gears-rust/pull/4403) — visibility and authentication are **orthogonal axes**: cluster's routes stay at the default visibility (`is_public = false`, i.e. no `.exposed()`) *and* are `.authenticated()`. `OperationBuilder::public()`, which conflated the two, is removed. Nothing is registered with the gateway (§6.7).
7. **Stay inside the platform's latency budget.** Remoting adds a hop to every operation, which is the single largest consequence of this change (§7.2). The hot pattern — OAGW's read-modify-write under contention — roughly doubles, to ~2.5 ms against a 5 ms localhost / 10 ms intra-cluster budget (`cpt-cf-nfr-oop-latency`). **That cost is accepted rather than engineered away**: the API is not coarsened in advance of evidence, and the compound-operation lever stays available if production shows it is needed (§7.2.3, §7.2.7).

### Non-goals

- **Running** multiple cluster replicas in the first deployment. The design is multi-replica-*ready* — leases live in the backing store, so no replica is special (§5.8) — but the shipped Helm default stays at one until the failover suite passes (§5.8.3, §7.6).
- Backend credential management — still the platform OoP credential design's call; this design narrows *where* credentials are needed to one deployment target (§5.3).
- Per-primitive native routing (`cpt-cf-clst-fr-routing-per-primitive`) — orthogonal, still deferred.
- Changes to the Postgres/standalone plugins — they sit behind the same provider traits, now only instantiated in the cluster gear's process.
- **Discovery** — not a cluster primitive; it belongs to `DirectoryService` (see below).

### The primitive set: cache, distributed lock, leader election

**Cluster serves three primitives: cache, distributed lock and leader election.** Discovery is not one of them, in any profile.

Discovery belongs to `DirectoryService`, which every gear already registers with. ADR-0009 (`cpt-cf-adr-instance-addressable-discovery`) puts instance registration, serving intent and metadata filtering there: serving intent is the `entrypoint` flag, and metadata filtering is `resolve_by_labels`. A consumer that needs to reach a particular instance of another gear resolves it through the directory and dials its advertised endpoint — see §11 for what that looks like in consumer code.

## 2. Alignment with the Platform OoP Model

### 2.0 Framework state as of this writing

**Re-verified against `main` at `28528310` (2026-08-12). The four PRs this design was drafted against have all
merged**, so most of what follows is now shipped behaviour rather than intended behaviour. The tier table below is
kept because two dependencies are still unbuilt, and because the distinction between the platform DESIGN's
*intended* behaviour and the tree's *actual* behaviour still matters wherever they disagree.

| Tier | Meaning | What is in it |
|---|---|---|
| **1 — shipped on `main`** | Verified present; safe to build against today | The probe server, four-state `/readyz` (`runtime/readiness.rs`), background self-registration + presence loop, `deps`-driven readiness gating, `GrpcServiceCapability` + `grpc-hub`, `RestApiCapability::healthcheck`, `toolkit-canonical-errors` incl. the `Problem` OoP round-trip, `toolkit_security::{InternalAuthenticator, InternalCredential, PlatformIdentity, PeerAuthenticated}`, `toolkit_transport_grpc::{InternalAuthInterceptor, attach_internal_token_grpc}`, `GrpcClientConfig`, `DirectoryClient::resolve_grpc_service`. **Plus everything the four merged PRs added** — see the row below. Also still shipped: the OoP bootstrap's **eager DirectoryService connect** (`bootstrap/oop.rs:551-553`), which propagates its failure, so no OoP gear starts without a directory (§4.5, decision 21); and the Postgres per-incarnation liveness beacon from [#4411](https://github.com/constructorfabric/gears-rust/pull/4411) (`plugins/postgres-cluster-plugin/src/lock/beacon.rs`), which **this design retires** (§5.8.2, checklist 10c) |
| **1 — newly landed** | Merged since this design was drafted; the sections that hedged against them are now unconditional | **[#4084](https://github.com/constructorfabric/gears-rust/pull/4084) — MERGED.** `libs/toolkit-contract{,-macros,-macros-tests,-protogen}`; `#[toolkit::contract]` / `#[rest_contract]` / `#[grpc_contract]`; `#[idempotency]`, `#[streaming]`, `ProtoBridge`, `#[derive(ContractError)]` (`toolkit-contract-macros/src/contract_error.rs`); and the consumer-wiring stack — `#[toolkit::consumes]`, `ConsumerRegistration`, `StaticEndpointResolver`, `DirectoryEndpointResolver`, `NullEndpointResolver` (all `libs/toolkit/src/discovery.rs`) with `run_proxy_wiring_phase` (`host_runtime.rs:423`). `resolve_one_dep` / `ResolvedRestEndpoints` are **retired** — the name survives only in `oop_registration_tests.rs`. **[#4403](https://github.com/constructorfabric/gears-rust/pull/4403) — MERGED**: internal-auth configuration (`toolkit-security/src/{authenticator,internal_auth_config,shared_secret}.rs`), the `.exposed()` / `.anonymous()` visibility split, and `ListAllInstances`. **[#4426](https://github.com/constructorfabric/gears-rust/pull/4426) — MERGED**: ADR-0009 instance-addressable discovery |
| **3 — specified but unimplemented** | Present only in platform docs; cluster must not assume it | **A server-side (inbound) gRPC auth path.** Re-verified after #4403 merged: `toolkit-transport-grpc`'s inbound case still "yields a disabled interceptor that attaches nothing" (`internal_auth.rs:131`), `DynInternalAuthenticator` reaches only the HTTP path (`oop_serve.rs`), and `K8sTokenReviewAuthenticator::authenticate()` is still a bare `self.review(token).await` with **no cache** (`toolkit-k8s-auth/src/lib.rs:191-193`) — so ADR-0006's claim that TokenReview is cached is a defect in the ADR (decision 5, §10). `InternalAuthMiddleware` as a server-side middleware still does not exist. **`ApiContractsConfig.remote_grpc_endpoints`** — no Rust type. **DNS-derived endpoint resolution** for a dependency (`cpt-cf-fr-k8s-native`'s `{gear}.{ns}.svc.cluster.local`): the resolver trait exists with three impls, but no DNS impl and no port convention (decision 21) |

> **The proxy-wiring phase, as merged — and one property that changed in cluster's favour.**
>
> | Property | Where | Consequence here |
> |---|---|---|
> | `run_proxy_wiring_phase` replays inventoried `ConsumerRegistration`s and supersedes the old `resolve_one_dep` loop | `host_runtime.rs:423` | The phase, and its local-wins semantics, are what cluster's registration rides (§4.9.2) |
> | **It runs once, before `start`, in both profiles.** One call site, inside the shared `init → proxy-wiring → post-init` sequence (`host_runtime.rs:609-613`); `run_start_phase` is invoked separately afterwards (`:1310`, `:1467`) | `host_runtime.rs` | **The OoP-vs-in-process divergence earlier drafts hedged against is gone.** Inline capability validation is now the *normal* path in Profile 3, not the lucky one; `resolve()`'s self-construction arm remains as defence rather than as the mechanism (§4.7.1, §4.9.3) |
> | Endpoints come from a `DirectoryEndpointResolver` built from the hub's `DirectoryClient`; with none present the phase installs a `NullEndpointResolver`, so remote deps register as unresolved readiness gates and `/readyz` stays 503 | `host_runtime.rs:436-453` | **Deliberate, and endorsed by its owner**: silently skipping wiring would let a misconfigured consumer report Ready, which is worse. The fix is a resolver that resolves, never a phase that skips — so the wiring path stays **directory-required** until a DNS impl lands (§4.5, decision 21). The ADR-0004 static override (`gears.<owner>.config.consumer_wiring.<dep>`, `StaticEndpointResolver`) is the working directory-less answer today |
> | The generated client is **REST-only** — `<Contract>RestResolvingClient`, feature `contract-directory-rest-client`, and no gRPC variant is planned | `toolkit-contract-macros/src/consumes.rs:155-158` | Cluster is platform-plane and therefore gRPC, so it submits its own `ConsumerRegistration` by hand. The type is transport-agnostic, so that registration needs no rework if a generated gRPC client ever lands (§4.9.2, §4.9.3) |

Two consequences worth stating up front rather than discovering in §6: remote-capable contracts **must** carry a
security-plane context parameter on every method (§6.2 — enforced at parse time by the REST projection, stated but
apparently unenforced by the gRPC one), and platform-plane REST projections are **rejected outright** (§2.2.1). Both
narrow two choices §6 would otherwise leave open. Neither reaches the three consumer facades (§6.2).

### 2.1 What the platform PRD dictates

| Platform requirement | Consequence for cluster |
|---|---|
| `cpt-cf-fr-rest-primary` / ADR-0002 — REST is the primary OoP protocol; each OoP gear runs its own Axum server. **"gRPC remains available as an opt-in for performance-critical internal paths"** | Cluster runs the `oop_http` Axum server for lifecycle/probes/admin, and opts into gRPC for the coordination data plane (§2.2) |
| `cpt-cf-fr-client-transparency` — ClientHub returns an in-process impl or a generated remote client; callers use the same SDK trait | Exactly the remote-backend seam (§3.1). The platform mandates this shape; cluster does not invent it |
| `#[toolkit::contract]` → `#[rest_contract]` / `#[grpc_contract]` projections, proto generated by `toolkit-contract-protogen` with `proto.lock.toml` | Cluster must **not** hand-write a `.proto`. The contract traits are the source of truth (§6.1) |
| ADR-0005 / `cpt-cf-fr-eventual-readiness` — start immediately, self-register with retry in the background, gate `/readyz`; gear devs write no retry loops | Rewrites the startup story: consumers do **not** fail startup when cluster is unreachable (§4.7) |
| ADR-0008 — tenant plane = `Authorization: Bearer <jwt>` re-validated per hop; platform plane = `PlatformSecurityContext`; **`x-secctx-bin` MUST NOT be used over HTTP** | Cluster coordination is non-tenant-scoped platform work ⇒ platform plane (§4.6), with one genuine ambiguity flagged in §7.1 |
| `cpt-cf-fr-k8s-native` — k8s DNS discovery; DirectoryService MUST NOT be required in k8s | Endpoint resolution is DNS-first, DirectoryService-optional (§4.5) |
| `cpt-cf-nfr-oop-latency` — p95 < 5 ms localhost, < 10 ms intra-cluster | Directly constrains the hot cache path (§7.2) |
| `cpt-cf-fr-direct-communication` — caller → target directly, never via the gateway | Cluster is reached directly; no gateway routes |
| Helm library chart with conditional `grpc.enabled` / `grpc.port` | The deployment shape for a gRPC-serving gear already exists; cluster sets `grpc.enabled: true` |
| PRD §4.2 — *"Streaming / SSE over OoP boundaries (future work)"* is out of scope | **Conflict.** Cluster's watches and handle sessions are inherently streaming; see §2.3 |

### 2.2 Transport decision — contract-first, gRPC data plane, REST lifecycle plane

Cluster needs **both** planes, and they are not alternatives:

| Plane | Transport | Contents | Why |
|---|---|---|---|
| **Lifecycle / operability** | REST via `oop_http` (Axum) | `/healthz`, `/readyz`, `/health`, `/openapi.json`, self-registration, drain, `InternalAuthMiddleware`, admin endpoints (profiles, sessions) | Non-negotiable: ADR-0005's probe model, ADR-0006's internal auth, and the Helm probe wiring all live on the HTTP server. Also gives operators `curl`-debuggable diagnostics |
| **Coordination data plane** | gRPC, co-hosted via `grpc-hub` | The three primitives | ADR-0002 explicitly sanctions gRPC "for performance-critical internal paths (explicitly opted in per gear)". Cluster qualifies on three independent grounds below |

Why the data plane is the opt-in case rather than plain REST — and, equally important, **why this is a narrower argument than it first appears**:

1. **Throughput and payload shape.** OAGW's requirement is 10k+ counter updates/second (cluster PRD §2.2) under a 5 ms p95 budget, against cache values that are `Vec<u8>` by contract. Protobuf carries opaque bytes natively; JSON needs base64 (+33% and an encode/decode pass per operation), and HTTP/2 multiplexing handles 10k concurrent-ish RPC/s on one connection more gracefully than HTTP/1.1 request-per-operation. **This is the argument that survives scrutiny, and it applies to the cache primitive specifically.**
2. **Watch efficiency.** Cache watches and the leader watch are push-shaped. Native server streaming carries many events over one connection; the REST projection (SSE, or long-poll) works but churns a request per batch. A real advantage, not a decisive one.

> **What this argument is *not*.** It is not a claim that cluster's shape *requires* gRPC. Leader election and locks are ordinary request/response operations against a store-owned lease (§6.5, §6.6): `try_lock` / `lock` / `renew` / `release` / `join` / `resign` are all unary, and client-death is handled by the TTL safety net the contract already specifies rather than by observing a stream close. **Nothing in cluster requires bidirectional streaming**; the only push-shaped operations are the two cache watches and the leader watch, for which SSE — the platform's sanctioned REST projection for `#[streaming]` — is a perfect fit.
>
> So **a REST-only cluster API is structurally viable**, and the gRPC case rests on argument 1 (and weakly 2). What constrains the choice in practice is tooling, not structure: see §2.2.1, where option A is settled.

The lifecycle plane is what keeps either choice compliant rather than a bypass: cluster is a normal OoP gear by ADR-0005/0006/0008, and gRPC is an additional, declared internal port — precisely the shape `grpc.enabled` exists for.

#### 2.2.1 Live option — split the transport by primitive

Because only the cache primitive carries the throughput argument, a narrower split is available and arguably better aligned with the platform default:

| Option | Coordination transport | Assessment |
|---|---|---|
| **A** (as documented below) | All three primitives + profile discovery over gRPC | One transport, one client, one codegen path. Larger non-standard surface than strictly justified. **The only option the contract codegen currently supports for cluster** — see the constraint below |
| **B** | **Cache** over gRPC (throughput + binary values + watch streaming); **lock / leader-election** over REST with `#[rest_contract]` codegen, SSE for the leader watch | Would minimise the non-standard surface to exactly the part that earns it. Cost: two transports in the client, the leader watch on SSE — **plus a hand-written REST client for the two platform-plane contracts**, since the codegen refuses to generate one |
| **C** | Everything over REST + SSE | Maximum platform alignment on paper. Cost: base64 on every cache value, HTTP/1.1 request-per-op on the hot path, **and a fully hand-written client** for the same reason as B |

> **Constraint that narrows this choice (verified against PR #4084, tier 2).** `#[toolkit::rest_contract]`
> **rejects platform-plane contracts at compile time**. The macro's own diagnostic
> (`libs/toolkit-contract-macros-tests/tests/ui/fail/rest_platform_secctx_rejected.stderr`) reads: *"platform-plane
> REST projections are not supported yet: the generated client cannot source the internal token, so it would emit an
> unauthenticated request. Serve this contract over gRPC (`#[toolkit::grpc_contract]` + `InternalAuthInterceptor`) or
> write a manual client."* Cluster is platform-plane by §4.6, so **B and C are not "REST with generated clients"** —
> they are "REST with a hand-written client", which forfeits the codegen alignment that was their entire argument.
>
> **Decision: option A — all four services over gRPC.** B and C are not "REST with generated clients" but "REST with a
> hand-written client", which forfeits the codegen alignment that was their whole argument, and neither buys a
> semantic property A lacks. If the platform later lifts the platform-plane REST restriction, nothing here has to be
> redone: the contract traits are the source of truth and a REST projection would be additive.

The primitives that would move to REST under B are all cold (§7.2.2: leader renewal every ~10 s, locks hot only in OAGW's pattern), so the *semantic* cost of B was never the issue — the cost is tooling. The rest of this document describes A; the sections that would change under B or C are §6.4–6.6 and §6.7, and they are projection only, not semantics.

#### 2.2.2 What cluster reuses, and what it adds

Cluster owns as little transport, config and error machinery as possible. Everything below is either delegated to an existing platform mechanism or justified as a genuine gap.

**Delegated to the platform** — cluster defines no equivalent of its own:

| Concern | Owner |
|---|---|
| Client transport selection (embedded vs. remote) | The deployment profile + ADR-0005's dep-resolution loop — §4.9.2 |
| Which profiles a consumer binds | The typed `ClusterProfile` marker, per `cpt-cf-clst-fr-validation-typed-profile` — §4.9.2 |
| Endpoint discovery | Framework dep resolution, k8s DNS, DirectoryService, `ApiContractsConfig.remote_grpc_endpoints` — §4.5 |
| Connect / RPC timeouts | `GrpcClientConfig` defaults |
| Per-RPC retry policy | `#[idempotency(..)]` on the contract IR — §6.10 |
| `.proto` generation and field-number stability | `toolkit-contract-protogen` + `proto.lock.toml` — §6.1 |
| Cross-process error model | `CanonicalError` + `#[derive(ContractError)]`; `ClusterError` stays the frozen Rust-facing contract — §6.9 |
| Client-side spans, trace propagation and RED metrics | PR #4084's `PolicyStack` / `PolicyContext` and its `otel` feature once it lands; a `tracing` span in the tonic interceptor until then — §6.1, §7.5 |
| Client-death cleanup for locks and elections | The contract's TTL safety net (`cpt-cf-clst-fr-lock-release`) — §6.5–6.6 |
| Leader-liveness signalling | The client's own watch poll — §7.3 |

**Added by cluster** — each fills a gap no platform mechanism covers:

| Mechanism | Why nothing existing fits |
|---|---|
| `BackendInstanceCache` (§5.3) | `toolkit-db`'s `DbManager` caches handles **per gear name**, not per DSN (`libs/toolkit-db/src/manager.rs:16-25`). Cluster needs N instances per process keyed by connection identity |
| `ProfileRegistry` (§5.2) | `ClientHub` is a type-keyed map; it cannot enumerate profiles or describe their bindings. `arc-swap` (already a workspace dep) supplies the snapshot mechanism |
| `probe()` on the cache backend (§4.4) | The framework's `Healthcheck` trait defines *reporting*; nothing can ping a backend through the `ClusterCacheBackend` abstraction without the trait exposing it |
| Server-side session index (§5.4) | No framework analogue — watch-subscription tracking and lease diagnostics are cluster's own domain |
| Profile inventory (§4.9.2) | Uses `inventory`, the same mechanism `GearRegistry::discover_and_build()`, GTS registration and PR #4084's `ConsumerRegistration` use — adoption of an existing pattern for a new purpose |
| Cluster-owned migration orchestration (§4.10.1) | The framework's DB migration phase is tied to the `db` capability and assumes one database per gear; cluster's DDL is owned by *plugins* across N distinct DSNs, which no framework hook knows about |

The Postgres plugin's `connection_string` and pool settings likewise stay plugin-owned rather than moving to `toolkit-db`'s `DbConnConfig` / `PoolCfg`: the plugin uses `sqlx` directly rather than the `Db` SecureORM entrypoint, and `DbManager`'s per-gear handle model does not fit N non-tenant-scoped DSNs. Reuse would buy field-name consistency and nothing else, and the credential story is an open question in the cluster DESIGN regardless (`secret_ref`, deferred to the platform OoP credential design).

### 2.3 Where cluster does not fit the standard mould

Three divergences, each needing a platform-level decision (§9):

- **Streaming over OoP is listed out of scope in the platform PRD §4.2**, yet the platform DESIGN specifies SSE handling for `#[streaming]` contract methods and `#[grpc_contract]` supports native streaming. The platform docs are internally inconsistent here, and **the inconsistency is resolved in favour of the DESIGN: streaming is supported — SSE for REST projections, native for gRPC — with cluster's watches as the first consumer.** The dependency was narrow either way. Only the cache watches and the leader watch are push-shaped; locks and leader election are unary against a TTL-bounded server-side lease (§6.5–6.6). Even those have a non-streaming projection (long-poll, §6.8), so cluster could ship without any streaming at some efficiency cost. The platform PRD §4.2 needs the corresponding edit (§10); until it lands, cluster's watches are the reference case for the behaviour the DESIGN already specifies.
- **`toolkit-contract` / `toolkit-contract-macros` / `toolkit-contract-protogen` are shipped.** [PR #4084](https://github.com/constructorfabric/gears-rust/pull/4084) **merged** all three to the workspace (`libs/toolkit-contract{,-macros,-macros-tests,-protogen}`) together with `#[toolkit::contract]`, both projections, `#[idempotency]`, `#[streaming]`, `proto.lock.toml`, `ProtoBridge`, `#[derive(ContractError)]`, and the `#[toolkit::consumes]` consumer-wiring path, exercised end to end by the `examples/toolkit/api-contracts` payment example. There is therefore **no exposure left here**: cluster builds on the shipped macros, and the hand-rolled contingency earlier drafts carried is withdrawn (§6.1). Two of its constraints already bind cluster's design — the mandatory security-context parameter (§6.2) and the platform-plane REST rejection (§2.2.1).
- **Cluster is a dependency of nearly every gear**, so it inverts the usual drain order (§4.8) and makes its own readiness a fleet-wide gate (§4.4).

## 3. The Central Architectural Decision — the Remote Backend Seam

### 3.1 Where the boundary goes

The cluster SDK already has a clean two-layer split (ADR-005): a consumer-facing **facade** (`ClusterCacheV1`) over a plugin-facing **backend trait** (`Arc<dyn ClusterCacheBackend>`). Everything consumer-visible — resolver, capability validation, `scoped()`, polyfill, `RestartingWatch` — is implemented *above* the backend trait, in terms of it.

**Decision (proposed ADR-011, `cpt-cf-clst-adr-remote-backend-seam`): the process boundary is cut exactly at the three backend traits.** The remote client is not a new kind of facade; it is four ordinary backend implementations that satisfy their trait by making a remote call.

**One object is registered in the hub, under one trait, and it is a factory for those backends.** This is the platform's ubiquitous consumption shape — `hub.get::<dyn SomeClient>()`, one trait object per dependency, the local impl winning when the provider is co-located — applied to cluster rather than reinvented for it. Every gear in the tree consumes this way (`types-registry/src/gear.rs:148`, `authn-resolver/src/gear.rs:64`, `resource-group/src/gear.rs:132`, and a dozen more), and PR #4084's consumer wiring generalises it to remote providers with an explicit local-wins short-circuit (`consumes.rs:175-190`).

```rust
// cluster-sdk — the TRAIT is unfeatured, because Profile 1 needs it too.
// Only the remote impl sits behind `grpc-client` (§3.4).

/// The one cluster object per process, registered under `dyn ClusterClient`.
/// A factory for the three backend traits, not a facade and not a transport
/// detail: it answers "give me the backend for this profile" and nothing else.
///
/// Profile 1: `LocalClusterClient`, registered by the cluster gear (§11.2),
/// dispatching through the `ProfileRegistry` (§5.2).
/// Profile 3: `RemoteClusterClient`, registered by the SDK's consumer
/// registration (§4.9.3), holding the gRPC channel and the descriptor cache.
/// Local wins when both could apply.
#[async_trait]
pub trait ClusterClient: Send + Sync {
    fn cache_backend(&self, profile: &str) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError>;
    fn lock_backend(&self, profile: &str) -> Result<Arc<dyn DistributedLockBackend>, ClusterError>;
    fn leader_election_backend(&self, profile: &str)
        -> Result<Arc<dyn LeaderElectionBackend>, ClusterError>;

    /// The only async member: the descriptor needs I/O when remote (§5.5).
    async fn descriptor(&self, profile: &str) -> Result<ProfileDescriptor, ClusterError>;
}
```

**The factory methods are sync and pure in both profiles**, which is what keeps `resolve()`'s only await the descriptor. Locally they are a `ProfileRegistry` snapshot read returning the *real* backend — no wrapper, no indirection on the hot path, so Profile 1 keeps today's exact cost. Remotely they construct a `Remote*Backend`, which is an `Arc` clone plus an interned profile name:

```rust
// cluster-sdk, behind `grpc-client`
pub(crate) struct RemoteCacheBackend {
    client: Arc<RemoteClusterClient>,
    profile: Arc<str>,
    descriptors: Arc<DescriptorCache>,
}

#[async_trait]
impl ClusterCacheBackend for RemoteCacheBackend {
    fn consistency(&self) -> CacheConsistency { self.descriptors.cache_of(&self.profile).consistency }
    fn features(&self) -> CacheFeatures { self.descriptors.cache_of(&self.profile).features }
    fn provider_name(&self) -> &'static str { self.descriptors.cache_of(&self.profile).provider }
    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        // `profile` rides on the request; the gear routes it (§5.2).
        self.client.cache_get(&self.profile, key).await
    }
    // …
}
```

**The profile is a request parameter, not a wiring parameter.** Every RPC carries it, and the cluster gear resolves it to a bound backend on arrival (§5.2). The client never learns which provider serves a profile — that knowledge stays entirely server-side, which is what makes plugin linkage a cluster-gear concern only (§3.3). Three consequences shape the rest of this design:

- **Nothing profile-specific is wired.** The process registers **one** `Arc<dyn ClusterClient>`, which is the framework's ordinary one-dep-one-object shape. Per-profile backends are derived from it (§4.9.3).
- **Construction needs no round trip**, in either profile. The only thing `resolve()` may wait on is the profile *descriptor*, needed to validate declared capabilities — and it waits on a bounded timeout, never on cluster becoming reachable (§4.7.1).
- **The facade binds lazily, so wiring order stops mattering.** A resolved facade holds `Arc<ClientHub>`, its profile, and a `OnceLock` for the backend rather than the backend itself. `resolve()` fills that slot eagerly whenever the client is already in the hub — which is always in Profile 1 and normally in Profile 3 — and otherwise the first call fills it. Steady state is one atomic load; a consumer that resolved before the client was registered simply binds on first use instead of failing. This is what PR #4084's example achieves by resolving from the hub per call (`api-contracts-consumer/src/domain.rs`), with the per-call lookup paid once rather than on every operation.

Consumer-facing code is unchanged either way:

```rust
// No consumer code at all: the framework's dependency-resolution loop invokes the
// framework's proxy-wiring phase when the `cluster` dep resolves (§4.9).
// The consumer writes only its typed profile marker - no `deps = [cluster]` (4.9.2).

// identical in Profile 1 and Profile 3 — `.await` is the one SDK-level change,
// and both `init` and `start` are already async (Goal 2):
let cache = ClusterCacheV1::resolver(&hub)
    .profile(EventBrokerProfile)
    .require(CacheCapability::Linearizable)
    .resolve()
    .await?;
```

This is not a cluster-specific trick — it is `cpt-cf-fr-client-transparency` ("ClientHub MUST return an in-process implementation when available and a remote client when the target is remote"), in exactly the form the rest of the platform already uses it: one `Arc<dyn _>` per dep in the hub, local impl preferred. Cluster's only variation is that its single trait object is a *factory* for four backend traits rather than being the consumer-facing API itself — because cluster's consumer-facing API is four facades over a profile matrix, not one client. **Who performs the registration** is a separate question from where the boundary is, and it belongs to the framework's dependency-resolution loop rather than to consumer code — see §4.9.

Everything Goal 2 promises follows, because nothing above the seam knows the difference:

| Consumer-visible mechanism | Why it keeps working unchanged |
|---|---|
| Typed profile + fluent resolver | Looks up `dyn ClusterClient` in the hub and asks it for this profile's backend — the same call in both profiles, differing only in which impl is registered. Same builder, same typed marker, now `async` |
| Capability validation | Same `validate_*_capabilities` call, same `CapabilityNotMet` error naming the server-side provider, normally returned from `resolve()` in **both** profiles. Only when cluster is unreachable within the resolve timeout does enforcement fall back to readiness gating (§4.7.1, §5.5) |
| `scoped()` | `Scoped*Backend` wraps *any* backend; prefix translation stays client-side (§7.4) |
| `*WatchEvent` union, `Lagged`/`Reset`/`Closed` | Stream messages map 1:1 onto the existing variants (§6.8) |
| `RestartingWatch` / `auto_restart` | Operates on `*Watch`; retryability read from `ProviderErrorKind`, which survives the wire (§6.9) |
| `LockGuard`, `LeaderWatch` | Already channel-based (`LockCommandReceiver`, `ResignReceiver`). The remote backend constructs them with `*::channel(..)` and pumps the channel into a stream (§6.5–6.6) |
| `PollingPrefixWatch` | Works over the remote `scan_prefix`; still opt-in, cost now in network round trips (§7.2) |

Those pre-existing command-channel seams are why this is cheap rather than a rewrite: `LockGuard::channel(name, buffer) -> (LockCommandReceiver, LockGuard)` was built so a backend could service `renew`/`release` asynchronously. A remote backend is just another such servicer.

### 3.2 Deployment profiles, one consumer API

Using the platform's profile vocabulary rather than inventing terms:

| Platform profile | Who owns backends | Consumer wiring |
|---|---|---|
| **Profile 1 — Embedded** | The consumer's own process (cluster gear or `ClusterWiring` linked in) | Cluster gear's `init` claims `dyn ClusterClient` with a `LocalClusterClient`; its `start` registers the real backends and publishes the profile set. Ordering comes from cluster's `system` tier, not from a `deps` edge (4.9.2) |
| **Profile 2 — Host + Workers** (P2) | The Platform Host or a dedicated worker process | **Not designed — out of scope for the first deployable version (§8 phase 5, decision 12).** Single-host P2 is "Profile 3 over UDS/localhost" only for the *transport*; multi-host P2 has neither UDS nor k8s DNS, no mechanism exists to resolve the endpoint (§4.5), and the topology fork below is unanswered |
| **Profile 3 — K8s Native** | The cluster pod | The framework's proxy-wiring phase replays cluster's `ConsumerRegistration`, which registers a `RemoteClusterClient` under the same `dyn ClusterClient` — unless a local impl is already there, in which case local wins. Per-profile backends are derived from it at `resolve()`, and the profile rides on each request (§3.1, §4.9.3). Endpoint via k8s DNS (§4.5). No consumer code (§4.9) |

> **The Profile 2 topology fork, stated so it is not decided by accident (§9 decision 12).** Two shapes are
> possible and this document endorses neither yet:
>
> | Shape | What it costs |
> |---|---|
> | **One cluster process per deployment** | Every coordination op crosses hosts, and it is a single point of failure sitting on one host with no scheduler to restart it |
> | **One cluster process per host** | Leases become **per-host**: a lock held on host A is invisible on host B, so locks and elections silently stop being deployment-wide. This is a different product, not a deployment variant |
>
> Nothing in the current design forbids the second, which is exactly the problem — it fails silently and looks like
> a scaling choice. When P2 is designed, the shape must be picked explicitly **and** enforced by a mechanism
> explicitly, because leases being store-owned (§5.8) makes *replica count* free but does not make *deployment
> scope* free: one cluster process per host still means each host's consumers coordinate against whatever backends
> that process is pointed at. The fix is that all P2 workers share one backend configuration, not that they share
> one process.

**There is no cluster-specific transport configuration**, and **no mode flag anywhere**. Which impl a process gets is decided by which one is registered, and that is decided by what the binary links — not by config, and not by a probe. The local-wins check is the whole of the decision logic.

> **Tier check (§2.0).** Wiring a client on dep resolution is **shipped behaviour** as of #4084: `run_proxy_wiring_phase` (`host_runtime.rs:423`) replays inventoried `ConsumerRegistration`s and has superseded the old `resolve_one_dep` loop. It runs once, before `start`, in both profiles (`:609-613`), so a consumer resolving in its own `start` finds the client already in the hub — and cluster's `resolve()` still self-constructs if it does not (§4.7.1).

So the consumer's entire cluster-facing surface is:

```rust
#[toolkit::gear(name = "event-broker", capabilities = [..])]        // no `deps = [cluster]` - see 4.9.2
```

plus the typed `ClusterProfile` marker it already declares. **No mode flag, no endpoint, no profile list, no timeouts** — cluster defines no client-side configuration block at all, because every field such a block would carry is already owned by the framework or the platform transport layer (§2.2.2, §4.9.2).

**The crate graph carries the profile, not the source.** The consuming gear's *source* contains no `cfg`, no flag and nothing profile-specific to get wrong; what varies is a Cargo feature. A Profile 1 monolith leaves it off and links `cluster` + plugins; a Profile 3 image turns it on and links neither.

> **Where the feature is declared, precisely — verified against #4084.** The generated wiring is gated on `#[cfg(feature = "…")]` **evaluated in the consuming gear crate**, not in the SDK and not in the binary (`consumes.rs` module doc). So each consuming gear crate must declare a feature of its own that forwards to both the SDK's client feature and the toolkit's proxy-wiring feature; the binary then enables *that*. Cluster's equivalent is one forwarding feature per consuming gear crate — three lines of manifest, no source change — and it is the price of the mechanism being compile-time rather than config-driven. Feature unification is benign: a build that enables it unnecessarily links a `ConsumerRegistration` whose wire closure short-circuits on the local impl (§4.9.3).

### 3.3 Revised layer diagram

```
┌──────────────────── consumer pod (Profile 3) ──────────────────────────┐
│  Consumer gear (event-broker, OAGW, …)                                 │
│    ClusterCacheV1 / LeaderElectionV1 / DistributedLockV1               │
│      ── unchanged ──   (peers: DirectoryClient, not cluster)           │
│  ────────────────────────────────────────────────────────────────────  │
│  cluster-sdk  (facades, backend traits, resolvers, scoping, restart)   │
│    dyn ClusterClient  ── the ONE hub entry; a factory for the three    │
│                          backend traits. Trait is unfeatured (§3.4)    │
│    facades hold Arc<ClientHub> + profile + OnceLock<backend>           │
│  ────────────────────────────────────────────────────────────────────  │
│  cluster-sdk  [feature: grpc-client]  — same crate, gated (§3.4)       │
│    contract traits + generated client                                  │
│    RemoteClusterClient : ClusterClient  + descriptor cache             │
│    Remote{Cache,LeaderElection,Lock}Backend                            │
│      — per-profile handles, derived at resolve(); profile              │
│        travels on every request and is resolved server-side            │
│    ConsumerRegistration (replayed by the framework's proxy-wiring)     │
└──────────────┬──────────────────────────────────┬──────────────────────┘
               │ gRPC :50051 (internal port,      │ HTTP :8080
               │ platform plane, coordination)    │ (admin/diagnostics)
┌──────────────▼──────────────────────────────────▼──────────────────────┐
│  cluster pod  (cf-gears-cluster: oop_http + grpc-hub in-process)       │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ oop_http (framework): /healthz /readyz /health /openapi.json,    │  │
│  │ InternalAuthMiddleware, self-registration, drain                 │  │
│  ├──────────────────────────────────────────────────────────────────┤  │
│  │ api/grpc (NEW): 4 services; profile→backend dispatch;            │  │
│  │ session index (watch subscriptions + lease diagnostics, §5.4)     │  │
│  ├──────────────────────────────────────────────────────────────────┤  │
│  │ ProfileRegistry (NEW §5.2)  +  ClusterHandle / ClusterWiring     │  │
│  │   also backs LocalClusterClient in Profile 1 (§3.1)              │  │
│  │ SDK defaults: CasBasedLeaderElection / CasBasedLock              │  │
│  ├──────────────────────────────────────────────────────────────────┤  │
│  │ BackendInstanceCache (NEW §5.3) — one provider instance per      │  │
│  │ (provider, canonical options); pools/reapers/listeners kept hot   │  │
│  └──────────────────────────────────────────────────────────────────┘  │
│  Plugins: standalone / postgres / (k8s, redis, nats, etcd …)           │
└──────────────────────────────┬─────────────────────────────────────────┘
                               │  the only place backend credentials live
                    PostgreSQL · Redis · K8s API · NATS · etcd
```

### 3.4 Crate layout changes

**No new crate.** The contract, its projections and the remote client all live in `cluster-sdk`, behind features, following the platform's own reference implementation in [PR #4084](https://github.com/constructorfabric/gears-rust/pull/4084) (`examples/toolkit/api-contracts/api-contracts-sdk`), which puts `contract.rs`, `grpc.rs`, `rest.rs`, `models.rs`, `error.rs`, `proto/`, `proto.lock.toml` and a `build.rs` in the **SDK** crate and gates every transport dependency behind `rest-client` / `grpc-client` / `rest-server`.

```
gears/system/cluster/
  cluster-sdk/            facades, backend traits, resolvers, scoping, restart — plus:
    src/contract.rs            [grpc-client|rest-client] #[toolkit::contract] traits (§6.2)
    src/dto.rs                 [grpc-client|rest-client] serde/schemars DTOs + ProtoBridge
    src/grpc.rs                [grpc-client] #[toolkit::grpc_contract] projection, and the
                               `stubs` module — tonic::include_proto!, carrying BOTH the
                               generated *_client and *_server traits (§6.1)
    src/rest.rs                [rest-client] #[toolkit::rest_contract] projection —
                               ONLY under §2.2.1 option B/C; unused under option A
    src/convert.rs             [grpc-client|rest-client] DTO ⇄ domain, ClusterError codec
    src/client.rs              UNFEATURED  the `ClusterClient` trait (§3.1) — Profile 1
                               needs it, so it must not be gated
    src/client/remote.rs       [grpc-client] RemoteClusterClient : ClusterClient
    src/descriptors.rs         [grpc-client] descriptor cache + requirement registry
    src/backend/{cache,leader,lock}.rs   [grpc-client] Remote*Backend handles
    src/wiring.rs              [grpc-client] the ConsumerRegistration cluster submits
    proto/, proto.lock.toml, build.rs   GENERATED by toolkit-contract-protogen
  cluster/                 the gear — depends on cluster-sdk with `grpc-client` enabled
    src/main.rs             NEW  OoP binary entrypoint
    src/registered_gears.rs NEW  links cluster + grpc_hub
    src/api/grpc/           NEW  hand-written service impls over the SDK's generated
                                 *_server traits — server codegen is out of scope (§6.1)
    src/api/rest/           NEW  admin/diagnostic routes (no primitives)
    src/local_client.rs     NEW  LocalClusterClient : ClusterClient over ProfileRegistry
    src/api/grpc/subscriptions.rs  NEW  watch subscriptions (§5.4); sweep.rs beside it (§5.4.1).
                            No lease index - see §5.4
    src/registry.rs         NEW  ProfileRegistry + BackendInstanceCache
    src/health.rs           NEW  composite readiness healthcheck
    src/{gear,wiring,config,provider,defaults}.rs   amended
  plugins/                 unchanged — depend on cluster-sdk with no feature, so no tonic
```

**`LocalClusterClient` lives in the `cluster` crate, not the SDK**, because it dispatches through the `ProfileRegistry` — which is gear state. That places it exactly where every other gear's local impl lives (`types-registry/src/gear.rs:148` and siblings), and it keeps the SDK free of any dependency on the gear.

Features mirror #4084's split, and `default = []` keeps the SDK core lean:

| Feature | Pulls | Enabled by |
|---|---|---|
| *(none — unfeatured)* | The `ClusterClient` trait and `ProfileDescriptor`. Must stay ungated: Profile 1 resolves through the same trait, and gating it would put a `cfg` back in the resolve path | Always |
| `grpc-client` | `tonic`, `prost`, `tonic-prost-build`, the contract traits, the `stubs` module (**both** `*_client` and `*_server`), `RemoteClusterClient`, `Remote*Backend`, the `ConsumerRegistration` | A consuming gear crate's forwarding feature (§3.2), and the `cluster` gear crate |
| `rest-client` | `toolkit-http`, generated REST client | Only under §2.2.1 option B/C |

**There is no separate server feature.** `tonic-prost-build` emits the client and server traits into one module, so `grpc-client` gates both directions — the same shape as #4084's example, whose `stubs` module is `#[cfg(feature = "grpc-client")]` and supplies the `payment_api_server` trait its gear implements. The cluster gear therefore enables `grpc-client` too; the consumer-side compile cost of carrying unused `*_server` traits is dead code the linker drops, and splitting the feature would mean splitting the generated module, which the codegen does not offer. Plugins enable neither feature and so never compile tonic.

**Why features rather than a separate crate.** `cpt-cf-clst-constraint-no-serde` does not stand in the way, because it does not say what it is sometimes read as saying:

- `cluster-sdk` **already** depends on `serde`, `serde_json` and `schemars` unconditionally, for the `gts_type_schema` plugin-discovery scaffolding.
- The constraint's scope, per every place it is cited (`cluster-sdk/src/lib.rs:3`, `gts.rs:11`, `cluster/src/config.rs:12`), is that the **coordination contract types** stay serde-free — a statement about which types derive `Serialize`, not about the crate's dependency graph. Nothing enforces it mechanically: there is no such lint in `deny.toml`, no test, and no CI check.

So the only dependency genuinely worth gating is `tonic`/`prost`, and a feature does that — exactly as the existing `otel = ["dep:opentelemetry"]` in the same manifest already gates OpenTelemetry.

The decisive argument against putting the remote client in the **`cluster`** crate instead: `#[toolkit::gear]` emits an `inventory::submit!` (`libs/toolkit-macros/src/lib.rs:836`) collected by `inventory::collect!(Registrator)` (`libs/toolkit/src/registry.rs:260`), which is the whole point of the `registered_gears.rs` convention. A consumer linking `cluster` would therefore **register the cluster gear in its own process** — `ClusterGear::init` runs, `start` tries to build backends from whatever `gears.cluster.config` it finds — silently becoming Profile 1 as a side effect of a crate dependency. It would also compile both plugins and `sqlx` (`cluster/Cargo.toml`, `postgres-cluster-plugin/Cargo.toml:84`), reopening the credential-distribution problem §5.3 exists to close.

Consumers therefore depend only on `cluster-sdk`, and never name a `Remote*Backend`: those types are `pub(crate)`/`#[doc(hidden)]`, constructed solely by `RemoteClusterClient`'s factory methods and handed out as `Arc<dyn ClusterCacheBackend>`. Code that names the concrete remote type has already broken the seam (§3.1).

The one real cost: the wire contract now versions with `cluster-sdk` rather than independently (§6.11).

### 3.5 Rejected alternatives

| Alternative | Why rejected |
|---|---|
| **Cut the boundary at the facades** — a `RemoteClusterCacheV1` consumers use instead of `ClusterCacheV1` | Doubles the consumer-facing API, breaks Goal 2 and `cpt-cf-fr-client-transparency`, duplicates resolver/scoping/capability logic per transport |
| **Only the cache primitive goes remote**; consumers keep local SDK-default backends over it | Each consumer process then runs its own renewal loops and CAS spin over the network — more round trips, worse contention, and an extra network hop *inside* the renewal loop. Also splits the "which backend serves this primitive" decision between operator config and consumer code. **Distinct from §2.2.1 option B**, which remotes all four primitives and only varies the transport |
| **Pure REST for the primitives** | **Not rejected — see §2.2.1 option C.** Cluster's shape does not preclude it; what stands against it is throughput, base64 of opaque byte values on the cache path, and the codegen's platform-plane REST restriction. The phase-0a benchmark settles the first two, not this table |
| **Pure gRPC, no HTTP server** | Forfeits ADR-0005 probes, ADR-0006 internal auth, self-registration and drain — the `oop_http` machinery every OoP gear is required to have. Also the shape the `oop_http` config comment calls "the legacy gRPC-only lifecycle" |
| **Hand-written `.proto`** | The platform generates proto from the contract IR with a `proto.lock.toml` for wire-stable field numbers. Hand-writing forks that pipeline (§6.1) |
| **Cluster gear as a standalone tonic server outside the gear framework** | Loses directory registration, config rendering, logging, lifecycle. `gear-orchestrator` is the precedent to follow |

## 4. Making Cluster a Deployable Gear

### 4.1 Current state audit

`gears/system/cluster/cluster/src/gear.rs:24`:

```rust
#[toolkit::gear(name = "cluster", capabilities = [stateful])]
struct ClusterGear { hub, config, handle }
```

`Gear::init` captures the hub and parses `ClusterConfig`; `RunnableCapability::start` calls `ClusterWiring::from_config` and stores the `ClusterHandle`; `stop` runs `handle.stop()` under the framework deadline. `provider_registry()` hard-wires the linked plugins.

Missing for deployability, measured against `gear-orchestrator` (`capabilities = [grpc, system, rest]`) and the `calculator` OoP example:

| Missing | Consequence today |
|---|---|
| No `GrpcServiceCapability` | Nothing is served; no other process can reach the primitives |
| No contract traits / wire surface | — |
| No binary target, no `main.rs`, no `registered_gears.rs` | Cannot be deployed as its own process |
| No `RestApiCapability` | No `healthcheck()` hook ⇒ no readiness signal ⇒ pod takes traffic before backends are up (§4.4) |
| No `oop_http` in the deployment config | No `/healthz`, `/readyz`, `/health`, no drain, no internal auth, no self-registration |
| Profiles written once into the hub, never enumerable | Cannot answer "what is this client bound to"; no runtime management (§5) |
| Backends built per profile unconditionally | Two profiles on one DSN open two pools (§5.3) |
| No `deps` declaration | Nothing for consumers' readiness gating to key on |

### 4.2 Capability set changes

```rust
#[toolkit::gear(name = "cluster", capabilities = [stateful, system, grpc, rest])]
```

| Capability | Trait required | Why |
|---|---|---|
| `stateful` | `RunnableCapability` | Already present. Owns `ClusterHandle`, plus the profile and session registries |
| `grpc` | `GrpcServiceCapability` | The coordination data plane. `get_grpc_services(&ctx)` returns one `RegisterGrpcServiceFn` per service; `grpc-hub` installs them into its tonic `RoutesBuilder` |
| `rest` | `RestApiCapability` | Two jobs: the `healthcheck()` hook (the framework reads readiness *only* from here — `libs/toolkit/src/runtime/readiness.rs`, `host_runtime.rs:477,957`), and the admin/diagnostic routes (§6.7). **No primitive is exposed over REST** |
| `system` | none — it is a flag | Marks cluster platform-tier so it initialises in the system phase ahead of application gears, like `authn-resolver`, `types-registry`, `credstore` |

Server-side registration follows `gear-orchestrator/src/gear.rs:132`:

```rust
#[async_trait]
impl GrpcServiceCapability for ClusterGear {
    async fn get_grpc_services(&self, _ctx: &GearCtx) -> anyhow::Result<Vec<RegisterGrpcServiceFn>> {
        let profiles = self.profiles()?;   // Arc<ProfileRegistry>
        let sessions = self.sessions()?;   // Arc<SessionRegistry>
        Ok(vec![ /* cache, lock, leader, profile services */ ])
    }
}
```

> **Lifecycle constraint (verified).** The framework's phase order is `pre-init → db → init → post-init → REST → gRPC registration → start → OoP spawn → … → stop` (`libs/toolkit/src/runtime/host_runtime.rs:847`). So `get_grpc_services` (phase 6) and `healthcheck()` (collected in phase 5) both run **before** `RunnableCapability::start` (phase 7) — but backends only exist *after* `start`, where `ClusterWiring::from_config` runs.
>
> Neither the service impls nor the healthcheck may therefore capture backends. Both capture `Arc<ProfileRegistry>` — created in `init`, *populated* in `start`. An RPC arriving before `start` completes gets `ProfileNotBound`, and `/readyz` reports `Starting` until the registry is populated (§4.4). This is why the profile registry must be a mutable runtime object, not a snapshot handed to the services at registration time (§5.2).

> **The `grpc` capability makes `grpc-hub` mandatory wherever `cluster` is linked, Profile 1 included (verified).**
> The framework refuses to build a registry that has gRPC services and no hub: `RegistryError::GrpcRequiresHub`,
> `libs/toolkit/src/runtime/host_runtime.rs:506-508`. So this capability change is not confined to the deployable
> case. **Any monolith that links `cluster` must also link `grpc-hub`** in its `registered_gears.rs` and give it a
> `listen_addr`, or fail at startup — and once it does, cluster's four coordination services are served on that
> process's hub port. That is a network surface an embedded cluster never had, and it lands squarely on §7.2.7's
> option 2, the shape recommended for a hot consumer.
>
> Two consequences to carry into the deployment docs: an embedding process needs the same `NetworkPolicy`
> treatment as the cluster pod (§7.1), and its `grpc-hub` must bind a port the operator is willing to expose.
>
> The alternative — gating `GrpcServiceCapability` behind a `serve-grpc` feature so an embedding links the hub but
> serves nothing — is **not obviously expressible**: `capabilities = [..]` is a list inside `#[toolkit::gear]`, and
> whether the macro tolerates a `cfg`-varying capability set (or whether it would need two gear attributes behind
> mutually exclusive features) is unverified. Treat the linkage requirement as the design, and check the macro
> before promising the feature gate.

### 4.3 Binary target and OoP bootstrap

`cluster/Cargo.toml`:

```toml
[lib]
name = "cluster"

[[bin]]
name = "cluster-oop"
path = "src/main.rs"

[dependencies]
cluster-sdk = { workspace = true, features = ["grpc-client"] }   # contract traits, DTOs, *_server stubs
tonic     = { workspace = true, features = ["transport"] }
grpc_hub  = { path = "../../grpc-hub", package = "cf-gears-grpc-hub" }
axum      = { workspace = true }
clap      = { workspace = true, features = ["derive"] }
```

`src/main.rs` mirrors `examples/oop-gears/calculator/calculator/src/main.rs`: a `clap` CLI (`--config`, `-v`) feeding `OopRunOptions { gear_name: "cluster", .. }` into `toolkit::bootstrap::oop::run_oop_with_options`. `src/registered_gears.rs` links `cluster as _` and `grpc_hub as _`.

With `oop_http` present, the bootstrap supplies (per platform DESIGN §"OoP bootstrap"): the Axum server with probes served **as soon as the listener binds** (before `start`), background self-registration with exponential backoff 100 ms → 30 s, background dependency resolution from `deps`, the presence loop (single writer for registration + heartbeat + self-heal), the drain sequence, and DirectoryService deregistration on shutdown. **None of that is cluster's code to write** — ADR-0005's confirmation step is a code review asserting no gear contains registration or dependency retry loops.

> **Tier check (§2.0) — the auth half is not shipped.** There is no `InternalAuthMiddleware` in the workspace; it
> exists only in toolkit-oop DESIGN and ADR-0006. What `main` has is the *toolkit*: `toolkit_security::{InternalAuthenticator,
> InternalCredential, PlatformIdentity, PeerAuthenticated}`, the client-side `toolkit_transport_grpc::InternalAuthInterceptor`,
> and an `oop_http.internal_auth: Option<InternalAuthConfig>` slot whose Kubernetes `TokenReview` authenticator is
> installed only when the `k8s-auth` feature is on. [PR #4403](https://github.com/constructorfabric/gears-rust/pull/4403)
> fills in the rest (`toolkit-security/src/{internal_auth_config,authenticator,shared_secret}.rs`,
> `toolkit-transport-grpc/src/sa_token.rs`). Cluster should still write none of it — but phase 5 must confirm the
> server-side interceptor exists by then rather than assuming it, and decision 5's caching question is answered
> against #4403's authenticator, not against a middleware that does not exist.

### 4.4 Readiness — the four-state model

Cluster is the coordination dependency of nearly every gear, so its readiness is a fleet-wide gate. ADR-0005 defines four states; cluster's composite healthcheck drives the health dimension:

| State | HTTP | When |
|---|---|---|
| `Starting` | 503 `{"state":"starting","ready":false,"unresolved_deps":[…]}` | Before `start` completes, or while the registry has no profiles though config declares some |
| `Ready` | 200 `{"state":"ready","ready":true}` | Every configured profile has three bound backends and every backend instance's probe passes |
| `Degraded` | 200 `{"state":"degraded","ready":true}` | ≥1 profile healthy, another profile's backend unreachable |
| `Draining` | 503 `{"state":"draining","ready":false}` | Set by the SIGTERM handler before deregistration (§4.8) |

The bodies above are the framework's `ReadinessReport` verbatim (`runtime/readiness.rs`): `state`, a `ready` mirror of `state ∈ {ready, degraded}`, and `unresolved_deps` omitted when empty. Cluster does not shape them — it only supplies the health dimension through `healthcheck()`.

```rust
impl RestApiCapability for ClusterGear {
    fn register_rest(&self, ctx: &GearCtx, router: axum::Router, openapi: &dyn OpenApiRegistry)
        -> anyhow::Result<axum::Router> {
        Ok(crate::api::rest::admin_routes(router, openapi, self.profiles()?))  // no primitives
    }
    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        Some(Arc::new(ClusterReadiness { profiles: self.profiles_weak() }))
    }
}
```

Two points that follow from ADR-0005 and matter here:

- **`Degraded`, not `Unhealthy`, for one bad profile.** Evicting the pod because one DSN is unreachable would take down coordination for *every* profile. The per-component detail lands on `/health`, which is where an operator decides whether to page.
- **DirectoryService registration is explicitly not a readiness signal.** A transient directory failure must not pull a healthy cluster pod out of rotation. Directory visibility is a `/health` component, tracked by metrics and logs.

**`Degraded` is right for the pod and wrong for the consumer, and that gap needs closing here.** The framework's dep gate is **gear-granular**: a consumer's readiness records `cluster` as resolved or unresolved and has no vocabulary for "profile `oagw` is unavailable". So a `Degraded` cluster reports `ready: true`, the dep resolves, an OAGW consumer's `resolve()` returns `Ok` (validation deferred per §4.7.1), and the consumer starts taking traffic against a profile that cannot serve — the precise failure `Degraded` was chosen to *contain* on the server side leaks through to the client side.

Closing it is the SDK's job, not the framework's, because only the SDK knows which profiles this process resolved:

| Piece | Requirement |
|---|---|
| Descriptor payload | `ProfileDescriptor` carries a per-profile health state (`serving` / `degraded`), so a client can distinguish "cluster is up" from "my profile is up" (§5.5, §6.7) |
| SDK readiness contributor | The requirement registry (§4.9.1) already records `(profile, primitive, requirements)`. It reports **not-ready** while any *recorded* profile is non-serving — profiles nobody resolved stay irrelevant, exactly as with capability validation |
| Refresh | The contributor re-reads on `generation` change (§5.6) and on a bounded poll, so a profile that degrades mid-life pulls its consumers out of rotation rather than only its own pod |

This is a per-profile gate built on top of a per-gear one, not a change to the framework's dep model — the framework keeps deciding whether `cluster` resolved, and the SDK decides whether *this consumer's* profiles can serve. Three details make it implementable rather than aspirational:

- **The poll is short and unconditional.** Health moves without a configuration change, so `generation` cannot be the only trigger; the contributor re-reads descriptors every **10 s** (an SDK constant, like the resolve timeout of §4.7.1). One `DescribeProfiles` per process per interval, covering every recorded profile, is not a load the cluster gear notices.
- **This `Unhealthy` is reversible, and must not crash-loop.** §4.7's escalation applies to *permanent* errors only — `ProfileNotBound`, `CapabilityNotMet`, nothing-wired. A degraded profile is a backend outage: the consumer goes 503, stays running, and returns to rotation on the next poll that reports `Serving`. Conflating the two would turn a transient Redis failure into a fleet-wide crash loop, which is the opposite of the intent.
- **Only recorded profiles count.** A profile this process never resolved is not a readiness input, exactly as with capability validation (§4.7.1) — so a degraded `oagw` profile pulls OAGW's pods out of rotation and touches nothing else.

Per-instance liveness needs a cheap non-mutating probe, which `ClusterCacheBackend` lacks. Add a defaulted `async fn probe(&self) -> Result<(), ClusterError>` returning `Ok(())` — additive and dyn-safe, the same shape as ADR-010's `scan_prefix` extension. The Postgres plugin implements it as `SELECT 1` on the pool, which is exactly what "is this profile serving" means. Note the framework caches healthcheck results for 2 s and bounds each check by `oop_http.healthcheck_timeout_ms` (default 500 ms), so the probe must be fast or report `Degraded` on timeout rather than hanging.

### 4.5 Discovery: name resolution vs. finding the name

These are two different problems and conflating them makes the work look far larger than it is. Only the second one is cluster's concern at all.

**Name resolution — hostname to address — is the transport's job, and it is already solved.** Hand tonic `http://cluster.platform.svc.cluster.local:50051` and hyper's connector resolves it through the OS stub resolver on each connection attempt. No resolver code exists in cluster or needs to. Two consequences worth relying on:

- **Construction is pure.** `Endpoint::parse(..)` + `connect_lazy()` returns a `Channel` without touching the network, which is what lets the registration run at any point and await nothing (§4.9.3).
- **Reconnects re-resolve.** A transport failure sends the channel back through the connector, so a cluster pod rescheduled to a new address is picked up with no re-discovery step and no stale-endpoint cache to invalidate. This is strictly better behaviour than resolving once and caching, and it is why DNS-plus-ClusterIP is the robust default rather than merely the convenient one.

**Finding the name is the part that needs deciding**, and it is a string built from convention — not machinery. Ordered:

1. **Explicit override** — `ApiContractsConfig.remote_grpc_endpoints` (or its REST twin), the platform's designated "how do I reach dep X". Covers Profile 2's UDS path and test fixtures. **Not** a cluster-specific config key. *Tier 3 (§2.0): a toolkit-oop DESIGN concept (`DESIGN.md:646`) with no Rust type yet.*
2. **K8s DNS by convention** — `cluster.{namespace}.svc.cluster.local:{grpc.port}`, from the Helm-provisioned Service, with the namespace from `POD_NAMESPACE` (downward API) or config. The default in Profile 3.

   > **The name is derivable; the port is not.** Cluster can write this string because it knows its own gRPC port. A *framework* resolver cannot: the directory returns a full base URI, while DNS returns a name only, so a generic `DnsEndpointResolver` needs a port convention that does not exist today — per-dep config, a platform-wide default, or reading it back from the directory, which defeats the purpose. The resolver is cheap (`EndpointResolver` is already a trait with three impls) and its owner has agreed to add it; **the port convention is the actual open decision, and it belongs in the platform PRD** (decision 21).
3. **DirectoryService** — `resolve_grpc_service(..)`, shipped at `libs/system-sdks/sdks/directory/src/api.rs:82`.

> **What each source is actually for.** (3) is not a DNS substitute; it answers "where is this thing" when the address is **not derivable by convention** — Profile 2's UDS paths and dynamic ports, non-k8s deployments with no cluster DNS, ephemeral test ports, and enumerating instances that a single VIP hides. In k8s none of those apply, which is exactly why `cpt-cf-fr-k8s-native` forbids *requiring* DirectoryService there.

**Who builds the string.** The platform DESIGN assigns this to the framework — `cpt-cf-fr-k8s-native` and the sequence at `DESIGN.md:1338` (`Discovery: k8s DNS ({gear}.{ns}.svc.cluster.local)`) — but no DNS-derived resolution exists in `libs/toolkit` today (tier 3, §2.0); the shipped dep loop calls `resolve_rest_service`, and the only `svc.cluster.local` reference in the toolkit is a doc comment on `oop_serve.rs:188`'s `advertise_uri`, which is the address a gear publishes *for itself*. So cluster's `ConsumerRegistration` derives the endpoint until the framework does (§4.9.3, decision 16).

> **The ordering above is the intended state. Today there are two directory gates, with two different owners, and
> the binding one is not in the wiring phase (§2.0, decision 21).**
>
> 1. **The gate that actually blocks — the OoP bootstrap, on `main` today.** `run_oop_with_options` connects to
>    DirectoryService eagerly and propagates the failure (`bootstrap/oop.rs:542`), so **no OoP gear starts without
>    a directory at all**. This predates #4084 and is owned by the OoP runtime owner, not by #4084's author. A DNS
>    fallback in the wiring phase is unreachable until this is lifted, so **the two have to move together**.
> 2. **The wiring phase — directory-required, and deliberately so.** `run_proxy_wiring_phase` builds a
>    `DirectoryEndpointResolver` from the hub's `DirectoryClient`, and with none present installs a
>    `NullEndpointResolver`, so remote deps register as unresolved readiness gates and `/readyz` stays 503
>    (`host_runtime.rs:436-453`, #4084 branch). Its owner will not weaken this, and is right not to: silently
>    skipping wiring would let a misconfigured consumer report Ready, which is the worse failure. The fix is a
>    resolver that resolves, not a phase that skips.
>
> Between them, a k8s deployment that deliberately runs no DirectoryService — which `cpt-cf-fr-k8s-native`
> explicitly permits ("DirectoryService MUST NOT be required in k8s") — cannot start today. Gate 1 is why; gate 2
> would keep it from becoming ready even once gate 1 is lifted.
>
> **There is a working answer for gate 2 today, with no new code.** The ADR-0004 static override —
> `gears.<owner>.config.consumer_wiring.<dep> = "http://host:port"` (`StaticEndpointResolver`,
> `discovery.rs:118`) — lets a directory-less k8s deployment pin endpoints from Helm values. This document
> previously dismissed it as a dev/test escape hatch; that is wrong, and its owner names it as the **current**
> mechanism for exactly this shape. Two caveats to carry rather than bury: it is `warn!`-level today, which should
> be reconsidered if it is a supported production path, and **it covers the wiring phase only** — it does nothing
> about gate 1, so it is not by itself a directory-less deployment story.
>
> Two things still wanted from the framework, in this order:
>
> 1. **Lift the bootstrap's eager connect** so a directory is optional at startup (OoP runtime owner).
> 2. **A DNS fallback resolver in the phase** — when no `DirectoryClient` is in the hub, resolve
>    `{dep}.{POD_NAMESPACE}.svc.cluster.local:{port}` rather than installing `NullEndpointResolver`. Agreed by
>    #4084's owner, and cheap — but **not merely a `format!`**: it needs a platform-wide port convention that does
>    not exist, which is the real decision and belongs in the platform PRD (see the note on source 2 above).
>
> **Cluster is not blocked on either.** It derives its own endpoint inside its hand-written registration and
> ignores the `endpoint` argument when empty (§4.9.3), and the static override covers a directory-less Profile 3
> in the interim. So §4.9.3's "`endpoint` is whatever the framework resolved" means "whatever DirectoryService or
> the static override resolved", and decision 21 is raised on the platform's behalf.

Framework ownership here buys **convention consistency, not saved code** — the details that diverge when every SDK derives its own address are `http` vs `https`, the port default, where the namespace comes from, and ClusterIP vs headless. That last one **used to be** a correctness constraint for cluster — a headless Service balancing across replicas would have broken lease affinity — and is now a plain preference, because store-owned leases make every replica interchangeable (§5.8). `ClusterIP` stays the default simply because it needs no per-pod DNS.

Cluster still registers with DirectoryService — the bootstrap does it unconditionally — so instance enumeration and platform tooling see it; consumers simply need not depend on that path. PR #4403 extends that registration with a stable `instance_id`, the REST endpoint and the OpenAPI spec, and adds `ListAllInstances` for edge discovery. Cluster gets all of it for free and needs none of it for consumer resolution.

### 4.6 Platform-plane authentication

Per ADR-0008, the plane is chosen by **tenant-scoped vs non-tenant-scoped**, not user vs system. Cluster coordination — cache keys, lock names, election names, service registrations — is non-tenant-scoped platform infrastructure, like DirectoryService and types-registry. So:

| Aspect | Design |
|---|---|
| Plane | **Platform.** Calls carry an `InternalCredential`; the server resolves a `PlatformSecurityContext` (`toolkit-security`). **No tenant AuthZ**, and the `PlatformSecurityContext` is never passed to a tenant `PolicyEnforcer` |
| Phase 1 credential | Projected K8s ServiceAccount token (audience `toolkit-internal`), validated via TokenReview; sets `PeerAuthenticated { name }` for workload policy only. **Scoped to the REST lifecycle/admin plane and other low-rate calls** — see the row below |
| **The gRPC data plane has no viable SA-token phase** | Confirmed, not assumed: `K8sTokenReviewAuthenticator::authenticate()` is a bare `self.review(token).await` with no cache in the crate, so an uncached TokenReview per request is unusable at cluster's rate (§7.2.5). So the data plane gets **per-connection** authentication from the start — mTLS — or it ships behind a `NetworkPolicy` only. There is no intermediate SA-token step for it, which is why the interceptor and pulling ADR-0006's mTLS phase forward are **one work item, not two** (decision 5) |
| Next phase | mTLS + SPIFFE via cert-manager, populating the same `PlatformSecurityContext` — no cluster code change, per ADR-0006. For the data plane this is not "next" but the first workable phase |
| HTTP plane | The bootstrap's internal-auth layer (`oop_http.internal_auth`; the `InternalAuthMiddleware` of ADR-0006, tier 3 per §2.0 — PR #4403 supplies the authenticators). **`x-secctx-bin` MUST NOT be used** (ADR-0008 drops it from the HTTP contract) |
| gRPC plane | Internal credential in gRPC metadata via `toolkit_transport_grpc::attach_internal_token_grpc` / `extract_internal_token_grpc` (both shipped), with `InternalAuthInterceptor` on the client side. **Do not** use `attach_secctx`/`x-secctx-bin`: it is scoped to in-process gRPC metadata in Profile 1, and cluster's gRPC is cross-process |
| Caller identity | `PlatformIdentity` (SA name, later SPIFFE) is the `ClientId` for lease ownership (§5.8.1), session indexing (§5.4) and — once decision 9 lands — the source of the enforced key scope (§7.1) |

**One case looks like an exception and is not: OAGW's per-tenant rate-limit counters.** Under ADR-0008's criterion — "does this operation act within a tenant?" — a per-tenant counter arguably says yes. **Cluster stays platform-plane anyway, and tenant isolation of cluster *data* is the caller's responsibility**, expressed through key scoping (§7.4).

The reasoning is that cluster has no tenant model and should not acquire one: a key is an opaque string, a lock name is an opaque string, and the primitive cannot tell a tenant-derived key from any other. Giving it a tenant model would mean threading a `SecurityContext` through a 10k-ops/s coordination path and running tenant AuthZ per operation — a latency cost §7.2 has no room for, in service of an isolation boundary the caller is better placed to enforce, since only the caller knows which part of its key namespace is tenant-derived.

Two obligations follow, and both belong in the consumer-facing documentation rather than being left implicit:

- **A consumer putting tenant-derived data in cluster must scope its keys**, and `scoped()` is the mechanism (§7.4). Nothing in cluster enforces it today.
- **That makes §7.1's namespacing question sharper, not softer.** `scoped()` is cooperative and client-side; over a network it is not an isolation boundary at all. Server-enforced namespacing derived from the caller's identity is the hardening that would turn the caller's responsibility into a platform guarantee, and it is tracked as decision 9.

The ADR-0008 owner records the ruling (§10).

### 4.7 Startup: eventual readiness vs. loud capability validation

This is where the platform model and the cluster contract genuinely pull against each other.

- Cluster's `cpt-cf-clst-fr-validation-startup-fail` demands that a capability mismatch **fail startup** with an actionable error, never complete with a silently-degraded primitive.
- ADR-0005 demands that gears **start immediately and unconditionally**, resolve dependencies in the background, and express not-readiness through `/readyz` — no blocking startup, no retry loops in gear code.

These are reconcilable once transient and permanent failures are separated:

| Situation | Classification | Behaviour |
|---|---|---|
| Cluster pod not up yet / DNS not resolving / connection refused | **Transient** | Framework's background dep resolution retries with backoff. Consumer's `/readyz` reports `Starting` with `cluster` in `unresolved_deps`. **No startup failure** — this is ADR-0005's whole point |
| Cluster reachable; requested profile not bound (`ProfileNotBound`) | **Permanent config error** | `Err` from `resolve().await` naming the profile and cluster endpoint. If the error is swallowed rather than propagated, the healthcheck reports `Unhealthy` and `/readyz` stays 503, so the pod never takes traffic (§4.7.1) |
| **No `dyn ClusterClient` in the hub at all** — `cluster` not linked, forwarding feature off, or config missing | **Permanent build/config error**, indistinguishable at `resolve()` from a Profile 3 cold start | `resolve()` returns `Ok` and the facade binds lazily, so this row is *not* an `Err`. Enforcement is the readiness contributor: `Unhealthy` once the grace window lapses with requirements recorded and no client registered, and `ProfileNotBound` from any call that arrives first (§4.9.1). This is the one situation where a Profile 1 mistake and a Profile 3 timing condition look identical at the resolve site, which is why readiness rather than the return value is the enforcement point |
| Cluster reachable; profile bound but capability unmet (`CapabilityNotMet`) | **Permanent config error** | Same shape: `Err` naming primitive, unmet capability, and the **server-side provider** — the full diagnostic `cpt-cf-clst-fr-validation-startup-fail` requires — with `Unhealthy` as the backstop |
| Cluster **unreachable** at startup and a requirement later proves unmet | **Permanent, discovered late** | `resolve()` already returned `Ok`; the readiness contributor reports `Unhealthy` with the identical diagnostic once the descriptor lands (§4.7.1) |

So the *guarantee* is preserved in every row — no consumer ever serves traffic against a primitive that fails its declared requirements. What varies is whether the failure arrives as a return value or as a readiness verdict, and §4.7.1 makes that rule explicit.

#### 4.7.1 `resolve()` is async, and validates inline whenever it can

Two independent obstacles stand between a remote resolution and the loud, immediate `CapabilityNotMet` that `cpt-cf-clst-fr-validation-startup-fail` demands. They are separate problems with separate answers, and answering only one of them is what produces a design that looks complete and is not:

| | Obstacle | Answer |
|---|---|---|
| **A** | Validating a declared capability needs a `ProfileDescriptor`, which requires I/O — and a sync `resolve()` cannot await one | **`resolve()` becomes `async`** (Goal 2). It awaits the descriptor and then runs the identical `validate_*_capabilities` call |
| **B** | Cluster may be unreachable when the consumer starts, and ADR-0005 forbids *both* blocking startup and failing startup on an unresolved dep | **A bounded await.** `resolve()` waits on the descriptor, not on cluster becoming reachable. On timeout it defers validation to readiness |

Async alone does not solve B — it changes how you wait, not whether you may. So `resolve()` has two outcomes, and which one applies depends solely on whether the descriptor is obtainable in time:

| | Descriptor available (Profile 1, or Profile 3 with cluster reachable) | Descriptor unavailable within the timeout (cold start) |
|---|---|---|
| Validation | **Inline**, at `resolve()` | Deferred to the SDK's readiness contributor |
| On failure | `Err(CapabilityNotMet { primitive, capability, provider })` — the full diagnostic, returned to the caller | `Unhealthy` carrying the same triple; `/readyz` stays 503 |
| Consumer code | Unchanged | Unchanged |

**In Profile 1 the reachable case is the only case.** The descriptor is intrinsic — the bound object *is* the real backend — so validation is immediate and exactly as it is today.

> **The SDK builds its own client rather than waiting for the wiring phase.** #4084's OoP path replays
> `ConsumerRegistration`s *after* `run_start_phase` (`host_runtime.rs:1441-1451`), while consumers resolve facades in
> `start` — so a design that waited for the phase would find an empty hub at every Profile 3 `resolve()`, and would
> defer validation unconditionally. Cluster does not wait: when the hub holds no `dyn ClusterClient` and the
> `grpc-client` feature is on, `resolve()` constructs the `RemoteClusterClient` itself (§4.9.3). `connect_lazy`
> touches no network and cluster derives its own endpoint (§4.5), so this cannot fail, block, or race anything
> meaningful.
>
> **The ordering it works around is being removed, and this branch stays anyway.** The #4084 owner has confirmed the
> post-`start` replay is incidental rather than required — the phase body needs only a populated `ClientHub` and a
> known gear registry — and will align the OoP path with the in-process order, so wiring runs before `start` in both
> (decision 16). That changes the *distribution*, not the design: inline validation becomes the normal case rather
> than the lucky one, and self-construction becomes defence against a phase that did not run rather than the
> load-bearing path for Profile 3. Keeping it is what makes the phase order a non-dependency in either direction,
> and its cost is one lazily-built channel.
>
> Two properties make that safe rather than clever:
>
> - **Local-wins is preserved by the crate graph, not by timing.** Self-construction only compiles under
>   `grpc-client`, which a Profile 1 monolith does not link (§3.2). "Empty hub" therefore cannot be misread as
>   "build a remote client" in the profile where an empty hub means a build mistake (§4.9.1).
> - **The wiring phase becomes a pre-population, not a precondition.** When it does run it registers the same client
>   and starts the descriptor prefetch. Both paths check before registering, and `ClientHub::register` is
>   last-write-wins, so the worst case of them racing is one redundant lazy channel dropped unused.
>
> The result is that inline validation is available in Profile 3 on its own terms, without a framework change on the
> critical path (decision 16). What still defers is a genuine cold start: cluster unreachable, descriptor not
> obtainable within the resolve timeout. **After the ordering alignment, that is the only reason left** — phase order
> stops contributing to the deferred path, so "deferred" means "cluster was not reachable in time" and nothing else.

**The deferred case is the platform cold start**, where cluster and its consumers come up together. `resolve()` returns `Ok`, having recorded `(profile, primitive, requirements)` in a process-local registry; each descriptor is validated against every requirement recorded for it when it lands, and any unmet requirement flips the gear to `Unhealthy`. Because a requirement can only be recorded by a `resolve()` call that actually happened, nothing is validated speculatively, and a profile no consumer resolves is never a readiness input.

The timeout is a startup-path cost sized in the hundreds of milliseconds to low seconds — long enough that a reachable cluster always answers, short enough not to stall phase 7 behind an absent one. It is an SDK constant, not consumer-facing config.

Three properties worth stating rather than leaving to be discovered:

- **`resolve()` has two behaviours.** This is not removable: the descriptor either exists at that moment or it does not, and the two permissible responses genuinely differ. What is invariant is the *guarantee* — no consumer serves traffic against an unmet requirement — and the error text. Which path a startup took must be logged at `info`, so an operator is never guessing.
- **`resolve()` retains a side effect** (recording the requirement) even in the inline case, because §5.6's descriptor refresh on a `generation` change re-validates against the recorded set.
- **A consumer that branches on `CapabilityNotMet` is correct in the reachable case and silently skipped in the deferred one.** The fallback arm does not run, and the pod stays not-ready instead. That is a cold-start caveat rather than a profile-wide one, and it belongs in the consumer docs (§10); the alternative — failing `resolve()` on a transient absence — would make correct configurations flaky, which is worse.

**A permanent error crash-loops rather than sitting not-ready.** After a bounded grace period — five minutes — a gear whose readiness is `Unhealthy` for a *permanent* reason exits the process, so the failure surfaces as `CrashLoopBackOff` with the diagnostic in the container's last log lines. The permanent set is exactly the rows classified so in §4.7's table: `ProfileNotBound`, `CapabilityNotMet`, and nothing-wired-in-the-hub past the grace window. A transient unresolved dependency never escalates, no matter how long it lasts.

The reasoning is that these errors do not resolve on their own. A never-ready pod is a quiet failure that a rollout can sit behind indefinitely, an operator has to go looking for, and a dashboard renders identically to "still starting"; a crash loop is loud, has a standard alert attached in every cluster, and puts the actionable error where the on-call already looks. ADR-0005 discourages startup failure for good reason — it must not be possible to fail startup on a *dependency* — but it says nothing about a configuration that cannot become valid, and the grace period is what keeps the two apart. Record the deviation against ADR-0005 (§10) so it reads as a considered exception rather than a violation.

### 4.8 Shutdown and the reverse-dependency drain rule

Platform DESIGN §"Drain order" states: **"if Gear A declares `deps = ["B"]`, B MUST drain *after* A"**, and that across processes the orchestrator/k8s preStop hooks must issue SIGTERMs in reverse-dependency order — an operator requirement, not runtime-enforced.

Cluster is a dependency of nearly everything, so it drains **last**. The ordering matters less here than the platform rule implies, because leases are store-owned (§5.8): a cluster that drains early revokes no coordination state, it only interrupts in-flight calls and subscriptions. `ClusterHandle::stop`'s existing three phases keep their order, with framework phases around them:

| Phase | Owner | Action |
|---|---|---|
| 1 | framework | `/readyz` → `Draining` (503); stop accepting new work; new session opens rejected with `Shutdown` |
| 2 | framework | Drain in-flight unary requests up to `drain_timeout_secs` (default 30 s) |
| 3 | cluster | **Close subscriptions, do not revoke leases.** Cache and leader watches get `Closed(Shutdown)`; a blocking `lock()` call in progress returns `Err(Shutdown)` as its unary response. **Held locks and leader claims are deliberately untouched** — they are store-owned (§5.8.1), so they survive this process and are renewed by their holders against the next replica |
| 4 | cluster | Deregister backends from the local hub (`ProfileNotBound` for embedded consumers); clear the `ProfileRegistry` |
| 5 | cluster | Plugin stop hooks, reverse-start order |
| 6 | framework | Deregister from DirectoryService; stop presence and auth-refresh tasks; close listeners |

**`cpt-cf-clst-fr-shutdown-revoke` changes meaning here, and the PRD must say so (§10).** The requirement was written for the embedded model, where the process shutting down *was* the leader — so revoking its claim before exit was the honest thing to do. When cluster is a broker, the process shutting down holds nothing of its own: revoking on its way out would tell a live, working leader it had lost leadership because an unrelated pod restarted. Under store-owned leases the guarantee is re-stated as **"a leader that loses its lease observes the loss"**, and a cluster restart is not a lease loss. What still propagates end to end is the *subscription* close: `Closed(Shutdown)` becomes a stream message the `Remote*Backend` translates back into `*WatchEvent::Closed(ClusterError::Shutdown)`, which `RestartingWatch` treats as non-retryable and re-establishes against the next replica. **But** a client whose process is already gone cannot observe anything, and its locks and claims lapse via TTL exactly as `cpt-cf-clst-fr-shutdown-ttl-cleanup` specifies — which is now the *only* path by which a lease ends other than an explicit release. The requirement's wording ("a current leader MUST observe loss-of-leadership before any consumer code runs again") needs both a reachability caveat and the re-statement above (§10).

**Operator requirement to document loudly**: a simultaneous SIGTERM blast to all pods breaks this ordering. Cluster's preStop hook needs a delay, or the orchestrator must terminate cluster last. Left implicit, a rolling restart will produce spurious `Closed(Shutdown)` storms across the fleet.

### 4.9 Consumer-side wiring

#### 4.9.1 What is wired, and what is resolved per call

**Profile resolution is server-side.** `profile` travels on every request and the cluster gear dispatches it to the bound backend (§5.2). No client-side object encodes which provider serves a profile, and none needs to: the plugins are linked into the cluster gear, so it is the only process that can make that mapping (§3.3).

What must still exist in the consumer's process is an object satisfying `Arc<dyn ClusterCacheBackend>`, because that is what every facade is built over. That object is structurally consumer-side:

- `ClusterCacheV1::resolver(&hub).profile(P).resolve()` executes **in the consumer's process**, against the consumer's own `ClientHub`.
- The cluster gear is a different process with a different hub; it cannot insert anything into a peer's hub.

But it is **derived, not registered** — obtained by asking the process's single `dyn ClusterClient` for it (§3.1), which costs an `Arc` clone and an interned name. So the split of responsibilities is:

| Registered once per process (§4.9.3) | Derived per `resolve()`, locally | Resolved per request, server-side |
|---|---|---|
| One `Arc<dyn ClusterClient>` — `LocalClusterClient` or `RemoteClusterClient`, local wins | The three backend handles for a given profile, via the client's factory methods | Which provider instance serves `(profile, primitive)` (§5.2) |

The consumer never *configures* what a profile means; it names one, and the cluster gear is the authority on the rest.

**The facade binds lazily, and that is what makes wiring order a non-issue.** A resolved facade holds `Arc<ClientHub>`, its profile, its recorded requirements, and a `OnceLock` for the backend:

- `resolve()` fills the slot eagerly when `dyn ClusterClient` is already in the hub. That is *always* true in Profile 1 (the cluster gear claims it in `init`, before any consumer's `start`) and normally true in Profile 3 (the wiring phase precedes `start`).
- If the client is not there yet, `resolve()` still returns `Ok` and the first call fills the slot.
- Steady state is one atomic load per call, not a hub lookup — the per-call resolution PR #4084's example performs (`api-contracts-consumer/src/domain.rs`) is paid once here instead of on every operation.

This is what keeps late registration *correct*: a consumer resolving during its own `start` does not need the client to have been registered first, and under #4084's OoP phase order it never is (§2.0). What lazy binding does **not** buy is the inline-validation behaviour of §4.7.1: an empty slot means no descriptor, so a facade that only bound lazily would defer validation on every Profile 3 startup. That is why `resolve()` constructs the client itself rather than waiting for the phase (§4.7.1) — lazy binding is the correctness backstop, self-construction is what makes inline validation the norm. Neither depends on the framework changing its ordering, which is why decision 16 is non-blocking (§9).

**What lazy binding must not hide.** Tolerating an empty hub is what makes the cold path work, but taken alone it would trade a loud startup failure for a quiet runtime one — and it would do so in **Profile 1**, where the failure is always a build or config mistake rather than a timing one. Today a hub miss is terminal at `resolve()`: `ProfileNotBound { profile }`, immediately (`cluster-sdk/src/cache/resolver.rs:55-64`). "Forgot to link `cluster`", "forgot the `profiles` block", "left the forwarding feature off" must not become first-request errors in a handler. Three rules close it:

- **A first call against a still-empty slot returns `ProfileNotBound { profile }`** — the same variant a reachable server returns for an unknown profile, distinguished by its message ("no cluster client registered in this process"). No new error variant, so the frozen `ClusterError` contract is untouched (§5.2).
- **The requirement registry is also the readiness contributor, and it is unfeatured.** §4.7.1 already has `resolve()` record `(profile, primitive, requirements)` in a process-local registry; that registry registers itself as a readiness contributor on first use. It must live in the SDK's ungated core rather than in the `ConsumerRegistration` (§4.9.3 step 3), because **in Profile 1 the registration closure never runs at all** — the forwarding feature is off, so nothing is inventoried to replay. A contributor reachable only from the remote branch would leave the embedded profile with no backstop whatsoever. It reports `Unhealthy` when a facade recorded requirements and no `dyn ClusterClient` ever appeared within a grace window, which puts "nothing is wired" back on `/readyz` instead of in a request path.
- **Resolve in `start`, never in `init`.** The framework's phases are global, not per-gear (`host_runtime.rs:847`): every gear's `init` runs before any gear's `start`, so the cluster gear's `start` — which registers `LocalClusterClient` (§11.2) — has not run during *any* consumer's `init`. An `init`-time resolve therefore never binds eagerly in either profile. **What happens next is not symmetric, and the rule is enforced by this prose alone** (`CFG-2`): since the gear registers its `LocalClusterClient` in `init` rather than `start`, a Profile 1 `init`-time resolve now finds a client over a still-empty `ProfileRegistry` and returns `Err(ProfileNotBound)`, while Profile 3 — whose hub is still empty at `init`, because wiring runs after it — returns `Ok` with an unbound facade. Measured both ways. So a consumer that ignores this rule starts in Profile 3 and **fails to start** in Profile 1. Arguably the better failure of the two, but it is an I1 asymmetry, and making the rule structural rather than advisory is open.

Together these keep the loud-failure guarantee of §4.7 while still letting a Profile 3 cold start proceed: absence is tolerated by `resolve()`, reported by readiness, and named by the first call if one arrives anyway.

**The one thing that is not per-call: the descriptor.** `ClusterCacheBackend::consistency()`, `features()` and `provider_name()` are **sync**, so they cannot make a request. They read a `ProfileDescriptor` held in a process-wide cache, populated by the registration's prefetch or by `resolve()` itself (§5.5). This is the sole reason any up-front server interaction survives.

Because `resolve()` is `async` (§4.7.1), it awaits that descriptor before constructing the facade — so in the reachable case **the sync accessors are populated by the time any consumer can call them**, and they answer exactly as they do in Profile 1.

> **Residual rough edge, confined to the cold-start path.** If `resolve()` timed out waiting for the descriptor, a consumer calling `consistency()` before it lands has no correct answer to receive: `CacheConsistency` is a frozen enum with no "unknown" variant. Containment: the descriptor cache is an `Arc<OnceLock<_>>` per profile; readiness gating means no consumer respecting `/readyz` can observe the unpopulated state; and the window exists only when cluster was unreachable at startup — the same condition under which the consumer is not serving traffic anyway. A read before population is a programming error by a consumer that started working before it was ready, not a state the design must render sensibly.

**The wiring must not, however, be *blocking*.** ADR-0005 forbids gating `start` on a dependency, so no design may make a consumer's startup wait for cluster to become reachable. Three independent properties guarantee that here, and no single one of them is relied upon alone: the registered object is profile-agnostic and its construction touches no network (§4.5), so registration cannot fail on an absent cluster; `resolve()` awaits only the descriptor, on a bounded timeout, falling back rather than failing (§4.7.1); and the facade binds lazily, so even a `resolve()` that ran before registration succeeds. An absent cluster delays a consumer's *readiness*, never its `start`.

#### 4.9.2 But it must not be hand-written per consumer

Requiring each consumer's `init` to call a wiring function is misplaced boilerplate, and it contradicts ADR-0005 directly: *"Gear developers MUST NOT write retry loops, health-check polling, or registration code. The `deps` declaration in `#[toolkit::gear(deps = [...])]` is the only input required."*

There is likewise **no cluster-side client configuration block**. Each field one would contain is already owned elsewhere, and introducing a cluster-specific answer would either duplicate framework knowledge or violate a cluster requirement outright:

| Field a client config would carry | Who owns it instead |
|---|---|
| `mode: auto \| embedded \| remote` | **Which impl is registered under `dyn ClusterClient`**, decided by what the binary links and settled by the local-wins check (§3.1). This is the platform's own mechanism, not a probe standing in for one: #4084's wire closure asks `hub.try_get::<dyn Contract>()` and yields to a local impl if present (`consumes.rs:179-181`) |
| `endpoint` | The **platform's** transport config (`ApiContractsConfig.remote_grpc_endpoints`), with k8s DNS by convention as the default (§4.5). Note the reason: not "the framework already resolves this" — the shipped dep loop resolves REST only — but that an override belongs in the platform's one designated place rather than in a second, cluster-specific one. A cluster-owned endpoint-resolver trait would be that second place |
| `connect_timeout_ms`, `rpc_timeout_ms` | `GrpcClientConfig`, which has both with defaults |
| `profiles: [..]` | The typed `ClusterProfile` markers. Listing them in config would put the profile string in a **third** place, which `cpt-cf-clst-fr-validation-typed-profile` explicitly forbids ("There MUST NOT be a third place where the string is re-typed") |

**The wiring belongs in the framework's proxy-wiring phase**, which is specified and now implemented to do precisely this shape of work.

> **What the merged phase does (§2.0).** `#[toolkit::consumes(contract = …, from = "…")]` emits an `inventory`
> `ConsumerRegistration { owner_module, dep_module, wire }` that the runtime's **proxy-wiring phase** replays at
> startup (ADR-0004, `cpt-cf-binding-adr-consumer-wiring`). Three of its properties are exactly what cluster needs:
>
> - the generated `wire` closure **short-circuits when the hub already holds the impl** (`consumes.rs:179-181`) — a
>   co-located provider wins, which is the whole of the embedded/remote decision, with no mode flag;
> - it returns `WireOutcome::{Local, Remote}` — a local dep is marked readiness-resolved immediately and spawns no
>   directory probe, so §4.7's Profile 1 row falls out of the framework;
> - it runs **once, before `start`, in both profiles** (`host_runtime.rs:609-613`), so a consumer that resolves in
>   its own `start` finds the client already registered.
>
> **Two constraints of the mechanism bind cluster's design, and neither is optional:**
>
> | Constraint | Consequence |
> |---|---|
> | `#[toolkit::consumes]` **does not inject a topo-sort dependency.** A separate attribute cannot mutate the `&'static` deps baked by `#[toolkit::gear]`, and auto-injecting `from` would make topo-sort fail for a *remote* provider — which would contradict non-blocking startup. The macro's own guidance is to declare co-located hard deps explicitly | Profile 1's ordering comes from cluster's `system` tier, which `run_start_phase` honours ahead of every application gear - **not** from a `deps` edge, which `K3` found is fatal in Profile 3 (§4.9.2). Nothing is automatic, and nothing needs to be |
> | The generated client is a **REST** resolving client (`<Contract>RestResolvingClient`, feature `contract-directory-rest-client` — the feature name admits it). There is no gRPC variant, and **none is planned in #4084** | Cluster is platform-plane and therefore gRPC (§2.2.1), so **cluster cannot use the attribute at all** under option A. It submits its own `ConsumerRegistration` — a plain struct — with a gRPC-backed `RemoteClusterClient`. The mechanism is reused; only the macro sugar is not. **This is settled rather than pending**: its owner confirms `ConsumerRegistration` is transport-agnostic (owner gear, dep, wire closure), so cluster's hand-written registration rides the same inventory and replay path and **will not need rework** when a generated gRPC client eventually lands (decision 16) |
>
> So cluster adopts the framework's wiring *phase* and its local-wins *semantics*, and hand-writes the one registration
> the macro cannot generate for it. That is a smaller deviation than it sounds: the macro emits a `ConsumerRegistration`
> and an `inventory::submit!`, both of which cluster can write directly.

Two small additions make cluster fit it:

1. **Profiles self-register.** A `ClusterProfile` impl emits an `inventory` entry, the same process-global collection mechanism `GearRegistry::discover_and_build()` and GTS type registration already use. The wiring enumerates inventoried markers instead of reading a config list — so the profile name stays in its two legitimate places.

   ```rust
   #[derive(Clone, Copy)]
   pub struct EventBrokerProfile;
   impl ClusterProfile for EventBrokerProfile { const NAME: &'static str = "event-broker"; }
   cluster_sdk::register_cluster_profile!(EventBrokerProfile);
   ```

2. **An SDK-submitted `ConsumerRegistration`** (§4.9.3), submitted by `cluster-sdk` rather than written per consumer. Because the registered object is a single `Arc<dyn ClusterClient>`, this is exactly the one-dep-one-object shape `#[toolkit::consumes]` generates — cluster writes the registration by hand only because that macro emits a REST client and cluster's transport is gRPC, and because cluster's registration also starts the descriptor prefetch.

Consumer-visible surface then reduces to the marker, and nothing else:

```rust
#[toolkit::gear(name = "event-broker", capabilities = [..])]        // no `deps = [cluster]`
```

No wiring call, no cluster-specific config, no dependency entry, and the transport choice moves out of consumer code entirely.

> **Corrected by `K3`: a consumer must NOT declare `deps = [cluster]`.** This section previously said the surface was
> the marker *plus* that `deps` entry. It is fatal in Profile 3. `deps` is a **hard** topo-sort edge — `RegistryBuilder`
> fails the entire registry build with `RegistryError::UnknownDependency { gear, depends_on }` when a named gear is not
> linked (`registry.rs:565-568`) — and a Profile 3 consumer links no cluster gear at all. So the line that was meant to
> be the one thing a consumer writes is the one thing that would stop it starting, which would have broken invariant I1
> outright: the same source file could not compile in both profiles.
>
> Omitting it costs nothing, because neither property the edge would buy depends on it:
>
> | What `deps = [cluster]` was for | What actually supplies it |
> |---|---|
> | **Start ordering** — cluster's `start` before the consumer's | The `system` tier. `run_start_phase` iterates `gears_by_system_priority()` (`host_runtime.rs:838`), which runs every `system`-capability gear ahead of every other one (`registry.rs:290-304`), and cluster declares `system` (§4.2). Measured, not assumed: a non-`system` gear in `cluster/tests/oop_probe_ordering.rs` finds cluster's profiles already published when its own `start` runs, with no `deps` edge in the process |
> | **Readiness gating** on cluster | `ConsumerRegistration::dep_gear`, which the wiring phase registers with the `DependencyChecker` (`host_runtime.rs:520`). Cluster's own registration carries `dep_gear: "cluster"`, so a consumer gets the `/readyz` gate in **both** profiles without writing anything |
>
> **The one exception, named so it is not discovered:** a consumer that is *itself* `system`-tier shares cluster's
> priority group, where ordering falls back to topo order within the group. Such a gear does need the edge — and may
> write it only when cluster is co-located, which makes it a Profile 1 declaration and not part of the portable surface.

> **Platform-doc tension to resolve — the one part of decision 16 still outstanding.** The platform DESIGN says both "the gear's `init` picks the transport ... then registers the chosen `Arc<dyn FooApi>` in ClientHub" (§"Responsibility boundaries") and that the bootstrap wires clients on dep resolution (§"OoP bootstrap"). Those are different owners for the same act. PR #4084's ADR-0004 settles it — the runtime's proxy-wiring phase replays an inventoried `ConsumerRegistration` — so the DESIGN's first reading should be narrowed to "the gear's SDK declares how to build each transport variant". Ownership is therefore a **documentation fix** (§10), and it is what remains of decision 16 now that ordering and the gRPC-codegen question are answered: the OoP path's post-`start` replay was incidental and is being aligned, and no gRPC counterpart to `#[toolkit::consumes]` is planned. Cluster needed neither answer — its SDK builds its own client at `resolve()` (§4.7.1) — so both are improvements to the surrounding platform rather than unblockings.

#### 4.9.3 The registration itself

Cluster's `ConsumerRegistration` takes no cluster-specific configuration, does no profile-specific work, and **awaits nothing**. **It ships enabled** — `ConsumerRegistration` is on `main` (§2.0), and the phase that replays it now runs before `start`, so this is the ordinary construction path and `resolve()`'s self-construction is the fallback rather than the rule (§4.7.1). Its `wire` closure has the same three-step body the macro would generate, plus a prefetch:

```rust
// cluster-sdk/src/wiring.rs  [feature: grpc-client]
//
// Submitted via `inventory::submit!`, replayed by the framework's proxy-wiring
// phase — never called from consumer code. It PRE-POPULATES what `resolve()`
// would otherwise build on demand (§4.7.1). The OoP path replays it after
// `start` today and will be aligned to run before `start` (decision 16), but
// nothing here may depend on it having run either way. `endpoint` is whatever
// the framework resolved — DirectoryService or the ADR-0004 static override —
// and when it is empty cluster derives its own (§4.5).
//
// Written by hand rather than via `#[toolkit::consumes]` because that macro
// emits a REST resolving client and cluster's transport is gRPC (§4.9.2).

inventory::submit! {
    ConsumerRegistration {
        owner_module: "cluster-sdk",
        dep_module:   "cluster",
        wire: |hub: &ClientHub, endpoint: &str| -> anyhow::Result<WireOutcome> {
            // 1. Local wins — a co-located cluster gear already registered its
            //    LocalClusterClient, so this process resolves every profile through
            //    it and no channel is built at all (§7.2.7).
            // `ClientHub` has get / get_scoped / try_get_scoped but NO unscoped
            // `try_get` (verified, client_hub.rs:142-248), so this is `.is_ok()`.
            if hub.get::<dyn ClusterClient>().is_ok() {
                return Ok(WireOutcome::Local);
            }
            // 2. Register the remote impl unless `resolve()` already built one
            //    (§4.7.1). `ClientHub::register` is last-write-wins with no
            //    if-absent form (client_hub.rs:142-149), so this is a
            //    check-then-register; the race is benign, since both clients are
            //    equivalent and the loser's lazy channel is dropped unused.
            if hub.get::<dyn ClusterClient>().is_err() {
                let client = RemoteClusterClient::connect_lazy(
                    if endpoint.is_empty() { &derive_endpoint()? } else { endpoint },
                )?;
                hub.register::<dyn ClusterClient>(Arc::new(client));
            }
            // 3. Background: descriptor prefetch. The readiness contributor is
            //    NOT registered here — it belongs to the unfeatured requirement
            //    registry, so Profile 1 has it too (§4.9.1).
            spawn_descriptor_prefetch(hub);
            Ok(WireOutcome::Remote)
        },
    }
}
```

Step 3 gates `/readyz` only: one `DescribeProfiles` covering every inventoried profile marker (§5.5). It gates neither `start` nor `resolve()`.

**The readiness contributor is deliberately not part of this closure.** It reports `Starting` until descriptors land, `Unhealthy` if any recorded requirement is unmet (§4.7.1), and `Unhealthy` if no client was ever registered (§4.9.1) — and the last of those is a *Profile 1* failure mode, in a process where this closure never runs. So the contributor is owned by the requirement registry in the SDK's unfeatured core and registered by the first `resolve()`, not by the wiring. Only the prefetch, which needs a channel, belongs behind `grpc-client`.

**`resolve()` is where a profile becomes a backend**, and it makes no embedded/remote decision of its own — that was settled by which impl is in the hub:

1. Take `Arc<dyn ClusterClient>` from the hub. If it is not there, and the `grpc-client` feature is on, **build one**: `RemoteClusterClient::connect_lazy(derive_endpoint()?)`, registered unless one appeared meanwhile, descriptor prefetch spawned (§4.7.1). This is the branch that makes the phase order irrelevant — and it stays after the OoP path is aligned to wire before `start` (decision 16), at which point it is defence against a phase that did not run rather than the usual path. Unfeatured — Profile 1 — an empty hub instead leaves the facade's backend slot empty and skips to step 4; the first call binds it, and the readiness contributor reports the build mistake (§4.9.1).
2. Ask it for this profile's backend — `client.cache_backend(profile)` and siblings. Locally that returns the real backend from the `ProfileRegistry`; remotely it constructs a `Remote*Backend`. Sync and pure either way, no I/O.
3. Await this profile's descriptor, bounded by the resolve timeout (§4.7.1): served from the process cache if the prefetch already landed, otherwise by one `DescribeProfiles`. In Profile 1 it is intrinsic — the bound object *is* the real backend.
4. Record `(profile, primitive, requirements)`, then **validate inline** if a descriptor is in hand, or leave validation to the readiness contributor if it is not. Return the facade.

Three properties follow. There is **no fallback branch and no mode flag** — one hub lookup, one factory call, and the local-wins check in the registration is the entire decision, which is what makes this the same code path in both profiles. It can only ever resolve *toward* whatever was registered, so there is no path by which a consumer invents a local in-memory cache; the split-brain hazard a mode flag would have to guard against is structurally absent rather than merely checked. And because step 1 tolerates an empty hub, step 3's wait is bounded, and step 4 never fails on absence alone, **a consumer resolving during its own `start` succeeds in both profiles** whether or not cluster is up yet — the property ADR-0005's non-blocking startup requires.

> **What this asks of the framework (decision 16) — preferences, not dependencies, and both now answered.** PR
> #4084's closure is `wire: |hub: &ClientHub, endpoint: &str| -> anyhow::Result<WireOutcome>`, replayed at startup.
> Cluster's registration fits that signature as written, confirmed by its owner: `async` and
> N-registrations-per-dep are not needed, because the registered object is one synchronously-constructed
> `Arc<dyn ClusterClient>`.
>
> | Ask | Weight | Status |
> |---|---|---|
> | Invoke the registration before `start` in the OoP path | Preference | **Granted.** The divergence was incidental, not required, and the OoP path is being aligned with the in-process order (§2.0). Cluster keeps its own branch regardless — the ordering costs one lazily-constructed channel, not a behaviour — but inline validation becomes the normal case (§4.7.1) |
> | Own the k8s-DNS naming convention, incl. a fallback when no `DirectoryClient` is present | Preference | **Agreed in principle, with a prerequisite.** The resolver is cheap and its owner will add it; the platform-wide **port convention** it needs does not exist and belongs in the platform PRD, and the bootstrap's eager directory connect must be lifted first (§4.5, decision 21). Cluster derives the string itself either way, so this buys uniformity across gears rather than capability for this one |
>
> Phase 4 is therefore buildable and *deliverable* against the shipped order, and strictly better against the
> aligned one. What the framework buys by reordering is a more predictable validation path for every consumer, not
> the recovery of a property cluster had lost.

Deliberately **not** here: any client-side wrapping in SDK-default backends. In remote mode the server already composed each profile's three primitives (native or SDK-default per operator config); the client takes them as given, keeping the omit-default decision in exactly one place as `cpt-cf-clst-fr-routing-omit-default` intends.

### 4.10 Deployment artifacts and operator config

Cluster pod config:

```yaml
oop_http:
  listen_addr: "0.0.0.0:8080"
  probe_bind_addr: "0.0.0.0:9090"      # probes off the Service, per ADR-0005
  drain_timeout_secs: 30
  healthcheck_timeout_ms: 500
  internal_auth: { audiences: ["toolkit-internal"] }   # shape indicative — see note

gears:
  grpc-hub:
    config:
      listen_addr: "0.0.0.0:50051"
      advertise_addr: "cluster.platform.svc.cluster.local:50051"

  cluster:
    config:
      profiles:
        default:
          cache: { provider: postgres, connection_string: "postgres://gears:${DB_PASSWORD}@pg:5432/gears" }
        event-broker:
          cache: { provider: postgres, connection_string: "postgres://gears:${DB_PASSWORD}@pg:5432/gears" }
          lock:  { provider: postgres, connection_string: "postgres://gears:${DB_PASSWORD}@pg:5432/gears" }
        oagw:
          cache: { provider: redis, url: "redis://redis:6379" }
      # Optional. How long a lease record outlives its lease so the fence stays
      # monotonic across a lapse (§5.8.1). Must exceed the longest lease TTL in
      # use; the backends refuse to start otherwise.
      fence_retention: 1h
```

Helm values, using the existing `toolkit-common` library chart:

```yaml
replicaCount: 1                          # shipped default, not a constraint — §5.8.3
strategy: { type: RollingUpdate }        # safe: leases are store-owned, not replica-owned — §5.8
grpc: { enabled: true, port: 50051 }     # the sanctioned internal-gRPC shape
service: { type: ClusterIP, port: 8080 }
internalAuth: { audience: toolkit-internal }
```

Note that `default` and `event-broker` name the same DSN — three bindings, and today three independent pools. §5.3 collapses them to one. Also needed: a `NetworkPolicy` restricting :50051 to platform namespaces, and a preStop delay per §4.8.

**Neither of those two values is load-bearing any more**, which is the visible payoff of §5.8. A `RollingUpdate` transiently running two pods is harmless — both serve the same store-owned leases — and `ClusterIP` is now a genuine load balancer across replicas rather than a pin holding one channel to one backend. `replicaCount: 1` is a shipped default awaiting the failover suite (§5.8.3), not a rule.

> **Nothing here needs enforcing, in k8s or outside it.** Two cluster processes are two workers against the same
> store-owned leases (§5.8), so process count is a capacity decision — there is no correctness rule for a values
> file, a startup assertion or an operator to get wrong.
>
> **Cross-reference — [PR #4426](https://github.com/constructorfabric/gears-rust/pull/4426) (instance-addressable
> discovery, open).** Its §5 requires *targeted* roles to sit behind a headless Service + `StatefulSet` and never a
> VIP. Cluster is an **entrypoint** role under that ADR's own taxonomy — and note the reason, because it is *not*
> replica count: **no consumer ever addresses a specific cluster instance.** Any replica serves any request
> (§5.8.1), so there is nothing to target, and that holds at one replica or ten. `ClusterIP` is therefore permitted
> rather than in conflict at any replica count, and cluster is not a *consumer* of instance targeting either,
> because it does not serve discovery. One sequencing question remains: ADR-0009 cites "a sharded cluster gear"
> as a motivating shape, and **per-shard targeting is a routing model this document does not adopt** — not because
> cluster is a singleton, but because sharding leases across instances would reintroduce the affinity §5.8 removed
> (`cpt-cf-clst-fr-routing-per-primitive` stays deferred). Two edits are owed to #4426 regardless (§10) — the
> cluster-coordinator motivation and the topology-watch ownership.

> **`internal_auth` field names are provisional.** `oop_http.internal_auth` is `Option<InternalAuthConfig>` on `main`, populated only when the `k8s-auth` feature is enabled, and [PR #4403](https://github.com/constructorfabric/gears-rust/pull/4403) reworks it (`libs/toolkit-security/src/internal_auth_config.rs`) to cover shared-secret alongside Kubernetes SA tokens. Reconcile the block above against that type before this YAML is copied into a deployment (phase 5).

#### 4.10.1 Schema migrations

The Postgres plugin runs its own `sqlx` migrators inside `build_cache` / `build_lock` — that is, during gear `start` (phase 7), not the framework's DB migration phase (phase 2).

**This is safe as it stands.** The migrator records applied versions in `_sqlx_migrations` and takes a Postgres advisory lock before migrating (`plugins/postgres-cluster-plugin/src/pg_setup.rs`), so re-runs are no-ops and concurrent replicas serialize rather than corrupt. Migration-on-startup is therefore a hardening item, not a defect.

**The framework's migration phase does not fit**: that phase is tied to the `db` capability, which assumes one database per gear. Cluster's DDL is owned by *plugins*, potentially several of them, across N distinct DSNs drawn from the profile matrix. No framework hook knows that matrix — but cluster does, so the orchestration belongs to cluster.

What migration-on-startup does cost, once cluster is a multi-replica pod:

| Cost | Why it matters |
|---|---|
| **Blast radius** | A failed migration crash-loops the pod, and cluster is the coordination dependency for the fleet |
| **Least privilege** | The runtime DB user must retain DDL rights permanently; a compromised cluster pod can `ALTER` / `DROP` |
| **Startup budget** | Replicas serialize on the advisory lock; a long migration eats the `startupProbe` window (§4.4) |

Rolling-update safety is orthogonal — backward-compatible migrations are a discipline regardless of where they run.

**Recommendation — a cluster-owned migration entrypoint, additive and non-breaking:**

1. **`cluster-oop migrate` subcommand** on the existing binary. Loads the same `ClusterConfig`, dedupes bindings by the §5.3 instance key so one DSN migrates once regardless of how many profiles reference it, invokes each provider's migration hook, and exits.
2. **One defaulted provider-trait method**, so plugins without DDL (standalone, K8s, Redis) are unaffected — the same additive, dyn-safe shape as ADR-010's `scan_prefix` and §4.4's `probe()`:

   ```rust
   /// Runs this provider's schema migrations. Default: no-op.
   async fn migrate(&self, options: &Map<String, Value>) -> Result<(), ClusterError> { Ok(()) }
   ```

3. **A per-binding `migrations` mode**:

   | Mode | Behaviour | Intended for |
   |---|---|---|
   | `auto` (default) | Migrate during `build_*`, exactly as today | Profile 1, dev, tests — **nothing changes** |
   | `verify` | Assert the schema is at the expected version and fail startup naming the mismatch; **no DDL** | Production pods |
   | `skip` | Trust the operator | Escape hatch |

4. **Helm** runs `cluster-oop migrate` as a `pre-upgrade` / `pre-install` Job, with pods configured `verify`.

`verify` earns its place independently of the Job: it turns "wrong schema" from a confusing runtime error into a startup failure that names the expected version, and it is what allows the runtime DB grant to drop DDL.

Because `auto` stays the default, this is purely additive — it can land with the deployment artifacts (phase 5) rather than blocking the design.

### 4.11 Modification checklist

| # | Change | Location | Effort |
|---|---|---|---|
| 1 | Contract traits (`#[contract]`-shaped) + DTOs + `ClusterError` ⇄ `CanonicalError` codec (§6.9) | `cluster-sdk/src/{contract,dto,convert}.rs` | L |
| 2 | gRPC projection **generated from the contract IR** — `#[toolkit::grpc_contract]` + `toolkit-contract-protogen`, `proto.lock.toml` committed from the first commit, and the generated client behind the `*Api` traits; the `ClusterError` ⇄ `CanonicalError` codec via `#[derive(ContractError)]`. Generated from the contract IR after #4084 lands (§6.1) | `cluster-sdk/src/{grpc,convert}.rs`, `proto/`, `build.rs` | L |
| 2b | **The `ClusterClient` trait** — unfeatured, three sync factory methods plus the async `descriptor()` (§3.1). Small but ordering-critical: items 3, 4, 7c and 13c all target it | `cluster-sdk/src/client.rs` | S |
| 3 | `RemoteClusterClient` + descriptor cache + the three `Remote*Backend` handles. Lock handles are unary (§6.5), so only the leader watch needs a channel pump | `cluster-sdk/src/{client/remote,descriptors,backend}.rs` | M |
| 4 | `register_cluster_profile!` plus the `ConsumerRegistration` cluster submits by `inventory` — **ungated: `ConsumerRegistration` ships on `main`** (`libs/toolkit/src/discovery.rs`). Nothing waits on it: `resolve()` self-constructs the client (§4.7.1), so the phase is pre-population. **No config type, no endpoint resolver** — cluster derives its own endpoint (§4.5) | `cluster-sdk/src/{wiring,profile}.rs` | S–M |
| 5 | Four hand-written gRPC service impls over the generated `*_server` traits — the sanctioned pattern, not interim glue (§6.1). Each adds `ProfileRegistry` dispatch between DTO conversion and backend call | `cluster/src/api/grpc/` | L |
| 6 | Server-side session index: watch subscriptions keyed by subscription id, swept per §5.4.1. No lease lifetime here — that is the store's (§5.8.1), and no lease *index* either (§5.4) | `cluster/src/api/grpc/{subscriptions,sweep}.rs` | M |
| 7 | `ProfileRegistry` — runtime-queryable, populated by `start` | `cluster/src/registry.rs` | M |
| 7b | **`ClusterWiring::from_config` returns the bound-profile set alongside the `ClusterHandle`.** Today it returns `Result<ClusterHandle, ClusterError>` (`wiring.rs:127-131`) and discards exactly what the registry needs — per-profile provider identity, declared features and shared-instance refs. Without this there is nothing for `start` to `publish`, so item 7 has no data source. Hub registration under `cluster:{profile}` (`cluster-sdk/src/registration.rs:44-52`) and the all-or-nothing rollback (`wiring.rs:298,365-396`) are unchanged; this is a return-shape change, and the only signature in the current tree this design alters | `cluster/src/wiring.rs` | S |
| 7c | **`LocalClusterClient`** — implements `ClusterClient` over the `ProfileRegistry`, registered under `dyn ClusterClient` by the gear's `start` alongside the existing per-profile hub registrations (§11.2). This is what makes Profile 1 resolve through the same trait as Profile 3 and gives the local-wins check something to find. Lives in the gear crate, not the SDK, because it depends on gear state (§3.4) | `cluster/src/local_client.rs`, `gear.rs` | S |
| 8 | `BackendInstanceCache` — dedup by (provider, canonical options), refcounted | `cluster/src/registry.rs`, `wiring.rs` | M |
| 9 | Capabilities `+grpc, rest, system`; `GrpcServiceCapability`; `RestApiCapability` | `cluster/src/gear.rs` | S |
| 10 | Composite readiness healthcheck + defaulted `probe()` on the cache trait; **`ProfileHealth` on `ProfileDescriptor`**, which is what the consumer-side per-profile gate reads (§4.4, item 13) | `cluster/src/health.rs`, `cluster-sdk/src/cache/backend.rs`, `dto.rs` | S |
| 10b | **Store-owned leases** (§5.8.1): lease records carry `owner` + `deadline` + monotonic `fence`; `renew` / `release` become conditional writes predicated on the token, with no session lookup. Touches the SDK-default CAS backends (which already have this shape — a cache key plus an owner token) and the lock/leader wire contracts | `cluster-sdk/src/{lock,leader}/`, `cluster/src/defaults/` | M |
| 10c | **Postgres lock: remove the liveness beacon** (§5.8.2). `stealable_predicate` becomes `expires_at <= now()`; drop `holder_beacon_*`, the non-negative check and the beacon connection; delete the shutdown drain and the incarnation-keyed orphan sweep. **Changes merged code** ([#4411](https://github.com/constructorfabric/gears-rust/pull/4411)) — pair with its author | `postgres-cluster-plugin/src/lock/{mod,beacon,reaper}.rs`, `src/shutdown.rs`, `migrations/lock/` | M |
| 10d | **Fence retention in the cache-backed defaults** (§5.8.1): the lease record carries `{ owner, deadline, fence }` in the value, acquisition CASes rather than delete-inserts so the counter survives a change of owner, and the record's physical expiry is `deadline + fence_retention`. The TTL reaper must skip records inside the retention window, and both plugins reject a `fence_retention` shorter than the longest configured lease TTL | `cluster/src/defaults/`, `standalone-cluster-plugin/src/cache.rs`, `postgres-cluster-plugin/src/cache/reaper.rs` | M |
| 11 | Admin/diagnostic REST routes (profiles, sessions, instances) | `cluster/src/api/rest/` | M |
| 12 | Binary target: `main.rs`, `registered_gears.rs`, `[[bin]]` | `cluster/` | S |
| 13 | Consumer-side readiness contributor: transient/permanent classification (§4.7) plus recorded-requirement validation for the deferred path and §5.6 refreshes (§4.7.1), **plus the per-profile health gate** — not-ready while any recorded profile reports `Degraded`, re-read on a 10 s poll and reversible without a restart (§4.4). **Unfeatured, owned by the requirement registry, registered by the first `resolve()`** — not by item 4's wiring, which never runs in Profile 1 (§4.9.1). Note this carries the validation guarantee whenever the inline path could not run — a cold start, or cluster unreachable within the resolve timeout (§4.7.1) | `cluster-sdk/src/requirements.rs` (unfeatured), `descriptors.rs` | M |
| 13b | **Make the three resolvers `async`** and migrate the 73 in-tree `.resolve()` call sites. Signature-only and mechanical; do it early so it lands before any gear consumes cluster (Goal 2, §4.7.1) | `cluster-sdk/src/{cache,lock,leader,discovery}/resolver.rs`, cluster tests/examples | S |
| 13c | **Repoint the resolvers at `dyn ClusterClient`, and make the facades bind lazily.** Today `resolve()` reads a scoped backend straight out of the hub and a miss is terminal (`cache/resolver.rs:55-64`). It becomes §4.9.3's four steps: hub lookup for the client, factory call for the backend, bounded descriptor await, record-then-validate. The facade holds `Arc<ClientHub>` + profile + `OnceLock<backend>` instead of the backend, so a resolve that preceded registration binds on first call. **No `#[cfg]` in the resolve path and no fallback branch** — the local-wins check in item 4 is the whole decision. **Not mechanical**: this is where §4.7.1's inline-vs-deferred validation split is implemented | same four `resolver.rs` files, plus the four facades | M |
| 14 | Revoke fan-out in `ClusterHandle::stop` — terminal events to remote watch subscribers and in-flight `lock()` callers (§4.8 phase 3) | `cluster/src/wiring.rs` | M |
| 15 | Container image, Helm values, NetworkPolicy, preStop delay, example configs | deploy assets | M |
| 15b | `cluster-oop migrate` subcommand, defaulted `migrate()` provider hook, `migrations: auto\|verify\|skip` mode, Helm pre-upgrade Job (§4.10.1) | `cluster/src/main.rs`, `cluster-sdk/src/provider.rs`, deploy assets | M |
| 15c | **`cluster_sdk::testing::embedded_hub(config)`** behind a `testing` feature — builds a hub with the profiles wired *and* a `LocalClusterClient` registered, which is the three-step setup every consumer test now needs and the one thing `ClusterWiring::from_config` no longer suffices for (§12.3). Without it each consumer re-derives `publish` + `register`, and the first to forget it hits §12.5's failure inside a test | `cluster-sdk/src/testing.rs` | S |
| 16 | Conformance suite over the remote transport (§7.6) | `cluster-conformance`, `cluster/tests/remote_conformance.rs` | M |
| 17 | Doc deltas: cluster PRD/DESIGN, ADR-002 amendment, new ADRs, platform-doc fixes | `docs/` | M |

## 5. Runtime Profile Management

### 5.1 What must become runtime state

Today a profile is transient: `ClusterWiring::from_config` reads it, builds the primitive `Arc`s, registers them in the hub, and forgets everything else. The name, provider identity, declared features and options survive in no queryable form. The hub is a type-keyed map — it can answer "give me the cache backend for `cluster:event-broker`" but not "what profiles exist", "which provider serves this one", or "is this DSN shared".

Serving remote clients needs all of it:

| Need | Why |
|---|---|
| Enumerate/describe profiles | A client must be told what it is bound to, and must obtain `consistency()`/`features()` for capability validation — its `Remote*Backend` accessors are sync (§3.1) |
| Dispatch a request's `profile` to the right backend | The "route the request from the specific connection" requirement. One process now serves *all* profiles, so routing is per-request |
| Identify the provider instance behind a profile | Observability, the readiness aggregate, refcounted teardown |
| Share instances across profiles | The N-hot-connections problem (§5.3) |
| Track which client holds which session against which profile | Reap on client death, scope shutdown revocation, per-client metrics, quotas (§5.4) |
| Add/remove profiles without a restart | An operator config change should not require draining every consumer (§5.6) |

### 5.2 The `ProfileRegistry`

```rust
/// Runtime, queryable view of every bound profile. Created in `init`, populated
/// by `start`, read by every service impl on every request.
pub struct ProfileRegistry { inner: arc_swap::ArcSwap<RegistrySnapshot> }

struct RegistrySnapshot { generation: u64, profiles: BTreeMap<String, Arc<BoundProfile>> }

pub struct BoundProfile {
    pub name: String,
    pub cache: Arc<dyn ClusterCacheBackend>,
    pub leader_election: Arc<dyn LeaderElectionBackend>,
    pub lock: Arc<dyn DistributedLockBackend>,
    /// Shipped to clients: provider identity + declared characteristics per
    /// primitive, so a client answers consistency()/features() synchronously.
    pub descriptor: ProfileDescriptor,
    /// Which shared backend instances this profile is built from (§5.3).
    pub instances: ProfileInstanceRefs,
}
```

Reads are `ArcSwap::load()` — no lock on the request path, which matters at 10k ops/s under a 5 ms budget. `generation` lets a client detect that the server's profile set changed under it (§5.6).

> **SDK friction.** `ClusterError::ProfileNotBound { profile: &'static str }` (verified at `cluster-sdk/src/error.rs:84`) cannot hold a name arriving in a request. Either widen the variant (a breaking change to a frozen error enum) or intern profile names at registration with `Box::leak`. **Recommend interning** — the profile set is config-bounded, so the leak is bounded, and the frozen `ClusterError` contract stays intact. Revisit if dynamic reload (§5.6) makes the set unbounded over process lifetime.

The registry replaces nothing. Hub registrations stay, because the server's own SDK-default backends resolve through them; the registry is the additional index the wire needs.

**It is also what `LocalClusterClient` dispatches through**, so it is load-bearing in Profile 1 rather than only on the remote path (§3.1). That is deliberate: one profile→backend dispatch mechanism serves both profiles, where an earlier shape would have had the hub's scoped entries serve Profile 1 and the registry serve Profile 3. `cache_backend(profile)` is an `ArcSwap::load()` plus a `BTreeMap` lookup returning the real `Arc` — no wrapper interposed, so the embedded hot path is unchanged.

### 5.3 Backend instance sharing — the N-connections problem

The concern raised: *"a configuration could define more than 1 deployment profile and the connection strings could differ, so you could end up with N number of postgres connections to ensure they stay hot."* Two halves, different answers.

**Half 1 — connections move from consumers to the cluster gear, which shrinks the total.** Today each consumer *process* binding a Postgres profile opens its own pool: `replicas × gears × pool_max_size`, plus per-instance LISTEN connections and reapers. In Profile 3 consumers hold **zero** backend connections — one channel each. The total becomes a function of the cluster gear's own deployment: `cluster_replicas × distinct_instances × (pool_max_size + listeners)`. A 10-replica event broker on a 5-connection pool goes 50 → 5 at one cluster replica, and scales with the cluster replica count rather than with every consumer's — which is the point, since cluster replicas are counted in single digits and consumer replicas are not. This also narrows the deferred credential problem to one deployment target.

**Half 2 — distinct connection strings do mean distinct hot instances, so the job is not to multiply them needlessly.** Today `from_config` calls `provider.build_cache(&options)` once per profile per primitive, unconditionally (`wiring.rs:135,171,185,199`). Two profiles naming the same DSN get two pools, two reapers, two LISTEN connections. With one process hosting every profile that waste is no longer hypothetical — the §4.10 config alone produces three Postgres pools where one would do.

**Proposed: a `BackendInstanceCache` keyed by `(primitive, provider, canonical_options_digest)`**, holding `Weak` refs while profiles hold strong `Arc`s, so an instance's `StopHook` runs exactly when the last profile releases it — which is what makes dynamic profile removal safe (§5.6).

Design points worth stating explicitly:

- **Canonicalisation must be conservative.** Digest the options map with sorted keys *after* `${VAR}` expansion (the Postgres plugin already expands via `toolkit_macros::ExpandVars`). Do **not** attempt DSN-semantic equivalence (`localhost` vs `127.0.0.1`, default-port elision): a false merge silently points a profile at the wrong database, a false split only costs a redundant pool. Asymmetric risk ⇒ digest raw canonical text.
- **`secret_ref` participates in the key.** Same DSN template, different credentials ⇒ different instances.
- **Sharing is not merging profiles.** Two profiles sharing one Postgres cache still have independent SDK-default LE/lock backends layered above it. Their coordination namespaces are whatever consumers scope them to — sharing changes nothing there, since both already write to one `cluster_cache` table today. Any consumer treating separate profiles as an *isolation* boundary rather than a *backend-selection* one was already mistaken; say so in the operator docs. This is also why cluster does not authorize callers per profile — a check on a name that two profiles can route around is not a boundary (§7.1).
- **Pool sizing must account for fan-in.** One instance now backs every profile bound to it and every remote client behind those profiles. `pool_max_size: 5` was sized for one consumer process; it now backs the fleet. Sizing guidance and a saturation metric are required (§5.7), and the default should be revisited for this deployment shape.
- **Keeping instances hot** is then automatic: they are owned by the long-lived registry, and the plugins' existing background work (sqlx keepalive, TTL reapers, LISTEN connection) runs for the process lifetime.

### 5.4 Per-client profile binding and session tracking

*"we will need to track what deployment profile is being used by a specific SDK client."* The `profile` field is the routing key; the **session index** holds the per-client state that is genuinely connection-scoped.

```rust
pub struct ClientId {
    /// From the platform plane (§4.6): SA name now, SPIFFE workload later.
    pub identity: PlatformIdentity,
    pub instance_id: String,
}

pub enum Session {
    /// Replica-affine and re-establishable: a subscription carries no lease
    /// authority, so losing it costs a `RestartingWatch` cycle (§5.8.1).
    Watch { id, client: ClientId, profile: Arc<str>, kind: WatchKind, target: String },
}

/// Index-only, not lifetime ownership: leases live in the backing store
/// (§5.8.1), so nothing in the lease path reads this. It exists for
/// diagnostics, quotas and shutdown fan-out.
pub struct LeaseIndexEntry {
    pub client: ClientId,
    pub profile: Arc<str>,
    pub kind: LeaseKind,      // Lock | Election
    pub name: String,
    pub deadline: Instant,    // mirror of the stored deadline, for reporting
}
```

**The registry holds subscriptions and an index, not leases.** That is the §5.8.1 change landing here: `Renew` / `Release` / `Resign` are conditional writes predicated on the token the client presents, so the replica serving them needs no memory of the acquire. What remains registry-owned is the watch subscription, which genuinely is per-connection state.

- **Reaping on client death is TTL-driven**, exactly as the contract specifies (`cpt-cf-clst-fr-lock-release`, `cpt-cf-clst-fr-shutdown-ttl-cleanup`). A holder that stops renewing lapses at its stored `deadline`; no reaper has to notice the client is gone, and no replica has to be the one that issued the lease.
- **Leader renewal liveness needs nothing extra**, because renewal is client-driven again (§7.3) — a wedged consumer stops renewing and loses its claim, which is the in-process behaviour.
- **Shutdown fan-out is watches only.** §4.8 phase 3 closes subscriptions; leases are deliberately left alone, since revoking them is the failure mode §5.8.2 exists to remove.
- **Observability.** Gauges per `(profile, primitive)`. Note ADR-004's cardinality rule: `profile` and `provider` are bounded and allowed as labels; lock/election **names** and cache **keys** are not, and stay in trace attributes and log fields. The SDK's field classification put `profile` under the high-cardinality `fields::attr` group, which contradicted this bullet and I15; corrected in `S2` — `profile` is a `fields::label` key and is on `METRIC_LABEL_ALLOWLIST`.
- **Diagnostics.** An admin endpoint answering "who holds lock X" during an incident — something the in-process design could not answer at all. With multiple replicas this reads the *store*, not the local index, so the answer is fleet-wide rather than replica-local.
- **Quotas.** Optional per-client caps (§5.7).

**`LeaseIndexEntry` is not built, and the bullet above is why.** `S2` ships the watch-subscription half of this section and deliberately skips the lease half. Three facts in this section defeat it: nothing in the lease path may read it (I7), the diagnostics answer it exists to serve reads the **store** rather than the local index the moment there is more than one replica, and its only two consumers — `S4`'s `/admin/sessions` and §5.7's optional quotas — do not exist. What is left is a per-replica mirror of durable state, kept current by writes on the lease path, read by nobody. The scoping statement loses to its own diagnostics bullet: when `S4` lands it queries the store, and if quota accounting later needs a local counter it can carry a count rather than a mirror of every lease. Recorded rather than silently dropped.

#### 5.4.1 The watch-subscription sweep (ask `A6`, decision 18)

The specification decision 18 asks for, before phase 3. It is written here rather than as an ADR because the sweep is a mechanism internal to the serving gear — its only consumer-visible surface is one metric name — and §5.4 already owns the session index; ADRs 001–012 record decisions that reach consumers or plugin authors. Decision 18 is one of this document's own open questions, so it is answered where it was asked.

**What is swept, and what needs no sweep.** Abandoned *watch subscriptions* — entries whose client will never come back for them. Leases need no equivalent: they expire in the store at their stored `deadline` (§5.8.1), which is the whole point of I7.

**The key is "last poll", adapted to the streaming projection.** Decision 18 was written against a long-poll `await_change` and keyed the sweep off each subscription's last poll, aged by `poll_timeout × grace`. `C1`/`C2` made `await_change` `#[streaming]`, so there is no poll and no `poll_timeout` (the audit's verified-drift annex). The property the key was reaching for survives intact: it is *the last moment the client was demonstrably present for this subscription*. That is one timestamp, `last_seen`, written at three moments:

1. **`open`** — the client just joined and is expected to attach.
2. **`attach`** — the client opened (or re-opened) its stream.
3. **every sweep pass that observes a live reader** — a client holding a stream open is present, continuously, and the pass that sees it says so.

Point 3 is what makes one timestamp sufficient. Without it a long-held stream would look maximally stale the instant its reader vanished; with it, a subscription whose client dies gets a full grace window measured from the last pass rather than from a `join` an hour earlier.

**The predicate.** A subscription is reaped when it has **no live reader** *and* has been that way for at least the grace window. Three states, and only the third is garbage:

| State | Reader | Reaped? |
|---|---|---|
| Attached, client holding the stream | live | Never — `last_seen` is refreshed each pass |
| Registered by `join`, no `await_change` yet | none | After the grace window. This is the follower-pump leak |
| Attached, then the client vanished or cancelled | dropped | After the grace window, measured from the last pass that saw it live |

The third state requires the server's stream task to *notice* a departed client: parked on `recv()` it would hold its receiver alive forever and never look dead. It therefore also selects on its outbound channel closing, so a cancelled or crashed client's receiver is dropped promptly and the entry becomes reader-less. Without that, the sweep would bound the follower-pump leak and miss the original crashed-client case decision 18 was written about.

The grace window rather than immediate removal is what preserves §6.6's reconnect affordance: a client's `election_id` stays valid across a broken stream, so a re-`attach` inside the window needs no fresh `join`.

**Cadence: 5 s**, matching the plugins' lock-reaper default (`default_lock_reaper_interval()`), which is what "sharing the plugins' existing reaper cadence" can mean here — a plugin's interval is per-provider operator config that the gear cannot read, and two bound providers may disagree, so the gear carries the same *value* rather than the same knob. It is a constant, not a config key: no operator has a reason to tune the lifetime of a server-side bookkeeping entry, and a key would need its own decision.

**Grace multiplier: 3**, so the window is 15 s. It must exceed one cadence — an entry must never be reaped on the pass that first observes it reader-less — and it must be far above a `join`→`await_change` round trip. Three gives two full passes of slack and bounds the steady-state follower population at `grace / renewal_interval` ≈ 2 entries per participant on the default `ElectionConfig`, which is a constant rather than a leak.

**Metrics** (ADR-004, both labelled `(profile, primitive)` and nothing else):

- `cluster_subscriptions_reaped` — counter, incremented by each pass's reap count. Per §5.1 of the observability catalog the instrument carries no `_total`; the scraped series is `cluster_subscriptions_reaped_total`.
- `cluster_subscriptions_active` — gauge, the live population after each pass. A profile that falls to zero is reported as zero rather than going quiet, so the series does not strand at its last non-zero value.

An election **name** appears in neither, per I15; it is a log field on the reap event and a span attribute.

**What the sweep may not do.** Nothing it touches is a lease. Reaping an entry revokes no leadership and fails no renewal (I7) — the subscription is a feed, the claim is a row in the store, and the only thing that sustains the claim is the client's own renewal (I8).

**Why a sweep and not `join` reusing the caller's subscription.** The cheaper-looking fix is for `join` to hand back the caller's existing subscription for the same election instead of minting one, which would fix the *rate* at its source. It cannot be keyed safely on the identity available: with v1's `TrustedNetwork` mode every caller resolves to the single name `unauthenticated` (§4.6), so `(caller, election)` does not identify a participant — it identifies the whole fleet — and even under a real authenticator two replicas of one Deployment share a `ServiceAccount` name. Collapsing them is precisely the case the subscription id exists to tell apart. Reuse becomes available if a participant ever carries an identity of its own on `JoinRequest`; until then the sweep is both the necessary fix and the sufficient one, because it bounds abandonment (the crashed-client case reuse never addressed) and the follower-pump rate with the same mechanism.

### 5.5 Profile discovery and capability validation over the wire

`ClusterCacheBackend::consistency()`, `features()` and `provider_name()` are **sync**, so a remote backend cannot make a call inside them. This is the one piece of profile knowledge that cannot ride on the request, and `DescribeProfiles` exists to supply it: one call returning a `ProfileDescriptor` per profile plus the server `generation`.

It has **two readers on two paths**, which is what lets validation be inline in the normal case without ever blocking startup (§4.7.1):

1. **Prefetch.** The registration's wire closure spawns **one** background `DescribeProfiles` covering every `inventory`-registered profile marker (§4.9.3). One round trip per process, not per profile.
2. Results populate the process **descriptor cache**, an `Arc<OnceLock<ProfileDescriptor>>` per profile that every `Remote*Backend` for that profile reads through.
3. **`resolve()` awaits** this profile's entry, bounded by the resolve timeout — normally already populated by the prefetch, otherwise issuing its own `DescribeProfiles`. On success it validates inline; on timeout it defers to readiness.
4. A requested profile the server does not have ⇒ `ProfileNotBound` ⇒ **permanent** classification ⇒ `Err` from `resolve()` when reached inline, readiness `Unhealthy` with the same message when deferred.
5. Validation uses the same `validate_*_capabilities` code and produces the same error on either path: `CapabilityNotMet { primitive, capability, provider }`, where `provider` is the **server-side** provider (e.g. `"postgres"`), not `"remote"`. That distinction matters: the operator must see which real backend failed the requirement. The transport identity is exposed separately as a trace attribute.

6. Each descriptor also carries **per-profile health** (`ProfileHealth`, §6.7). The readiness contributor reads it for every profile the process recorded a requirement against, so a degraded profile pulls *its* consumers out of rotation without touching consumers of healthy profiles (§4.4). This is why the client re-reads descriptors on a 10 s poll rather than only on `generation` change: health moves without a configuration change, and the poll is also what returns a consumer to rotation once the backend recovers.

The prefetch is not redundant with step 3 — it is what makes step 3 a cache hit for every profile after the first, keeping resolve latency off the startup path even when a gear resolves several profiles.

Because descriptors are cached client-side, a server-side profile change can make a client's view stale; `generation` is the detector (§5.6).

### 5.6 Dynamic profile reload

Full dynamic reconfiguration is not required for the first deployable version, but the mechanisms above exist for it.

| Phase | Capability | Mechanism |
|---|---|---|
| **A** (initial) | Profiles fixed at `start`; registry runtime-*queryable* but immutable | `ArcSwap` populated once, `generation = 1` |
| **B** | **Add** a profile without restart | Build its backends (reusing shared instances), register in hub, swap a new snapshot with `generation + 1`. Purely additive |
| **C** | **Remove / rebind** a profile | Swap the snapshot first (new resolves get `ProfileNotBound`), then close that profile's subscriptions with `Closed(Shutdown)`, then drop its instance refs — the `Weak`-keyed cache runs each `StopHook` only when the last referencing profile is gone. Clients see the same terminal signal as a cluster shutdown, which `RestartingWatch` correctly treats as non-retryable |

Trigger: an explicit admin endpoint (`POST /admin/profiles/reload`) rather than a config-file watch — a watch makes reconfiguration implicit and hard to audit, and an endpoint composes with whatever config-push mechanism the platform lands on.

Client-side staleness on generation mismatch: log at `warn` with both generations and re-issue `DescribeProfiles`. Do **not** hot-swap descriptors under a live facade — a cache whose declared consistency changed under a consumer that validated `Linearizable` must not silently keep serving. Because `resolve()` records its requirements rather than checking and discarding them (§4.7.1), the refreshed descriptor is re-validated against every requirement recorded for that profile; an unmet one flips the gear to `Unhealthy` and pulls it out of rotation. This is why the requirement registry is load-bearing even though validation is normally inline: a mid-life profile rebind has no `resolve()` call to fail, so readiness is the only enforcement point available.

### 5.7 Capacity, isolation and failure modes

| Concern | Mitigation |
|---|---|
| One instance now backs the whole fleet; `pool_max_size: 5` is fleet-wide | Revised sizing guidance per profile; `cluster_backend_pool_saturation` gauge; WARN on acquire-timeouts. Document that exhaustion surfaces as `Provider{ResourceExhausted}` ⇒ retryable-with-backoff |
| One noisy client starves others on a shared instance | Phase 2: per-`ClientId` concurrent-request and open-subscription caps; over-cap ⇒ `RESOURCE_EXHAUSTED` + `Provider{ResourceExhausted}`, so well-behaved clients back off. The session index is where this lands — and the cap is therefore **per replica**, so a fleet-wide limit is `cap × replicas` unless the accounting moves into the store. At single digits of replicas that approximation is the right trade; say so in the operator docs rather than implying a global cap (§5.8.3) |
| One unreachable profile must not fail the whole pod | Readiness `Degraded`, not `Unhealthy` (§4.4); healthy profiles keep serving |
| Cluster gear is a single point of failure for coordination | **Structurally removable now** (§5.8): leases live in the backing store, so any replica serves any lease operation and a lost replica costs subscribers one `RestartingWatch` cycle, not a lock. The shipped default is still one replica pending the failover suite (§5.8.3), so until then treat the pod as a restart-tolerant SPOF: a restart loses *watch subscriptions*, never coordination state |
| Consumer up before cluster | Framework background dep resolution + readiness gating (§4.7) — no startup failure |

### 5.8 Replication — leases live in the store, not in a replica

**Decision (proposed ADR-012, `cpt-cf-clst-adr-store-owned-leases`): a lease is a record in the backing store, fenced by a token the client presents; no server-side session is required to interpret it.** Two requirements drive it. A cluster replica must be replaceable — restarted, upgraded, rescheduled — without revoking the fleet's locks (§5.8.2), and coordination must scale past one process. Holding lease state in the replica that issued it satisfies neither: it makes every deploy a revocation event and every second replica a correctness hazard.

This is the shape the industry converged on:

| System | Where the lease lives | Who serves a renew |
|---|---|---|
| etcd | Lease ID in the raft log | Any member |
| Consul | Session ID in Consul's own state | Any server |
| Kubernetes | A `Lease` object: `holderIdentity` + `renewTime` | Any apiserver replica |
| **Cluster (this design)** | The `cluster_lock` / `cluster_cache` row: owner identity, deadline, fence | **Any replica** |

#### 5.8.1 The model

Every lease-bearing operation becomes a **conditional write predicated on state the store already holds**, so the replica handling the request needs no memory of the one that issued it.

| Element | Definition |
|---|---|
| **Lease record** | Lives in the backend row: `owner` (the `ClientId` of §5.4), `deadline` (absolute, server-clock), `fence` (per lease name, monotonic within the retention window below) |
| **Lease token** | What the client holds and presents: `(name, owner, fence)`, opaque and server-issued. It is the whole of the authority — there is no lookup table behind it |
| `renew(token, ttl)` | `UPDATE … SET deadline = now() + ttl WHERE name = $1 AND owner = $2 AND fence = $3 AND deadline > now()`. Zero rows ⇒ `LockExpired` — and that answer is identical on every replica, because it is a property of the row |
| `release(token)` | Same predicate, `DELETE`. Absence ⇒ `Ok` (idempotent by absence, §6.10) |
| `acquire(name, ttl)` | Insert-or-steal-if-expired, bumping `fence`. A stolen lease strictly increases the fence, so a stale holder's `renew` can never match again |
| **Liveness** | The client's own `renew` cadence. A holder that stops renewing lapses at `deadline` — the TTL safety net the contract already specifies (`cpt-cf-clst-fr-lock-release`) |

Three properties follow, and they are the whole justification:

- **Any replica serves any request.** Nothing is affine, so a `ClusterIP` Service round-robining across replicas is correct rather than dangerous — the constraint that previously ruled out a headless Service disappears with it (§4.10, §7.1).
- **A cluster restart revokes nothing.** No process vouches for a lease, so no process's death invalidates one. This is the fix for §5.8.2.
- **Stale holders are fenced, not trusted.** The `fence` is what makes "steal on expiry" safe: the previous holder's operations fail their predicate rather than silently succeeding against a lease someone else now owns.

**Where the fence comes from, and what it actually guarantees.** The fence must not restart while any stale token bearing the old value could still be presented, and the cache-backed defaults do not get that for free: `CacheEntry.version` is monotonic *per key while the key exists* (cluster DESIGN.md §2.2), and the TTL reaper deletes expired keys — the standalone plugin then writes `version: 1` on the next insert (`standalone-cluster-plugin/src/cache.rs:362`). A lease that lapses, is reaped, and is re-acquired by the **same** `ClientId` would otherwise hand the old token a matching predicate again.

The lease record therefore carries its own fence and outlives its lease:

| Rule | Detail |
|---|---|
| **The fence lives in the value, not in the store's version** | The lease record is `{ owner, deadline, fence }` serialised into the cache value (or the native lock row). `version`/`xmin` still drives the CAS, but authority is the `fence` field |
| **Acquisition preserves and increments** | Taking a lease whose `deadline` has passed reads the existing record and writes `fence + 1`. The record is CAS'd, not deleted-then-inserted, so the counter survives the change of owner |
| **The record outlives the lease** | Its physical expiry is `deadline + fence_retention`, not `deadline`. The reaper may only remove a record once the retention window has also passed, which is what keeps the counter alive across a lapse. `fence_retention` defaults to an hour — orders of magnitude above any lease TTL, and cheap: one small row per lease *name*, not per acquisition |

**The guarantee this buys, stated exactly**: a fence value is never reused for a given lease name **within `fence_retention` of its lease lapsing**. Beyond that window the counter may restart, and reuse then additionally requires the same `ClientId` to re-acquire and a token that has been stale for longer than the retention window — a holder whose renew cadence is a fraction of its TTL learns it lost the lease orders of magnitude sooner.

Two corrections to what this paragraph originally claimed, both made when `L3` implemented it:

- **"Its lease *ending*" is narrowed to "its lease *lapsing*".** `release` deletes the record, by the ruling in ADR-012, so a voluntary release drops the fence with it. Lapsing is the case a stale holder can be in.
- **"The backends reject it at startup" cannot be checked against the longest lease TTL**, because there is no configured lease TTL to compare against: a TTL is a per-call argument to `lock(name, ttl)` and to an election claim. What the backends do reject at startup is a **zero** window — the absence of retention rather than a short one. The `ttl >= fence_retention` case is caught where a TTL exists, at acquisition, as a `warn!` naming both durations (once per backend). Denying coordination service for a config the caller did not set would be the worse failure.

**This is why the fence stays internal.** Exposing it as `LockGuard::fence()` for external fencing — passing a monotonic token to a third-party resource so it can reject stale writers — would promise *global* monotonicity for the lifetime of the protected resource, which no backend here can honour across the retention window. The fence is scoped to making cluster's own lease predicates safe. External fencing, if a consumer needs it, is an additive `LockGuard` method backed by a source that can actually promise it (a Postgres sequence, say), and it needs its own ADR (§9).

**The session registry is an index, not an owner.** It carries diagnostics ("who holds lock X"), quota accounting and watch-subscription bookkeeping (§5.4). Nothing in the lease path reads it, so a lost or stale entry costs a reporting artefact rather than a lease.

**What is still replica-affine, and why it is acceptable.** Watch subscriptions and long-poll cursors live on the replica serving them. A replica going away closes those streams, the client sees `Closed(Provider{ConnectionLost})`, and `RestartingWatch` re-subscribes — possibly against a different replica, which is fine because a subscription carries no lease authority. So **failover costs a re-subscribe, never a lost lock**. That is the property being bought, and it is worth stating that leader *election* rides the same rails: the lease is store-owned, so a leader survives the replica it was elected through.

**Renewal runs at the client's cadence**, because the token it holds is the authority. That keeps renewal doing double duty as a consumer-liveness signal — a wedged holder stops renewing and loses its claim — which is the property §7.3 turns on.

#### 5.8.2 Restarts and upgrades revoke nothing

**A cluster restart is not a lease event.** Rolling the pod, upgrading it, or losing it to a node failure leaves every held lock and every leader claim exactly where it was: the lease is a row, its deadline is stored, and its holder keeps renewing against whichever replica answers next. Consumers observe a dropped subscription and one `RestartingWatch` cycle, nothing more.

**The deadline is the only liveness authority, in every profile.** A lease ends when its holder stops renewing and `deadline` passes, or when the holder releases it. No process vouches for a lease — not the holder's, not the broker's — so no process's death can end one early.

This retires the **liveness beacon** ([PR #4411](https://github.com/constructorfabric/gears-rust/pull/4411), merged, tier 1 per §2.0), which is the one piece of the shipped Postgres lock that assumed otherwise. Today a row is stealable when

```sql
CASE WHEN expires_at <= now() THEN true
     ELSE NOT EXISTS (SELECT 1 FROM pg_locks
                       WHERE locktype = 'advisory' AND objsubid = 2 AND granted
                         AND classid = cluster_lock.holder_beacon_hi::oid
                         AND objid   = cluster_lock.holder_beacon_lo::oid)
END
```

— an advisory key drawn fresh per incarnation, held on a dedicated connection, whose disappearance from `pg_locks` publishes "the process that took this lock is gone" (`lock/mod.rs:200-210`, `lock/beacon.rs`). It buys **sub-TTL reclaim** of a crashed holder's locks, and it is sound precisely when the process holding the beacon is the process using the lock.

Brokered, that stops being true: cluster's beacon would vouch for locks held by other, live consumers, so its restart would revoke the fleet's locks. The predicate therefore has to change for remote clients — and **it changes for everyone**:

| | Behaviour |
|---|---|
| **Predicate** | `expires_at <= now()`. One branch, no `pg_locks` scan, no beacon columns, no dedicated connection |
| **Crash recovery** | Bounded by the lease TTL the holder chose, in Profile 1 and Profile 3 alike |
| **Shutdown drain** | Removed. Deleting rows on the way out is the revocation this section exists to prevent |

**Uniformity is the reason, and it is worth the cost.** Keeping the beacon for in-process acquisitions and dropping it for brokered ones would mean one deployment reclaims a dead holder's lock in milliseconds and another waits out the TTL — the same code, the same config, two timings, and a class of bug that only reproduces in one profile. Cluster's whole premise is that a consumer behaves identically wherever it runs (Goal 2); lease expiry is not the place to make an exception. The price is explicit: **a crashed holder's lock now lingers until its TTL in every profile**, where the embedded path previously reclaimed it early. That is the same bound every non-Postgres backend already has, and it is a reason to keep lock TTLs tight rather than to keep two mechanisms.

Two things fall out of the removal that are worth stating so they are not rediscovered as gaps:

- **The orphan sweep and post-reconnect cleanup lose their key** — both used "rows bearing this incarnation's beacon" as their filter. Neither is needed: an expired row is reclaimed by the existing TTL reaper regardless of who wrote it, and a row is no longer tied to the connection that created it.
- **The migration is a column drop.** `holder_beacon_hi` / `holder_beacon_lo` and the `cluster_lock_beacon_nonneg_check` constraint go with the predicate (`0002_cluster_lock.sql`), and the acquire path loses its only unindexed scan — a small win on contended acquires (§7.2.4).

#### 5.8.3 Replica count: the model now, the count after conformance

**`replicaCount: 1` is a shipped default, not a constraint.** Nothing in §5.8.1 breaks at two replicas — but "correct in principle" is not "the failover suite passes", and that suite is phase 6 work.

| | Before conformance (phase 5) | After (phase 6, §7.6) |
|---|---|---|
| Helm | `replicaCount: 1`, `strategy: RollingUpdate` — an upgrade is an ordinary pod replacement, not a coordination event | `replicaCount: 2+`, `maxUnavailable: 0`, PodDisruptionBudget |
| Service | `ClusterIP` — now a genuine load balancer rather than a pin | Unchanged; headless is also safe, just unnecessary |
| Enforcement | None. Replica count is a capacity decision, so there is nothing to enforce in Helm, at startup, or anywhere else | — |

The gate is empirical, and §7.6 names the tests: a renew issued against a *different* replica than the acquire succeeds; a rolling upgrade under held locks revokes nothing; a killed replica costs subscribers one `RestartingWatch` cycle and no lease; a stolen-after-expiry lease fences its predecessor.

## 6. API Surface

### 6.1 Contract-first, three projections

The platform mandates a contract-first pipeline: a `#[toolkit::contract]` Rust trait is the single source of truth, emitting a transport-neutral `ContractIr`; `#[toolkit::rest_contract]` and `#[toolkit::grpc_contract]` project it; `toolkit-contract-protogen` generates the `.proto` from the IR plus schemars schemas, with `proto.lock.toml` guaranteeing wire-stable field numbers. `openapi.json` is a published *output*, never a codegen input.

**Cluster follows this directly. The crates are shipped** — [PR #4084](https://github.com/constructorfabric/gears-rust/pull/4084) merged `toolkit-contract`, `toolkit-contract-macros` and `toolkit-contract-protogen` to `main` (§2.0), so there is no sequencing problem left to manage and nothing to migrate later.

**Define the contract traits directly against the merged macros** — the shapes are readable from the tree, and `examples/toolkit/api-contracts` is a working reference:

- one trait per primitive, named with the `Api` suffix — the suffix is what classifies the contract kind, and it must not collide with the plugin-facing `*Backend` traits (§6.2);
- **a security-plane context as the first parameter of every method** (§6.2) — the documented constraint, hard-enforced on the REST projection and to be followed on gRPC regardless;
- `#[idempotency(..)]` on every method, `#[streaming]` on the push-shaped ones (declared as `fn`, not `async fn`, returning the per-message type — the same shape as the example's `list_payments`);
- a `#[toolkit::grpc_contract]` projection trait with `PrimitiveApi` as its supertrait, carrying `#[rpc(name = "…")]`, `#[idempotency_level(..)]` and `#[retryable]` (§6.10);
- DTOs deriving serde + schemars with `ProtoBridge` conversions; errors via `#[derive(ContractError)]` (§6.9).

**What is generated, and what is not.** The split matters for effort estimates and for reading `cluster/src/api/grpc/` correctly:

| Artefact | Generated? | Where |
|---|---|---|
| `.proto` from the contract IR | Yes — `toolkit-contract-protogen` + `proto.lock.toml` | `cluster-sdk/proto/` |
| prost messages, `*_client` **and** `*_server` traits | Yes — `tonic-prost-build` via `build.rs` | `cluster-sdk/src/grpc.rs` (`stubs`) |
| The consumer-side client implementing the contract trait | Yes — `#[toolkit::grpc_contract]` | `cluster-sdk/src/grpc.rs` |
| REST routes, when a REST projection exists | Yes — `rest-server`, with `#[server_manual]` to opt a method out | n/a under §2.2.1 option A |
| **The gRPC service impls** | **No — hand-written, by design** | `cluster/src/api/grpc/` |

**gRPC server codegen is explicitly out of scope** for the platform's contract pipeline: #4084's own example states it (*"Server codegen is explicitly out-of-scope per PRD ADR-0002 — this is the supported escape hatch for service authors"*) and hand-writes `PaymentApiGrpcService` against the generated `payment_api_server` trait. So cluster's five service impls are the **sanctioned permanent pattern**, not interim glue awaiting a codegen that will never arrive. Each does the same four steps as the example: proto → DTO, resolve the caller identity from metadata (platform plane, §4.6), dispatch, and map `ClusterError` → `Status` (§6.9). Cluster's one addition is profile dispatch through the `ProfileRegistry` between steps 2 and 3 (§5.2).

**Nothing is hand-rolled, and nothing is migrated later.** Earlier drafts of this section planned a hand-written
`.proto`, a hand-rolled client and a hand-written error codec, to be reconciled with protogen's output once #4084
landed. It has landed, so that plan is withdrawn along with the wire-compatibility risk that was its main cost: the
`.proto` is **generated from the IR** and `proto.lock.toml` is committed from the first commit, so field numbers are
pinned before anything deploys rather than reconciled after.

What that removes from the build, concretely:

| Artefact | Source |
|---|---|
| `.proto` + `proto.lock.toml` | `toolkit-contract-protogen` from the contract IR |
| prost messages, `*_client` / `*_server` | `tonic-prost-build` via `build.rs` |
| The client behind the `*Api` traits | `#[toolkit::grpc_contract]` |
| `ClusterError` ⇄ `CanonicalError` | `#[derive(ContractError)]` (`toolkit-contract-macros/src/contract_error.rs`); `toolkit-canonical-errors` and `#[resource_error]` were already on `main` |
| `#[idempotency(..)]` / `#[streaming]` | Macro-enforced rather than documentary — so §6.10's retry rules are checked by the toolchain, not only by tests |
| `ConsumerRegistration` + the proxy-wiring phase | `libs/toolkit/src/discovery.rs` + `host_runtime.rs:423`. `cluster-sdk/src/wiring.rs` ships enabled, not behind a dead cfg |

**Two disciplines still apply**, because they are what keeps the generated artefacts behind the `*Api` trait
boundary: write the security-context first parameter into every trait method even though the gRPC projection does not
yet enforce it, and follow protogen's naming conventions rather than inventing better ones. The gear's four service
impls stay hand-written either way — that is the sanctioned pattern, not interim glue — so a contract change should
never reach `cluster/src/api/grpc/`. If it does, the trait boundary leaked, and that is the finding.

The risk is not "will the IR express what cluster needs" — with no bidirectional streaming required (§6.2), cluster's contract fits the documented IR, and the example proves the streaming and error paths work. It is **API drift while #4084 is in review**, which is bounded and is managed by reviewing cluster's contract shape *with* the `toolkit-contract` owners before phase 1 starts. Two specifics belong in that review, because both change cluster's traits rather than merely confirming them:

- **Is the gRPC projection's missing first-argument security-context check deliberate?** `#[rest_contract]` errors at parse time; `#[grpc_contract]` only type-asserts the parameters it finds (§6.2). Cluster follows the documented constraint either way, but if the gap is an oversight the check will land later and must not then reject a contract written to the current behaviour.
- **Will the `Api` / `Backend` suffix classification stay as it is?** `is_remote_capable()` returns true for both suffixes (`model.rs:15-45`), and cluster's plugin-facing traits are `ClusterCacheBackend` and siblings. If that classification ever grows teeth, a security-context parameter lands on the trait every plugin implements, breaking `cpt-cf-clst-nfr-plugin-stability` for no benefit — so the two-trait split needs to be understood as load-bearing by both sides (§6.2, ADR-011).

### 6.2 The contract traits

The wire mirrors the **backend traits**, not the facades: the facades' sync/local concerns (`resolver`, `scoped`, `status()`, `is_leader()`, `auto_restart`) stay client-side. Four contracts:

```rust
#[toolkit::contract(gear = "cluster", version = "v1")]
pub trait ClusterCacheApi: Send + Sync {
    #[idempotency(SafeRead)]
    async fn get(&self, ctx: &PlatformSecurityContext, req: GetRequest)
        -> Result<GetResponse, CanonicalError>;
    #[idempotency(IdempotentWrite)]
    async fn put(&self, ctx: &PlatformSecurityContext, req: PutRequest)
        -> Result<(), CanonicalError>;
    #[idempotency(NonIdempotentWrite)]
    async fn put_if_absent(&self, ctx: &PlatformSecurityContext, req: PutRequest)
        -> Result<PutIfAbsentResponse, CanonicalError>;
    #[idempotency(NonIdempotentWrite)]
    async fn compare_and_swap(&self, ctx: &PlatformSecurityContext, req: CasRequest)
        -> Result<CacheEntryDto, CanonicalError>;
    // … delete, contains, compare_and_delete, scan_prefix
    #[idempotency(SafeRead)] #[streaming]
    fn watch(&self, ctx: &PlatformSecurityContext, req: WatchRequest)
        -> Result<CacheWatchEventDto, CanonicalError>;
    #[idempotency(SafeRead)] #[streaming]
    fn watch_prefix(&self, ctx: &PlatformSecurityContext, req: WatchPrefixRequest)
        -> Result<CacheWatchEventDto, CanonicalError>;
}
```

The lease-bearing traits are worth showing too, because their shape is the least obvious part of the contract — every operation after the acquire is predicated on a token rather than addressed by a handle (§5.8.1):

```rust
#[toolkit::contract(gear = "cluster", version = "v1")]
pub trait DistributedLockApi: Send + Sync {
    /// Insert-or-steal-if-expired, bumping `fence`. Not retryable: a lost
    /// response on a successful acquire would come back as `LockContended`
    /// against the caller's own lease (§6.10).
    #[idempotency(NonIdempotentWrite)]
    async fn try_lock(&self, ctx: &PlatformSecurityContext, req: TryLockRequest)
        -> Result<LockAcquired, CanonicalError>;

    /// Same, but the server waits up to `timeout` before giving up.
    #[idempotency(NonIdempotentWrite)]
    async fn lock(&self, ctx: &PlatformSecurityContext, req: LockRequest)
        -> Result<LockAcquired, CanonicalError>;

    /// Conditional write on `(name, owner, fence, deadline > now())`. Zero rows
    /// ⇒ `Aborted`, which the client reconstructs as `LockExpired` (§6.9).
    /// Any replica answers identically — there is no session to look up.
    #[idempotency(IdempotentWrite)]
    async fn renew(&self, ctx: &PlatformSecurityContext, req: LeaseRef)
        -> Result<(), CanonicalError>;

    /// Same predicate, delete. Idempotent by absence: a token that matches
    /// nothing has already achieved what the caller wanted (§6.10).
    #[idempotency(IdempotentWrite)]
    async fn release(&self, ctx: &PlatformSecurityContext, req: LeaseRef)
        -> Result<(), CanonicalError>;
}
```

**`LeaderElectionApi` is the same shape plus a subscription**: `join` returns `{ lease_token, election_id, initial_status }`, `renew` and `resign` take a `LeaseRef` exactly as above, and `await_change` is keyed by the `election_id` — the one piece of replica-local state, because it addresses a *subscription* rather than a lease (§5.8.1, §6.6). `ClusterProfileApi` carries `describe_profiles` alone.

The error type is the platform's `CanonicalError` throughout (§6.9); the `Remote*Backend` impls translate it back into `ClusterError` so the consumer-facing contract is unchanged.

> **The security context is a required parameter on the *contract* traits.** PR #4084's
> `cpt-cf-binding-constraint-security-context` requires a security-plane context as the first non-`self` argument of
> every method on a remote-capable contract — tenant `SecurityContext` or platform `PlatformSecurityContext`, by
> value or reference. Cluster takes the **platform-plane** form, which is also what selects platform-plane treatment
> and what makes `#[rest_contract]` reject these traits (§2.2.1).
>
> **Where it is enforced, precisely** — this differs by projection, and the difference is worth knowing before phase 1:
>
> | Macro | Enforcement (verified against #4084) |
> |---|---|
> | `#[toolkit::contract]` alone | **None.** The requirement lives in the projection macros; an unprojected contract trait is never checked |
> | `#[toolkit::rest_contract]` | **Hard parse-time error** (`rest_contract_parse.rs:272-300`, `ui/fail/rest_missing_secctx.stderr`), plus the separate platform-plane rejection of §2.2.1 |
> | `#[toolkit::grpc_contract]` | **A type assertion, not a presence check.** `generate_repr_guards` (`grpc_contract.rs:132-155`) emits `assert_security_context::<T>()` for each parameter whose type path *ends with* `SecurityContext`, catching a wire DTO misnamed as a context (`ui/fail/grpc_fake_secctx`). No "must take one first" check was found on this path |
>
> So on cluster's chosen projection (gRPC, option A) the constraint is **stated but apparently unenforced**. Follow it
> anyway: it is the documented contract, the REST path enforces it, and a contract that quietly omits the context
> would have to be re-signed the moment either the enforcement is tightened or a REST projection is wanted. Whether
> the gRPC gap is deliberate is one of the two items in the pre-phase-1 contract review (§6.1).
>
> The parameter costs nothing on the wire: it carries the IR `FieldRole` that filters it out of the generated schema,
> so the credential still travels in gRPC metadata and resolves server-side exactly as §4.6 describes. It is a
> signature requirement, not a payload one. A `#[secctx]` / `#[security_context]` parameter attribute is available as
> an explicit alternative to the `ctx:`-name heuristic (`ui/pass/contract_secctx_attr.rs`) if a different parameter
> name is wanted.

> **Scope of the requirement — it stops at the contract traits.** Nothing above the seam acquires a context
> parameter. The three facades (`ClusterCacheV1` and siblings) are not contracts and never become contracts; the
> `*Api` traits here are new types gated behind `cluster-sdk/grpc-client` that no consumer names. That is the §3.1 seam doing its
> job, and it is why Goal 2 survives a mandatory-context contract layer unscathed.
>
> **A naming trap to avoid, though.** `ContractKind::from_suffix` classifies a contract by its trait-name suffix —
> `Api`, `Embedded`, `Backend`, `Extension` — and `is_remote_capable()` returns true for **`Api` *and* `Backend`**
> (`model.rs:15-45`). Cluster's plugin-facing traits are named `ClusterCacheBackend`, `DistributedLockBackend`
> and `LeaderElectionBackend`. Annotating *those* with `#[toolkit::contract]` plus a
> projection — rather than the separate `*Api` traits — would therefore classify them remote-capable and push a
> security-context parameter onto the trait **every plugin implements**, breaking `cpt-cf-clst-nfr-plugin-stability`
> for no benefit. The two-trait split (`*Backend` stays local and serde-free, `*Api` carries the wire contract) is
> load-bearing, not stylistic; say so in the ADR-011 write-up.

Every method above is either unary or single-return-`#[streaming]`, both of which the platform's contract shape already expresses — so **cluster needs no IR extension**. The three push-shaped operations (`cache.watch`, `cache.watch_prefix`, `leader.await_change`) are server-push only; the handle-bearing flows are unary against a store-owned lease (§6.5–6.6). The pre-phase-1 contract review (§6.1) is therefore a sanity check on DTO and error shapes plus the two enforcement questions, not a request for new IR capability.

### 6.3 Complete facade → wire mapping

Every public method on the three facades and two handle types. "Client-local" means no remote call — the behaviour lives above the seam, unchanged.

**`ClusterCacheV1`**

| Method | Wire |
|---|---|
| `resolver(hub)` … `.resolve().await` | Client-local hub lookup, plus at most one `DescribeProfiles` for the descriptor if the prefetch has not landed (§5.5). A cache hit after the first profile |
| `consistency()` / `features()` | Client-local, from the descriptor (§5.5) |
| `scoped(prefix)` | Client-local (`ScopedCacheBackend` above the remote backend) |
| `get` / `put` / `delete` / `contains` / `put_if_absent` / `compare_and_swap` / `scan_prefix` | Unary, one contract method each |
| `watch(key)` / `watch_prefix(prefix)` | Server-streaming |
| `watch_prefix_polling(..)` | Client-local (`PollingPrefixWatch` over remote `scan_prefix` + `get`) |
| *(backend-only)* `compare_and_delete` | Unary — **must** be on the wire: the trait's default impl is a non-atomic `get`-then-`delete`, a genuine race over a network, and it is what the CAS-based leader release depends on |
| `CacheWatch::auto_restart(policy)` | Client-local (`RestartingWatch`) |

**`LeaderElectionV1`**

| Method | Wire |
|---|---|
| `resolver` / `scoped` | Client-local |
| `elect` / `elect_with_config` | **Unary** `Join` → `{ lease_token, election_id, initial_status }`, then a watch subscription (§6.6); returns a `LeaderWatch` from `LeaderWatch::channel(..)` |
| `LeaderWatch::changed()` | **The one push-shaped operation here** — stream/SSE or long-poll `AwaitChange` (§6.6, §6.8) |
| `LeaderWatch::status()` / `is_leader()` | Client-local cached snapshot, as today |
| `LeaderWatch::resign()` | **Unary** `Resign { lease_token }`, answered through the existing `ResignReceiver`/`ResignResponder` |
| `LeaderWatch::run_while_leader(..)` | Client-local helper |

**`DistributedLockV1`**

| Method | Wire |
|---|---|
| `resolver` / `scoped` | Client-local |
| `try_lock(name, ttl)` | **Unary** `TryLock` → `{ lease_token }` |
| `lock(name, ttl, timeout)` | **Unary** `Lock` (server waits up to `timeout`) |
| `LockGuard::name()` | Client-local |
| `LockGuard::renew` / `release` | **Unary** `Renew { lease_token }` / `Release { lease_token }` |

Nothing consumer-facing is unreachable, and nothing new is exposed to consumers. Note the shape of this table: of the operations across the three facades, **only three are push-shaped** — `cache.watch`, `cache.watch_prefix` and `LeaderWatch::changed()`. Everything else is either client-local or a plain unary call.

### 6.4 Cache contract

Semantics are inherited verbatim from cluster DESIGN §3.3 — this is a transport, not a redefinition. Every request carries `profile`.

One deliberate divergence from the trait: **`scan_prefix` is paginated** on the wire (`page_token`, `limit`, `next_page_token`) where the in-process trait returns an unbounded `Vec<String>`. A prefix scan over a large keyspace must not build one giant message; the `RemoteCacheBackend` loops pages and presents the flat `Vec` the trait requires, and the server enforces a max page size.

Worth flagging for a follow-up: a batched `MultiGet` would collapse the polyfill's N+1 into one round trip. It is deferred with the rest of the compound-operation question (§7.2.3), and prefix polling showing up in a real profile is what would justify revisiting it.

### 6.5 Lock contract — unary against a store-owned lease

**The lock primitive needs no streaming.** Its entire surface is four request/response operations, and the client-death case is covered by the contract's TTL safety net (`cpt-cf-clst-fr-lock-release`: "If a consumer panics, crashes, or forgets to release, the backend's TTL bounds the leak").

| Facade method | RPC | Notes |
|---|---|---|
| `try_lock(name, ttl)` | Unary `TryLock { profile, name, ttl }` → `{ lease_token }` | `LockContended` if held. `LeaseToken { name, owner, fence }` (§5.8.1, §12.1) is the whole authority, not a handle into a table. It stays inside the backend — no consumer-facing accessor (§5.8.1) |
| `lock(name, ttl, timeout)` | Unary `Lock { profile, name, ttl, timeout }` → `{ lease_token }` | The server does the waiting; the call simply takes up to `timeout` to return. `LockTimeout` on expiry, `Shutdown` if revoked mid-wait |
| `LockGuard::renew(new_ttl)` | Unary `Renew { lease_token, ttl }` | Conditional write on `(name, owner, fence, deadline > now())`; zero rows ⇒ `LockExpired`. **Any replica gives the same answer** (§5.8.1) |
| `LockGuard::release()` | Unary `Release { lease_token }` | Same predicate, delete. Absence ⇒ `Ok` (§6.10) |

Server side: **no lease table.** The lease is the backend row; the gear translates a token into a predicate and executes it, which is what lets a `renew` land on a replica that never saw the acquire (§5.8.1). The `LeaseIndexEntry` of §5.4 is written for diagnostics and read by nothing in this path. Client side: `RemoteLockBackend::try_lock` makes the call, then builds the guard through `LockGuard::channel(name, 1)` and spawns a pump that translates `LockCommand::Renew`/`Release` into unary RPCs (§12.11).

> **The `LockGuard::channel(..)` seam is load-bearing here, not merely convenient.** `LockGuard` is `{ name: String, commands: mpsc::Sender<LockCommand> }` — both fields private, one public constructor — so **the guard cannot carry the `lock_id`**. The id lives in the pump task's closure instead. That works without any SDK change, and it keeps `renew`/`release` returning the backend's real result as they do in-process, but it means one client-side task per held lock (§7.2.5) and it rules out the "guard carrying the lock_id" shape this section previously described.

Consequences of the unary shape:

- **Liveness is TTL, exactly as specified.** With no stream there is no stream-close signal, so a crashed client's lock lapses at TTL. That is the contract's designed mechanism (`cpt-cf-clst-fr-shutdown-ttl-cleanup`), and matching it keeps Profile 1 and Profile 3 behaviourally identical.
- **`Drop` stays a no-op with no I/O.** Dropping the client guard drops a token. ADR-002's constraint holds trivially.
- **Blocking `lock()` is just a slow unary call**, and client disconnect during the wait is handled by ordinary HTTP/gRPC cancellation — the server abandons the wait.
- **No session reaping machinery for locks, and no server-side ownership table to sweep** (§5.4). A stale holder is fenced by the monotonic `fence`, not remembered.

### 6.6 Leader-election contract — unary join/resign plus one watch

Leader election is *not* a streaming API either. It is three unary operations and one subscription:

| Facade method | RPC |
|---|---|
| `elect(name)` / `elect_with_config(name, cfg)` | Unary `Join { profile, name, optional ElectionConfig }` → `{ lease_token, election_id, initial_status }` — the token carries leadership, the `election_id` addresses the *subscription* |
| *(no facade method)* | Unary `Renew { lease_token, ttl }`, issued by the client's pump on a `renewal_interval()` cadence — **the operation that holds leadership** (§7.3). Absent from the facade because it is the backend's job in both profiles; what changes remotely is that it crosses the wire |
| `LeaderWatch::resign()` | Unary `Resign { lease_token }` — a conditional delete on the election's lease record, servable by any replica |
| `LeaderWatch::status()` / `is_leader()` | Client-local cached snapshot — no call |
| `LeaderWatch::changed()` | **The only push-shaped part**: awaits the next `Status` / `Lagged` / `Reset` / `Closed` |

So the streaming question reduces to `changed()`, which is a watch like any other — the type is called `LeaderWatch` and its events use the same union shape as the cache watch. Two projections satisfy it (§6.8):

- **Server-stream / SSE**: `Subscribe { election_id }` → stream of `LeaderWatchEventDto`.
- **Long-poll**: unary `AwaitChange { election_id, since_cursor, timeout }` returning the next event or "no change" — which is precisely `changed()`'s contract, and returns immediately when a transition occurs, so failover-detection latency is unaffected.

> **At most one in-flight `AwaitChange` per `election_id`.** `LeaderWatch::changed()` takes `&mut self` because the watch owns an `mpsc::Receiver` (`leader/watch.rs`), so the server's session must hold it behind a `Mutex` and cannot serve two concurrent polls for one election. A second concurrent poll is rejected with `FailedPrecondition` rather than serialised: serialising would hand one of the two callers a stale event, and a duplicate poll for one election is a client bug in any case. The client-side pump issues exactly one at a time (§12.12).

**Renewal is client-driven**, against the lease token (§5.8.1). A wedged consumer whose renewals stop must lose its claim, and only client-driven renewal makes that true — renewing in the gear on the consumer's behalf would keep a dead consumer elected (§7.3).

**A failed renewal is a status change, not a terminal close.** Zero rows matched means this instance no longer holds the lease — stolen after expiry, or fenced by a newer holder — which is exactly the in-process meaning of losing leadership. The pump emits `Status(Lost)` and **keeps the subscription open**, because the election continues and this instance may win it again; only a `Closed` from the server or an unrecoverable transport failure terminates the watch (§6.8, §6.9). The subscription is then purely an observation channel — losing it costs a re-subscribe, not a leadership change.

Client side: `LeaderWatch::channel(..)` yields `(LeaderWatchSender, ResignReceiver, LeaderWatch)`; a pump feeds inbound events into the sender (from either projection) and outbound resigns from the receiver. `status()`/`is_leader()` keep reading the locally cached snapshot.

### 6.7 Profile / admin contract

| Operation | Transport | Purpose |
|---|---|---|
| `DescribeProfiles` | gRPC, once per process after wiring, then re-issued on `generation` change and on a 10 s poll | §5.5 — supplies the sync accessors' descriptors and gates readiness; `resolve()` awaits only this profile's entry, bounded (§4.7.1). Each descriptor carries **per-profile health** (`ProfileHealth`), which is what lets a consumer of a degraded profile leave rotation while the pod itself stays `Degraded`/ready (§4.4) |
| `GET /admin/profiles` | REST | Operator-facing profile inventory incl. shared-instance mapping |
| `GET /admin/sessions` | REST | "Who holds lock X" during an incident |
| `POST /admin/profiles/reload` | REST | §5.6 phase B/C |

Admin endpoints are REST because they are operator surface, where `curl`-debuggability is the point — exactly ADR-0002's argument. They need an authorization decision distinct from the data plane: `/admin/sessions` reveals lock and election names across all clients (§7.1).

They are also **hand-written `OperationBuilder` routes, not a `#[rest_contract]` projection** — which sidesteps the platform-plane REST restriction of §2.2.1 entirely, since that restriction is about *generated clients* sourcing an internal token, and these endpoints have no generated client at all.

Their visibility follows the two-axis model PR #4403 establishes (Goal 6): build them with `.authenticated()` and **without** `.exposed()`, so `OperationSpec.is_public` stays `false`. That is what keeps them off the gateway, and it now happens by omission rather than by cluster doing anything — under #4403 route exposure is directory-driven, with the api-gateway edge polling `ListAllInstances`, fetching specs via `GetOpenApiSpec` and registering proxy routes itself. A gear never calls `GatewayProvider`, so "cluster does not register with the gateway" is a property of the spec it publishes, not of a call it declines to make.

### 6.8 Watch stream semantics

The union shape (ADR-003) maps directly; the wire adds no new states.

| Rust variant | Produced when |
|---|---|
| `Event(..)` / `Status(..)` / `Change(..)` | Ordinary value event |
| `Lagged { dropped }` | The server's per-stream send buffer overflowed (slow client) **or** the server's own upstream watch reported lag. Both collapse to the same consumer-visible signal, which is correct: "you missed events, re-read" |
| `Reset` | The server's upstream subscription was re-established |
| `Closed(err)` | Terminal; server sends it, then closes the stream |

**Two projections, identical semantics.** The watch contract does not require a stream; it requires ordered server-push with a lag signal. Both projections satisfy it, and the choice is per §2.2.1:

| | Streaming (gRPC server-stream, or SSE) | Long-poll |
|---|---|---|
| Shape | `Subscribe { target }` → stream of events | Unary `AwaitChange { subscription_id, since_cursor, timeout }` → next event batch or "no change" |
| Server-side state | Per-stream buffer | Per-subscription buffer + cursor |
| `Lagged` | Buffer overflow | Cursor older than the retained buffer |
| Unsubscribe | Cancel the stream | `Unsubscribe`, or subscription TTL expiry when polling stops |
| Cost per event | Amortised over one connection | One request per batch — churn scales with event rate |
| Client liveness | Needs an explicit signal (§7.3) | The poll *is* the signal |

Long-poll is not a downgrade for low-event-rate watches (leader status) and is measurably worse only for high-rate cache-prefix watches. Note that a held long-poll request and an open stream cost about the same server-side — one connection either way.

Rules, common to both:

- **Bounded per-subscription buffer, drop-then-`Lagged`.** Never block the backend's fan-out on a slow remote client — one wedged consumer must not stall a shared watch for everyone. Mirrors the existing `try_send` + `Lagged` behaviour of `CacheWatchSender`/`ServiceWatchSender`.
- **Per-key ordering preserved** within a stream (`cpt-cf-clst-nfr-watch-delivery`): HTTP/2 delivers in order per stream, and one watch is one stream.
- **At-most-once** unchanged; the wire adds no retransmission.
- **Client cancel = unsubscribe**, equivalent to in-process `Drop`.
- **Transient backend errors are not events** — retried inside the server's watch task, per ADR-003. Transient *transport* errors are different: a broken stream surfaces as `Closed(Provider{ConnectionLost})`, which `RestartingWatch` classifies as retryable and transparently resubscribes. The auto-restart combinator, built for in-process backends, turns out to be the load-bearing piece of the remote story and gets its first real workout here.

### 6.9 Error model over the wire

**Decided: the wire form is `CanonicalError`** (`toolkit-canonical-errors`), not a bespoke cluster DTO.

Cluster defines no bespoke error DTO and no hand-written code-mapping table. The platform already ships a typed cross-process error model whose variant set matches cluster's, with a `Problem` / `application/problem+json` wire form, a documented OoP round-trip (`libs/toolkit-canonical-errors/src/problem.rs:150-170`), and `#[derive(ContractError)]` in the contract pipeline. Adopting it supplies the code mapping, the admin plane's problem+json, and the round-trip without cluster-side machinery.

**Two-layer model.** `ClusterError` stays the frozen Rust-facing contract consumers match on (unchanged — no consumer edit). `CanonicalError` is the wire form. The client reconstructs `ClusterError` from the canonical variant **plus a typed cluster detail context** carrying the discriminant and payload.

The mechanism is `#[derive(ContractError)]` from PR #4084. A cluster error enum annotated `#[error_domain("cluster.v1")]`, with `#[error_code("…")]` and `#[canonical(..)]` per variant, gets `From<_> for Problem` / `TryFrom<Problem>` generated, **serialises each variant's payload fields into `context["data"]`**, and bounces unknown `(error_domain, error_code)` pairs back as the original `Problem` — which is exactly the forward-compatibility rule §6.11's skew section needs. The companion `#[resource_error("gts.cf.…~")]` attribute (a string literal) supplies the typed resource-error constructors.

The mapping:

| `ClusterError` | `CanonicalError` variant | Detail context | Retryable? |
|---|---|---|---|
| `InvalidName`, `InvalidConfig`, `ProfileNotSpecified` | `InvalidArgument` | `FieldViolation` naming the field | no |
| `ProfileNotBound` | `NotFound` | resource = profile | no |
| `CapabilityNotMet` | `FailedPrecondition` | `PreconditionViolation` — carries the primitive / capability / provider triple natively | no |
| `Unsupported` | `Unimplemented` | feature name | no |
| `LockContended` | `Aborted` | lock name | no |
| `CasConflict` | `Aborted` | key + current version — see the caveat below | no |
| `LockExpired` | `FailedPrecondition` | lock name | no |
| `LockTimeout` | `DeadlineExceeded` | `waited`, populated server-side (the server did the waiting) | no |
| `Shutdown` | `ServiceUnavailable` | — (`ServiceUnavailableBuilder` exists for this) | no, per ADR-003 |
| `Provider{ConnectionLost}` | `ServiceUnavailable` | `kind = ConnectionLost` | **yes** |
| `Provider{Timeout}` | `DeadlineExceeded` | `kind = Timeout` | **yes** |
| `Provider{ResourceExhausted}` | `ResourceExhausted` | `QuotaViolation`, `kind = ResourceExhausted` | yes, with backoff |
| `Provider{AuthFailure}` | `Internal` | `kind = AuthFailure` | no |
| `Provider{Other}` | `Internal` | `kind = Other` | no |

Four consequences that must be got right:

- **`ProviderErrorKind` must be carried explicitly, not inferred from the canonical variant.** `Provider{ConnectionLost}` and `Shutdown` both map to `ServiceUnavailable`, yet one is retryable and the other is terminal — and `RestartingWatch` reads retryability from `ProviderErrorKind`. The mapping is therefore not injective, so the discriminant travels in the detail context and the client reconstructs from it. Getting this wrong would make the auto-restart combinator retry a shutdown forever.
- **`Provider{AuthFailure}` → `Internal`, not `Unauthenticated`.** The failure is the *cluster gear's* credentials against Postgres/Redis, not the caller's against cluster. `Unauthenticated` would send the platform's internal-auth interceptors and retry middleware down a token-refresh path that cannot help, and would point the operator at the wrong credential. The detail carries the truth; the variant must not mislead.
- **A transport failure with no canonical body** (channel down, pod gone) is synthesised client-side as `Provider{ConnectionLost, "cluster unreachable: …"}` — retryable, so an unreachable cluster gear behaves for consumers exactly like an unreachable Postgres. Same recovery path, no new consumer branch.
- **A lease-keyed operation that matches nothing is not a cache miss, and needs its own reverse mapping.** `Renew` / `Release` / `Resign` predicate on `(name, owner, fence, deadline)` (§5.8.1), so "zero rows" covers expired, released, and fenced-out-by-a-newer-holder alike — deliberately indistinguishable, so tokens are not probeable. `AwaitChange` is different: it addresses a *subscription*, which is replica-local, so it returns `NotFound` whenever the replica serving it goes away. Each maps to a variant the consumer can act on:

| Operation | Server returns | Client reconstructs | Why |
|---|---|---|---|
| `Renew { lease_token }` | zero rows matched | `LockExpired { name }` | The guard holds the name, so the variant is constructible. Semantically exact: the lease is gone, which is what the caller must act on, and DESIGN §3.3 pattern C already establishes `LockExpired` as the authoritative loss signal |
| `Release` / `Resign` | `Ok` (idempotent by absence, §6.10) | `Ok(())` | No error to map — the effect the caller wanted has already happened |
| `AwaitChange { election_id }` | `NotFound` | `LeaderWatchEvent::Closed(ClusterError::Shutdown)` | Terminal and non-retryable, so `RestartingWatch` propagates rather than resubscribing. The consumer's recovery is an explicit re-`elect`, per §6.10's stream-opens row |

  Only the `AwaitChange` row is new behaviour in Profile 3 — a subscription can outlive the replica serving it, which cannot happen in-process. Lease outcomes are identical in both profiles, because the lease is a row either way (§5.8.1). The §7.6 session-lifetime tests must cover a **cluster-gear restart under a held handle**, not only a client death.

> **Open detail (§9, decision 17a).** `CasConflict { key, current: Option<CacheEntry> }` carries a full entry including a `Vec<u8>` value. The question is whether `CanonicalError`'s context types can carry that *at all*; `#[derive(ContractError)]`'s payload-into-`context["data"]` serialisation answers that — a variant with a byte-vector field is expressible. What remains is **cost, not capability**: `context` is JSON, so the value is base64'd (+33% plus an encode/decode pass) on every conflict, on the error path of the hot CAS loop. Spike it in phase 1 and measure it in phase 0a. The fallback stays contract-legal either way: `current` is **SHOULD**, "if cheaply obtainable", so a remote caller may receive version-only (or `None`) and re-read at the cost of one round trip — and §7.2.3's compound ops remove the hot CAS loop that would feel it.

The REST admin surface gets problem+json from the same `CanonicalError` values with no separate mapping — which is the point of adopting it.

### 6.10 Idempotency and retry

The platform's `#[idempotency(..)]` annotation is a first-class IR concept driving retry-aware generated clients, which is exactly the mechanism this needs.

**Two annotation layers, not one.** PR #4084 splits them, and cluster must set both: `#[idempotency(SafeRead | IdempotentWrite | NonIdempotentWrite)]` on the **contract** trait feeds the IR, while the **gRPC projection** trait carries its own per-method `#[rpc(name = "…")]`, `#[idempotency_level(NoSideEffects | NotIdempotent)]` and an opt-in `#[retryable]` marker that is what actually licenses the generated client to retry. The two must agree: a method annotated `NonIdempotentWrite` below must not carry `#[retryable]` on the projection, and nothing in the macro cross-checks that today — so it belongs in review and in the §7.6 error-round-trip suite.

The contract-level classification:

| Contract methods | Annotation | Why |
|---|---|---|
| `get`, `contains`, `scan_prefix`, `describe_profiles` | `SafeRead` | Idempotent reads, freely retryable |
| `put`, `delete`, `compare_and_delete` | `IdempotentWrite` | Re-applying the same `put`, or a matched `compare_and_delete`, is harmless |
| `put_if_absent`, `compare_and_swap` | `NonIdempotentWrite` | **Must not be auto-retried.** If the first attempt succeeded but the response was lost, a retried `put_if_absent` returns "key existed" — which a caller reads as "someone else won". On the leader-election and lock paths that is a **false negative on an acquisition it actually holds**. A retried CAS likewise conflicts against its own successful write. Surface the ambiguity as `Provider{ConnectionLost}` and let the consumer re-read and decide — that is what the retryability classification is for |
| **Lease acquisition** — `try_lock`, `lock`, `join` | `NonIdempotentWrite`, **no `#[retryable]`** | The same false negative as `put_if_absent`, one layer up and worse, because the caller has no key to re-read. A retried `TryLock` whose first response was lost returns `LockContended` — **self-contention**, reported as another holder. A retried `Join` reports another leader when the caller *is* the leader. A retried `Register` orphans the first `instance_id`, leaving a phantom instance in discovery until TTL. All three are silent wrong answers, not errors |
| **Lease release** — `release`, `resign` | `IdempotentWrite` | Retry-safe, but only because the server makes them **idempotent by absence**: an unknown or already-consumed id returns `Ok`, not `NotFound`. Without that rule a retried `release` after a lost response surfaces a spurious failure on the cleanup path. `Ok` also leaks nothing, which is what keeps it consistent with the lease token being **opaque and server-issued with no lookup table behind it** (§5.8.1): a caller cannot use `release` to discover whether a token was ever valid, because both answers are `Ok`. An unauthorized release is an `Ok` that does nothing (§12.6) |
| **Lease renewal** — `renew` (locks and elections alike) | `IdempotentWrite` | Renewing twice is harmless, so retry is safe. But absence must **not** be `Ok` here: the caller needs to learn it lost the lease, so an unknown or non-owned `lock_id` is `LockExpired` (§6.9). This is the one lease op where absence is an error and idempotency stops at the wire |
| Stream opens | n/a | Re-establishment is `RestartingWatch`'s job for watches, and an explicit re-`elect`/re-`try_lock` for handles. Silently reopening a lock session would re-acquire a lock the consumer had abandoned |

**The acquisition row is the one that bites hardest across the boundary**, because in-process it cannot happen: a local `try_lock` either returns a `LockGuard` or an error, with no lost-response case in between. Two safeguards, neither optional: the projection must not carry `#[retryable]` on any acquisition method (nothing in the macro cross-checks this, §6.10 above), and `client_request_id` — introduced below as unused-in-phase-1 for cache writes — is **more valuable on the lease ops**, since server-side dedup is the only thing that can turn a retried acquisition from a wrong answer into the right one. Consider populating it for acquisitions from phase 1 even if the server ignores it.

Add an optional `client_request_id` to the mutating cache requests now, unused in phase 1, so server-side dedup can be added later without a wire break.

Timeouts: `rpc_timeout` per unary call; streams are long-lived and must **not** carry an RPC timeout, relying on HTTP/2 keepalive for liveness. Getting this wrong — an RPC timeout on a watch stream — would sever every watch on a fixed interval, so it warrants an explicit test (§7.6).

### 6.11 Versioning

| Contract | Policy |
|---|---|
| Rust (`cluster-sdk` facades + backend traits) | Per-primitive `*V1`/`*V2` as today (ADR-005, `cpt-cf-clst-nfr-plugin-stability`) |
| Contract traits + wire (`cluster.v1`) | Additive-only within `v1`: new optional fields, new enum values with `*_UNSPECIFIED = 0` defaults, new methods. `proto.lock.toml` guarantees field numbers never move |

A new Rust facade major does **not** force `cluster.v2` unless the wire shape actually changes, and vice versa — the wire mirrors the *backend traits*, which are more stable than the facades by design. Skew rules: a newer client against an older server tolerates missing optional fields and maps unknown enum values to `Provider{Other}` rather than panicking; an older client against a newer server ignores unknown fields. Both directions must be tested (§7.6) because a rolling deployment produces both.

**Consequence of the single-crate layout (§3.4).** The wire contract ships inside `cluster-sdk`, so it carries no independent crate version — a `cluster-sdk` release bumps whether or not the wire shape moved. The `cluster.v1` / `proto.lock.toml` pair remains the real wire-compatibility contract, and it is what skew testing asserts against; the crate version is not a wire signal and must not be read as one. This is the accepted cost of not having a separate crate, and it makes the lockfile load-bearing rather than merely convenient.

## 7. Cross-Cutting Consequences

### 7.1 Security

Cluster's primitives are unauthenticated in-process, because in-process means one trust domain. A network boundary changes that materially: `cache.get(key)` against a shared Postgres cache can read *any* gear's coordination state, and `lock(name)` can block any gear's critical section.

| Concern | Design |
|---|---|
| Transport exposure | Internal only. No primitive over REST; no gateway routes. `NetworkPolicy` restricting :50051 to platform namespaces. Probes on `probe_bind_addr`, off the k8s Service (ADR-0005) |
| Caller authentication | Platform plane per §4.6, both behind `InternalCredential` — but **split by plane**: SA token via TokenReview for the REST lifecycle/admin plane, and **mTLS+SPIFFE for the gRPC data plane, not "later"**, because an uncached per-request TokenReview is unusable at cluster's rate (§4.6, §7.2.5, decision 5) |
| Admin/data-plane split | `/admin/*` needs an authorization decision distinct from the data plane; `/admin/sessions` is incident-response data, not consumer data |
| **A profile is not a trust boundary** | It selects a backend; it does not partition access (§5.3), and two profiles routinely share one DSN and one `cluster_cache` table — the §4.10 example config does exactly that. So a per-profile caller allow-list is **not** the control it appears to be: it says nothing about two gears sharing one profile, and nothing about a caller reaching the same rows through a profile it *is* allowed to name. Cluster does not implement one, and the operator docs must say a profile name carries no authorization |
| **Cluster is a credential proxy — the cost of §5.3's win** | Concentrating backend credentials in one pod means cluster will use any configured backend on behalf of any authenticated caller. In-process, a gear could only reach backends whose credentials it held; that limit is gone, and no profile-level check restores it. The boundary is therefore the **network and the credential**: `NetworkPolicy` on :50051 plus platform-plane authentication (§4.6), which is why both are load-bearing rather than defence in depth. A deployment binding a profile to infrastructure at a genuinely different trust level — customer data, another tenant's Redis — needs a deployment-specific control at that layer, not a cluster config key |
| **Namespace enforcement** | `scoped()` is client-side and *cooperative* — a wrapper the consumer opts into. Over a network that is not an isolation boundary at all: a caller can simply not scope and read another gear's keys. Phase 7 hardening (decision 9): cluster derives a mandatory scope prefix from the authenticated caller identity and rejects keys outside it. This is a **contract change** for consumers writing unscoped keys today, so it needs its own ADR and a migration path — and it is what §4.6's ruling leans on, since until it lands there is no *enforced* tenant isolation of cluster data anywhere |

**Namespace enforcement is the one piece of new security work that matters**, and the profile and credential-proxy rows are why: with profiles ruled out as a boundary and credentials concentrated by design, key scoping derived from the caller's identity is the only mechanism that partitions one gear's coordination state from another's — regardless of which profile is named, and regardless of whether profiles share a backend. It does not block a first internal-only deployment behind a NetworkPolicy, but it is what has to exist before cluster serves gears across trust boundaries, and the enforcement point is far cheaper to design now than to retrofit (decision 9).

### 7.2 Performance — the primary consequence

Making cluster a separate process is, above everything else in this document, a **performance change**. It deserves to be quantified rather than asserted, and the cost turns out to be neither uniform across primitives nor uniformly negative.

> All figures below are **order-of-magnitude estimates with visible reasoning**, not measurements. Phase 0 gates the design on a real benchmark (§8); do not treat these as verified.

#### 7.2.1 The hop arithmetic

The framing that makes this tractable: **for every backend other than standalone, cluster already pays a network round trip today.** Remoting adds one hop — it does not convert memory access into network access.

| Path | Hops | Est. p50 | vs. today |
|---|---|---|---|
| `cache.get`, Profile 1, Postgres backend | app → PG | ~0.3 ms | baseline |
| `cache.get`, Profile 3, Postgres backend | app → cluster → PG | ~0.6 ms | **~2×** |
| `cache.get`, Profile 1, standalone backend | none (in-memory) | ~0.001 ms | baseline |
| `cache.get`, Profile 3, standalone backend | app → cluster | ~0.3 ms | **~300×** |

Two conclusions. Against a real backend, latency roughly doubles and the absolute addition (~0.3 ms intra-cluster) sits comfortably inside `cpt-cf-nfr-oop-latency`'s 5 ms localhost / 10 ms intra-cluster budget. The alarming ratio appears only against the standalone backend — which is the dev/test configuration, and dev/test keeps Profile 1 (§3.2). The pathological case is the one nobody runs.

#### 7.2.2 The cost is concentrated, not spread

Sorting the three primitives by actual call frequency rather than by API surface area:

| Primitive | Call frequency | Effect of remoting |
|---|---|---|
| **Leader election** | One renewal per `renewal_interval()` (~10 s default), client-driven (§7.3) | **One hop per renewal, and nothing else changes.** ~0.3 ms every ~10 s per held election is not a cost worth optimising; keeping renewal client-side is what preserves the wedged-consumer liveness proxy (§7.3) |
| **Watches** | Cost is per *event*, not per poll | **Better** — the gear holds one upstream subscription per watched key/prefix and fans out to N consumers, replacing N consumer-side Postgres `LISTEN` connections |
| **Cache + locks** | Hot **only** in the counter / CAS-loop pattern | The real exposure — see below |

So the hot path is one access pattern (read-modify-write under contention) in effectively one consumer (OAGW's rate limiter), plus `PollingPrefixWatch`. That is narrow enough to fix specifically rather than to pay for globally.

#### 7.2.3 The hot pattern, priced

OAGW's rate-limit flow (cluster PRD UC-002), in-process over Postgres:

```
try_lock  → 1 PG round trip
get       → 1 PG round trip
CAS       → 1 PG round trip
release   → 1 PG round trip
                            ≈ 4 × 0.3 ms = 1.2 ms
```

Remoted, every step gains a hop: 4 gRPC + 4 PG ≈ 2.5 ms — **roughly 2×**, against a 5 ms localhost / 10 ms intra-cluster budget (`cpt-cf-nfr-oop-latency`). **That cost is accepted.** It lands inside the budget with room, it applies to one access pattern in one consumer (§7.2.2), and it buys the deployment model the whole document exists for.

**The API is not coarsened to avoid it.** A compound operation — `increment(key, delta, ttl)` as a single `INSERT … ON CONFLICT DO UPDATE … RETURNING`, atomic in the backend and needing no lock at all — would collapse that flow to one hop (~0.6 ms) and beat today's in-process implementation. It is the obvious lever, and it is deliberately **not pulled now**: it extends a frozen contract (`cpt-cf-clst-fr-cache-atomic`), it needs an ADR and both backends implementing it, and it is speculative work against a cost that measurement has not yet shown to hurt. The interface is amended later if production says so, not in advance of evidence.

Two consequences follow from leaving it out, and both are load-bearing elsewhere in this section:

1. **The read-modify-write loop stays**, so `CasConflict` payload fidelity is a live concern rather than a vestigial one (§6.9, decision 17a).
2. **The ADR-002 critical-section conflict must be settled by amending ADR-002**, since there is no compound operation to dissolve it (§7.2.8).

#### 7.2.4 The binding constraint is pool fan-in, not gRPC

`pool_max_size: 5` at ~0.5 ms per operation yields roughly 10k ops/s from a single Postgres instance — **exactly** OAGW's stated requirement, with zero headroom, and that is before every other profile bound to the same instance adds its traffic (§5.3 deliberately makes them share).

This, not protocol overhead, is what will actually break first. gRPC over multiplexed HTTP/2 handles 10k RPC/s on one channel without difficulty; five Postgres connections serving the whole fleet do not. It is a sizing and admission-control problem:

- Revised `pool_max_size` guidance per deployment shape, sized against *fleet* traffic rather than one consumer process.
- `cluster_backend_pool_saturation` gauge with a WARN on acquire-timeout, so saturation is visible before it is an incident.
- Per-`ClientId` concurrency caps (§5.7) so one noisy consumer cannot starve the rest.

#### 7.2.5 Other per-operation costs

- **Platform-plane auth is per-request, and per-request is the wrong granularity.** An uncached SA-token TokenReview is a call to the API server — tens of milliseconds — which at 10k ops/s would dominate the budget outright and invalidate every figure above. This is a design constraint, not a tuning problem. **Requirement, in preference order**: (1) **validate once per connection**, which is what every high-throughput internal RPC layer relies on and what mTLS gives for free — a long-lived HTTP/2 channel makes the check amortise to nothing, and it is the natural fit for a data plane whose clients are long-lived pods; (2) failing that, per-credential caching with a bounded TTL, which works but reintroduces a revocation window and a cache to size. Because (1) is what ADR-0006's **mTLS + SPIFFE** phase delivers structurally, cluster is an argument for pulling that phase forward rather than treating it as "next phase" — the SA-token phase is the one that carries the per-request cost (§4.6, decision 5).

  **The caching question is settled, and the answer is no.** ADR-0006 states that TokenReview is cached; the implementation does not. `K8sTokenReviewAuthenticator::authenticate()` is a bare `self.review(token).await` with no cache anywhere in the crate — verified here and independently re-confirmed by the #4084 owner. So ADR-0006's claim is a **defect in the ADR** rather than an open question (§10), and the consequence is structural: even once a server-side inbound interceptor exists, dropping it in front of an uncached TokenReview leaves cluster's data plane unusable. **The interceptor and the mTLS pull-forward are therefore one work item, not two.** Confirm implementation details against [PR #4403](https://github.com/constructorfabric/gears-rust/pull/4403)'s authenticators (`toolkit-security/src/{authenticator,internal_auth_config,shared_secret}.rs`, `toolkit-transport-grpc/src/sa_token.rs`) — **not** against `InternalAuthMiddleware`, which does not exist in code (§2.0, §4.3).
- **Watch events cost a follow-up read.** By `cpt-cf-clst-principle-lightweight-notifications`, a `CacheEvent` carries only the key; the consumer calls `get` for the value. Remotely that is 2 round trips per event instead of 1. The principle's rationale (stale values, Postgres `NOTIFY`'s 8 KB limit) still holds, so this is a real cost to accept rather than optimise away — though a server-side read-through, where the gear serves the follow-up `get` from the subscription it already holds, is worth exploring later.
- **`PollingPrefixWatch`** becomes N+1 *network* round trips per interval. `MultiGet` collapses it to 2. Its doc comment's cost warning needs restating in network terms.
- **Paginated `scan_prefix`** adds round trips proportional to keyspace size (§6.4).
- **One client-side task per held handle.** Every `LockGuard` and `LeaderWatch` is serviced by a pump task translating its command channel into RPCs (§12.11–12.12), and every watch adds one more. The in-process backends already pay this, so it is not a regression — but a consumer holding hundreds of concurrent locks now holds hundreds of pumps, which is worth a note in the sizing guidance rather than a surprise in a heap profile.

#### 7.2.6 What gets better

Set against the per-operation costs above:

| Improvement | Magnitude |
|---|---|
| Backend connections | `consumer_replicas × gears × pool_size` → `cluster_replicas × distinct_instances × pool_size`. ~50 → 5 for a 10-replica gear against a single cluster replica, and the multiplier that remains is one you set (§5.3, §5.8.3) |
| **Duplicate TTL reapers** | Today every consumer process runs its own reaper against the same tables: `consumer_replicas × gears` reaper queries per interval. Remoting collapses this to one per instance per cluster replica, which reduces load on the *backend* — usually the real scaling ceiling — so total system throughput can improve even though per-op latency rises. Reapers stay idempotent (`DELETE … WHERE expired`), so concurrent replicas race harmlessly; with fence retention they must also skip records still inside their window (§5.8.1) |
| Watch subscriptions | N consumer-side `LISTEN` connections → one per watched target **per cluster replica**. The fan-out win is real and shrinks in proportion to the replica count, which is the ordinary cost of horizontal scale (§5.8.3) |
| Incident diagnostics | "Who holds lock X" becomes answerable from the lease index and the store behind it (§5.4) — a question the in-process design could not pose |
| Credential distribution | Every consumer pod → one pod (§5.3) |

And one deployment shape that is **impossible today** becomes available: a single cluster pod with the standalone backend gives **multi-instance coordination with zero infrastructure** — no Postgres, no Redis, ~0.3 ms per op. Today multi-instance coordination requires provisioning a backend (cluster PRD §3.1). This is a genuine capability gain hiding inside a performance cost, and it is probably the right default for small on-prem deployments.

#### 7.2.7 Escape hatches, in order of preference

| Option | Effect | Cost |
|---|---|---|
| **1. Compound operations** (§7.2.3) | Hot path becomes *faster* than in-process | Extends the frozen cache contract; needs an ADR and both backends. **Deferred** — the lever to reach for first if production shows the cost hurts |
| **2. Profile 1 for hot consumers** | OAGW links the cluster wiring into its own process and pays only the direct DB round trip. Note this is **not** a correctness compromise: embedding puts the backend *client* in-process while the backend itself stays shared, so cross-replica coordination is unaffected. Note also that this is **deployment composition, not configuration** — the gear links `cluster`, whose `start` registers a `LocalClusterClient`, and local-wins means no remote client is registered alongside it (§4.9.3). **Whole-process**: the gear links the same `cluster` gear and must configure every profile it resolves — see the note below | That gear needs DB credentials and its own pool, reopening §5.3's win for it alone, **and it must link `grpc-hub` and expose a gRPC port it does not otherwise need** (§4.2) |
| **3. Colocation** — cluster as a DaemonSet, consumers reaching a node-local instance over UDS | Cuts the added hop from ~0.3 ms to ~0.05 ms, **and removes the coordination SPOF §5.7 lists** | Multiplies pools by node count, undoing much of the connection-count win — but the blocking cost is the lease model, not the pools: see below |
| **4. Client-side read caching** | Removes reads entirely for tolerant consumers | **Discouraged**: silently breaks the `Linearizable` guarantee a consumer validated at resolve time. Only defensible for consumers that did not require it, and even then it needs explicit opt-in |

Recommendation: **none of them, for now.** The cost is accepted (§7.2.3), so no consumer pays for an escape hatch it has not been shown to need. If production says otherwise, take them in order: 1 first, because it makes the hot path faster than today and benefits Profile 1 equally; 2 only for a consumer that measurement singles out. Treat 4 as available but discouraged. Option 3 is a genuine alternative topology rather than a fallback, so it gets a full assessment:

> **Node-local is the standard shape for this class of component, not a fallback.** Consul's client agent, Vault
> Agent, etcd's `grpc-proxy` and kubelet itself are all node-local **by design** — precisely to avoid a central
> coordination SPOF and to keep the added hop in microseconds. The industry has made this trade repeatedly, and
> consistently in the opposite direction to a single central instance.
>
> **Nothing structural stands in the way.** A lease is a row (§5.8.1), so every node-local instance reads the same
> state and a renew landing on any of them behaves identically. A DaemonSet is a replica topology, not a different
> design.
>
> **The remaining costs are real but ordinary**, and they are why this is still not the default: pools multiply by
> node count (§5.3's win shrinks toward `nodes × pool_size`, mitigable with a small per-node pool), and each
> node-local instance holds its own upstream watch subscriptions, so the fan-out collapse of §7.2.6 weakens in
> proportion. Both are capacity arithmetic against a ~0.25 ms per-op saving and the removal of a central hop.
>
> **Position: available and coherent, not planned for the first deployment.** Revisit against the phase-0a numbers
> once the failover suite (§5.8.3) has run — a DaemonSet is the most demanding consumer of exactly the properties
> that suite proves.

> **Embedding is whole-process, and there is one cluster implementation either way.** Option 2 is not a second
> cluster: the embedding process links the *same* `cluster` gear crate and the *same* plugins, and points them at the
> *same* backends. What differs is where the code runs. Two instances of one implementation then coexist — one inside
> the embedding process, one in the pod — and they coordinate correctly because the backend row, not either instance,
> is the arbiter (§6.5). Nothing forks, and no profile is served by different code in different places.
>
> The constraint that follows: **a process that links `cluster` resolves every profile through its own
> `LocalClusterClient`**, because local-wins suppresses the remote one and no channel is built (§4.9.3). So an
> embedding gear must configure every profile it resolves, or get `ProfileNotBound` — loudly, at `resolve()` — for the
> ones it did not.
>
> Partial embedding — one hot profile local, the rest from the pod — is therefore **not supported**, and is not worth
> supporting speculatively. It would require a multi-profile consumer whose profiles differ materially in heat, *and*
> a measured cost the accepted budget cannot absorb (§7.2.3), *and* an operator willing to split
> profile definitions across two YAMLs, which muddies `cpt-cf-clst-fr-validation-typed-profile`'s "exactly two places"
> rule. If it is ever wanted, the fix is additive and cheap at that point: one defaulted `ClusterClient` method
> offering the local impl a remote peer to fall through to, in the same shape as `probe()` (§4.4). Adding it before a
> measured need would put an untested path inside the one mechanism that keeps §4.9.3 branch-free.

#### 7.2.8 The critical-section rule — ADR-002 is amended

`cpt-cf-clst-fr-lock-no-remote` / ADR-002 / `cpt-cf-clst-nfr-bounded-critical-section` state that consumers MUST NOT make remote calls inside a lock's critical section, enforced by a workspace lint. The premise is that cluster's own operations are local, so the only thing a critical section can reach across the network is something *other* than cluster.

In Profile 3 that premise does not hold. UC-002 — OAGW's canonical flow — reads: *"Lock is available; OAGW reads and increments the rate counter via **local** cache CAS — no remote calls inside the critical section."* Remoted, the `get` and the CAS **are** remote calls, so the rule as written forbids the design's own reference flow.

**Decision: amend ADR-002.** Cluster-primitive round trips inside a critical section are permitted and bounded; the ban on *non-cluster* remote I/O — database writes, HTTP calls to other services — stands unchanged, and the lint is re-scoped to distinguish the two. The alternatives were considered and rejected:

| Option | Why not |
|---|---|
| Compound server-side operations, so the read-modify-write is one atomic statement | Would remove the critical section entirely for the motivating pattern, and it remains the right answer if measurement demands it — but it extends a frozen contract on speculation (§7.2.3), and the rule still needs amending for every *other* lock consumer |
| Forbid Profile 3 for lock consumers | Not viable; locks are first-class in the OoP story |

What the amendment must preserve is the *reason* for the original rule: a critical section whose duration is unbounded, or which depends on a third party's availability, turns a lock into an outage. A cluster round trip is bounded by `rpc_timeout` and fails closed, so the holder learns quickly; an arbitrary HTTP call is neither. DESIGN §3.3's pattern C — treat the lock as advisory and make the protected write conditional — remains the answer for correctness-critical work, and becomes more important rather than less, since a remote critical section has a wider window in which a lease can lapse unnoticed.

The lint change is not cosmetic: left as-is it either fires on every correct Profile 3 consumer or gets suppressed wholesale, and a suppressed lint protects nothing.

### 7.3 Leadership liveness tracks the consumer, not the transport

Leader renewal doubles as a liveness proxy for the consumer, and that is a property the remote path must keep. If a consumer wedges — deadlock, blocked runtime, stop-the-world — its renewals stop, its claim lapses at the lease deadline, and a healthy peer takes over. The safety property is "the leader is working", not "some process is up".

**That is why renewal is client-driven** (§5.8.1, §6.6). The lease is a row with an owner, a deadline and a fence; the authority to extend it is the token the client holds, and the operation that extends it is the client's own `renew`. Renewing inside the gear on a consumer's behalf would sever the proxy — the gear would keep a wedged consumer elected indefinitely, and the guarantee would silently weaken to "cluster is up". Four consequences:

- **The proxy holds identically in both profiles.** A wedged consumer loses its claim on the same timeline whether the backend is in its own process or behind a network hop.
- **The transport owes no keepalive.** Liveness is carried by an operation the consumer must perform anyway, so neither projection of the leader watch (§6.8) has to stand in for it — a subscription is an observation channel and nothing more.
- **`max_missed_renewals` governs one thing**: how much renewal jitter the lease tolerates.
- **The staleness bound gains one hop.** DESIGN §3.3's worst case (`TTL + observation_lag`) grows by the client→cluster hop on the observation side; renewal adds no server-side term. State the revised bound in the DESIGN delta (§10).

The cost is renewal traffic proportional to held elections rather than one loop per election in the gear: one round trip per `renewal_interval()` (~10 s default) per holder, which is negligible against the 10k ops/s the cache path carries and buys a correctness property no amount of server-side bookkeeping can reconstruct.

None of this changes leader election's advisory nature (`cpt-cf-clst-fr-leader-advisory`) — it was always advisory, and pattern C remains the answer for correctness-critical work. What it does mean is that the advisory signal means the same thing in Profile 1 and Profile 3.

### 7.4 Scoping across the boundary

`scoped()` stays client-side: `Scoped*Backend` wraps the remote backend, so full prefixed keys go over the wire and the server needs no scope concept. Composition (`cache.scoped("event-broker").scoped("shard-0")`) and read-path prefix stripping on `CacheEvent` keys both keep working — including through the polyfill, which emits full backend keys precisely so the scoped wrapper can strip them.

That scoping is cooperative rather than enforced is §7.1's last row.

### 7.5 Observability

ADR-004 makes span/metric/log names part of cluster's contract, with a cardinality rule. The transport adds a layer to instrument without breaking either.

**Client-side instrumentation is delegated once #4084 lands, and a plain `tracing` span in the tonic interceptor until then (§6.1).** PR #4084 ships a `PolicyStack` for exactly this — its example wraps every client call in one, passing a `PolicyContext { service, method, idempotency, kind }`, and its ADR-0006 (`cpt-cf-binding-adr-client-observability`) covers W3C `traceparent` propagation and RED metrics, behind the SDK's `otel` feature. Cluster's `Remote*Backend` methods therefore wrap their calls in the `PolicyStack` rather than emitting spans and counters by hand: the trace context propagation, the per-method latency/error metrics and the naming convention all come from the framework, and cluster only supplies the `PolicyContext` and its own domain attributes below. This is the same reuse-over-reinvent rule as §2.2.2, applied to telemetry.

- **Spans**: client-side span per call (from the `PolicyStack`), server-side span linked via propagated trace context, so a consumer's `cache.get` and the gear's Postgres query appear in one trace. Add a `cluster.transport = "grpc" | "embedded"` attribute and the `profile`; do **not** rename existing spans — a rename is a breaking change under ADR-004.
- **New metrics** beyond the stack's RED set, labels bounded to `profile`, `provider`, `primitive`, `method`, `code` — never keys or names: open streams and sessions by primitive; watch `Lagged` events; per-instance pool saturation and probe failures; internal-auth validation latency and cache hit rate (§7.2); descriptor-prefetch outcome and whether validation was inline or deferred (§4.7.1).
- **Attributes**: caller `PlatformIdentity` on server spans, so "which gear is hammering the cache" becomes answerable — a question the in-process design could not even pose.
- **Cardinality trap to call out explicitly**: `/admin/sessions` and per-session logs contain lock and election names. They belong in logs and traces, never as metric labels — easy to get wrong when someone adds a "locks held by name" gauge.

### 7.6 Testing strategy

The `cluster-conformance` crate is a backend-agnostic suite already run against the gear's cache-derived defaults (`cluster/tests/conformance.rs`). That is the leverage point: **run the same suite through the remote transport.**

| Layer | Test |
|---|---|
| Conformance over the wire | New `cluster/tests/remote_conformance.rs`: in-process tonic server over the standalone plugin + `Remote*Backend` clients over a real channel, running the existing conformance suite. Any semantic drift between transports fails here — the single highest-value test in the change |
| Contract equivalence | For each facade method, assert Profile 1 and Profile 3 produce identical results and identical `ClusterError` variants |
| Startup ordering | **Resolve during the consumer's own `start` with cluster not yet reachable, and assert it succeeds** after the bounded timeout — never hangs, never errors (§4.9.3). Then assert `/readyz` stays 503 until descriptors land, and that the first RPC through the facade succeeds once cluster appears |
| Inline validation (the normal path) | With cluster reachable, an unmet requirement ⇒ `Err(CapabilityNotMet)` **from `resolve().await`**, naming primitive, capability and server-side provider — byte-identical to the Profile 1 error for the same misconfiguration. This is the parity assertion that matters most |
| Deferred validation (cold start) | With cluster unreachable past the timeout, `resolve()` returns `Ok`; once the descriptor lands and fails a recorded requirement ⇒ `Unhealthy` with the same triple, `/readyz` 503. Assert the `info` log distinguishes the two paths |
| Resolve timeout | Assert the bound is honoured — a cluster that never answers delays `resolve()` by the timeout and no more, and phase 7 completes |
| Error round-trip | Every `ClusterError` variant encoded → status+DTO → decoded, asserting variant equality *and* `RestartingWatch` retryability classification |
| Session lifetime | Kill the client mid-lock ⇒ the lease expires at TTL and the next acquirer proceeds; mid-election ⇒ polling stops, renewal stops, a successor is elected within TTL (§7.3); mid-registration ⇒ instance leaves discovery at TTL. Assert the *timing bound*, not immediate release — TTL is the contract |
| **Restart under load — the property the lease model buys** | Restart the cluster gear (and, once >1, roll every replica) while consumers hold locks and leadership: **no `LockExpired`, no `Status(Lost)`, no re-acquire**. Only watch subscribers see `Closed`, and `RestartingWatch` re-establishes them (§5.8.2) |
| **Cross-replica leases (gates `replicaCount > 1`)** | Acquire against replica A, `renew` and `release` against replica B — identical results, no `LockExpired`. Same for `Join` / `Resign`. This is the test that turns §5.8.3's default into a decision (§5.8.3) |
| **Fencing** | Let a lease expire, have a second client acquire it (fence bumped), then have the *original* holder `renew` and `release`: both must fail their predicate — `LockExpired` and a no-op `Ok` — and must not touch the new holder's lease |
| **Uniform expiry** | The same crash test in Profile 1 and Profile 3 yields the same timing: a killed holder's lock stays held until its TTL and is then acquirable, with no path that reclaims it earlier (§5.8.2) |
| **Stale subscriptions** | Restart the cluster gear under a held `LeaderWatch` and assert the *subscription* terminates per §6.9 — `AwaitChange` ⇒ `Closed(Shutdown)`, `RestartingWatch` re-establishes — while the lease itself is untouched: `renew` against the new process succeeds and leadership never moves |
| **Retried acquisition** | Inject a lost response on `TryLock` / `Join` and assert the generated client does **not** retry (§6.10): a retry would report `LockContended` against the caller's own lease. Assert no acquisition method carries `#[retryable]` on the projection — a static check, since the macro does not cross-check it |
| **Nothing wired** | A consumer whose hub has no `dyn ClusterClient` (Profile 1 with `cluster` unlinked, or the forwarding feature left off): `resolve()` returns `Ok`, `/readyz` goes `Unhealthy` after the grace window, and any call made first returns `ProfileNotBound` naming the process rather than the profile (§4.9.1). This is the misconfiguration lazy binding would otherwise hide |
| Watch semantics | `Lagged` on a deliberately slow consumer; `Reset` on server resubscribe; `Closed(Shutdown)` on `stop()`; and explicitly: **a watch stream survives longer than `rpc_timeout`** (§6.10's trap) |
| Readiness | All four ADR-0005 states with their exact bodies; `Degraded` on one bad profile; `Unhealthy` on a permanent capability mismatch (§4.7); registration failure does **not** affect readiness |
| **Per-profile readiness gate** | Degrade one profile's backend and assert that consumers of *that* profile go 503 within one poll interval while consumers of a healthy profile keep serving; then restore it and assert they return to rotation without a restart, and that neither ever escalates to process exit (§4.4, §4.7) |
| Shutdown | `stop()` delivers `Status(Lost)` then `Closed(Shutdown)` to *remote* leaders in that order, before returning |
| Profile routing | Two profiles on different providers under concurrent load, assert no cross-talk; two profiles on one DSN share exactly one pool (assert via `pg_stat_activity` or a provider build counter) |
| Auth | Missing/invalid internal credential ⇒ rejected; validation-cache hit rate under load (§7.2) |
| Latency | `cpt-cf-nfr-oop-latency` benchmark: framework overhead only, p95 < 5 ms localhost |
| Version skew | Newer client / older server and the reverse (§6.11) |
| OoP smoke | Spawn `cluster-oop` + a consumer; assert DNS resolution, registration, `/readyz` gating, end-to-end coordination, clean drain |

Use the standalone plugin for the conformance runs to stay hermetic — the Postgres plugin's known full-suite flakiness is Docker/timing infrastructure, and Postgres belongs in the provider-specific paths.

## 8. Phasing

| Phase | Deliverable | Depends on |
|---|---|---|
| **0a — baseline measurement** | Not a gate: the cost is accepted (§7.2.3, §7.2.7). Still worth running early, because every figure in §7.2 is an estimate and pool sizing (§7.2.4) depends on real numbers. A throwaway harness: tonic server over the standalone and Postgres plugins, `Remote*Backend` clients, and the four access patterns (single `get`, counter read-modify-write, watch event + follow-up read, prefix poll) at 1 / 100 / 1k / 10k ops/s, reporting p50/p95/p99 and pool saturation against the same patterns in Profile 1. Its output sets sizing guidance and tells us whether the deferred compound-operation lever is ever needed | — |
| **0b — open decisions** | §9's remaining blocking question is now **only internal-auth granularity (5)**, and it has a named owner — the **OoP runtime / ADR-0006 two-plane work**, not #4084 — plus a scope correction: the inbound gRPC interceptor and the mTLS pull-forward are one work item, because an interceptor over an uncached TokenReview is still unusable at cluster's rate. Open it first; its lead time is not ours, and phase 5's gate depends on it. Decision 21 is **no longer blocking** (cluster derives its own endpoint, and the ADR-0004 static override covers a directory-less Profile 3) but should be raised in the same window because it is two coupled asks on two other owners — the bootstrap eager-connect lift, and a DNS fallback whose real prerequisite is a **port convention in the platform PRD**. Phase 1 additionally opens with the contract-shape review against #4084 (§6.1), which is a working session rather than a decision to wait on. The Postgres beacon removal (§5.8.2) is ours to make but lands on merged code, so open it with #4411's author at the same time | — |
| **1 — contract** | Opens with the **contract-shape review** (§6.1) — the two enforcement questions plus a DTO, wire-shape and error-shape pass with the `toolkit-contract` owners. Now a review of a contract written *with* the shipped macros rather than a negotiation about unshipped ones. Then: the `*Api` traits, DTOs (incl. `LeaseToken`, §12.1), the protogen-generated `.proto` + `proto.lock.toml`, the generated client, the `#[derive(ContractError)]` codec + `CasConflict` fidelity spike (decision 17a — sharper now that the CAS loop stays, §7.2.3). No behaviour | 0b |
| **2b — store-owned leases** | The lease-model change (checklist items 10b, 10c): fenced lease records in the SDK-default backends and the lock contract, plus removing the Postgres liveness beacon. **Blocking for phase 3**, because the wire contract's lease operations are shaped by it, and it is the change that makes a cluster restart harmless (§5.8.2) | 1 |
| **2 — runtime profile management** | `ProfileRegistry`, `BackendInstanceCache`, `probe()`, composite readiness, revised pool-sizing guidance + saturation metric (§7.2.4). **Ships value before any OoP work**: the shared-instance dedup fixes real waste in today's in-process gear | — (parallel with 1) |
| **3 — server** | Four service impls, session index, `GrpcServiceCapability`, `RestApiCapability` + admin routes, capabilities `+grpc,rest,system`, revoke fan-out | 1, 2 |
| **4 — client** | `ClusterClient` trait, `RemoteClusterClient` + three `Remote*Backend`s, the `ConsumerRegistration` + profile inventory, lazy-binding facades, transient/permanent readiness classification | 1 |
| **5 — deployable (Profile 3 only)** | `main.rs`, `registered_gears.rs`, `[[bin]]`, image, Helm values, NetworkPolicy, preStop, migration subcommand + Job (§4.10.1), OoP smoke test. **Scope limit, stated rather than inferred: this phase delivers Kubernetes (Profile 3) only.** Profile 2 has no endpoint-resolution mechanism at all — `ApiContractsConfig.remote_grpc_endpoints` does not exist (tier 3), #4084's wiring path resolves REST endpoints from DirectoryService (§4.5), and the P2 topology fork is unanswered (§3.2) — so single-host P2, the on-prem baseline, is out of scope here and not merely deferred by decision 12. **Two Profile-3 notes:** a directory-less deployment is served in the interim by the ADR-0004 static override pinned from Helm values, *provided* the bootstrap's eager directory connect has been lifted (decision 21); and the gRPC port ships **NetworkPolicy-only** unless the inbound interceptor plus mTLS have landed as one item by this phase (decision 5) | 3, 4 |
| **6 — conformance** | Remote conformance run, equivalence/error/session/readiness/skew/latency suites, **plus the failover suite that gates `replicaCount > 1`** — cross-replica renew, rolling upgrade under held locks, fencing, uniform expiry (§5.8.3, §7.6) | 3, 4 |
| **7 — hardening** | Server-enforced namespacing (decision 9 — the isolation boundary, §7.1), per-client quotas, and — **no longer a migration item**, since #4084 merged before phase 1 began (§6.1): the wire is generated and `proto.lock.toml` pinned from the first commit, so phase 7 inherits only the standing CI check that the lock file matches the IR — and raising the replica count on the evidence from phase 6 | 5, 6 |

Phases 1 and 2 are independent and can run in parallel. Phase 2 is worth landing regardless of the OoP outcome.

## 9. Open Questions and Decisions Needed

**Blocking:**

| # | Question | Recommendation |
|---|---|---|
| 5 | **Internal-auth granularity, and the missing server-side gRPC path** (§7.2.5). Two facts, **both now confirmed by the framework owner across `main`, #4403 *and* #4084**. (a) Validation is **not cached**: `K8sTokenReviewAuthenticator::authenticate()` is `self.review(token).await` (`toolkit-k8s-auth/src/lib.rs:191-193`), a live TokenReview per call, and neither #4403 nor #4084 adds caching — so ADR-0006's claim that it is cached is a defect in the ADR (§10). At 10k ops/s that is 10k API-server calls/sec against a ~0.3 ms hop budget — two orders of magnitude out, and the API server fails before cluster does. (b) **No server-side gRPC interceptor exists** in any of the three trees: the gRPC side ships token *extraction* plus an outbound interceptor, while the authenticator is wired into the HTTP path only (`oop_serve.rs`'s `DynInternalAuthenticator`). **Owner named**: the inbound platform-plane path belongs to the **OoP runtime / ADR-0006 two-plane work**, the same area as #4403 — not to #4084's contract codegen | **Per-connection validation is the target**, which mTLS gives structurally — an argument for pulling ADR-0006's mTLS+SPIFFE phase forward rather than shipping SA tokens plus a cache, since caching buys the latency back by introducing a revocation window cluster's own posture absorbs (§7.1). **The interceptor and the mTLS pull-forward are one work item, not two**: an interceptor in front of an uncached TokenReview is still unusable at cluster's rate, so there is no viable SA-token phase for the gRPC data plane at all (§4.6). Cluster is the forcing function for both halves, not the owner: it is the first gear whose data plane is gRPC **and** platform-plane, and the only caller whose rate makes granularity a design constraint. **The gate stands as written, and its owner agrees it is the right gate**: the interceptor must exist by phase 5 or cluster's coordination port ships unauthenticated behind a NetworkPolicy |

**Non-blocking:**

| # | Question |
|---|---|
| 21 | **Where does the endpoint come from when no DirectoryService is deployed?** (§4.5, §2.0) **Downgraded from blocking — cluster has a working path either way** (own derivation plus the ADR-0004 static override), so this is raised on the platform's behalf. It is **two asks with two owners, which must move together**: (a) **lift the OoP bootstrap's eager directory connect** (`run_oop_with_options`, `bootstrap/oop.rs:542`, on `main` today, OoP runtime owner) — until this goes, no OoP gear starts without a directory and a wiring-phase fallback is unreachable; (b) **a DNS fallback resolver** in the wiring phase, agreed by #4084's owner. The wiring phase's `NullEndpointResolver`/503 behaviour is **deliberate and stays** — skipping wiring would let a misconfigured consumer report Ready. **The real decision inside (b) is the port convention**: the directory returns a full base URI, DNS returns a name only, so this is *not* "a `format!`" as this document previously claimed — it needs a per-dep key, a platform-wide default, or a directory read-back, and that choice **belongs in the platform PRD**, not in #4084. Against `cpt-cf-fr-k8s-native` (platform PRD:272), which says DirectoryService "MUST NOT be required for basic service resolution in k8s" |
| 17b | **Should `LockGuard` expose a fencing token for external resources?** (§5.8.1) Cluster's internal `fence` is monotonic only within `fence_retention`, which is enough for its own predicates and not enough to hand a third-party store. A consumer protecting an external resource with a cluster lock has no sound token today. Additive if wanted — a `LockGuard::fence()` backed by a source that can promise global monotonicity (a Postgres sequence) — and it touches a frozen consumer-facing type, so it needs its own ADR |
| 16b | Does `inventory`-based **profile** self-registration belong in `cluster-sdk` (§4.9.2)? The framework already uses `inventory` for `ConsumerRegistration`, `GearRegistry::discover_and_build()` and GTS registration, so this is adoption rather than invention — but the placement is worth confirming with the SDK owners |
| 17a | **`CasConflict` payload fidelity** (§6.9) — a cost question, not a capability one, and a live one now that the read-modify-write loop stays (§7.2.3). `#[derive(ContractError)]` serialises variant payloads into `context["data"]`, so a `Vec<u8>` value *is* expressible; the question is the base64 overhead on the CAS error path. Spike in phase 1, measure in phase 0a; the version-only fallback is contract-legal (`current` is SHOULD), and the CAS loop that would feel the overhead is the one pattern §7.2.3 declines to optimise away |
| 9 | **Server-enforced namespacing from caller identity** — the isolation boundary, now that profiles are ruled out as one (§7.1) and tenant isolation is the caller's responsibility (§4.6). Cluster derives a mandatory scope prefix from the authenticated `PlatformIdentity` and rejects keys outside it. Needed before cluster serves gears across trust boundaries; it is a **contract change** for consumers writing unscoped keys today, so it needs its own ADR and a migration path. Phase 7 |
| 12 | **Profile 2 (Host + Workers) — undesigned, and out of scope for phase 5 (§3.2, §8).** Three separate gaps, not one: (a) **transport** — UDS for single-host, bootstrap-token credential; (b) **endpoint resolution** — none exists (`remote_grpc_endpoints` is tier 3, #4084's resolver is directory + REST), so even single-host P2, the on-prem baseline, has no working path today; (c) **topology** — process count is no longer a correctness question (§5.8: any process serves any lease), but *backend scope* still is. One cluster process per host is safe **only if every host's process is pointed at the same backend configuration**; point them at per-host backends and locks silently stop being deployment-wide, which looks like a scaling choice and is not one. Say that explicitly in the P2 design and make the shared-backend requirement checkable at startup |
| 13 | Backend credentials — still the platform OoP credential design's call, now narrowed to one deployment target |
| 14 | `ClusterError::ProfileNotBound{profile: &'static str}` interning vs. widening the frozen enum (§5.2) |
**Resolved:**

| # | Question | Resolution |
|---|---|---|
| 18 | **Who sweeps abandoned watch subscriptions?** (§5.4) | **Closed — specified in [§5.4.1](#541-the-watch-subscription-sweep-ask-a6-decision-18) and implemented by `S2`.** The serving gear sweeps, on a 5 s cadence with a 3× grace window, keyed off a `last_seen` that the sweep itself refreshes for any subscription with a live reader — which is what "last poll" means once `await_change` is a stream rather than a long poll. It emits `cluster_subscriptions_reaped` and a `cluster_subscriptions_active` gauge, both per `(profile, primitive)`. Leases need no equivalent, as this question already said. Two things the question did not anticipate: the urgency is not the crashed client but `K2`'s follower pump, which leaves one unattached subscription **per renewal interval** — measured at 4 leaked across five 100 ms intervals (the audit's verified-drift annex); and the alternative fix, `join` reusing the caller's subscription, is unavailable because v1's `TrustedNetwork` mode gives every caller the same name |
| 16 | **Who registers the client, and does the framework want the ordering changed?** (§4.9.2, §4.9.3) | **Closed — all three parts answered by #4084's owner.** (a) *Ownership*: settled by ADR-0004 — the runtime's proxy-wiring phase replays an inventoried `ConsumerRegistration`, and cluster's registration fits that closure signature as shipped. What remains is the platform DESIGN's two contradictory statements, which is a **documentation fix** (§10). (b) *Ordering*: **not deliberate.** The phase body needs only a populated `ClientHub` and a known gear registry, neither of which depends on `start`, so the OoP path will be aligned with the in-process order and will wire before `start`. Cluster keeps its self-construction branch as defence, but inline capability validation becomes the normal case rather than the lucky one (§4.7.1). (c) *gRPC codegen*: **none planned** — the feature is `contract-directory-rest-client` and generates a REST client only. But `ConsumerRegistration` is transport-agnostic, so cluster's hand-written gRPC registration rides the same inventory and replay path and **needs no rework** when a generated gRPC client lands. That answer was given while #4084 was open and cluster planned to hand-roll the client; the PR has since **merged**, so cluster builds on the macros directly and only the hand-written `ConsumerRegistration` remains — which the transport-agnostic type makes permanent-safe rather than interim (§6.1) |

## 10. Documentation Deltas

| Document | Change |
|---|---|
| [cluster PRD.md](./PRD.md) §3.1 | Add the platform's Profile 2 / Profile 3 shapes alongside the five existing ones |
| cluster PRD.md §5.7 `cpt-cf-clst-fr-shutdown-revoke` | Qualify "consumer observes loss before running again" with *if the client is reachable*; unreachable clients fall back to TTL (§4.8) |
| cluster PRD.md §5.3 `cpt-cf-clst-fr-lock-no-remote` | Reword to match the ADR-002 amendment, and update UC-002's "no remote calls inside the critical section" narration, which describes Profile 1 only (§7.2.8) |
| cluster PRD.md §5.6 `cpt-cf-clst-fr-validation-startup-fail` | Restate as "never serves traffic against an unmet requirement", with readiness gating as the Profile 3 mechanism (§4.7) |
| cluster PRD.md §6.1 | New NFRs: remote-transport latency budget tied to `cpt-cf-nfr-oop-latency`; readiness gating; backend-connection count as a function of cluster-gear config, not consumer replica count |
| cluster PRD.md §7 | New interface: the contract traits + wire projection, with their versioning policy |
| **platform** ADR-0009 §Context, §Relationship (from #4426, **merged**) | **Post-merge amendment — the window closed.** ADR-0009 shipped naming the cluster gear as a motivating consumer that must "address a specific backend instance" (`:35`), and assigning topology watch / change-notification to the cluster gear "layered above the directory". Cluster serves no discovery, so `event-broker` is the sole motivating case and no component owns a topology watch — say that, or raise a directory-side watch as a Layer 1.5 item. It already records that cluster discovery is retired (`:412-425`), so this is the remaining half |
| cluster DESIGN.md §2.2 (`CacheEntry`) | "Version is opaque, monotonically increasing per key, starting at 1" holds **while the key exists**. Add the caveat that a delete (including a TTL reap) resets it, so nothing may treat `version` as a durable fence — the lease records carry their own (§5.8.1) |
| [cluster DESIGN.md](./DESIGN.md) §3.15 | Replace "no deployment topology of its own" with the profile-mapped topology |
| cluster DESIGN.md §3.3 | Note the Profile 3 staleness bound on the leader-election contract (§7.3) |
| cluster DESIGN.md §1.3, §3.2 | Add the `grpc-client` feature layer to the layer diagram and component model (§3.4) |
| cluster PRD.md `cpt-cf-clst-constraint-no-serde` | Restate as a constraint on the coordination contract *types*, not on `cluster-sdk`'s dependency graph — which already carries `serde`/`schemars` for GTS scaffolding. Note that nothing enforces it mechanically today (§3.4) |
| cluster DESIGN.md §3.10, ADR-005 | `resolve()` is `async`; record the bounded-descriptor-await contract and the inline-vs-deferred validation paths (§4.7.1) |
| cluster PRD.md §5.6 `cpt-cf-clst-fr-validation-startup-fail` | Already listed above; add that a consumer branching on `CapabilityNotMet` at the resolve site is relying on the **inline** path, which holds whenever cluster is reachable within the resolve timeout but not during a platform cold start, where the pod goes not-ready instead (§4.7.1) |
| `cluster/examples/capability_mismatch.rs` | Teaches "resolution fails **at startup**" as unconditional, which is Profile-1-and-reachable-Profile-3 reasoning. Add the deferred path and the `.await` |
| [ADR-002](./ADR/002-async-boundary-no-remote-in-critical-section.md) | **Amend**: bounded cluster-primitive round trips are permitted inside a critical section; the ban on non-cluster remote I/O stands; the workspace lint is re-scoped to distinguish the two (§7.2.8) |
| [ADR-003](./ADR/003-watch-event-lifecycle-contract.md) | Add "watch events across a process boundary": server buffering → `Lagged`, transport close → `Closed(Provider{ConnectionLost})` → `RestartingWatch` |
| ADR-011 (new) | Remote backend seam: the boundary is the three backend traits; **one `dyn ClusterClient` per process as a factory for them**, local impl winning over remote per the platform's ubiquitous hub convention; profile carried per request and resolved server-side; facades bind lazily so wiring order is not load-bearing; capability enforcement at readiness in Profile 3 (§3.1, §3.5, §4.7.1, §4.9.3). Record the `*Api` / `*Backend` two-trait split as load-bearing (§6.2) |
| ADR-012 (new) | **Store-owned leases and handle lifetime across the boundary** (`cpt-cf-clst-adr-store-owned-leases`). A lease is a fenced record in the backing store — `owner` + `deadline` + `fence` — not a session in a replica, so every lease-keyed operation is a conditional write and **any replica serves any of them**; watch subscriptions remain replica-affine and re-establishable. Renewal is **client-driven**, making the client's own renew both the authority and the consumer-liveness signal, with TTL as the safety net. Record three consequences: a cluster restart or upgrade revokes nothing, the single-replica constraint disappears, and the Postgres liveness beacon goes with it. Record the fence's exact guarantee — no reuse within `fence_retention` of a lease ending, not global monotonicity — and that it is therefore not exposed to consumers. **Rejected alternatives**: a server-side session registry owning lease lifetime (makes the service a permanent singleton and turns every deploy into a revocation event), and keeping the beacon for in-process acquisitions only (same code and config, two reclaim timings, bugs that reproduce in one profile). **Costs to state plainly**: renewal traffic per holder rather than one loop per election, and TTL-bounded reclaim of a crashed holder's lock in every profile (§4.8, §5.8, §6.5–6.6, §7.3) |
| cluster PRD.md §5.7 `cpt-cf-clst-fr-shutdown-revoke` (PRD.md:526) | **Re-state, not just caveat.** Written for the embedded model, where the process shutting down *was* the leader. Under store-owned leases a cluster restart is not a lease loss, and revoking on shutdown would falsely unseat a live leader. New wording: *a leader that loses its lease observes the loss*; cluster shutdown closes subscriptions only (§4.8, §5.8.2) |
| [postgres-cluster-plugin DESIGN.md](../plugins/postgres-cluster-plugin/docs/DESIGN.md) §5.1 | **Remove the liveness-beacon design.** Lock arbitration is `expires_at` plus the fencing token; the advisory-key beacon, its dedicated connection, the shutdown drain and the incarnation-keyed orphan sweep all go, and `0002_cluster_lock.sql` drops the beacon columns. Record what is traded — sub-TTL reclaim of a crashed holder's lock — and why: one lease mechanism across every deployment profile (§5.8.2). Changes merged code, so pair it with the #4411 author |
| ADR-013 (new) | Runtime profile registry + backend instance sharing on canonical options (§5.2, §5.3) |
| ADR-014 (new) | Transport split: gRPC data plane (ADR-0002 opt-in) + REST lifecycle/admin plane; `CanonicalError` as the wire error form and the `#[idempotency]` retry model (§2.2, §6.9, §6.10) |
| ~~ADR-015~~ | **Not written.** Compound cache operations (`increment`, `MultiGet`) are deferred (§7.2.3); the frozen cache contract is not extended. If production shows the hot path needs them, this is the ADR to write, and it applies to Profile 1 equally |
| cluster DESIGN.md §3.15, deployment docs | **Any process linking `cluster` must also link `grpc-hub`** and expose a gRPC port, because the `grpc` capability makes the hub mandatory (`RegistryError::GrpcRequiresHub`). This is a Profile 1 requirement, not only a deployable-gear one, and it applies to §7.2.7's embedding recommendation (§4.2) |
| cluster DESIGN.md §3.10 | Resolve facades in `start`, never `init` — phases are global, so no consumer's `init` can see the cluster gear's `start` registrations (§4.9.1) |
| cluster DESIGN.md §3.8, operator docs | **A profile name carries no authorization.** State that profiles select a backend and do not partition access — two profiles may share one DSN and one table — and that concentrating backend credentials in the cluster pod means any authenticated caller can reach any configured backend through the cluster API. The boundary is the `NetworkPolicy` plus platform-plane authentication until decision 9 lands (§5.3, §7.1) |
| [OBSERVABILITY.md](./OBSERVABILITY.md) | Transport spans/metrics; restate the cardinality rule for session and lock-name data (§7.5) |
| [TESTING-STRATEGY.md](./TESTING-STRATEGY.md) | Conformance-over-the-wire as the primary equivalence gate (§7.6) |
| **platform** [toolkit-oop PRD](../../../../docs/arch/toolkit-oop/PRD.md) §4.2 | Remove "Streaming / SSE over OoP boundaries (future work)" from out-of-scope: the DESIGN already specifies SSE for `#[streaming]` and `#[grpc_contract]` supports native streaming, and cluster's watches are the first consumer (§2.3) |
| **platform** ADR-0008 | Record the ruling: cluster is platform-plane, including for per-tenant data such as OAGW's counters; tenant isolation of cluster data is the caller's responsibility via key scoping (§4.6) |
| **platform** [ADR-0005](../../../../docs/arch/toolkit-oop/ADR/0005-cpt-cf-adr-eventual-readiness.md) | Record the considered exception: a **permanent** configuration error escalates from `Unhealthy` to process exit after a five-minute grace period, so it surfaces as `CrashLoopBackOff`. Transient dependency failures never escalate — that part of ADR-0005 is untouched (§4.7) |
| **platform** [toolkit-oop DESIGN](../../../../docs/arch/toolkit-oop/DESIGN.md) | Reconcile "the gear's `init` picks the transport" with "the bootstrap wires clients on dep resolution"; specify that an SDK may submit its own `ConsumerRegistration`, and note that the generated client is REST-only (decision 16, §4.9.2) |
| **platform** toolkit-contract-binding ADR-0004 (PR #4084) | **Confirmed by its owner — record it.** An SDK may submit its own `ConsumerRegistration` directly when `#[toolkit::consumes]` cannot generate its client, and the macro's generated client is REST-only (`contract-directory-rest-client`) with **no gRPC counterpart planned** — so every platform-plane gRPC consumer takes that path. Record that `ConsumerRegistration` is **transport-agnostic** (owner gear, dep, wire closure), so a hand-written gRPC registration needs no rework when a generated gRPC client eventually lands, and that the intended sequencing is cluster-proves-the-shape then lift-into-the-macro. Note the topology rule (no injected topo-sort dep) prominently: it is easy to read the attribute as replacing an explicit `deps` entry. Cluster needs no closure-signature change (decision 16, §4.9.3) |
| **platform** toolkit-oop DESIGN + #4084 `host_runtime.rs` | **Proxy-wiring ordering — resolved, record the outcome.** The divergence (OoP replays after `run_start_phase`, in-process after `init`) is incidental, and the OoP path is being **aligned** to wire before `start`. Document the single guaranteed order and state what a consumer may assume at `start` — the DESIGN currently describes only the in-process order, so every gear on the macro-generated wiring inherits the ambiguity. Cluster works either way (§4.7.1), but records that inline capability validation is the normal case once aligned (decision 16) |
| **platform** OoP runtime — `bootstrap/oop.rs:542` | **Lift the eager DirectoryService connect.** `run_oop_with_options` connects and propagates the failure, so no OoP gear starts without a directory — on `main` today, and the gate that actually blocks `cpt-cf-fr-k8s-native`'s "DirectoryService MUST NOT be required in k8s". This is the OoP runtime owner's, not #4084's, and a wiring-phase DNS fallback is unreachable until it lands (decision 21) |
| **platform** PRD (`cpt-cf-fr-k8s-native`) + #4084 wiring phase | **Specify the port convention for DNS-resolved dep endpoints, then add the fallback resolver.** The directory returns a full base URI; DNS returns a name only, so a `DnsEndpointResolver` needs a per-dep key, a platform-wide default, or a directory read-back. The convention is a **PRD** decision; the resolver itself is cheap and its owner has agreed to it. Note also that the phase's `NullEndpointResolver`/503 behaviour is **deliberate and must not be weakened** — the fix is a resolver that resolves, not a phase that skips (decision 21) |
| **platform** toolkit-contract-binding ADR-0004 — static override | Record `gears.<owner>.config.consumer_wiring.<dep>` (`StaticEndpointResolver`, `discovery.rs:118`) as the **current** mechanism for a directory-less k8s deployment, pinnable from Helm values — not as a dev/test escape hatch. Reconsider the `warn!` log level if it is a supported production path (§4.5, decision 21) |
| **platform** [ADR-0006](../../../../docs/arch/toolkit-oop/ADR/) | **Correct the caching claim**: the ADR states TokenReview is cached; `K8sTokenReviewAuthenticator::authenticate()` is a bare `self.review(token).await` with no cache in the crate — verified twice. Then record two consequences: the **inbound** platform-plane gRPC auth interceptor is in scope of the two-plane work (no such path exists in `main`, #4403 or #4084), and it must land **together with** the mTLS+SPIFFE phase, since an interceptor over an uncached TokenReview leaves a 10k-ops/s data plane unusable (§4.6, §7.2.5, decision 5) |
| **platform** [PR #4426](https://github.com/constructorfabric/gears-rust/pull/4426) ADR-0009 §1 | Sequence the topology question against this document — **stating the premise correctly**, because an earlier draft of this row said "§5.8 mandates a single replica" and that is no longer true. §5.8's store-owned leases mean **any replica serves any lease operation**; `replicaCount: 1` is a shipped default awaiting the phase-6 failover suite (§5.8.3), not a design constraint. So the tension with #4426's "sharded cluster gear" is **not** singleton-vs-sharded: cluster scales by adding interchangeable replicas, and *per-shard targeting* is the routing model it does not adopt, since sharding leases across instances would reintroduce the affinity §5.8 removed. Cluster is an **entrypoint** role because nothing addresses a specific instance — at any replica count — so the `ClusterIP`/headless question is settled independently of topology (§4.10, §6.7). Record which lands first and what the other assumes. The discovery-consumer and topology-watch edits are the separate row above |
| This document, §2.0 | **Discharged for #4084 / #4403 / #4426 / #4490** — re-verified at `28528310` and §2.0 rewritten accordingly, with §3.2, §4.5, §4.7.1, §4.9.2, §4.9.3, §4.11, §6.1, §8 and §9 updated to match. Standing instruction for future platform merges: re-check here first, because API drift lands in this table before anywhere else |
| New cluster feature docs | `013-contract-and-wire`, `014-runtime-profile-registry`, `015-deployable-gear-bootstrap`, `016-remote-backend-clients` |

## 11. Appendix — A Consumer and the Gear, End to End

This appendix is the concrete counterpart to §3.1's transparency claim. It shows one consumer gear with a public REST endpoint that coordinates through the facades, then the cluster gear that serves it, then what differs between the two deployment profiles. Signatures follow the current SDK (`cluster-sdk/src/{cache,lock,leader}/facade.rs`) and the REST idiom of `nodes-registry`.

### 11.1 Consumer side

The profile name is typed in exactly two places — the marker, and the `.profile()` call that uses it:

```rust
// reservations/src/cluster_profile.rs
#[derive(Clone, Copy)]
pub struct ReservationsProfile;
impl ClusterProfile for ReservationsProfile { const NAME: &'static str = "reservations"; }
cluster_sdk::register_cluster_profile!(ReservationsProfile);      // inventory entry (§4.9.2)
```

Facades are resolved in `start` and stored; nothing here names a transport:

```rust
// reservations/src/gear.rs
#[toolkit::gear(name = "reservations", capabilities = [rest, stateful])]   // no `deps = [cluster]` (4.9.2)
struct ReservationsGear { hub: OnceLock<Arc<ClientHub>>, service: OnceLock<Arc<Service>> }

#[async_trait]
impl RunnableCapability for ReservationsGear {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        let hub = self.hub.get().expect("init ran first");

        let cache = ClusterCacheV1::resolver(hub)
            .profile(ReservationsProfile)
            .require(CacheCapability::Linearizable)     // enforced per §4.7.1
            .resolve().await?;
        let locks = DistributedLockV1::resolver(hub)
            .profile(ReservationsProfile).resolve().await?;
        let elections = LeaderElectionV1::resolver(hub)
            .profile(ReservationsProfile).resolve().await?;

        let svc = Arc::new(Service::new(cache.scoped("reservations")?, locks));
        let _ = self.service.set(Arc::clone(&svc));

        // Exactly one consumer replica runs the reconciler. The lease is store-owned,
        // so leadership survives a cluster restart; renewal is client-driven in both
        // profiles, which is what makes a wedged holder lose its claim (§7.3, §5.8).
        let watch = elections.elect("reservation-reconciler").await?;
        tokio::spawn(async move {
            watch.run_while_leader(Duration::from_secs(5), move || {
                let svc = Arc::clone(&svc);
                async move { svc.reconcile_expired_holds().await }
            }).await;
        });
        Ok(())
    }
}
```

**What `resolve()` finds in this gear's hub is one `dyn ClusterClient`, in both profiles.** That is the whole of the difference, and it is worth following explicitly rather than inferring it from §11.2:

| | Profile 1 (monolith) | Profile 3 (own pod) |
|---|---|---|
| Who registered `dyn ClusterClient` | The cluster gear's `start`, in this same process — a `LocalClusterClient` over the `ProfileRegistry` (§11.2) | The framework's proxy-wiring phase, replaying cluster-sdk's `ConsumerRegistration` — a `RemoteClusterClient` over the gRPC channel (§4.9.3) |
| What `resolve()` does | `hub.get::<dyn ClusterClient>()`, then `cache_backend("reservations")` → the **real** `PostgresCacheBackend` out of the registry, no wrapper | The identical two calls → a `RemoteCacheBackend`, an `Arc` clone plus an interned name |
| Descriptor for `require(Linearizable)` | Intrinsic — the bound object *is* the real backend | From the prefetch cache, or one `DescribeProfiles` under the bounded timeout (§4.7.1) |

**No cluster backend is ever registered into a *consumer's* hub in Profile 3, and none needs to be.** §11.2's `register_*_backend` calls populate the **cluster gear's** hub, which is this consumer's hub only in Profile 1, where the two gears share a process. In Profile 3 they are different processes with different hubs, and a peer cannot insert into another's (§4.9.1) — which is why the backend is *derived* from the client rather than registered.

Two things make this one code path rather than two. The `ClusterClient` trait is **unfeatured**, so the resolve path carries no `#[cfg]` and no fallback branch; and the embedded/remote decision was already made, once, by the local-wins check in the registration (§4.9.3). What varies between the profiles is which impl answered — never what this gear's source does, and never a flag it sets.

The facade this returns holds `Arc<ClientHub>`, the profile and a `OnceLock` for the backend rather than the backend itself, so a `resolve()` that ran before the client was registered still succeeds and binds on first use (§4.9.1). Steady state is one atomic load per call.

The request path takes a lock and reads/writes coordination state:

```rust
// reservations/src/domain/service.rs
pub async fn hold_seat(&self, seat_id: &str, who: &str) -> Result<HoldOutcome, ClusterError> {
    let guard = match self.locks.try_lock(&format!("seat/{seat_id}"), self.lock_ttl).await {
        Ok(g) => g,
        Err(ClusterError::LockContended) => return Ok(HoldOutcome::Busy),
        Err(e) => return Err(e),
    };
    let key = format!("hold/{seat_id}");
    let outcome = match self.cache.get(&key).await? {
        Some(_) => HoldOutcome::AlreadyHeld,
        None => {
            self.cache.put(PutRequest { key: &key, value: who.as_bytes(), ttl: self.hold_ttl }).await?;
            HoldOutcome::Granted
        }
    };
    guard.release().await?;
    Ok(outcome)
}
```

and is published as this gear's own edge-facing endpoint:

```rust
// reservations/src/api/rest/routes.rs
router = OperationBuilder::<Missing, Missing, ()>::post("/reservations/v1/seats/{seat_id}/hold")
    .operation_id("reservations.hold_seat")
    .tag(API_TAG)
    .authenticated()        // auth axis
    .exposed()              // visibility axis — edge-facing (Goal 6, PR #4403)
    .path_param("seat_id", "Seat identifier")
    .handler(handlers::hold_seat)
    .json_response_with_schema::<HoldDto>(openapi, http::StatusCode::OK, "Hold outcome")
    .error_409(openapi).error_500(openapi)
    .register(router, openapi);
router.layer(Extension(service))
```

Two planes meet in one request: the inbound call is **tenant-plane** REST carrying a JWT, while the coordination beneath it is **platform-plane** gRPC to cluster (§4.6). The handler never sees the second.

Two caveats this example deliberately exposes. `hold_seat` makes cluster calls *inside* the critical section — local in Profile 1, two bounded remote round trips in Profile 3, which the amended ADR-002 permits (§7.2.8). What the amendment does not do is make the section free: keep it short, and use pattern C where correctness depends on it. And `release()` runs on the happy path only, so an early `?` leaks the lock until TTL — contract-legal (§6.5) but worth a `Drop`-based wrapper in real code.

### 11.2 Gear side

The gear carries four capabilities, and the phase order dictates what its services may capture:

```rust
// cluster/src/gear.rs
#[toolkit::gear(name = "cluster", capabilities = [stateful, system, grpc, rest])]
struct ClusterGear {
    hub: OnceLock<Arc<ClientHub>>,
    config: OnceLock<ClusterConfig>,
    profiles: OnceLock<Arc<ProfileRegistry>>,   // created in init, POPULATED in start
    sessions: OnceLock<Arc<SessionRegistry>>,
    handle: Mutex<Option<ClusterHandle>>,
}

#[async_trait]
impl GrpcServiceCapability for ClusterGear {
    async fn get_grpc_services(&self, _ctx: &GearCtx) -> anyhow::Result<Vec<RegisterGrpcServiceFn>> {
        let p = Arc::clone(self.profiles.get().unwrap());
        let s = Arc::clone(self.sessions.get().unwrap());
        Ok(vec![ /* cache, lock, leader, profile — each capturing p (and s) */ ])
    }
}

#[async_trait]
impl RunnableCapability for ClusterGear {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        let hub = self.hub.get().expect("init ran first");
        // from_config registers each profile's three backends in the hub under
        // `cluster:{profile}` and returns the bound set for the registry (§4.11
        // item 7b). The framework registers nothing on cluster's behalf; it only
        // hands the gear its hub in `init`.
        let (handle, bound) = ClusterWiring::from_config(
            Arc::clone(hub),
            self.config.get().expect("init ran first"),
            &Self::provider_registry(),
        ).await?;
        let profiles = self.profiles.get().unwrap();
        profiles.publish(bound);            // ArcSwap swap — RPCs start succeeding

        // Profile 1's half of the seam: consumers in THIS process resolve through
        // `dyn ClusterClient`, and this is the impl the local-wins check finds, so
        // no remote client is ever registered here (§3.1, §4.9.3). In Profile 3
        // this gear is alone in its pod and nothing resolves against it locally —
        // the registration is harmless there, not conditional.
        hub.register::<dyn ClusterClient>(Arc::new(LocalClusterClient::new(
            Arc::clone(profiles),
        )));

        *self.handle.lock() = Some(handle);
        Ok(())
    }
}
```

`get_grpc_services` runs in phase 6 and `healthcheck()` is collected in phase 5, but backends exist only after phase 7 (`host_runtime.rs:847`) — so neither may capture a backend, and both capture the registry instead (§4.2). An RPC arriving before `publish` gets `ProfileNotBound`.

**Hub registration is cluster's own work, not the framework's.** `from_config` passes the hub down to `register_{cache,leader_election,lock}_backend`, each of which calls `hub.register_scoped::<dyn _Backend>(cluster:{profile}, backend)` (`cluster-sdk/src/registration.rs:44-52`; call sites `cluster/src/wiring.rs:376-379`). Those registrations stay exactly as they are — the gear's own SDK-default backends resolve through them, and the `ProfileRegistry` is an additional index rather than a replacement (§5.2). Two changes sit on top: `from_config` must also *return* the bound set, since the registry needs per-profile data the hub cannot enumerate; and `start` registers the `LocalClusterClient` that consumer-side `resolve()` calls actually go through.

Note what is **not** conditional here. The gear does not know or care which deployment profile it is in, and neither branch of §11.1's table is expressed in this code. It registers its local client unconditionally; whether any consumer in this process finds it is a property of what the binary linked.

A stateless service is four steps — authenticate, resolve the profile, dispatch, map the error:

```rust
// cluster/src/api/grpc/cache.rs — hand-written over the SDK's generated *_server trait (§6.1)
#[tonic::async_trait]
impl ClusterCacheApi for CacheService {
    async fn get(&self, req: Request<stubs::GetRequest>)
        -> Result<Response<stubs::GetResponse>, Status>
    {
        let _caller = platform_identity(req.metadata())?;              // §4.6
        let req = req.into_inner();
        let bound = self.profiles.resolve(&req.profile).map_err(to_status)?;   // §5.2
        let entry = bound.cache.get(&req.key).await.map_err(to_status)?;
        Ok(Response::new(entry.into()))
    }
}
```

A lease-bearing service owns **no** server-side lease state: the token the client presents is the whole authority, so the handler is a conditional write (§5.8.1):

```rust
// cluster/src/api/grpc/lock.rs
async fn try_lock(&self, req: Request<stubs::TryLockRequest>)
    -> Result<Response<stubs::LockAcquired>, Status>
{
    let caller = platform_identity(req.metadata())?;
    let r = req.into_inner();
    let bound = self.profiles.resolve(&r.profile).map_err(to_status)?;
    // Insert-or-steal-if-expired, bumping `fence`. The returned token is
    // (name, owner, fence) — no table behind it, so any replica can honour it.
    let token = bound.lock.acquire(&r.name, caller.clone(), r.ttl.into())
        .await.map_err(to_status)?;
    self.sessions.index_lease(caller, bound.name, &r.name, &token);  // diagnostics only
    Ok(Response::new(stubs::LockAcquired { token: token.into() }))
}

async fn release(&self, req: Request<stubs::LeaseRef>) -> Result<Response<()>, Status> {
    let caller = platform_identity(req.metadata())?;
    let r = req.into_inner();
    let bound = self.profiles.resolve(&r.profile).map_err(to_status)?;
    // Ownership is the row predicate: a token whose owner or fence
    // does not match matches zero rows, which is indistinguishable from an
    // already-released lease — so tokens are not probeable, and `Ok` is correct
    // either way (idempotent by absence, §6.10).
    bound.lock.release(&r.token.try_into().map_err(to_status)?, &caller)
        .await.map_err(to_status)?;
    Ok(Response::new(()))
}
```

Leader election has the same shape: `Join` performs the fenced acquire and returns `(lease_token, election_id)`; `Resign { lease_token }` is a conditional delete; `AwaitChange { election_id }` long-polls the *subscription*, which is the one piece of genuinely replica-local state (§5.4).

`DescribeProfiles` is what makes the consumer's `require(Linearizable)` answerable:

```rust
// cluster/src/api/grpc/profile.rs
let snap = self.profiles.snapshot();
Ok(Response::new(stubs::DescribeResponse {
    generation: snap.generation,
    profiles: snap.profiles.values().map(|p| p.descriptor.clone().into()).collect(),
}))
```

REST carries operability only — bootstrap probes plus `/admin/{profiles,sessions}` and `/admin/profiles/reload`, built `.authenticated()` and **without** `.exposed()` so they stay internal (§6.7). `healthcheck()` returns `ClusterReadiness`, which probes every bound profile and reports `Degraded` — not `Unhealthy` — when one profile's backend is unreachable (§4.4). No primitive is served over REST.

> **Open: subscription reaping (§9 decision 18).** Nothing above needs a lease reaper — a lease expires in the store when its holder stops renewing (§5.8.1). Watch subscriptions are the exception: a long-poll subscription is freed by an explicit unsubscribe or a closed stream, and a client that dies sends neither, so the index needs a sweep keyed off each subscription's last poll.

### 11.3 Operator config

```yaml
gears:
  grpc-hub:
    config: { listen_addr: "0.0.0.0:50051" }
  cluster:
    config:
      profiles:
        reservations:
          cache: { provider: postgres, connection_string: "postgres://…/gears" }
          lock:  { provider: postgres, connection_string: "postgres://…/gears" }
          # leader_election omitted → SDK-default CAS-over-cache
          # (cpt-cf-clst-fr-routing-omit-default)
```

Both bindings name one DSN, so `BackendInstanceCache` yields **one** pool (§5.3). The consumer's `Linearizable` requirement holds because the Postgres cache declares it; with `provider: standalone` here, `resolve()` would fail with `CapabilityNotMet { primitive: "cache", capability: "Linearizable", provider: "standalone" }` — naming the server-side provider, not `"remote"` (§5.5).

### 11.4 One call, end to end

`svc.hold_seat("12", …)`:

| Where | What happens |
|---|---|
| consumer | `locks.try_lock("reservations/seat/12", ttl)` on `RemoteLockBackend` |
| wire | `TryLock { profile: "reservations", name: "reservations/seat/12", ttl }`, internal token in metadata |
| cluster | authenticate → `profiles.resolve("reservations")` → `bound.lock.try_lock(..)` → Postgres |
| cluster | conditional insert into the backend row; returns a fenced `lease_token` (§5.8.1) |
| consumer | `LockGuard` carrying the token; `release()` → `Release { lease_token }`, ownership enforced by the row predicate |
| consumer dies | no release arrives; the lease lapses at TTL — same guarantee as in-process (§6.5) |

The `reservations/` prefix is applied client-side by `ScopedCacheBackend`, so the gear sees full keys and needs no scope concept (§7.4).

### 11.5 What actually differs between the profiles

Nothing in §11.1. The difference is the binary and the config:

| | Profile 1 (monolith) | Profile 3 (own pod) |
|---|---|---|
| Consumer gear crate | `cluster-sdk = { workspace = true }`, plus a forwarding feature it leaves off (§3.2) | *identical crate*, forwarding feature enabled by the binary |
| Consumer **binary** crate | links `cluster` + plugins + **`grpc-hub`**, which the `grpc` capability makes mandatory (§4.2) | enables the gear's forwarding feature; links none of them |
| Consumer config | `gears.cluster.config.profiles.…` — DSNs here | none; endpoint from k8s DNS by convention, though the shipped wiring path resolves it from DirectoryService (§4.5, decision 21) |
| Who owns the Postgres pool | this process | the cluster pod only |
| Who registered `dyn ClusterClient` | the co-located cluster gear's `start` | `resolve()` itself, since the proxy-wiring phase runs after `start` in the OoP path; the phase then finds one already there (§4.7.1, §4.9.3) |
| `resolve()` returns | a facade over the real `PostgresCacheBackend` | a facade over a `RemoteCacheBackend` |
| `try_lock` cost | one Postgres round trip | one gRPC hop, then Postgres (§7.2.1) |
| `Linearizable` check | against the real backend, at `resolve()` | against the descriptor, inline at `resolve()` — deferred to readiness only when cluster is unreachable within the timeout (§4.7.1) |
| Reconciler leadership | renewal loop in this process | **same renewal loop, one hop further** — the lease is store-owned, so a cluster restart does not unseat the leader (§7.3, §5.8.2) |

## 12. Appendix — Implementation Sketch

§11 shows the shape; this appendix is the code, in the order it would be written. Signatures are taken from the current tree (`cluster-sdk/src/{cache,lock,leader,discovery}/`) and from #4084's example, and where the existing type does not permit what an earlier section assumed, the constraint is called out rather than papered over — two such cases appear below (§12.6, §12.11).

Nothing here is a new consumer-facing concept: every type is either an existing SDK type, a DTO, or one of the six new objects §4.11 already lists.

### 12.1 The contract and its DTOs — `cluster-sdk/src/{contract,dto}.rs`

```rust
// contract.rs  [feature: grpc-client]
#[toolkit::contract(gear = "cluster", version = "v1")]
pub trait ClusterCacheApi: Send + Sync {
    #[idempotency(SafeRead)]
    async fn get(&self, ctx: &PlatformSecurityContext, req: GetRequest)
        -> Result<GetResponse, CanonicalError>;

    #[idempotency(IdempotentWrite)]
    async fn put(&self, ctx: &PlatformSecurityContext, req: PutRequest)
        -> Result<(), CanonicalError>;

    /// NonIdempotentWrite: a retry after a lost response returns "key existed",
    /// which the lock and election paths read as "someone else won" (§6.10).
    #[idempotency(NonIdempotentWrite)]
    async fn put_if_absent(&self, ctx: &PlatformSecurityContext, req: PutRequest)
        -> Result<PutIfAbsentResponse, CanonicalError>;

    #[idempotency(NonIdempotentWrite)]
    async fn compare_and_swap(&self, ctx: &PlatformSecurityContext, req: CasRequest)
        -> Result<CacheEntryDto, CanonicalError>;

    /// On the wire even though the trait defaults it: the default is a
    /// non-atomic get-then-delete, which is a real race over a network, and the
    /// CAS-based lock/leader release depends on it (§6.3).
    #[idempotency(IdempotentWrite)]
    async fn compare_and_delete(&self, ctx: &PlatformSecurityContext, req: CadRequest)
        -> Result<bool, CanonicalError>;

    /// Paginated on the wire; the trait's `Vec<String>` is reassembled
    /// client-side by looping pages (§6.4).
    #[idempotency(SafeRead)]
    async fn scan_prefix(&self, ctx: &PlatformSecurityContext, req: ScanRequest)
        -> Result<ScanResponse, CanonicalError>;

    #[idempotency(SafeRead)] #[streaming]
    fn watch(&self, ctx: &PlatformSecurityContext, req: WatchRequest)
        -> Result<CacheWatchEventDto, CanonicalError>;
}
```

```rust
// dto.rs  [feature: grpc-client]  — every request carries `profile` (§3.1)
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct GetRequest { pub profile: String, pub key: String }

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct PutRequest {
    pub profile: String,
    pub key: String,
    #[serde(with = "serde_bytes")] pub value: Vec<u8>,   // opaque bytes by contract
    pub ttl_ms: Option<u64>,
    /// Unused in phase 1; present so server-side dedup can land without a wire
    /// break. More valuable on the lease ops than here (§6.10).
    pub client_request_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CacheEntryDto {
    #[serde(with = "serde_bytes")] pub value: Vec<u8>,
    pub version: u64,
    pub expires_at_ms: Option<u64>,
}

/// The whole authority for a lease-keyed operation (§5.8.1). Opaque to the
/// consumer: it is produced by `TryLock` / `Lock` / `Join`, presented on
/// `Renew` / `Release` / `Resign`, and never inspected above the backend seam.
///
/// The fields are named rather than a blob so the server can predicate on them
/// directly and an operator can read one out of a log. `fence` is deliberately
/// NOT surfaced on `LockGuard`: it is monotonic only within `fence_retention`
/// (§5.8.1), which is enough for cluster's own predicates and not enough to
/// promise a third-party resource.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct LeaseToken {
    /// Lock name, election name — the lease's identity within the profile.
    pub name: String,
    /// The acquiring `ClientId`, rendered (§5.4). Two clients never share one.
    pub owner: String,
    /// Bumped on every acquisition of this name, including a steal-on-expiry.
    pub fence: u64,
}

/// Acquisition is the one lease operation that carries no token — it mints one.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TryLockRequest {
    pub profile: String,
    pub name: String,
    pub ttl_ms: u64,
    /// Server-side dedup for the retry that must not happen (§6.10): a lost
    /// response on an acquire is the expensive case, since a blind retry
    /// reports `LockContended` against the caller's own lease.
    pub client_request_id: Option<String>,
}

/// `lock()` is `try_lock` plus server-side waiting; the wait is bounded by
/// `timeout_ms`, and `LockTimeout` on expiry (§6.5).
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct LockRequest {
    pub profile: String,
    pub name: String,
    pub ttl_ms: u64,
    pub timeout_ms: u64,
    pub client_request_id: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct LockAcquired {
    pub token: LeaseToken,
}

/// The request shape shared by every lease-keyed operation. `profile` routes
/// it; the token decides it.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct LeaseRef {
    pub profile: String,
    pub token: LeaseToken,
    /// Renewal only; absent on release/resign.
    pub ttl_ms: Option<u64>,
    /// See `PutRequest` — most valuable here, where a lost response on an
    /// acquire is the expensive case (§6.10).
    pub client_request_id: Option<String>,
}

/// What `DescribeProfiles` returns, and the only thing that makes the sync
/// accessors answerable remotely (§5.5). One per profile, per primitive.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProfileDescriptor {
    pub name: String,
    pub cache: CacheDescriptor,
    pub lock: LockDescriptor,
    pub leader_election: LeaderElectionDescriptor,
    /// Per-profile health, so a consumer of a degraded profile can leave
    /// rotation even though the pod as a whole reports `Degraded`/ready
    /// (§4.4). The gear-granular dep gate cannot express this, and the state is
    /// re-read on a 10 s poll because health moves without a config change.
    pub health: ProfileHealth,
}

/// Mirrors the per-profile dimension of §4.4's composite healthcheck.
#[derive(Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub enum ProfileHealth {
    Serving,
    /// At least one bound backend's `probe()` is failing.
    Degraded,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct CacheDescriptor {
    pub consistency: CacheConsistency,
    pub features: CacheFeatures,
    /// The **server-side** provider ("postgres"), never "remote" (§5.5).
    pub provider: String,
}
```

> **`provider_name()` returns `&'static str`, and the descriptor's provider arrives at runtime.** The remote backend cannot return a borrowed static from an owned `String`, so the provider name must be interned at descriptor-load time — the same `Box::leak` interning §5.2 already proposes for profile names, bounded by the config-declared provider set. Worth noting in the ADR: two independent places now need interning for the same reason, which argues for one small `intern(&str) -> &'static str` helper rather than two ad-hoc leaks.

### 12.2 The error codec — `cluster-sdk/src/convert.rs`

Both directions, and the two rules §6.9 makes non-negotiable: `ProviderErrorKind` travels explicitly, and a lease-keyed `NotFound` is mapped per primitive.

```rust
// Server → wire. Generated by `#[derive(ContractError)]` in the end state;
// written out here because the discriminant-carrying rule is easy to get wrong.
#[derive(ContractError)]
#[error_domain("cluster.v1")]
pub enum ClusterWireError {
    #[error_code("profile_not_bound")]   #[canonical(NotFound)]
    ProfileNotBound { profile: String },

    #[error_code("capability_not_met")]  #[canonical(FailedPrecondition)]
    CapabilityNotMet { primitive: String, capability: String, provider: String },

    #[error_code("lock_contended")]      #[canonical(Aborted)]
    LockContended { name: String },

    #[error_code("cas_conflict")]        #[canonical(Aborted)]
    CasConflict { key: String, current_version: Option<u64> },   // see decision 17a

    #[error_code("provider")]            #[canonical(ServiceUnavailable)]
    Provider { kind: ProviderErrorKind, message: String },
    // …
}
```

```rust
// Wire → client. The non-injective mapping is why this is a real function and
// not a `From` on the canonical variant (§6.9, first bullet).
pub(crate) fn to_cluster_error(problem: Problem, ctx: LeaseContext<'_>) -> ClusterError {
    match ClusterWireError::try_from(problem) {
        Ok(ClusterWireError::Provider { kind, message }) => ClusterError::Provider { kind, message },
        Ok(ClusterWireError::LockContended { name })     => ClusterError::LockContended { name },
        Ok(ClusterWireError::ProfileNotBound { profile }) =>
            ClusterError::ProfileNotBound { profile: intern(&profile) },
        // …
        // An unknown (domain, code) pair bounces back as the original Problem
        // (§6.11 skew rule) — never a panic, never a silent Ok.
        Err(unknown) => ClusterError::Provider {
            kind: ProviderErrorKind::Other,
            message: format!("unrecognised cluster error: {unknown}"),
        },
        // A bare canonical NotFound on a lease-keyed call is the stale-id class
        // of §6.9 — mapped by which operation asked, since ClusterError has no
        // generic NotFound variant.
        Ok(other) if other.canonical() == Canonical::NotFound => match ctx {
            LeaseContext::LockRenew { name }  => ClusterError::LockExpired { name: name.to_owned() },
            LeaseContext::LockRelease         => return ClusterError::none(),  // idempotent by absence
            // Renewal matched nothing: this instance no longer holds the lease.
            // NOT an error to the caller — the pump turns it into Status(Lost)
            // and keeps the subscription open (§6.6, §12.12).
            LeaseContext::ElectionRenew       => return ClusterError::lease_lost(),
            // The *subscription* is gone: its replica went away (§5.8.1).
            LeaseContext::ElectionSubscription => ClusterError::Shutdown,
        },
    }
}
```

**A transport failure with no body** — channel down, pod gone — is synthesised as `Provider { kind: ConnectionLost, .. }` so an unreachable cluster gear is indistinguishable, to a consumer, from an unreachable Postgres (§6.9).

### 12.3 Gear: the profile registry — `cluster/src/registry.rs`

```rust
pub struct ProfileRegistry { inner: ArcSwap<RegistrySnapshot> }

struct RegistrySnapshot { generation: u64, profiles: BTreeMap<String, Arc<BoundProfile>> }

impl ProfileRegistry {
    /// Created in `init` — empty. RPCs arriving before `publish` get
    /// ProfileNotBound, and /readyz says Starting (§4.2 lifecycle constraint).
    pub fn new() -> Self {
        Self { inner: ArcSwap::from_pointee(RegistrySnapshot {
            generation: 0, profiles: BTreeMap::new(),
        })}
    }

    /// Called by `start` once `ClusterWiring::from_config` returns (§4.11 item 7b).
    pub fn publish(&self, bound: Vec<Arc<BoundProfile>>) {
        let generation = self.inner.load().generation + 1;
        self.inner.store(Arc::new(RegistrySnapshot {
            generation,
            profiles: bound.into_iter().map(|p| (p.name.clone(), p)).collect(),
        }));
    }

    /// The request path: one ArcSwap load plus a BTreeMap lookup, no lock.
    /// This is also LocalClusterClient's dispatch, so it is hot in Profile 1 too.
    pub fn resolve(&self, profile: &str) -> Result<Arc<BoundProfile>, ClusterError> {
        self.inner.load().profiles.get(profile).cloned()
            .ok_or(ClusterError::ProfileNotBound { profile: intern(profile) })
    }

    pub fn snapshot(&self) -> Arc<RegistrySnapshot> { self.inner.load_full() }
}
```

And the instance cache that keeps two profiles on one DSN down to one pool (§5.3):

```rust
pub struct BackendInstanceCache {
    // Weak, so an instance's StopHook runs exactly when the last profile
    // referencing it is dropped — which is what makes §5.6 removal safe.
    cache: Mutex<HashMap<InstanceKey, Weak<dyn Any + Send + Sync>>>,
}

#[derive(Hash, PartialEq, Eq)]
struct InstanceKey {
    primitive: Primitive,
    provider: String,
    /// Digest of the options map, sorted keys, AFTER ${VAR} expansion, plus
    /// secret_ref. Deliberately NOT DSN-semantic: a false merge points a
    /// profile at the wrong database, a false split costs one pool (§5.3).
    options_digest: [u8; 32],
}
```

### 12.4 Gear: `LocalClusterClient` — `cluster/src/local_client.rs`

The whole of Profile 1's half of the seam. It lives in the gear crate because it depends on gear state (§3.4).

```rust
pub struct LocalClusterClient { profiles: Arc<ProfileRegistry> }

#[async_trait]
impl ClusterClient for LocalClusterClient {
    fn cache_backend(&self, profile: &str) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError> {
        // The REAL backend, no wrapper — Profile 1 keeps today's exact cost (§3.1).
        Ok(Arc::clone(&self.profiles.resolve(profile)?.cache))
    }
    fn lock_backend(&self, profile: &str) -> Result<Arc<dyn DistributedLockBackend>, ClusterError> {
        Ok(Arc::clone(&self.profiles.resolve(profile)?.lock))
    }
    // leader_election_backend — identical shape

    /// Intrinsic locally: the descriptor was computed at wiring time from the
    /// real backends' own `consistency()` / `features()` / `provider_name()`,
    /// so this never awaits anything (§4.7.1).
    async fn descriptor(&self, profile: &str) -> Result<ProfileDescriptor, ClusterError> {
        Ok(self.profiles.resolve(profile)?.descriptor.clone())
    }
}
```

### 12.5 Gear: the session index — `cluster/src/api/grpc/subscriptions.rs`

> **As built (`S2`), and this sketch lost on three counts.** There is no `session.rs`: the table landed with the service that needs it, at `cluster/src/api/grpc/subscriptions.rs`, with the sweep task beside it in `sweep.rs`. There is no `leases` map — see §5.4's `LeaseIndexEntry` note for why the lease half is not built. And the container is a `Mutex<HashMap<..>>` rather than a `DashMap`: every access is a single point lookup or a whole-table walk, `attach`'s check-and-swap has to be one critical section against a concurrent sweep, and the crate has no other `dashmap` dependency to justify. What survives verbatim is the shape below minus its second field, and both bullets under it.

Leases are **not** here — they are rows in the backing store (§5.8.1). What remains is a watch-subscription table plus a lease *index* for diagnostics, and the ownership check that used to live here is now a `WHERE` clause.

```rust
pub struct SessionRegistry {
    /// The only genuinely replica-local state: a subscription is an open stream
    /// or a long-poll cursor, and losing it costs a `RestartingWatch` cycle.
    watches: DashMap<SubscriptionId, WatchSession>,
    /// Index only. Never read on the lease path — `renew` / `release` predicate
    /// on the stored row, so a replica that never saw the acquire answers
    /// identically. Read by /admin/sessions and the quota accounting.
    leases: DashMap<(Arc<str>, String), LeaseIndexEntry>,   // (profile, name)
}

impl SessionRegistry {
    /// Fire-and-forget bookkeeping: a failure here must never fail the acquire,
    /// because the lease is already durable in the store without it.
    pub fn index_lease(&self, client: ClientId, profile: Arc<str>, name: &str,
                       token: &LeaseToken) {
        self.leases.insert(
            (profile.clone(), name.to_owned()),
            LeaseIndexEntry {
                client, profile, kind: LeaseKind::Lock,
                name: name.to_owned(), fence: token.fence,
                deadline: Instant::now() + token.ttl,
            },
        );
    }
}
```

Two things it does not need, because it does not own lease lifetime:

- **No ownership check in Rust.** Ownership is the row predicate — `WHERE name = $1 AND owner = $2 AND fence = $3` — which fails identically for a stolen token and an expired lease, so tokens stay unprobeable without a lookup table.
- **No deadline sweep for leases.** An entry in `leases` is a cache of something the store already knows, so a stale one is a reporting artefact rather than a leak. Refresh it from the store on read, and let a bounded LRU bound it.

### 12.6 Gear: the service impls — `cluster/src/api/grpc/`

Hand-written over the generated `*_server` traits, which is the sanctioned permanent pattern, not interim glue (§6.1). Three shapes cover all four services.

**Stateless — four steps, no server state:**

```rust
// cache.rs
#[tonic::async_trait]
impl stubs::cluster_cache_api_server::ClusterCacheApi for CacheService {
    async fn get(&self, req: Request<stubs::GetRequest>)
        -> Result<Response<stubs::GetResponse>, Status>
    {
        let _caller = platform_identity(req.metadata())?;                    // §4.6
        let r = req.into_inner();
        let bound = self.profiles.resolve(&r.profile).map_err(to_status)?;   // §5.2
        let entry = bound.cache.get(&r.key).await.map_err(to_status)?;
        Ok(Response::new(entry.into()))
    }

    async fn compare_and_swap(&self, req: Request<stubs::CasRequest>)
        -> Result<Response<stubs::CacheEntryDto>, Status>
    {
        let _caller = platform_identity(req.metadata())?;
        let r = req.into_inner();
        let bound = self.profiles.resolve(&r.profile).map_err(to_status)?;
        // CasConflict's `current` is SHOULD, "if cheaply obtainable" — the
        // version-only form is contract-legal and avoids base64'ing the value
        // on the hot error path (decision 17a).
        let entry = bound.cache
            .compare_and_swap(&r.key, r.expected_version, &r.value, r.ttl())
            .await.map_err(to_status)?;
        Ok(Response::new(entry.into()))
    }
}
```

**Lease-bearing — the same four steps plus registry ownership:**

```rust
// lock.rs — no server-side lease state; the token is the authority (§5.8.1)
async fn try_lock(&self, req: Request<stubs::TryLockRequest>)
    -> Result<Response<stubs::LockAcquired>, Status>
{
    let caller = platform_identity(req.metadata())?;
    let r = req.into_inner();
    let bound = self.profiles.resolve(&r.profile).map_err(to_status)?;
    // Insert-or-steal-if-expired; the backend bumps `fence` on every steal, so a
    // previous holder's token can never match again.
    let token = bound.lock.acquire(&r.name, caller.clone(), r.ttl())
        .await.map_err(to_status)?;
    self.sessions.index_lease(caller, Arc::clone(&bound.name), &r.name, &token);
    Ok(Response::new(stubs::LockAcquired { token: token.into() }))
}

async fn renew(&self, req: Request<stubs::RenewRequest>) -> Result<Response<()>, Status> {
    let caller = platform_identity(req.metadata())?;
    let r = req.into_inner();
    let bound = self.profiles.resolve(&r.profile).map_err(to_status)?;
    // One conditional UPDATE. Zero rows means expired, stolen, or never-yours —
    // all indistinguishable, all `LockExpired` (§6.9). No lookup, so a replica
    // that never saw the acquire answers identically.
    bound.lock.renew(&r.token.try_into().map_err(to_status)?, &caller, r.ttl())
        .await.map_err(to_status)?;
    Ok(Response::new(()))
}

async fn release(&self, req: Request<stubs::LeaseRef>) -> Result<Response<()>, Status> {
    let caller = platform_identity(req.metadata())?;
    let r = req.into_inner();
    let bound = self.profiles.resolve(&r.profile).map_err(to_status)?;
    // Idempotent by absence (§6.10): a retried release, or one bearing a fenced-out
    // token, deletes nothing and returns Ok. Never NotFound, never PermissionDenied.
    bound.lock.release(&r.token.try_into().map_err(to_status)?, &caller)
        .await.map_err(to_status)?;
    Ok(Response::new(()))
}
```

> **Three things the handler does not do**, all of which would follow from owning lease lifetime: look the lease up,
> check ownership in Rust, or maintain a deadline for a sweep. The row predicate does the first two and the store
> holds the third — which is what makes a second replica ordinary (§5.8) and a restart harmless (§5.8.2).

**Long-poll — the projection that makes the leader watch work without streaming:**

```rust
// leader.rs
async fn await_change(&self, req: Request<stubs::AwaitChangeRequest>)
    -> Result<Response<stubs::LeaderWatchEventDto>, Status>
{
    let caller = platform_identity(req.metadata())?;
    let r = req.into_inner();
    let session = self.sessions.borrow_election(&caller, &r.election_id)
        .ok_or_else(|| Status::not_found("unknown election_id"))?;
    // NOT a keepalive: leadership is held by the client's own renew against the
    // lease token (§7.3), so this is purely an observation channel.
    match timeout(r.timeout(), session.watch.changed()).await {
        Ok(Some(event)) => Ok(Response::new(event.into())),
        Ok(None)        => Ok(Response::new(closed(ClusterError::Shutdown))),
        Err(_elapsed)   => Ok(Response::new(stubs::LeaderWatchEventDto::no_change())),
    }
}
```

> **`LeaderWatch::changed()` needs `&mut self`, but the registry holds the watch behind a concurrent map.** `LeaderWatch` owns an `mpsc::Receiver` (`leader/watch.rs`), and receiving is `&mut`. So an election session cannot be a shared borrow the way a `LockGuard` can — it needs a `Mutex<LeaderWatch>` per session, and two concurrent `AwaitChange` calls for one `election_id` must be serialised or rejected. Rejecting is better: a second concurrent poll on one election is a client bug, and serialising it would silently give one of the two callers a stale event. Add it to §6.6 as an explicit rule — *at most one in-flight `AwaitChange` per `election_id`*, second concurrent call gets `FailedPrecondition`.

### 12.7 Gear: the gear — `cluster/src/gear.rs`

```rust
#[toolkit::gear(name = "cluster", capabilities = [stateful, system, grpc, rest])]
struct ClusterGear {
    hub: OnceLock<Arc<ClientHub>>,
    config: OnceLock<ClusterConfig>,
    profiles: OnceLock<Arc<ProfileRegistry>>,   // created in init, POPULATED in start
    sessions: OnceLock<Arc<SessionRegistry>>,
    handle: Mutex<Option<ClusterHandle>>,
}

#[async_trait]
impl Gear for ClusterGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let _ = self.hub.set(ctx.client_hub());
        let _ = self.config.set(ctx.gear_config::<ClusterConfig>()?);
        // Both registries exist from `init`, because phases 5 and 6 capture them
        // before any backend exists (§4.2).
        let _ = self.profiles.set(Arc::new(ProfileRegistry::new()));
        let _ = self.sessions.set(Arc::new(SessionRegistry::new()));
        Ok(())
    }
}

#[async_trait]
impl GrpcServiceCapability for ClusterGear {
    async fn get_grpc_services(&self, _ctx: &GearCtx) -> anyhow::Result<Vec<RegisterGrpcServiceFn>> {
        let p = Arc::clone(self.profiles.get().expect("init ran first"));
        let s = Arc::clone(self.sessions.get().expect("init ran first"));
        Ok(vec![
            register_cache(CacheService::new(Arc::clone(&p))),
            register_lock(LockService::new(Arc::clone(&p), Arc::clone(&s))),
            register_leader(LeaderService::new(Arc::clone(&p), Arc::clone(&s))),
            register_profile(ProfileService::new(p)),
        ])
    }
}

#[async_trait]
impl RestApiCapability for ClusterGear {
    fn register_rest(&self, _ctx: &GearCtx, router: Router, openapi: &dyn OpenApiRegistry)
        -> anyhow::Result<Router>
    {
        // Admin only. No primitive over REST (§6.7), .authenticated() without
        // .exposed() so is_public stays false (Goal 6).
        Ok(crate::api::rest::admin_routes(router, openapi,
            Arc::clone(self.profiles.get().expect("init ran first")),
            Arc::clone(self.sessions.get().expect("init ran first"))))
    }

    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        // Weak: the healthcheck must not keep the registry alive past stop, and
        // it is collected in phase 5, before backends exist (§4.4).
        Some(Arc::new(ClusterReadiness { profiles: Arc::downgrade(self.profiles.get()?) }))
    }
}

#[async_trait]
impl RunnableCapability for ClusterGear {
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let hub = self.hub.get().expect("init ran first");
        let (handle, bound) = ClusterWiring::from_config(
            Arc::clone(hub), self.config.get().unwrap(), &Self::provider_registry(),
        ).await?;

        let profiles = self.profiles.get().unwrap();
        profiles.publish(bound);                    // RPCs start succeeding here

        // Profile 1's half of the seam; harmless in Profile 3 where nothing
        // resolves locally (§11.2).
        hub.register::<dyn ClusterClient>(
            Arc::new(LocalClusterClient::new(Arc::clone(profiles))));

        // Decision 18's sweep — one task for the whole registry.
        let sessions = Arc::clone(self.sessions.get().unwrap());
        tokio::spawn(async move {
            let mut tick = interval(SWEEP_INTERVAL);
            loop {
                select! { _ = tick.tick() => sessions.sweep().await, _ = cancel.cancelled() => break }
            }
        });

        *self.handle.lock() = Some(handle);
        Ok(())
    }

    async fn stop(&self) -> anyhow::Result<()> {
        // §4.8 phase 3: revoke before teardown, so remote subscribers observe
        // Status(Lost) then Closed(Shutdown) rather than a bare transport close.
        self.sessions.get().unwrap().revoke_all(ClusterError::Shutdown).await;
        if let Some(handle) = self.handle.lock().take() { handle.stop().await?; }
        Ok(())
    }
}
```

### 12.8 Gear: the binary — `cluster/src/{main,registered_gears}.rs`

```rust
// registered_gears.rs
#![allow(unused_imports)]
use cluster as _;                    // the gear itself, via inventory (§3.4)
use grpc_hub as _;                   // MANDATORY: `grpc` capability ⇒ hub (§4.2)
use postgres_cluster_plugin as _;
use standalone_cluster_plugin as _;
```

```rust
// main.rs — mirrors examples/oop-gears/calculator/calculator/src/main.rs
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[derive(Parser)]
    #[command(name = "cluster-oop")]
    struct Cli {
        #[arg(short, long)] config: Option<PathBuf>,
        #[arg(short, long, action = ArgAction::Count)] verbose: u8,
        #[command(subcommand)] command: Option<Command>,
    }
    #[derive(Subcommand)]
    enum Command {
        /// §4.10.1: dedupe bindings by the §5.3 instance key, run each
        /// provider's `migrate()` hook once per DSN, exit.
        Migrate,
    }

    let cli = Cli::parse();
    match cli.command {
        Some(Command::Migrate) => cluster::migrate::run(cli.config).await,
        None => run_oop_with_options(OopRunOptions {
            gear_name: "cluster".to_string(),
            verbose: cli.verbose,
            config_path: cli.config,
            ..Default::default()
        }).await,
    }
}
```

Everything else the pod needs — `/healthz`, `/readyz`, `/health`, self-registration with backoff, dependency resolution, the presence loop, drain, DirectoryService deregistration — is supplied by `run_oop_with_options` and is **not cluster's code to write** (§4.3).

### 12.9 Client: `RemoteClusterClient` — `cluster-sdk/src/client/remote.rs`

```rust
pub struct RemoteClusterClient {
    channel: Channel,                        // one per process, multiplexed
    descriptors: Arc<DescriptorCache>,
    policies: PolicyStack,                   // #4084's; a tracing interceptor until then (§6.1, §7.5)
}

impl RemoteClusterClient {
    /// Pure: `connect_lazy` touches no network, which is what lets the
    /// registration run at any point and await nothing (§4.5, §4.9.3).
    pub fn connect_lazy(endpoint: &str) -> Result<Self, ClusterError> {
        let channel = Endpoint::from_shared(endpoint.to_owned())
            .map_err(invalid_endpoint)?
            .connect_timeout(GrpcClientConfig::default().connect_timeout)
            .connect_lazy();
        Ok(Self { channel, descriptors: Arc::new(DescriptorCache::new()), policies: PolicyStack::default() })
    }
}

#[async_trait]
impl ClusterClient for RemoteClusterClient {
    /// Sync and pure in both profiles — an Arc clone plus an interned name.
    /// This is what keeps `resolve()`'s only await the descriptor (§3.1).
    fn cache_backend(&self, profile: &str) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError> {
        Ok(Arc::new(RemoteCacheBackend {
            client: self.clone_handle(),
            profile: intern_arc(profile),
            descriptors: Arc::clone(&self.descriptors),
        }))
    }
    // …

    async fn descriptor(&self, profile: &str) -> Result<ProfileDescriptor, ClusterError> {
        if let Some(d) = self.descriptors.get(profile) { return Ok(d) }
        let resp = self.profile_stub().describe_profiles(DescribeRequest::all()).await?;
        self.descriptors.populate(resp.generation, resp.profiles);   // §5.6 generation
        self.descriptors.get(profile).ok_or(ClusterError::ProfileNotBound { profile: intern(profile) })
    }
}
```

### 12.10 Client: the cache backend — the simple case

```rust
pub(crate) struct RemoteCacheBackend {
    client: Arc<RemoteClusterClient>,
    profile: Arc<str>,
    descriptors: Arc<DescriptorCache>,
}

#[async_trait]
impl ClusterCacheBackend for RemoteCacheBackend {
    // Sync accessors, answered from the descriptor cache — the one piece of
    // profile knowledge that cannot ride on the request (§5.5).
    fn consistency(&self) -> CacheConsistency { self.cache_desc().consistency }
    fn features(&self) -> CacheFeatures       { self.cache_desc().features }
    fn provider_name(&self) -> &'static str   { self.cache_desc().provider }   // interned, §12.1

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        let req = GetRequest { profile: self.profile.to_string(), key: key.to_owned() };
        self.client.policies                                   // spans/metrics (§7.5)
            .run(PolicyContext::unary("ClusterCacheApi", "get", SafeRead), || async {
                self.client.cache_stub().get(req.clone()).await
            })
            .await
            .map(Into::into)
            .map_err(|e| to_cluster_error(e, LeaseContext::None))
    }

    /// The trait returns an unbounded Vec; the wire is paginated (§6.4).
    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        let mut out = Vec::new();
        let mut page = None;
        loop {
            let resp = self.client.cache_stub().scan_prefix(ScanRequest {
                profile: self.profile.to_string(), prefix: prefix.to_owned(), page_token: page,
            }).await.map_err(|e| to_cluster_error(e, LeaseContext::None))?;
            out.extend(resp.keys);
            match resp.next_page_token { Some(t) => page = Some(t), None => break }
        }
        Ok(out)
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        let (tx, watch) = CacheWatch::channel(WATCH_BUFFER);
        let mut stream = self.client.cache_stub().watch(WatchRequest {
            profile: self.profile.to_string(), key: key.to_owned(),
        }).await?.into_inner();
        // One pump task per watch. try_send + Lagged mirrors the in-process
        // sender's behaviour exactly, so one slow consumer cannot stall the
        // shared server-side subscription (§6.8).
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(dto) => { let _ = tx.try_send(dto.into()); }
                    Err(status) => {
                        // Transport loss ⇒ retryable ⇒ RestartingWatch resubscribes.
                        let _ = tx.send_closed(to_cluster_error(status.into(), LeaseContext::None)).await;
                        break;
                    }
                }
            }
        });
        Ok(watch)
    }
}
```

### 12.11 Client: the lock backend

```rust
#[async_trait]
impl DistributedLockBackend for RemoteLockBackend {
    async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError> {
        let acquired = self.client.lock_stub().try_lock(TryLockRequest {
            profile: self.profile.to_string(), name: name.to_owned(), ttl_ms: ttl.as_millis() as u64,
            client_request_id: Some(Uuid::new_v4().to_string()),   // §6.10
        }).await.map_err(|e| to_cluster_error(e, LeaseContext::None))?;

        // `LockGuard` has exactly one public constructor and two private fields
        // (`name`, `commands`), so the lease token lives in the pump's closure
        // rather than in the guard (§12.17).
        let (mut commands, guard) = LockGuard::channel(name.to_owned(), 1);
        let (client, token, name) = (self.client.clone_handle(), acquired.token, name.to_owned());
        tokio::spawn(async move {
            while let Some(cmd) = commands.recv().await {
                match cmd {
                    LockCommand::Renew { new_ttl, reply } => {
                        let r = client.lock_stub().renew(RenewRequest { token: token.clone(), ttl_ms: … })
                            .await.map_err(|e| to_cluster_error(e, LeaseContext::LockRenew { name: &name }));
                        let _ = reply.send(r);
                    }
                    LockCommand::Release { reply } => {
                        let r = client.lock_stub().release(LeaseRef { profile: profile.clone(), token: token.clone(), ttl_ms: None, client_request_id: None })
                            .await.map_err(|e| to_cluster_error(e, LeaseContext::LockRelease));
                        let _ = reply.send(r);
                        break;                       // release consumes the guard
                    }
                }
            }
            // Channel closed without a release: the guard was dropped. No I/O —
            // the lease lapses at TTL, exactly as in-process (guard.rs module doc).
        });
        Ok(guard)
    }
}
```

> **Two properties this shape carries, both verified against `lock/guard.rs`.**
>
> 1. **The token cannot live in the guard.** `LockGuard` is `{ name: String, commands: mpsc::Sender<LockCommand> }` with both fields private and `LockGuard::channel(name, buffer)` as its only public constructor, so the pump's closure holds the token and the guard addresses it over the channel (§12.17). Widening `LockGuard` with an opaque `lease` field would remove the task but touch a frozen consumer-facing type for one backend's benefit.
> 2. **One tokio task per held lock, client-side.** The in-process backends already pay this, so it is not a regression, but §7.2.5's cost list says so plainly: a consumer holding many concurrent locks holds many pumps.

### 12.12 Client: leader election — event pump plus resign

```rust
async fn elect(&self, name: &str) -> Result<LeaderWatch, ClusterError> {
    let joined = self.client.leader_stub().join(JoinRequest {
        profile: self.profile.to_string(), name: name.to_owned(), config: None,
    }).await.map_err(|e| to_cluster_error(e, LeaseContext::None))?;

    let (events, mut resigns, watch) =
        LeaderWatch::channel(WATCH_BUFFER, joined.initial_status.into());

    let (client, id, token) =
        (self.client.clone_handle(), joined.election_id, joined.token);
    tokio::spawn(async move {
        // Client-driven renewal is what keeps the claim alive (§7.3); the poll below
        // observes status and carries no authority.
        let mut renew_tick = interval(RENEWAL_INTERVAL);
        loop {
            select! {
                _ = renew_tick.tick() => {
                    match client.leader_stub()
                        .renew(RenewRequest { token: token.clone(), ttl_ms: LEASE_TTL_MS }).await
                    {
                        Ok(_) => {}
                        // Matched nothing: stolen after expiry, or fenced by a newer
                        // holder. That is loss of leadership, which is a STATUS event —
                        // the election continues and this instance may win it again, so
                        // the subscription stays open (§6.6).
                        Err(s) if is_lease_lost(&s) => {
                            let _ = events.send(LeaderStatus::Lost.into()).await;
                        }
                        // A transport failure is not evidence about the lease. Keep
                        // renewing; the deadline is the only thing that ends it.
                        Err(s) => warn!(error = %s, "leader renewal failed; retrying"),
                    }
                },
                // At most one in-flight poll per election_id (§6.6).
                res = client.leader_stub().await_change(AwaitChangeRequest {
                    election_id: id.clone(), timeout_ms: POLL_TIMEOUT_MS,
                }) => match res {
                    Ok(dto) if dto.is_no_change() => continue,
                    Ok(dto)  => { let _ = events.send(dto.into()).await; }
                    Err(status) => {
                        let err = to_cluster_error(status.into(), LeaseContext::ElectionSubscription);
                        let _ = events.send_closed(err).await;   // Shutdown ⇒ terminal
                        break;
                    }
                },
                Some(resign) = resigns.recv() => {
                    let r = client.leader_stub().resign(LeaseRef { profile: profile.clone(), token: token.clone(), ttl_ms: None, client_request_id: None })
                        .await.map_err(|e| to_cluster_error(e, LeaseContext::ElectionSubscription));
                    resign.reply(r);
                    break;
                }
            }
        }
    });
    Ok(watch)
}
```

`status()` and `is_leader()` need no call at all — they read the `watch::Receiver` snapshot the sender updates, exactly as in-process (§6.6).

### 12.13 Client: the requirement registry and readiness — `cluster-sdk/src/requirements.rs`

**Unfeatured**, because Profile 1 needs it and the wiring closure never runs there (§4.9.1).

```rust
/// Process-global. Every `resolve()` records into it; it is also the readiness
/// contributor, registered on first use.
pub(crate) struct RequirementRegistry {
    recorded: Mutex<Vec<Recorded>>,
    first_resolve_at: OnceLock<Instant>,
    client_seen: AtomicBool,
}

struct Recorded { profile: Arc<str>, primitive: Primitive, requirements: Requirements }

impl RequirementRegistry {
    pub fn record(&self, r: Recorded) {
        let _ = self.first_resolve_at.set(Instant::now());
        self.recorded.lock().push(r);
    }

    /// Three verdicts, and the third is the one that only exists because
    /// binding is lazy (§4.9.1).
    pub fn readiness(&self, descriptors: &DescriptorCache) -> HealthStatus {
        // 1. A recorded requirement the descriptor does not satisfy — permanent.
        for r in self.recorded.lock().iter() {
            if let Some(d) = descriptors.get(&r.profile) {
                if let Err(e) = validate(&r.primitive, d, &r.requirements) {
                    return HealthStatus::Unhealthy(e.to_string());   // same triple as inline
                }
            }
        }
        // 2. Nothing wired at all, past the grace window — a build/config error
        //    that `resolve()` could not distinguish from a cold start (§4.7).
        if !self.client_seen.load(Relaxed)
            && self.first_resolve_at.get().is_some_and(|t| t.elapsed() > NOT_WIRED_GRACE)
        {
            return HealthStatus::Unhealthy(
                "no `dyn ClusterClient` registered: is the `cluster` gear linked, \
                 or the client feature enabled?".into());
        }
        // 3. A recorded profile the server reports as degraded (§4.4). Distinct
        //    from (1) in lifetime, not in verdict: this one clears itself when
        //    the backend recovers, so it must NOT feed §4.7's escalation to
        //    process exit — only the permanent verdicts do.
        for r in self.recorded.lock().iter() {
            if descriptors.get(&r.profile).is_some_and(|d| d.health != ProfileHealth::Serving) {
                return HealthStatus::Unhealthy(
                    format!("profile `{}` is degraded server-side", r.profile)).transient();
            }
        }
        // 4. Descriptors still landing — ordinary cold start.
        if self.recorded.lock().iter().any(|r| descriptors.get(&r.profile).is_none()) {
            return HealthStatus::Starting;
        }
        HealthStatus::Ready
    }
}
```

The contributor is driven by the descriptor refresh, not by the readiness probe: a **10 s poll** re-issues `DescribeProfiles` for every recorded profile (§5.5), which is what makes verdict 3 both reachable and self-clearing. Verdicts 1 and 2 are permanent and carry §4.7's five-minute escalation to process exit; verdict 3 never does, which is why it is tagged rather than merely worded differently — a transient backend outage must not crash-loop the fleet.

### 12.14 Client: the resolver — `cluster-sdk/src/cache/resolver.rs`

The four steps of §4.9.3, replacing today's terminal hub read.

```rust
pub async fn resolve(self) -> Result<ClusterCacheV1, ClusterError> {
    let profile = self.profile_name.ok_or(ClusterError::ProfileNotSpecified)?;
    validate_name(profile)?;

    // 1. The client, not the backend. NOTE: `ClientHub` has `get`/`get_scoped`/
    //    `try_get_scoped` but NO unscoped `try_get` (verified,
    //    libs/toolkit/src/client_hub.rs:142-248), so this is `.get().ok()`.
    let client = self.hub.get::<dyn ClusterClient>().ok()
        // Nothing registered yet: under `grpc-client` the proxy-wiring phase has
        // simply not run (it is replayed after `start`, §2.0), so build the
        // client here rather than deferring validation. Unfeatured, this arm
        // does not exist and absence is tolerated — the facade binds on first
        // use and readiness reports the build mistake (§4.7.1, §4.9.1).
        .or_else(|| self_construct_remote(&self.hub).ok());
    requirements().set_client_seen(client.is_some());

    let backend = match &client {
        // 2. Sync, pure, no I/O — the real backend locally, a handle remotely.
        Some(c) => Some(c.cache_backend(profile)?),
        None    => None,
    };

    // 4. Record first, so §5.6's refresh and the deferred path both have it.
    requirements().record(Recorded { profile: intern_arc(profile), primitive: Primitive::Cache,
                                     requirements: self.requirements.clone() });

    // 3. Bounded descriptor await; validate inline when it lands (§4.7.1).
    if let Some(c) = &client {
        match timeout(RESOLVE_DESCRIPTOR_TIMEOUT, c.descriptor(profile)).await {
            Ok(Ok(d)) => {
                validate_cache_capabilities_from(&d.cache, &self.requirements)?;   // loud, inline
                tracing::info!(profile, "cluster resolve: validated inline");
            }
            Ok(Err(e)) if e.is_permanent() => return Err(e),      // ProfileNotBound
            _ => tracing::info!(profile, "cluster resolve: validation deferred to readiness"),
        }
    }

    Ok(ClusterCacheV1::lazy(Arc::clone(&self.hub), intern_arc(profile), backend))
}
```

And the facade holds a slot rather than a backend, which is what makes wiring order a non-issue:

```rust
pub struct ClusterCacheV1 {
    hub: Arc<ClientHub>,
    profile: Arc<str>,
    backend: OnceLock<Arc<dyn ClusterCacheBackend>>,
}

impl ClusterCacheV1 {
    /// Steady state: one atomic load. Cold: one hub lookup, once.
    fn backend(&self) -> Result<&Arc<dyn ClusterCacheBackend>, ClusterError> {
        if let Some(b) = self.backend.get() { return Ok(b) }
        let client = self.hub.get::<dyn ClusterClient>()
            .map_err(|_| ClusterError::ProfileNotBound { profile: intern(&self.profile) })?;
        let _ = self.backend.set(client.cache_backend(&self.profile)?);
        Ok(self.backend.get().expect("just set"))
    }
}
```

### 12.15 Client: the registration — `cluster-sdk/src/wiring.rs`

**Shipped** (item `K3`). The sketch below predates the merged type and diverges from it in three ways, all recorded in
the audit's verified-drift annex; read the file, not this block:

- the fields are **`owner_gear` / `dep_gear`**, not `owner_module` / `dep_module`;
- `wire` receives an **`Arc<dyn EndpointResolver>`**, not a resolved `&str` — and `resolve_endpoint` is `async` while
  `wire` is a sync `fn`, so the resolver cannot be consulted inside the closure at all. Cluster ignores it and derives
  its own endpoint, which also avoids handing a **REST** base URI to a gRPC channel. The cost is that the ADR-0004
  static override does not reach cluster;
- the local-wins probe is **`try_get_local`**, not `get(..).is_ok()`, and the reason is sharper than telling a local
  impl from a proxy: this registration is replayed in the `cluster-oop` process too (the gear links `cluster-sdk` with
  `grpc-client` on), so without it a cluster-hosting process wires a remote client to its own socket. What makes the
  probe answer correctly is that the gear claims `dyn ClusterClient` in **`init`** — one phase before the wiring
  phase — rather than in `start`.

```rust
inventory::submit! {
    ConsumerRegistration {
        owner_module: "cluster-sdk",
        dep_module:   "cluster",
        wire: |hub: &ClientHub, endpoint: &str| -> anyhow::Result<WireOutcome> {
            // Local wins: a co-located cluster gear already registered its
            // LocalClusterClient, so no channel is built at all (§4.9.3). This
            // also short-circuits when `resolve()` self-constructed one first —
            // the phase runs after `start` in OoP, so that is the common case.
            if hub.get::<dyn ClusterClient>().is_ok() {
                return Ok(WireOutcome::Local);   // or already-remote; same action
            }
            let ep = if endpoint.is_empty() { derive_endpoint()? } else { endpoint.to_owned() };
            hub.register::<dyn ClusterClient>(Arc::new(RemoteClusterClient::connect_lazy(&ep)?));
            spawn_descriptor_prefetch(hub);      // readiness only, never `start` (§4.9.3)
            Ok(WireOutcome::Remote)
        },
    }
}
```

### 12.16 Consumer: the whole surface, once

The point of §3.1's seam is that this file is identical in both profiles. Every primitive, so nothing is left to imagination:

```rust
// reservations/src/gear.rs — resolve in `start`, never `init` (§4.9.1)
let cache = ClusterCacheV1::resolver(hub).profile(ReservationsProfile)
    .require(CacheCapability::Linearizable).resolve().await?;
let locks = DistributedLockV1::resolver(hub).profile(ReservationsProfile).resolve().await?;
let elections = LeaderElectionV1::resolver(hub).profile(ReservationsProfile).resolve().await?;

// Scoping is client-side in both profiles; the gear never sees a bare key (§7.4)
let cache = cache.scoped("reservations")?;
```

```rust
// A lock, with the failure modes that actually occur
match locks.try_lock("seat/12", Duration::from_secs(30)).await {
    Ok(guard) => {
        // A read-modify-write inside the critical section: two remote calls in
        // Profile 3, which the amended ADR-002 permits because both are bounded
        // cluster round trips (§7.2.8). Keep the section short and prefer
        // pattern C for anything correctness-critical.
        let cur = cache.get("holds/12").await?;
        let n = cache.compare_and_swap("holds/12", cur.version(), &next(cur), Some(hold_ttl)).await?;
        match guard.renew(Duration::from_secs(30)).await {
            // Covers both an ordinary TTL lapse and a cluster-gear restart that
            // dropped the session (§6.9) — one arm, one variant, both profiles.
            Err(ClusterError::LockExpired { name }) => return Ok(Outcome::LostLease(name)),
            r => r?,
        }
        guard.release().await?;                  // Ok even if the lease is gone (§6.10)
        Ok(Outcome::Held(n))
    }
    Err(ClusterError::LockContended { .. }) => Ok(Outcome::Busy),
    Err(e) => Err(e),
}
```

```rust
// Leadership: one replica runs the reconciler. Identical in both profiles — the
// lease is a fenced record in the store, renewed by this process, so neither a
// cluster restart nor a replica swap unseats the leader (§5.8, §7.3).
let watch = elections.elect("reservation-reconciler").await?;
watch.run_while_leader(Duration::from_secs(5), || async { svc.reconcile().await }).await;

// Finding peers is NOT cluster's job. The gear's own registration is the
// framework's, and reaching another gear goes through the directory:
let peers = directory.resolve_by_labels("ingest", LabelSelector::new([("shard", "7")])).await?;
// … pick one (caller policy), dial its advertised endpoint, never a cached IP.

// A watch, with auto_restart doing the reconnection work (§6.8)
let mut w = cache.watch_prefix("holds/")?.auto_restart(RestartPolicy::default());
while let Some(ev) = w.next().await {
    match ev {
        CacheWatchEvent::Event(e)      => svc.invalidate(&e.key).await,
        CacheWatchEvent::Lagged { .. } | CacheWatchEvent::Reset => svc.resync().await,
        CacheWatchEvent::Closed(err)   => { warn!(%err, "watch closed"); break }
    }
}
```

**Nothing above names a transport, an endpoint, a timeout, a profile string outside the marker, or a `cfg`.** That is the whole of Goal 2, and it is the test to apply to any future addition: if a consumer would have to write it twice, the seam is in the wrong place.

### 12.17 Constraints the SDK's existing types impose

Four properties of types already in the tree that the design above has to respect. They are recorded here because each is invisible until code is written against them, and each has a consequence that reaches the prose sections.

| Constraint | Consequence |
|---|---|
| `LockGuard` is `{ name, commands }` with private fields and one constructor (`LockGuard::channel`), so it cannot carry a lease token in a field | The channel seam is load-bearing rather than convenient: the remote backend keeps the token in its pump task and the guard addresses it by channel (§6.5, §12.11). Each held lock costs one client-side task (§7.2.5) |
| `LeaderWatch::changed()` takes `&mut self`, because the watch owns an `mpsc::Receiver` | An election subscription needs a `Mutex`, and **at most one in-flight `AwaitChange` per `election_id`** — a second concurrent poll is rejected with `FailedPrecondition` rather than serialised, which would hand one caller a stale event (§6.6) |
| `ClientHub` has no unscoped `try_get`, and `register` is last-write-wins with no if-absent form (`client_hub.rs:142-248`) | The local-wins check uses `hub.get::<dyn ClusterClient>().is_ok()`, and both construction paths (§4.7.1, §4.9.3) check before registering. The race is benign — equivalent clients, one redundant lazy channel — but a `register_if_absent` on `ClientHub` would remove it, and is worth proposing alongside `try_get` |
| `provider_name()` returns `&'static str`, while a descriptor's provider arrives as a runtime `String` | Both need the same interning §5.2 specifies for profile names — one shared `intern()` helper, not two leaks (§12.1) |

---

## Appendix — Invariants

Properties no implementation may break. A change that appears to require breaking one of these is a
change to *this document*, not an implementation choice.

These were carried in the build plan while the work was in flight. That plan has been retired now
the work has landed, so they live here, with the design they constrain. `AUDIT-DEPLOYABLE-GEAR.md`
judges the implementation against this table and cites these by number.

| # | Invariant | §ref |
|---|---|---|
| **I1** | **Profile transparency.** One consumer source file compiles and behaves identically in Profile 1 and Profile 3. No `cfg`, no mode flag, no profile-specific consumer code | Goal 2, §3.2, §11.5 |
| **I2** | **`resolve()` becoming `async` is the only SDK signature change.** The four facades, the typed-profile resolver, `scoped()`, the watch-event unions and `auto_restart` keep their shapes | Goal 2, §3.1 |
| **I3** | **`ClusterError` is frozen.** No new variants, no widened fields. `ProfileNotBound` carries an interned `&'static str` | §5.2, §6.10, decision 14 |
| **I4** | **The boundary is the three backend traits**, and exactly one `Arc<dyn ClusterClient>` is registered per process, local winning over remote. No consumer names a `Remote*Backend` | §3.1, ADR-011 |
| **I5** | **No consumer serves traffic against an unmet requirement.** Whether that arrives as `Err(CapabilityNotMet)` from `resolve()` or as an `Unhealthy` readiness verdict varies; the guarantee and the error text do not. **Enforced on both paths since `K5`** — the deferred verdict re-runs the *same* validator closure the inline path ran, so the error text is identical by construction rather than by convention. One residual: the contributor is wired by a consumer's `healthcheck()`, so a consumer that omits that line has no deferred enforcement; the registry `warn!`s when that happens (the audit's verified-drift annex) | §4.7, §4.7.1 |
| **I6** | **Startup never blocks on cluster reachability.** `resolve()` awaits only the descriptor, bounded; the facade binds lazily; registration touches no network | §4.7.1, §4.9.1, ADR-0005 |
| **I7** | **A lease is a fenced record in the backing store.** No process's death — holder's or broker's — ends another's lease. Any replica serves any lease operation | §5.8.1, §5.8.2, ADR-012 |
| **I8** | **Renewal is client-driven**, so renewal remains the consumer-liveness proxy. A failed renew is `LockExpired` / `Status(Lost)`, never a terminal close | §6.6, §7.3 |
| **I9** | **No cluster-side client configuration exists** — no mode flag, no endpoint key, no timeouts, no profile list. Every field such a block would carry is owned elsewhere | §3.2, §4.9.2 |
| **I10** | **The profile name is typed in exactly two places** — the marker and the `.profile()` call. A config list would be a third | §4.9.2, `cpt-cf-clst-fr-validation-typed-profile` |
| **I11** | **The plugin-facing `*Backend` traits stay stable.** Extensions are defaulted and dyn-safe (`probe()`, `migrate()`), never required methods | §4.4, §4.10.1, `cpt-cf-clst-nfr-plugin-stability` |
| **I12** | **The wire is additive-only within `cluster.v1`.** `proto.lock.toml` — not the `cluster-sdk` crate version — is the wire-compatibility contract **for field _numbers_**; the committed `.proto` diff is the contract for field _types_ (`MACRO-4`). The lock records `field_name → number` plus tombstones and no type, so retyping `string key = 1` to `uint64 key = 1` leaves it byte-identical and all eight `protogen_shape` tests still pass — verified. Numbers themselves are correctly pinned: an inserted field takes the next free number rather than shifting its neighbours. A reviewer checking a wire change must read the `.proto`, not the lock | §6.12 |
| **I13** | **No compound cache operations.** The frozen cache contract is not extended; the ~2× hot-path cost is accepted | §7.2.3, §7.2.7 |
| **I14** | **Profile 1's hot path is unchanged.** `LocalClusterClient` returns the real backend `Arc` — no wrapper interposed on the request path | §3.1, §5.2 |
| **I15** | **Metric labels stay bounded** to `profile`, `provider`, `primitive`, `method`, `code`. Lock names, election names and cache keys live in traces and logs only | §5.4, §7.5, ADR-004 |

