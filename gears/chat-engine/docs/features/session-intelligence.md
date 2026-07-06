Created:  2026-03-20 by Constructor Tech
Updated:  2026-03-20 by Constructor Tech
# Feature: Session Intelligence


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Generate Session Summary](#generate-session-summary)
  - [Get Retention Policy](#get-retention-policy)
  - [Update Retention Policy](#update-retention-policy)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Validate Summarization Support](#validate-summarization-support)
  - [Invoke Summary via Backend Plugin](#invoke-summary-via-backend-plugin)
  - [Evaluate Retention Policy](#evaluate-retention-policy)
  - [Enforce Retention Cleanup](#enforce-retention-cleanup)
- [4. States (CDSL)](#4-states-cdsl)
  - [None](#none)
- [5. Definitions of Done](#5-definitions-of-done)
  - [On-Demand Session Summary](#on-demand-session-summary)
  - [Retention Policy Configuration](#retention-policy-configuration)
  - [Retention Policy Enforcement](#retention-policy-enforcement)
  - [Cascade Deletion During Retention](#cascade-deletion-during-retention)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-featstatus-session-intelligence`

## 1. Feature Context

- [ ] `p2` - `cpt-cf-chat-engine-feature-session-intelligence`

### 1.1 Overview

Session-level intelligence and data lifecycle management: on-demand AI-generated session summaries routed through the backend plugin, and configurable per-session-type retention policies for automatic message cleanup with cascade deletion of message subtrees.

### 1.2 Purpose

Enable clients to request AI-generated session summaries and enable developers to configure retention policies that automatically clean up old messages. Summary generation delegates all AI logic to the backend plugin (respecting backend authority). Retention enforcement keeps storage bounded and supports data minimization obligations.

Success criteria: Summary streaming first-byte under 500ms; retention cleanup processes within the configured SLA window without impacting concurrent request latency.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-chat-engine-actor-client` | Requests session summaries; reads retention policy |
| `cpt-cf-chat-engine-actor-developer` | Configures retention policies on session types |
| `cpt-cf-chat-engine-actor-backend-plugin` | Processes SessionSummaryEvent; returns streamed summary text |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADR**: [ADR-0021: Session Deletion Strategy](../ADR/0021-session-deletion-strategy.md), [ADR-0023: LLM Gateway Plugin](../ADR/0023-llm-gateway-plugin.md)
- **Dependencies**: `cpt-cf-chat-engine-feature-message-processing`

**Traces to**:
- `cpt-cf-chat-engine-fr-session-summary` — on-demand AI-generated session summaries
- `cpt-cf-chat-engine-fr-retention-policy` — retention policy configuration and enforcement
- `cpt-cf-chat-engine-fr-conversation-memory` — conversation memory management via summarization
- `cpt-cf-chat-engine-fr-context-overflow` — summarization as context overflow strategy
- `cpt-cf-chat-engine-nfr-streaming` — summary streaming first-byte performance
- `cpt-cf-chat-engine-nfr-retention-sla` — retention cleanup within SLA window
- `cpt-cf-chat-engine-nfr-lifecycle-performance` — retention cleanup without impacting concurrent requests

## 2. Actor Flows (CDSL)

### Generate Session Summary

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-flow-session-intelligence-generate-summary`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client requests summary; backend plugin streams AI-generated summary text; summary is returned to client via NDJSON streaming

**Error Scenarios**:
- Session not found or not owned by caller (403/404)
- Session is not in active or archived lifecycle state (409)
- Summarization not supported for this session type (422)
- Backend plugin unavailable or fails during summary generation (502)

**Steps**:
1. [ ] - `p2` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-gs-auth`
2. [ ] - `p2` - API: invoke generate-summary endpoint (see `cpt-cf-chat-engine-seq-generate-summary`) - `inst-gs-api`
3. [ ] - `p2` - Algorithm: validate session ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-gs-ownership`
4. [ ] - `p2` - **IF** session.lifecycle_state IN (soft_deleted, hard_deleted) **RETURN** 409 Conflict - `inst-gs-check-state`
5. [ ] - `p2` - Algorithm: validate summarization support using `cpt-cf-chat-engine-algo-session-intelligence-validate-summarization` - `inst-gs-validate-support`
6. [ ] - `p2` - DB: load visible message history for session (excluding hidden-from-backend), ordered chronologically - `inst-gs-load-history`
7. [ ] - `p2` - Algorithm: invoke summary via backend plugin using `cpt-cf-chat-engine-algo-session-intelligence-invoke-summary` - `inst-gs-invoke`
8. [ ] - `p2` - Stream: emit StreamingStartEvent to client - `inst-gs-stream-start`
9. [ ] - `p2` - **FOR EACH** chunk received from plugin stream - `inst-gs-stream-chunks`
   1. [ ] - `p2` - Stream: emit StreamingChunkEvent(chunk) to client - `inst-gs-stream-chunk`
10. [ ] - `p2` - Stream: emit StreamingCompleteEvent(metadata) to client - `inst-gs-stream-complete`
11. [ ] - `p2` - **RETURN** stream closed - `inst-gs-return`

### Get Retention Policy

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-flow-session-intelligence-get-retention`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client retrieves the effective retention policy for a session (inherited from session type or overridden at session level)

**Error Scenarios**:
- Session not found or not owned by caller (403/404)

**Steps**:
1. [ ] - `p2` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-gr-auth`
2. [ ] - `p2` - API: invoke get-retention-policy endpoint (defined in DESIGN) - `inst-gr-api`
3. [ ] - `p2` - Algorithm: validate session ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-gr-ownership`
4. [ ] - `p2` - DB: load retention_policy from session record - `inst-gr-load-session`
5. [ ] - `p2` - **IF** session.retention_policy is null: DB: load retention_policy from session type as fallback - `inst-gr-fallback-type`
6. [ ] - `p2` - **RETURN** 200 — `retention_policy` is internally tagged on `"type"` and matches the SDK `RetentionPolicy` enum (`chat-engine-sdk::models::RetentionPolicy`). Wire shapes: `{"type": "none"}`, `{"type": "age_based", "max_age_days": N}`, `{"type": "count_based", "max_message_count": N}` — type-specific fields are flattened at the top level, no nested wrapper. Note: `soft_delete_retention_days` (the grace period before hard-delete after a soft-delete) is **not** part of `RetentionPolicy`; it is configured at session-type level and surfaced through the session-deletion flow described in ADR-0021. - `inst-gr-return`

### Update Retention Policy

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-flow-session-intelligence-update-retention`

**Actor**: `cpt-cf-chat-engine-actor-developer`

**Success Scenarios**:
- Developer updates the retention policy on an individual session; future cleanup runs use the new per-session policy, overriding the session type default

**Error Scenarios**:
- Session not found or not owned by caller (403/404)
- Invalid retention policy configuration (400)

**Steps**:
1. [ ] - `p2` - Algorithm: authenticate developer request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-ur-auth`
2. [ ] - `p2` - API: invoke update-retention-policy endpoint (defined in DESIGN) - `inst-ur-api`
3. [ ] - `p2` - Algorithm: validate session ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-ur-ownership`
4. [ ] - `p2` - **IF** retention_policy.type not IN (age_based, count_based, none) **RETURN** 400 Bad Request - `inst-ur-validate-type`
5. [ ] - `p2` - **IF** type == age_based AND max_age_days < 1 **RETURN** 400 Bad Request - `inst-ur-validate-age`
6. [ ] - `p2` - **IF** type == count_based AND max_message_count < 1 **RETURN** 400 Bad Request - `inst-ur-validate-count`
7. [ ] - `p2` - DB: update session record with new retention_policy and updated_at timestamp - `inst-ur-db`
8. [ ] - `p2` - **RETURN** 200 (updated retention_policy) - `inst-ur-return`

## 3. Processes / Business Logic (CDSL)

### Validate Summarization Support

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-session-intelligence-validate-summarization`

**Input**: session_type_id
**Output**: Validated or 422 error

**Steps**:
1. [ ] - `p2` - DB: load plugin_instance_id from session type record - `inst-vs-load-type`
2. [ ] - `p2` - DB: load plugin config for the plugin instance and session type - `inst-vs-load-config`
3. [ ] - `p2` - Resolve plugin: `hub.get_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(&plugin_instance_id))` - `inst-vs-resolve-plugin`
4. [ ] - `p2` - **IF** plugin does not support `on_session_summary` method **RETURN** 422 Unprocessable Entity (summarization not supported) - `inst-vs-check-support`
5. [ ] - `p2` - **RETURN** validated (plugin_instance_id, plugin_config) - `inst-vs-return`

### Invoke Summary via Backend Plugin

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-session-intelligence-invoke-summary`

**Input**: session record, message history, enabled_capabilities, plugin_instance_id, plugin_config
**Output**: Streaming summary response handle or 502 error

**Steps**:
1. [ ] - `p2` - Resolve plugin: `hub.get_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(&plugin_instance_id))` - `inst-is-resolve-plugin`
2. [ ] - `p2` - Build SessionSummaryCtx: {session_id, session_type_id, plugin_config, enabled_capabilities, messages, summarization_settings, timestamp} - `inst-is-build-ctx`
3. [ ] - `p2` - **TRY** - `inst-is-try`
   1. [ ] - `p2` - Call `plugin.on_session_summary(ctx, &mut stream)` -- plugin streams summary text via ResponseStream - `inst-is-call`
   2. [ ] - `p2` - **RETURN** streaming response handle for NDJSON pipe - `inst-is-return-handle`
4. [ ] - `p2` - **CATCH** plugin error (timeout, unavailable, etc.) - `inst-is-catch`
   1. [ ] - `p2` - **RETURN** 502 Bad Gateway with error detail from plugin - `inst-is-return-error`

### Evaluate Retention Policy

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-session-intelligence-evaluate-retention`

**Input**: session_id, retention_policy
**Output**: List of message IDs eligible for deletion

**Steps**:
1. [ ] - `p2` - **IF** retention_policy.type == none **RETURN** empty list - `inst-er-skip-none`
2. [ ] - `p2` - **IF** retention_policy.type == age_based - `inst-er-age`
   1. [ ] - `p2` - DB: load message IDs older than max_age_days with a parent (non-root), ordered chronologically. Root messages are excluded to preserve conversation anchors. - `inst-er-age-query`
3. [ ] - `p2` - **IF** retention_policy.type == count_based - `inst-er-count`
   1. [ ] - `p2` - DB: count total non-root messages in session - `inst-er-count-total`
   2. [ ] - `p2` - **IF** total <= retention_policy.max_message_count **RETURN** empty list - `inst-er-count-skip`
   3. [ ] - `p2` - DB: load oldest non-root message IDs exceeding max_message_count threshold. Root messages are excluded to preserve conversation anchors (consistent with age-based retention). - `inst-er-count-query`
4. [ ] - `p2` - **RETURN** list of eligible message IDs - `inst-er-return`

> **Design Note — Root Message Preservation**: Both age-based and count-based retention exclude root messages (messages with no parent) from deletion eligibility. Root messages serve as conversation anchors that establish the session context. Deleting a root message would orphan the entire conversation tree. If a root message must be removed, use session-level hard delete instead.

### Enforce Retention Cleanup

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-session-intelligence-enforce-retention`

**Input**: None (scheduled or event-triggered)
**Output**: Count of deleted messages per session

**Steps**:
1. [ ] - `p2` - DB: load all active sessions with an effective retention policy (session-level or inherited from session type) - `inst-rc-load-sessions`
2. [ ] - `p2` - **FOR EACH** (session_id, effective_policy) in result set - `inst-rc-loop`
   1. [ ] - `p2` - DB: acquire per-session advisory lock (`pg_try_advisory_xact_lock(session_id)`) to prevent concurrent enforcement runs from conflicting; **IF** lock not acquired **CONTINUE** to next session - `inst-rc-lock`
   2. [ ] - `p2` - Algorithm: evaluate retention policy using `cpt-cf-chat-engine-algo-session-intelligence-evaluate-retention` - `inst-rc-evaluate`
   2. [ ] - `p2` - **IF** eligible message IDs is empty: **CONTINUE** - `inst-rc-skip-empty`
   3. [ ] - `p2` - **FOR EACH** root_message_id in eligible message IDs - `inst-rc-delete-loop`
      1. [ ] - `p2` - DB: recursively delete message and its entire subtree (all descendants via parent_message_id) - `inst-rc-cascade-delete`
   4. [ ] - `p2` - Log: structured event {session_id, messages_deleted, policy_type, duration_ms} - `inst-rc-log`
3. [ ] - `p2` - **RETURN** summary of deletions per session - `inst-rc-return`

## 4. States (CDSL)

### None

Not applicable. Session Intelligence does not introduce new entity lifecycle states. Summary generation is a stateless request-response operation. Retention enforcement is a scheduled batch operation that transitions messages directly to deletion.

## 5. Definitions of Done

### On-Demand Session Summary

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-session-intelligence-summary`

The system **MUST** accept POST /sessions/{session_id}/summary, validate summarization support for the session's type, retrieve the visible message history, invoke the backend plugin with `on_session_summary`, and stream the summary response back to the client via NDJSON.

**Implements**:
- `cpt-cf-chat-engine-flow-session-intelligence-generate-summary`
- `cpt-cf-chat-engine-algo-session-intelligence-validate-summarization`
- `cpt-cf-chat-engine-algo-session-intelligence-invoke-summary`

**Touches**:
- API: `POST /sessions/{session_id}/summary`
- DB: `sessions`, `session_types`, `plugin_configs`, `messages`
- Entities: `SessionSummaryEvent`, `SessionSummaryResponse`, `StreamingStartEvent`, `StreamingChunkEvent`, `StreamingCompleteEvent`

### Retention Policy Configuration

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-session-intelligence-retention-config`

The system **MUST** allow reading and updating retention policies per individual session via GET and PATCH /sessions/{session_id}/retention-policy, with validation of policy type (age_based, count_based, none) and corresponding parameters. Per-session policies override the session type default when present.

**Implements**:
- `cpt-cf-chat-engine-flow-session-intelligence-get-retention`
- `cpt-cf-chat-engine-flow-session-intelligence-update-retention`

**Touches**:
- API: `GET /sessions/{session_id}/retention-policy`, `PATCH /sessions/{session_id}/retention-policy`
- DB: `sessions`, `session_types`
- Entities: `RetentionPolicy`

### Retention Policy Enforcement

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-session-intelligence-retention-enforcement`

The system **MUST** enforce configured retention policies via a scheduled background job (or event-triggered cleanup), evaluating age-based and count-based policies against the messages table and deleting eligible messages with their subtrees.

**Implements**:
- `cpt-cf-chat-engine-algo-session-intelligence-evaluate-retention`
- `cpt-cf-chat-engine-algo-session-intelligence-enforce-retention`

**Touches**:
- DB: `sessions`, `session_types`, `messages`
- Entities: `RetentionPolicy`

### Cascade Deletion During Retention

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-session-intelligence-cascade-delete`

The system **MUST** delete message subtrees atomically during retention enforcement using recursive CTE queries, ensuring no orphaned child messages remain after a parent message is removed by retention policy.

**Implements**:
- `cpt-cf-chat-engine-algo-session-intelligence-enforce-retention`

**Touches**:
- DB: `messages` (recursive CTE delete with FK on parent_message_id)
- Entities: `Message`

## 6. Acceptance Criteria

- [ ] POST /sessions/{session_id}/summary returns 422 when the session type's backend plugin does not support summarization
- [ ] POST /sessions/{session_id}/summary streams NDJSON summary response with StreamingStartEvent, StreamingChunkEvent, and StreamingCompleteEvent when summarization is supported
- [ ] GET /sessions/{session_id}/retention-policy returns the session-level policy or falls back to the session type default
- [ ] PATCH /sessions/{session_id}/retention-policy rejects invalid policy types and out-of-range parameters with 400
- [ ] Age-based retention deletes messages older than max_age_days; count-based retention deletes oldest messages exceeding max_message_count
- [ ] Retention enforcement deletes the entire subtree of each eligible message, leaving no orphaned children
- [ ] Retention cleanup runs without degrading concurrent request latency beyond acceptable thresholds
- [ ] Retention enforcement emits structured log events per session with deletion count and duration

## 7. Non-Functional Considerations

- **Performance**: Summary streaming first-byte target < 500ms (dependent on backend plugin latency). Retention cleanup batches deletions per session within a single transaction to minimize lock duration. Background job runs during low-traffic windows when scheduled.
- **Security**: Summary content is streamed opaquely; Chat Engine does not interpret or log summary text. Retention policy updates require authenticated requests with ownership validation.
- **Reliability**: Retention enforcement is idempotent; re-running the job does not double-delete. If a retention run fails mid-batch, the next run picks up remaining sessions. Backend plugin failures during summary generation return 502 without side effects.
- **Data**: Retention evaluation queries use existing indexes on `messages.session_id` and `messages.created_at`. Recursive CTE for subtree deletion uses index on `messages.parent_message_id`. No new tables introduced; retention policy stored in `sessions.metadata` JSONB or dedicated column per DESIGN.
- **Observability**: Metrics: `summary_duration_seconds` (histogram by session_type_id), `retention_messages_deleted_total` (counter by policy_type). Log events for retention start/complete/error with `session_id`, `messages_deleted`, `duration_ms`.
- **Compliance / UX / Business**: Not applicable -- see session-lifecycle section 7.
