# Algorithms — GitHub Mirror

- [ ] `p1` - **ID**: `cpt-cf-github-mirror-algorithms`

## 1. Purpose

This document captures the synchronization and incremental-fetch algorithms that the GitHub Mirror design inherits from the current prototype implementation. It is the detailed algorithm companion to `DESIGN.md` and focuses on reducing upstream GitHub API cost while preserving correctness, resumability, and authorization boundaries.

## 2. Source-grounded algorithm inventory

| Priority | Algorithm | Primary design requirement | Prototype source area |
|----------|-----------|----------------------------|-----------------------|
| `p1` | Scope and collection-cost bounding | `cpt-cf-github-mirror-fr-sync-scope` | CLI config resolution and engine scope defaults |
| `p1` | Cache-before-network conditional requests | `cpt-cf-github-mirror-fr-cost-efficiency` | HTTP client cache probe, validators, and raw-response store |
| `p1` | Visibility-aware cache partitioning | `cpt-cf-github-mirror-nfr-security` | HTTP `CacheStore` and repository visibility partitions |
| `p1` | Incremental list sweep with watermark and page-1 ETag | `cpt-cf-github-mirror-fr-cost-efficiency` | indexing watermark and list sweeper |
| `p1` | Fingerprint-based refinement gate | `cpt-cf-github-mirror-fr-idempotent` | entity fingerprint store |
| `p1` | Re-enterable resume-by-rescan | `cpt-cf-github-mirror-fr-session-resume` | CLI `resume` command and fingerprint statuses |
| `p1` | Rate-limit admission and adaptive soft cap | `cpt-cf-github-mirror-fr-rate-limit` | rate-limit controller |
| `p1` | `--since` closed-partition bounding | `cpt-cf-github-mirror-fr-sync-order` | CLI extraction config |
| `p2` | Cost-governed GraphQL PR extraction | `cpt-cf-github-mirror-fr-commit-ci-refinement` | GraphQL PR extractor |
| `p2` | Logical-conversation incremental regrouping | `cpt-cf-github-mirror-fr-extended-api` | logical conversation derivation |
| `p2` | Telemetry and summary cost accounting | `cpt-cf-github-mirror-fr-telemetry` | telemetry and summary types |

## 3. Scope and collection-cost bounding

### 3.1 Object scope resolution

The CLI is the environment/config boundary. It resolves a concrete object extraction scope before calling the library.

Precedence is:

1. CLI `--include`: start from no objects and enable only named objects.
2. CLI `--exclude`: start from defaults and disable named objects.
3. Config `[extract] include`: start from no objects and enable only configured objects.
4. Config `[extract] exclude`: start from defaults and disable configured objects.
5. Built-in defaults.

Default enabled object families are:

- `issues`
- `pull_requests`
- `commits`
- `releases`
- `branches`
- `labels`
- `milestones`
- `github_actions`
- `contributors`

Security extraction is disabled by default because those endpoints need elevated permissions.

### 3.2 Expensive sub-resource collection scope

Expensive per-issue/PR child resources are controlled separately from object-family enablement.

Supported modes are:

- `all`: collect for all issues/PRs.
- `open`: collect only for open issues/PRs.
- `none`: do not collect the sub-resource.

Default collection scope is:

| Sub-resource | Default | Reason |
|--------------|---------|--------|
| Actions / CI checks | `open` | Open PRs are active and actionable. |
| Reactions | `open` | Open discussions need current sentiment; closed history can be skipped by default. |
| Timeline | `none` | Timeline is high-volume and low-signal unless explicitly requested. |

### 3.3 `since` cutoff policy

The `since` cutoff accepts `YYYY-MM-DD`, `Nd`, `Nw`, or `Nm`. Open issues and PRs are always included. Closed or merged issues and PRs are included only when created, closed, merged, or updated at or after the cutoff.

This keeps active work complete while bounding historical partitions.

## 4. Cache-before-network conditional request algorithm

### 4.1 Request pipeline

Every GitHub request follows a cache-before-network flow unless force refresh is enabled.

```text
FUNCTION fetch_native(request):
    visibility = resolve_repo_visibility(request.repo_id)
    partition = cache_partition(tenant_id, request.owner, request.repo, visibility)
    key = canonical_cache_key(partition, request)

    IF force_refresh:
        cached = None
    ELSE:
        cached = cache.get(key, partition)

    IF cached is hit:
        RETURN cached_body_without_network

    validators = cache.peek(key, partition)

    headers = base_headers()
    IF validators.etag: headers[If-None-Match] = validators.etag
    IF validators.last_modified: headers[If-Modified-Since] = validators.last_modified

    response = github_http_request(headers)

    IF response.status == 304 AND cached body exists:
        RETURN cached_body_as_not_modified

    persist_raw_response_before_normalization(response)
    cache.put(key, partition, response.body, validators, visibility)
    RETURN response.body
```

### 4.2 Raw-response persistence

Successful response bodies are persisted before normalization. The filesystem store layout is:

```text
{cache_root}/{visibility_partition}/{host}/{owner}/{repo}/{api_type}/{endpoint}/{page_or_cursor}
```

Each body has a companion `.meta.json` with:

- URL and method
- HTTP status
- `ETag`
- `Last-Modified`
- fetch timestamp
- SHA-256 of the uncompressed body
- compression mode
- schema version
- rate-limit remaining
- `Link rel="next"` pagination URL

Compression modes are `none`, `gzip`, and `zstd`. Integrity verification always hashes the uncompressed body so the hash is independent of storage compression.

## 5. Visibility-aware cache partitioning

The cache stores public repository raw responses in a shared public partition and private or visibility-unknown repository raw responses in a tenant-scoped partition.

```text
FUNCTION cache_partition(tenant_id, owner, repo, visibility):
    IF visibility == public:
        RETURN public/{owner}/{repo}

    RETURN tenant/{tenant_id}/{owner}/{repo}

FUNCTION cache_read(session, request):
    visibility = resolve_repo_visibility(request.repo_id)
    partition = cache_partition(session.tenant_id, request.owner, request.repo, visibility)
    key = canonical_cache_key(partition, request)

    RETURN cache body and validators if present in partition

FUNCTION cache_write(session, request, response):
    IF response.status == 200:
        visibility = response.repo_visibility or resolve_repo_visibility(request.repo_id)
        partition = cache_partition(session.tenant_id, request.owner, request.repo, visibility)
        store body and validators under canonical key in partition
```

Invariants:

- Public repository bodies and validators can be reused across tenants through the public partition.
- Private and visibility-unknown repository bodies and validators are keyed by `tenant_id` and never cross tenant boundaries.
- Consumer authorization is enforced by the GitHub Mirror API over normalized data, not by raw cache reads.
- Raw tokens are never stored; token fingerprints are limited to telemetry, rate-limit accounting, and credential health where needed.

## 6. Incremental list sweep algorithm

### 6.1 Watermark state

Each `(repo, endpoint_family)` has a durable watermark record:

- `last_seen_updated_at`
- `page1_etag`
- `sweep_in_progress`
- `candidate_high_water`

The current implementation stages candidate high-water marks and promotes them only when the dependent synchronization work is complete.

### 6.2 Sweep start

```text
FUNCTION start_sweep(repo, family, page1_etag, force, ignore_watermark_bounds):
    watermark = load_watermark(repo, family)
    epoch = 1970-01-01T00:00:00Z

    IF force OR ignore_watermark_bounds:
        stop_threshold = epoch
    ELSE:
        stop_threshold = watermark.last_seen_updated_at - overlap
        IF no watermark exists: stop_threshold = epoch

    IF NOT force AND NOT ignore_watermark_bounds AND page1_etag == watermark.page1_etag:
        RETURN clean

    mark_sweep_started(repo, family, page1_etag)
    RETURN SweepState(stop_threshold, seen_ids = {}, max_seen = stop_threshold)
```

The overlap absorbs timestamp ties and clock skew at the boundary.

### 6.3 Page processing

```text
FUNCTION process_page(state, page):
    changed = []

    FOR entity IN page:
        entity_id = extract id
        IF entity_id is absent: CONTINUE
        IF entity_id already in state.seen_ids: CONTINUE
        add entity_id to state.seen_ids

        updated_at = extract updated_at
        IF updated_at exists AND updated_at < state.stop_threshold:
            CONTINUE

        state.max_seen = max(state.max_seen, updated_at)

        inputs = build_gate_inputs(entity)
        reason = fingerprint_store.evaluate(repo, family, entity_id, inputs, now, force)
        fingerprint_store.upsert_sweep(repo, family, entity_id, inputs, now)

        IF reason exists:
            changed.push(entity_id)

    RETURN changed
```

The implementation skips stale entities rather than breaking on the first stale row. This preserves correctness when pages are fetched or streamed in an order that is not strictly sequential.

### 6.4 Sweep finish

```text
FUNCTION finish_sweep(state):
    candidate = max(state.max_seen, state.stop_threshold)
    stage_candidate_high_water(repo, family, candidate)
    RETURN candidate
```

Promotion from `candidate_high_water` to `last_seen_updated_at` is reserved for the family-complete point, after the dependent refinement pass has succeeded. This prevents a crash from advancing the watermark past unrefined entities.

## 7. Fingerprint-based refinement gate

### 7.1 Fingerprints

Fingerprints are stable hashes of list-visible state. They decide whether a full detail fetch is needed.

Pull request fingerprints include:

- `updated_at`
- `head.sha`
- `base.sha`
- `state`
- `draft`
- `merged_at`
- `closed_at`
- sorted label IDs
- sorted assignee IDs
- sorted requested reviewer IDs
- milestone ID

Issue fingerprints include:

- `updated_at`
- `state`
- `closed_at`
- sorted label IDs
- sorted assignee IDs
- milestone ID

Commit fingerprints use the commit SHA because commit bodies are immutable. CI state is tracked separately.

### 7.2 Child-count hash

When list/detail payloads expose child counts, the implementation computes a hash over:

- `comments`
- `review_comments`
- `commits`
- `changed_files`
- `reactions`
- nested `reactions.total_count`

A child-count hash change triggers re-refinement even if the parent fingerprint is stable.

### 7.3 Gate reasons

```text
FUNCTION evaluate_refinement_gate(record, inputs, now, force):
    IF force:
        RETURN Forced

    IF no existing record:
        RETURN New

    IF stored.fingerprint != inputs.fingerprint:
        RETURN FingerprintChanged

    IF inputs.child_counts_hash exists AND stored.child_counts_hash != inputs.child_counts_hash:
        RETURN ChildCountsChanged

    IF stored.refinement_status != complete:
        RETURN Incomplete

    ttl = family_ttl(inputs.family, inputs.terminal)
    IF ttl exists AND stored.last_refined_at is missing or older than ttl:
        RETURN TtlExpired

    RETURN None
```

TTL defaults in the current prototype are:

| Family/state | TTL behavior |
|--------------|--------------|
| Commits | No TTL-only body re-refinement. |
| Open pull requests | 2 hours. |
| Closed or merged pull requests | 7 days. |
| Open issues | 4 hours. |
| Closed issues | 7 days. |
| Other families | 1 day. |

### 7.4 Mark-refined transaction boundary

Refinement completion is tracked in the fingerprint table. The `mark_refined_in` operation is transaction-aware so entity data and `refinement_status = complete` can commit together.

If a crash occurs after fingerprint sweep but before entity rows are stored, the next run sees `refinement_status != complete` and re-enqueues the entity even when the list endpoint is otherwise unchanged.

### 7.5 Content-change timestamps

When refined content is marked complete:

- `last_checked_at` is updated every successful check.
- `content_hash` is updated only when normalized content changes.
- `content_changed_at` is updated only when `content_hash` changes.

This separates freshness checks from actual data changes for `extracted_since`-style consumers.

## 8. Re-enterable resume-by-rescan

The current CLI `resume` behavior re-runs extraction for a repository. It does not restore a persisted task queue.

```text
FUNCTION resume(repo, config):
    resolve scope from config
    resolve collection scope from config
    resolve inline snippet config from config
    resolve since cutoff from config
    options.force = user force flag
    run extraction again
```

Cost remains low because:

- raw HTTP cache can return hits or conditional `304` responses;
- page-1 ETags can short-circuit unchanged list families;
- watermark sweeps skip old entities;
- fingerprints skip unchanged details;
- incomplete refinements are re-enqueued from durable status.

The prototype library `ExtractionSession::resume` placeholder returns not implemented for direct persisted-session reload. The gear design should preserve the proven resume-by-rescan behavior while adding any PRD-required session status APIs around it.

## 9. Rate-limit admission and adaptive soft cap

### 9.1 Admission gates

The per-token controller parks requests before they consume concurrency capacity.

```text
FUNCTION admit():
    LOOP:
        gate = snapshot(soft_cap, backoff_until, remaining, reset_at, reserve)

        IF now < gate.backoff_until:
            sleep until backoff expires
            CONTINUE

        IF gate.remaining - gate.reserve <= 0:
            authoritative = fetch_or_reuse_rate_limit_probe()
            IF authoritative has quota above reserve:
                adopt authoritative quota
                CONTINUE
            ELSE IF reset_at is future:
                sleep until reset
                refill local quota
                CONTINUE
            ELSE:
                return BudgetExhausted

        IF in_flight >= soft_cap:
            wait for released slot
            CONTINUE

        increment in_flight
        RETURN admitted
```

The reserve is approximately 10% of the hourly quota, capped to avoid stranding excessive quota.

### 9.2 Observation and AIMD

After a response, callers release the in-flight slot and feed headers back to the controller.

```text
FUNCTION observe(headers, http_status, max_cap):
    IF headers describe core REST quota:
        update remaining, limit, reset_at

    IF http_status == 429 OR Retry-After exists:
        soft_cap = max(minimum, soft_cap / 2)
        backoff_until = now + Retry-After if present
        notify waiters
        RETURN

    clear backoff

    IF remaining > 10% of limit AND soft_cap < max_cap:
        soft_cap += 1

    notify waiters
```

Only core REST quota headers drive the core budget gate. Search and GraphQL have independent rate-limit pools and must not clobber the core REST controller.

## 10. Cost-governed GraphQL pull-request extraction

The prototype GraphQL extractor is DB-free and operates behind a fetcher trait. It extracts a set of PRs plus child lists while minimizing GraphQL point cost.

### 10.1 Cost principles

- Leaf connections are point-flat in `first`; request `first: 100` for leaf lists.
- Parent connections drive point cost; `reviewThreads` is the key sized parent connection.
- GitHub GraphQL cost is approximately total request count divided by 100, rounded, minimum 1.
- Query size is bounded by node count and request count ceilings.

### 10.2 Primary phase

```text
FUNCTION extract_pull_requests(owner, repo, pr_numbers):
    prs_per_query = default chunk size
    node_ceiling = MAX_NODES_PER_QUERY

    FOR chunk of PR numbers:
        build aliased primary query
        graphql_governor.admit()
        response = governed_fetch(query)

        IF node-limit error AND prs_per_query > 1:
            prs_per_query = max(1, prs_per_query / 2)
            node_ceiling = max(1, node_ceiling / 2)
            retry same chunk

        observe GraphQL cost and remaining budget

        FOR each PR alias:
            IF alias resolved:
                store extracted PR and enqueue continuations for paginated children
            ELSE IF alias is null without field error:
                record tombstone
            ELSE:
                record per-PR failure
```

### 10.3 Continuation phase

```text
WHILE continuation work exists:
    graphql_governor.admit()
    request_ceiling = governor.pacing_request_ceiling()
    batches = greedy_first_fit(work, node_ceiling, request_ceiling)
    batch = first batch
    requeue remaining batches

    response = governed_fetch(build_continuation_query(batch))

    IF node-limit error AND batch has more than one item:
        node_ceiling = max(1, node_ceiling / 2)
        requeue batch
        CONTINUE

    observe GraphQL cost and remaining budget
    ingest continuation nodes
    enqueue follow-up continuations for remaining pages
```

### 10.4 GraphQL governor

The GraphQL governor uses:

- token-bucket pacing at 1,800 points per minute;
- hourly point reserve of 250 points;
- `rateLimit.cost`, `rateLimit.remaining`, and `rateLimit.resetAt` from responses;
- packing ceilings derived from current token bucket state.

When hourly remaining drops below the reserve, the governor sleeps until `resetAt`.

## 11. Mergeability polling

GitHub can return `mergeable: null` while it computes PR mergeability. The prototype treats this as pending and uses bounded exponential polling with fresh requests.

```text
FUNCTION poll_mergeability(pr_number, repo):
    delay = 1 second

    FOR attempt IN 1..=6:
        sleep(delay)
        raw = fetch_fresh_pr_detail_without_etag()

        IF raw.mergeable is boolean:
            RETURN Resolved(raw.mergeable)

        delay = min(delay * 2, 30 seconds)

    RETURN Indeterminate
```

A PR with unresolved mergeability keeps `mergeable_pending = true`.

## 12. Logical conversation incremental regrouping

Logical conversations are derived from stored comments and review comments. The incremental grouping algorithm accepts a `changed_since` timestamp.

```text
FUNCTION group_logical_conversations(repo, changed_since):
    IF changed_since is None:
        regroup all issue and PR comment parents
    ELSE:
        inline_scope = PRs with changed or tombstoned review comments since timestamp
        toplevel_scope = issues/PRs with changed or tombstoned top-level comments since timestamp
        regroup only scoped parents
```

Inline conversations group review comments by reply chains. Top-level conversations group comment threads using blockquote overlap. This avoids regrouping the entire repository on every incremental run.

## 13. Telemetry and summary cost accounting

Per-request telemetry records:

- session ID
- URL or GraphQL endpoint label
- method
- status
- duration in milliseconds
- rate-limit remaining
- cache hit / `304` status
- whether an ETag was used
- response bytes
- GraphQL points
- request timestamp

Session telemetry aggregates:

- total requests
- `304 Not Modified` count
- downloaded bytes
- estimated bytes saved
- cache hit ratio
- elapsed seconds
- last request timestamp

The synchronization summary adds operator-facing totals for repository identity, per-object metrics, REST/GraphQL usage, remaining budgets, elapsed time, and storage footprint.

## 14. Design invariants

The GitHub Mirror implementation should preserve these invariants from the prototype:

- The library does not read environment variables or config files; only the CLI resolves environment and TOML configuration.
- Raw responses are persisted before normalization.
- Force mode bypasses cache and conditional validators.
- Re-runs are safe and cheap because cached validators, watermarks, fingerprints, and incomplete-refinement statuses are durable.
- Shared public cache storage never bypasses GitHub Mirror API tenancy and access checks; private and visibility-unknown cache storage is tenant-partitioned.
- Rate-limited tokens do not monopolize global concurrency.
- Open issues and PRs are never excluded by `since` cutoffs.
- High-cost child resources are independently scope-gated.
- GraphQL is used where batching reduces cost; REST with ETags remains preferred for stable resources.
