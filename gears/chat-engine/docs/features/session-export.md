Created:  2026-03-20 by Constructor Tech
Updated:  2026-03-20 by Constructor Tech
# Feature: Session Export & Sharing


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Export Session](#export-session)
  - [Create Share Token](#create-share-token)
  - [Access Shared Session](#access-shared-session)
  - [Revoke Share Token](#revoke-share-token)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Build Export Content](#build-export-content)
  - [Upload Export](#upload-export)
  - [Generate Share Token](#generate-share-token)
  - [Validate Share Token](#validate-share-token)
- [4. States (CDSL)](#4-states-cdsl)
  - [Share Token State Machine](#share-token-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Session Export](#session-export)
  - [Share Token Creation](#share-token-creation)
  - [Shared Session Access](#shared-session-access)
  - [Share Token Revocation](#share-token-revocation)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-featstatus-session-export`

## 1. Feature Context

- [ ] `p3` - `cpt-cf-chat-engine-feature-session-export`

### 1.1 Overview

Session export and sharing capabilities: export conversation history (active path) as JSON or Markdown, generate time-limited shareable read-only links backed by cryptographic tokens, and provide token-based read-only access for non-authenticated viewers. Share token lifecycle includes creation, expiry validation, and owner-initiated revocation.

### 1.2 Purpose

Allow end-users to export their conversation history in portable formats for offline use or archival, and to share read-only session views with others via time-limited shareable links without requiring recipient authentication.

Success criteria: Export renders the active message path in the requested format and uploads to file storage within performance bounds; share tokens are cryptographically secure (min 32 chars), time-limited, revocable, and grant read-only access without exposing session_id.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-chat-engine-actor-client` | Exports sessions, creates share tokens, revokes share tokens |
| `cpt-cf-chat-engine-actor-end-user` | Accesses shared session via token (unauthenticated read-only) |
| `cpt-cf-chat-engine-actor-file-storage` | Stores exported session files; returns download URL |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADR**: [ADR-0016: Token-Based Session Sharing](../ADR/0016-session-sharing.md)
- **Dependencies**: `cpt-cf-chat-engine-feature-session-lifecycle`

**Traces to**:
- `cpt-cf-chat-engine-fr-export-session` — export session in JSON or Markdown format
- `cpt-cf-chat-engine-fr-share-session` — generate shareable read-only links
- `cpt-cf-chat-engine-nfr-response-time` — export rendering performance target
- `cpt-cf-chat-engine-nfr-authentication` — share token security, unauthenticated read-only access

## 2. Actor Flows (CDSL)

### Export Session

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-flow-session-export-export`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client requests export; system traverses active message path, renders to requested format, uploads to file storage, and returns download URL

**Error Scenarios**:
- Session not found or not owned by caller (403/404)
- Session has no messages (empty export; 200 with empty content)
- Unsupported format requested (400)
- File storage upload fails (502)

**Steps**:
1. [ ] - `p3` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-exp-auth`
2. [ ] - `p3` - API: invoke export-session endpoint (see `cpt-cf-chat-engine-seq-export-session`) - `inst-exp-api`
3. [ ] - `p3` - Algorithm: validate session ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-exp-ownership`
4. [ ] - `p3` - **IF** format not in (json, markdown) **RETURN** 400 Bad Request (unsupported format) - `inst-exp-check-format`
5. [ ] - `p3` - Algorithm: build export content using `cpt-cf-chat-engine-algo-session-export-build-export` - `inst-exp-build`
6. [ ] - `p3` - Algorithm: upload export to file storage using `cpt-cf-chat-engine-algo-session-export-upload` - `inst-exp-upload`
7. [ ] - `p3` - **RETURN** 200 (download_url, format, message_count, exported_at) - `inst-exp-return`

### Create Share Token

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-flow-session-export-create-share`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client creates a share token for a session; system generates cryptographic token with optional expiry and returns share URL

**Error Scenarios**:
- Session not found or not owned by caller (403/404)
- Session is soft_deleted or hard_deleted (409 Conflict)

**Steps**:
1. [ ] - `p3` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-cs-auth`
2. [ ] - `p3` - API: invoke create-share-token endpoint (see `cpt-cf-chat-engine-seq-share-session`) - `inst-cs-api`
3. [ ] - `p3` - Algorithm: validate session ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-cs-ownership`
4. [ ] - `p3` - **IF** session.lifecycle_state IN (soft_deleted, hard_deleted) **RETURN** 409 Conflict - `inst-cs-check-state`
5. [ ] - `p3` - Algorithm: generate share token using `cpt-cf-chat-engine-algo-session-export-generate-token` - `inst-cs-generate`
6. [ ] - `p3` - DB: persist new share token record (token, session_id, created_by, expires_at) - `inst-cs-db-insert`
7. [ ] - `p3` - **RETURN** 201 Created (share_token, share_url, expires_at) - `inst-cs-return`

### Access Shared Session

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-flow-session-export-access-shared`

**Actor**: `cpt-cf-chat-engine-actor-end-user`

**Success Scenarios**:
- Recipient accesses shared session via token; system returns read-only session data with active path messages

**Error Scenarios**:
- Token not found (404)
- Token expired (410 Gone)
- Token revoked (410 Gone)
- Referenced session is hard_deleted (404)

**Steps**:
1. [ ] - `p3` - API: invoke access-shared-session endpoint (see `cpt-cf-chat-engine-seq-share-session`) - `inst-as-api`
2. [ ] - `p3` - Algorithm: validate share token using `cpt-cf-chat-engine-algo-session-export-validate-token` - `inst-as-validate`
3. [ ] - `p3` - DB: load session record by share_token.session_id - `inst-as-get-session`
4. [ ] - `p3` - **IF** the session lifecycle state is hard_deleted **RETURN** 404 Not Found - `inst-as-check-deleted`
5. [ ] - `p3` - DB: load active-path messages for session, ordered chronologically - `inst-as-get-messages`
6. [ ] - `p3` - **RETURN** 200 (session metadata, messages on active path, read_only=true) - `inst-as-return`

### Revoke Share Token

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-flow-session-export-revoke-share`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Session owner revokes a share token; token is immediately invalidated

**Error Scenarios**:
- Token not found or not associated with a session owned by caller (403/404)
- Token already revoked (no-op, 200)

**Steps**:
1. [ ] - `p3` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-rv-auth`
2. [ ] - `p3` - API: invoke revoke-share-token endpoint (see `cpt-cf-chat-engine-seq-share-session`) - `inst-rv-api`
3. [ ] - `p3` - Algorithm: validate session ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-rv-ownership`
4. [ ] - `p3` - DB: look up share token by token value and session_id - `inst-rv-lookup`
5. [ ] - `p3` - **IF** token not found **RETURN** 404 Not Found - `inst-rv-not-found`
6. [ ] - `p3` - **IF** token already revoked **RETURN** 200 (no-op) - `inst-rv-already-revoked`
7. [ ] - `p3` - DB: set revoked_at timestamp on share token record - `inst-rv-revoke`
8. [ ] - `p3` - **RETURN** 200 (token revoked) - `inst-rv-return`

## 3. Processes / Business Logic (CDSL)

### Build Export Content

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-algo-session-export-build-export`

**Input**: session_id, format (json | markdown)
**Output**: Rendered export content (bytes) and message_count

**Steps**:
1. [ ] - `p3` - DB: load active-path messages for session, ordered chronologically - `inst-be-select`
2. [ ] - `p3` - Filter to active path only: traverse from root following is_active=true nodes - `inst-be-active-path`
3. [ ] - `p3` - **IF** the format is json: serialize messages as JSON array with session metadata envelope - `inst-be-json`
4. [ ] - `p3` - **IF** the format is markdown: render messages as Markdown with role headers and timestamps - `inst-be-markdown`
5. [ ] - `p3` - **RETURN** (rendered_content, message_count) - `inst-be-return`

### Upload Export

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-algo-session-export-upload`

**Input**: rendered_content (bytes), session_id, format
**Output**: download_url or 502 error

**Steps**:
1. [ ] - `p3` - Generate storage key: `exports/{tenant_id}/{session_id}/{timestamp}.{format_extension}` - `inst-up-key`
2. [ ] - `p3` - **TRY** - `inst-up-try`
   1. [ ] - `p3` - Upload rendered_content to file storage at generated key - `inst-up-upload`
   2. [ ] - `p3` - **RETURN** download_url from file storage response - `inst-up-return-url`
3. [ ] - `p3` - **CATCH** storage error - `inst-up-catch`
   1. [ ] - `p3` - **RETURN** 502 Bad Gateway (file storage unavailable) - `inst-up-error`

### Generate Share Token

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-algo-session-export-generate-token`

**Input**: session_id, created_by (user_id), expires_in_hours (optional, default from config)
**Output**: ShareToken record

**Steps**:
1. [ ] - `p3` - Generate cryptographically random token (minimum 32 characters, URL-safe base64) - `inst-gt-generate`
2. [ ] - `p3` - **IF** expires_in_hours provided: compute expires_at = NOW() + expires_in_hours - `inst-gt-expiry-custom`
3. [ ] - `p3` - **IF** expires_in_hours not provided: compute expires_at = NOW() + default_share_ttl from config - `inst-gt-expiry-default`
4. [ ] - `p3` - Build ShareToken: {token, session_id, created_by, created_at=NOW(), expires_at, revoked_at=NULL} - `inst-gt-build`
5. [ ] - `p3` - **RETURN** ShareToken record - `inst-gt-return`

### Validate Share Token

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-algo-session-export-validate-token`

**Input**: token string
**Output**: Validated ShareToken record or 404/410 error

**Steps**:
1. [ ] - `p3` - DB: look up share token by token value - `inst-vt-lookup`
2. [ ] - `p3` - **IF** no row returned **RETURN** 404 Not Found - `inst-vt-not-found`
3. [ ] - `p3` - **IF** share_token.revoked_at IS NOT NULL **RETURN** 410 Gone (token revoked) - `inst-vt-revoked`
4. [ ] - `p3` - **IF** share_token.expires_at < NOW() **RETURN** 410 Gone (token expired) - `inst-vt-expired`
5. [ ] - `p3` - **RETURN** validated ShareToken record - `inst-vt-return`

## 4. States (CDSL)

### Share Token State Machine

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-state-session-export-share-token`

**States**: active, expired, revoked
**Initial State**: active

**Transitions**:
1. [ ] - `p3` - **FROM** active **TO** expired **WHEN** current time exceeds expires_at - `inst-st-to-expired`
2. [ ] - `p3` - **FROM** active **TO** revoked **WHEN** session owner invokes revoke-share-token endpoint - `inst-st-to-revoked`

**Invalid Transitions (rejected or no-op)**:
- **FROM** expired **TO** revoked: Revoking an already-expired token is a no-op (200). The token is already unusable; setting revoked_at has no observable effect.
- **FROM** revoked **TO** expired: Implicit. A revoked token remains revoked regardless of expiry time passing. State is terminal.
- **FROM** revoked **TO** active: Not supported. Revocation is irreversible; a new share token must be created instead.
- **FROM** expired **TO** active: Not supported. Expired tokens cannot be renewed; a new share token must be created instead.
- **Duplicate share creation**: Creating a share token for an already-shared session is allowed — multiple tokens per session are supported (different recipient groups, different expiry times).

## 5. Definitions of Done

### Session Export

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-dod-session-export-export`

The system **MUST** export a session's active message path as JSON or Markdown, upload the rendered content to file storage, and return a download URL to the requesting client. Empty sessions produce a valid export with zero messages.

**Implements**:
- `cpt-cf-chat-engine-flow-session-export-export`
- `cpt-cf-chat-engine-algo-session-export-build-export`
- `cpt-cf-chat-engine-algo-session-export-upload`

**Touches**:
- API: `GET /sessions/{session_id}/export?format=json|markdown`
- DB: `sessions`, `messages`
- Entities: `ExportedSession`

### Share Token Creation

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-dod-session-export-create-share`

The system **MUST** generate a cryptographically secure share token (minimum 32 characters, URL-safe), persist it with an expiry timestamp and creator audit trail, and return a shareable URL. Multiple tokens per session are supported for sharing with different recipient groups.

**Implements**:
- `cpt-cf-chat-engine-flow-session-export-create-share`
- `cpt-cf-chat-engine-algo-session-export-generate-token`

**Touches**:
- API: `POST /sessions/{session_id}/share`
- DB: `share_tokens`
- Entities: `ShareToken`

### Shared Session Access

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-dod-session-export-access-shared`

The system **MUST** provide read-only session access to non-authenticated viewers via a valid, non-expired, non-revoked share token. The session_id is never exposed to the recipient; the token is the sole access credential. Hard-deleted sessions return 404 even with a valid token.

**Implements**:
- `cpt-cf-chat-engine-flow-session-export-access-shared`
- `cpt-cf-chat-engine-algo-session-export-validate-token`

**Touches**:
- API: `GET /sessions/shared/{token}`
- DB: `share_tokens`, `sessions`, `messages`
- Entities: `ShareToken`, `Session`, `Message`

### Share Token Revocation

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-dod-session-export-revoke-share`

The system **MUST** allow the session owner to revoke a share token instantly by setting revoked_at, making all subsequent access attempts return 410 Gone. Revoking an already-revoked token is a no-op returning 200.

**Implements**:
- `cpt-cf-chat-engine-flow-session-export-revoke-share`
- `cpt-cf-chat-engine-algo-session-export-validate-token`

**Touches**:
- API: `DELETE /sessions/{session_id}/share/{token}`
- DB: `share_tokens`
- Entities: `ShareToken`

## 6. Acceptance Criteria

- [ ] Export with format=json returns a valid JSON document containing session metadata and active-path messages ordered chronologically
- [ ] Export with format=markdown returns a readable Markdown document with role headers and message content
- [ ] Export with an unsupported format parameter returns 400 Bad Request
- [ ] Share token is at least 32 characters of cryptographically random URL-safe base64
- [ ] Accessing a shared session via valid token returns read-only session data without requiring authentication
- [ ] Accessing a shared session with an expired token returns 410 Gone
- [ ] Accessing a shared session with a revoked token returns 410 Gone
- [ ] Session owner can revoke any share token for their session; revocation takes effect immediately
- [ ] Share tokens never expose the underlying session_id to recipients
- [ ] Hard-deleted sessions return 404 when accessed via share token
- [ ] Shared session access returns only active-path messages; non-active variants are excluded
- [ ] Concurrent share/revoke operations on the same session are handled safely without race conditions (token creation and revocation use atomic DB operations)

## 7. Non-Functional Considerations

- **Performance**: Export rendering targets < 500ms p95 for sessions with up to 1000 messages. Share token validation uses UNIQUE index for O(1) lookup.
- **Reliability**: Export operations are idempotent — repeated exports produce identical output for the same session state. Share token generation uses cryptographically secure random tokens (CSPRNG) to prevent collision. Token revocation is idempotent (revoking an already-revoked token is a no-op).
- **Security**: Share tokens are cryptographically random (CSPRNG, min 32 chars). Session_id is never exposed via share endpoints. Expired and revoked tokens are indistinguishable to the caller (both return 410) to prevent information leakage. Export download URLs should use time-limited signed URLs from file storage.
- **Data**: UNIQUE index on `share_tokens.token` for O(1) lookup. Index on `share_tokens.session_id` for listing tokens per session. Expired/revoked tokens accumulate and require periodic cleanup (background job or retention policy).
- **Observability**: Structured log events for export (session_id, format, message_count, duration_ms) and share token lifecycle (create, access, revoke) with trace_id.
- **Compliance / UX / Business**: Not applicable -- see session-lifecycle section 7.
