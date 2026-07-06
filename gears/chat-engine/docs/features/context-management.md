Created:  2026-03-20 by Constructor Tech
Updated:  2026-03-20 by Constructor Tech
# Feature: Context & Memory Management


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Configure Memory Strategy](#configure-memory-strategy)
  - [Build Context for Plugin Invocation](#build-context-for-plugin-invocation)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Validate Memory Strategy](#validate-memory-strategy)
  - [Extract Active Path](#extract-active-path)
  - [Apply Memory Strategy](#apply-memory-strategy)
  - [Handle Context Overflow](#handle-context-overflow)
- [4. States (CDSL)](#4-states-cdsl)
  - [Memory Strategy State](#memory-strategy-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Memory Strategy Configuration](#memory-strategy-configuration)
  - [Active-Path Context Construction](#active-path-context-construction)
  - [Context Overflow Handling](#context-overflow-handling)
  - [Default Strategy Behavior](#default-strategy-behavior)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-featstatus-context-management`

## 1. Feature Context

- [ ] `p2` - `cpt-cf-chat-engine-feature-context-management`

### 1.1 Overview

Configurable per-session memory strategies that control which portion of the message tree is sent as conversation history to backend plugins. Implements three strategies (full history, sliding window, AI-summarized), active-path extraction for context payload construction, and graceful context overflow detection with configurable degradation behavior. Memory strategy configuration is persisted in session metadata and applied transparently during every plugin invocation.

**Traces to**: `cpt-cf-chat-engine-fr-conversation-memory`, `cpt-cf-chat-engine-fr-context-overflow`, `cpt-cf-chat-engine-nfr-message-history`, `cpt-cf-chat-engine-nfr-response-time`

### 1.2 Purpose

Enable operators and clients to control conversation memory behavior per session, balancing context richness against backend token limits. This feature replaces the fixed-depth history approach in Message Processing with a pluggable strategy that adapts to session characteristics and backend constraints.

Success criteria: Context payloads are constructed correctly for each strategy; overflow is detected and handled within a single message round-trip; strategy changes take effect on the next plugin invocation without session restart.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-chat-engine-actor-client` | Configures memory strategy per session; sends messages that trigger context construction |
| `cpt-cf-chat-engine-actor-backend-plugin` | Receives context payload; signals context_overflow when history exceeds model limits |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADR**: [ADR-0023 LLM Gateway Plugin](../ADR/0023-llm-gateway-plugin.md) (context overflow, summarization, is_hidden_from_backend)
- **Dependencies**: `cpt-cf-chat-engine-feature-message-processing`

## 2. Actor Flows (CDSL)

### Configure Memory Strategy

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-flow-context-management-configure-strategy`

**Actor**: `cpt-cf-chat-engine-actor-client`

**Success Scenarios**:
- Client sets memory strategy on an active session; strategy is persisted and takes effect on next plugin invocation

**Error Scenarios**:
- Session not found or not owned by caller (403/404)
- Session is not in active or archived lifecycle state (409)
- Invalid strategy type or missing required parameters (400)

**Steps**:
1. [ ] - `p2` - Algorithm: authenticate request using `cpt-cf-chat-engine-algo-session-lifecycle-authenticate` - `inst-cfg-auth`
2. [ ] - `p2` - API: PATCH /sessions/{session_id} (body: `memory_strategy: { "type": "full" | "sliding_window" | "summarized", <type-specific fields> }`; type-specific fields are flattened next to `"type"` — no nested `config` object). Examples: `{"type": "full"}`, `{"type": "sliding_window", "window_size": 10}`, `{"type": "summarized", "recent_messages_to_keep": 5}` - `inst-cfg-api`
3. [ ] - `p2` - Algorithm: validate session ownership using `cpt-cf-chat-engine-algo-session-lifecycle-validate-ownership` - `inst-cfg-ownership`
4. [ ] - `p2` - **IF** session.lifecycle_state IN (soft_deleted, hard_deleted) **RETURN** 409 Conflict - `inst-cfg-check-state`
5. [ ] - `p2` - Algorithm: validate strategy using `cpt-cf-chat-engine-algo-context-management-validate-strategy` - `inst-cfg-validate`
6. [ ] - `p2` - DB: Update the session's metadata to set the memory_strategy field and refresh updated_at, identified by session_id - `inst-cfg-db`
7. [ ] - `p2` - **RETURN** 200 (updated session with memory_strategy) - `inst-cfg-return`

### Build Context for Plugin Invocation

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-flow-context-management-build-context`

**Actor**: `cpt-cf-chat-engine-actor-client` (triggered indirectly via message send)

**Success Scenarios**:
- Context payload is constructed according to the session's memory strategy and forwarded to the backend plugin

**Error Scenarios**:
- Backend plugin returns context_overflow after context construction (triggers overflow handling)

**Steps**:
1. [ ] - `p2` - Algorithm: extract active path using `cpt-cf-chat-engine-algo-context-management-extract-active-path` - `inst-bc-extract`
2. [ ] - `p2` - Algorithm: apply memory strategy using `cpt-cf-chat-engine-algo-context-management-apply-strategy` - `inst-bc-apply`
3. [ ] - `p2` - Algorithm: invoke backend plugin using `cpt-cf-chat-engine-algo-message-processing-invoke-plugin` with constructed context - `inst-bc-invoke`
4. [ ] - `p2` - **IF** plugin returns context_overflow: Algorithm: handle overflow using `cpt-cf-chat-engine-algo-context-management-handle-overflow` - `inst-bc-overflow`
5. [ ] - `p2` - **RETURN** streaming response handle to caller - `inst-bc-return`

## 3. Processes / Business Logic (CDSL)

### Validate Memory Strategy

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-context-management-validate-strategy`

**Input**: memory_strategy object — internally-tagged on `"type"` with type-specific fields flattened at the top level (no nested `config` wrapper). Matches the SDK `MemoryStrategy` enum (`chat-engine-sdk::models::MemoryStrategy`).
**Output**: Validated strategy or 400 error

**Steps**:
1. [ ] - `p2` - **IF** `type` NOT IN ('full', 'sliding_window', 'summarized') **RETURN** 400 Bad Request (unknown strategy type) - `inst-vs-check-type`
2. [ ] - `p2` - **IF** `type` == 'sliding_window' AND top-level `window_size` is absent or < 1 **RETURN** 400 Bad Request (window_size required and must be >= 1) - `inst-vs-check-window`
3. [ ] - `p2` - **IF** `type` == 'summarized' AND top-level `recent_messages_to_keep` is absent or < 2 **RETURN** 400 Bad Request (recent_messages_to_keep required and must be >= 2) - `inst-vs-check-summarized`
4. [ ] - `p2` - **IF** `type` == 'full': no additional fields required - `inst-vs-full-noop`
5. [ ] - `p2` - **RETURN** validated strategy - `inst-vs-return`

### Extract Active Path

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-context-management-extract-active-path`

**Input**: session_id
**Output**: Ordered list of active-path messages (root to leaf)

**Steps**:
1. [ ] - `p2` - DB: Fetch all messages for the given session where is_active=true and is_hidden_from_backend=false, ordered by created_at ascending - `inst-eap-select`
2. [ ] - `p2` - **IF** no messages found **RETURN** empty list - `inst-eap-empty`
3. [ ] - `p2` - Map each message to history entry: {message_id, role, content, file_ids, metadata} - `inst-eap-map`
4. [ ] - `p2` - **RETURN** ordered active-path message list - `inst-eap-return`

### Apply Memory Strategy

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-context-management-apply-strategy`

**Input**: active-path messages, session memory_strategy, current user message
**Output**: Context payload (messages array for plugin invocation)

**Steps**:
1. [ ] - `p2` - Load memory_strategy from session metadata; default to 'full' if not set - `inst-as-load`
2. [ ] - `p2` - **IF** strategy.type == 'full': context = all active-path messages + current user message - `inst-as-full`
3. [ ] - `p2` - **IF** strategy.type == 'sliding_window': context = last N messages from active path (where N = strategy.window_size, read from the flat top-level field) + current user message - `inst-as-window`
4. [ ] - `p2` - **IF** strategy.type == 'summarized': context = messages with is_hidden_from_backend=false from active path + current user message (summary messages included, summarized originals excluded by visibility flag); retain the last `strategy.recent_messages_to_keep` messages from the active path regardless of visibility flags to ensure recent context is always available - `inst-as-summarized`
5. [ ] - `p2` - **RETURN** context payload as ordered messages array - `inst-as-return`

### Handle Context Overflow

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-context-management-handle-overflow`

**Input**: session_id, context_overflow error from plugin, current memory_strategy
**Output**: Retry result (streaming response handle) or error propagated to client

**Steps**:
1. [ ] - `p2` - **IF** strategy.type == 'summarized': invoke `on_session_summary` via backend plugin with full visible history (as defined in ADR-0023) - `inst-ho-summarize`
   1. [ ] - `p2` - **IF** plugin returns SummaryResult: persist summary message (role=system, is_hidden_from_user=true), mark summarized_message_ids with is_hidden_from_backend=true - `inst-ho-persist-summary`
   2. [ ] - `p2` - Rebuild context using `cpt-cf-chat-engine-algo-context-management-apply-strategy` with updated visibility - `inst-ho-rebuild`
   3. [ ] - `p2` - Retry plugin invocation using `cpt-cf-chat-engine-algo-message-processing-invoke-plugin` with rebuilt context - `inst-ho-retry`
   4. [ ] - `p2` - **IF** retry returns context_overflow again **RETURN** error to client (context still exceeds limit after summarization) - `inst-ho-retry-fail`
2. [ ] - `p2` - **IF** strategy.type == 'sliding_window': **RETURN** error to client (sliding window does not support automatic recovery; client should reduce window_size) - `inst-ho-window-fail`
3. [ ] - `p2` - **IF** strategy.type == 'full': **RETURN** error to client (full history overflow; client should switch to sliding_window or summarized strategy) - `inst-ho-full-fail`
4. [ ] - `p2` - **RETURN** result (streaming response handle on success, or error propagated to client) - `inst-ho-return`

## 4. States (CDSL)

### Memory Strategy State

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-state-context-management-strategy`

**States**: full, sliding_window, summarized
**Initial State**: full (default when memory_strategy is not explicitly set)

**Transitions**:
1. [ ] - `p2` - **FROM** full **TO** sliding_window **WHEN** client sets memory_strategy.type='sliding_window' via PATCH /sessions/{session_id} - `inst-st-full-to-window`
2. [ ] - `p2` - **FROM** full **TO** summarized **WHEN** client sets memory_strategy.type='summarized' via PATCH /sessions/{session_id} - `inst-st-full-to-summarized`
3. [ ] - `p2` - **FROM** sliding_window **TO** full **WHEN** client sets memory_strategy.type='full' via PATCH /sessions/{session_id} - `inst-st-window-to-full`
4. [ ] - `p2` - **FROM** sliding_window **TO** summarized **WHEN** client sets memory_strategy.type='summarized' via PATCH /sessions/{session_id} - `inst-st-window-to-summarized`
5. [ ] - `p2` - **FROM** summarized **TO** full **WHEN** client sets memory_strategy.type='full' via PATCH /sessions/{session_id} - `inst-st-summarized-to-full`
6. [ ] - `p2` - **FROM** summarized **TO** sliding_window **WHEN** client sets memory_strategy.type='sliding_window' via PATCH /sessions/{session_id} - `inst-st-summarized-to-window`

## 5. Definitions of Done

### Memory Strategy Configuration

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-context-management-strategy-config`

The system **MUST** allow clients to set a memory strategy (full, sliding_window, or summarized) on any active or archived session via PATCH /sessions/{session_id}, persisting the configuration in session metadata and applying it on the next plugin invocation.

**Implements**:
- `cpt-cf-chat-engine-flow-context-management-configure-strategy`
- `cpt-cf-chat-engine-algo-context-management-validate-strategy`
- `cpt-cf-chat-engine-state-context-management-strategy`

**Touches**:
- API: `PATCH /sessions/{session_id}` (memory_strategy field)
- DB: `sessions.metadata`
- Entities: `MemoryStrategy`

### Active-Path Context Construction

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-context-management-active-path`

The system **MUST** extract the active path from the message tree (is_active=true, is_hidden_from_backend=false) and apply the configured memory strategy to construct the context payload forwarded to backend plugins on every message send and recreate operation.

**Implements**:
- `cpt-cf-chat-engine-flow-context-management-build-context`
- `cpt-cf-chat-engine-algo-context-management-extract-active-path`
- `cpt-cf-chat-engine-algo-context-management-apply-strategy`

**Touches**:
- DB: `messages` (active path query)
- Entities: `ContextWindow`, `Message`

### Context Overflow Handling

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-context-management-overflow`

The system **MUST** detect context_overflow errors from backend plugins and handle them according to the session's memory strategy: for summarized strategy, trigger on_session_summary via the backend plugin, persist the summary, and retry; for full and sliding_window strategies, propagate the error to the client. A second overflow after summarization retry is propagated to the client.

**Implements**:
- `cpt-cf-chat-engine-algo-context-management-handle-overflow`

**Touches**:
- DB: `messages` (summary message creation, is_hidden_from_backend updates)
- Entities: `Message`, `SummaryResult`

### Default Strategy Behavior

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-context-management-default`

The system **MUST** default to full history strategy when a session has no explicit memory_strategy set, preserving backward compatibility with sessions created before this feature is deployed.

**Implements**:
- `cpt-cf-chat-engine-algo-context-management-apply-strategy`

**Touches**:
- DB: `sessions.metadata`
- Entities: `MemoryStrategy`

## 6. Acceptance Criteria

- [ ] Sessions without an explicit memory_strategy default to full history; all active-path messages with is_hidden_from_backend=false are sent to the backend plugin
- [ ] Setting memory_strategy to sliding_window with window_size=10 sends only the 10 most recent active-path messages plus the current user message to the backend plugin
- [ ] Setting memory_strategy to summarized includes summary messages in the context payload and excludes messages marked is_hidden_from_backend=true
- [ ] Invalid strategy configurations (unknown type, missing window_size, recent_messages_to_keep < 2) return 400 Bad Request
- [ ] When a backend plugin returns context_overflow on a session with summarized strategy, on_session_summary is called, the summary is persisted, summarized messages are hidden from backend, and the original request is retried with the reduced context
- [ ] When a backend plugin returns context_overflow on a session with full or sliding_window strategy, the error is propagated to the client
- [ ] Memory strategy changes via PATCH /sessions/{session_id} take effect on the next plugin invocation without requiring session restart
- [ ] Strategy transitions between all three types (full, sliding_window, summarized) are permitted on active and archived sessions; soft_deleted and hard_deleted sessions return 409
- [ ] Context strategy changes are applied atomically — no partial strategy state is persisted

## 7. Non-Functional Considerations

- **Performance**: Active-path extraction query targets < 20ms p95 using existing index on messages.session_id. Sliding window truncation is applied in-memory after active-path extraction; a future optimization may push the LIMIT into the SQL query to avoid loading the full message history for large sessions. Context construction adds < 5ms overhead to the message processing pipeline.
- **Security**: Memory strategy is a session-level configuration scoped by the same ownership and tenant isolation rules as other session metadata. No new authentication surface.
- **Reliability**: Default full strategy ensures no behavioral change for existing sessions. Overflow handling is idempotent: repeated context_overflow on the same request produces the same summarization result (single retry, then propagate).
- **Data**: Memory strategy persisted in sessions.metadata JSONB field; no schema migration required. Summary messages use existing messages table columns (is_hidden_from_user, is_hidden_from_backend).
- **Observability**: Structured log events for strategy changes (session_id, old_strategy, new_strategy) and overflow handling (session_id, strategy_type, summarization_triggered, retry_result). Metric: `context_overflow_total` counter by session_type_id and strategy_type.
- **Compliance / UX / Business**: Not applicable -- defers to session-lifecycle feature section 7 for compliance, UX, and business considerations, as context management inherits those cross-cutting concerns from the foundational session feature.