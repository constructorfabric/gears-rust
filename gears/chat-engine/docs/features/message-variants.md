Created:  2026-03-20 by Constructor Tech
Updated:  2026-03-20 by Constructor Tech
# Feature: Message Variants & Branching


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Recreate Response](#recreate-response)
  - [Branch from Message](#branch-from-message)
  - [Navigate Variants](#navigate-variants)
  - [Switch Session Type](#switch-session-type)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Assign Variant Index](#assign-variant-index)
  - [Update Active Path](#update-active-path)
  - [Validate Plugin Capability](#validate-plugin-capability)
- [4. States (CDSL)](#4-states-cdsl)
  - [None](#none)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Recreate Response](#recreate-response-1)
  - [Branch from Message](#branch-from-message-1)
  - [Variant Navigation](#variant-navigation)
  - [Session Type Switching](#session-type-switching)
  - [Active Path Consistency](#active-path-consistency)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-featstatus-message-variants`

## 1. Feature Context

- [ ] `p2` - `cpt-cf-chat-engine-feature-message-variants`

### 1.1 Overview

Message tree operations for the Chat Engine: recreating assistant responses as sibling variants, branching conversations from any historical message node, navigating between existing variants at a given position, and switching session types mid-session with plugin capability validation. All operations preserve the immutable message tree and track the session's active path.

**Traces to**: `cpt-cf-chat-engine-fr-recreate-response`, `cpt-cf-chat-engine-fr-branch-message`, `cpt-cf-chat-engine-fr-navigate-variants`, `cpt-cf-chat-engine-fr-switch-session-type`, `cpt-cf-chat-engine-nfr-message-history`

### 1.2 Purpose

Enable end-users to explore alternative AI responses, branch conversations from historical points, and switch between backend plugins mid-session. This feature extends the message processing pipeline with tree-level operations that do not alter existing message nodes.

Success criteria: Variant creation latency equivalent to normal message send; branch operations complete within 100ms p95 (excluding plugin invocation); active path updates are atomic and consistent.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-chat-engine-actor-client` | Requests response recreation, initiates branching, navigates variants, switches session type |
| `cpt-cf-chat-engine-actor-backend-plugin` | Processes MessageRecreateEvent, returns streaming response chunks; provides capabilities on session type switch |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADRs**: [ADR-0011](../ADR/0011-message-variants.md) (Message Variants), [ADR-0012](../ADR/0012-variant-indexing.md) (Variant Indexing), [ADR-0013](../ADR/0013-message-recreation.md) (Message Recreation), [ADR-0014](../ADR/0014-branching-strategy.md) (Branching Strategy), [ADR-0015](../ADR/0015-session-switching.md) (Session Switching)
- **Dependencies**: `cpt-cf-chat-engine-feature-message-processing`

## 2. Actor Flows (CDSL)

### Recreate Response

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-message-variants-recreate`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client requests recreation of an assistant message; a new variant sibling is created with incremented variant_index; plugin streams a new response; old variant marked inactive, new variant marked active

**Error Scenarios**:
- Target message not found or not owned by caller (403/404)
- Target message role is not assistant (400)
- Session is not in active lifecycle state (409)
- Backend plugin unavailable (503)
- Concurrent variant creation race — retry exhausted (409)

**Steps**:
1. [ ] - `p1` - Algorithm: validate request using `cpt-cf-chat-engine-algo-message-processing-validate-request` - `inst-recreate-validate`
2. [ ] - `p1` - API: POST /sessions/{session_id}/messages/{message_id}/recreate (body: enabled_capabilities?) - `inst-recreate-api`
3. [ ] - `p1` - DB: Load the target message (message_id, parent_message_id, role, session_id) by message_id and session_id - `inst-recreate-load-target`
4. [ ] - `p1` - **IF** target message role != 'assistant' **RETURN** 400 Bad Request (only assistant messages can be recreated) - `inst-recreate-check-role`
5. [ ] - `p1` - Algorithm: compute next variant_index using `cpt-cf-chat-engine-algo-message-variants-assign-variant-index` - `inst-recreate-assign-index`
6. [ ] - `p1` - DB: Deactivate all currently active sibling messages sharing the same parent_message_id within the session - `inst-recreate-deactivate-old`
7. [ ] - `p1` - DB: Create a new assistant message with parent_message_id=target.parent_message_id, variant_index=new_index, is_active=true, is_complete=false — returns new_message_id - `inst-recreate-insert`
8. [ ] - `p1` - Algorithm: build history context using `cpt-cf-chat-engine-algo-message-processing-build-history` - `inst-recreate-build-history`
9. [ ] - `p1` - Algorithm: invoke backend plugin with message.recreate event using `cpt-cf-chat-engine-algo-message-processing-invoke-plugin` - `inst-recreate-invoke`
10. [ ] - `p1` - Stream: emit StreamingStartEvent(message_id=new_message_id) to client - `inst-recreate-stream-start`
11. [ ] - `p1` - **FOR EACH** chunk received from plugin stream - `inst-recreate-stream-chunks`
    1. [ ] - `p1` - Stream: emit StreamingChunkEvent(message_id, chunk) to client - `inst-recreate-stream-chunk`
12. [ ] - `p1` - Algorithm: persist complete response using `cpt-cf-chat-engine-algo-message-processing-persist-response` - `inst-recreate-persist`
13. [ ] - `p1` - Algorithm: update active path using `cpt-cf-chat-engine-algo-message-variants-update-active-path` - `inst-recreate-update-path`
14. [ ] - `p1` - Stream: emit StreamingCompleteEvent(message_id, metadata, variant_info) to client - `inst-recreate-stream-complete`
15. [ ] - `p1` - **RETURN** stream closed - `inst-recreate-return`

### Branch from Message

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-flow-message-variants-branch`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client sends a new user message from a historical branch point; session active path is updated to the new branch; plugin receives truncated history up to the branch point

**Error Scenarios**:
- Branch point message not found in session (400)
- Session is not in active lifecycle state (409)
- Backend plugin unavailable (503)

**Steps**:
1. [ ] - `p2` - Algorithm: validate request using `cpt-cf-chat-engine-algo-message-processing-validate-request` - `inst-branch-validate`
2. [ ] - `p2` - API: POST /sessions/{session_id}/messages/{message_id}/branch (body: content, file_ids?, enabled_capabilities?) - `inst-branch-api`
3. [ ] - `p2` - DB: Load the branch point message by message_id and session_id - `inst-branch-load-point`
4. [ ] - `p2` - **IF** branch point message not found **RETURN** 400 Bad Request (branch point not in session) - `inst-branch-check-point`
5. [ ] - `p2` - Algorithm: compute next variant_index using `cpt-cf-chat-engine-algo-message-variants-assign-variant-index` with parent_message_id=branch_point_message_id - `inst-branch-assign-index`
6. [ ] - `p2` - DB: Deactivate existing active children of branch_point_message_id within the session (set is_active=false for all currently active variants at the same parent position) - `inst-branch-deactivate-existing`
7. [ ] - `p2` - DB: Create a new user message as child of branch_point_message_id with content, file_ids, variant_index=new_index, is_active=true, is_complete=true — returns user_message_id - `inst-branch-insert-user`
8. [ ] - `p2` - DB: Create a new assistant message as child of user_message_id with variant_index=0, is_active=true, is_complete=false — returns assistant_message_id - `inst-branch-insert-assistant`
9. [ ] - `p2` - Algorithm: build history context using `cpt-cf-chat-engine-algo-message-processing-build-history` (history truncated to branch point ancestry) - `inst-branch-build-history`
10. [ ] - `p2` - Algorithm: invoke backend plugin with message.new event using `cpt-cf-chat-engine-algo-message-processing-invoke-plugin` - `inst-branch-invoke`
11. [ ] - `p2` - Stream: emit StreamingStartEvent(message_id=assistant_message_id) to client - `inst-branch-stream-start`
12. [ ] - `p2` - **FOR EACH** chunk received from plugin stream - `inst-branch-stream-chunks`
    1. [ ] - `p2` - Stream: emit StreamingChunkEvent(message_id, chunk) to client - `inst-branch-stream-chunk`
13. [ ] - `p2` - Algorithm: persist complete response using `cpt-cf-chat-engine-algo-message-processing-persist-response` - `inst-branch-persist`
14. [ ] - `p2` - Algorithm: update active path using `cpt-cf-chat-engine-algo-message-variants-update-active-path` - `inst-branch-update-path`
15. [ ] - `p2` - Stream: emit StreamingCompleteEvent(message_id, metadata) to client - `inst-branch-stream-complete`
16. [ ] - `p2` - **RETURN** stream closed - `inst-branch-return`

### Navigate Variants

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-flow-message-variants-navigate`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client retrieves all variants for a given message position; client selects a variant, updating the active path

**Error Scenarios**:
- Target message not found or not owned by caller (403/404)
- Requested variant_index does not exist (404)

**Steps**:
1. [ ] - `p2` - Algorithm: validate request using `cpt-cf-chat-engine-algo-message-processing-validate-request` - `inst-nav-validate`
2. [ ] - `p2` - **IF** session.lifecycle_state NOT IN (active, archived) **RETURN** 409 Conflict (variant navigation requires active or archived session) - `inst-nav-check-state`
3. [ ] - `p2` - **IF** listing variants: API GET /sessions/{session_id}/messages/{message_id}/variants - `inst-nav-list-api`
   1. [ ] - `p2` - DB: Fetch all sibling messages sharing the same parent as the target message within the session, ordered by variant_index ascending (fields: message_id, variant_index, is_active, is_complete, content, metadata, created_at) - `inst-nav-list-db`
   2. [ ] - `p2` - Map each variant to response entry with VariantInfo: {variant_index, total_variants, is_active} - `inst-nav-list-map`
   3. [ ] - `p2` - **RETURN** 200 (variants[], current_index) - `inst-nav-list-return`
4. [ ] - `p2` - **IF** selecting active variant: API PUT /sessions/{session_id}/messages/{message_id}/variants/active (body: variant_index) - `inst-nav-select-api`
   1. [ ] - `p2` - DB: Find the sibling message with the requested variant_index sharing the same parent as the target message within the session - `inst-nav-select-find`
   2. [ ] - `p2` - **IF** variant not found **RETURN** 404 Not Found - `inst-nav-select-not-found`
   3. [ ] - `p2` - DB: Deactivate all currently active siblings at the same parent within the session - `inst-nav-select-deactivate`
   4. [ ] - `p2` - DB: Activate the selected variant by message_id - `inst-nav-select-activate`
   5. [ ] - `p2` - Algorithm: update active path using `cpt-cf-chat-engine-algo-message-variants-update-active-path` - `inst-nav-select-update-path`
   6. [ ] - `p2` - **RETURN** 200 (updated variant with VariantInfo) - `inst-nav-select-return`

### Switch Session Type

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-flow-message-variants-switch-type`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client switches session to a different session type; new plugin provides updated capabilities; session type and capabilities are persisted

**Error Scenarios**:
- Target session_type_id does not exist (404)
- Session is not in active lifecycle state (409)
- New plugin unavailable (502)

**Steps**:
1. [ ] - `p2` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-switch-auth`
2. [ ] - `p2` - Algorithm: validate ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-switch-ownership`
3. [ ] - `p2` - API: PATCH /sessions/{session_id}/session-type (body: session_type_id) - `inst-switch-api`
4. [ ] - `p2` - **IF** session.lifecycle_state != 'active' **RETURN** 409 Conflict (switching only allowed in active sessions) - `inst-switch-check-state`
5. [ ] - `p2` - DB: Load the target session type record (session_type_id, plugin_instance_id) by session_type_id - `inst-switch-load-type`
6. [ ] - `p2` - **IF** session type not found **RETURN** 404 Not Found - `inst-switch-type-not-found`
7. [ ] - `p2` - Algorithm: validate plugin capability using `cpt-cf-chat-engine-algo-message-variants-validate-plugin-capability` - `inst-switch-validate-plugin`
8. [ ] - `p2` - DB: Update the session's session_type_id, enabled_capabilities, and refresh updated_at, identified by session_id - `inst-switch-update-session`
9. [ ] - `p2` - **RETURN** 200 (updated session with new session_type_id and refreshed enabled_capabilities) - `inst-switch-return`

## 3. Processes / Business Logic (CDSL)

### Assign Variant Index

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-message-variants-assign-variant-index`

**Input**: session_id, parent_message_id
**Output**: next variant_index (integer) or 409 Conflict on exhausted retries

**Steps**:
1. [ ] - `p1` - Begin serializable sub-transaction scoped to (session_id, parent_message_id) - `inst-avi-begin-tx`
2. [ ] - `p1` - DB: Compute the next variant_index by finding the current maximum variant_index among siblings (same session_id and parent_message_id) and incrementing by 1; default to 0 if no siblings exist - `inst-avi-select-max`
3. [ ] - `p1` - **TRY** - `inst-avi-try`
   1. [ ] - `p1` - **RETURN** next_index for use in INSERT - `inst-avi-return-index`
4. [ ] - `p1` - **CATCH** unique constraint violation (concurrent race on UNIQUE(session_id, parent_message_id, variant_index)) - `inst-avi-catch`
   1. [ ] - `p1` - Retry from step 1; maximum 3 retries - `inst-avi-retry`
   2. [ ] - `p1` - **IF** retries exhausted **RETURN** 409 Conflict (concurrent variant creation) - `inst-avi-exhausted`

### Update Active Path

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-message-variants-update-active-path`

**Input**: session_id, newly active message_id
**Output**: active path updated from root to the newly active message

**Steps**:
1. [ ] - `p2` - Walk ancestor chain from newly active message_id to root using parent_message_id references - `inst-uap-walk-ancestors`
2. [ ] - `p2` - DB: Activate all ancestor messages in the chain for the given session - `inst-uap-activate-ancestors`
3. [ ] - `p2` - **FOR EACH** ancestor in the chain: deactivate sibling variants that are not on the active path - `inst-uap-deactivate-siblings`
   1. [ ] - `p2` - DB: Deactivate all active sibling messages at each ancestor's parent that are not on the active path - `inst-uap-deactivate-sibling`
4. [ ] - `p2` - **RETURN** active path updated - `inst-uap-return`

### Validate Plugin Capability

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-message-variants-validate-plugin-capability`

**Input**: new session_type_id, session record
**Output**: refreshed enabled_capabilities or 502 error

**Steps**:
1. [ ] - `p2` - DB: Load the plugin_instance_id from the session type record by session_type_id - `inst-vpc-load-plugin`
2. [ ] - `p2` - Resolve plugin: `hub.get_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(&plugin_instance_id))` - `inst-vpc-resolve`
3. [ ] - `p2` - **IF** plugin not found **RETURN** 502 Bad Gateway (target plugin not registered) - `inst-vpc-not-found`
4. [ ] - `p2` - **TRY** - `inst-vpc-try`
   1. [ ] - `p2` - Call the plugin's `on_session_updated` hook with the session context — the plugin queries Model Registry for capabilities and returns a list of capabilities - `inst-vpc-call`
   2. [ ] - `p2` - **RETURN** the list of capabilities as the refreshed enabled_capabilities - `inst-vpc-return-caps`
5. [ ] - `p2` - **CATCH** plugin error - `inst-vpc-catch`
   1. [ ] - `p2` - **RETURN** 502 Bad Gateway with error detail - `inst-vpc-error`

## 4. States (CDSL)

### None

Not applicable. Message variants do not introduce new entity lifecycle states beyond those defined in `cpt-cf-chat-engine-state-session-lifecycle-session` (session lifecycle) and `cpt-cf-chat-engine-state-message-processing-stream` (streaming state). The is_active flag on messages is a path-selection marker, not a state machine transition.

## 5. Definitions of Done

### Recreate Response

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-message-variants-recreate`

The system **MUST** accept POST /sessions/{session_id}/messages/{message_id}/recreate, create a new assistant message variant as a sibling of the target message (same parent_message_id, incremented variant_index), invoke the backend plugin with MessageRecreateEvent, stream the response via NDJSON, and update the active path to include the new variant.

**Implements**:
- `cpt-cf-chat-engine-flow-message-variants-recreate`
- `cpt-cf-chat-engine-algo-message-variants-assign-variant-index`
- `cpt-cf-chat-engine-algo-message-variants-update-active-path`
- `cpt-cf-chat-engine-algo-message-processing-invoke-plugin`
- `cpt-cf-chat-engine-algo-message-processing-persist-response`

**Touches**:
- API: `POST /sessions/{session_id}/messages/{message_id}/recreate`
- DB: `messages` (INSERT variant, UPDATE is_active)
- Entities: `Message`, `MessageRecreateEvent`, `VariantInfo`

### Branch from Message

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-message-variants-branch`

The system **MUST** accept POST /sessions/{session_id}/messages/{message_id}/branch, create a new user message as a child of the specified branch point, invoke the backend plugin with MessageNewEvent containing history truncated to the branch point ancestry, stream the response, and update the session's active path to follow the new branch.

**Implements**:
- `cpt-cf-chat-engine-flow-message-variants-branch`
- `cpt-cf-chat-engine-algo-message-variants-assign-variant-index`
- `cpt-cf-chat-engine-algo-message-variants-update-active-path`
- `cpt-cf-chat-engine-algo-message-processing-build-history`
- `cpt-cf-chat-engine-algo-message-processing-invoke-plugin`
- `cpt-cf-chat-engine-algo-message-processing-persist-response`

**Touches**:
- API: `POST /sessions/{session_id}/messages/{message_id}/branch`
- DB: `messages` (INSERT user + assistant nodes, UPDATE is_active)
- Entities: `Message`, `MessageNewEvent`, `VariantInfo`

### Variant Navigation

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-message-variants-navigation`

The system **MUST** allow clients to list all variants at a given message position via GET /sessions/{session_id}/messages/{message_id}/variants (returning variant_index, total_variants, is_active per entry) and select an active variant via PUT /sessions/{session_id}/messages/{message_id}/variants/active, updating the is_active flag and recomputing the active path.

**Implements**:
- `cpt-cf-chat-engine-flow-message-variants-navigate`
- `cpt-cf-chat-engine-algo-message-variants-update-active-path`

**Touches**:
- API: `GET /sessions/{session_id}/messages/{message_id}/variants`, `PUT /sessions/{session_id}/messages/{message_id}/variants/active`
- DB: `messages` (SELECT siblings, UPDATE is_active)
- Entities: `Message`, `VariantInfo`

### Session Type Switching

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-message-variants-switch-type`

The system **MUST** allow clients to switch the session's session_type_id via PATCH /sessions/{session_id}/session-type, validate that the target session type's plugin is available and capable, refresh enabled_capabilities from the new plugin via `on_session_updated`, and persist the updated session. Subsequent messages are routed to the new backend plugin with full conversation history.

**Implements**:
- `cpt-cf-chat-engine-flow-message-variants-switch-type`
- `cpt-cf-chat-engine-algo-message-variants-validate-plugin-capability`
- `cpt-cf-chat-engine-algo-session-lifecycle-authenticate`
- `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership`

**Touches**:
- API: `PATCH /sessions/{session_id}/session-type`
- DB: `sessions` (UPDATE session_type_id, enabled_capabilities), `session_types`
- Entities: `Session`, `SessionType`, `Capability`

### Active Path Consistency

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-message-variants-active-path`

The system **MUST** maintain a consistent active path through the message tree: at any given parent_message_id, exactly one child has is_active=true. After any variant creation, branch operation, or variant selection, the active path from root to the most recent active leaf is fully consistent.

**Implements**:
- `cpt-cf-chat-engine-algo-message-variants-update-active-path`

**Touches**:
- DB: `messages` (is_active flag consistency)
- Entities: `Message`

## 6. Acceptance Criteria

- [ ] Recreating an assistant message creates a new sibling variant with the next sequential variant_index; the original response is preserved with is_active=false
- [ ] Branching from a historical message creates a new user message child at the branch point; plugin receives history truncated to the branch point ancestry
- [ ] GET /variants returns all siblings at a position with accurate VariantInfo (variant_index, total_variants, is_active)
- [ ] PUT /variants/active switches the active variant and updates the is_active chain from root to the selected variant
- [ ] Concurrent variant creation on the same parent_message_id is handled via retry with serializable sub-transaction; duplicate variant_index is impossible at the database level (UNIQUE constraint)
- [ ] Session type switching updates session_type_id and refreshes enabled_capabilities from the new plugin; subsequent messages route to the new backend plugin
- [ ] All variant and branch operations respect session lifecycle state; operations on non-active sessions return 409 Conflict

## 7. Non-Functional Considerations

- **Performance**: Variant creation latency equivalent to normal message send (dominated by plugin invocation). Branch operation database overhead under 50ms p95 (excluding plugin). Active path update uses recursive ancestor walk bounded by message tree depth. Index on `messages.parent_message_id` critical for sibling queries.
- **Data**: UNIQUE constraint on `(session_id, parent_message_id, variant_index)` enforces variant integrity at the database level. Serializable sub-transaction for variant_index assignment prevents races. Index on `(session_id, parent_message_id)` for efficient variant listing.
- **Reliability**: Variant_index assignment uses up to 3 retries on concurrent conflict before returning 409. Active path update is atomic within a single transaction. Plugin failures during recreation do not corrupt the existing variant tree (pre-inserted assistant message remains with is_complete=false).
- **Observability**: Structured log events for recreate/branch/navigate/switch with `trace_id`, `session_id`, `message_id`, `variant_index`, `operation`, `duration_ms`. Metrics: `variant_creation_total` counter by operation type.
- **Security**: Ownership validation enforced on all variant operations via `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership`. Session type switching restricted to the session owner. Plugin capabilities are refreshed from the plugin, never supplied by the client.
- **Compliance / UX / Business**: Not applicable -- see session-lifecycle section 7.
