Created:  2026-03-06 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# Feature: Session Lifecycle


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Register Session Type](#register-session-type)
  - [Create Session](#create-session)
  - [Get Sessions](#get-sessions)
  - [Update Session Metadata](#update-session-metadata)
  - [Update Session Capabilities](#update-session-capabilities)
  - [Manage Session Lifecycle](#manage-session-lifecycle)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Authenticate Request](#authenticate-request)
  - [Validate Session Ownership](#validate-session-ownership)
  - [Invoke Backend Plugin](#invoke-backend-plugin)
- [4. States (CDSL)](#4-states-cdsl)
  - [Session State Machine](#session-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Session Type Registration](#session-type-registration)
  - [Session Creation](#session-creation)
  - [Session Retrieval](#session-retrieval)
  - [Session Metadata Update](#session-metadata-update)
  - [Session Lifecycle Operations](#session-lifecycle-operations)
  - [JWT Authentication Middleware](#jwt-authentication-middleware)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-featstatus-session-lifecycle`

## 1. Feature Context

- [ ] `p1` - `cpt-cf-chat-engine-feature-session-lifecycle`

### 1.1 Overview

Foundational service infrastructure for the Chat Engine: HTTP REST API surface with NDJSON streaming, session type registration for developers, session CRUD with full lifecycle state management, JWT-based multi-tenant authentication, and plugin notification on session events. All other Chat Engine features depend on this foundation.

**Traces to**: `cpt-cf-chat-engine-fr-create-session`, `cpt-cf-chat-engine-fr-delete-session`, `cpt-cf-chat-engine-fr-soft-delete-session`, `cpt-cf-chat-engine-fr-hard-delete-session`, `cpt-cf-chat-engine-fr-restore-session`, `cpt-cf-chat-engine-fr-archive-session`, `cpt-cf-chat-engine-nfr-availability`, `cpt-cf-chat-engine-nfr-authentication`, `cpt-cf-chat-engine-nfr-data-persistence`, `cpt-cf-chat-engine-nfr-scalability`, `cpt-cf-chat-engine-nfr-lifecycle-performance`, `cpt-cf-chat-engine-nfr-recovery`, `cpt-cf-chat-engine-nfr-developer-experience`

### 1.2 Purpose

Enable developers to register plugin-backed session types and enable clients to create, retrieve, update, and manage sessions through their full lifecycle (active → archived / soft_deleted / hard_deleted → restored).

Success criteria: Sessions are created, retrieved, updated, and deleted within lifecycle-performance bounds, with all requests authenticated, tenant-isolated, and durably persisted.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-chat-engine-actor-developer` | Registers session types with plugin backend configuration |
| `cpt-cf-chat-engine-actor-client` | Creates and manages sessions; updates metadata; controls lifecycle |
| `cpt-cf-chat-engine-actor-backend-plugin` | Receives session.created and session.deleted events; returns capabilities |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Dependencies**: None (this is the foundational feature)

## 2. Actor Flows (CDSL)

### Register Session Type

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-session-lifecycle-register-type`

**Actor**: `cpt-cf-chat-engine-actor-developer`

**Success Scenarios**:
- Developer submits valid config; session type is stored and returned with generated ID

**Error Scenarios**:
- plugin_instance_id not found in ClientHub (plugin not registered or not loaded)

**Steps**:
1. [ ] - `p1` - Algorithm: authenticate developer request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-reg-auth`
2. [ ] - `p1` - API: POST /session-types (body: name, plugin_instance_id, metadata) - `inst-reg-api`
3. [ ] - `p1` - Validate plugin_instance_id: resolve via ClientHub — `hub.get_scoped::<dyn ChatEngineBackendPlugin>(&scope)` must succeed - `inst-reg-validate-plugin`
4. [ ] - `p1` - Call `plugin.on_session_type_configured(ctx)` — plugin validates metadata; may return static capabilities - `inst-reg-call-plugin`
5. [ ] - `p1` - DB: Create a new session_types record with name, plugin_instance_id, metadata, and available_capabilities - `inst-reg-db-insert`
6. [ ] - `p1` - **RETURN** 201 Created (session_type_id) - `inst-reg-return`

### Create Session

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-session-lifecycle-create-session`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client creates session; plugin returns capabilities; session record persisted with tenant binding

**Error Scenarios**:
- Requested session_type_id does not exist
- Backend plugin unavailable on session.created call

**Steps**:
1. [ ] - `p1` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-create-auth`
2. [ ] - `p1` - API: POST /sessions (body: session_type_id, metadata?) - `inst-create-api`
3. [ ] - `p1` - DB: Load the session type record by session_type_id - `inst-create-get-type`
4. [ ] - `p1` - **IF** session type not found **RETURN** 404 Not Found - `inst-create-not-found`
5. [ ] - `p1` - DB: Create a new session record with session_type_id, tenant_id, user_id, client_id, metadata, and lifecycle_state=active - `inst-create-db-insert`
6. [ ] - `p1` - Algorithm: resolve plugin and call `on_session_created` using `cpt-cf-chat-engine-algo-session-lifecycle-invoke-plugin` — plugin queries Model Registry for available models and default model capabilities and returns a list of capabilities - `inst-create-notify`
7. [ ] - `p1` - DB: Update the session's enabled_capabilities, identified by session_id - `inst-create-store-caps`
8. [ ] - `p1` - **RETURN** 201 Created (session_id, lifecycle_state=active, enabled_capabilities) - `inst-create-return`

### Get Sessions

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-session-lifecycle-get-sessions`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client retrieves paginated list or single session, scoped to their tenant and user identity

**Error Scenarios**:
- Session ID does not exist (404)
- Session belongs to different user or tenant (403)

**Steps**:
1. [ ] - `p1` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-get-auth`
2. [ ] - `p1` - **IF** listing all sessions: API GET /sessions (query: cursor?, limit?) - `inst-get-list-api`
   1. [ ] - `p1` - DB: Fetch sessions for the given tenant_id and user_id, excluding hard_deleted sessions, ordered by created_at descending. Results are paginated using cursor-based pagination with configurable page size (default: 20, max: 100). The cursor is an opaque token encoding the last seen created_at + session_id for stable pagination - `inst-get-list-db`
   2. [ ] - `p1` - **RETURN** 200 (sessions[], next_cursor, has_more) - `inst-get-list-return`
3. [ ] - `p1` - **IF** getting single session: API GET /sessions/{session_id} - `inst-get-one-api`
   1. [ ] - `p1` - Algorithm: validate ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-get-ownership`
   2. [ ] - `p1` - **RETURN** 200 (session) - `inst-get-one-return`

### Update Session Metadata

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-session-lifecycle-update-metadata`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client updates metadata on an active or archived session

**Error Scenarios**:
- Session is soft_deleted or hard_deleted (409 Conflict)
- Session not found or owned by another user (403/404)

**Steps**:
1. [ ] - `p1` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-upd-auth`
2. [ ] - `p1` - API: PATCH /sessions/{session_id} (body: metadata) - `inst-upd-api`
3. [ ] - `p1` - Algorithm: validate ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-upd-ownership`
4. [ ] - `p1` - **IF** session.lifecycle_state IN (soft_deleted, hard_deleted) **RETURN** 409 Conflict - `inst-upd-check-state`
5. [ ] - `p1` - DB: Update the session's metadata and refresh updated_at, identified by session_id - `inst-upd-db`
6. [ ] - `p1` - **RETURN** 200 (updated session) - `inst-upd-return`

### Update Session Capabilities

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-session-lifecycle-update-capabilities`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client updates capability values (e.g., selects a different model); plugin re-resolves capabilities from Model Registry; session capabilities are overwritten

**Error Scenarios**:
- Session is soft_deleted or hard_deleted (409 Conflict)
- Session not found or owned by another user (403/404)
- Plugin fails to resolve capabilities from Model Registry (502 Bad Gateway)

**Steps**:
1. [ ] - `p1` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-cap-auth`
2. [ ] - `p1` - API: PATCH /sessions/{session_id} (body: enabled_capabilities: CapabilityValue[]) - `inst-cap-api`
3. [ ] - `p1` - Algorithm: validate ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-cap-ownership`
4. [ ] - `p1` - **IF** session.lifecycle_state IN (soft_deleted, hard_deleted) **RETURN** 409 Conflict - `inst-cap-check-state`
5. [ ] - `p1` - Resolve plugin by session_type.plugin_instance_id - `inst-cap-resolve-plugin`
6. [ ] - `p1` - Call the plugin's `on_session_updated` hook with the updated capability values — the plugin queries Model Registry for capabilities of the newly selected model and returns a list of capabilities - `inst-cap-call-plugin`
7. [ ] - `p1` - DB: Update the session's enabled_capabilities and refresh updated_at, identified by session_id - `inst-cap-db`
8. [ ] - `p1` - **RETURN** 200 (updated session with refreshed enabled_capabilities) - `inst-cap-return`

### Manage Session Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-session-lifecycle-manage-lifecycle`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client transitions session through valid lifecycle states (archive, soft-delete, restore, hard-delete)

**Error Scenarios**:
- Requested transition is invalid per state machine (422)
- Plugin notification fails on soft-delete (502)

**Steps**:
1. [ ] - `p1` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-lc-auth`
2. [ ] - `p1` - Algorithm: validate ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-lc-ownership`
3. [ ] - `p1` - State machine: validate requested transition using `cpt-cf-chat-engine-state-session-lifecycle-session` - `inst-lc-validate-transition`
4. [ ] - `p1` - **IF** operation is soft-delete (DELETE /sessions/{id}): set lifecycle_state=soft_deleted - `inst-lc-soft-delete`
   1. [ ] - `p1` - Algorithm: notify backend plugin with session.deleted event using `cpt-cf-chat-engine-algo-session-lifecycle-invoke-plugin` - `inst-lc-notify-delete`
5. [ ] - `p1` - **IF** operation is hard-delete (DELETE /sessions/{id}?hard=true): physically remove session row and cascade delete messages - `inst-lc-hard-delete`
6. [ ] - `p2` - **IF** operation is archive (POST /sessions/{id}/archive): **IF** lifecycle_state != 'active' **RETURN** 422 (archive only from active state); set lifecycle_state=archived - `inst-lc-archive`
7. [ ] - `p2` - **IF** operation is restore (POST /sessions/{id}/restore): **IF** lifecycle_state NOT IN ('archived', 'soft_deleted') **RETURN** 422 (restore only from archived or soft_deleted state); set lifecycle_state=active - `inst-lc-restore`
8. [ ] - `p1` - **RETURN** 200 (updated session) or 204 No Content for hard-delete - `inst-lc-return`

## 3. Processes / Business Logic (CDSL)

### Authenticate Request

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-session-lifecycle-authenticate`

**Input**: HTTP request with Authorization header
**Output**: Verified identity (tenant_id, user_id, client_id) or 401 error

**Steps**:
1. [ ] - `p1` - Extract bearer token from HTTP Authorization header - `inst-auth-extract`
2. [ ] - `p1` - **IF** Authorization header missing or scheme is not Bearer **RETURN** 401 Unauthorized - `inst-auth-check-header`
3. [ ] - `p1` - Verify JWT signature using configured public key and check token expiry - `inst-auth-verify-sig`
4. [ ] - `p1` - **IF** signature invalid or token expired **RETURN** 401 Unauthorized - `inst-auth-check-validity`
5. [ ] - `p1` - Extract tenant_id, user_id, client_id claims from token payload - `inst-auth-extract-claims`
6. [ ] - `p1` - **IF** tenant_id or user_id claim is absent **RETURN** 401 Unauthorized (malformed token) - `inst-auth-check-claims`
7. [ ] - `p1` - **RETURN** identity (tenant_id, user_id, client_id) for downstream scoping - `inst-auth-return`

### Validate Session Ownership

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership`

**Input**: session_id, request identity (tenant_id, user_id)
**Output**: Session record or 403/404 error

**Steps**:
1. [ ] - `p1` - DB: Load the session record (session_id, tenant_id, user_id, lifecycle_state) by session_id - `inst-own-db-get`
2. [ ] - `p1` - **IF** no row returned **RETURN** 404 Not Found - `inst-own-not-found`
3. [ ] - `p1` - **IF** the session lifecycle state is hard_deleted **RETURN** 404 Not Found (hard-deleted sessions are invisible) - `inst-own-hard-deleted`
4. [ ] - `p1` - **IF** session.tenant_id != request.tenant_id **RETURN** 403 Forbidden - `inst-own-check-tenant`
5. [ ] - `p1` - **IF** session.user_id != request.user_id **RETURN** 403 Forbidden - `inst-own-check-user`
6. [ ] - `p1` - **RETURN** session record for caller use - `inst-own-return`

### Invoke Backend Plugin

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-session-lifecycle-invoke-plugin`

**Input**: event_type, session record, session_type_id
**Output**: `Vec<Capability>` (for on_session_created) or void; 502 on on_session_created failure

**Steps**:
1. [ ] - `p1` - DB: Load the plugin_instance_id from the session type record, then load the plugin config for the given plugin_instance_id and session_type_id - `inst-ntfy-load-config`
2. [ ] - `p1` - Resolve plugin: `hub.get_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(&plugin_instance_id))` - `inst-ntfy-resolve-plugin`
3. [ ] - `p1` - Build ctx: {event_type, session_id, session_type_id, plugin_config, tenant_id, user_id, client_id, metadata, timestamp} - `inst-ntfy-build-ctx`
4. [ ] - `p1` - **TRY** - `inst-ntfy-try`
   1. [ ] - `p1` - Call the plugin's `on_session_created` hook — the plugin queries Model Registry for the available models list and default model capabilities and returns a list of capabilities - `inst-ntfy-call`
5. [ ] - `p1` - **CATCH** plugin error - `inst-ntfy-catch`
   1. [ ] - `p1` - **IF** the event type is on_session_created **RETURN** 502 Bad Gateway - `inst-ntfy-created-fail`
   2. [ ] - `p2` - **IF** event_type != on_session_created: log warning and continue (fire-and-forget) - `inst-ntfy-other-fail`
6. [ ] - `p1` - **IF** the event type is on_session_created **RETURN** the list of capabilities from the plugin response - `inst-ntfy-return-caps`
7. [ ] - `p1` - **RETURN** void for all other event types - `inst-ntfy-return-void`

## 4. States (CDSL)

### Session State Machine

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-state-session-lifecycle-session`

**States**: active, archived, soft_deleted, hard_deleted
**Initial State**: active

**Transitions**:
1. [ ] - `p1` - **FROM** active **TO** archived **WHEN** client calls POST /sessions/{id}/archive - `inst-st-to-archived`
2. [ ] - `p1` - **FROM** active **TO** soft_deleted **WHEN** client calls DELETE /sessions/{id} without ?hard=true - `inst-st-to-soft-deleted`
3. [ ] - `p1` - **FROM** active **TO** hard_deleted **WHEN** client calls DELETE /sessions/{id}?hard=true - `inst-st-active-to-hard`
4. [ ] - `p2` - **FROM** archived **TO** active **WHEN** client calls POST /sessions/{id}/restore - `inst-st-archived-to-active`
5. [ ] - `p2` - **FROM** archived **TO** soft_deleted **WHEN** client calls DELETE /sessions/{id} - `inst-st-archived-to-soft`
6. [ ] - `p2` - **FROM** archived **TO** hard_deleted **WHEN** client calls DELETE /sessions/{id}?hard=true — enables direct permanent removal of archived sessions without requiring intermediate soft-delete - `inst-st-archived-to-hard`
7. [ ] - `p2` - **FROM** soft_deleted **TO** active **WHEN** client calls POST /sessions/{id}/restore - `inst-st-soft-to-active`
8. [ ] - `p1` - **FROM** soft_deleted **TO** hard_deleted **WHEN** client calls DELETE /sessions/{id}?hard=true - `inst-st-to-hard-deleted`

## 5. Definitions of Done

### Session Type Registration

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-session-lifecycle-type-registration`

The system **MUST** allow developers to register session types with a name, plugin_instance_id, and optional metadata, persisting the configuration and optionally probing backend availability before storing.

**Implements**:
- `cpt-cf-chat-engine-flow-session-lifecycle-register-type`
- `cpt-cf-chat-engine-algo-session-lifecycle-invoke-plugin`

**Touches**:
- API: `POST /session-types`, `GET /session-types`
- DB: `session_types`
- Entities: `SessionType`

### Session Creation

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-session-lifecycle-create`

The system **MUST** create sessions bound to the requesting user's JWT identity (tenant_id, user_id, client_id), invoke the backend plugin with on_session_created, persist returned capabilities, and return the session record in a single atomic operation.

**Implements**:
- `cpt-cf-chat-engine-flow-session-lifecycle-create-session`
- `cpt-cf-chat-engine-algo-session-lifecycle-authenticate`
- `cpt-cf-chat-engine-algo-session-lifecycle-invoke-plugin`

**Touches**:
- API: `POST /sessions`
- DB: `sessions`, `session_types`
- Entities: `Session`

### Session Retrieval

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-session-lifecycle-retrieval`

The system **MUST** return sessions scoped by tenant_id and user_id from JWT claims; hard-deleted sessions must be invisible; ownership validation must precede all reads.

**Implements**:
- `cpt-cf-chat-engine-flow-session-lifecycle-get-sessions`
- `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership`

**Touches**:
- API: `GET /sessions`, `GET /sessions/{session_id}`
- DB: `sessions`
- Entities: `Session`

### Session Metadata Update

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-session-lifecycle-metadata`

The system **MUST** update session metadata via PATCH, blocking updates on soft_deleted and hard_deleted sessions, with ownership validation before any write.

**Implements**:
- `cpt-cf-chat-engine-flow-session-lifecycle-update-metadata`
- `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership`

**Touches**:
- API: `PATCH /sessions/{session_id}`
- DB: `sessions`
- Entities: `Session`

### Session Lifecycle Operations

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-session-lifecycle-lifecycle-ops`

The system **MUST** transition sessions through the Session State Machine (soft-delete, hard-delete, archive, restore), notifying the backend plugin with session.deleted event on soft-delete, and physically removing the session row and all its messages on hard-delete.

**Implements**:
- `cpt-cf-chat-engine-flow-session-lifecycle-manage-lifecycle`
- `cpt-cf-chat-engine-state-session-lifecycle-session`
- `cpt-cf-chat-engine-algo-session-lifecycle-invoke-plugin`

**Touches**:
- API: `DELETE /sessions/{id}`, `DELETE /sessions/{id}?hard=true`, `POST /sessions/{id}/archive`, `POST /sessions/{id}/restore`
- DB: `sessions`, `messages` (cascade delete on hard-delete)
- Entities: `Session`

### JWT Authentication Middleware

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-session-lifecycle-jwt-auth`

The system **MUST** authenticate every HTTP REST request via JWT bearer token, extract tenant_id, user_id, and client_id server-side (never from request body), and enforce tenant isolation on all data queries. A health check endpoint at GET /health must be available without authentication for load balancer probes.

**Implements**:
- `cpt-cf-chat-engine-algo-session-lifecycle-authenticate`

**Touches**:
- API: all REST endpoints, `GET /health`
- DB: `sessions` (tenant_id scope on all queries)
- Entities: JWT claims (tenant_id, user_id, client_id)

## 6. Acceptance Criteria

- [ ] JWT authentication is enforced on all REST endpoints; requests without valid bearer tokens receive 401
- [ ] Session records always contain tenant_id and user_id extracted from JWT; these fields are never accepted from the request body
- [ ] All session queries are scoped by tenant_id from JWT; cross-tenant access returns 403
- [ ] Session lifecycle transitions follow the Session State Machine; invalid transitions return 422
- [ ] Session soft-delete triggers a session.deleted plugin notification; hard-delete permanently removes the session and all its messages
- [ ] Health check endpoint GET /health responds 200 OK without authentication

## 7. Non-Functional Considerations

- **Performance**: Session CRUD operations target < 50ms p95 latency. Database queries scoped by tenant_id + user_id use composite index.
- **Security**: JWT validation on every request. 404 (not 403) returned for sessions belonging to other users to prevent session ID enumeration. Token expiry enforced; no refresh within Chat Engine.
- **Data**: Composite index on `(tenant_id, user_id)` for session listing. `share_token` column has UNIQUE index for O(1) lookup.
- **Observability**: Structured log events for session create/delete/archive/restore with `trace_id`, `session_id`, `operation`, `duration_ms`.
- **Compliance**: Not applicable — compliance is addressed at PRD level; Chat Engine provides technical mechanisms (soft/hard delete, retention) for data controller obligations.
- **UX / Accessibility**: Not applicable — backend API service; UX is client application responsibility.
- **Business**: Not applicable — business alignment tracked via PRD traceability.