Created:  2026-03-06 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# Feature: Message Processing & Streaming


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Send Message](#send-message)
  - [Cancel Streaming](#cancel-streaming)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Validate Request](#validate-request)
  - [Build History Context](#build-history-context)
  - [Invoke Backend Plugin](#invoke-backend-plugin)
  - [Persist Response](#persist-response)
- [4. States (CDSL)](#4-states-cdsl)
  - [Streaming State Machine](#streaming-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Send Message and Invoke Backend Plugin](#send-message-and-invoke-backend-plugin)
  - [File UUID Attachments](#file-uuid-attachments)
  - [NDJSON Streaming Pipeline](#ndjson-streaming-pipeline)
  - [Streaming Cancellation](#streaming-cancellation)
  - [Backend Plugin Isolation](#backend-plugin-isolation)
  - [Message Tree Persistence](#message-tree-persistence)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-featstatus-message-processing`

## 1. Feature Context

- [ ] `p1` - `cpt-cf-chat-engine-feature-message-processing`

**Traces to**: `cpt-cf-chat-engine-fr-send-message` (FR-002), `cpt-cf-chat-engine-fr-attach-files` (FR-003), `cpt-cf-chat-engine-fr-stop-streaming` (FR-008), `cpt-cf-chat-engine-nfr-response-time` (NFR-001), `cpt-cf-chat-engine-nfr-streaming` (NFR-005), `cpt-cf-chat-engine-nfr-data-integrity` (NFR-006), `cpt-cf-chat-engine-nfr-file-size` (NFR-009), `cpt-cf-chat-engine-nfr-backend-isolation` (NFR-008)

### 1.1 Overview

Core message exchange pipeline: accepts user messages with optional file UUID references, invokes backend plugins synchronously with full session context, pipes streaming AI responses back to clients over HTTP chunked transfer (NDJSON), and persists the complete exchange atomically. Implements streaming cancellation via client connection close with partial response persistence.

### 1.2 Purpose

Enable end-users to send messages to backend plugins and receive real-time streamed responses. This feature owns the message tree append, NDJSON streaming pipeline, and streaming cancellation — all other message-level features (variants, context strategies, reactions) build on this foundation.

Success criteria: Message routing latency under 100ms p95; streaming first-byte under 200ms; partial responses saved reliably on cancellation.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-chat-engine-actor-client` | Sends messages, receives NDJSON stream, cancels streaming |
| `cpt-cf-chat-engine-actor-backend-plugin` | Processes MessageNewEvent, returns streaming response chunks |
| `cpt-cf-chat-engine-actor-file-storage` | Stores files independently; backend plugin retrieves files by UUID |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Dependencies**: `cpt-cf-chat-engine-feature-session-lifecycle`

## 2. Actor Flows (CDSL)

### Send Message

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-message-processing-send-message`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client sends message (with or without file UUIDs); plugin streams response; both messages persisted

**Error Scenarios**:
- Session not found or not owned by caller (403/404)
- Session is not in active lifecycle state (409)
- parent_message_id not found in session (400)
- Backend plugin unavailable (503)
- Plugin timeout (504)

**Steps**:
1. [ ] - `p1` - Algorithm: validate request using `cpt-cf-chat-engine-algo-message-processing-validate-request` - `inst-send-validate`
2. [ ] - `p1` - API: POST /sessions/{session_id}/messages (body: content, file_ids?, parent_message_id?, enabled_capabilities?) - `inst-send-api`
3. [ ] - `p1` - **IF** file_ids present: validate each UUID format using `cpt-cf-chat-engine-algo-message-processing-validate-request` - `inst-send-validate-files`
4. [ ] - `p1` - DB: Insert user message record (session_id, parent_message_id, role='user', content, file_ids, variant_index=0, is_active=true, is_complete=true) and return user_message_id - `inst-send-insert-user`
5. [ ] - `p1` - Algorithm: build history context using `cpt-cf-chat-engine-algo-message-processing-build-history` - `inst-send-build-history`
6. [ ] - `p1` - DB: Pre-insert assistant message record (session_id, parent_message_id=user_message_id, role='assistant', is_complete=false) to obtain assistant_message_id - `inst-send-preinsert-assistant`
7. [ ] - `p1` - Algorithm: invoke backend plugin with message.new event using `cpt-cf-chat-engine-algo-message-processing-invoke-plugin` - `inst-send-invoke`
8. [ ] - `p1` - Stream: emit StreamingStartEvent(message_id=assistant_message_id) to client - `inst-send-stream-start`
9. [ ] - `p1` - **FOR EACH** chunk received from plugin stream - `inst-send-stream-chunks`
   1. [ ] - `p1` - Stream: emit StreamingChunkEvent(message_id, chunk) to client - `inst-send-stream-chunk`
10. [ ] - `p1` - Algorithm: persist complete response using `cpt-cf-chat-engine-algo-message-processing-persist-response` - `inst-send-persist`
11. [ ] - `p1` - Stream: emit StreamingCompleteEvent(message_id, metadata) to client - `inst-send-stream-complete`
12. [ ] - `p1` - **RETURN** stream closed - `inst-send-return`

### Cancel Streaming

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-message-processing-cancel-streaming`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client closes connection or calls DELETE; partial response saved with is_complete=false

**Error Scenarios**:
- Cancellation arrives after stream already completed (no-op)

**Steps**:
1. [ ] - `p1` - **IF** client closes HTTP connection: detect connection close event - `inst-cancel-detect-close`
2. [ ] - `p1` - **IF** client calls DELETE /sessions/{session_id}/messages/{message_id}/streaming: validate ownership using `cpt-cf-chat-engine-algo-message-processing-validate-request` - `inst-cancel-explicit`
3. [ ] - `p1` - **IF** stream already completed: **RETURN** 200 (no-op) - `inst-cancel-noop`
4. [ ] - `p1` - Cancel backend plugin stream - `inst-cancel-close-webhook`
5. [ ] - `p1` - Algorithm: persist partial response using `cpt-cf-chat-engine-algo-message-processing-persist-response` (is_complete=false) - `inst-cancel-persist`
6. [ ] - `p2` - Send MessageAbortedEvent to backend plugin (fire-and-forget) - `inst-cancel-notify`
7. [ ] - `p1` - **RETURN** 204 No Content (explicit DELETE) or connection close acknowledged - `inst-cancel-return`

## 3. Processes / Business Logic (CDSL)

### Validate Request

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-message-processing-validate-request`

**Input**: session_id, request identity (tenant_id, user_id), message body
**Output**: Validated request context or 400/403/404/409 error

**Steps**:
1. [ ] - `p1` - Algorithm: authenticate using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-vr-auth`
2. [ ] - `p1` - Algorithm: validate session ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-vr-ownership`
3. [ ] - `p1` - **IF** session.lifecycle_state != 'active' **RETURN** 409 Conflict (messages only allowed in active sessions) - `inst-vr-check-state`
4. [ ] - `p1` - **IF** content is empty or null **RETURN** 400 Bad Request - `inst-vr-check-content`
5. [ ] - `p1` - **IF** file_ids provided: **FOR EACH** file_id validate UUID v4 format - `inst-vr-file-ids`
   1. [ ] - `p1` - **IF** format invalid **RETURN** 400 Bad Request (invalid file_id) - `inst-vr-file-format`
6. [ ] - `p1` - **IF** parent_message_id provided: DB: Look up message by message_id and session_id to verify parent exists in session - `inst-vr-parent`
   1. [ ] - `p1` - **IF** not found **RETURN** 400 Bad Request (parent not in session) - `inst-vr-parent-missing`
7. [ ] - `p1` - **RETURN** validated context (session, identity, content, file_ids, parent_message_id, enabled_capabilities) - `inst-vr-return`

### Build History Context

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-message-processing-build-history`

**Input**: session_id, session_type config
**Output**: Ordered message history array for plugin payload

**Steps**:
1. [ ] - `p1` - DB: Retrieve all visible active messages for session_id (where is_hidden_from_backend=false and is_active=true), ordered by created_at ascending - `inst-bh-select`
2. [ ] - `p1` - **IF** session_type.history_depth is set: apply depth limit (take last N messages) - `inst-bh-depth`
3. [ ] - `p1` - Map each message to history entry: {role, content, file_ids, metadata} - `inst-bh-map`
4. [ ] - `p1` - **RETURN** ordered history array - `inst-bh-return`

### Invoke Backend Plugin

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-message-processing-invoke-plugin`

**Input**: event_type, session_id, message_id, history, enabled_capabilities, session_type_id
**Output**: Streaming response handle or 502/503/504 error

**Steps**:
1. [ ] - `p1` - DB: Retrieve plugin_instance_id from session_types by session_type_id - `inst-iw-load-config`
2. [ ] - `p1` - Resolve plugin: `hub.get_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(&plugin_instance_id))` - `inst-iw-resolve-plugin`
3. [ ] - `p1` - **IF** plugin not found **RETURN** 503 Service Unavailable (plugin not registered) - `inst-iw-not-found`
4. [ ] - `p1` - Build MessageCtx: {session_id, message_id, session_metadata, enabled_capabilities, message, history, timestamp} - `inst-iw-build-ctx`
5. [ ] - `p1` - **TRY** - `inst-iw-try`
   1. [ ] - `p1` - Call `plugin.on_message(ctx, &mut stream)` — plugin streams chunks via ResponseStream - `inst-iw-call`
   2. [ ] - `p1` - **RETURN** streaming response handle for NDJSON pipe - `inst-iw-return-handle`
6. [ ] - `p1` - **CATCH** plugin error (timeout, unavailable, etc. — managed by plugin/adapter) - `inst-iw-catch`
   1. [ ] - `p1` - **RETURN** 502 Bad Gateway with error detail from plugin - `inst-iw-return-error`

### Persist Response

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-message-processing-persist-response`

**Input**: assistant_message_id, accumulated chunks, completion status (complete / cancelled / error), plugin metadata
**Output**: Updated message_id

**Steps**:
1. [ ] - `p1` - Assemble content array from accumulated StreamingChunkEvent payloads - `inst-pr-assemble`
2. [ ] - `p1` - **IF** the completion status is complete: mark the message as complete and include the plugin metadata - `inst-pr-complete`
3. [ ] - `p1` - **IF** the completion status is cancelled: mark the message as incomplete and record in its metadata that it was cancelled and is partial - `inst-pr-cancelled`
4. [ ] - `p1` - **IF** the completion status is error: mark the message as incomplete and store the error details in its metadata - `inst-pr-error`
5. [ ] - `p1` - DB: Update message record by message_id, setting content, is_complete, metadata, and updated_at timestamp - `inst-pr-update`
6. [ ] - `p1` - **RETURN** message_id - `inst-pr-return`

## 4. States (CDSL)

### Streaming State Machine

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-state-message-processing-stream`

**States**: pending, streaming, completed, cancelled, error
**Initial State**: pending

**Transitions**:
1. [ ] - `p1` - **FROM** pending **TO** streaming **WHEN** plugin responds with first chunk - `inst-st-to-streaming`
2. [ ] - `p1` - **FROM** pending **TO** error **WHEN** plugin unavailable, non-2xx, or timeout before first chunk - `inst-st-pending-to-error`
3. [ ] - `p1` - **FROM** streaming **TO** completed **WHEN** stream-end signal received and response persisted with is_complete=true - `inst-st-to-completed`
4. [ ] - `p1` - **FROM** streaming **TO** cancelled **WHEN** client closes connection mid-stream and partial response persisted - `inst-st-to-cancelled`
5. [ ] - `p1` - **FROM** streaming **TO** error **WHEN** plugin connection drops or backend error mid-stream - `inst-st-streaming-to-error`

## 5. Definitions of Done

### Send Message and Invoke Backend Plugin

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-message-processing-send-message`

The system **MUST** accept POST /sessions/{session_id}/messages, persist the user message node, invoke the backend plugin with MessageNewEvent (including history and enabled_capabilities), and return a streaming NDJSON response identified by the pre-inserted assistant message_id.

**Implements**:
- `cpt-cf-chat-engine-flow-message-processing-send-message`
- `cpt-cf-chat-engine-algo-message-processing-validate-request`
- `cpt-cf-chat-engine-algo-message-processing-build-history`
- `cpt-cf-chat-engine-algo-message-processing-invoke-plugin`

**Touches**:
- API: `POST /sessions/{session_id}/messages`
- DB: `messages`, `session_types`
- Entities: `Message`, `MessageNewEvent`

### File UUID Attachments

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-message-processing-file-attachments`

The system **MUST** store file UUIDs in messages.file_ids and forward them to backend plugins in MessageNewEvent; file content must never pass through Chat Engine.

**Implements**:
- `cpt-cf-chat-engine-flow-message-processing-send-message`
- `cpt-cf-chat-engine-algo-message-processing-validate-request`

**Touches**:
- API: `POST /sessions/{session_id}/messages` (file_ids field)
- DB: `messages.file_ids`
- Entities: `Message`

### NDJSON Streaming Pipeline

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-message-processing-ndjson-streaming`

The system **MUST** pipe plugin streaming response to client as NDJSON over HTTP chunked transfer, emitting StreamingStartEvent, StreamingChunkEvent, and StreamingCompleteEvent with streaming overhead under 10ms p95 and first byte under 200ms.

**Implements**:
- `cpt-cf-chat-engine-flow-message-processing-send-message`
- `cpt-cf-chat-engine-state-message-processing-stream`
- `cpt-cf-chat-engine-algo-message-processing-persist-response`

**Touches**:
- API: `POST /sessions/{session_id}/messages` (NDJSON response stream)
- DB: `messages` (pre-insert for message_id, UPDATE on complete)
- Entities: `StreamingStartEvent`, `StreamingChunkEvent`, `StreamingCompleteEvent`

### Streaming Cancellation

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-message-processing-cancellation`

The system **MUST** detect client connection close mid-stream, cancel the backend plugin stream, and persist the partial response with is_complete=false. Explicit DELETE endpoint provides an alternative cancellation trigger.

**Implements**:
- `cpt-cf-chat-engine-flow-message-processing-cancel-streaming`
- `cpt-cf-chat-engine-algo-message-processing-persist-response`

**Touches**:
- API: `DELETE /sessions/{session_id}/messages/{message_id}/streaming`
- DB: `messages.is_complete`, `messages.metadata`
- Entities: `Message`

### Backend Plugin Isolation

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-message-processing-circuit-breaker`

The system **MUST** isolate backend plugin failures per session type: a failing plugin must not affect sessions using other session types. Plugin implementations own their own resilience patterns (circuit breaker, retry, timeout).

**Implements**:
- `cpt-cf-chat-engine-algo-message-processing-invoke-plugin`

**Touches**:
- ClientHub: `dyn ChatEngineBackendPlugin` resolved by `plugin_instance_id`
- DB: `session_types` (plugin_instance_id lookup)

### Message Tree Persistence

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-message-processing-persistence`

The system **MUST** persist user and assistant messages with ACID guarantees, enforcing tree integrity: FK on parent_message_id, UNIQUE (session_id, parent_message_id, variant_index), and immutable parent relationships.

**Implements**:
- `cpt-cf-chat-engine-algo-message-processing-persist-response`

**Touches**:
- DB: `messages` (UNIQUE constraint on session_id, parent_message_id, variant_index; FK on parent_message_id)
- Entities: `Message`

## 6. Acceptance Criteria

- [ ] User message is persisted to the database before plugin invocation begins; zero message loss on backend failure
- [ ] File UUIDs are stored in messages.file_ids and forwarded to backend plugins; Chat Engine never fetches, validates, or proxies file content
- [ ] Streaming first byte arrives at client within 200ms of plugin starting to stream; chunk forwarding overhead is under 10ms p95
- [ ] Closing the client HTTP connection cancels the backend plugin stream and saves a partial response with is_complete=false
- [ ] Backend plugin failures for one session_type do not affect sessions on other session_types; plugin-level resilience (circuit breaker, retry) is the plugin's responsibility
- [ ] Message tree constraints reject orphaned parent_message_id values and duplicate variant_index at the database level

## 7. Non-Functional Considerations

- **Performance**: Message routing latency < 100ms p95. Streaming first-byte < 200ms. Chunk forwarding overhead < 10ms p95. Database connection pooling sized for concurrent streaming sessions.
- **Security**: Message content treated as opaque; never logged or inspected. File UUIDs validated as UUID format only; content authorization delegated to File Storage Service.
- **Reliability**: Partial responses persisted with `is_complete=false` on connection drop or cancellation. Plugin failures isolated per session type — other session types unaffected. Plugin implementations own their own resilience patterns (circuit breaker, retry, timeout).
- **Data**: Index on `messages.session_id` for message listing. Index on `messages.parent_message_id` for tree traversal. UNIQUE constraint on `(session_id, parent_message_id, variant_index)`.
- **Observability**: Metrics: `request_duration_seconds`, `plugin_duration_seconds`. Log events for message send, stream start/complete/cancel with `trace_id`.
- **Compliance / UX / Business**: Not applicable — see session-lifecycle §7.