-- Created:  2026-11-04 by Constructor Tech
--
-- Task Engine DDL — FIRST DRAFT (PostgreSQL form): reference copy of every table and index the
-- gear's migrations will create; the migrations in the gear's code are authoritative and also
-- carry backend-specific tuning (e.g. PostgreSQL autovacuum, DESIGN §3.7). Only indexes that a
-- known access path needs are created; each index comment names that path.
-- Sources: DESIGN.md §3.7–§4.6; ADR-0005, -0006, -0008, -0009, -0010, -0017; the accepted
-- proposals (PR #5730; [Open] items not assumed); openapi.yaml. `P0x` = proposal 0x;
-- `G-XXX-NN` = entry in scenarios/GAPS.md; storage choices made by this draft: G-WIRE-20.
--
-- Dialects (DESIGN §3.7): JSONB → MySQL json, SQLite jsonb_text; BYTEA → LONGBLOB / BLOB
-- (ADR-0006 frame); TIMESTAMPTZ → MySQL TIMESTAMP(6) (session time zone UTC; ends in 2038, so a
-- caller time past 2038-01-19T03:14:07Z is refused 400 on MySQL; needs
-- explicit_defaults_for_timestamp=ON), as in OAGW (G-WIRE-20). Partial indexes state their MySQL form
-- (rule 1 / rule 2). Ring tables: PG and MySQL LIST (ring_slot), SQLite unpartitioned.
-- SKIP LOCKED: PG, MySQL 8.0+, MariaDB 10.6+ (SQLite: guarded updates, P02). MySQL ascii:
-- event_type, event_subject_id, event_subject_type, worker_id, assignee. MySQL expands
-- row-value keysets `(a, b) > ($1, $2)`.
--
-- Domains (P05, ADR-0010): one database per task domain, each with this whole schema;
-- te_runtime_config* and the SDK offset tables are used only in `default`. Task, entry,
-- event and wait IDs are UUID v7; types, queues, entry and resource types are Registry
-- References (G-WIRE-16); subjects are UUID. No FKs between task tables (ADR-0009).
-- vs DESIGN §3.7: removed te_dependency_locks (P01), te_tenant_queue_backlog and
-- backlog_shard (P04), authz_scope (P08); added te_wait_condition_history (P07),
-- te_task_predecessors (P09), te_queue_holds (P06), the depth sample tables (P04).
--
-- ── Enum codes (SMALLINT; only urgency is fixed by DESIGN, G-WIRE-20) ────────
--   task state        1 scheduled 2 queued 3 blocked 4 claimed 5 running 6 waiting 7 completed 8 failed 9 cancelled 10 dead_lettered
--   urgency           0 critical 1 normal
--   result            1 success 2 warning 3 error 4 cancelled 5 timed_out 6 forcibly_cancelled 7 superseded 8 dependency_failed
--   dependency mode   1 all 2 any
--   on_pred_failure   1 cancel 2 ignore
--   dep outcome       0 pending 1 succeeded 2 failed_ignored 3 failed
--   wait kind         1 timer 2 input 3 children 4 event
--   wait state        1 pending 2 satisfied 3 timed_out 4 cancelled
--   wait mode         1 all 2 any
--   on_timeout        1 fail 2 resume
--   timer type        1 queue 5 lifetime 6 sla 7 scheduled 8 wait 9 cancel_grace 10 cascade 11 children_check (2-4 reserved)
--   timer status      1 pending 2 claimed 3 completed
--   cascade kind      1 children 2 successors
--   counter dim       1 queue_global 2 type_global 3 queue_tenant 4 type_tenant
--   wait-counter dim  1 tenant_event_waits 2 tenant_event_waits_per_type 3 tenant_unkeyed_event_waits_per_type
--   dedup window      1 active 2 active_or_succeeded 3 caller_key
--   session status    1 active 2 expired
--   ring status       1 active 2 recycling (no `free`: truncate sets the slot active for B + N, G-OPS-44)
--   depth dimension   1 tenant (tenant + queue) 2 deferred (tenant); the 429 names (G-CLM-30)
--   event type        1 taskCreated 2 taskQueued 3 taskClaimed 4 taskStarted 5 taskProgressed 6 taskCheckpointed 7 taskWaiting 8 waitConditionSet 9 waitConditionSatisfied 10 waitTimedOut 11 taskResumed 12 taskRetrying 13 taskReassigned 14 taskCancelRequested 15 taskCancelled 16 taskCompleted 17 taskFailed 18 taskTimedOut 19 taskDeadLettered 20 slaBreached 21 workerBlacklisted 22 configUpdated 23 queueHeld 24 queueReleased 25 taskTagsUpdated 26 taskUpdated

-- ── GTS type path domain ─────────────────────────────────────────────────────
-- GTS type string; stored only for event-wait `event_type` (matched as a string).
CREATE DOMAIN gts_type_path AS TEXT
    CHECK (
        LENGTH(VALUE) <= 1024
        AND VALUE ~ '^gts\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.v(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*))?(?:~[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.[a-z_][a-z0-9_]*\.v(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*))?)*~$'
);

-- ── Ring tables (ADR-0009) ───────────────────────────────────────────────────
-- History tables are LIST-partitioned into N fixed slots (retention.ring: W = 7 days, N = 17);
-- ring_slot = floor(days_since_epoch(created_at) / W) mod N, from the task's UUID v7 id. Each
-- has an unpartitioned `<table>_retained` twin for rescued tasks (reads: ring UNION ALL retained).
-- Twins hold rescued rows and new rows of retained tasks; each sits next to its ring table.
-- MySQL/SQLite: plain CREATE TABLE. Partitions are created at the end of this file.

-- Ring state: one row per slot, the fence between old-bucket writers (FENCE) and the
-- retention Mark step (DESIGN §3.7). Written by the retention leader. Unrestricted.
CREATE TABLE te_ring_state (
    ring_slot    SMALLINT     NOT NULL CHECK (ring_slot >= 0),
    bucket       INTEGER      NOT NULL,     -- floor(days_since_epoch / bucket_days) the slot holds
    status_code  SMALLINT     NOT NULL CHECK (status_code BETWEEN 1 AND 2),  -- active | recycling
    changed_at   TIMESTAMPTZ  NOT NULL,

    PRIMARY KEY (ring_slot)                 -- fence read / mark / reactivate (by slot)
);

-- ── Immutable task data ──────────────────────────────────────────────────────
-- Written once by create. Ring table. queue_id is the queue at creation (audit); the
-- current queue, urgency and priority live in te_task_state / te_task_results.
CREATE TABLE te_tasks (
    id                 UUID          NOT NULL,                 -- UUID v7
    ring_slot          SMALLINT      NOT NULL CHECK (ring_slot >= 0),
    owner_tenant_id    UUID          NOT NULL,
    type_id            UUID          NOT NULL,
    type_revision      VARCHAR(64)   NOT NULL,                 -- output is validated against it
    queue_id           UUID          NOT NULL,
    owner_id           UUID,                                   -- subject the task is done for
    created_by         UUID          NOT NULL,                 -- server-set
    idempotency_key    VARCHAR(256),                           -- caller key, reference only; NULL for derived dedup
    cancellable        BOOLEAN       NOT NULL DEFAULT TRUE,
    parent_task_id     UUID,                                   -- p2; same tenant and domain
    hierarchy_depth    SMALLINT      NOT NULL DEFAULT 0 CHECK (hierarchy_depth >= 0),  -- parent's + 1; ≤ max_hierarchy_depth (G-WAI-39)
    wait_for_children  BOOLEAN       NOT NULL DEFAULT FALSE,
    propagate_cancel   BOOLEAN       NOT NULL DEFAULT TRUE,
    input              BYTEA         NOT NULL,                 -- compressed JSON (ADR-0006)
    retry_policy_json  JSONB,                                  -- effective policy (G-CRT-04)
    timeouts_json      JSONB,                                  -- effective timeouts (G-CRT-04)
    request_overrides_json JSONB,                              -- timeouts / retry policy as sent; a move re-resolves around them (G-OPS-20)
    sla_deadline       TIMESTAMPTZ,
    authority_json     JSONB         NOT NULL,                 -- {mode, checks[{source,resource_type,action,verdict}], checked_at}
    created_at         TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (id, ring_slot)                                -- task read by id (+ slot_of(id))
) PARTITION BY LIST (ring_slot);

-- Terminal children; id appended (G-WIRE-20). MySQL: rule 1.
CREATE INDEX ix_te_tasks_parent ON te_tasks (parent_task_id, id) WHERE parent_task_id IS NOT NULL;

-- DESIGN's ix_te_tasks_type is not created: no query uses it (G-OPS-46).

-- Retained twin (see Ring tables).
CREATE TABLE te_tasks_retained (LIKE te_tasks INCLUDING DEFAULTS INCLUDING CONSTRAINTS, PRIMARY KEY (id));
CREATE INDEX ix_te_tasks_retained_parent ON te_tasks_retained (parent_task_id, id) WHERE parent_task_id IS NOT NULL;

-- ── Live mutable state (non-terminal tasks only) ─────────────────────────────
-- ≤ ~100K rows (§6.1): inserted by create / re-enqueue, updated by claims and transitions
-- (never by heartbeats, ADR-0017), moved out by finalize(). Authoritative queue / urgency /
-- priority. Lease columns NULL unless claimed or running.
CREATE TABLE te_task_state (
    task_id                  UUID          NOT NULL,           -- = te_tasks.id
    owner_tenant_id          UUID          NOT NULL,           -- denormalized
    queue_id                 UUID          NOT NULL,
    type_id                  UUID          NOT NULL,           -- denormalized for claim
    urgency_code             SMALLINT      NOT NULL DEFAULT 1 CHECK (urgency_code IN (0, 1)),
    priority                 INTEGER       NOT NULL CHECK (priority BETWEEN 0 AND 1000),  -- tenant-local, lower = first
    created_at               TIMESTAMPTZ   NOT NULL,           -- denormalized for claim order
    state_code               SMALLINT      NOT NULL CHECK (state_code BETWEEN 1 AND 6),
    cancel_requested         BOOLEAN       NOT NULL DEFAULT FALSE,
    -- Pending cancel request of a leased task, latest wins (G-WIRE-19).
    cancel_mode_code         SMALLINT      CHECK (cancel_mode_code IN (1, 2)),  -- 1 requested 2 graceful (forced never pends)
    cancel_requested_by      UUID,                             -- requesting subject (or the superseding creator)
    cancel_requested_at      TIMESTAMPTZ,
    cancel_reason            VARCHAR(1024),                    -- G-OPS-47
    attempt                  INTEGER       NOT NULL DEFAULT 0 CHECK (attempt >= 0),            -- claims
    retry_count              INTEGER       NOT NULL DEFAULT 0 CHECK (retry_count >= 0),
    liveness_requeues        INTEGER       NOT NULL DEFAULT 0 CHECK (liveness_requeues >= 0),
    resume_pending           BOOLEAN       NOT NULL DEFAULT FALSE,  -- next claim does not bump attempt
    -- Group the task resumed from, for the next Claim.resumed (G-WIRE-20).
    resume_wait_group_id     UUID,
    revision                 BIGINT        NOT NULL DEFAULT 0, -- payload-write version (§4.6 Payload-Changing Writes)
    payload_bytes            INTEGER       NOT NULL CHECK (payload_bytes >= 0),  -- input+context+output, uncompressed canonical JSON
    context                  BYTEA,                            -- compressed JSON
    output                   BYTEA,                            -- compressed JSON
    result_code              SMALLINT      CHECK (result_code BETWEEN 1 AND 8),  -- set only by finalize(); NULL here
    error_json               BYTEA,                            -- compressed JSON (last reported error)
    warnings_json            BYTEA,                            -- compressed JSON
    assignee_subject         UUID,                             -- lease holder / reassignment target
    assignee                 VARCHAR(128)  CHECK (assignee ~ '^[A-Za-z0-9._:-]{1,128}$'),  -- worker_id
    parent_task_id           UUID,                             -- denormalized: children check on the small table
    claimed_at               TIMESTAMPTZ,
    started_at               TIMESTAMPTZ,                      -- set by start, cleared by claim; feeds te_task_run_duration_seconds, not on the wire (G-WIRE-20)
    session_id               UUID,                             -- te_worker_sessions; NULL when unleased / during hand-off
    handoff_until            TIMESTAMPTZ,                      -- accept window after a reassignment
    lease_token              UUID,                             -- random v4; never logged
    lease_epoch              BIGINT        NOT NULL DEFAULT 0 CHECK (lease_epoch >= 0),  -- +1 per claim / reassignment
    bg_slot                  SMALLINT      NOT NULL CHECK (bg_slot BETWEEN 0 AND 1023),  -- murmur3(task_id) mod 1024
    assignment_deadline      TIMESTAMPTZ,                      -- claimed only
    execution_deadline       TIMESTAMPTZ,                      -- set by the claim; always set while claimed or running (G-EXE-10)
    superseded_by            UUID,                             -- p2 supersession
    run_after                TIMESTAMPTZ,                      -- scheduled only
    dead_lettered_from       UUID,                             -- always NULL on a live row (re-enqueue clears it)
    progress_json            JSONB,                            -- {percent, milestone, completed_units, total_units, current_stage, eta}
    entry_count              INTEGER       NOT NULL DEFAULT 0 CHECK (entry_count >= 0),  -- ≤ max_entries_per_task; entry writes lock this row (G-EXE-21)
    queue_slot_shard         SMALLINT,                         -- recorded te_concurrency_slots shard (queue_global)
    type_slot_shard          SMALLINT,                         -- … type_global
    tenant_queue_slot_shard  SMALLINT,                         -- … te_tenant_concurrency_slots queue_tenant
    tenant_type_slot_shard   SMALLINT,                         -- … type_tenant
    updated_at               TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (task_id),                                     -- every by-id read and guarded update
    CHECK (run_after IS NULL OR state_code = 1),
    CHECK (cancel_requested = (cancel_requested_at IS NOT NULL)),
    CHECK (cancel_requested_at IS NULL OR (cancel_mode_code IS NOT NULL AND cancel_requested_by IS NOT NULL)),
    CHECK (execution_deadline IS NOT NULL OR state_code NOT IN (4, 5)),
    CHECK (assignment_deadline IS NULL OR state_code = 4),
    CHECK (state_code IN (4, 5) OR (lease_token IS NULL AND session_id IS NULL AND handoff_until IS NULL
                                    AND queue_slot_shard IS NULL AND type_slot_shard IS NULL
                                    AND tenant_queue_slot_shard IS NULL AND tenant_type_slot_shard IS NULL))
);

-- Live search by state; deferred depth.
CREATE INDEX ix_te_task_state_tenant_state ON te_task_state (owner_tenant_id, state_code, task_id);
-- Live search, creation order.
CREATE INDEX ix_te_task_state_tenant_id ON te_task_state (owner_tenant_id, task_id);
-- round_robin claim: next tenant, tenant head, refill; per-tenant depth sampling.
CREATE INDEX ix_te_task_state_tenant_dispatch ON te_task_state
    (queue_id, state_code, urgency_code, owner_tenant_id, priority, created_at, task_id);
-- fifo claim, critical probe, queue watcher, per-state counts.
CREATE INDEX ix_te_task_state_queue_dispatch ON te_task_state
    (queue_id, state_code, urgency_code, priority, created_at, task_id);
-- Session requeue and cleanup. MySQL: (session_id), rule 1.
CREATE INDEX ix_te_task_state_session ON te_task_state (session_id) WHERE session_id IS NOT NULL;
-- Deadline scan, hand-off arm. MySQL: same columns, rule 1.
CREATE INDEX ix_te_task_state_handoff ON te_task_state (handoff_until, bg_slot) WHERE handoff_until IS NOT NULL;
-- Deadline scan, assignment arm. MySQL: same columns, rule 1.
CREATE INDEX ix_te_task_state_assignment_deadline ON te_task_state (assignment_deadline, bg_slot) WHERE state_code = 4;
-- Deadline scan, execution arm. MySQL: same columns, rule 1.
CREATE INDEX ix_te_task_state_execution_deadline ON te_task_state (execution_deadline, bg_slot) WHERE state_code = 5;
-- DESIGN's ix_te_task_state_run_after and ix_te_task_state_assignee are not created: no query
-- uses them (G-OPS-46).
-- Live children; task_id appended (G-WIRE-20). MySQL: rule 1.
CREATE INDEX ix_te_task_state_parent ON te_task_state (parent_task_id, task_id) WHERE parent_task_id IS NOT NULL;

-- ── Terminal task state ──────────────────────────────────────────────────────
-- Terminal tasks: inserted by finalize(), deleted by retention or moved back by
-- re-enqueue. Ring table. te_task_state minus live-only columns, plus terminal_at, predecessors.
CREATE TABLE te_task_results (
    task_id             UUID          NOT NULL,
    ring_slot           SMALLINT      NOT NULL CHECK (ring_slot >= 0),
    owner_tenant_id     UUID          NOT NULL,
    queue_id            UUID          NOT NULL,                -- DLQ for dead_lettered
    type_id             UUID          NOT NULL,
    urgency_code        SMALLINT      NOT NULL CHECK (urgency_code IN (0, 1)),
    priority            INTEGER       NOT NULL CHECK (priority BETWEEN 0 AND 1000),
    created_at          TIMESTAMPTZ   NOT NULL,
    state_code          SMALLINT      NOT NULL CHECK (state_code BETWEEN 7 AND 10),
    cancel_requested    BOOLEAN       NOT NULL,
    attempt             INTEGER       NOT NULL,
    retry_count         INTEGER       NOT NULL,
    liveness_requeues   INTEGER       NOT NULL,
    revision            BIGINT        NOT NULL,
    payload_bytes       INTEGER       NOT NULL,
    context             BYTEA,
    output              BYTEA,
    result_code         SMALLINT      NOT NULL CHECK (result_code BETWEEN 1 AND 8),
    error_json          BYTEA,
    warnings_json       BYTEA,
    assignee_subject    UUID,
    assignee            VARCHAR(128),
    parent_task_id      UUID,
    claimed_at          TIMESTAMPTZ,
    started_at          TIMESTAMPTZ,                           -- of the last attempt; internal
    lease_token         UUID,                                  -- worker completion only (replay)
    lease_epoch         BIGINT        NOT NULL,
    superseded_by       UUID,
    dead_lettered_from  UUID,                                  -- source queue when dead_lettered
    progress_json       JSONB,
    entry_count         INTEGER       NOT NULL DEFAULT 0 CHECK (entry_count >= 0),  -- G-EXE-21
    updated_at          TIMESTAMPTZ   NOT NULL,
    terminal_at         TIMESTAMPTZ   NOT NULL,                -- retention clock (ADR-0009)
    predecessors        JSONB,                                 -- [{predecessor_id, mode, on_predecessor_failure, outcome}] from te_dependencies

    PRIMARY KEY (task_id, ring_slot),                          -- find_task_any, completion replay
    -- PRD §5.12 state/result pairing.
    CHECK (   (state_code = 7  AND result_code IN (1, 2))
           OR (state_code IN (8, 10) AND result_code IN (3, 5))
           OR (state_code = 9  AND result_code IN (4, 6, 7, 8))),
    CHECK (dead_lettered_from IS NULL OR state_code = 10)
) PARTITION BY LIST (ring_slot);

-- Terminal search in creation order.
CREATE INDEX ix_te_task_results_tenant_id ON te_task_results (owner_tenant_id, task_id);
-- Terminal search filtered by state in creation order.
CREATE INDEX ix_te_task_results_tenant_state ON te_task_results (owner_tenant_id, state_code, task_id);
-- Search ordered by terminal_at, dead-letter inspection by state.
CREATE INDEX ix_te_task_results_tenant_terminal ON te_task_results (owner_tenant_id, state_code, terminal_at, task_id);
-- Search filtered by type.
CREATE INDEX ix_te_task_results_tenant_type ON te_task_results (owner_tenant_id, type_id, task_id);
-- Search filtered by queue.
CREATE INDEX ix_te_task_results_tenant_queue ON te_task_results (owner_tenant_id, queue_id, task_id);
-- Retention: rescue candidates and shorter-than-horizon expiry per slot.
CREATE INDEX ix_te_task_results_terminal ON te_task_results (terminal_at);
-- Dead-lettered per DLQ: counts and unscoped DLQ listing, window-exempt (G-WIRE-20, G-RTY-10).
-- Tenant-scoped DLQ listing uses ix_te_task_results_tenant_state. MySQL: (state_code, queue_id, task_id), rule 2.
CREATE INDEX ix_te_task_results_dlq ON te_task_results (queue_id, task_id) WHERE state_code = 10;
-- `dead_lettered_from` search filter (G-RTY-11). MySQL: (state_code, owner_tenant_id, dead_lettered_from, task_id), rule 2.
CREATE INDEX ix_te_task_results_tenant_dlq_from ON te_task_results (owner_tenant_id, dead_lettered_from, task_id) WHERE state_code = 10;

-- Retained twin (see Ring tables).
CREATE TABLE te_task_results_retained (LIKE te_task_results INCLUDING DEFAULTS INCLUDING CONSTRAINTS, PRIMARY KEY (task_id));
CREATE INDEX ix_te_task_results_retained_tenant_id ON te_task_results_retained (owner_tenant_id, task_id);
CREATE INDEX ix_te_task_results_retained_terminal ON te_task_results_retained (terminal_at);   -- expiry
CREATE INDEX ix_te_task_results_retained_dlq ON te_task_results_retained (queue_id, task_id) WHERE state_code = 10;
CREATE INDEX ix_te_task_results_retained_tenant_dlq_from ON te_task_results_retained (owner_tenant_id, dead_lettered_from, task_id) WHERE state_code = 10;

-- ── Concurrency counters ─────────────────────────────────────────────────────
-- Exact concurrency limits (P04 "Unchanged"): guarded reserve `used + k <= limit_value`,
-- release on the recorded shard; no row = unlimited. Lock order 1 (ADR-0008). Unrestricted.
CREATE TABLE te_concurrency_slots (
    dimension_code  SMALLINT     NOT NULL CHECK (dimension_code IN (1, 2)),  -- queue_global | type_global
    scope_id        UUID         NOT NULL,                     -- queue id or task type id
    shard           SMALLINT     NOT NULL DEFAULT 0 CHECK (shard >= 0),
    limit_value     INTEGER      NOT NULL CHECK (limit_value >= 0),  -- per-shard sub-limit; 0 = draining
    used            INTEGER      NOT NULL DEFAULT 0 CHECK (used >= 0),
    updated_at      TIMESTAMPTZ  NOT NULL,

    PRIMARY KEY (dimension_code, scope_id, shard)              -- plan read, reserve, release, reconcile
);

-- Per-tenant concurrency counters. Scopable. Lock order 2.
CREATE TABLE te_tenant_concurrency_slots (
    owner_tenant_id  UUID         NOT NULL,
    dimension_code   SMALLINT     NOT NULL CHECK (dimension_code IN (3, 4)),  -- queue_tenant | type_tenant
    scope_id         UUID         NOT NULL,                    -- queue id or task type id
    shard            SMALLINT     NOT NULL DEFAULT 0 CHECK (shard >= 0),
    limit_value      INTEGER      NOT NULL CHECK (limit_value >= 0),
    used             INTEGER      NOT NULL DEFAULT 0 CHECK (used >= 0),
    updated_at       TIMESTAMPTZ  NOT NULL,

    PRIMARY KEY (owner_tenant_id, dimension_code, scope_id, shard)
);

-- ── Event-wait counters ──────────────────────────────────────────────────────
-- Pending event conditions per tenant (PRD §5.4): +1 at set/arm/create, −1 when one
-- leaves pending; created on first use. Scopable. Lock order 3.
CREATE TABLE te_tenant_wait_counters (
    owner_tenant_id  UUID         NOT NULL,
    dimension_code   SMALLINT     NOT NULL CHECK (dimension_code BETWEEN 1 AND 3),
    scope_id         UUID         NOT NULL,                    -- event type Registry Reference; nil UUID for dimension 1
    limit_value      INTEGER      NOT NULL CHECK (limit_value >= 0),
    used             INTEGER      NOT NULL DEFAULT 0 CHECK (used >= 0),
    updated_at       TIMESTAMPTZ  NOT NULL,

    PRIMARY KEY (owner_tenant_id, dimension_code, scope_id)
);

-- ── Sampled queue depth (P04, DESIGN Hot-row rule 2) ─────────────────────────
-- Written by the maintenance leader every queue_depth.sample_secs; every node polls them into
-- its admission snapshot (G-WIRE-20). Queue-wide counts per live state + DLQ counts. Unrestricted.
CREATE TABLE te_queue_depth_samples (
    queue_id    UUID         NOT NULL,
    state_code  SMALLINT     NOT NULL CHECK (state_code IN (1, 2, 3, 4, 5, 6, 10)),
    task_count  BIGINT       NOT NULL CHECK (task_count >= 0),
    sampled_at  TIMESTAMPTZ  NOT NULL,

    PRIMARY KEY (queue_id, state_code)                         -- upsert per sample; read per queue
);

-- Per-tenant refusal list: only keys whose sampled count is ≥ the limit. Scopable.
CREATE TABLE te_tenant_depth_samples (
    owner_tenant_id  UUID         NOT NULL,
    dimension_code   SMALLINT     NOT NULL CHECK (dimension_code IN (1, 2)),  -- tenant_queued | tenant_deferred
    queue_id         UUID         NOT NULL,                    -- nil UUID for tenant_deferred (whole domain)
    task_count       BIGINT       NOT NULL CHECK (task_count >= 0),
    limit_value      BIGINT       NOT NULL CHECK (limit_value > 0),  -- the limit the count was compared with
    sampled_at       TIMESTAMPTZ  NOT NULL,

    PRIMARY KEY (owner_tenant_id, dimension_code, queue_id)
);
-- Drop rows the latest sample did not refresh.
CREATE INDEX ix_te_tenant_depth_samples_sampled ON te_tenant_depth_samples (sampled_at);

-- ── Creation identities ──────────────────────────────────────────────────────
-- Occupied creation identities (derived dedup and caller keys); unpartitioned, so the PK is
-- global. Inserted first by create; finalize() deletes it or sets expires_at by window_code.
-- P05: during a move create also reads the source domain. Scopable. G-WIRE-20 (window_code).
CREATE TABLE te_dedup (
    owner_tenant_id  UUID         NOT NULL,
    dedup_key        BYTEA        NOT NULL CHECK (octet_length(dedup_key) = 32),  -- MySQL BINARY(32)
    task_id          UUID         NOT NULL,                    -- holder
    request_hash     BYTEA        CHECK (request_hash IS NULL OR octet_length(request_hash) = 32),  -- caller keys only; type, input, context (not queue)
    queue_id         UUID,                                     -- caller keys only: queue resolved at create; a retry without queue or with this one matches (G-CRT-11)
    window_code      SMALLINT     NOT NULL CHECK (window_code BETWEEN 1 AND 3),
    created_at       TIMESTAMPTZ  NOT NULL,                    -- holder's creation time
    expires_at       TIMESTAMPTZ,                              -- NULL while the holder is unfinished

    PRIMARY KEY (owner_tenant_id, dedup_key),                  -- insert-or-conflict, replay read
    CHECK ((window_code = 3) = (request_hash IS NOT NULL)),
    CHECK ((window_code = 3) = (queue_id IS NOT NULL))
);
-- A conflicting row with expires_at < now is deleted (guarded) and the insert retried once in the
-- same transaction (G-CRT-12); finalize() sets expires_at = min(created_at + window, terminal_at +
-- retention) (G-CRT-13).
-- Lifetime update in finalize() and re-enqueue, by holder.
CREATE INDEX ix_te_dedup_task ON te_dedup (task_id);
-- Cleanup. MySQL: (expires_at), rule 1.
CREATE INDEX ix_te_dedup_expires ON te_dedup (expires_at) WHERE expires_at IS NOT NULL;

-- Latest order per supersede group (p2, PRD §5.1). Removed by retention. Scopable.
CREATE TABLE te_supersede_heads (
    owner_tenant_id    UUID         NOT NULL,
    group_key          BYTEA        NOT NULL CHECK (octet_length(group_key) = 32),  -- identity hash without the order value
    latest_order       BIGINT       NOT NULL CHECK (latest_order >= 0),
    latest_task_id     UUID         NOT NULL,
    latest_created_at  TIMESTAMPTZ  NOT NULL,

    PRIMARY KEY (owner_tenant_id, group_key)
);
-- Retention cleanup.
CREATE INDEX ix_te_supersede_heads_created ON te_supersede_heads (latest_created_at);

-- ── Task entries ─────────────────────────────────────────────────────────────
-- Immutable entries from the worker (lease-fenced) or an API consumer. Ring table. Scopable.
CREATE TABLE te_task_entries (
    id               UUID          NOT NULL,                   -- UUID v7
    ring_slot        SMALLINT      NOT NULL CHECK (ring_slot >= 0),  -- from task_id
    task_id          UUID          NOT NULL,
    owner_tenant_id  UUID          NOT NULL,
    type_id          UUID          NOT NULL,                   -- entry type (Registry Reference)
    creator          VARCHAR(256)  NOT NULL,                   -- subject id (worker or user)
    payload          BYTEA         NOT NULL,                   -- compressed JSON, ≤ 64 KiB uncompressed
    created_at       TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (id, ring_slot)                                -- GET /tasks/{id}/entries/{entry_id}
) PARTITION BY LIST (ring_slot);
-- Entries page.
CREATE INDEX ix_te_entries_task ON te_task_entries (task_id, created_at, id);

-- Retained twin (see Ring tables).
CREATE TABLE te_task_entries_retained (LIKE te_task_entries INCLUDING DEFAULTS INCLUDING CONSTRAINTS, PRIMARY KEY (id));
CREATE INDEX ix_te_entries_retained_task ON te_task_entries_retained (task_id, created_at, id);

-- ── Wait conditions (live) ───────────────────────────────────────────────────
-- Wait conditions of live tasks: the current group plus earlier, replaced groups (P07);
-- finalize() moves them to te_wait_condition_history (G-WIRE-20). One current group per task
-- (the latest wait_group_id, G-WAI-14); mode_code is the group's mode. Scopable.
CREATE TABLE te_wait_conditions (
    id                      UUID          NOT NULL,            -- UUID v7
    task_id                 UUID          NOT NULL,
    owner_tenant_id         UUID          NOT NULL,
    -- openapi WaitGroup.id (G-WIRE-19)
    wait_group_id           UUID          NOT NULL,
    kind_code               SMALLINT      NOT NULL CHECK (kind_code BETWEEN 1 AND 4),
    state_code              SMALLINT      NOT NULL CHECK (state_code BETWEEN 1 AND 4),
    mode_code               SMALLINT      NOT NULL CHECK (mode_code IN (1, 2)),
    on_timeout_code         SMALLINT      CHECK (on_timeout_code IN (1, 2)),  -- NULL = no timeout (G-WAI-32)
    -- duration kept while armed (G-WIRE-20)
    timeout_secs            INTEGER       CHECK (timeout_secs > 0),
    timeout_at              TIMESTAMPTZ,                        -- parked_at + timeout_secs; NULL while armed
    parked_at               TIMESTAMPTZ,                        -- NULL = armed
    timer_resume_at         TIMESTAMPTZ,                        -- timer kind
    event_type              gts_type_path,                      -- event kind (GTS string; MySQL VARCHAR(512) ascii)
    event_subject_id        VARCHAR(512),                       -- event kind, optional (MySQL ascii)
    event_subject_type      VARCHAR(512),                       -- event kind, effective subject type (MySQL ascii)
    event_consume_filter    JSONB,                              -- decidable consume constraints from set time
    event_match_expression  TEXT,                               -- CEL (p3)
    input_schema            JSONB,                              -- input kind (p2); untrusted, size-checked
    input_prompt            TEXT,                               -- input kind
    resolved_payload        BYTEA,                              -- compressed: input value | event header | child counts | timeout note
    resolved_by             UUID,                               -- P07: resolving subject (input kind only)
    resolved_at             TIMESTAMPTZ,
    eval_failures           SMALLINT      NOT NULL DEFAULT 0 CHECK (eval_failures >= 0),
    created_at              TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (id),                                          -- resolve by condition id
    CHECK (kind_code <> 1 OR timer_resume_at IS NOT NULL),
    CHECK (kind_code <> 2 OR input_schema IS NOT NULL),
    CHECK (kind_code <> 4 OR (event_type IS NOT NULL AND event_subject_type IS NOT NULL)),
    CHECK (kind_code = 4 OR (event_type IS NULL AND event_subject_id IS NULL AND event_subject_type IS NULL
                             AND event_consume_filter IS NULL AND event_match_expression IS NULL)),
    CHECK (resolved_by IS NULL OR kind_code = 2),
    CHECK ((state_code = 1) = (resolved_at IS NULL)),
    CHECK ((timeout_secs IS NULL) = (on_timeout_code IS NULL)),
    CHECK (timeout_at IS NULL OR parked_at IS NOT NULL)
);

-- Per task: active group, resolution, page, finalize move.
CREATE INDEX ix_te_waits_task ON te_wait_conditions (task_id, created_at, id);
-- Event consumer probe. MySQL: (state_code, event_type, owner_tenant_id, event_subject_id), rule 2.
CREATE INDEX ix_te_waits_event_subject ON te_wait_conditions (event_type, owner_tenant_id, event_subject_id)
    WHERE kind_code = 4 AND state_code = 1;
-- Safety sweep (every ~5 min) that re-arms a lost `wait` timer, timeout arm (G-OPS-46). MySQL: (state_code, timeout_at), rule 2.
CREATE INDEX ix_te_waits_timeout ON te_wait_conditions (timeout_at) WHERE state_code = 1;
-- Same sweep, timer-condition arm (G-OPS-46). MySQL: (kind_code, state_code, timer_resume_at), rule 2.
CREATE INDEX ix_te_waits_resume ON te_wait_conditions (timer_resume_at) WHERE kind_code = 1 AND state_code = 1;
-- Children sweep (G-WIRE-20). MySQL: (kind_code, state_code, parked_at), rule 2.
CREATE INDEX ix_te_waits_children_pending ON te_wait_conditions (parked_at) WHERE kind_code = 3 AND state_code = 1;

-- ── Wait conditions (terminal tasks) ─────────────────────────────────────────
-- P07: wait conditions of terminal tasks, moved here by finalize(); immutable. Ring table.
CREATE TABLE te_wait_condition_history (
    LIKE te_wait_conditions INCLUDING DEFAULTS INCLUDING CONSTRAINTS,
    ring_slot  SMALLINT  NOT NULL CHECK (ring_slot >= 0),     -- from task_id

    -- resolve replay by id probes every partition (G-WIRE-20)
    PRIMARY KEY (id, ring_slot)
) PARTITION BY LIST (ring_slot);
CREATE INDEX ix_te_wait_history_task ON te_wait_condition_history (task_id, created_at, id);

-- Retained twin (see Ring tables).
CREATE TABLE te_wait_condition_history_retained (LIKE te_wait_condition_history INCLUDING DEFAULTS INCLUDING CONSTRAINTS, PRIMARY KEY (id));
CREATE INDEX ix_te_wait_history_retained_task ON te_wait_condition_history_retained (task_id, created_at, id);

-- ── Timers ───────────────────────────────────────────────────────────────────
-- Durable deadlines of live tasks for the timer wheel (lease deadlines are columns, §3.2).
-- Deleted by finalize() except `cascade`. One `wait` timer per task at the earliest deadline of its
-- pending parked conditions (timeouts and timer_resume_at), re-armed after each fire (G-WAI-09). Scopable.
CREATE TABLE te_timers (
    id                     UUID          NOT NULL,
    task_id                UUID          NOT NULL,
    owner_tenant_id        UUID          NOT NULL,
    timer_type_code        SMALLINT      NOT NULL CHECK (timer_type_code IN (1, 5, 6, 7, 8, 9, 10, 11)),
    bg_slot                SMALLINT      NOT NULL CHECK (bg_slot BETWEEN 0 AND 1023),  -- = te_task_state.bg_slot
    fires_at               TIMESTAMPTZ   NOT NULL,
    expected_state_code    SMALLINT      CHECK (expected_state_code BETWEEN 1 AND 6),  -- stale detection; NULL for lifetime/sla/cascade/children_check
    status_code            SMALLINT      NOT NULL DEFAULT 1 CHECK (status_code BETWEEN 1 AND 3),
    claimed_by             VARCHAR(128),                       -- claim token of the processing instance
    claim_until            TIMESTAMPTZ,                        -- processing lease
    cascade_kind_code      SMALLINT      CHECK (cascade_kind_code IN (1, 2)),
    cascade_after_task_id  UUID,                               -- cascade resume cursor
    created_at             TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (id),
    CHECK ((timer_type_code = 10) = (cascade_kind_code IS NOT NULL)),
    CHECK ((status_code = 2) = (claimed_by IS NOT NULL AND claim_until IS NOT NULL))
);

-- Due-timer claim scan with the shard range inside the index.
CREATE INDEX ix_te_timers_pending_fires ON te_timers (status_code, fires_at, bg_slot);
-- Abandoned claims. MySQL: (status_code, claim_until), rule 2.
CREATE INDEX ix_te_timers_claim_until ON te_timers (claim_until) WHERE status_code = 2;
-- Per-task timer cleanup / replacement (every transition that sets or drops a timer).
CREATE INDEX ix_te_timers_task ON te_timers (task_id, timer_type_code);

-- ── Task event history ───────────────────────────────────────────────────────
-- Immutable task history, written in every state-change transaction (PRD §5.10); `id` is
-- reused on the published event; task_id NULL for engine/queue events. Ring table. Scopable.
CREATE TABLE te_events (
    id               UUID          NOT NULL,                   -- UUID v7, stable event id
    ring_slot        SMALLINT      NOT NULL CHECK (ring_slot >= 0),
    task_id          UUID,                                     -- NULL for engine/queue-scoped events
    owner_tenant_id  UUID          NOT NULL,                   -- engine-scoped events: the actor's tenant
    event_type_code  SMALLINT      NOT NULL CHECK (event_type_code BETWEEN 1 AND 26),
    -- NULL for engine-caused events (G-WIRE-20)
    actor_identity   VARCHAR(256),
    payload          BYTEA         NOT NULL,                   -- compressed JSON = the broker event's `data`, plus history-only fields (taskRetrying's full error, G-RTY-03)
    -- wire `timestamp` (G-WIRE-20)
    occurred_at      TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (id, ring_slot)
) PARTITION BY LIST (ring_slot);
-- Task history. MySQL: (task_id, id), rule 1.
CREATE INDEX ix_te_events_task ON te_events (task_id, id) WHERE task_id IS NOT NULL;

-- Retained twin (see Ring tables).
CREATE TABLE te_events_retained (LIKE te_events INCLUDING DEFAULTS INCLUDING CONSTRAINTS, PRIMARY KEY (id));
CREATE INDEX ix_te_events_retained_task ON te_events_retained (task_id, id);

-- ── Event inbox ──────────────────────────────────────────────────────────────
-- Inbound broker events already applied in this domain (§3.2), pruned after inbox_retention_days.
CREATE TABLE te_event_inbox (
    event_id         UUID         NOT NULL,                    -- broker event id
    owner_tenant_id  UUID         NOT NULL,
    received_at      TIMESTAMPTZ  NOT NULL,

    PRIMARY KEY (event_id)
);
CREATE INDEX ix_te_event_inbox_received ON te_event_inbox (received_at);   -- pruning

-- Not defined here: the Event Broker SDK producer and offset tables and the toolkit-db
-- outbox (queue `task-engine-events-<domain>`); the engine only calls producer.enqueue.

-- ── Dependencies (live) ──────────────────────────────────────────────────────
-- Edges while the successor is live: written only by create (P01), outcome_code set by the
-- predecessor's cascade, copied to te_task_results.predecessors by finalize(). Scopable.
CREATE TABLE te_dependencies (
    task_id                      UUID      NOT NULL,           -- successor
    predecessor_id               UUID      NOT NULL,           -- may already be terminal
    owner_tenant_id              UUID      NOT NULL,
    mode_code                    SMALLINT  NOT NULL CHECK (mode_code IN (1, 2)),  -- task-level, same on every row of a task
    on_predecessor_failure_code  SMALLINT  NOT NULL DEFAULT 1 CHECK (on_predecessor_failure_code IN (1, 2)),  -- P01
    -- replaces DESIGN `satisfied`: satisfied = outcome_code IN (1, 2) (G-WAI-37)
    outcome_code                 SMALLINT  NOT NULL DEFAULT 0 CHECK (outcome_code BETWEEN 0 AND 3),

    PRIMARY KEY (task_id, predecessor_id),                     -- successor's set (unblock check, finalize copy)
    CHECK (outcome_code <> 2 OR on_predecessor_failure_code = 2)
);
-- Successor cascade, live `predecessor_id` search; task_id appended (G-WIRE-20).
CREATE INDEX ix_te_deps_predecessor ON te_dependencies (owner_tenant_id, predecessor_id, task_id);

-- ── Predecessor edges (immutable, for search) ────────────────────────────────
-- Immutable edges for the `predecessor_id` search over terminal tasks (P09; G-WIRE-20).
-- Written by create. Ring table. Scopable.
CREATE TABLE te_task_predecessors (
    task_id                      UUID      NOT NULL,           -- successor
    ring_slot                    SMALLINT  NOT NULL CHECK (ring_slot >= 0),
    predecessor_id               UUID      NOT NULL,
    owner_tenant_id              UUID      NOT NULL,
    mode_code                    SMALLINT  NOT NULL CHECK (mode_code IN (1, 2)),
    on_predecessor_failure_code  SMALLINT  NOT NULL CHECK (on_predecessor_failure_code IN (1, 2)),

    PRIMARY KEY (task_id, predecessor_id, ring_slot)
) PARTITION BY LIST (ring_slot);
-- `predecessor_id` search filter in successor-creation order.
CREATE INDEX ix_te_task_predecessors_pred ON te_task_predecessors (owner_tenant_id, predecessor_id, task_id);

-- Retained twin (see Ring tables).
CREATE TABLE te_task_predecessors_retained (LIKE te_task_predecessors INCLUDING DEFAULTS INCLUDING CONSTRAINTS,
                                            PRIMARY KEY (task_id, predecessor_id));
CREATE INDEX ix_te_task_predecessors_retained_pred ON te_task_predecessors_retained (owner_tenant_id, predecessor_id, task_id);

-- ── Tags ─────────────────────────────────────────────────────────────────────
-- Labels set at create and by PATCH /tasks/{id}/tags (P01); count limit G-CRT-06.
-- Ring table. Scopable. G-WIRE-20 (key / value renamed).
CREATE TABLE te_tags (
    task_id          UUID          NOT NULL,
    ring_slot        SMALLINT      NOT NULL CHECK (ring_slot >= 0),
    tag_key          VARCHAR(128)  NOT NULL CHECK (char_length(tag_key) >= 1),
    tag_value        VARCHAR(256)  NOT NULL,
    owner_tenant_id  UUID          NOT NULL,

    PRIMARY KEY (task_id, tag_key, ring_slot)                  -- task's tags; patch upsert/delete
) PARTITION BY LIST (ring_slot);
-- Tenant-scoped tag filter, matches in creation order.
CREATE INDEX ix_te_tags_key_value ON te_tags (owner_tenant_id, tag_key, tag_value, task_id);

-- Retained twin (see Ring tables).
CREATE TABLE te_tags_retained (LIKE te_tags INCLUDING DEFAULTS INCLUDING CONSTRAINTS, PRIMARY KEY (task_id, tag_key));
CREATE INDEX ix_te_tags_retained_key_value ON te_tags_retained (owner_tenant_id, tag_key, tag_value, task_id);

-- ── Resource references ──────────────────────────────────────────────────────
-- Platform resources the task works on (p2), set at create, ≤ 16; `verified` = confirmed
-- by a creator-authority check (§3.3). Ring table. Scopable.
CREATE TABLE te_resource_refs (
    task_id           UUID          NOT NULL,
    ring_slot         SMALLINT      NOT NULL CHECK (ring_slot >= 0),
    resource_type_id  UUID          NOT NULL,                  -- Registry Reference of the resource's GTS type
    resource_id       VARCHAR(256)  NOT NULL,                  -- G-CRT-10
    verified          BOOLEAN       NOT NULL DEFAULT FALSE,
    owner_tenant_id   UUID          NOT NULL,

    PRIMARY KEY (task_id, resource_type_id, resource_id, ring_slot)
) PARTITION BY LIST (ring_slot);
-- "All tasks touching this resource", tenant-scoped, creation order.
CREATE INDEX ix_te_resource_refs_lookup ON te_resource_refs (owner_tenant_id, resource_type_id, resource_id, task_id);

-- Retained twin (see Ring tables).
CREATE TABLE te_resource_refs_retained (LIKE te_resource_refs INCLUDING DEFAULTS INCLUDING CONSTRAINTS,
                                        PRIMARY KEY (task_id, resource_type_id, resource_id));
CREATE INDEX ix_te_resource_refs_retained_lookup ON te_resource_refs_retained (owner_tenant_id, resource_type_id, resource_id, task_id);

-- ── Worker sessions ──────────────────────────────────────────────────────────
-- One row per worker process and domain (ADR-0017); heartbeats write only this row.
-- Expired by the session scan, deleted once empty. Unrestricted; guarded by worker_subject.
CREATE TABLE te_worker_sessions (
    session_id      UUID          NOT NULL,                    -- random v4, not a credential
    worker_subject  UUID          NOT NULL,
    worker_id       VARCHAR(128)  NOT NULL CHECK (worker_id ~ '^[A-Za-z0-9._:-]{1,128}$'),  -- MySQL ascii
    lease_secs      INTEGER       NOT NULL CHECK (lease_secs >= 1),  -- shortest lease of any task held; never raised
    lease_until     TIMESTAMPTZ   NOT NULL,
    status_code     SMALLINT      NOT NULL DEFAULT 1 CHECK (status_code IN (1, 2)),
    bg_slot         SMALLINT      NOT NULL CHECK (bg_slot BETWEEN 0 AND 1023),  -- murmur3(session_id) mod 1024
    created_at      TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (session_id)                                   -- heartbeat / claim / accept renew
);
-- Session scan; expired-session cleanup.
CREATE INDEX ix_te_sessions_expiry ON te_worker_sessions (status_code, lease_until, bg_slot);
-- DESIGN's ix_te_sessions_worker is not created: no query uses it (G-OPS-46).

-- ── Worker blacklist ─────────────────────────────────────────────────────────
-- Consecutive worker-attributed failures per (worker, queue) (PRD §5.6). Unrestricted.
CREATE TABLE te_worker_blacklist (
    worker_subject            UUID          NOT NULL,
    worker_id                 VARCHAR(128)  NOT NULL,          -- MySQL ascii
    queue_id                  UUID          NOT NULL,
    consecutive_failures      INTEGER       NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),  -- reset to 0 when a blacklisting starts (G-OPS-31)
    last_liveness_failure_at  TIMESTAMPTZ,                     -- coalescing anchor
    blacklisted_until         TIMESTAMPTZ,
    created_at                TIMESTAMPTZ   NOT NULL,
    updated_at                TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (worker_subject, worker_id, queue_id)          -- record / reset failure
);
-- Node reload, cleanup (G-WIRE-20). MySQL: rule 1.
CREATE INDEX ix_te_worker_blacklist_until ON te_worker_blacklist (blacklisted_until) WHERE blacklisted_until IS NOT NULL;

-- ── Queue holds (P06) ────────────────────────────────────────────────────────
-- Admin hold (P06): claims skip the queue, everything else goes on. One row per held queue
-- in each domain of its set; nodes cache it and refresh every runtime_config_poll_secs (G-OPS-03).
-- Queue timers keep running while held (G-OPS-07). Unrestricted.
CREATE TABLE te_queue_holds (
    queue_id    UUID          NOT NULL,
    held_since  TIMESTAMPTZ   NOT NULL,                        -- QueueHold.since
    held_by     UUID          NOT NULL,                        -- QueueHold.by: the latest actor to hold (G-OPS-09)
    reason      VARCHAR(1024),                                 -- G-OPS-47
    hold_until  TIMESTAMPTZ,                                   -- nodes stop skipping at this time; the leader sweep deletes the row (G-OPS-08)

    PRIMARY KEY (queue_id)
);
-- Automatic release. MySQL: rule 1.
CREATE INDEX ix_te_queue_holds_until ON te_queue_holds (hold_until) WHERE hold_until IS NOT NULL;

-- ── Runtime configuration ────────────────────────────────────────────────────
-- Saved runtime config versions, `default` domain only (§3.8); each save merges into the
-- previous one. Columns: G-OPS-11. Unrestricted.
CREATE TABLE te_runtime_config (
    config_version  BIGINT        NOT NULL CHECK (config_version >= 1),
    overrides       JSONB         NOT NULL,                    -- merged {queues: {...}, types: {...}} after this save
    saved_by        UUID          NOT NULL,
    saved_at        TIMESTAMPTZ   NOT NULL,

    PRIMARY KEY (config_version)                               -- latest = max(config_version)
);

-- Version this domain has applied (cumulative). Singleton (G-OPS-11).
CREATE TABLE te_runtime_config_applied (
    singleton        BOOLEAN       NOT NULL DEFAULT TRUE CHECK (singleton),
    applied_version  BIGINT        NOT NULL DEFAULT 0 CHECK (applied_version >= 0),
    applied_at       TIMESTAMPTZ,

    PRIMARY KEY (singleton)
);

-- ── Ring partitions (generated) ──────────────────────────────────────────────
-- N = 17 partitions per ring table, created once (#5481). MySQL: inline PARTITION
-- clauses; SQLite: no partitions, ring_slot indexed.
DO $$
DECLARE
    t text;
    s int;
BEGIN
    FOREACH t IN ARRAY ARRAY['te_tasks', 'te_task_results', 'te_task_entries', 'te_events',
                             'te_tags', 'te_resource_refs', 'te_wait_condition_history',
                             'te_task_predecessors'] LOOP
        FOR s IN 0..16 LOOP
            EXECUTE format('CREATE TABLE %I PARTITION OF %I FOR VALUES IN (%s)',
                           t || '_p' || lpad(s::text, 2, '0'), t, s);
        END LOOP;
    END LOOP;
END $$;

-- Seed: slot s holds bucket b ≡ s mod N, c0 − N < b ≤ c0 (G-WIRE-20); applied-config row.
INSERT INTO te_ring_state (ring_slot, bucket, status_code, changed_at)
SELECT s,
       (floor(extract(epoch FROM now()) / 86400 / 7))::int
         - (((floor(extract(epoch FROM now()) / 86400 / 7))::int - s) % 17 + 17) % 17,
       1, now()
FROM generate_series(0, 16) AS s;
INSERT INTO te_runtime_config_applied (singleton) VALUES (TRUE);
