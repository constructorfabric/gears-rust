# Technical Design — GitHub Mirror

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-design-github-mirror`

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions & Sequences](#36-interactions--sequences)
  - [3.7 Database schemas & tables](#37-database-schemas--tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional context](#4-additional-context)
  - [Algorithm Companion](#algorithm-companion)
  - [Synchronization Phases](#synchronization-phases)
  - [Change Detection Policy](#change-detection-policy)
  - [Cost Optimization Strategy](#cost-optimization-strategy)
  - [Write-Back Architecture](#write-back-architecture)
  - [Public Library API](#public-library-api)
  - [Persistence Layer Plugin Architecture](#persistence-layer-plugin-architecture)
  - [Task Queue and Scheduling Architecture](#task-queue-and-scheduling-architecture)
  - [Caching and Conditional Request Architecture](#caching-and-conditional-request-architecture)
  - [Rate-Limit Control Architecture](#rate-limit-control-architecture)
  - [Staleness TTL Policy (Design Detail)](#staleness-ttl-policy-design-detail)
  - [GraphQL Cost Optimization (Design Detail)](#graphql-cost-optimization-design-detail)
  - [Entity Lifecycle Events Architecture](#entity-lifecycle-events-architecture)
  - [Progress and Sync Status](#progress-and-sync-status)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

**GitHub Mirror** uses an **index-first, detail-later** task-graph architecture with clear separation between synchronization scheduling, execution, caching, storage, and API serving. The system treats raw API responses as the source of truth — every successful response is persisted before normalization, enabling reproducibility, debugging, schema migration, and re-normalization without re-fetching.

Synchronization proceeds in two phases: an **index pass** that cheaply enumerates all entities via list endpoints (storing IDs, numbers, `updated_at` timestamps, counts, and ETags), followed by a **targeted refinement pass** that fetches details only for entities proven to be new, changed, or incomplete. This index-first approach exploits the fact that most GitHub repositories are sparse-change workloads.

The gear exposes two REST API surfaces: a **GitHub-compatible API** that mirrors GitHub's native REST API v3 endpoints and response schemas (enabling drop-in replacement), and an **extended analytics API** under `/github-mirror/v1/` that provides cross-repository queries, incremental consumption, logical conversation grouping, write-back operations, session management, and synchronization control. Both APIs serve exclusively from the local normalized store — no upstream GitHub calls on the read path.

The architecture supports two deployment modes: an **in-process gear** within the CF/Gears runtime (library + HTTP server + DB) and a **standalone CLI tool** built from a dedicated binary crate. Python bindings via PyO3 provide a third integration surface.

### 1.2 Architecture Drivers

#### Functional Drivers

| Priority | Requirement | Design Response |
|----------|-------------|-----------------|
| `p1` | `cpt-cf-github-mirror-fr-session-init` | Engine singleton manages global request semaphore, per-token rate-limit controllers, token pool manager, and GraphQL batcher. Sessions created via `init_sync_session(engine, config)` with caller-provided token(s), DB credentials, telemetry path, logger callback |
| `p1` | `cpt-cf-github-mirror-fr-session-resume` | Tasks orchestrated in memory, never persisted. Current prototype resume re-runs repository extraction; ETag cache + durable watermarks/fingerprints/incomplete-refinement status skip unchanged work. Gear-level per-repo run status wraps this resume-by-rescan behavior; `--force` bypasses cache |
| `p1` | `cpt-cf-github-mirror-fr-sync-scope` | TOML config parsed into typed scope struct; Phase Runner enqueues tasks only for enabled entity types. Defaults match the prototype: standard entities enabled, security opt-in, actions/reactions open-only, timeline disabled |
| `p1` | `cpt-cf-github-mirror-fr-issue-pr-detection` | Issue normalizer inspects `pull_request` field; enqueues PR refinement via Phase Runner |
| `p2` | `cpt-cf-github-mirror-fr-security-sync` | Security alert extractors are scope-gated and skipped with warnings when token permissions are insufficient |
| `p2` | `cpt-cf-github-mirror-fr-contributor-derivation` | Derive contributors from embedded user objects — zero extra API calls |
| `p1` | `cpt-cf-github-mirror-fr-cost-efficiency` | Conditional requests (ETag/Last-Modified), canonical cache keys (SHA-256), page-1 ETag list short-circuit, watermark sweeps, and fingerprint gates before detail fetching; see `ALGORITHMS.md` |
| `p1` | `cpt-cf-github-mirror-fr-memory-efficiency` | Incremental processing without loading entire entity sets into memory; bounded buffers |
| `p1` | `cpt-cf-github-mirror-fr-parallel-fetch` | Parallel fetching of independent entities within a repo and across repos; respects rate limits while maximizing throughput |
| `p1` | `cpt-cf-github-mirror-fr-rate-limit` | Per-token admission gates, quota reserve, authoritative `/rate_limit` reconciliation, AIMD adaptive soft cap, and `Retry-After` backoff; see `ALGORITHMS.md` |
| `p1` | `cpt-cf-github-mirror-fr-idempotent` | All operations idempotent; in-memory task queue with deduplication; phase-scoped scheduling |
| `p1` | `cpt-cf-github-mirror-fr-raw-storage` | Raw Response Store persists every response body with URL/status/validators/fetch timestamp/content hash/schema/rate-limit/pagination/compression metadata before normalization |
| `p1` | `cpt-cf-github-mirror-fr-normalized-storage` | Normalized Store upserts typed entity records via `MetadataStore` trait |
| `p1` | `cpt-cf-github-mirror-fr-multi-db` | `MetadataStore` trait abstracts upserts for SQLite, PostgreSQL, MariaDB via SeaORM |
| `p1` | `cpt-cf-github-mirror-fr-persistence-plugins` | Pluggable persistence: filesystem-only, database-only, hybrid; gear fully functional without filesystem |
| `p1` | `cpt-cf-github-mirror-fr-repo-discovery` | Worker fetches `/repos/{owner}/{repo}`, normalizes metadata, seeds task graph |
| `p1` | `cpt-cf-github-mirror-fr-issue-refinement` | Issue refinement: detail + comments + events + timeline + reactions |
| `p1` | `cpt-cf-github-mirror-fr-pr-refinement` | PR refinement: reviews, review comments, commits, files, diff, patch, timeline, reactions, mergeability, CI |
| `p2` | `cpt-cf-github-mirror-fr-commit-ci-refinement` | Commit/CI refinement plus cost-governed GraphQL PR child extraction where batching reduces requests; see `ALGORITHMS.md` |
| `p1` | `cpt-cf-github-mirror-fr-sync-order` | Priority tiers: open PRs > open issues > global metrics > closed PRs > closed issues; `--since` bounds only closed |
| `p1` | `cpt-cf-github-mirror-fr-completeness-check` | Verification compares stored vs expected counts; bounded convergence repair and final report persistence |
| `p1` | `cpt-cf-github-mirror-fr-stale-refresh` | Per-endpoint-family freshness TTLs based on entity lifecycle state |
| `p1` | `cpt-cf-github-mirror-fr-github-compat-api` | GitHub v3 compatible REST API from normalized store |
| `p2` | `cpt-cf-github-mirror-fr-extended-api` | Extended analytics API under `/github-mirror/v1/` |
| `p3` | `cpt-cf-github-mirror-fr-write-back` | Durable write-back queue; executes against GitHub when capacity available |
| `p1` | `cpt-cf-github-mirror-fr-multi-tenancy` | Standard gears multi-tenancy: tenant-scoped DB isolation, routing, config |
| `p1` | `cpt-cf-github-mirror-fr-access-control` | Standard gears HTTP and CLI access control through SecurityContext and tenant-scoped configuration |
| `p2` | `cpt-cf-github-mirror-fr-token-pool` | Token pool distributes requests; automatic rotation on near-exhaustion |
| `p1` | `cpt-cf-github-mirror-fr-public-api` | 29+ async public functions |
| `p3` | `cpt-cf-github-mirror-fr-python-bindings` | PyO3 module with Pythonic naming |
| `p2` | `cpt-cf-github-mirror-fr-logging` | Layered `tracing` logging (info/debug/trace); never logs tokens |
| `p2` | `cpt-cf-github-mirror-fr-sync-summary` | Composite sync summary with repo identity, metrics, API usage, and storage footprint |
| `p2` | `cpt-cf-github-mirror-fr-progress` | Phase-weighted progress percentage, monotonically non-decreasing |
| `p1` | `cpt-cf-github-mirror-fr-telemetry` | Per-request and per-session telemetry in JSON Lines and API-accessible form |
| `p1` | `cpt-cf-github-mirror-fr-env-independence` | Library reads no env vars or config files; CLI tool MAY read env vars |
| `p2` | `cpt-cf-github-mirror-fr-state-events` | Entity lifecycle events emitted on key state changes during synchronization |
| `p1` | `cpt-cf-github-mirror-fr-cli-crate` | Dedicated CLI crate is the environment/config/stdout boundary and delegates behavior to the library |
| `p1` | `cpt-cf-github-mirror-fr-cli-sync` | CLI exposes `sync` and `resume`; `resume` preserves prototype resume-by-rescan semantics with prototype-compatible extraction flags |
| `p1` | `cpt-cf-github-mirror-fr-cli-query` | CLI exposes local normalized-store queries for prototype-supported entities |
| `p1` | `cpt-cf-github-mirror-fr-cli-management` | CLI exposes status, rate-limit, and cache-management commands |
| `p1` | `cpt-cf-github-mirror-fr-cli-config` | CLI supports TOML configuration and default-template printing |
| `p1` | `cpt-cf-github-mirror-fr-cli-global-opts` | CLI resolves tokens and logging options at the command boundary |
| `p1` | `cpt-cf-github-mirror-fr-cli-output` | CLI renders table and JSON output formats |
| `p1` | `cpt-cf-github-mirror-fr-cli-access-control` | CLI adapts standard tenant/access-control behavior for command-line usage |

#### NFR Allocation

| Priority | NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|----------|--------|-------------|--------------|-----------------|----------------------|
| `p1` | `cpt-cf-github-mirror-nfr-reliability` | Idempotent upserts, zero corruption | Normalized Store, Scheduler | All DB writes idempotent upserts; tasks retryable; failed requests recorded | Integration tests with interrupt/resume on 10k+ entities |
| `p1` | `cpt-cf-github-mirror-nfr-rate-compliance` | No secondary rate-limit bans | Rate Limit Controller, GitHub Client | Per-token admission before permit; AIMD; `Retry-After` | E2E tests with parallelism <= 8 |
| `p1` | `cpt-cf-github-mirror-nfr-memory-efficiency` | Bounded memory for 100k+ entities | All pipeline components | Stream pages to disk; bounded buffers | Memory profiling |
| `p1` | `cpt-cf-github-mirror-nfr-code-coverage` | >= 85% line coverage | All library modules | Trait-based mocking; recorded response tests | `cargo llvm-cov` + pytest |
| `p1` | `cpt-cf-github-mirror-nfr-security` | Zero token exposure | Client, Logging, Cache, Storage | `tracing` filters auth header; salted HMAC token fingerprints; credential store gear for secret storage | Static analysis + grep CI |
| `p1` | `cpt-cf-github-mirror-nfr-parallel-sync` | Fair parallel sync | Engine, Scheduler | Global semaphore + per-token controllers | Parallel sync benchmarks |

#### Key ADRs

| ADR ID | Decision | Materialized By |
|--------|----------|-----------------|
| [`cpt-cf-github-mirror-adr-incremental-sync-state`](./ADR/0001-cpt-cf-github-mirror-adr-incremental-sync-state.md) | Use re-enterable repository rescans with durable watermarks, fingerprints, and incomplete-refinement status instead of persisted per-task recovery | `cpt-cf-github-mirror-principle-resumable`, `cpt-cf-github-mirror-component-phase-runner`, `cpt-cf-github-mirror-component-indexing` |
| [`cpt-cf-github-mirror-adr-cache-authorization`](./ADR/0002-cpt-cf-github-mirror-adr-cache-authorization.md) | Use visibility-aware raw response cache partitioning: shared org/repo cache for public repositories and strict tenant-scoped cache for private or visibility-unknown repositories | `cpt-cf-github-mirror-principle-cache-before-network`, `cpt-cf-github-mirror-component-github-client`, `cpt-cf-github-mirror-component-cache-layer` |
| [`cpt-cf-github-mirror-adr-rate-limit-admission`](./ADR/0003-cpt-cf-github-mirror-adr-rate-limit-admission.md) | Perform per-token rate-limit admission before shared request capacity is consumed, with adaptive soft caps | `cpt-cf-github-mirror-component-rate-limit-controller`, `cpt-cf-github-mirror-component-sync-engine` |
| [`cpt-cf-github-mirror-adr-selective-graphql-pr-extraction`](./ADR/0004-cpt-cf-github-mirror-adr-selective-graphql-pr-extraction.md) | Prefer REST with conditional requests and use cost-governed GraphQL only for PR child extraction where batching reduces cost | `cpt-cf-github-mirror-component-graphql-batcher`, `cpt-cf-github-mirror-component-entity-refinement` |

### 1.3 Architecture Layers

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-tech-rust-async`

```
+------------------------------------------------------------------------+
|                   CF/Gears Runtime (or standalone CLI)                 |
+------------------------------------------------------------------------+
|  +-------------------+  +--------------------+  +-------------------+  |
|  | GitHub-Compatible |  | Extended Analytics |  | Write-Back        |  |
|  | REST API (GH v3)  |  | REST API           |  | Queue Processor   |  |
|  +-------------------+  +--------------------+  +-------------------+  |
|  +------------------------------------------------------------------+  |
|  |                      Public Library API                          |  |
|  +------------------------------------------------------------------+  |
|  +------------------------------------------------------------------+  |
|  |                     Sync Engine (singleton)                      |  |
|  |  Global Semaphore | Per-Token RateLimit | Token Pool | GQL       |  |
|  +------------------------------------------------------------------+  |
|  +------------------------------------------------------------------+  |
|  |                         Session Manager                          |  |
|  +------------------------------------------------------------------+  |
|  +------------------------------------------------------------------+  |
|  |                     Phase Runner & Scheduler                     |  |
|  +------------------------------------------------------------------+  |
|  +--------------------+  +-------------------+                         |
|  | Indexing & Change  |  | Entity Refinement |                         |
|  | Detection          |  |                   |                         |
|  +--------------------+  +-------------------+                         |
|  +------------------------------------------------------------------+  |
|  |                         GitHub Client                            |  |
|  +------------------------------------------------------------------+  |
|  +--------------------+  +-------------------+  +-------------------+  |
|  |  Cache Layer       |  | Normalized Store  |  | Raw Response      |  |
|  |  (plugin-based)    |  | (SeaORM)          |  | Store             |  |
|  +--------------------+  +-------------------+  +-------------------+  |
|  +------------------------------------------------------------------+  |
|  |        PostgreSQL | MariaDB | SQLite | Filesystem (zstd/gzip)    |  |
|  +------------------------------------------------------------------+  |
|  +--------------------+  +-------------------+                         |
|  | Observability      |  | Python Bindings   |                         |
|  | (tracing/telemetry)|  | (PyO3/maturin)    |                         |
|  +--------------------+  +-------------------+                         |
+------------------------------------------------------------------------+
```

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| REST API — GitHub-Compatible | Serve GitHub v3 compatible endpoints from local store | ToolKit OperationBuilder, OpenAPI |
| REST API — Extended Analytics | Cross-repo queries, write-back, session management | ToolKit OperationBuilder, OpenAPI |
| Public Library API | `init_engine`, `sync_repo`, `get_*`, `list_*`, `enqueue_write` | Rust async, PyO3 |
| Orchestration | Engine singleton, session lifecycle, scheduling, worker pool | Rust async, tokio |
| Synchronization | Task execution, pagination, normalization, child task generation | Rust async |
| Network | HTTP requests, conditional headers, rate-limit tracking, retry | reqwest, tokio |
| Cache | Cache key computation, lookup, ETag management, plugin dispatch | Rust, SHA-256, gears plugins |
| Storage | Raw responses, normalized upserts, session state, write-back queue | SeaORM, filesystem, zstd/gzip |
| Observability | Structured logging, telemetry, progress tracking | tracing, JSON Lines |

## 2. Principles & Constraints

### 2.1 Design Principles

#### Raw Responses Are Source of Truth

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-principle-raw-source-of-truth`

Every successful API response MUST be persisted before normalization. Raw responses enable reproducibility, debugging, schema migration, re-normalization, and auditability.

#### Synchronization Is Task-Driven

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-principle-task-driven`

The system operates as a task graph, not a recursive crawler. Tasks produce new tasks in a directed acyclic manner. This ensures predictable execution order, priority-based scheduling, and deduplication.

#### Index First, Detail Later

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-principle-index-first`

List endpoints are treated as indexes. The Scheduler MUST complete the full index pass before enqueueing detail/refinement tasks. Detail tasks are only enqueued for entities that pass the change detection gate.

#### Everything Is Resumable

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-principle-resumable`

An interrupted synchronization must be recoverable by simply re-running it. The task queue is in memory and NOT persisted; recoverability comes from durable, idempotent state: ETag/Last-Modified cache, change-detection state (watermarks + fingerprints), and per-repo run-status rows. ADR: [`cpt-cf-github-mirror-adr-incremental-sync-state`](./ADR/0001-cpt-cf-github-mirror-adr-incremental-sync-state.md).

#### Cache Before Network

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-principle-cache-before-network`

Before any API request: determine repository visibility → compute visibility-aware cache key → attempt cache read in the public or tenant partition → use cache metadata/validators for conditional requests when needed → evaluate stale policy → only then perform network request. ADR: [`cpt-cf-github-mirror-adr-cache-authorization`](./ADR/0002-cpt-cf-github-mirror-adr-cache-authorization.md).

#### Idempotent Writes

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-principle-idempotent-writes`

All local DB/cache writes MUST be idempotent. Running the same synchronization twice produces identical state.

#### Performance Is API Budget

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-principle-perf-budget`

Performance is measured in API calls consumed, not CPU cycles. Every design decision must minimize API consumption, control queue depth, bound memory, and optimize time-to-first-useful-data.

#### Serve Locally, Sync Incrementally

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-principle-serve-locally`

All read API traffic is served from the local normalized store with zero GitHub API calls. GitHub is contacted only during synchronization runs.

### 2.2 Constraints

#### Rust Async Runtime

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-constraint-async-runtime`

The system requires tokio as the async runtime for concurrent HTTP, database, filesystem, and API serving operations.

#### GitHub API Rate Limits

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-constraint-github-rate-limits`

GitHub enforces 5,000 REST requests/hour per token and a separate point-based GraphQL budget. Secondary rate limits trigger on excessive concurrency.

#### PAT-Only Authentication

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-constraint-pat-only`

v1.0 supports Personal Access Tokens only. Tokens are provided exclusively by the caller.

#### Storage Engine Compatibility

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-constraint-multi-db`

All database operations must work across PostgreSQL, MariaDB, and SQLite via SeaORM. No engine-specific code may leak into business logic.

#### Gears Platform Integration

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-constraint-gears-platform`

The gear MUST integrate with the CF/Gears runtime: ToolKit OperationBuilder, SecurityContext, ClientHub, RFC-9457 errors, and standard multi-tenancy/access control.

## 3. Technical Architecture

### 3.1 Domain Model

**Technology**: Rust structs with serde derive, SeaORM entity models

**Core Entities**:

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-entity-domain-model`

| Entity | Description |
|--------|-------------|
| SyncSession | Stateful context: config, storage handles, rate-limit state, task progress |
| SyncTask | Unit of work: endpoint, cache key, priority, state, parent task |
| CacheKey | SHA-256 of canonical HTTP request form |
| CachedResponse | Raw response body + metadata (URL, ETag, Last-Modified, status, timestamp, SHA-256 of uncompressed body, schema version, rate-limit metadata, pagination metadata, compression mode) |
| Repository | Normalized repository metadata |
| Issue | Normalized issue with PR detection flag |
| PullRequest | Normalized PR with `list_fingerprint`, `expected_*` counts, `mergeable_pending` |
| Comment | Normalized comment with `conversation_id`, soft-delete via `deleted_at` |
| GithubReviewThread | Raw GitHub inline PR review thread |
| LogicalConversation | Derived conversation grouping (inline/toplevel) |
| Review | Normalized PR review with soft-delete |
| TimelineEvent | Normalized timeline/event entry |
| Reaction | Normalized reaction |
| Commit | Normalized commit with file stats and `ci_state_hash` |
| CommitFile | Per-file change record |
| Contributor | Derived person record (zero API cost) |
| CheckRun, CheckSuite, WorkflowRun, WorkflowJob | CI entities |
| Status | Normalized commit status |
| Branch, Tag, Release, Milestone, Label | Repository metadata entities |
| Deployment | Normalized deployment with status |
| SecurityAlert | Dependabot/code-scanning/secret-scanning alert |
| EntityFingerprint | Change detection: entity_type, entity_id, content_hash, timestamps |
| SyncWatermark | Per-endpoint progress: high-water mark, sweep state, page1 ETag |
| BranchHead | Force-push detector: branch, head_sha, seen_at |
| CachePartition | Visibility-aware raw-cache boundary: shared public org/repo partition or private tenant/org/repo partition |
| WriteBackOperation | Queued mutation: type, payload, status, result, retries |
| TokenPoolEntry | Token pool member: fingerprint, scopes, rate-limit state, health |
| Tenant | Multi-tenancy: config, sync targets, token pool refs |
| SyncReport | Per-repo completeness report |
| SyncSummary | Composite result: per-object metrics, API usage, storage footprint |
| SessionTelemetry | Aggregated session telemetry |
| TelemetryEntry | Per-request telemetry record |

**Relationships**:
- SyncSession → SyncTask (1:N in-memory only; tasks are not persisted), SyncTask → CacheKey (1:1)
- Repository → Issue/PullRequest/Commit/Branch/Tag/Release/Milestone/Label (1:N each)
- Issue → Comment/TimelineEvent/Reaction (1:N each)
- PullRequest → Review/Comment/Commit/CheckSuite (1:N each)
- Commit → CommitFile/Comment/Status/CheckSuite (1:N each)
- CheckSuite → CheckRun (1:N)
- Repository → Contributor (1:N, derived)
- Repository → EntityFingerprint/SyncWatermark (1:N each)
- Tenant → Repository/TokenPoolEntry/WriteBackOperation (1:N each)

### 3.2 Component Model

#### Synchronization Engine

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-sync-engine`

##### Why this component exists

Process-wide singleton owning global request semaphore, per-token rate-limit controllers, token pool manager, and GraphQL query batcher.

##### Responsibility scope

Global request semaphore, per-token controller registry, token pool rotation and health monitoring, GraphQL batcher, engine lifecycle. ADRs: [`cpt-cf-github-mirror-adr-rate-limit-admission`](./ADR/0003-cpt-cf-github-mirror-adr-rate-limit-admission.md), [`cpt-cf-github-mirror-adr-selective-graphql-pr-extraction`](./ADR/0004-cpt-cf-github-mirror-adr-selective-graphql-pr-extraction.md).

##### Responsibility boundaries

Does NOT execute tasks, persist state, or serve APIs.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-session-manager` — creates sessions
- `cpt-cf-github-mirror-component-rate-limit-controller` — per-token controllers
- `cpt-cf-github-mirror-component-github-client` — acquires semaphore permits

#### Session Manager

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-session-manager`

##### Why this component exists

Manages synchronization session lifecycle: initialization, config validation, scope resolution, resumability.

##### Responsibility scope

Session init via `init_sync_session(engine, config)`, PAT validation, storage backend opening, prior state loading, scope validation. Environment independence: never reads env vars or config files.

##### Responsibility boundaries

Does NOT own semaphore/controllers (Engine), does NOT schedule tasks (Phase Runner).

##### Related components (by ID)

- `cpt-cf-github-mirror-component-sync-engine` — engine creates sessions
- `cpt-cf-github-mirror-component-phase-runner` — sessions delegate to phase runner

#### Phase Runner and Scheduler

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-phase-runner`

##### Why this component exists

Orchestrates the 5-phase synchronization pipeline with in-memory priority task queue and worker pool.

##### Responsibility scope

5-phase pipeline (Discovery/Indexing/ChangeDetection/Refinement/Verification), P0–P5 priority queue, worker pool, phase-scoped claiming, progress tracking, backpressure.

##### Responsibility boundaries

Does NOT perform HTTP requests (GitHub Client), does NOT persist tasks.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-github-client` — workers use client
- `cpt-cf-github-mirror-component-indexing` — phase 2
- `cpt-cf-github-mirror-component-entity-refinement` — phase 4
- `cpt-cf-github-mirror-component-verification` — phase 5

#### GitHub Client

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-github-client`

##### Why this component exists

Cache-before-network request flow for all GitHub API interactions.

##### Responsibility scope

Cache key computation, visibility-aware cache reads, validator-only cache peeks, ETag/Last-Modified conditional headers, REST Link pagination, retry with exponential backoff, and public/private cache partition selection. ADR: [`cpt-cf-github-mirror-adr-cache-authorization`](./ADR/0002-cpt-cf-github-mirror-adr-cache-authorization.md).

##### Responsibility boundaries

Does NOT own rate-limit state (Rate Limit Controller), does NOT normalize responses.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-rate-limit-controller` — admission before requesting
- `cpt-cf-github-mirror-component-cache-layer` — cache lookup/store
- `cpt-cf-github-mirror-component-sync-engine` — acquires permits

#### Rate Limit Controller

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-rate-limit-controller`

##### Why this component exists

Per-token admission control with adaptive concurrency to prevent rate-limit violations.

##### Responsibility scope

AIMD adaptive soft cap, quota reserve, authoritative `/rate_limit` reconciliation, `X-RateLimit-*` tracking, `Retry-After` compliance, and secondary rate-limit backoff. Admission happens before shared request capacity is consumed. ADR: [`cpt-cf-github-mirror-adr-rate-limit-admission`](./ADR/0003-cpt-cf-github-mirror-adr-rate-limit-admission.md).

##### Responsibility boundaries

Does NOT send HTTP requests.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-sync-engine` — manages controller registry
- `cpt-cf-github-mirror-component-github-client` — calls admission

#### Cache Layer

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-cache-layer`

##### Why this component exists

Pluggable cache backends for raw API responses with content-addressed storage.

##### Responsibility scope

`CacheStore` trait with visibility-aware `get`, validator-only `peek`, and `put` operations. Public repositories use a shared org/repo cache partition; private or visibility-unknown repositories use a strict tenant-scoped partition. Built-in: filesystem, database, hybrid. Plugin interface for custom backends. Compression (none/gzip/zstd). ADR: [`cpt-cf-github-mirror-adr-cache-authorization`](./ADR/0002-cpt-cf-github-mirror-adr-cache-authorization.md).

##### Responsibility boundaries

Does NOT normalize responses, does NOT make HTTP requests.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-github-client` — uses cache

#### Normalized Store

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-normalized-store`

##### Why this component exists

Idempotent upserts for all normalized entities across PostgreSQL, MariaDB, and SQLite.

##### Responsibility scope

`MetadataStore` trait, SeaORM entity models, idempotent upserts, `extracted_at` timestamps, session state persistence, configurable DB isolation.

##### Responsibility boundaries

Does NOT perform API requests, does NOT manage raw files.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-entity-refinement` — writes entities
- `cpt-cf-github-mirror-component-api-github-compat` — reads entities
- `cpt-cf-github-mirror-component-api-extended` — reads entities

#### Indexing and Change Detection

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-indexing`

##### Why this component exists

Drives the index pass: discovery, watermark monotone scans, fingerprint computation, force-push detection.

##### Responsibility scope

Repo metadata fetch and task seeding, watermark monotone scan (updated-desc with early-stop), entity fingerprint computation, branch head SHA comparison, page coverage tracking.

##### Responsibility boundaries

Does NOT refine entities, does NOT serve APIs.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-phase-runner` — indexing is phase 2
- `cpt-cf-github-mirror-component-entity-refinement` — enqueues refinement tasks

#### Entity Refinement

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-entity-refinement`

##### Why this component exists

Targeted detail extraction for issues, PRs, commits, CI, contributors, security, and logical conversations.

##### Responsibility scope

Issue/PR/commit refinement with all sub-resources, contributor derivation (zero API cost), tombstone reconciliation, logical conversation grouping (inline + toplevel blockquote-overlap), security alerts.

##### Responsibility boundaries

Does NOT index list endpoints, does NOT manage task queue.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-phase-runner` — refinement is phase 4
- `cpt-cf-github-mirror-component-normalized-store` — writes entities

#### Verification and Repair

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-verification`

##### Why this component exists

Post-synchronization completeness verification with bounded convergence repair.

##### Responsibility scope

Count verification, gap detection, repair task generation, bounded convergence (max 3 passes), status reporting.

##### Responsibility boundaries

Does NOT fetch details directly — generates repair tasks.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-phase-runner` — verification is phase 5

#### GraphQL Query Batcher

- [ ] `p2` - **ID**: `cpt-cf-github-mirror-component-graphql-batcher`

##### Why this component exists

Minimizes GraphQL point consumption through aliased PR chunks, continuation packing, node-limit backoff, and point-budget pacing. ADR: [`cpt-cf-github-mirror-adr-selective-graphql-pr-extraction`](./ADR/0004-cpt-cf-github-mirror-adr-selective-graphql-pr-extraction.md).

##### Responsibility scope

Aliased primary PR queries, continuation work items for paginated children, greedy first-fit packing by node and request ceilings, node-limit backoff, point-budget tracking, and REST fallback when GraphQL does not reduce request cost.

##### Responsibility boundaries

Does NOT execute HTTP requests directly.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-sync-engine` — owns batcher reference

#### GitHub-Compatible REST API

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-api-github-compat`

##### Why this component exists

Serves GitHub v3 compatible REST API from local normalized store for drop-in replacement.

##### Responsibility scope

All GitHub-compatible endpoint families, v3 response schemas, Link-header pagination, same query parameters. Read-only from normalized store.

##### Responsibility boundaries

Does NOT make upstream GitHub calls, does NOT modify data.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-normalized-store` — reads from store

#### Extended Analytics REST API

- [ ] `p2` - **ID**: `cpt-cf-github-mirror-component-api-extended`

##### Why this component exists

Capabilities beyond GitHub's native API: cross-repo queries, write-back, session management, telemetry.

##### Responsibility scope

Cross-repo queries, `extracted_since` filtering, logical conversations, write-back endpoints, sync triggers, session/telemetry endpoints.

##### Responsibility boundaries

Does NOT make upstream calls on read path.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-normalized-store` — reads from store
- `cpt-cf-github-mirror-component-write-back-processor` — enqueues operations

#### Write-Back Queue Processor

- [ ] `p3` - **ID**: `cpt-cf-github-mirror-component-write-back-processor`

##### Why this component exists

Executes enqueued write-back operations against GitHub when API capacity is available.

##### Responsibility scope

Durable queue (DB-persisted), operation lifecycle (pending/executing/completed/failed/cancelled), GitHub API execution, result recording, queryable status, cancellation while pending, retry with exponential backoff, permanent failure marking.

##### Responsibility boundaries

Does NOT synchronize data, does NOT serve API responses.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-github-client` — executes against GitHub
- `cpt-cf-github-mirror-component-rate-limit-controller` — respects rate limits

#### Observability

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-component-observability`

##### Why this component exists

Structured logging and telemetry for operational visibility and cost auditing.

##### Responsibility scope

Per-request telemetry, session telemetry aggregation, JSON Lines telemetry file, API-accessible telemetry, `tracing`-based logging (info/debug/trace), composite sync summary rendering, progress computation.

##### Responsibility boundaries

Does NOT own subscriber installation.

##### Related components (by ID)

- `cpt-cf-github-mirror-component-session-manager` — configures telemetry paths

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-interface-mirror-api`

- **Technology**: REST/OpenAPI (via ToolKit OperationBuilder)

| Priority | Interface ID | Design Surface |
|----------|--------------|----------------|
| `p1` | `cpt-cf-github-mirror-interface-github-compat-rest` | GitHub-compatible REST endpoints served from the normalized store |
| `p2` | `cpt-cf-github-mirror-interface-extended-rest` | Extended analytics and management endpoints under `/github-mirror/v1/` |
| `p1` | `cpt-cf-github-mirror-interface-rust-lib` | Rust async library entry points for engine/session/sync/query/cache/write-back |
| `p1` | `cpt-cf-github-mirror-interface-sdk` | SDK contract consumed by HTTP server, CLI, plugins, and bindings |
| `p3` | `cpt-cf-github-mirror-interface-python` | PyO3/maturin Python module wrapping the Rust library |
| `p1` | `cpt-cf-github-mirror-interface-cache-store` | Pluggable cache backend trait with visibility-aware reads and validator peeks |
| `p1` | `cpt-cf-github-mirror-interface-metadata-store` | SeaORM-backed metadata store abstraction for SQLite, PostgreSQL, and MariaDB |

**GitHub-Compatible Endpoints** (selected):

| Method | Path | Description | Stability |
|--------|------|-------------|-----------|
| `GET` | `/repos/{owner}/{repo}` | Repository metadata | stable |
| `GET` | `/repos/{owner}/{repo}/issues` | Issues list | stable |
| `GET` | `/repos/{owner}/{repo}/pulls` | Pull requests list | stable |
| `GET` | `/repos/{owner}/{repo}/commits` | Commits list | stable |
| `GET` | `/repos/{owner}/{repo}/branches` | Branches | stable |
| `GET` | `/repos/{owner}/{repo}/releases` | Releases | stable |
| `GET` | `/repos/{owner}/{repo}/contributors` | Derived contributors | stable |

**Extended Analytics Endpoints** (under `/github-mirror/v1/`):

| Method | Path | Description | Stability |
|--------|------|-------------|-----------|
| `GET` | `/github-mirror/v1/repos` | Cross-repo list | unstable |
| `GET` | `/github-mirror/v1/issues` | Cross-repo issues | unstable |
| `GET` | `/github-mirror/v1/pulls` | Cross-repo PRs | unstable |
| `GET` | `/github-mirror/v1/conversations` | Logical conversations | unstable |
| `POST` | `/github-mirror/v1/repos/{owner}/{repo}/sync` | Trigger sync | unstable |
| `POST` | `/github-mirror/v1/write-back` | Enqueue write operation | unstable |
| `GET` | `/github-mirror/v1/write-back/{id}` | Operation status | unstable |

### 3.4 Internal Dependencies

| Dependency Gear | Interface Used | Purpose |
|-----------------|----------------|---------|
| ToolKit | OperationBuilder, OpenAPI | REST route wiring |
| SecurityContext | Platform auth path | Authenticated request context |
| ClientHub | SDK client registration | In-process SDK contract |
| Credential Store gear | Secret reference lookup | Secure storage/retrieval of GitHub PATs, token-pool entries, and other secrets; the mirror does not implement credential storage |
| Event Manager / Event Broker gear | Entity lifecycle event publication | Delivers entity lifecycle state-change events to platform subscribers; when unavailable, events remain on the standard gear-local event path |
| Canonical error library | RFC-9457 problem details | Standardized errors |

### 3.5 External Dependencies

#### GitHub REST API v3

**Contract**: `cpt-cf-github-mirror-contract-github-rest`

HTTPS/REST, JSON, Link-header pagination, ETag/Last-Modified conditional requests, `X-RateLimit-*` headers, `X-GitHub-Api-Version` tracking.

#### GitHub GraphQL API v4

**Contract**: `cpt-cf-github-mirror-contract-github-graphql`

HTTPS/POST `/graphql`, JSON, cursor-based pagination, point-based rate limiting, 500k node limit.

### 3.6 Interactions & Sequences

Detailed pseudocode for the cost-effective synchronization algorithms is maintained in [`ALGORITHMS.md`](./ALGORITHMS.md). This section summarizes the sequence boundaries only.

#### Full Repository Synchronization

**ID**: `cpt-cf-github-mirror-seq-full-sync`

**Use cases**: `cpt-cf-github-mirror-usecase-full-sync`, `cpt-cf-github-mirror-usecase-incremental-refresh`

**Actors**: `cpt-cf-github-mirror-actor-cli-operator`, `cpt-cf-github-mirror-actor-lib-consumer`

```
Phase 1: Discovery (2%)  → Fetch repo metadata, seed task graph
Phase 2: Indexing (10%)   → Watermark scans, fingerprint computation
Phase 3: Change Det. (3%) → Compare fingerprints, determine refinement set
Phase 4: Refinement (80%) → Fetch details for new/changed entities
Phase 5: Verification (5%)→ Compare counts, repair gaps (max 3 passes)
```

#### Write-Back Operation Execution

**ID**: `cpt-cf-github-mirror-seq-write-back`

**Use cases**: `cpt-cf-github-mirror-usecase-write-back`

**Actors**: `cpt-cf-github-mirror-actor-api-consumer`

```
1. Consumer POSTs write-back request → 2. Validate + enqueue → 3. Return operation ID
4. Processor polls queue → 5. Acquire rate-limit admission + permit
6. Execute against GitHub → 7. Record result → 8. Retry on failure
```

#### Dashboard Read Path

**Use cases**: `cpt-cf-github-mirror-usecase-dashboard`

Dashboard and analytics clients read synchronized repository, issue, PR, contributor, status, summary, and telemetry data from the GitHub-compatible and extended REST APIs. The read path is local-store only and never calls upstream GitHub.

### 3.7 Database schemas & tables

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-db-schema`

#### Table: sync_sessions

**ID**: `cpt-cf-github-mirror-dbtable-sync-sessions`

| Column | Type | Description |
|--------|------|-------------|
| id | BIGINT PK | Session ID |
| tenant_id | BIGINT | Tenant reference |
| started_at | TIMESTAMP | Session start |
| ended_at | TIMESTAMP | Session end |
| status | TEXT | in_progress / complete / failed |
| progress_percent | INTEGER | 0–100 |

#### Table: repositories

**ID**: `cpt-cf-github-mirror-dbtable-repositories`

| Column | Type | Description |
|--------|------|-------------|
| id | BIGINT PK | GitHub repo ID |
| tenant_id | BIGINT | Tenant reference |
| node_id | TEXT | GraphQL global ID |
| owner | TEXT | Owner login |
| name | TEXT | Repo name |
| full_name | TEXT | owner/name |
| default_branch | TEXT | Default branch |
| extracted_at | TIMESTAMP | Local extraction time |

#### Table: issues

**ID**: `cpt-cf-github-mirror-dbtable-issues`

| Column | Type | Description |
|--------|------|-------------|
| id | BIGINT PK | GitHub issue ID |
| tenant_id | BIGINT | Tenant reference |
| repo_id | BIGINT | FK to repositories |
| number | INTEGER | Issue number |
| node_id | TEXT | GraphQL global ID |
| state | TEXT | open / closed |
| is_pull_request | BOOLEAN | PR detection flag |
| content_hash | TEXT | Normalized body hash |
| extracted_at | TIMESTAMP | Local extraction time |

#### Table: pull_requests

**ID**: `cpt-cf-github-mirror-dbtable-pull-requests`

| Column | Type | Description |
|--------|------|-------------|
| id | BIGINT PK | GitHub PR ID |
| tenant_id | BIGINT | Tenant reference |
| repo_id | BIGINT | FK to repositories |
| number | INTEGER | PR number |
| node_id | TEXT | GraphQL global ID |
| state | TEXT | open / closed |
| merged | BOOLEAN | Merge status |
| head_sha | TEXT | Head commit SHA |
| list_fingerprint | TEXT | List-visible change hash |
| expected_commits | INTEGER | Expected commit count |
| mergeable_pending | BOOLEAN | Mergeability pending |
| extracted_at | TIMESTAMP | Local extraction time |

#### Table: contributors

**ID**: `cpt-cf-github-mirror-dbtable-contributors`

| Column | Type | Description |
|--------|------|-------------|
| tenant_id | BIGINT | Tenant reference |
| user_id | BIGINT | GitHub user ID (PK component) |
| repo_id | BIGINT | Repository ID (PK component) |
| login | TEXT | GitHub login |
| account_type | TEXT | User / Bot / Organization |
| roles | TEXT | Comma-separated association roles |
| first_seen_at | TIMESTAMP | First appearance |
| last_seen_at | TIMESTAMP | Last appearance |

**PK**: `(user_id, repo_id)`

#### Table: write_back_operations

**ID**: `cpt-cf-github-mirror-dbtable-write-back-operations`

| Column | Type | Description |
|--------|------|-------------|
| id | TEXT PK | Operation UUID |
| tenant_id | BIGINT | Tenant reference |
| repo_id | BIGINT | Target repository |
| operation_type | TEXT | merge_pr / post_comment / ... |
| payload | TEXT | JSON payload |
| status | TEXT | pending / executing / completed / failed |
| result | TEXT | JSON result |
| retry_count | INTEGER | Current retries |
| created_at | TIMESTAMP | Enqueue time |

#### Table: entity_fingerprints

**ID**: `cpt-cf-github-mirror-dbtable-entity-fingerprints`

**PK**: `(repo_id, entity_type, entity_id)`

#### Table: sync_watermarks

**ID**: `cpt-cf-github-mirror-dbtable-sync-watermarks`

**PK**: `(repo_id, endpoint_family)`

### 3.8 Deployment Topology

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-topology-deployment`

**Server Mode** (primary): In-process gear within CF/Gears runtime. HTTP server serves both API surfaces. Synchronization and write-back processing run as background tasks. PostgreSQL or MariaDB recommended.

**CLI Mode** (secondary): Standalone binary from dedicated CLI crate. Synchronization triggered by commands. No HTTP server. SQLite default.

## 4. Additional context

### Algorithm Companion

[`ALGORITHMS.md`](./ALGORITHMS.md) is the source-grounded companion for the implementation algorithms: scope resolution, cache-before-network requests, visibility-aware cache partitioning, watermark sweeps, fingerprint refinement gates, resume-by-rescan, rate-limit control, GraphQL packing, mergeability polling, logical-conversation incremental regrouping, and telemetry cost accounting.

### Synchronization Phases

The 5-phase pipeline is the core architectural pattern with fixed progress weights: Discovery (2%), Indexing (10%), Change Detection (3%), Refinement (80%), Verification (5%). Progress is monotonically non-decreasing and persisted incrementally.

### Change Detection Policy

Primary signal: watermark monotone scan with page-1 ETag whole-family short-circuit. Per-entity refinement gates use list-visible fingerprints, `child_counts_hash`, incomplete-refinement status, and state-aware TTL backstops. PR fingerprints include `head.sha`, so force-pushes that change a PR head are detected during list indexing. Commit bodies are treated as immutable by SHA; CI state is refreshed separately. See [`ALGORITHMS.md`](./ALGORITHMS.md).

### Cost Optimization Strategy

REST with ETags for stable resources. Page-1 ETag and watermark scans avoid full-list re-fetch. Fingerprint gates avoid detail fetches for unchanged entities. GraphQL PR extraction uses cost-governed packing and continuations only where batching reduces request cost. Token pool multiplication is part of the gear target design for large-scale deployments. See [`ALGORITHMS.md`](./ALGORITHMS.md).

### Write-Back Architecture

Write-back operations are always asynchronous. The API returns immediately after enqueueing. The processor runs as a background task, polling the durable queue and executing against GitHub when rate-limit capacity is available. Operations support idempotent retry with exponential backoff.

### Public Library API

The library exposes the following async public functions:

**Engine and Session Management (8 functions):**
1. `init_engine(config) -> Result<Engine>` — create the global engine with request semaphore, per-token rate-limit controllers, and GraphQL batcher
2. `init_sync_session(engine, config) -> Result<SyncSession>` — initialize a session; caller provides token(s), DB credentials, telemetry log path, optional logger callback
3. `sync_repo(session, repo, options) -> Result<SyncReport>` — unified synchronization entry point (auto-detects initial vs incremental); supports dry-run mode
4. `clear_cache(session, scope) -> Result<CacheClearReport>` — clear cached data for an org or repo
5. `list_sessions(session) -> Result<Vec<SessionSummary>>` — list prior sessions
6. `get_session_telemetry(session, session_id) -> Result<SessionTelemetry>` — retrieve telemetry
7. `query_entities(session, query) -> Result<EntityResultSet>` — generic query across all entity types
8. `fetch_native(session, request) -> Result<CachedResponse<T>>` — escape hatch for arbitrary GitHub API requests

**Entity Detail Retrieval (9 functions):**
- `get_repo`, `get_issue` (with `include`), `get_pull_request` (with `include`), `get_milestone`, `get_branch`, `get_commit` (with `include`), `get_release`, `get_label`, `get_contributor`

**Entity List Retrieval (9 functions):**
- `list_issues`, `list_pull_requests`, `list_milestones`, `list_branches`, `list_commits`, `list_releases`, `list_labels`, `list_repos`, `list_contributors`

All `list_*` and `query_entities` filters support an `extracted_since` parameter. The `include` parameter on `get_issue`, `get_pull_request`, and `get_commit` controls which child collections are loaded.

**Write-Back Operations (3 functions):**
- `enqueue_write(session, operation) -> Result<OperationId>`
- `get_write_status(session, operation_id) -> Result<WriteOperationStatus>`
- `cancel_write(session, operation_id) -> Result<()>`

All entity retrieval functions read from the normalized store — they do not make GitHub API calls.

### Persistence Layer Plugin Architecture

The persistence layer is pluggable with three built-in modes:

- **Filesystem-only**: raw responses stored on disk with `.meta.json` companion files; organized by host/owner/repo/api-type/endpoint/page. Supports optional compression (none, gzip, zstd), with integrity hashes computed over uncompressed response bodies. Suitable for CLI tool where manual inspection and filesystem caching are valuable.
- **Database-only**: all raw responses and metadata stored in the relational database. No filesystem dependency. Suitable for REST API service deployments in containerized/ephemeral environments.
- **Hybrid**: raw response bodies on filesystem, normalized entities and metadata in database. Balances inspection convenience with query performance.

The `CacheStore` trait defines visibility-aware body reads, validator-only peeks, writes, and cache-key partition selection. Public repository entries are shared by canonical org/repo/request key. Private or visibility-unknown repository entries include `tenant_id` and are never reused across tenants. Custom backends (Redis, S3, etc.) can be implemented via the gears plugin framework and loaded at runtime.

### Task Queue and Scheduling Architecture

Synchronization tasks are orchestrated through an in-memory priority queue (tasks are NOT persisted to the database). Resume is achieved by re-running extraction and relying on durable cache metadata, watermarks, fingerprints, and incomplete-refinement statuses rather than restoring task records. Tasks are ordered by synchronization phase and within-phase priority levels: P0 (repo metadata, rate-limit recovery), P1 (list pages), P2 (issue/PR detail), P3 (comments/reviews/timeline), P4 (reactions/status/check details), P5 (stale refinements). The scheduler prefers breadth-first list completion before deep refinement. Task deduplication is by canonical cache key within the same (session, repo). ADR: [`cpt-cf-github-mirror-adr-incremental-sync-state`](./ADR/0001-cpt-cf-github-mirror-adr-incremental-sync-state.md).

### Caching and Conditional Request Architecture

The GitHub Client implements a cache-before-network request pipeline: determine repository visibility, defaulting unknown visibility to private → compute a visibility-aware cache key (public: SHA-256 of METHOD + URL + sorted query params + API version + Accept; private/unknown: same canonical dimensions plus `tenant_id`) → attempt a cache read in the selected partition → fall back to cache `peek` for validators in the same partition → attach ETag (`If-None-Match`) and Last-Modified (`If-Modified-Since`) unless force mode is active → perform the network request only when required. On 304 Not Modified, the cached body is reused when available and child refinement tasks are skipped unless the stale policy requires it.

Public repository raw responses are reusable across tenants because the raw cache is not a consumer authorization surface; GitHub Mirror API handlers enforce tenant and access-control checks over normalized data. Private or visibility-unknown repository raw responses and validators are tenant-partitioned and never reused across tenants. See [`ALGORITHMS.md`](./ALGORITHMS.md) and ADR [`cpt-cf-github-mirror-adr-cache-authorization`](./ADR/0002-cpt-cf-github-mirror-adr-cache-authorization.md).

### Rate-Limit Control Architecture

Per-token rate-limit admission happens before shared request capacity is consumed — a rate-limited token cannot starve other tokens. Each token has an independent controller using a quota reserve, authoritative `/rate_limit` reconciliation before long reset sleeps, `X-RateLimit-Remaining`/`X-RateLimit-Reset` tracking, `Retry-After` compliance, and AIMD soft-cap adjustment. Secondary rate-limit detection halves the soft cap and activates backoff. See [`ALGORITHMS.md`](./ALGORITHMS.md) and ADR [`cpt-cf-github-mirror-adr-rate-limit-admission`](./ADR/0003-cpt-cf-github-mirror-adr-rate-limit-admission.md).

### Staleness TTL Policy (Design Detail)

Per-endpoint-family freshness TTLs based on entity lifecycle state:
- **Open issues/PRs**: short TTL (hours)
- **Open PRs with pending CI**: very short TTL (minutes)
- **Merged PRs**: medium TTL decaying to long based on age
- **Closed issues/PRs**: medium-to-long TTL based on recency
- **Commits with pending CI**: very short TTL; with complete CI: long TTL
- **Branches**: medium TTL
- **Labels, milestones, releases**: long TTL
- **Reactions**: optional, configurable opt-in

TTL=0 forces re-refinement on every synchronization. TTL=infinity disables TTL-based re-refinement for that family.

### GraphQL Cost Optimization (Design Detail)

The system tracks GraphQL point costs and remaining budget. REST with ETags is preferred for stable resources; GraphQL is used only when it reduces total request count for PR child extraction. The current prototype uses aliased primary PR chunks, continuation work items for paginated children, greedy first-fit packing bounded by node and request ceilings, node-limit backoff, token-bucket pacing at 1,800 points/minute, and an hourly point reserve. Cost model: `max(1, round(sum_requests / 100))`. See [`ALGORITHMS.md`](./ALGORITHMS.md) and ADR [`cpt-cf-github-mirror-adr-selective-graphql-pr-extraction`](./ADR/0004-cpt-cf-github-mirror-adr-selective-graphql-pr-extraction.md).

### Entity Lifecycle Events Architecture

The system emits events on key entity state changes detected during synchronization. Change detection compares the current synchronized state against the prior stored state during the refinement phase. Events are published via the gear's standard event/notification mechanism (SSE or plugin-based). Event payloads include: entity type, entity ID, repository, change type (created/updated/merged/closed/deleted), and timestamp.

### Progress and Sync Status

The `RepoPhaseRunner` computes a phase-weighted, object-aware progress percentage using fixed weights: Discovery (2%), Indexing (10%), Change Detection (3%), Refinement (80%), Verification (5%). Progress is clamped monotonically non-decreasing and published via a shared atomic. `sync_repo` persists progress incrementally to `sync_sessions.progress_percent` via a heartbeat (also stamping `ended_at`); duration = `ended_at - started_at`. The CLI `status` command renders previous/current scan progress and duration. The extended REST API exposes per-repo sync status with progress, duration, entity counts, and last-sync timestamp. The synchronization summary contains the session ID, repository slug and URL, stars/forks, overall status, unresolved-mergeability flag, completion timestamp, elapsed duration, per-object metrics, grand totals, REST/GraphQL usage and errors, remaining budgets, reset time, and database/cache/total storage footprint.

## 5. Traceability

- **PRD**: [PRD.md](./PRD.md)
- **Algorithms**: [ALGORITHMS.md](./ALGORITHMS.md)
- **ADRs**: [ADR/](./ADR/) — [`cpt-cf-github-mirror-adr-incremental-sync-state`](./ADR/0001-cpt-cf-github-mirror-adr-incremental-sync-state.md), [`cpt-cf-github-mirror-adr-cache-authorization`](./ADR/0002-cpt-cf-github-mirror-adr-cache-authorization.md), [`cpt-cf-github-mirror-adr-rate-limit-admission`](./ADR/0003-cpt-cf-github-mirror-adr-rate-limit-admission.md), [`cpt-cf-github-mirror-adr-selective-graphql-pr-extraction`](./ADR/0004-cpt-cf-github-mirror-adr-selective-graphql-pr-extraction.md)
- **Features**: [features/](./features/)
