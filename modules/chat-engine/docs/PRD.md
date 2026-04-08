Created:  2026-03-06 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# PRD — Chat Engine


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Module-Specific Environment Constraints](#31-module-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Core Session & Messaging](#51-core-session--messaging)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 NFR Exclusions](#61-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Additional Context](#13-additional-context)
- [14. Intentional Exclusions](#14-intentional-exclusions)
- [15. Traceability](#15-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

**Purpose**: Chat Engine is a Gateway module (CyberFabric ModKit) that manages session lifecycle and message routing between clients and Backend Plugin modules.

Chat Engine provides a unified interface for building conversational applications by abstracting session management, message history persistence, and flexible message processing. The system acts as an intermediary layer that handles the complexity of session state, message tree structures, and backend integration, allowing application developers to focus on building user experiences and backend plugin developers to focus on message processing logic.

The core value proposition is enabling flexible, stateful conversation management with support for advanced features like message regeneration and conversation branching. By decoupling the conversation infrastructure from processing logic, Chat Engine enables rapid experimentation with different backend implementations and conversation patterns without requiring changes to client applications.

The system supports various conversation patterns including traditional linear chat and branching conversations with variant exploration. This flexibility enables use cases ranging from automated assistants to human-in-the-loop support systems.

**Target Users**:
- **Application Developers** - Build chat applications using Chat Engine as backend infrastructure for session and message management
- **Backend Plugin Developers** - Implement custom message processing logic (AI, rule-based, human-in-the-loop) that integrates with Chat Engine
- **End Users** (indirect) - Use applications built on Chat Engine, experiencing responsive conversational interfaces

### 1.2 Background / Problem Statement

**Key Problems Solved**:
- **Session Management Complexity**: Eliminates the need for each application to implement session lifecycle, message history persistence, and state management from scratch
- **Message Routing Flexibility**: Decouples message processing logic from infrastructure, enabling easy switching between different backend implementations (automated, custom logic, human operators)
- **Conversation Variants**: Provides built-in support for message regeneration and branching conversations, enabling users to explore alternative responses without losing conversation history
- **Multi-Backend Support**: Allows seamless switching between different message processing backends mid-conversation, enabling hybrid approaches like starting with AI and escalating to human support
- **Plugin Extensibility**: Predefined domain model schemas (message types, content types, event types, error types) are designed as base schemas that plugin vendors can extend via GTS, enabling custom scenarios — custom content rendering, domain-specific events, vendor error taxonomies — without modifying Chat Engine core

### 1.3 Goals (Business Outcomes)

**Success Criteria**:
- Message routing latency < 100ms (p95) excluding backend processing time
- 99.9% uptime for session management operations
- Support for 10,000 concurrent sessions per instance
- Zero message loss during backend failures
- First message response time < 200ms from session creation

**Capabilities**:
- Session lifecycle management (create, delete, retrieve)
- Message routing to backend plugins with real-time streaming
- Message variant preservation (regeneration, branching)
- File attachment references in messages
- Session type switching mid-conversation
- Session export (JSON, Markdown, TXT)
- Session sharing via links with read-only and branching access
- Message search within sessions and across sessions
- Message tree navigation and variant selection
- Extensible domain model schemas — plugin vendors can define custom message types, content types, event types, and error types on top of the predefined base schemas, enabling custom scenarios without forking Chat Engine core (see FR-021)

### 1.4 Glossary

| Term | Definition |
|------|------------|
| **Session** | A persistent conversation context with a unique ID, owned by a client and associated with a session type |
| **Session Type** | A configuration profile that maps a session to a backend plugin and declares available capabilities (the maximum set the plugin can provide) |
| **Backend Plugin** | A CyberFabric ModKit plugin module implementing `ChatEngineBackendPlugin` trait; co-located in the same CyberFabric process and called directly via `ClientHub`. External HTTP backends are supported via the `chat-engine-webhook-adapter` plugin. See ADR-0022. |
| **Message Tree** | A tree structure where each message references a parent message; sibling nodes with the same parent are variants |
| **Message Variant** | An alternative response at the same position in the conversation tree — created by regeneration or branching |
| **Capability** | A typed feature declared by the backend plugin (`bool`, `enum`, `str`, `int`). `SessionType.available_capabilities` is the maximum set the plugin supports; `Session.enabled_capabilities` is the confirmed set for a specific session. Per-message settings are passed as `CapabilityValue` (id + value). |
| **CapabilityValue** | A per-message capability setting: `{id, value}` where value matches the type declared in the corresponding `Capability` definition |
| **Streaming Response** | Real-time forwarding of response chunks from the backend plugin to the client as they are generated |
| **Lifecycle State** | One of four session states: `active`, `archived`, `soft_deleted`, `hard_deleted` |
| **is_hidden_from_user** | Message visibility flag that excludes the message from client-facing APIs |
| **is_hidden_from_backend** | Message visibility flag that excludes the message from the context sent to backend plugins |

## 2. Actors

### 2.1 Human Actors

#### Client Application Developer

**ID**: `cpt-cf-chat-engine-actor-developer`

<!-- fdd-id-content -->
**Role**: Integrates Chat Engine into applications by configuring session types, implementing client-side UI for message display and navigation, and managing user authentication and file uploads.
<!-- fdd-id-content -->

#### End User

**ID**: `cpt-cf-chat-engine-actor-end-user`

<!-- fdd-id-content -->
**Role**: Interacts with client applications built on Chat Engine, sending messages, receiving responses, and navigating conversation variants (indirect actor, does not directly interact with Chat Engine).
<!-- fdd-id-content -->

#### Backend Plugin Developer

**ID**: `cpt-cf-chat-engine-actor-backend-developer`

<!-- fdd-id-content -->
**Role**: Implements CyberFabric ModKit plugin modules that satisfy the `ChatEngineBackendPlugin` trait. Registers the plugin in `types-registry` and declares its capabilities. May call external processing services, retrieval systems, or human-in-the-loop workflows internally. Optionally wraps an external HTTP endpoint using the `chat-engine-webhook-adapter` plugin.
<!-- fdd-id-content -->

### 2.2 System Actors

#### Client Application

**ID**: `cpt-cf-chat-engine-actor-client`

<!-- fdd-id-content -->
**Role**: Frontend application (web, mobile, desktop) that sends messages to Chat Engine, receives streaming responses, and renders conversation UI including message trees and variants.
<!-- fdd-id-content -->

#### Backend Plugin

**ID**: `cpt-cf-chat-engine-actor-backend-plugin`

<!-- fdd-id-content -->
**Role**: CyberFabric ModKit plugin module that implements the `ChatEngineBackendPlugin` trait and registers itself in the platform `types-registry`. Receives full session context, message history, and declared capabilities from Chat Engine. Implements custom message processing logic (LLM calls, RAG, rule-based responses, etc.).

Plugin modules are co-located within the same CyberFabric server process and called directly via `ClientHub` — no HTTP round-trip, no auth negotiation, no retry logic required at the Chat Engine level. Plugin vendors who need to delegate to an external HTTP endpoint use the first-party **`chat-engine-webhook-adapter`** plugin, which internally handles auth, retry, circuit breaker, and throttling.

**See**: ADR-0022 (CyberFabric Plugin System for Backend Integration)
<!-- fdd-id-content -->

#### File Storage Service

**ID**: `cpt-cf-chat-engine-actor-file-storage`

<!-- fdd-id-content -->
**Role**: External file storage service (e.g., S3, GCS) that stores file attachments. Provides signed URL access for file upload and download. Client applications upload files directly to storage.
<!-- fdd-id-content -->

#### Database Service

**ID**: `cpt-cf-chat-engine-actor-database`

<!-- fdd-id-content -->
**Role**: Persistent storage for sessions, messages, message tree structures, and metadata. Supports ACID transactions to ensure data integrity and consistency.
<!-- fdd-id-content -->

#### Chat Engine System

**ID**: `cpt-cf-chat-engine-actor-system`

**Role**: The Chat Engine service itself, acting as orchestrator for session lifecycle events, background tasks (retention policy enforcement, session archival), and automated system operations.

#### Tenant Administrator

**ID**: `cpt-cf-chat-engine-actor-tenant-admin`

**Role**: Administrative user responsible for configuring session types, retention policies, and plugin assignments at the tenant level.

## 3. Operational Concept & Environment

> Chat Engine operates as a stateless ModKit gateway module within the CyberFabric platform. No module-specific environment constraints beyond platform defaults.

### 3.1 Module-Specific Environment Constraints

No module-specific environment constraints beyond platform defaults.

## 4. Scope

### 4.1 In Scope

- Session lifecycle management (create, delete, retrieve)
- Message routing to backend plugins with real-time streaming
- Message variant preservation (regeneration, branching)
- File attachment references in messages
- Session type switching mid-conversation
- Session export (JSON, Markdown, TXT)
- Session sharing via links with read-only and branching access
- Message search within sessions and across sessions
- Message tree navigation and variant selection
- Extensible domain model schemas — plugin vendors can define custom message types, content types, event types, and error types on top of the predefined base schemas, enabling custom scenarios without forking Chat Engine core (see FR-021)

### 4.2 Out of Scope


<!-- fdd-id-content -->
The following are explicitly out of scope for Chat Engine:
- Message content processing, analysis, or moderation (handled by backend plugins)
- User authentication and identity management (handled by client applications)
- File upload/download implementation (handled by external file storage service)
- UI rendering and conversation visualization (handled by client applications)
- Rate limiting per user or organization (handled by client applications or API gateway)
- Billing, usage tracking, and quota management (separate service)
- Real-time collaboration features (multiple users in same session)
- Message encryption at rest (delegated to database service)
- Content delivery network (CDN) integration for file serving
<!-- fdd-id-content -->

## 5. Functional Requirements

### 5.1 Core Session & Messaging

#### FR-001: Create Session

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-fr-create-session`

<!-- fdd-id-content -->
The system **MUST** create a new session with a specified session type and client ID. The system binds each session to the requesting user (`user_id`) and tenant (`tenant_id`), both extracted from the JWT bearer token — they are never accepted from the request body. The system notifies the backend plugin of the new session and receives `enabled_capabilities` confirmed by the plugin for this session. The enabled capabilities determine which features are active (file attachments, session switching, summarization, etc.).

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-002: Send Message with Streaming Response

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-fr-send-message`

<!-- fdd-id-content -->
The system **MUST** forward user messages to backend plugin with full session context (session metadata, capabilities, message history) and stream responses back to client in real-time. The system persists the complete message exchange (user message and assistant response) after streaming completes.

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-003: Attach Files to Messages

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-fr-attach-files`

<!-- fdd-id-content -->
The system **MUST** support file references in messages. Clients upload files to File Storage Service, obtain file UUIDs (stable identifiers), and include these UUIDs in message payloads. The system stores UUIDs in message records and forwards them to backend plugins as part of message context. File handling is enabled only if session capabilities allow it.

**File Upload Workflow:**
1. Client calls File Storage Service upload endpoint
2. File Storage returns UUID as file identifier
3. Client includes UUID in message send request (file_ids array, max 10 files)
4. Chat Engine stores UUIDs in message record
5. Webhook backends receive UUIDs and fetch files from File Storage as needed

**File Access Control:**
- UUIDs are stable identifiers that do not expire
- File Storage Service controls access via separate authentication
- Webhook backends must have credentials for File Storage API
- Clients retrieve files by requesting temporary signed URLs from File Storage

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-file-storage`
<!-- fdd-id-content -->

#### FR-004: Switch Session Type

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-switch-session-type`

<!-- fdd-id-content -->
The system **SHOULD** allow switching to a different session type mid-session. When switching occurs, the next message is routed to the new backend plugin with full message history. The new backend returns updated capabilities which apply for subsequent messages.

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-005: Recreate Assistant Response

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-fr-recreate-response`

<!-- fdd-id-content -->
The system **MUST** allow regeneration of assistant responses. When recreation is requested, the old response is preserved as a variant in the message tree, and a new response is generated and stored as a sibling (same parent, different branch). Both variants remain accessible for navigation.

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-006: Branch from Message

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-branch-message`

<!-- fdd-id-content -->
The system **SHOULD** allow creating new messages from any point in conversation history, creating alternative conversation paths. When branching, the system loads context up to the specified parent message and forwards the new message to the backend with truncated history. Both conversation branches remain preserved.

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-007: Navigate Message Variants

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-navigate-variants`

<!-- fdd-id-content -->
The system **SHOULD** allow navigation between message variants (siblings with same parent message). When retrieving messages, the system provides variant position information (e.g., "2 of 3") and allows clients to request specific variants.

Webhook backends receive message history with file_ids (UUIDs). Backends must implement File Storage Service client to fetch file content when needed.

**Actors**: `cpt-cf-chat-engine-actor-client`
<!-- fdd-id-content -->

#### FR-008: Stop Streaming Response

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-fr-stop-streaming`

<!-- fdd-id-content -->
The system **MUST** allow canceling streaming responses mid-generation. When cancellation occurs, the system stops forwarding data from backend plugin, closes the connection, and saves the partial response as an incomplete message with appropriate metadata.

**Actors**: `cpt-cf-chat-engine-actor-client`
<!-- fdd-id-content -->

#### FR-009: Export Session

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-fr-export-session`

<!-- fdd-id-content -->
The system **MAY** export sessions in JSON, Markdown, or TXT format. Export can include only the active conversation path or all message variants. The system uploads the formatted export to file storage and returns a download URL.

**Actors**: `cpt-cf-chat-engine-actor-client`
<!-- fdd-id-content -->

#### FR-010: Share Session

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-fr-share-session`

<!-- fdd-id-content -->
The system **MAY** generate shareable links for sessions. Recipients can view sessions in read-only mode and create branches from the last message in the session. Branches created by recipients do not affect the original session owner's conversation path.

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-end-user`
<!-- fdd-id-content -->

#### FR-011: Session Summary

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-session-summary`

<!-- fdd-id-content -->
The system **SHOULD** support session summarization if enabled by session type capabilities. Summary generation is triggered automatically or on demand and can be handled by the backend plugin or a dedicated summarization service. The summary is stored as session metadata.

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-016: Conversation Memory Management Strategies

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-conversation-memory`

<!-- fdd-id-content -->
The system **SHOULD** provide guidance and capabilities to support conversation memory management strategies for handling long-running sessions that exceed backend processing capacity limits. Backend plugins can implement various strategies to manage context depth while preserving conversation quality.

**Memory Management Strategies**:
1. **Full History** (default) - Send complete message history (suitable for short conversations)
2. **Sliding Window** - Keep last N messages (predictable context depth)
3. **Summarization + Recent** - Summarize old messages, keep recent ones verbatim
4. **Importance Filtering** - Keep semantically important messages, filter filler
5. **Hierarchical Summarization** - Multi-level summaries for very long conversations
6. **Visibility Flags** - Use `is_hidden_from_backend` to exclude messages from context

**System Capabilities Supporting Strategies**:
- Session Summary (FR-011) - Backend can request conversation summaries
- Message Visibility Flags - Mark messages as `is_hidden_from_backend=true` to exclude from context
- Branching (FR-006) - Create new conversation path with truncated history
- Message Tree Navigation - Backends can traverse history to implement custom strategies
- Session Metadata - Store strategy configuration and state (e.g., last summarization point)

**Backend Responsibilities**:
- Choose appropriate strategy based on session type and conversation length
- Implement context depth management and history filtering logic
- Handle summarization or filtering logic
- Store strategy state in session metadata if needed
- Monitor context depth and adjust strategy dynamically

**Strategy Selection Guidelines**:
- **<50 messages**: Full History (default)
- **50-200 messages**: Sliding Window or Visibility Flags
- **200-1000 messages**: Summarization + Recent Messages
- **1000+ messages**: Hierarchical Summarization or Importance Filtering
- **Backend context limits**: Adjust strategy based on backend processing capacity

**Trade-offs**:
- **Full History**: High fidelity but expensive for long conversations
- **Sliding Window**: Predictable costs but loses older context
- **Summarization**: Balanced approach but adds summarization overhead
- **Importance Filtering**: Optimal quality but complex to implement

**Actors**: `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-017: Individual Message Deletion

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-fr-delete-message`

<!-- fdd-id-content -->
The system **MUST** support deletion of individual messages within a session. When a message is deleted, all associated reactions are cascade-deleted automatically to maintain referential integrity. The system validates ownership (authenticated user must own the message) before deletion and notifies the backend plugin of the deletion event.

**Deletion Behavior**:
- **Hard delete only**: Messages are permanently removed (no soft delete for individual messages)
- **Cascade delete reactions**: All reactions associated with the message are automatically deleted
- **Ownership validation**: Only the message owner (authenticated user) can delete their messages
- **Webhook notification**: Backend receives message_deleted event with message_id and timestamp
- **Error handling**: Returns 403 Forbidden if user doesn't own message, 404 if message not found

**Use Cases**:
- User wants to remove a message they regret sending
- User wants to clean up test messages or mistakes
- User wants to remove sensitive information accidentally shared

**Constraints**:
- Cannot delete messages that are parent to other messages (would break conversation tree)
- Cannot delete assistant messages (only user messages can be deleted by users)
- Deletion is permanent and cannot be undone

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-018: Per-Message Feedback

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-message-feedback`

<!-- fdd-id-content -->
The system **SHOULD** support per-message feedback in the form of like/dislike reactions and optional text comments. Feedback enables quality monitoring, response quality evaluation, and user satisfaction tracking. Each message can have at most one reaction per user, with reaction changes (like → dislike) replacing the previous reaction. The system stores feedback metadata and optionally forwards it to backend plugins for analytics.

**Reaction Types**:
- **like**: Positive feedback (thumbs up)
- **dislike**: Negative feedback (thumbs down)
- **none**: Remove existing reaction

**Behavioral Rules**:
- Each message can have at most one reaction per user
- Reaction changes (like → dislike) replace the previous reaction
- Optional text comment supported per reaction (bounded length)
- Reactions are tied to authenticated users (not anonymous)

**Webhook Integration**:
- Backends receive `message_feedback` events when reactions are added/changed
- Events include message_id, reaction_type, comment, user_id, timestamp
- Backends can use feedback for backend optimization, quality metrics, A/B testing

**Privacy & Data Handling**:
- Feedback is tied to authenticated user (not anonymous)
- Comments are stored encrypted if they contain sensitive information
- Feedback can be queried by client for display or exported with session data

**Capability Gating**: Enabled if session type supports feedback capability

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-019: Context Overflow Strategies

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-context-overflow`

<!-- fdd-id-content -->
The system **SHOULD** provide explicit support for handling context overflow when message history exceeds backend processing capacity. Chat Engine provides primitives and metadata to enable backend plugins to implement various overflow strategies. The system does not enforce a specific strategy but provides the mechanisms for backends to implement their chosen approach.

**Supported Strategy Primitives**:

1. **Sliding Window**: Keep only the most recent N messages to bound context size
2. **Hard Stop**: Reject new messages when the session exceeds a configured message count threshold
3. **Drop-Middle**: Retain the beginning and end of the conversation, dropping the middle portion
4. **Summarization**: Use `cpt-cf-chat-engine-fr-session-summary` to compress older messages into a summary that is included instead of verbatim history
5. **Message Visibility Flags**: Mark individual messages with `is_hidden_from_backend` to exclude them from context sent to backends

**System Support**:
- Session metadata exposes estimated message count and processing metrics for backend decision-making
- Session metadata stores strategy configuration and state between messages
- Message tree navigation supports arbitrary history traversal by backends
- `cpt-cf-chat-engine-fr-session-summary` provides summarization capability

**Default Strategy**: Full History (send all messages until overflow, then error)

**Backend Selection Guidelines**:
- **Short sessions (<50 msgs)**: Full History
- **Medium sessions (50-200 msgs)**: Sliding Window
- **Long sessions (200-1000 msgs)**: Summarization + Recent
- **Very long sessions (1000+ msgs)**: Hierarchical Summarization or Drop-Middle

**Strategy Trade-offs**:
- **Full History**: Highest fidelity, expensive for long conversations
- **Sliding Window**: Predictable costs, loses context over time
- **Summarization**: Balanced approach, adds latency for summary generation
- **Drop-Middle**: Preserves key context, may lose important middle details
- **Hard Stop**: Simplest, worst UX (forces session restart)

**Capability Gating**: Strategy configuration exposed via session capabilities

**Actors**: `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-020: Message Retention & Cleanup Policies

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-message-retention`

<!-- fdd-id-content -->
The system **SHOULD** support message-level retention policies that automatically clean up old messages while preserving session structure. Unlike session deletion (FR-014), message retention policies allow selective message cleanup to optimize storage costs while keeping sessions accessible. Cleanup operations preserve message tree integrity and notify backend plugins.

**Message Retention Behavior**:
- **Age-based cleanup**: Delete messages older than N days
- **Count-based cleanup**: Keep only last N messages per session
- **Selective cleanup**: Remove non-active conversation branches (unused variants)
- **Tree-aware deletion**: Preserve parent messages required for tree structure
- **Webhook notification**: Backend receives `messages_cleaned` event with deleted message IDs

**Retention Policy Configuration** (per session type):
- `message_retention_days`: Auto-delete messages older than N days (default: null/unlimited)
- `max_messages_per_session`: Keep only last N messages (default: null/unlimited)
- `cleanup_inactive_branches`: Remove unused message variants (default: false)
- `preserve_favorited`: Keep messages marked with feedback reactions (default: true)

**Cleanup Execution**:
- Automated job runs daily to enforce retention policies
- Cleanup preserves active conversation path (marked by is_active=true)
- Parent messages required for tree structure are never deleted (even if old)
- Webhook backends notified asynchronously after cleanup completes

**Use Cases**:
- Reduce storage costs for long-running sessions with thousands of messages
- Comply with data minimization regulations (GDPR, CCPA)
- Clean up experimental branches that users never navigate to
- Archive old conversations while keeping recent context accessible

**Constraints**:
- Cannot delete messages that are parents to active messages (breaks tree)
- Cannot delete messages with pending operations or incomplete streaming
- Cleanup respects session lifecycle state (no cleanup on soft_deleted sessions)

**Integration with Session Retention**:
- Session retention (FR-014e) operates at session level (all-or-nothing)
- Message retention operates within active sessions (selective cleanup)
- When session is deleted, all messages are deleted (session takes precedence)

**Actors**: `cpt-cf-chat-engine-actor-system`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-012: Search Session History

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-fr-search-session`

<!-- fdd-id-content -->
The system **MAY** search within a single session's message history and return matching messages with surrounding context. Search supports text matching across all message roles (user and assistant).

**Actors**: `cpt-cf-chat-engine-actor-client`
<!-- fdd-id-content -->

#### FR-013: Search Across Sessions

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-fr-search-sessions`

<!-- fdd-id-content -->
The system **MAY** search across all sessions belonging to a client and return ranked results with session metadata (session ID, title, timestamp, match context). Results are ordered by relevance.

**Actors**: `cpt-cf-chat-engine-actor-client`
<!-- fdd-id-content -->

#### FR-014: Session Lifecycle Management

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-fr-delete-session`

<!-- fdd-id-content -->
The system **MUST** support session lifecycle management with four states: active, archived, soft_deleted, and hard_deleted. Sessions transition through these states based on user actions or retention policies. Each lifecycle transition notifies backend plugins to enable synchronized resource management.

**Lifecycle States:**
- **active** - Normal operational state (default)
- **archived** - Inactive sessions optimized for long-term storage
- **soft_deleted** - Deleted but recoverable within retention period
- **hard_deleted** - Permanently removed from database

**Operations:** Detailed in FR-014a (soft delete), FR-014b (hard delete), FR-014c (restore), FR-014d (archive), and FR-014e (retention policies).

**State Inheritance:** Messages inherit lifecycle_state from their session and transition together to maintain referential integrity.

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-014a: Soft Delete Session (Recoverable)

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-fr-soft-delete-session`

<!-- fdd-id-content -->
The system **MUST** support soft deletion as the default deletion mechanism. Soft-deleted sessions are hidden from normal queries but remain in the system and can be restored within a retention period. The system notifies backend plugins of soft deletion, allowing them to cleanup or suspend associated resources. Sessions automatically transition to permanent deletion after the retention period expires unless restored.

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-014b: Hard Delete Session (Permanent)

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-fr-hard-delete-session`

<!-- fdd-id-content -->
The system **MUST** support permanent hard deletion that irreversibly removes sessions and all associated messages. Hard deletion is triggered explicitly by user request or automatically when soft-deleted sessions reach their retention period expiry. The system notifies backend plugins of permanent deletion, requiring them to cleanup all external resources (files, analytics, indices). This supports data minimization requirements (GDPR, CCPA).

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`, `cpt-cf-chat-engine-actor-system`
<!-- fdd-id-content -->

#### FR-014c: Restore Soft-Deleted Session

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-restore-session`

<!-- fdd-id-content -->
The system **SHOULD** support restoring soft-deleted sessions back to active state. Restoration is only possible before the retention period expires. This enables recovery from accidental deletions. The system notifies backend plugins when sessions are restored, allowing them to reinstate any suspended resources. Hard-deleted sessions cannot be restored.

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-014d: Archive Inactive Sessions

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-fr-archive-session`

<!-- fdd-id-content -->
The system **MAY** support archiving inactive sessions to optimize database performance. Archived sessions remain accessible and queryable but may have reduced query performance. Archival can be triggered manually or automatically based on inactivity period. The system notifies backend plugins of lifecycle state changes. Archived sessions can transition back to active state when new activity occurs or be deleted.

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`, `cpt-cf-chat-engine-actor-system`
<!-- fdd-id-content -->

#### FR-014e: Retention Policy Configuration and Enforcement

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-retention-policy`

<!-- fdd-id-content -->
The system **SHOULD** support configurable retention policies that automatically manage session lifecycle based on age and inactivity. Retention policies enable automated data lifecycle management while balancing storage costs and compliance requirements. Policies are configured per session type and control automatic archival of inactive sessions, automatic hard deletion of soft-deleted sessions after grace period, and optional immediate deletion for compliance scenarios. The system processes retention policies periodically and notifies backend plugins of all lifecycle transitions.

**Actors**: `cpt-cf-chat-engine-actor-system`, Admin
<!-- fdd-id-content -->

#### FR-015: WebSocket Protocol Support

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-websocket-protocol`

<!-- fdd-id-content -->
The system **SHOULD** support WebSocket protocol as an alternative to HTTP streaming for client-server communication. Clients can connect via WebSocket and perform all operations (session management, message sending, streaming responses) over a persistent connection instead of HTTP REST endpoints.

**Protocol Features**:
- Persistent bidirectional connection using WebSocket (RFC 6455)
- JSON message framing for commands and NDJSON for streaming chunks
- Connection lifecycle management (authenticate, heartbeat, reconnect)
- All HTTP REST operations available via WebSocket commands
- Graceful degradation to HTTP if WebSocket unavailable

**Use Cases**:
- Clients preferring WebSocket client libraries
- Lower latency for rapid message exchanges
- Future bidirectional features (typing indicators, presence, notifications)
- Mobile apps with persistent connections

**Trade-offs**:
- Requires sticky session configuration for load balancing
- Adds connection state management complexity
- WebSocket proxy configuration needed in deployment
- Not compatible with serverless architectures

**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`
<!-- fdd-id-content -->

#### FR-021: Domain Model Schema Extensibility for Plugin Vendors

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-fr-schema-extensibility`

<!-- fdd-id-content -->
The system **SHOULD** provide extensible, versioned base schemas for all core domain model entities, enabling plugin vendors to derive custom schemas and implement their own scenarios on top of Chat Engine without modifying the engine itself.

**Extensible Domain Model Categories**:

| Category | Base Schemas | Extension Point |
|---|---|---|
| **Message content types** | `TextContent`, `ImageContent`, `AudioContent`, `VideoContent`, `DocumentContent`, `CodeContent` | Plugins declare custom `ContentPart` subtypes |
| **Event types** | `MessageNewEvent`, `SessionCreatedEvent`, `StreamingChunkEvent`, etc. | Plugins emit custom typed events via webhook response extensions |
| **Error types** | `ErrorResponse`, `ErrorCode` | Plugins define domain-specific error codes in the `ErrorCode` enum space |
| **Session / Message metadata** | `Session.metadata`, `Message.metadata` | Plugins store and validate typed custom metadata blobs |

**Plugin Schema Contract**:
- Chat Engine publishes base schemas via its GTS schema registry
- Plugin vendors reference base schemas and declare derived schemas using the same GTS ID format (`gts://gts.x.core.events.event.v1~{plugin-namespace}.{name}.v{N}~`)
- Chat Engine stores plugin-provided metadata in opaque `metadata` fields but **validates** the metadata blob against the declared derived schema
- Access control checks are applied to custom metadata based on the plugin's declared schema and the session/message ownership model

**Validation and Access Control**:
- When a plugin declares a derived schema for `Session.metadata` or `Message.metadata`, Chat Engine registers it and validates incoming metadata against it
- Plugins cannot override or extend base schema required fields; they extend via the open `metadata` fields only
- Read access to custom metadata follows the same tenant/user isolation as the parent entity
- Write access is gated by the plugin's JWT `client_id` claim — plugins may only write to metadata they own

**Non-Goals**:
- Chat Engine does not execute plugin code, only validates schemas and enforces access
- Plugin schemas are purely structural (JSON Schema); behavioral logic stays in backend plugins
- Modification of base Chat Engine schemas (fields, enums) is not allowed; only `metadata` fields are extensible

**Actors**: `cpt-cf-chat-engine-actor-backend-plugin`, `cpt-cf-chat-engine-actor-tenant-admin`
<!-- fdd-id-content -->

## 6. Non-Functional Requirements

#### NFR-001: Response Time

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-nfr-response-time`

<!-- fdd-id-content -->
Message routing latency must be less than 100ms at p95, measured from receiving client message to forwarding to backend plugin (excluding backend processing time). Session creation must complete within 200ms at p95, including database write and backend notification.
<!-- fdd-id-content -->

#### NFR-002: Availability

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-nfr-availability`

<!-- fdd-id-content -->
System must maintain 99.9% uptime for session management operations (create, retrieve, delete sessions). During backend plugin failures, the system must support degraded mode with read-only access to session history. Planned maintenance windows must be scheduled during low-traffic periods with advance notice.
<!-- fdd-id-content -->

#### NFR-003: Scalability

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-nfr-scalability`

<!-- fdd-id-content -->
System must support at least 10,000 concurrent active sessions per instance. Message throughput must support at least 1,000 messages per second per instance. System must support horizontal scaling by adding instances without shared state constraints.
<!-- fdd-id-content -->

#### NFR-004: Data Persistence

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-nfr-data-persistence`

<!-- fdd-id-content -->
All messages must be persisted to database before sending acknowledgment to client. Zero message loss is required during system failures, network interruptions, or backend failures. Database writes must use ACID transactions to ensure consistency.
<!-- fdd-id-content -->

#### NFR-005: Streaming Performance

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-nfr-streaming`

<!-- fdd-id-content -->
Streaming latency overhead (time between receiving chunk from backend and forwarding to client) must be less than 10ms at p95. First byte of streamed response must arrive at client within 200ms of backend starting to stream. Streaming must support backpressure to handle slow clients.
<!-- fdd-id-content -->

#### NFR-006: Authentication

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-nfr-authentication`

<!-- fdd-id-content -->
System must authenticate all client requests using JWT bearer tokens. Each token must carry `client_id`, `user_id`, and `tenant_id` claims, all extracted server-side and never accepted from request body. Session access must be restricted to the owning user (`user_id` match) within the owning tenant (`tenant_id` match), or to share token holders for read-only access. All data queries must be scoped by `tenant_id` to ensure tenant isolation.
<!-- fdd-id-content -->

#### NFR-007: Data Integrity

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-nfr-data-integrity`

<!-- fdd-id-content -->
Message tree structure must maintain referential integrity at all times. Orphaned messages (messages with non-existent parent) are not allowed. Parent-child relationships must be immutable once created. Database constraints must enforce tree structure integrity.
<!-- fdd-id-content -->

#### NFR-008: Backend Isolation

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-nfr-backend-isolation`

<!-- fdd-id-content -->
Webhook backend failures must not affect other sessions using different backends. Request timeout must be configurable per session type with a default of 30 seconds. Backend errors must be isolated and logged without cascading to other system components.
<!-- fdd-id-content -->

#### NFR-009: File Size Limits

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-nfr-file-size`

<!-- fdd-id-content -->
System must enforce file size limits with a default of 10MB per individual file. Total attachments per message must be limited to 50MB. File size validation occurs at client upload time (enforced by file storage service) and limits are configurable per session type.
<!-- fdd-id-content -->

#### NFR-010: Search Performance

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-nfr-search`

<!-- fdd-id-content -->
Session history search must return results within 1 second at p95 for sessions with up to 10,000 messages. Cross-session search must return results within 3 seconds at p95 for clients with up to 1,000 sessions. Search must support pagination for large result sets.
<!-- fdd-id-content -->

#### NFR-011: WebSocket Performance

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-nfr-websocket-performance`

<!-- fdd-id-content -->
When WebSocket is enabled, connection establishment must complete within 500ms at p95. Message routing latency over WebSocket must be less than 50ms at p95 (lower than HTTP's 100ms target). Heartbeat interval must be 30 seconds with automatic reconnection using exponential backoff (maximum 60 seconds). The system must support at least 5,000 concurrent WebSocket connections per instance.
<!-- fdd-id-content -->

#### NFR-012: WebSocket Reliability

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-nfr-websocket-reliability`

<!-- fdd-id-content -->
When WebSocket is enabled, connections must support automatic reconnection with state restoration after network interruptions. Message delivery guarantees must match HTTP protocol (at-least-once for operations, exactly-once for streaming). The system must handle graceful connection closure with pending operation completion or cancellation. Connection timeout must be 5 minutes for idle connections, configurable per deployment.
<!-- fdd-id-content -->

#### NFR-013: Message History Handling

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-nfr-message-history`

<!-- fdd-id-content -->
System must support sessions with up to 10,000 messages without performance degradation. Message history forwarding to backend plugins must complete within 2 seconds at p95 for sessions with 1,000 messages. Backends must implement conversation memory management strategies when approaching their processing capacity limits. System must provide message count and estimated processing metrics in session metadata to help backends make memory management decisions.
<!-- fdd-id-content -->

#### NFR-014: Lifecycle Operation Performance

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-nfr-lifecycle-performance`

<!-- fdd-id-content -->
Lifecycle operations (soft delete, restore, archive) must complete within 500ms at p95 for sessions with up to 10,000 messages. Hard delete operations may take up to 5 seconds at p95 for large sessions. Restoration must preserve complete session state including message tree structure, metadata, and file references. Lifecycle state transitions must be atomic.
<!-- fdd-id-content -->

#### NFR-015: Retention Policy Enforcement SLA

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-nfr-retention-sla`

<!-- fdd-id-content -->
Automatic retention policy enforcement must run at least daily. Sessions must transition to permanent deletion within 24 hours of reaching their retention period expiry. Policy processing must handle at least 10,000 sessions per run without impacting production query performance (p95 latency increase <10%). Failed operations must retry and alert on repeated failures.
<!-- fdd-id-content -->

#### NFR-016: Recovery Requirements

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-nfr-recovery`

<!-- fdd-id-content -->
Recovery objectives for Chat Engine persistent data:

- **RPO (Recovery Point Objective)**: ≤ 5 minutes — maximum acceptable data loss window in the event of a catastrophic failure
- **RTO (Recovery Time Objective)**: ≤ 30 minutes — maximum acceptable downtime before service is restored to degraded mode; ≤ 2 hours for full recovery
- **Backup frequency**: Session and message data must be backed up at minimum every 5 minutes via continuous WAL shipping or equivalent
- **Backup retention**: Backups must be retained for at least 30 days
- **Point-in-time recovery**: Database must support point-in-time recovery to any point within the backup retention window
- **Atomic lifecycle transitions**: All session lifecycle state transitions must be ACID-compliant; partial transitions are not acceptable
<!-- fdd-id-content -->

#### NFR-017: Developer Experience

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-nfr-developer-experience`

<!-- fdd-id-content -->
Chat Engine's primary users are Application Developers and Backend Plugin Developers. Integration quality is a core product metric:

- **Time-to-first-message**: A developer familiar with REST APIs must be able to send a first message within ≤ 30 minutes of reading the API documentation, without prior Chat Engine knowledge
- **Error response quality**: All API errors must return structured responses with: machine-readable error code, human-readable message, and actionable remediation hint
- **API documentation**: A complete OpenAPI specification must be published and kept up-to-date with every API change
- **Webhook contract documentation**: Webhook backend developers must have a documented contract covering all event types, payload schemas, and expected response formats
- **Client SDK**: At minimum one reference client SDK must be provided (language TBD) demonstrating session creation, message exchange, and streaming
<!-- fdd-id-content -->

### 6.1 NFR Exclusions

No explicit NFR exclusions at this time. All listed NFRs are in scope.

## 7. Public Library Interfaces

### 7.1 Public API Surface

See [api/README.md](../api/README.md) for the public HTTP REST and Webhook API specifications.

See [schemas/README.md](../schemas/README.md) for JSON schema definitions.

### 7.2 External Integration Contracts

Backend Plugin integration is defined through the `ChatEngineBackendPlugin` trait (see DESIGN). No external HTTP integration contracts beyond the plugin interface.

## 8. Use Cases

#### UC-001: Create Session and Send First Message

**ID**: `cpt-cf-chat-engine-usecase-create-session`

<!-- fdd-id-content -->
**Actor**: `cpt-cf-chat-engine-actor-client`

**Preconditions**: Client has valid session type ID and client ID

**Flow**:
1. Client requests session creation with session type ID and client ID
2. System creates session record in database with unique session ID
3. System notifies backend plugin of session creation with session metadata
4. Backend processes creation notification and returns `enabled_capabilities` — typed `Capability` definitions (bool/enum/str/int with default values) confirmed for this session
5. System stores `enabled_capabilities` in session record and returns session ID with capability list to client
6. Client sends first message with `enabled_capabilities` — a list of `CapabilityValue` objects (`{id, value}`) specifying per-message capability settings
7. System validates capability IDs in request against session's `enabled_capabilities`
8. System forwards message to backend with full context (session metadata, `CapabilityValue` list, empty message history)
9. Backend processes message and streams response
10. System streams response chunks to client in real-time
11. System stores complete message exchange in database

**Postconditions**: Session created with unique ID, capabilities stored, first message exchanged and persisted

**Acceptance criteria**:
- Session ID returned to client within 200ms of creation request
- Capabilities list correctly stored and accessible for subsequent messages
- First message routed to correct backend plugin based on session type
- Streaming response delivered to client without data loss
- Complete message exchange persisted in database before acknowledgment
<!-- fdd-id-content -->

#### UC-002: Recreate Assistant Response

**ID**: `cpt-cf-chat-engine-usecase-recreate-response`

<!-- fdd-id-content -->
**Actor**: `cpt-cf-chat-engine-actor-client`

**Preconditions**: Session exists with at least one assistant message

**Flow**:
1. Client requests recreation of last assistant response, specifying message ID
2. System validates that the specified message exists and is an assistant message
3. System identifies the parent message of the assistant message to recreate
4. System loads message history up to and including the parent message
5. System sends recreation request to backend plugin with context (message history, session metadata, capabilities)
6. Backend generates new response based on context
7. System streams new response chunks to client in real-time
8. System stores new response as a sibling of the original response (same parent message ID)
9. System marks the new response as the active variant
10. System returns variant information to client (e.g., "variant 2 of 2")

**Postconditions**: New response variant created and stored, both variants preserved and navigable, new variant marked as active

**Acceptance criteria**:
- Old response remains unchanged in database
- New response has same parent message ID as old response
- Client receives variant position information
- Both variants can be retrieved and navigated
- Message tree integrity maintained (no orphaned messages)
<!-- fdd-id-content -->

#### UC-003: Branch from Historical Message

**ID**: `cpt-cf-chat-engine-usecase-branch-message`

<!-- fdd-id-content -->
**Actor**: `cpt-cf-chat-engine-actor-client`

**Preconditions**: Session exists with message history containing at least one message

**Flow**:
1. Client selects a message in history to branch from (parent message ID)
2. Client sends new message with specified parent message ID
3. System validates parent message exists in session
4. System loads message history from session start up to and including parent message
5. System forwards message to backend plugin with truncated context
6. Backend processes message with historical context (ignoring messages after parent)
7. System streams response chunks to client in real-time
8. System stores new message with parent reference
9. System stores assistant response with new message as parent
10. System marks new branch as active path
11. Client can navigate between original path and new branch

**Postconditions**: New conversation branch created starting from specified message, both paths preserved, new branch marked as active

**Acceptance criteria**:
- New message has correct parent message ID reference
- Context sent to backend includes only messages up to parent
- Both conversation branches preserved in database
- Both branches navigable by client
- No data loss in original conversation path
- Message tree structure maintains referential integrity
<!-- fdd-id-content -->

#### UC-004: Export Session

**ID**: `cpt-cf-chat-engine-usecase-export-session`

<!-- fdd-id-content -->
**Actor**: `cpt-cf-chat-engine-actor-client`

**Preconditions**: Session exists with at least one message

**Flow**:
1. Client requests export with specified format (JSON, Markdown, or TXT) and scope (active path only or all variants)
2. System validates session exists and client has access
3. System retrieves session messages according to scope:
   - Active path only: follows current active variant chain
   - All variants: retrieves entire message tree
4. System formats data according to requested format:
   - JSON: structured data with message tree relationships
   - Markdown: human-readable format with message roles and content
   - TXT: plain text format with minimal formatting
5. System generates formatted file content
6. System uploads formatted file to file storage service
7. File storage returns signed URL with expiration
8. System returns download URL to client

**Postconditions**: Session exported to requested format, file uploaded to storage, download URL provided

**Acceptance criteria**:
- Export completes within 5 seconds for sessions with <1000 messages
- All message variants included if "all variants" scope requested
- Active path only includes messages in current variant chain if "active path" scope requested
- Generated file is valid and parseable according to format
- Download URL is accessible and valid for at least 24 hours
- File content accurately represents session data without loss
<!-- fdd-id-content -->

#### UC-005: Share Session

**ID**: `cpt-cf-chat-engine-usecase-share-session`

<!-- fdd-id-content -->
**Actor**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-end-user`

**Preconditions**: Session exists with at least one message

**Flow**:
1. Client requests shareable link for session
2. System generates unique share token and associates it with session ID
3. System returns shareable URL containing share token
4. Client shares URL with recipient
5. Recipient opens shared URL in client application
6. Client application sends share token to system
7. System validates share token and retrieves associated session ID
8. System returns session data in read-only mode to recipient
9. Recipient views session messages
10. Recipient sends new message in shared session
11. System creates new message branching from last message in session
12. System routes message to backend plugin with full history
13. Backend processes message and returns response
14. System stores new branch separately from original session path

**Postconditions**: Session shared via unique URL, recipient can view original messages and create branches, original session remains unchanged

**Acceptance criteria**:
- Share token is unique, secure, and not guessable
- Original session data cannot be modified by recipient
- Recipient's messages create new branch in message tree
- Recipient cannot modify or delete original messages
- Original session owner can still access and modify their conversation path
- Share link can be revoked by original owner
<!-- fdd-id-content -->

#### UC-006: Send Message and Receive Streaming Response

**ID**: `cpt-cf-chat-engine-usecase-send-message`

<!-- fdd-id-content -->
**Actor**: `cpt-cf-chat-engine-actor-client`

**Preconditions**: Session exists in active state; client has valid session ID

**Flow**:
1. Client sends message with content and optional file attachment UUIDs
2. System validates session is active and client owns or has access to the session
3. System validates file UUIDs against session capabilities (if file attachments enabled)
4. System persists user message to database and assigns message ID
5. System loads full message history for the session (respecting `is_hidden_from_backend` flags)
6. System forwards message to backend plugin with: session metadata, capabilities, message history, new message content
7. Backend begins processing and streams response chunks
8. System forwards each chunk to client in real-time
9. Upon stream completion, system persists assistant message to database
10. System acknowledges message exchange to client with message IDs

**Postconditions**: User and assistant messages persisted; client receives complete streaming response

**Alternative Flows**:
- **Client cancels mid-stream**: System stops forwarding, saves partial response with incomplete status (see `cpt-cf-chat-engine-fr-stop-streaming`)
- **Webhook backend timeout**: System closes stream, saves error message with timeout metadata, returns 504 to client
- **Webhook backend returns error**: System saves error message, propagates structured error to client
<!-- fdd-id-content -->

#### UC-007: Delete Session

**ID**: `cpt-cf-chat-engine-usecase-delete-session`

<!-- fdd-id-content -->
**Actor**: `cpt-cf-chat-engine-actor-client`

**Preconditions**: Session exists; client owns the session

**Flow (Soft Delete)**:
1. Client requests session soft-deletion
2. System validates ownership (client ID matches session owner)
3. System transitions session to `soft_deleted` state
4. System hides session from normal queries
5. System notifies backend plugin of soft-deletion event
6. System returns success to client with retention period expiry timestamp

**Flow (Hard Delete)**:
1. Client requests permanent hard-deletion (or retention period expires)
2. System transitions session to `hard_deleted` state
3. System permanently removes all session messages and metadata from database
4. System notifies backend plugin with `session.hard_deleted` event (backend must clean up external resources)
5. System returns success to client

**Postconditions**: Session hidden (soft) or permanently removed (hard); backend plugin notified

**Alternative Flows**:
- **Client requests restore within retention period**: Session transitions back to `active` (see `cpt-cf-chat-engine-fr-restore-session`)
- **Session not found**: System returns 404
- **Client does not own session**: System returns 403 Forbidden
<!-- fdd-id-content -->

#### UC-008: Backend Failure During Streaming

**ID**: `cpt-cf-chat-engine-usecase-backend-failure`

<!-- fdd-id-content -->
**Actor**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`

**Preconditions**: Session active; message forwarded to backend; streaming in progress

**Flow**:
1. Backend connection drops or returns an error mid-stream
2. System detects connection failure or error response from backend
3. System stops forwarding chunks to client
4. System saves partial response with `incomplete` status and error metadata
5. System sends structured error event to client indicating streaming failure
6. Session remains in `active` state — client can retry or branch

**Postconditions**: Partial assistant message saved with error metadata; client notified of failure; session remains operational

**Alternative Flows**:
- **Timeout before first byte**: System returns 504 to client; no assistant message saved
- **Backend returns 503/429 (rate limit)**: System logs backend health event; client receives retryable error with backoff hint
- **All retries exhausted** (if retry configured for session type): System marks session backend as degraded; client can still read history (degraded mode per `cpt-cf-chat-engine-nfr-availability`)
<!-- fdd-id-content -->

## 9. Acceptance Criteria

**Success Criteria**:
- Message routing latency < 100ms (p95) excluding backend processing time
- 99.9% uptime for session management operations
- Support for 10,000 concurrent sessions per instance
- Zero message loss during backend failures
- First message response time < 200ms from session creation

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| File Storage Service | External file storage for message attachments | p1 |
| Database Service | PostgreSQL for session and message persistence | p1 |
| Backend Plugin modules | Message processing implementations | p1 |

## 11. Assumptions


<!-- fdd-id-content -->
Key assumptions underlying this PRD:
- Webhook backends are always HTTP-accessible from Chat Engine instances
- Client applications handle all UI rendering of message trees and conversation visualization
- File storage service provides signed URL access with configurable expiration
- Database service supports ACID transactions and can handle write loads from concurrent sessions
- Network between Chat Engine and backend plugins is reliable (same region/VPC preferred)
- Client applications handle user authentication and pass validated client IDs to Chat Engine
- Webhook backends have reasonable response times (<30 seconds for most operations)
<!-- fdd-id-content -->

## 12. Risks


<!-- fdd-id-content -->
Identified risks and mitigation strategies:
- **Backend Plugin Latency**: Slow backends directly impact user experience. Mitigation: configurable timeouts per session type, monitoring and alerting for slow backends, consider caching for idempotent operations.
- **Database Contention**: High message volume may cause database write contention and slow queries. Mitigation: read replicas for query operations, connection pooling, query optimization, consider sharding by client ID.
- **Message Tree Complexity**: Deep branching (many variants or deep trees) may impact query performance and UI rendering. Mitigation: implement depth limits, pagination for variant navigation, database indexing on parent relationships.
- **File Storage Costs**: Unrestricted file attachments may lead to high storage costs. Mitigation: enforce file size limits, implement retention policies, consider compression for certain file types.
- **Session Abandonment**: Large numbers of inactive sessions may consume database resources. Mitigation: implement session cleanup policies, archive old sessions, monitor active session metrics.
<!-- fdd-id-content -->

## 13. Additional Context

#### Integration with Backend Plugins


<!-- fdd-id-content -->
Backend plugins receive session context (session metadata, capabilities, message history) and return responses. Backends are responsible for all message processing logic, enabling flexible implementations including automated chat (e.g. LLMs), rule-based systems, human-in-the-loop support, or hybrid approaches. The backend contract is designed to be implementation-agnostic, allowing easy experimentation with different processing approaches.
<!-- fdd-id-content -->

#### Message Tree Structure


<!-- fdd-id-content -->
Messages form a tree structure where each message (except the root) references a parent message. This tree structure enables conversation branching and message variant preservation. Multiple sibling messages with the same parent represent variants (alternative responses). The client application is responsible for rendering the tree structure in UI and providing navigation controls. The system maintains tree integrity but does not enforce a specific UI representation.
<!-- fdd-id-content -->

#### Message Visibility Control


<!-- fdd-id-content -->
Messages can be selectively hidden from users or backend plugins using visibility flags:

- **`is_hidden_from_user`** (boolean): When true, the message is excluded from client-facing APIs and UI rendering. The message remains in the database and message tree but is not returned to clients. Use cases include system prompts, backend configuration messages, and internal tracking notes.

- **`is_hidden_from_backend`** (boolean): When true, the message is excluded from the context history sent to backend plugins during message processing. The message is still visible to users (unless also hidden via `is_hidden_from_user`) but does not affect backend processing. Use cases include user feedback, debug messages, and messages that should not affect conversation context.

These flags enable flexible message handling patterns:
- **System prompts**: `is_hidden_from_user=true, is_hidden_from_backend=false` - Configure backend behaviour without showing configuration to users
- **Internal notes**: `is_hidden_from_user=true, is_hidden_from_backend=true` - Store metadata or debug information without affecting UI or backend
- **User feedback**: `is_hidden_from_user=false, is_hidden_from_backend=true` - Show user messages in UI but exclude from backend context (e.g., rating messages)
- **Normal messages**: `is_hidden_from_user=false, is_hidden_from_backend=false` - Standard visible messages that are part of conversation flow
<!-- fdd-id-content -->

#### Conversation Memory Management


<!-- fdd-id-content -->
Chat Engine forwards complete message history to backend plugins by default, enabling backends to implement their own memory management strategies. For long conversations that exceed backend processing capacity, backends should implement strategies such as sliding windows, summarization, or importance filtering.

The system provides building blocks for memory management:
- **Session Summary (FR-011)**: Request conversation summaries at any point
- **Message Visibility Flags**: Mark messages to exclude from backend context
- **Branching (FR-006)**: Create new conversation paths with truncated history
- **Session Metadata**: Store strategy state and configuration

Backends are responsible for:
- Monitoring conversation length and processing metrics
- Choosing appropriate strategy for session type
- Implementing context depth management and history filtering
- Storing strategy state in session metadata

Common strategies include sending only recent messages (sliding window), summarizing older messages while keeping recent ones verbatim, or filtering messages by semantic importance.
<!-- fdd-id-content -->

#### Session Lifecycle State Flow


<!-- fdd-id-content -->
Sessions and messages progress through four lifecycle states that control visibility, accessibility, and storage optimization:

**Lifecycle States:**

1. **active** (default) - Normal operational state. Sessions are visible in queries and fully accessible. Messages can be sent and received.

2. **archived** - Inactive sessions optimized for long-term storage. Sessions remain queryable but may have reduced performance.

3. **soft_deleted** - Deleted but recoverable. Sessions are hidden from normal queries but remain in the system. Can be restored before retention period expires.

4. **hard_deleted** - Permanently removed. Cannot be recovered.

**State Transition Flows:**

Common transitions:
- active → soft_deleted (user deletion) → hard_deleted (retention policy or explicit)
- active → archived (inactivity) → soft_deleted (deletion) → hard_deleted
- soft_deleted → active (restoration, before expiry)
- archived → active (new activity or manual restore)

**State Inheritance:**
Messages inherit lifecycle state from their session. When a session transitions, all its messages transition together to maintain referential integrity.

**Webhook Events:**
The system notifies backend plugins of all lifecycle transitions (`session.soft_deleted`, `session.hard_deleted`, `session.restored`, `session.lifecycle_changed`) to enable synchronized resource management.
<!-- fdd-id-content -->

#### Retention Policy Design Philosophy


<!-- fdd-id-content -->
Retention policies enable automated data lifecycle management while balancing user safety, storage costs, and compliance requirements. The design prioritizes safety and flexibility over aggressive data deletion.

**Design Principles:**

1. **Safety by Default**
   - Soft delete is the default deletion mechanism
   - Grace period before permanent deletion protects against accidental data loss
   - Hard delete requires explicit action or policy configuration

2. **Flexibility Over Rigidity**
   - Policies configured per session type (not global)
   - Policies can be disabled for manual lifecycle management
   - Different retention periods for different use cases

3. **Compliance Support**
   - Automatic hard delete supports data minimization (GDPR, CCPA)
   - Configurable retention periods meet regulatory requirements
   - Audit trail via webhook events for compliance reporting
   - Immediate deletion option for right-to-erasure requests

4. **Performance Optimization**
   - Archival separates active and inactive data
   - Automatic cleanup reduces storage growth over time
   - Lifecycle operations maintain system performance at scale

**Use Cases:**
- **Temporary chat**: Short inactivity threshold, moderate retention period
- **Support tickets**: Long inactivity threshold, extended retention for audit
- **Legal compliance**: Minimal retention, automatic cleanup enabled
- **User data (GDPR)**: Moderate thresholds, automatic cleanup for data minimization
<!-- fdd-id-content -->

#### Privacy by Design


<!-- fdd-id-content -->
Chat Engine processes user messages and user identifiers on behalf of client applications. Privacy requirements are embedded by design:

**Personal Data Handled**:
- User identifiers (client IDs passed by client applications)
- Message content (text, file attachment UUIDs)
- Per-message feedback (reaction type, optional comment text)
- Session metadata (timestamps, session type, lifecycle state)

**Data Minimization**: Chat Engine collects only the data operationally required to route messages and maintain session state. No analytics, profiling, or secondary use of message content occurs within Chat Engine.

**Purpose Limitation**: Message content is forwarded to backend plugins for processing purposes only. Chat Engine does not analyse or index message content for any other purpose.

**Privacy by Default**: Optional data collection (feedback comments, session metadata fields) is disabled unless explicitly enabled by session type capabilities.

**Data Subject Rights Support**: Hard-delete (`cpt-cf-chat-engine-fr-hard-delete-session`) supports the right to erasure (GDPR Art. 17). Client applications are responsible for accepting erasure requests from end users and forwarding them to Chat Engine.

**Responsibility Boundary**: Chat Engine acts as a **data processor** on behalf of client applications (the data controllers). Client applications are responsible for obtaining valid legal basis for processing user messages and for data subject consent where required.
<!-- fdd-id-content -->

#### Data Ownership


<!-- fdd-id-content -->
**Data Controller**: The client application that creates sessions and sends messages. The client application is responsible for obtaining user consent and establishing the legal basis for processing message content.

**Data Processor**: Chat Engine acts as a data processor on behalf of the client application. Chat Engine processes message data solely as instructed by the client application via the API.

**User-Generated Content**: Message content is owned by the end user who authored it, as represented through the client application. Chat Engine makes no claim to ownership of message content.

**Data Processing Agreement**: Client applications deploying Chat Engine in environments subject to GDPR or equivalent regulations must establish a Data Processing Agreement (DPA) governing Chat Engine's processing role.

**Third-Party Processors**: Webhook backends receive message content from Chat Engine. Client applications are responsible for ensuring their backend plugins also operate under appropriate data processing agreements.
<!-- fdd-id-content -->

## 14. Intentional Exclusions

The following checklist categories are **not applicable** to this PRD. Each is explicitly excluded with reasoning to distinguish intentional omission from oversight.

| Category | Status | Reason |
|----------|--------|--------|
| **Safety (SAFE-PRD-001/002)** | N/A | Chat Engine is a pure information API service with no physical interaction, no hardware control, and no potential for physical harm. ISO 25010:2023 Safety characteristic does not apply. |
| **Accessibility (UX-PRD-002)** | N/A | Chat Engine exposes a server-side REST/WebSocket API only — no user interface. Accessibility standards (WCAG) apply to client applications built on top of Chat Engine, not to Chat Engine itself. |
| **Internationalization (UX-PRD-003)** | N/A | Chat Engine is message-content-agnostic. It stores and forwards opaque text without interpreting language, encoding, or locale. I18n is the responsibility of client applications and backend plugins. |
| **Inclusivity (UX-PRD-005)** | N/A | Chat Engine has no user interface. Inclusivity concerns apply to client applications. |
| **Market Positioning (BIZ-PRD-002)** | N/A | Chat Engine is an internal platform module, not a market-facing product. Competitive analysis and market positioning are not applicable. |
| **Documentation Requirements (MAINT-PRD-001)** | Addressed in NFR-017 | Developer documentation, API spec, and webhook contract documentation are covered under `cpt-cf-chat-engine-nfr-developer-experience`. |
| **Support Requirements (MAINT-PRD-002)** | Deferred | Support tier SLAs are defined at the CyberFabric platform level, not per-module. Chat Engine inherits platform-wide support policies. |
| **Deployment Requirements (OPS-PRD-001)** | Deferred | Deployment environment, release cadence, and rollback policies are defined in the CyberFabric platform-level PRD and infrastructure documentation. Chat Engine inherits these. |
| **Monitoring Requirements (OPS-PRD-002)** | Deferred | Alerting, dashboards, and log retention are governed by the CyberFabric platform observability standards. Chat Engine must emit standard structured logs and metrics — specifics defined in DESIGN. |
| **Industry Standards (COMPL-PRD-002)** | Partial | Applicable standards are referenced inline: GDPR (Art. 17, 25), CCPA, and ACID transaction guarantees. No formal certification (ISO 27001, SOC 2) is currently required. |

## 15. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/)
- **Decomposition**: [DECOMPOSITION.md](./DECOMPOSITION.md)
