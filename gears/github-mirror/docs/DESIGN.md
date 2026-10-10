# Technical Design: GitHub Mirror

- [x] `p1` - **ID**: `cpt-cf-github-mirror-design-github-mirror`

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
  - [3.7 Database Schemas & Tables](#37-database-schemas--tables)
  - [3.8 Core Algorithms](#38-core-algorithms)
- [4. Additional Context](#4-additional-context)
  - [Sync Phases](#sync-phases)
  - [Priority Tiers](#priority-tiers)
  - [Consistency Model](#consistency-model)
  - [Security](#security)
  - [Operations](#operations)
  - [Future Work](#future-work)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

GitHub Mirror keeps a local, tenant-scoped copy of GitHub repositories and serves it back. A tenant asks for a repository to be synced; the gear queues the sync, walks the repository's listings, fetches the details of what changed, and writes everything into its own `gm_` tables. Reads never call GitHub: they answer from those tables, either on GitHub's own paths (`/repos/{owner}/{name}/issues`, …) or on the gear's own API (`/github-mirror/v1/...`).

The gear is a ToolKit gear (`github-mirror`) in the DDD-light layout: `api/rest` for the endpoints, `domain` for the service, the sync engine and the ports, `infra` for the GitHub client, the HTTP cache and the SeaORM storage. It is built from the reference implementation repotap, whose design document it adapts; the parts of repotap that belong to a single-token command-line tool (Python bindings, the GraphQL batcher, per-token rate controllers) are not part of this gear.

A sync is designed to be stopped and started again. The task queue lives in memory and is never persisted; what survives is the HTTP cache (so unchanged pages come back as `304` and cost no quota), the per-family watermarks and the per-entity fingerprints (so unchanged entities are skipped), and a per-repository run status that `POST /sync/resume` uses to find the repositories left unfinished.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Phase | Design Response |
|---|---|---|
| `cpt-cf-github-mirror-fr-session-init` | `p1` | `POST /github-mirror/v1/repos/{owner}/{name}/sync` writes a `queued` session and answers `202` with its id; see [Queued Sync](#queued-sync) |
| `cpt-cf-github-mirror-fr-session-resume` | `p1` | `gm_repo_sync_status` keeps `in_progress` until a run completes; `POST /sync/resume` re-queues those repositories (up to 64 per call, the depth of the sync queue) |
| `cpt-cf-github-mirror-fr-sync-scope` | `p1` | `include` picks the object families; `actions_scope`, `reactions_scope`, `timeline_scope` pick `all` / `open` / `none`; see [Collection Scopes](#386-collection-scopes) |
| `cpt-cf-github-mirror-fr-issue-pr-detection` | `p1` | Pull requests come from `/pulls`, issues from `/issues` with pull requests left out |
| `cpt-cf-github-mirror-fr-cost-efficiency` | `p1` | Conditional requests with stored `ETag` / `Last-Modified`; watermark sweeps; the change gate; see [3.8](#38-core-algorithms) |
| `cpt-cf-github-mirror-fr-memory-efficiency` | `p1` | Listings are written page by page; the task queue applies backpressure at 10,000 pending tasks |
| `cpt-cf-github-mirror-fr-parallel-fetch` | `p1` | `max_concurrent_syncs` repositories at once, `max_concurrent_tasks` tasks inside one, `max_concurrent_requests` GitHub calls in flight |
| `cpt-cf-github-mirror-fr-rate-limit` | `p1` | One shared cooldown in the GitHub client honours `Retry-After` / `X-RateLimit-Reset`; see [3.8.9](#389-github-client) |
| `cpt-cf-github-mirror-fr-sync-deadline` | `p1` | `sync_deadline_minutes` (default 360) stops a run in order; the session ends `failed` with the reason |
| `cpt-cf-github-mirror-fr-idempotent` | `p1` | Every write is an upsert keyed on the GitHub id and the tenant |
| `cpt-cf-github-mirror-fr-raw-storage` | `p1` | `gm_http_cache` keeps each fetched page body (compressed) with its validators and content hash |
| `cpt-cf-github-mirror-fr-persistence-plugins` | `p1` | `HttpCache` has a database store (default, no disk use) and a filesystem store chosen by `cache_dir`; mirrored rows always stay in the database |
| `cpt-cf-github-mirror-fr-normalized-storage` | `p1` | 31 `gm_` tables, one per entity family |
| `cpt-cf-github-mirror-fr-multi-db` | `p1` | SeaORM through toolkit-db; migrations cover SQLite, PostgreSQL and MariaDB, and `make test-github-mirror-db` runs the suite on the last two |
| `cpt-cf-github-mirror-nfr-code-coverage` | `p1` | `cargo llvm-cov` measures the gear at 93% line coverage; the `test-github-mirror-coverage` CI job fails below 85% |
| `cpt-cf-github-mirror-fr-repo-discovery` | `p1` | Phase 1 fetches the repository row and seeds one indexing task per enabled family |
| `cpt-cf-github-mirror-fr-issue-refinement` | `p1` | Comments and events come with the issue listing; `Refine(Issue)` fetches reactions and timeline, each by its scope |
| `cpt-cf-github-mirror-fr-pr-refinement` | `p1` | Review comments come with the pull listing; `Refine(PullRequest)` fetches reviews, files, commits and review threads (GraphQL) |
| `cpt-cf-github-mirror-fr-commit-ci-refinement` | `p2` | Commit comments come with the commit listing; `Refine(Commit)` fetches files, plus statuses and check runs when `actions_scope=all`; workflow-run jobs follow `actions_scope` |
| `cpt-cf-github-mirror-fr-sync-order` | `p1` | Priority tiers: open pull requests, open issues, repository-wide families, closed pull requests, closed issues |
| `cpt-cf-github-mirror-fr-completeness-check` | `p1` | Phase 5 compares declared and stored counts; see [3.8.5](#385-verification) |
| `cpt-cf-github-mirror-fr-stale-refresh` | `p2` | Per-entity refresh TTLs in the change gate |
| `cpt-cf-github-mirror-fr-contributor-derivation` | `p2` | Contributors are derived from authors, assignees, reviewers, commenters and committers seen during the sync |
| `cpt-cf-github-mirror-fr-github-compat-api` | `p1` | GitHub-shaped read endpoints at the root, answering with GitHub's field names and error bodies |
| `cpt-cf-github-mirror-fr-extended-api` | `p2` | `/github-mirror/v1/...`: sessions, sync status, resume, cache clear, commit files, review threads |
| `cpt-cf-github-mirror-fr-multi-tenancy` | `p1` | Every table carries `tenant_id`; every query goes through the secure ORM with the caller's `AccessScope`; cached responses of a public repository live in one shared `gm_http_cache` partition |
| `cpt-cf-github-mirror-fr-access-control` | `p1` | The platform policy enforcer decides each read and write; a refused read answers `404`, never `403` |
| `cpt-cf-github-mirror-fr-public-api` | `p1` | The SDK trait `GithubMirrorClientV1`, served in process by `LocalClient` |
| `cpt-cf-github-mirror-fr-log-redaction` | `p1` | Tokens and credential-shaped text are redacted before logging; GraphQL error bodies are logged, not stored on the session |
| `cpt-cf-github-mirror-fr-sync-summary` | `p2` | A completed session stores a `SyncSummary` with one counter per table and any accepted drift |
| `cpt-cf-github-mirror-fr-progress` | `p2` | `progress_percent` is weighted by phase, never goes down, and is written by the heartbeat |
| `cpt-cf-github-mirror-fr-env-independence` | `p1` | All settings come from the gear config; the GitHub token comes from the credential store |

Not covered by this design yet: Python bindings (`cpt-cf-github-mirror-fr-python-bindings`), write-back (`cpt-cf-github-mirror-fr-write-back`), the token pool (`cpt-cf-github-mirror-fr-token-pool`) and security-alert collection (`cpt-cf-github-mirror-fr-security-sync`, refused by scope validation today).

#### NFR Allocation

| NFR | Design Response |
|---|---|
| `cpt-cf-github-mirror-nfr-reliability` | Resumable runs; per-task transactions; claim and lock released on every exit path, including an aborted task |
| `cpt-cf-github-mirror-nfr-rate-compliance` | Request semaphore, shared cooldown, conditional requests |
| `cpt-cf-github-mirror-nfr-memory-efficiency` | Page-by-page writes; 64 MiB cap on any single response body; queue backpressure |
| `cpt-cf-github-mirror-nfr-security` | Tenant scoping in storage, policy checks per operation, token read from the credential store and sent only over HTTPS (or loopback) |
| `cpt-cf-github-mirror-nfr-parallel-sync` | Tenant-fair pool of `max_concurrent_syncs` workers |
| `cpt-cf-github-mirror-nfr-data-governance` | Per-repository and per-owner cache clear; a maximum cache age, gear-wide or per tenant; additive migrations with `down()` |

### 1.3 Architecture Layers

| Layer | Responsibility | Where |
|---|---|---|
| REST API | GitHub-compatible reads, the gear's own endpoints, OpenAPI | `src/api/rest` |
| In-process API | `GithubMirrorClientV1` for other gears | `src/domain/local_client.rs`, `github-mirror-sdk` |
| Service | Authorization, sessions, claims, the queue, resume, cache clear | `src/domain/service.rs` |
| Sync engine | Phases, tasks, change gate, watermarks, verification, the pool | `src/domain/sync` |
| Ports | `GithubPort`, `SyncWriter` and the per-table repositories | `src/domain/ports`, `src/domain/repo.rs` |
| GitHub client | REST and GraphQL calls, pagination, rate limits, the HTTP cache | `src/infra/github` |
| Storage | SeaORM entities, repositories, migrations | `src/infra/storage` |

## 2. Principles & Constraints

### 2.1 Design Principles

#### Sync Is Task-Driven

- [x] `p1` - **ID**: `cpt-cf-github-mirror-principle-task-driven`

A sync is a set of small tasks (discover, index one family, refine one entity, verify one entity) drained phase by phase. Each task writes what it fetched in its own transaction, so a run stopped anywhere leaves nothing half-written.

#### Index First, Detail Later

- [x] `p1` - **ID**: `cpt-cf-github-mirror-principle-index-first`

Listings are cheap and cacheable; they give ids, `updated_at` and child counts. Detail requests are made only for entities the change gate lets through.

#### Everything Is Resumable

- [x] `p1` - **ID**: `cpt-cf-github-mirror-principle-resumable`

Nothing about a run's progress lives only in memory except the queue itself. The cache, the watermarks, the fingerprints and the run status let the next run carry on where the last one stopped.

#### Cache Before Network

- [x] `p1` - **ID**: `cpt-cf-github-mirror-principle-cache-first`

Every GET replays the stored `ETag` / `Last-Modified`; a `304` is served from the cached body and costs no rate limit.

#### Idempotent Writes

- [x] `p1` - **ID**: `cpt-cf-github-mirror-principle-idempotent`

Running the same sync twice leaves the same rows: every write is an upsert on the entity's GitHub id within the tenant.

#### The Cache Holds Responses, the Tables Hold Truth

- [x] `p1` - **ID**: `cpt-cf-github-mirror-principle-cache-role`

Unlike repotap, raw bodies are kept only as the HTTP cache. Reads are answered from the normalized tables, and nothing is re-normalized from the cache.

### 2.2 Constraints

#### Runs as a ToolKit Gear

- [x] `p1` - **ID**: `cpt-cf-github-mirror-constraint-toolkit-gear`

The gear is started and stopped by the framework. `stop()` cancels the pool and waits for running syncs, then for the in-process syncs `LocalClient` started; when the framework's hard-stop deadline fires first, the pool task is aborted. A job cut short gives its claim back through a drop guard, and its session is closed out as `interrupted` by the next start-up sweep.

#### One GitHub Token

- [x] `p1` - **ID**: `cpt-cf-github-mirror-constraint-one-token`

The gear is configured with one token: `github_token_secret` names its secret in the credential store, read once at init. All tenants' syncs share its rate limit, so a cooldown pauses every request made with it. A token per tenant is future work.

#### Tenant Isolation in Storage

- [x] `p1` - **ID**: `cpt-cf-github-mirror-constraint-tenant-isolation`

Every table has `tenant_id`, and every query is built through the secure ORM with the caller's `AccessScope`. A handler cannot forget the tenant filter, because storage adds it.

#### Storage Engines

- [x] `p1` - **ID**: `cpt-cf-github-mirror-constraint-storage-engines`

SeaORM through toolkit-db, with SQLite and PostgreSQL in the migrations. The migration runner applies migrations in name order; names are `m0001_…` to `m0044_…`, so name order is number order and a new migration takes the next number.

## 3. Technical Architecture

### 3.1 Domain Model

| Type | What it is |
|---|---|
| `SyncSessionRecord` | One sync run: repository, status, progress, error, summary, timestamps |
| `SessionStatus` | `queued`, `in_progress`, `complete`, `failed`, `interrupted` |
| `RepoRunStatus` | Per repository: `in_progress` until a run completes, then `complete` |
| `ScopeConfig` | `objects` (which families) and `collection` (`actions`, `reactions`, `timeline`: `all` / `open` / `none`) |
| `SyncJob` | A queued run: tenant context, repository, scope, force mode, `since`, and the claim guard |
| `Claim` | The in-memory mark that a repository has a run in flight, with the terms it was asked for |
| `ExtractionTask` | One unit of work: kind, entity id, priority, `attempt` (repair pass), `retries` (database retries) |
| `TaskKind` | `Discover`, `Index(Family)`, `Refine(Entity)`, `Verify(Entity)` |
| `Family` | `Issues`, `PullRequests`, `Commits`, `Metadata`, `Actions` |
| `Entity` | `Issue`, `PullRequest`, `Commit`, `WorkflowRun` |
| `GateInputs` | Fingerprint, child-counts hash, `updated_at`, node id, whether the entity is closed |
| `CountGap` / `CountDrift` | A declared-versus-stored shortfall found by verification, and one accepted after the repair budget |
| `SyncSummary` | Per-table counters of a completed run, stored as the session's `summary_json` |

### 3.2 Component Model

```
            REST handlers                      LocalClient (SDK)
                 │                                   │
                 ▼                                   ▼
┌────────────────────────────── Service ──────────────────────────────┐
│ policy enforcer · active syncs · claim gates · sessions              │
│ prepare_sync ──► enqueue_sync_scoped ──► sync channel (64)           │
│             └──► sync_now ──► run_and_record (on its own task)       │
└──────────────────────────────────────────────────────────────────────┘
                 │ channel
                 ▼
        SyncPoolRunner  (per-tenant queues, max_concurrent_syncs workers)
                 │ run_sync_job
                 ▼
        RepoPhaseRunner (TaskQueue, lanes, max_concurrent_tasks)
                 │
                 ▼
        MirrorWorker ── ChangeGate (fingerprints) ── SweepWatermark
          │        │
          ▼        ▼
   GithubClient   SyncWriter / repositories (SeaORM, tenant-scoped)
   (semaphore, cooldown, HttpCache)
```

#### Service

- [x] `p1` - **ID**: `cpt-cf-github-mirror-component-service`

Authorization, sessions, claims, the queue, resume, cache clear, reads.

#### SyncPoolRunner

- [x] `p1` - **ID**: `cpt-cf-github-mirror-component-sync-pool-runner`

Takes queued jobs, keeps one queue per tenant and serves tenants in turn, runs up to `max_concurrent_syncs`.

#### RepoPhaseRunner

- [x] `p1` - **ID**: `cpt-cf-github-mirror-component-repo-phase-runner`

Drives one repository through the phases; claims tasks round-robin across three lanes.

#### MirrorWorker

- [x] `p1` - **ID**: `cpt-cf-github-mirror-component-mirror-worker`

Executes every task kind: fetches through `GithubPort`, writes through `SyncWriter`.

#### ChangeGate

- [x] `p1` - **ID**: `cpt-cf-github-mirror-component-change-gate`

Decides whether an entity needs a detail fetch; records fingerprints.

#### SweepWatermark

- [x] `p1` - **ID**: `cpt-cf-github-mirror-component-sweep-watermark`

Per-family high-water mark for incremental listing sweeps.

#### GithubClient

- [x] `p1` - **ID**: `cpt-cf-github-mirror-component-github-client`

REST and GraphQL calls with conditional requests, pagination, retries, the rate-limit cooldown and the body cap.

#### HttpCache

- [x] `p1` - **ID**: `cpt-cf-github-mirror-component-http-cache`

Response store with compression and a content hash, in `gm_http_cache` or, with `cache_dir` set, in files. Responses of a repository Discovery saw as public go to one shared partition (`tenant_id` is the nil UUID) that every tenant reads and writes through a scope for that tenant only. Private repositories, and the repository metadata call made before the visibility is known, stay in the caller's partition. A shared body is served only after GitHub answers `304` to the caller's own request.

### 3.3 API Contracts

#### Gear API (`/github-mirror/v1`)

| Method and path | What it does |
|---|---|
| `POST /repos/{owner}/{name}/sync` | Queue a sync. Query: `force`, `force_full`, `include`, `actions_scope`, `reactions_scope`, `timeline_scope`, `since`. `202` with the session id; a repeat on the same terms joins the running sync; other terms get `409` |
| `POST /sync/resume` | Re-queue repositories still `in_progress` (optionally one, `?repo=owner/name`) |
| `GET /sessions`, `GET /sessions/{id}` | Session list and one session: status, progress, error, summary |
| `GET /sync-status` | Per-repository run status; filter `status=in_progress\|complete` |
| `DELETE /cache` | Clear the caller's cached responses for `?owner=` or `?repo=owner/name`; shared public-repository entries stay, and a sync with `force=true` re-fetches them |
| `GET /repos` | Mirrored repositories |
| `GET /repos/{owner}/{name}/commits/{sha}/files`, `GET /repos/{owner}/{name}/pulls/{number}/threads` | Data GitHub's REST paths do not serve in this shape |
| `GET /health` | Liveness |

Errors use the platform's canonical problem body; a validation error names the parameter in `context.field_violations`.

#### GitHub-Compatible API (root)

`/user`, `/user/repos`, and under `/repos/{owner}/{name}`: the repository, issues (with comments, events, reactions, timeline), pull requests (with reviews, review comments, files, commits), commits (with comments, statuses, check runs), branches, tags, releases, labels, milestones, contributors, deployments, workflow runs and their jobs. Responses use GitHub's field names, paging (`page`, `per_page` up to 100) and error body.

#### SDK

`GithubMirrorClientV1` in `github-mirror-sdk`: `status`, `list_repos`, `sync_repository`. `LocalClient` serves it in process; `sync_repository` runs a sync on a task of its own, waits for it and returns its `SyncSummary` (see [In-Process Sync](#in-process-sync)).

#### Ports

| Port | Used for |
|---|---|
| `GithubPort` | Every GitHub read the engine makes: listings, details, actions, the cache clear |
| `SyncWriter` | Every write a sync makes, one transaction per call |
| `*Repository` traits | Reads for the API and the few reads the engine needs (for example `open_head_shas`) |

### 3.4 Internal Dependencies

| Dependency | Used for |
|---|---|
| toolkit-db | Database handle, advisory locks, the migration runner |
| Secure ORM (toolkit-db) | `scope_with(AccessScope)` on every query, tenant checks on writes |
| Policy enforcer (authz resolver SDK) | Access decisions per resource type (`github_mirror.*`) and action (`list`, `get`, `upsert`, `sync`) |
| Canonical errors (toolkit) | Problem bodies for the gear API |
| `OperationBuilder` (toolkit) | Route registration and OpenAPI, including enum query parameters |

### 3.5 External Dependencies

#### GitHub REST API

Base URL from `api_base_url` (default `https://api.github.com`; an Enterprise server is `https://host/api/v3`). Requests pin API version `2022-11-28`, ask for 100 items per page, follow `Link: next` only on the same origin, and refuse to send the token over plain HTTP to a host that is not loopback.

#### GitHub GraphQL API

Used only for pull-request review threads, which REST does not serve. The endpoint is `{base}/graphql` on github.com and `https://host/api/graphql` on Enterprise. A response with `data` and field errors keeps the data and marks the pull's threads incomplete; `data: null` is a refusal.

### 3.6 Interactions & Sequences

#### Queued Sync

- [x] `p1` - **ID**: `cpt-cf-github-mirror-seq-queued-sync`

```mermaid
sequenceDiagram
    participant C as REST caller
    participant S as Service
    participant P as SyncPoolRunner
    participant R as RepoPhaseRunner
    participant G as GitHub
    C->>S: POST /repos/rust-lang/rust/sync
    S->>S: prepare_sync: claim, session "queued", repo "in_progress"
    S->>P: channel send (SyncJob)
    S-->>C: 202 { session_id, status: queued }
    P->>R: run_sync_job (session "in_progress", heartbeat every 2 s)
    R->>G: discover, index, refine, verify
    R-->>P: summary
    P->>S: session "complete", repo "complete", claim released
    C->>S: GET /sessions/{id}
    S-->>C: status, progress, summary
```

A second request for the same repository while the first is queued or running gets the same session on the same terms, and `409` on other terms. A full queue or a stopped pool fails the new session at once and answers `503`; a full queue adds `Retry-After: 30`.

#### In-Process Sync

- [x] `p1` - **ID**: `cpt-cf-github-mirror-seq-in-process-sync`

`LocalClient::sync_repository` calls `Service::sync_now`: the same `prepare_sync` (claim, session row, repository status), then the run itself on a task of its own that the service starts and tracks (`spawn_sync_now`) and `LocalClient` awaits, so a caller that drops the call does not stop it, then the run's `SyncSummary` or its own error. It gets the deadline, the heartbeat and a `/sessions` row; it does not take a pool slot, but at most `max_concurrent_syncs` in-process syncs run at once, and `stop()` waits for them. A sync of the same repository already in flight answers `Conflict`.

#### Resume

- [x] `p1` - **ID**: `cpt-cf-github-mirror-seq-resume`

`POST /sync/resume` lists repositories whose run status is `in_progress` and queues each with the configured scope. A repository that cannot be queued (another run on different terms, or a full queue) is listed in the answer's `failed` with its error; with `?repo=owner/name` that one repository's error is returned instead. The repository status stays `in_progress` after a failed or interrupted run on purpose; only a completed run moves it to `complete`.

#### Cache-Before-Network Request

- [x] `p1` - **ID**: `cpt-cf-github-mirror-seq-cache-first`

```mermaid
sequenceDiagram
    participant W as MirrorWorker
    participant C as GithubClient
    participant H as HttpCache
    participant G as GitHub
    W->>C: GET /repos/rust-lang/rust/issues?page=1
    C->>H: look up (tenant, key)
    H-->>C: body, ETag
    C->>G: GET with If-None-Match
    alt 304 Not Modified
        C-->>W: cached body, no quota spent
    else 200
        C->>C: read at most 64 MiB
        C->>H: store body (compressed), ETag, next link
        C-->>W: new body
    end
```

### 3.7 Database Schemas & Tables

#### Table: gm_sync_sessions

| Column | Description |
|---|---|
| `tenant_id`, `id` | Primary key |
| `repo_full_name`, `repo_id` | The repository; `repo_id` once discovery has run |
| `status` | `queued`, `in_progress`, `complete`, `failed`, `interrupted` |
| `progress_percent` | 0 to 100, written by the heartbeat |
| `error` | Public text of the error that ended a `failed` or `interrupted` run |
| `summary_json` | The `SyncSummary` of a `complete` run |
| `created_at`, `started_at`, `ended_at`, `updated_at` | `updated_at` is the heartbeat |

Transitions: `queued` → `in_progress` when a worker (or `sync_now`) starts the run, or `queued` → `failed` when it cannot start (its scope or its row cannot be read or written); `in_progress` → `complete`, `failed` (an error, or the deadline) or `interrupted` (cancelled by a shutdown). A `queued` or `in_progress` row whose `updated_at` has not moved for five minutes, and whose `gm_active_syncs` row has gone as long without a refresh, has nobody behind it and is set to `interrupted`: at start-up, and once more five minutes later, for a process that died just before this one started. Another replica keeps both fresh, so its live runs are left alone. A run that starts clears `ended_at` and `error`.

#### Table: gm_repo_sync_status

| Column | Description |
|---|---|
| `tenant_id`, `repo_full_name` | Primary key |
| `repo_id` | Once known |
| `status` | `in_progress` or `complete` |
| `last_session_id` | The session that last touched it; used to tell whether its holder is still alive |
| `last_synced_at` | When a run last completed |

#### Table: gm_active_syncs

| Column | Description |
|---|---|
| `tenant_id`, `repo_full_name` | Primary key: at most one queued or running sync per repository, across every process |
| `session_id` | The session holding the repository |
| `scope_json`, `since` | The terms it runs on; a request on the same terms joins it, others get `409` |
| `owner_id`, `updated_at` | The process that holds it, which refreshes `updated_at` every minute |

#### Table: gm_http_cache

| Column | Description |
|---|---|
| `tenant_id`, `cache_key` | Primary key; the key is computed from method, URL and `Accept`; `tenant_id` is the nil UUID for shared public-repository entries |
| `url`, `status` | What was fetched |
| `etag`, `last_modified`, `next_page` | Validators and the `Link: next` of the page |
| `body`, `compression` | The body as stored (`gzip` by default, or `none`) |
| `content_hash` | SHA-256 of the uncompressed body |
| `fetched_at` | When the entry was written |

#### Table: gm_sync_watermarks

| Column | Description |
|---|---|
| `tenant_id`, `repo_id`, `family` | Primary key (`issues`, `pull_requests`, `commits`) |
| `last_seen_updated_at` | High-water mark promoted after a complete sweep |
| `page1_etag`, `last_head_sha` | Page one's `ETag` and, for commits, the head commit, both from the last complete sweep |
| `sweep_in_progress`, `candidate_high_water` | The sweep under way and the mark it will promote |

#### Table: gm_entity_fingerprints

| Column | Description |
|---|---|
| `tenant_id`, `repo_id`, `family`, `entity_id` | Primary key |
| `fingerprint`, `child_counts_hash`, `updated_at`, `node_id` | What the gate compares |
| `last_refined_at`, `refinement_status` | `pending` until the detail fetch completes, then `complete` |

#### Table: gm_logical_conversations

| Column | Description |
|---|---|
| `tenant_id`, `repo_id`, `conv_type`, `root_comment_id` | Primary key; `conv_type` is `inline` (review comments chained by `in_reply_to_id`) or `toplevel` (issue or pull-request comments tied together by quoting) |
| `parent_kind`, `parent_number` | `pull_request` or `issue`, and its number |
| `comment_count`, `created_at` | How many comments the conversation holds, and GitHub's stamp of its root |
| `is_resolved` | For `inline`, the resolution of the review thread paired with it by comment count; `NULL` for `toplevel` |
| `extracted_at` | When the grouping pass last wrote the row |

Derived after every completed sync whose scope has issues or pull requests (`domain/sync/conversations.rs`, ported from the reference implementation's `logical.rs`): a normal run re-groups only the issues and pull requests whose comments the run wrote; `--force` and `--force-full` re-group the whole repository. Each member comment carries its root's id in `conversation_id` on `gm_comments` and `gm_review_comments`. A top-level comment counts as a reply when its blockquoted lines share at least 16 consecutive characters, case and spacing ignored, with an earlier comment of the same issue. A failure of the pass is logged and does not fail the sync.

#### Mirrored Entity Tables

`gm_repositories`, `gm_issues`, `gm_pull_requests`, `gm_commits`, `gm_comments`, `gm_review_comments`, `gm_reviews`, `gm_review_threads`, `gm_labels`, `gm_milestones`, `gm_releases`, `gm_branches`, `gm_tags`, `gm_contributors`, `gm_workflow_runs`, `gm_workflow_jobs`, `gm_pull_request_files`, `gm_pull_request_commits`, `gm_commit_files`, `gm_commit_comments`, `gm_commit_statuses`, `gm_check_runs`, `gm_issue_events`, `gm_issue_reactions`, `gm_issue_timeline`, `gm_deployments`. Each has `tenant_id` in its key and an `extracted_at` stamp written by every upsert.

Tasks have no table: the queue is in memory.

#### Migrations

`m0001_initial` to `m0044_session_telemetry`, applied in name order by the toolkit migration runner. A change to a table that already holds rows is additive, and every migration has a `down()` (see the PRD's schema-change rule). A new migration takes the next number.

### 3.8 Core Algorithms

#### 3.8.1 Incremental Listing Sweep

Issues and pull requests are listed newest first. A sweep starts from the family's `last_seen_updated_at` minus a five-minute overlap and stops at the first page whose rows are all older. The newest `updated_at` seen is staged as the candidate; it is promoted only when the whole family, including its refinements, has finished. A run that stops early promotes nothing, so the next run walks the listing again and the gate re-seeds whatever was left `pending`. `force_full` and `force` ignore the watermark. A run narrowed by `since`, or run on a scope narrower than the gear's configured one in any object type or collection mode, moves no watermark: it may have skipped rows a default run would store, so the next default sync walks from where the last default one stopped.

Commits have no date watermark. Their only date is `committer.date`, which the committer's machine sets: a commit dated in the future would push the bound past every later commit, and one made before the last sync but pushed after it would fall below the bound. The commits sweep keeps the head commit instead. When page one's `ETag` has changed and a head from the last complete sweep is stored, it asks `GET /repos/{owner}/{name}/compare/{last_head}...{head}` for the commits added since, whatever their dates, and then walks the commit comments as usual. The first sync, `force_full` or `force`, and a head GitHub no longer has below the new one (a force push answers `diverged` or `404`) walk the whole listing instead. The new head is promoted with the page-one `ETag`, at the same family-complete point.

Workflow runs have no watermark either: a sync reads the newest ten pages of `/actions/runs` (1,000 runs) and stops there, and older runs keep what was stored. Runs are never reconciled away, so the cap removes nothing.

#### 3.8.2 Change Gate

An entity is refined when it is new, its fingerprint or child-counts hash changed, its last refinement did not complete, its refresh TTL ran out, or the run is forced (`force_full` or `force`). TTLs:

| Entity | Open | Closed |
|---|---|---|
| Issue | 4 hours | 7 days |
| Pull request | 2 hours | 7 days |
| Commit | 1 hour while its CI is still collected | never |
| Workflow run | 1 day | 1 day |

After a refinement the entity is marked `complete`. A pull whose review threads came back incomplete keeps `pending`, so the next run fetches it again.

#### 3.8.3 Deletion Reconciliation

Rows are hard-deleted, not tombstoned. After a listing fetched to completion, rows of that family whose `extracted_at` predates the run's start were not seen and are removed. A truncated listing, or a family left out of the scope, proves nothing and deletes nothing. A pull's reviews, files and commits are replaced as a whole on each refinement; its review threads only when the thread fetch was complete.

#### 3.8.4 Scheduler

One in-memory `TaskQueue` per run. Discovery runs alone; indexing, change detection and refinement drain together (indexing seeds refinement as pages arrive); verification runs last. Tasks are claimed by priority, then age, round-robin across three lanes (pull requests, issues, everything else) so a long pull-request backlog cannot starve issues. While 10,000 tasks are pending and others are running, no new Indexing task is claimed; refinement keeps every slot busy until the backlog drops. A task that fails on database contention is retried up to three times with a growing delay, counted in `retries`, apart from its repair pass in `attempt`.

#### 3.8.5 Verification

For every pull request the declared commit and file counts are compared with what was stored. A shortfall seeds a repair pass (`Verify`, priority `HIGH`) that fetches the pull again. After three passes, or when a pass does not shrink the gap, the gap is accepted: logged as a warning and recorded in the summary's `accepted_drift`, and the session still ends `complete`.

#### 3.8.6 Collection Scopes

| Scope | `all` | `open` (default for actions and reactions) | `none` (default for timeline) |
|---|---|---|---|
| `reactions_scope`, `timeline_scope` | every issue and pull request | open ones only | not fetched |
| `actions_scope` | jobs of every workflow run; statuses and check runs of every commit | jobs of runs on the head commit of an open pull request (from `gm_pull_requests`); no commit statuses or check runs | neither |

An issue is refined when either its reactions or its timeline scope wants it.

`inline_comment_snippets` (`before`, `after`; CLI `--inline-comment-snippet-before/after <N>`, TOML `scope.collection.inline_comment_snippets`) keeps code context with every review comment: the N lines of its `diff_hunk` above the commented line in `snippet_before` and below it in `snippet_after`, `@@` header and diff markers removed; `-1` keeps the whole side, `0` (the default) nothing. GitHub's `diff_hunk` ends at the commented line, so `snippet_after` stays empty unless a hunk carries lines below it. A sync asking for more context than a running one does not join it.

#### 3.8.7 Claims, Locks and Liveness

| Mechanism | Scope | Purpose |
|---|---|---|
| Active sync (`gm_active_syncs`) | the database, per tenant and repository | One queued or running sync per repository, across every process; a request is joined or refused against it. Inserted before the session row; deleted by a guard carried in the job, so a job that ends, is dropped from the queue or is aborted gives it back. A row whose owner has not refreshed it for 300 seconds, or whose session has ended, is taken over |
| Claim gate | this process, per repository | Serializes the check, the session write and the claim of concurrent requests for one repository; dropped when nobody holds it |
| Advisory lock `sync/{tenant}/{owner}/{name}` | toolkit-db, across processes | One run per repository; held for the whole run; a held lock answers `409` |
| Heartbeat | the session row | `updated_at` every 2 seconds; a row silent for 300 seconds is abandoned, and its lock may be taken |

#### 3.8.8 Deadline

A run races `sync_deadline_minutes`. When the deadline wins, the run's own cancellation token is cancelled, the run winds down (tasks in flight finish their writes, the lock is released), and the session ends `failed` with "ran past its deadline of N minutes and was stopped; the next sync carries on from what it had already stored". The repository stays `in_progress`, so resume picks it up.

#### 3.8.9 GitHub Client

| Behaviour | Value |
|---|---|
| Requests in flight | `max_concurrent_requests` (default 8), one semaphore for all syncs |
| Rate limit | a `429`, or a `403` with rate-limit headers, sets one shared cooldown from `Retry-After` / `X-RateLimit-Reset` (at most 5 minutes per wait, 30 waits per request) |
| Server errors and send failures | 3 retries, backing off from 2 seconds |
| Timeouts | 10 s to connect, 60 s per request |
| Body cap | 64 MiB for any response and any cached body, read piece by piece |
| Review threads | 20 GraphQL pages of 100 per pull at most; past that the walk stops with a warning |

## 4. Additional Context

### Sync Phases

| Phase | Tasks | Share of progress |
|---|---|---|
| 1. Discovery | fetch the repository, seed one index task per enabled family | 2% |
| 2. Indexing | walk each family's listing, write it, seed refinements | 13% |
| 3. Change detection | the gate, run inside indexing | (part of indexing) |
| 4. Refinement | one detail fetch per entity the gate let through | 80% |
| 5. Verification | count checks and repair passes | 5% |

### Priority Tiers

| Tasks | Priority |
|---|---|
| Pull-request listing; refinement of open pull requests | 600 |
| Issue listing; refinement of open issues | 500 |
| Commit, metadata (labels, milestones, releases, branches, tags) and actions listings | 400 |
| Refinement of closed pull requests | 300 |
| Refinement of closed issues | 200 |
| Repair passes | 100 |
| Discovery, commit refinement, workflow-run jobs | 0 |

### Consistency Model

Each task commits on its own, so a reader can see a repository part way through a sync: new issues next to pull requests from the previous run. `GET /sync-status` says whether a repository is `in_progress` and when its last run completed. Every entity converges to its latest state by the end of a complete run.

### Security

Every read and write passes the policy enforcer for its resource type and is scoped to the caller's tenant in storage. A read the caller may not make answers `404`, so it does not confirm that a repository exists. The token is kept as a secret, never logged, and never sent over plain HTTP except to loopback. Logged text is redacted, and the bodies of GitHub's GraphQL errors are logged rather than stored in a session's `error`.

### Operations

Configuration (`config` of the gear):

| Setting | Default |
|---|---|
| `api_base_url` | `https://api.github.com` |
| `github_token_secret` | none (`tenant_id` and `key` of the token's secret in the credential store) |
| `scope` | every family except security; actions and reactions `open`, timeline `none` |
| `cache_compression` | `gzip` |
| `max_concurrent_syncs` | 4 |
| `max_concurrent_tasks` | 4 |
| `max_concurrent_requests` | 8 |
| `sync_deadline_minutes` | 360 |
| `telemetry_dir` | none; no telemetry file is written |
| `cache_max_age_days` | none; cached responses are kept until cleared |
| `cache_dir` | none; cached responses go to `gm_http_cache` |
| `tenants.<tenant_id>.cache_max_age_days` | `cache_max_age_days` |

At the start of each sync the gear deletes the syncing tenant's `gm_http_cache` rows whose `fetched_at` is older than that tenant's `cache_max_age_days`, and the shared public-repository rows older than the gear-wide `cache_max_age_days`. A `304` does not rewrite a row, so `fetched_at` is the time of the last `200`. A delete that fails is logged and the sync goes on.

With `cache_dir` set, cached responses are files instead of `gm_http_cache` rows: `<cache_dir>/<tenant_id>/<first two characters of the key>/<key>` holds the body, compressed as `cache_compression` says, and `<key>.meta.json` next to it holds `url`, `etag`, `last_modified`, `next_page`, `compression`, `content_hash` and `fetched_at`. Shared public-repository entries use the nil-UUID folder. Both files are written to a temporary name and renamed into place. `DELETE /cache` and `cache_max_age_days` read every `.meta.json` file of the tenant's folder to find what to delete.

With `telemetry_dir` set, every GitHub call of a sync, retries included, appends one JSON line to `<telemetry_dir>/<tenant_id>/<owner>/<name>.jsonl`: `session_id`, `repository`, `api`, `method`, `url` (query string removed), `status`, `outcome`, `duration_ms`, `rate_limit_remaining`, `rate_limit_reset`, `cache_hit`, `etag_used`, `response_bytes`, `graphql_points`, `requested_at`, and `tasks_pending`, the queue depth when the call was made. An in-process caller of `Service::sync_now` may pass its own file name in place of `<name>.jsonl`. It stays in the same folder, must be 1-100 characters from `[A-Za-z0-9._-]`, and nothing is written without `telemetry_dir`; a REST request cannot name a file. The gear only appends; rotating and deleting the files is the operator's. A write that fails is logged once per session and the sync goes on. The session's own totals, `cache_hit_ratio` included, are on `GET /github-mirror/v1/sessions/{id}`.

The database pool should allow at least `max_concurrent_syncs` × `max_concurrent_tasks` connections plus one per sync for its heartbeat and session writes (20 with the defaults).

### CLI

`github-mirror-cli` builds the `github-mirror` binary. It starts the same gear runtime in its own process (github-mirror, credstore, the AuthN, AuthZ and tenant resolvers with their plugins, and types-registry), takes the gear's `Service` from ClientHub, runs one command and stops. The runtime refuses to start without an HTTP gateway when a linked gear has REST routes, so `api-gateway` is linked too, bound to `127.0.0.1:0` and never called.

| Command | What it does |
|---|---|
| `sync <ORG/REPO>`, `resume <ORG/REPO>` | `Service::sync_now`; a repeat continues from the stored watermarks. The answer is the same report `status` prints (below), with the run's own counts. `sync` takes `--include`/`--exclude` (object types, with the PRD's aliases), `--actions-scope`/`--reactions-scope`/`--timeline-scope`, `--inline-comment-snippet-before/after`, `--since` (`YYYY-MM-DD`, `Nd`, `Nw` or `Nm`) and `--max-concurrent`, each mapped onto the request the REST `sync` route accepts. `--force-full` walks every listing and refines every entity but keeps asking GitHub whether a page changed; `--force` does that and also fetches every page afresh |
| `query <ENTITY> <ORG/REPO>` | Reads mirrored rows; `--number` for comments, reviews, review comments, review threads, reactions and timeline, optional for `conversations`, which prints each issue or pull request's derived conversations with their comments (`Service::list_conversations`, [gm_logical_conversations](#table-gm_logical_conversations)); `--limit`; `--since <CUTOFF>` (as `sync`'s) keeps rows whose GitHub stamp is at or after it (`updated_at`; `committed_at` for commits, `submitted_at` for reviews, `created_at` for events, reactions, timeline and releases; labels, branches, tags, files, jobs, check runs and contributors have no such stamp and ignore it), `--extracted-since <ISO 8601>` keeps rows a sync wrote at or after it (`extracted_at`, every entity), `--subject-type line\|file` for review comments; the cutoffs are applied in SQL through `PageWindow`, before `--limit` |
| `status <ORG/REPO>` | Five tables (one JSON object each with `--output-format json`): `repository` (the latest session: status, progress, `started_at`, `ended_at`, `duration_ms`, error), `run` (the `gm_repo_sync_status` row), `objects` (rows synced per type from the session's summary, plus `stale_rows_deleted` and `accepted_drift_total`), `api` (the session's telemetry: REST and GraphQL calls, fresh and `304` answers, rate-limit waits, bytes, `cache_hit_ratio`, task and entity counts) and `storage`: `cache_bytes`, what the HTTP cache holds for the repository (`Service::cache_size`, the same entries `clear-cache` drops; entries in the shared public partition are not counted), and `database_bytes`, the size of the gear's SQLite file and its `-wal` file when there is one, `null` on PostgreSQL and MariaDB. With `per_repo` placement that is the repository's own size; with `per_org` or `shared` the file holds other repositories too and the repository's own share is not available |
| `check-rate-limit` | `Service::rate_limit`: the token's quotas on every GitHub pool from `GET /rate_limit`, which costs nothing; `core` and `graphql` first |
| `clear-cache <ORG/REPO>` | `Service::delete_repository`: every mirrored row of the repository, its watermarks, fingerprints, sessions, run status and cached responses, then the repository row, in one transaction; refused with `409` while a sync of it is in flight. This is the per-repository deletion `cpt-cf-github-mirror-nfr-data-governance` asks for; the REST `DELETE /cache` keeps dropping cached responses only |

Configuration is one TOML file (`--config`, default `./github-mirror.toml`) merged over the built-in template that `--print-config` prints; its tables are the runtime's own settings, plus `[cli] tenant_id` and `[cli] database_placement`. Three flags override parts of it, with repotap's meaning: `--storage-dir <DIR>` (or `GITHUB_MIRROR_STORAGE_DIR`) sets `server.home_dir`, the root under which the toolkit keeps every gear's files (`<DIR>/github-mirror/`, `<DIR>/credstore/`); `--database-url <URL>` (or `GITHUB_MIRROR_DATABASE_URL`) replaces `gears.github-mirror.database` with that one `dsn`, `sqlite://`, `postgres://` or `mysql://`, after which placement is moot; `--database-placement <MODE>` (flag over TOML, default `per_repo`) picks the SQLite file when the gear's database is a `file`: `per_repo` is `<DIR>/github-mirror/cache/<owner>_<name>/github_mirror.db`, `per_org` is `<DIR>/github-mirror/cache/<owner>/github_mirror.db`, `shared` is `<DIR>/github-mirror/github_mirror.db`. `check-rate-limit` names no repository and opens the shared file. A per-repository or per-owner file holds that scope alone, HTTP cache included, so the shared public-repository partition only helps within one file. The GitHub token comes from `--token`, `GITHUB_TOKEN` or `~/.github-mirror/gh_token.txt` and reaches the gear through an in-memory static credstore secret. The platform token comes from `--platform-token`, `CF_PLATFORM_TOKEN` or `~/.github-mirror/platform_token.txt`; the AuthN resolver turns it into the `SecurityContext`, and when `cli.tenant_id` is set it must match the token's tenant. The `static-authn` feature (default) accepts any non-empty platform token as the plugin's configured identity, for local use; `oidc-authn` checks it against the platform's login server.

### Future Work

- A token per tenant, with its own rate-limit budget; shared public-repository entries would then need the token in the key, since GitHub's `permissions` field differs per token.
- An "as of" marker on read endpoints for readers that must not see a repository mid-sync.
- The CLI, Python bindings and write-back described in the PRD.

## 5. Traceability

- **PRD**: [PRD.md](./PRD.md)
- **Reference design**: repotap `docs/DESIGN.md`, adapted as described in [1.1](#11-architectural-vision)
