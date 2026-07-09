Created:  2026-03-06 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech

# Decomposition: Chat Engine



<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Session Lifecycle 🔄 HIGH](#21-session-lifecycle--high)
  - [2.2 Message Processing & Streaming 🔄 HIGH](#22-message-processing--streaming--high)
  - [2.3 Message Variants & Branching 🔄 MEDIUM](#23-message-variants--branching--medium)
  - [2.4 Context & Memory Management 🔄 MEDIUM](#24-context--memory-management--medium)
  - [2.5 Session Intelligence 🔄 MEDIUM](#25-session-intelligence--medium)
  - [2.6 Message Reactions & Feedback 🔄 MEDIUM](#26-message-reactions--feedback--medium)
  - [2.7 Session Export & Sharing 🔄 LOW](#27-session-export--sharing--low)
  - [2.8 Message Search 🔄 LOW](#28-message-search--low)
  - [2.9 Plugin System Infrastructure 🔄 HIGH](#29-plugin-system-infrastructure--high)
  - [2.10 LLM Gateway Plugin 🔄 HIGH](#210-llm-gateway-plugin--high)
  - [2.11 Authorization (PDP/PEP + SecureORM) 🔄 HIGH](#211-authorization-pdppep--secureorm--high)
- [3. Deliberate Omissions](#3-deliberate-omissions)
- [4. Feature Dependencies](#4-feature-dependencies)

<!-- /toc -->

## 1. Overview

The Chat Engine design is decomposed into 11 features organized around functional cohesion. The decomposition follows a strict dependency order: session infrastructure enables plugin system and message processing, which enable all higher-level capabilities; authorization is layered as a cross-cutting PEP feature over the sessions/messages/reactions resources once those exist.

**Decomposition Strategy**:

- Features grouped by domain cohesion: session lifecycle, plugin system, message core, tree operations, context window, session intelligence, reactions, export, search, LLM gateway plugin, and authorization
- Dependencies minimize coupling — each feature is fully implementable given its declared dependencies
- `cpt-cf-chat-engine-component-message-processing` is intentionally shared by features 03–05 because those features extend the message processing pipeline with distinct, independently testable capabilities (tree traversal, context selection, and summarization routing); each sharing is documented in the feature's Scope and Out of Scope sections
- 100% coverage of all DESIGN components, sequences, domain entities, DB tables, principles, and constraints verified, including the reworked §3.5 authorization elements (`cpt-cf-chat-engine-adr-authz-pep-secureorm` / `ADR/0028`)
- NFR `nfr-backend-isolation` is shared between Feature 2.2 (Message Processing) and Feature 2.9 (Plugin System): Feature 2.9 defines the plugin trait abstraction, while Feature 2.2 invokes plugins through that trait.
- Feature 2.10 (LLM Gateway Plugin) shares FRs `fr-send-message`, `fr-schema-extensibility`, `fr-session-summary`, `fr-context-overflow`, and `fr-conversation-memory` with other features because it implements the concrete plugin backend for these capabilities.
- `cpt-cf-chat-engine-component-webhook-integration` is shared by Features 2.2, 2.9, and 2.10: Feature 2.9 (Plugin System) defines the webhook trait, Feature 2.2 (Message Processing) invokes plugins through it, and Feature 2.10 (LLM Gateway Plugin) implements a concrete plugin.
- Feature 2.11 (Authorization) is a dedicated, cross-cutting PEP feature owning **all** §3.5 authorization design elements. It is a full PEP over the sessions/messages/reactions resources and depends on those tables/services existing; `nfr-authentication` is shared with Feature 2.1 (which owns AuthN + `SecurityContext` injection) while Feature 2.11 owns the AuthZ (PDP/PEP) half. `fr-share-session` is shared with Feature 2.7 (which owns the share route/entity) while Feature 2.11 owns the share-token read authorization boundary (`cpt-cf-chat-engine-seq-authz-shared-read`).

Feature numbering reflects logical grouping, not implementation order. Recommended implementation sequence: 2.1 → 2.2 → 2.9 → 2.3 → 2.4 → 2.6 → 2.5 → 2.7 → 2.8 → 2.10 → 2.11 (authorization is layered after the sessions/messages/reactions resources and their pipeline writes exist; its internal steps A→B→C→{D, F}→E→G are strictly ordered).


## 2. Entries

**Overall implementation status:**

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-status-overall`


### 2.1 [Session Lifecycle](features/session-lifecycle.md) 🔄 HIGH

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-feature-session-lifecycle`

- **Type**: Core
- **Phases**: Single-phase implementation

- **Purpose**: Establish the foundational service infrastructure: HTTP REST API surface with NDJSON streaming, session CRUD, session type configuration by developers, database schema, and authentication model. All other features depend on this foundation.

- **Depends On**: None

- **Scope**:
  - HTTP REST service wiring with NDJSON streaming support and bearer token authentication
  - Session type registration and configuration (developer-facing, references `plugin_instance_id`)
  - Session CRUD: create, get, list, soft-delete, hard-delete, archive, and restore
  - Database schema for sessions and session_types tables
  - Tenant isolation and per-request authentication enforcement
  - Health check and observability endpoints

- **Out of scope**:
  - Message sending, streaming, or any message-level operations
  - Message tree operations (variants, branching)
  - Session export, sharing, or search

- **Requirements Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-fr-create-session`
  - [x] `p1` - `cpt-cf-chat-engine-fr-delete-session`
  - [x] `p1` - `cpt-cf-chat-engine-fr-soft-delete-session`
  - [x] `p1` - `cpt-cf-chat-engine-fr-hard-delete-session`
  - [x] `p2` - `cpt-cf-chat-engine-fr-restore-session`
  - [x] `p3` - `cpt-cf-chat-engine-fr-archive-session`
  - [ ] `p1` - `cpt-cf-chat-engine-nfr-availability`
  - [ ] `p1` - `cpt-cf-chat-engine-nfr-authentication`
  - [x] `p1` - `cpt-cf-chat-engine-nfr-data-persistence`
  - [ ] `p1` - `cpt-cf-chat-engine-nfr-scalability`
  - [ ] `p2` - `cpt-cf-chat-engine-nfr-lifecycle-performance`
  - [ ] `p2` - `cpt-cf-chat-engine-nfr-recovery`
  - [ ] `p2` - `cpt-cf-chat-engine-nfr-developer-experience`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-principle-backend-authority`
  - [x] `p1` - `cpt-cf-chat-engine-principle-zero-business-logic`

- **Design Constraints Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-constraint-single-database`

- **Domain Model Entities**:
  - Session
  - SessionType

- **Design Entities**:

  - [x] `p1` - `cpt-cf-chat-engine-design-entity-session`
  - [x] `p1` - `cpt-cf-chat-engine-design-entity-session-type`
  - [ ] `p1` - `cpt-cf-chat-engine-design-entity-capability`
  - [ ] `p2` - `cpt-cf-chat-engine-design-entity-capability-value`

- **Design Components**:

  - [x] `p1` - `cpt-cf-chat-engine-component-service`
  - [x] `p1` - `cpt-cf-chat-engine-component-session-management`

- **API**:
  - `POST /session-types` — developer: register session type
  - `GET /session-types` — developer: list registered types
  - `POST /sessions` — create session
  - `GET /sessions` — list sessions (scoped to client)
  - `GET /sessions/{session_id}` — get session
  - `PATCH /sessions/{session_id}` — update metadata
  - `DELETE /sessions/{session_id}` — soft-delete
  - `DELETE /sessions/{session_id}?hard=true` — hard-delete
  - `POST /sessions/{session_id}/archive` — archive
  - `POST /sessions/{session_id}/restore` — restore
  - `GET /health`

- **Sequences**:

  - [ ] `p1` - `cpt-cf-chat-engine-seq-configure-session-type`
  - [ ] `p1` - `cpt-cf-chat-engine-seq-create-session`

- **Data**:

  - [x] `p1` - `cpt-cf-chat-engine-dbtable-sessions`
  - [x] `p1` - `cpt-cf-chat-engine-dbtable-session-types`


### 2.2 [Message Processing & Streaming](features/message-processing.md) 🔄 HIGH

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-feature-message-processing`

- **Type**: Core
- **Phases**: Single-phase implementation

- **Purpose**: Enable end-users to send messages with file attachments to backend plugins and receive streamed AI responses. Implements the core message-tree append, synchronous plugin invocation, NDJSON streaming pipeline, and streaming cancellation.

- **Depends On**: `cpt-cf-chat-engine-feature-session-lifecycle`

- **Scope**:
  - Message node creation and immutable tree append
  - File UUID forwarding to backend plugins (no direct file upload; UUIDs only)
  - Synchronous backend plugin invocation with backpressure handling
  - HTTP chunked transfer (NDJSON) streaming of AI response tokens to client
  - Per-message capability value forwarding to backend plugin
  - Streaming cancellation with partial message persistence
  - Data protection: response content encryption at rest

- **Out of scope**:
  - Message variants, branching, or variant navigation (owned by Message Variants & Branching)
  - Context window strategy selection (owned by Context & Memory Management)
  - Session export or sharing

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-chat-engine-fr-send-message`
  - [x] `p1` - `cpt-cf-chat-engine-fr-attach-files`
  - [ ] `p1` - `cpt-cf-chat-engine-fr-stop-streaming`
  - [ ] `p1` - `cpt-cf-chat-engine-nfr-response-time`
  - [ ] `p1` - `cpt-cf-chat-engine-nfr-streaming`
  - [x] `p1` - `cpt-cf-chat-engine-nfr-backend-isolation`
  - [ ] `p1` - `cpt-cf-chat-engine-nfr-file-size`
  - [x] `p1` - `cpt-cf-chat-engine-nfr-data-integrity`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-principle-immutable-tree`
  - [ ] `p1` - `cpt-cf-chat-engine-principle-streaming`

- **Design Constraints Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-constraint-external-storage`

- **Domain Model Entities**:
  - Message (MessageNode)
  - StreamingState

- **Design Entities**:

  - [x] `p1` - `cpt-cf-chat-engine-design-entity-message`

- **Design Components**:

  - [x] `p1` - `cpt-cf-chat-engine-component-message-processing`
  - [ ] `p1` - `cpt-cf-chat-engine-component-webhook-integration`
  - [ ] `p1` - `cpt-cf-chat-engine-component-response-streaming`

- **API**:
  - `POST /sessions/{session_id}/messages` — send message; streams NDJSON response
  - `DELETE /sessions/{session_id}/messages/{message_id}/streaming` — stop streaming

- **Sequences**:

  - [ ] `p1` - `cpt-cf-chat-engine-seq-send-message-with-files`
  - [ ] `p1` - `cpt-cf-chat-engine-seq-stop-streaming`

- **Data**:

  - [x] `p1` - `cpt-cf-chat-engine-dbtable-messages`


### 2.3 [Message Variants & Branching](features/message-variants.md) 🔄 MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-feature-message-variants`

- **Type**: Core
- **Phases**: Single-phase implementation

- **Purpose**: Allow end-users to recreate AI responses with alternative outputs, branch the conversation from any past message node, and navigate between available response variants. Also handles mid-session session-type switching with plugin capability validation.

- **Depends On**: `cpt-cf-chat-engine-feature-message-processing`

- **Scope**:
  - Recreate response: re-invoke backend plugin for an existing user message and append a new variant node
  - Branch from message: set the session's active path to any ancestor message node
  - Variant navigation: list and select among existing response variants at a given position
  - Session-type switching mid-session with capability compatibility check
  - Active path tracking per session (stored in sessions table)

- **Out of scope**:
  - Initial message send or NDJSON streaming setup (owned by Message Processing)
  - Context window selection (owned by Context & Memory Management)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-chat-engine-fr-recreate-response`
  - [ ] `p2` - `cpt-cf-chat-engine-fr-branch-message`
  - [x] `p2` - `cpt-cf-chat-engine-fr-navigate-variants`
  - [ ] `p2` - `cpt-cf-chat-engine-fr-switch-session-type`
  - [ ] `p2` - `cpt-cf-chat-engine-nfr-message-history`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-principle-immutable-tree`

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**:
  - MessageNode (variant tree)
  - VariantIndex

- **Design Components**:

  - [x] `p1` - `cpt-cf-chat-engine-component-message-processing`

- **API**:
  - `POST /sessions/{session_id}/messages/{message_id}/recreate`
  - `POST /sessions/{session_id}/messages/{message_id}/branch`
  - `GET /sessions/{session_id}/messages/{message_id}/variants`
  - `PUT /sessions/{session_id}/messages/{message_id}/variants/active`
  - `PATCH /sessions/{session_id}/session-type`

- **Sequences**:

  - [ ] `p1` - `cpt-cf-chat-engine-seq-recreate-response`
  - [ ] `p2` - `cpt-cf-chat-engine-seq-branch-message`
  - [ ] `p2` - `cpt-cf-chat-engine-seq-navigate-variants`
  - [ ] `p2` - `cpt-cf-chat-engine-seq-switch-session-type`

- **Data**:

  - None (extends `cpt-cf-chat-engine-dbtable-messages` owned by feature-message-processing)


### 2.4 [Context & Memory Management](features/context-management.md) 🔄 MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-feature-context-management`

- **Type**: Extension
- **Phases**: Single-phase implementation

- **Purpose**: Control which portion of the message tree is sent as conversation history to backend plugins. Implements configurable per-session memory strategies (full history, sliding window, AI-summarized) and graceful context overflow handling when the active path exceeds backend token limits.

- **Depends On**: `cpt-cf-chat-engine-feature-message-processing`

- **Scope**:
  - Configurable per-session memory strategy: full, sliding window, or AI-summarized
  - Context overflow detection and configurable degradation behavior
  - Active-path extraction and context payload construction for plugin invocations
  - Memory strategy persistence in session metadata (sessions table)

- **Out of scope**:
  - Summarization logic for overflow (orchestrated via backend plugin; summary generation owned by Session Intelligence)
  - Message storage schema changes (owned by Message Processing)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-chat-engine-fr-conversation-memory`
  - [ ] `p2` - `cpt-cf-chat-engine-fr-context-overflow`
  - [ ] `p2` - `cpt-cf-chat-engine-nfr-message-history`
  - [ ] `p2` - `cpt-cf-chat-engine-nfr-response-time`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-principle-immutable-tree`

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**:
  - MemoryStrategy
  - ContextWindow

- **Design Components**:

  - [x] `p1` - `cpt-cf-chat-engine-component-message-processing`

- **API**:
  - `PATCH /sessions/{session_id}` — set `memory_strategy` field

- **Sequences**:

  - None (context selection is embedded in `cpt-cf-chat-engine-seq-send-message-with-files`)

- **Data**:

  - None (`memory_strategy` persisted in `cpt-cf-chat-engine-dbtable-sessions` owned by feature-session-lifecycle)


### 2.5 [Session Intelligence](features/session-intelligence.md) 🔄 MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-feature-session-intelligence`

- **Type**: Extension
- **Phases**: Single-phase implementation

- **Purpose**: Add session-level intelligence and data lifecycle management: AI-generated session summaries routed through the backend plugin, and configurable retention policies for automatic message cleanup with cascade deletion.

- **Depends On**: `cpt-cf-chat-engine-feature-message-processing`, `cpt-cf-chat-engine-feature-message-reactions`

- **Scope**:
  - On-demand session summary generation via backend plugin invocation
  - Configurable message retention policies (age-based or count-based)
  - Retention policy enforcement via scheduled or event-triggered cleanup
  - Cascade deletion of message subtrees during retention enforcement

- **Out of scope**:
  - Message reactions and feedback (owned by Message Reactions & Feedback)
  - Session export or sharing (owned by Session Export & Sharing)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-chat-engine-fr-session-summary`
  - [ ] `p2` - `cpt-cf-chat-engine-fr-message-retention`
  - [ ] `p2` - `cpt-cf-chat-engine-fr-retention-policy`
  - [ ] `p2` - `cpt-cf-chat-engine-nfr-retention-sla`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-principle-backend-authority`

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**:
  - RetentionPolicy
  - SessionSummary

- **Design Components**:

  - [x] `p1` - `cpt-cf-chat-engine-component-session-management`
  - [x] `p1` - `cpt-cf-chat-engine-component-message-processing`

- **API**:
  - `POST /sessions/{session_id}/summary`
  - `GET /sessions/{session_id}/retention-policy`
  - `PATCH /sessions/{session_id}/retention-policy`

- **Sequences**:

  - [ ] `p2` - `cpt-cf-chat-engine-seq-generate-summary`
  - Uses `cpt-cf-chat-engine-seq-delete-message-cascade` (owned by feature-message-reactions) for retention policy enforcement

- **Data**:

  - None (policy stored in `cpt-cf-chat-engine-dbtable-sessions` owned by feature-session-lifecycle)


### 2.6 [Message Reactions & Feedback](features/message-reactions.md) 🔄 MEDIUM

- [x] `p2` - **ID**: `cpt-cf-chat-engine-feature-message-reactions`

- **Type**: Extension

- **Sub-features**: [Message Delete](features/message-delete.md)

- **Purpose**: Enable end-users to attach like/dislike reactions to individual messages (one per user per message with UPSERT semantics), and support cascade deletion of a message node together with its entire subtree.

- **Depends On**: `cpt-cf-chat-engine-feature-message-processing`

- **Scope**:
  - Like/dislike reactions on any message node (one per user per message, UPSERT semantics)
  - Reaction change and removal via `reaction_type: "none"`
  - Fire-and-forget `message.reaction` event to backend plugin for analytics
  - Hard delete of a message node and its full descendant subtree (cascade)
  - Reactions stored independently from message content in a separate `message_reactions` table

- **Out of scope**:
  - Session-level delete operations (owned by Session Lifecycle)
  - Retention policy management (owned by Session Intelligence)

- **Requirements Covered**:

  - [x] `p2` - `cpt-cf-chat-engine-fr-message-feedback`
  - [x] `p1` - `cpt-cf-chat-engine-fr-delete-message`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-principle-immutable-tree`

- **Design Constraints Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-constraint-single-database`

- **Domain Model Entities**:
  - MessageReaction

- **Design Entities**:

  - [x] `p2` - `cpt-cf-chat-engine-design-entity-message-reaction`

- **Design Components**:

  - [x] `p2` - `cpt-cf-chat-engine-component-message-reactions`

- **API**:
  - `POST /sessions/{session_id}/messages/{message_id}/reaction` — UPSERT like/dislike/none
  - `GET /sessions/{session_id}/messages/{message_id}/reactions` — list reactions for a message
  - `DELETE /sessions/{session_id}/messages/{message_id}` — cascade subtree deletion

- **Sequences**:

  - [x] `p2` - `cpt-cf-chat-engine-seq-add-reaction`
  - [x] `p1` - `cpt-cf-chat-engine-seq-delete-message-cascade`

- **Data**:

  - [x] `p2` - `cpt-cf-chat-engine-dbtable-reactions`


### 2.7 [Session Export & Sharing](features/session-export.md) 🔄 LOW

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-feature-session-export`

- **Type**: Extension
- **Phases**: Single-phase implementation

- **Purpose**: Allow end-users to export their conversation history in portable formats and share read-only session views with others via time-limited shareable links.

- **Depends On**: `cpt-cf-chat-engine-feature-session-lifecycle`

- **Scope**:
  - Export session as JSON or Markdown (active path only)
  - Generate time-limited shareable read-only links
  - Token-based read-only session access for non-authenticated viewers
  - Share token lifecycle: creation, expiry, and revocation

- **Out of scope**:
  - Message-level search (owned by Message Search)
  - Session modification by share link recipients

- **Requirements Covered**:

  - [ ] `p3` - `cpt-cf-chat-engine-fr-export-session`
  - [ ] `p3` - `cpt-cf-chat-engine-fr-share-session`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-principle-zero-business-logic`

- **Design Constraints Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-constraint-external-storage`

- **Domain Model Entities**:
  - ExportedSession
  - ShareToken

- **Design Entities**:

  - [x] `p2` - `cpt-cf-chat-engine-design-entity-share-token`

- **Design Components**:

  - [x] `p3` - `cpt-cf-chat-engine-component-conversation-export`

- **API**:
  - `GET /sessions/{session_id}/export?format=json|markdown`
  - `POST /sessions/{session_id}/share`
  - `GET /sessions/shared/{token}`

- **Sequences**:

  - [ ] `p3` - `cpt-cf-chat-engine-seq-export-session`
  - [ ] `p3` - `cpt-cf-chat-engine-seq-share-session`

- **Data**:

  - None (share tokens stored in `cpt-cf-chat-engine-dbtable-sessions` owned by feature-session-lifecycle)


### 2.8 [Message Search](features/message-search.md) 🔄 LOW

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-feature-message-search`

- **Type**: Extension
- **Phases**: Single-phase implementation

- **Purpose**: Enable end-users to search within a session's message history and across all their sessions using full-text search, with results ranked by relevance and paginated.

- **Depends On**: `cpt-cf-chat-engine-feature-message-processing`

- **Scope**:
  - Full-text search within a single session's message history
  - Cross-session full-text search scoped to the requesting client
  - Relevance-ranked and paginated result sets
  - Search index maintenance on message create and delete events

- **Out of scope**:
  - Session export (owned by Session Export & Sharing)
  - Message modification or variant creation from search results

- **Requirements Covered**:

  - [ ] `p3` - `cpt-cf-chat-engine-fr-search-session`
  - [ ] `p3` - `cpt-cf-chat-engine-fr-search-sessions`
  - [ ] `p2` - `cpt-cf-chat-engine-nfr-search`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-principle-zero-business-logic`

- **Design Constraints Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-constraint-single-database`

- **Domain Model Entities**:
  - SearchResult

- **Design Components**:

  - [ ] `p3` - `cpt-cf-chat-engine-component-message-search`

- **API**:
  - `GET /sessions/{session_id}/search?q={query}&page={n}&per_page={n}`
  - `GET /sessions/search?q={query}&page={n}&per_page={n}`

- **Sequences**:

  - [ ] `p3` - `cpt-cf-chat-engine-seq-search-session`
  - [ ] `p3` - `cpt-cf-chat-engine-seq-search-sessions`

- **Data**:

  - None (search index on `cpt-cf-chat-engine-dbtable-messages` owned by feature-message-processing)


### 2.9 [Plugin System Infrastructure](features/plugin-system.md) 🔄 HIGH

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-feature-plugin-system`

- **Type**: Plugin
- **Phases**: Single-phase implementation

- **Purpose**: Provide the backend plugin integration layer: define the `ChatEngineBackendPlugin` trait, implement the plugin registry with ClientHub/GTS-based discovery, manage per-session-type plugin configuration (`plugin_configs` table), and ship the first-party `webhook-compat` plugin for legacy HTTP backends.

- **Depends On**: `cpt-cf-chat-engine-feature-session-lifecycle`

- **Scope**:
  - `ChatEngineBackendPlugin` trait definition with all lifecycle and message methods (`on_session_type_configured`, `on_session_created`, `on_session_updated`, `on_message`, `on_message_recreate`, `on_session_summary`, `health_check`)
  - Plugin registry: startup registration via ClientHub, resolution by `plugin_instance_id` (GTS ID)
  - `plugin_configs` table: per-session-type plugin configuration (composite PK: `plugin_instance_id` + `session_type_id`, opaque JSONB `config`)
  - N:1 session type → plugin relationship: multiple session types can share the same plugin instance with different configs
  - `webhook-compat` first-party plugin: wraps legacy HTTP webhook endpoints, owns auth/retry/circuit breaker/timeout internally
  - Plugin health check mechanism for session type configuration

- **Out of scope**:
  - Concrete plugin business logic (owned by individual plugin features, e.g., LLM Gateway Plugin)
  - Session CRUD operations (owned by Session Lifecycle)
  - Message processing pipeline (owned by Message Processing)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-chat-engine-fr-schema-extensibility`
  - [x] `p1` - `cpt-cf-chat-engine-nfr-backend-isolation`
  - [ ] `p1` - `cpt-cf-chat-engine-nfr-availability`
  - [ ] `p1` - `cpt-cf-chat-engine-nfr-response-time`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-principle-backend-authority`
  - [x] `p1` - `cpt-cf-chat-engine-principle-zero-business-logic`

- **Design Constraints Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-constraint-single-database`

- **Domain Model Entities**:
  - ChatEngineBackendPlugin (trait)
  - PluginConfig

- **Design Components**:

  - [ ] `p1` - `cpt-cf-chat-engine-component-webhook-integration`

- **API**:
  - No public API — plugin system is internal infrastructure used by session lifecycle and message processing

- **Sequences**:

  - None (plugin invocation is embedded in session and message sequences)

- **Data**:

  - [x] `p1` - `cpt-cf-chat-engine-dbtable-plugin-configs`


### 2.10 [LLM Gateway Plugin](features/llm-gateway-plugin.md) 🔄 HIGH

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-feature-llm-gateway-plugin`

- **Type**: Plugin
- **Phases**: Single-phase implementation

- **Purpose**: First concrete `ChatEngineBackendPlugin` implementation: integrates with Model Registry for capability resolution, forwards messages to LLM Gateway service with streaming response, handles context overflow via summarization flow, and registers GTS-derived schemas for LLM-specific metadata.

- **Depends On**: `cpt-cf-chat-engine-feature-plugin-system`

- **Scope**:
  - Implement `ChatEngineBackendPlugin` trait for LLM gateway integration
  - Model Registry integration: query available models on `on_session_type_configured` (may defer), resolve capabilities on `on_session_created`, refresh on `on_session_updated` (model change)
  - GTS schema registration at startup: `LlmPluginConfig`, `LlmSummarizationSettings`, `LlmMessageMetadata`, `LlmUsage`, and entity schema extensions
  - Message processing: forward to LLM Gateway service via HTTP, stream response chunks back through `ResponseStream`
  - Summarization flow on context overflow: detect overflow, generate summary message with `is_hidden_from_user=true`, preserve `recent_messages_to_keep` recent messages
  - Visibility flags: `is_hidden_from_backend` and `is_hidden_from_user` for summary and system messages
  - Plugin-owned resilience: HTTP retry, circuit breaker, and timeout for LLM Gateway and Model Registry calls

- **Out of scope**:
  - Plugin trait definition or registry (owned by Plugin System Infrastructure)
  - Core message tree persistence (owned by Message Processing)
  - Memory strategy selection logic (owned by Context & Memory Management)
  - Other plugin implementations (webhook-compat is in Plugin System Infrastructure)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-chat-engine-fr-send-message`
  - [ ] `p1` - `cpt-cf-chat-engine-fr-schema-extensibility`
  - [ ] `p2` - `cpt-cf-chat-engine-fr-session-summary`
  - [ ] `p2` - `cpt-cf-chat-engine-fr-context-overflow`
  - [ ] `p2` - `cpt-cf-chat-engine-fr-conversation-memory`
  - [ ] `p1` - `cpt-cf-chat-engine-nfr-streaming`
  - [x] `p1` - `cpt-cf-chat-engine-nfr-backend-isolation`
  - [ ] `p1` - `cpt-cf-chat-engine-nfr-availability`

- **Design Principles Covered**:

  - [x] `p1` - `cpt-cf-chat-engine-principle-backend-authority`
  - [ ] `p1` - `cpt-cf-chat-engine-principle-streaming`

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**:
  - LlmPluginConfig
  - LlmSummarizationSettings
  - LlmMessageMetadata
  - LlmUsage

- **Design Components**:

  - [ ] `p1` - `cpt-cf-chat-engine-component-webhook-integration`

- **API**:
  - No public API — plugin is invoked internally by Chat Engine via trait methods

- **Sequences**:

  - None (uses existing sequences: `cpt-cf-chat-engine-seq-create-session`, `cpt-cf-chat-engine-seq-send-message-with-files`, `cpt-cf-chat-engine-seq-generate-summary`)

- **Data**:

  - None (uses `cpt-cf-chat-engine-dbtable-plugin-configs` owned by feature-plugin-system; GTS schemas registered at runtime)


### 2.11 Authorization (PDP/PEP + SecureORM) 🔄 HIGH

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-feature-authorization`

- **Type**: Core
- **Phases**: Ordered multi-step implementation (steps A–G below; foundation step A blocks all others)

- **Purpose**: Turn Chat Engine into a full Policy Enforcement Point (PEP) over the platform PDP/PEP + SecureORM model. Every sensitive database access is gated by a PDP decision compiled into an `AccessScope` and enforced by `SecureConn` as a SQL predicate against a denormalized owner pair `(owner_tenant_id, owner_id)` carried on every scoped row. Replaces the interim `AccessScope::allow_all()` + manual `(tenant, user)` filtering with query-level authorization, a fail-closed error surface, and an enumerated bypass registry for trusted-internal and capability-URL paths. This feature realizes the accepted authorization decision `ADR/0028-authz-pep-secureorm-uuid-migration.md` across the sessions, messages, and reactions resources plus their child tables.

- **Depends On**: `cpt-cf-chat-engine-feature-session-lifecycle`, `cpt-cf-chat-engine-feature-message-processing`, `cpt-cf-chat-engine-feature-message-reactions`, `cpt-cf-chat-engine-feature-plugin-system`, `cpt-cf-chat-engine-feature-message-variants`, `cpt-cf-chat-engine-feature-session-intelligence`, `cpt-cf-chat-engine-feature-session-export`, `cpt-cf-chat-engine-feature-message-search`

- **Scope** (implementation steps are strictly ordered; A is the foundation):
  - **A. Owner-column migration (foundation, blocks B–G)**: single engine-aware forward migration `m20260417_000006_authz_owner_columns` — cast `sessions.tenant_id`/`user_id` text→UUID guarded by a pre-check that aborts on NULL or non-castable rows; add `owner_tenant_id`/`owner_id` UUID columns to `messages`, `message_parts`, `message_reactions`, `file_citations`, `link_citations`, `link_references`; backfill the owner pair from the parent-session chain (abort on orphans / nullable-tenant failure); set `NOT NULL` and add indexes on `owner_tenant_id` and `(owner_tenant_id, owner_id)`
  - **B. Scopable entity updates (depends on A)**: lift `#[secure(unrestricted)]` on the scoped entities (session, message, reaction) and their scoped children; declare per-resource `supported_properties`; the `sessions` `Scopable` derive advertises PEP property names `owner_tenant_id`/`owner_id` and maps them onto the physical `tenant_id`/`user_id` columns; child tables resolve onto identically-named physical columns; global tables (`session_types`, `plugin_configs`, `stream_events`) stay `#[secure(unrestricted)]`
  - **C. PolicyEnforcer wiring (depends on B)**: declare `deps = ["authz-resolver"]` on the gear; resolve `dyn AuthZResolverClient` from `ClientHub` at init; construct a single `PolicyEnforcer` Arc-cloned into every domain service as an `enforcer` field; define `ResourceType` consts with GTS type ids `gts.cf.core.chat_engine.<resource>.v1~`
  - **D. Per-service PEP enforcement (depends on C)**: replace `AccessScope::allow_all()` + manual `(tenant, user)` filters with `access_scope` / `access_scope_with` across Session, Message, Reaction, Variant, Search, Export, and Intelligence services; two-step owner-pair prefetch for point ops (read/update/delete); map `EnforcerError` → `ChatEngineError` (`Denied` → 403, point-op scope-miss → 404, `CompileFailed`/`EvaluationFailed` → 403 fail-closed)
  - **E. Bypass registry (depends on D)**: named `internal_write_scope()` wrapper with `// AUTHZ-BYPASS: <reason>` markers at each enumerated site — internal pipeline writes (`finalize_assistant`, `insert_summary_message`, `insert_assistant_variant_stub`) derive the owner pair from the authorized parent in the same transaction; scheduled retention cross-tenant op and `list_tenants_with_active_sessions` (not HTTP-exposed); `find_by_share_token` capability-URL read (404 anti-enumeration) on the `.public()` route; `find_by_session_id_unscoped` internal-pipeline-only parent read
  - **F. `session_type` permission gate (depends on C)**: PDP decision-gate (`require_constraints=false`) on `session_type` create/update/delete mutations (`Denied` → 403); `session_type` reads stay `.authenticated()`-only with no PDP call
  - **G. Authorization tests (depends on D, E, F)**: PDP-deny parity per resource (sessions/messages/reactions) with `DenyAllAuthZResolver`; migration test (cast + backfill + `NOT NULL` + child inherits owner); share-token 200/404; retention op not HTTP-exposed; denormalization-invariant inheritance

- **Out of scope**:
  - Authentication / bearer-token validation and `SecurityContext` injection (AuthN is unchanged and already wired via Session Lifecycle; see `cpt-cf-chat-engine-nfr-authentication`)
  - Zero-downtime / expand-contract migration variant (deferred to a separate ADR — OQ2)
  - Cross-tenant session transfer (owner pair is immutable — OQ3)
  - The optional `allow_all()` lint (possible future hardening — OQ4)
  - Business logic of the enforced operations (owned by the respective feature: Session Lifecycle, Message Processing, Message Variants, Context Management, Session Intelligence, Message Reactions, Session Export, Message Search)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-chat-engine-nfr-authentication`
  - [ ] `p3` - `cpt-cf-chat-engine-fr-share-session`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-chat-engine-principle-owner-denorm-invariant`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-chat-engine-constraint-fail-closed-authz`
  - [ ] `p1` - `cpt-cf-chat-engine-constraint-no-allow-all-outside-registry`

- **Domain Model Entities**:
  - Session (owner pair)
  - Message (owner pair)
  - MessageReaction (owner pair)
  - AccessScope / PolicyEnforcer (PEP)

- **Design Entities**:

  - None (owner-pair columns extend existing entities; see Data)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-chat-engine-component-policy-enforcer`

- **Interfaces**:

  - [ ] `p1` - `cpt-cf-chat-engine-interface-pep`

- **Design Elements**:

  - [ ] `p1` - `cpt-cf-chat-engine-design-auth-model`
  - [ ] `p1` - `cpt-cf-chat-engine-design-authz-bypass-registry`

- **API**:
  - No new public API — authorization is enforced transparently across existing routes; the only authorization-specific route surface is the pre-existing `.public()` `POST /sessions/shared/{token}` capability-URL read (owned by Session Export & Sharing) and the `session_type` mutation routes (owned by Session Lifecycle), both now PDP-gated

- **Sequences**:

  - [ ] `p1` - `cpt-cf-chat-engine-seq-authz-list`
  - [ ] `p1` - `cpt-cf-chat-engine-seq-authz-point-op`
  - [ ] `p2` - `cpt-cf-chat-engine-seq-authz-shared-read`
  - [ ] `p1` - `cpt-cf-chat-engine-seq-authz-internal-write`

- **Data**:

  - [ ] `p1` - `cpt-cf-chat-engine-dbtable-authz-owner-columns`

- **Definition of Done**:
  - Migration `m20260417_000006_authz_owner_columns` casts and backfills the owner pair with the NULL/non-castable pre-check guard; all scoped child rows inherit the parent session's owner pair; `NOT NULL` + indexes applied (step A)
  - No production code path uses `AccessScope::allow_all()` except through `internal_write_scope()` at a registry-listed site carrying `// AUTHZ-BYPASS` + `@cpt` markers (steps D, E)
  - Every sensitive session/message/reaction access obtains its scope from the single shared `PolicyEnforcer`; point ops use the two-step prefetch (steps C, D)
  - Fail-closed surface verified: `Denied`/`CompileFailed`/`EvaluationFailed` → 403, point-op scope-miss → 404, PDP unavailability never leaks 503/500 (step D)
  - `session_type` mutations gated by a PDP decision; `session_type` reads stay `.authenticated()`-only with no PDP call (step F)
  - Share-token read returns 200 on valid/active/non-revoked token and 404 otherwise (anti-enumeration), with no PDP call (step E)
  - Tests green for PDP-deny parity, migration cast+backfill+NOT NULL+child inheritance, share-token 200/404, retention op not HTTP-exposed, and the denormalization invariant (step G)


---

## 3. Deliberate Omissions

The following items are intentionally excluded from this decomposition cycle:

- **Monitoring & Alerting infrastructure** — deferred to Gears middleware-level observability; Chat Engine emits metrics and structured logs but does not own dashboards or alert rules.
- **Client SDK** — client-side libraries are out of scope for the backend decomposition; API contracts defined in DESIGN.md are sufficient for client teams.
- **Admin UI / Management Console** — session type registration and retention policy configuration are API-only in this cycle.
- **FEATURE artifact for 2.11 Authorization** — the per-feature FEATURE spec (`features/authorization.md`) is not yet authored; Feature 2.11 is currently specified at decomposition level only (scope steps A–G, coverage, and definition of done). The heading is intentionally unlinked until that FEATURE artifact is created.

---

## 4. Feature Dependencies

```text
cpt-cf-chat-engine-feature-session-lifecycle
    ↓
    ├─→ cpt-cf-chat-engine-feature-plugin-system
    │       ↓
    │       └─→ cpt-cf-chat-engine-feature-llm-gateway-plugin
    ├─→ cpt-cf-chat-engine-feature-message-processing
    │       ↓
    │       ├─→ cpt-cf-chat-engine-feature-message-variants
    │       ├─→ cpt-cf-chat-engine-feature-context-management
    │       ├─→ cpt-cf-chat-engine-feature-session-intelligence ──→ cpt-cf-chat-engine-feature-message-reactions
    │       ├─→ cpt-cf-chat-engine-feature-message-reactions
    │       └─→ cpt-cf-chat-engine-feature-message-search
    └─→ cpt-cf-chat-engine-feature-session-export

cpt-cf-chat-engine-feature-session-lifecycle
cpt-cf-chat-engine-feature-message-processing
cpt-cf-chat-engine-feature-message-reactions
cpt-cf-chat-engine-feature-plugin-system
cpt-cf-chat-engine-feature-message-variants
cpt-cf-chat-engine-feature-session-intelligence
cpt-cf-chat-engine-feature-session-export
cpt-cf-chat-engine-feature-message-search
    └──(all eight)──→ cpt-cf-chat-engine-feature-authorization
                         [A migration] → [B scopable] → [C enforcer] → {[D per-service PEP], [F session-type gate]} → [E bypass registry] → [G tests]
```

**Dependency Rationale**:

- `cpt-cf-chat-engine-feature-plugin-system` requires `cpt-cf-chat-engine-feature-session-lifecycle`: plugin configs reference session types; the plugin registry needs the session type infrastructure to resolve `plugin_instance_id`.
- `cpt-cf-chat-engine-feature-llm-gateway-plugin` requires `cpt-cf-chat-engine-feature-plugin-system`: implements the `ChatEngineBackendPlugin` trait defined by the plugin system; uses the plugin registry for registration and the `plugin_configs` table for LLM-specific configuration.
- `cpt-cf-chat-engine-feature-message-processing` requires `cpt-cf-chat-engine-feature-session-lifecycle`: messages belong to sessions; the messages table and streaming infrastructure depend on the sessions table and the auth model established in F01.
- `cpt-cf-chat-engine-feature-message-variants` requires `cpt-cf-chat-engine-feature-message-processing`: variant creation re-invokes the same plugin pipeline and appends to the existing message tree.
- `cpt-cf-chat-engine-feature-context-management` requires `cpt-cf-chat-engine-feature-message-processing`: context selection modifies the plugin payload construction inside the message processing pipeline.
- `cpt-cf-chat-engine-feature-session-intelligence` requires `cpt-cf-chat-engine-feature-message-processing`: summary generation routes through the backend plugin; retention cleanup operates on the messages table.
- `cpt-cf-chat-engine-feature-session-intelligence` requires `cpt-cf-chat-engine-feature-message-reactions`: retention policy enforcement uses `seq-delete-message-cascade` which is owned by Message Reactions.
- `cpt-cf-chat-engine-feature-message-reactions` requires `cpt-cf-chat-engine-feature-message-processing`: reactions reference message IDs in the messages table; cascade delete operates on the fully populated message tree.
- `cpt-cf-chat-engine-feature-message-search` requires `cpt-cf-chat-engine-feature-message-processing`: the full-text search index is built on the messages table populated by message processing.
- `cpt-cf-chat-engine-feature-session-export` requires `cpt-cf-chat-engine-feature-session-lifecycle`: export reads session data from the sessions table and can operate on sessions without messages; share tokens are stored in the sessions table.
- Features `plugin-system` and `message-processing` are independent of each other and can be developed in parallel once `session-lifecycle` is complete.
- Features `message-variants`, `context-management`, `message-reactions`, and `message-search` are independent of each other and can be developed in parallel once `message-processing` is complete. Feature `session-intelligence` also requires `message-processing` but additionally depends on `message-reactions` (for `seq-delete-message-cascade` used in retention enforcement), so it can only start after both are complete.
- Feature `llm-gateway-plugin` can be developed once `plugin-system` is complete; it is independent of message-level features.
- Feature `session-export` is independent of all message-level and plugin features and can be developed in parallel with them.
- `cpt-cf-chat-engine-feature-authorization` requires `cpt-cf-chat-engine-feature-session-lifecycle`, `cpt-cf-chat-engine-feature-message-processing`, and `cpt-cf-chat-engine-feature-message-reactions`: the owner-column migration (step A) casts `sessions.tenant_id`/`user_id` and backfills the owner pair onto the `messages`, `message_parts`, `message_reactions`, `file_citations`, `link_citations`, and `link_references` tables, so those tables must exist; the per-service PEP enforcement (step D) replaces the interim `allow_all()` + manual filters across the Session, Message, and Reaction services.
- `cpt-cf-chat-engine-feature-authorization` requires `cpt-cf-chat-engine-feature-plugin-system`: the trusted-internal pipeline writes in the bypass registry (step E — `finalize_assistant`, `insert_summary_message`, `insert_assistant_variant_stub`) are the plugin-driven write sites whose owner pair is derived from the authorized parent session in the same transaction; the `session_type` permission gate (step F) applies to session-type mutations that reference plugin configuration.
- `cpt-cf-chat-engine-feature-authorization` also requires `cpt-cf-chat-engine-feature-message-variants`, `cpt-cf-chat-engine-feature-session-intelligence`, `cpt-cf-chat-engine-feature-session-export`, and `cpt-cf-chat-engine-feature-message-search`: step D retrofits PEP enforcement (`access_scope` / `access_scope_with`) into the Variant, Intelligence, Export, and Search service implementations, so those services must exist before their sensitive reads/writes can be gated.
- The authorization feature's internal steps are strictly ordered: **A** (migration) is the foundation and blocks **B–G**; **B** (Scopable entities) precedes **C** (PolicyEnforcer wiring, which needs per-resource `supported_properties`); **C** unblocks both **D** (per-service PEP enforcement) and **F** (`session_type` permission gate), which may proceed in parallel; **E** (bypass registry) depends on **D**; **G** (tests) trails **D**, **E**, and **F**.
- `cpt-cf-chat-engine-feature-authorization` shares `nfr-authentication` with `cpt-cf-chat-engine-feature-session-lifecycle` (which owns AuthN + `SecurityContext` injection; this feature owns the AuthZ/PDP-PEP half) and `fr-share-session` with `cpt-cf-chat-engine-feature-session-export` (which owns the share route and `ShareToken` entity; this feature owns the share-token read authorization boundary via `cpt-cf-chat-engine-seq-authz-shared-read`). These are documented shared coverage, not orphan or duplicate ownership.