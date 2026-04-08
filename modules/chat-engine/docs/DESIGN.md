Created:  2026-03-06 by Constructor Tech
Updated:  2026-03-06 by Constructor Tech
# Technical Design: Chat Engine


<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Architecture Overview](#32-architecture-overview)
  - [3.2.1 Component Model](#321-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.3.1 Internal Dependencies](#331-internal-dependencies)
  - [3.3.2 External Dependencies](#332-external-dependencies)
  - [3.4 Interactions & Sequences](#34-interactions--sequences)
  - [3.4.1 Database schemas & tables](#341-database-schemas--tables)
  - [3.5 Authorization Model](#35-authorization-model)
  - [3.6 Data Protection](#36-data-protection)
  - [3.7 Data Consistency](#37-data-consistency)
  - [3.8 Observability](#38-observability)
  - [3.9 Testing Architecture](#39-testing-architecture)
- [4. Additional Context](#4-additional-context)
- [5. Intentional Exclusions](#5-intentional-exclusions)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

Chat Engine is designed as a service that decouples conversational infrastructure from message processing logic. The system follows a **hub-and-spoke architecture** where Chat Engine acts as the central hub managing session state, message history, and routing, while Backend Plugin modules serve as spokes implementing custom message processing logic.

The architecture emphasizes **separation of concerns**: Chat Engine handles persistence, routing, and message tree management, while backend plugins focus solely on message processing. This enables flexible experimentation with different backend implementations, processing strategies, and conversation patterns without requiring changes to client applications or infrastructure.

**Key architectural decisions:**
- **Message Tree Structure**: Messages form an immutable tree structure enabling conversation branching and variant preservation
- **Streaming-First**: All plugin responses stream through Chat Engine to clients with minimal latency overhead
- **Plugin-Driven Capabilities**: Session capabilities are provided by backend plugins via `on_session_created()`, not hardcoded in Chat Engine
- **Stateless Routing**: Chat Engine instances can scale horizontally as all session state is persisted in the database
- **Plugin System**: Backend plugins are internal code modules implementing `ChatEngineBackendPlugin` trait; each plugin is referenced by `plugin_instance_id` in session type config (`cpt-cf-chat-engine-adr-plugin-backend-integration`)

The system supports both **linear conversations** (traditional chat) and **non-linear conversations** (branching, variants, regeneration), enabling advanced use cases like conversation exploration, A/B testing of different backends, and human-in-the-loop workflows.

### 1.2 Architecture Drivers

#### Functional Drivers

| FDD ID | Solution Description |
|--------|----------------------|
| `cpt-cf-chat-engine-fr-create-session` | RESTful API endpoint creates session record, invokes backend plugin with `session.created` event, stores returned `enabled_capabilities` (typed `Capability[]`) |
| `cpt-cf-chat-engine-fr-send-message` | HTTP streaming endpoint forwards message to backend plugin, pipes streamed response back to client, persists complete exchange after streaming |
| `cpt-cf-chat-engine-fr-attach-files` | Messages support file URL array field; client uploads to external storage first, includes URLs in message payload |
| `cpt-cf-chat-engine-fr-switch-session-type` | Session stores current session_type_id; switching updates this field and routes next message to new backend plugin |
| `cpt-cf-chat-engine-fr-recreate-response` | Creates new message with same parent_message_id as original, sends `message.recreate` event to backend plugin |
| `cpt-cf-chat-engine-fr-branch-message` | Client specifies parent_message_id; Chat Engine loads context up to parent, creates new branch in message tree |
| `cpt-cf-chat-engine-fr-navigate-variants` | Query API returns all messages with same parent_message_id; includes variant position metadata (e.g., "2 of 3") |
| `cpt-cf-chat-engine-fr-stop-streaming` | Client closes HTTP connection; Chat Engine cancels webhook request, saves partial response with incomplete flag |
| `cpt-cf-chat-engine-fr-export-session` | Background job traverses message tree (active path or all variants), formats to JSON/Markdown/TXT, uploads to storage |
| `cpt-cf-chat-engine-fr-share-session` | Generates unique share token stored in database, maps to session_id; recipients create branches from last message |
| `cpt-cf-chat-engine-fr-session-summary` | Routes `session.summary` event to dedicated summarization service URL or backend plugin based on session type config |
| `cpt-cf-chat-engine-fr-search-session` | Full-text search on messages table filtered by session_id; returns matches with context window |
| `cpt-cf-chat-engine-fr-search-sessions` | Full-text search across messages joined with sessions; ranks by relevance, returns session metadata |
| `cpt-cf-chat-engine-fr-delete-session` | Sends `session.deleted` event to backend plugin, then soft-deletes session and messages in database |
| `cpt-cf-chat-engine-fr-conversation-memory` | Message history forwarded to webhook with configurable depth; visibility flags (`is_hidden_from_backend`) enable context management strategies |
| `cpt-cf-chat-engine-fr-delete-message` | Hard delete individual messages with cascade reaction cleanup; ownership validation before deletion |
| `cpt-cf-chat-engine-fr-message-feedback` | UPSERT reaction per user per message; fire-and-forget webhook notification via `message.reaction` event |
| `cpt-cf-chat-engine-fr-context-overflow` | Session metadata exposes processing metrics; visibility flags and summary primitives enable overflow strategy implementation |
| `cpt-cf-chat-engine-fr-message-retention` | Background cleanup job enforces per-session-type retention policies; tree-aware deletion preserves active path integrity |

#### Non-functional Requirements

| FDD ID | Solution Description |
|--------|----------------------|
| `cpt-cf-chat-engine-nfr-response-time` | Async I/O event-driven architecture; database connection pooling; minimal business logic in routing layer |
| `cpt-cf-chat-engine-nfr-availability` | Stateless instances behind load balancer; health check endpoints; database read replicas for failover |
| `cpt-cf-chat-engine-nfr-scalability` | Horizontal scaling; database sharding by tenant_id; connection pool per instance |
| `cpt-cf-chat-engine-nfr-data-persistence` | Database transactions wrap message writes; acknowledge client only after commit confirmation |
| `cpt-cf-chat-engine-nfr-streaming` | HTTP chunked transfer encoding; buffering disabled; direct pipe from webhook to client |
| `cpt-cf-chat-engine-nfr-authentication` | JWT-based authentication; client_id, user_id, tenant_id claim extraction; session ownership validated by user_id; tenant isolation enforced by tenant_id on every request |
| `cpt-cf-chat-engine-nfr-data-integrity` | Database foreign key constraints on parent_message_id; unique constraint on (session_id, parent_message_id, variant_index) |
| `cpt-cf-chat-engine-nfr-backend-isolation` | Circuit breaker pattern per backend plugin; timeout configuration per session type; error isolation |
| `cpt-cf-chat-engine-nfr-file-size` | File size validation delegated to storage service; Chat Engine validates URL format and accessibility |
| `cpt-cf-chat-engine-nfr-search` | Full-text search indexes on message content; pagination with cursor-based queries |

#### Architecture Decision Records

| ADR ID | Decision |
|--------|----------|
| `cpt-cf-chat-engine-adr-message-tree-structure` | Immutable tree with parent_message_id for conversation branching |
| `cpt-cf-chat-engine-adr-capability-model` | Plugin-driven capability model for session type configuration |
| `cpt-cf-chat-engine-adr-streaming-architecture` | HTTP chunked transfer for streaming responses |
| `cpt-cf-chat-engine-adr-routing-layer` | Zero business logic routing layer |
| `cpt-cf-chat-engine-adr-file-handling` | URL-based file references with external storage |
| `cpt-cf-chat-engine-adr-http-client-protocol` | Synchronous HTTP webhook protocol for backend communication |
| `cpt-cf-chat-engine-adr-http-client-protocol` | WebSocket client protocol for real-time delivery |
| `cpt-cf-chat-engine-adr-webhook-event-types` | Typed webhook event categories for backend notifications |
| `cpt-cf-chat-engine-adr-streaming-cancellation` | Client-initiated streaming cancellation with partial save |
| `cpt-cf-chat-engine-adr-stateless-scaling` | Stateless instances for horizontal scaling |
| `cpt-cf-chat-engine-adr-backpressure-handling` | Backpressure handling for streaming pipelines |
| `cpt-cf-chat-engine-adr-message-variants` | Message variants with index and active flag |
| `cpt-cf-chat-engine-adr-variant-indexing` | Variant indexing for navigation |
| `cpt-cf-chat-engine-adr-message-recreation` | Recreation creates variants, branching creates children |
| `cpt-cf-chat-engine-adr-branching-strategy` | Conversation branching from any historical message |
| `cpt-cf-chat-engine-adr-session-switching` | Session type switching with capability reset |
| `cpt-cf-chat-engine-adr-session-sharing` | Token-based session sharing |
| `cpt-cf-chat-engine-adr-session-metadata` | Session metadata for extensible attributes |
| `cpt-cf-chat-engine-adr-capability-filtering` | Capability filtering for session type matching |
| `cpt-cf-chat-engine-adr-search-strategy` | Full-text search strategy for sessions and messages |
| `cpt-cf-chat-engine-adr-message-reactions` | Per-message reactions for user feedback |
| `cpt-cf-chat-engine-adr-session-deletion-strategy` | Soft delete as default with automatic hard delete after retention period |
| `cpt-cf-chat-engine-adr-plugin-backend-integration` | Internal plugin trait for backend integration |
| `cpt-cf-chat-engine-adr-llm-gateway-plugin` | LLM gateway plugin with schema extensions |

#### NFR Allocation

| NFR ID | Design Element | How Addressed |
|--------|---------------|---------------|
| `cpt-cf-chat-engine-nfr-response-time` | Stateless routing, async I/O | Direct plugin invocation without intermediate queuing; streaming starts immediately |
| `cpt-cf-chat-engine-nfr-availability` | Stateless scaling | Horizontal scaling with no shared in-memory state; database is single point of persistence |
| `cpt-cf-chat-engine-nfr-scalability` | Stateless architecture | Any instance can handle any session; load balancer distributes evenly |
| `cpt-cf-chat-engine-nfr-streaming` | HTTP chunked transfer | NDJSON streaming with backpressure; chunks forwarded as received from plugin |
| `cpt-cf-chat-engine-nfr-data-integrity` | ACID transactions | All state mutations wrapped in database transactions; message tree immutability enforced |

### 1.3 Architecture Layers

| Layer | Responsibility | Technology |
|-------|---------------|------------|
| **API Layer** | HTTP request handling, streaming response coordination, authentication, chunked transfer encoding | HTTP server with async I/O |
| **Application Layer** | Use case orchestration, webhook invocation, streaming coordination | Service classes with dependency injection |
| **Domain Layer** | Business logic, message tree operations, validation rules | Domain entities and value objects |
| **Infrastructure Layer** | Database access, HTTP client for webhooks, file storage client | PostgreSQL, HTTP client library, S3 SDK |

## 2. Principles & Constraints

### 2.1 Design Principles

#### Principle: Immutable Message Tree

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-principle-immutable-tree`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-message-tree-structure`

Once a message is created with a parent_message_id, that relationship is immutable. Messages are never moved or re-parented. This ensures referential integrity and enables safe concurrent message creation. Variants are created as siblings (same parent), not by modifying existing messages.
<!-- fdd-id-content -->

#### Principle: Backend Plugin Authority

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-principle-backend-authority`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-capability-model`, `cpt-cf-chat-engine-adr-plugin-backend-integration`, `cpt-cf-chat-engine-adr-llm-gateway-plugin`

Backend plugins are code modules inside Chat Engine implementing the `ChatEngineBackendPlugin` trait. A session type references its plugin via `plugin_instance_id`. Plugin configuration is stored separately in `plugin_configs` (keyed by `plugin_instance_id` + `session_type_id`) and forwarded to the plugin in every call context. On `on_session_created`, the plugin resolves capabilities (e.g., by querying external services) and returns `Vec<Capability>` stored as `Session.enabled_capabilities`. On each message operation, Chat Engine calls the corresponding trait method and receives a `ResponseStream`. Plugins own all outbound communication — for example, the LLM gateway plugin makes HTTP requests to the Model Registry and LLM gateway service. Chat Engine does not interpret capability semantics, transport details, or external service protocols. Plugins may extend `PluginConfig.config` and `Message.metadata` with typed fields by registering GTS derived schemas — see `cpt-cf-chat-engine-adr-llm-gateway-plugin`.
<!-- fdd-id-content -->

#### Principle: Stream Everything

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-principle-streaming`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-streaming-architecture`

All plugin responses are streamed by default to minimize time-to-first-byte. Plugins write chunks to a `ResponseStream` handle; Chat Engine pipes them directly to the client via HTTP chunked transfer with minimal buffering.
<!-- fdd-id-content -->

#### Principle: Zero Business Logic in Routing

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-principle-zero-business-logic`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-routing-layer`

Chat Engine does not process, analyze, or transform message content. All business logic (content moderation, language detection, sentiment analysis) belongs in backend plugins. Chat Engine only routes, persists, and manages message trees.
<!-- fdd-id-content -->

### 2.2 Constraints

#### Constraint: External File Storage

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-constraint-external-storage`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-file-handling`

Chat Engine does not store file content. Clients must upload files to File Storage Service and include file UUIDs (stable identifiers) in messages. File Storage Service provides separate API for accessing files by UUID. This constraint reduces infrastructure complexity and storage costs while enabling centralized access control.
<!-- fdd-id-content -->

#### Constraint: Synchronous Webhook Invocation

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-constraint-sync-webhooks`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-http-client-protocol`

Webhook backends must respond synchronously (with streaming) over HTTP. Asynchronous/callback-based backends are not supported. This constraint simplifies error handling and keeps client connections open for streaming. Note: The client-to-Chat Engine protocol is independent of the webhook protocol, which remains HTTP-based.
<!-- fdd-id-content -->

#### Constraint: Single Database Instance

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-constraint-single-database`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-http-client-protocol`

All Chat Engine instances share a single database cluster. No local caching of session state or messages. This ensures consistency but limits scalability to database write throughput.
<!-- fdd-id-content -->

## 3. Technical Architecture

### 3.1 Domain Model

**Technology**: GTS (JSON Schema)

**Location**: `schemas/`

**Core Schemas**:

#### Session Operations (session/)

- **SessionCreateRequest** - Create session (session_type_id, client_id)
- **SessionCreateResponse** - Session created (session_id, enabled_capabilities)
- **SessionGetRequest** - Get session (session_id)
- **SessionGetResponse** - Session details (session_id, client_id, user_id, tenant_id, session_type_id, enabled_capabilities, metadata, created_at)
- **SessionDeleteRequest** - Delete session (session_id)
- **SessionDeleteResponse** - Deletion confirmed (deleted)
- **SessionSwitchTypeRequest** - Switch type (session_id, new_session_type_id)
- **SessionSwitchTypeResponse** - Type switched (session_id, session_type_id)
- **SessionExportRequest** - Export session (session_id, format, scope)
- **SessionExportResponse** - Export ready (download_url, expires_at)
- **SessionShareRequest** - Generate share link (session_id)
- **SessionShareResponse** - Share link (share_token, share_url)
- **SessionAccessSharedRequest** - Access shared (share_token)
- **SessionAccessSharedResponse** - Shared session (session_id, messages, read_only)
- **SessionSearchRequest** - Search in session (session_id, query, limit, offset)
- **SessionSearchResponse** - Search results (results)
- **SessionsSearchRequest** - Search across sessions (query, limit, offset)
- **SessionsSearchResponse** - Sessions found (results)
- **SessionSummarizeRequest** - Generate summary (session_id, enabled_capabilities)

#### Message Operations (message/)

- **MessageSendRequest** - Send message (session_id, content, file_ids, parent_message_id, enabled_capabilities)
- **MessageListRequest** - List messages (session_id, parent_message_id)
- **MessageListResponse** - Messages list (messages)
- **MessageGetRequest** - Get message (message_id)
- **MessageGetResponse** - Message details (message_id, role, content, file_ids, metadata, variant_info)
- **MessageRecreateRequest** - Recreate response (message_id, enabled_capabilities)
- **MessageGetVariantsRequest** - Get variants (message_id)
- **MessageGetVariantsResponse** - Variants list (variants, current_index)

#### Streaming Events (streaming/)

**Note**: Sent via HTTP chunked response as newline-delimited JSON (NDJSON)

- **StreamingStartEvent** - Begin streaming (message_id)
- **StreamingChunkEvent** - Stream chunk (message_id, chunk)
- **StreamingCompleteEvent** - Streaming finished (message_id, metadata)
- **StreamingErrorEvent** - Stream error (message_id, error_code, message)

#### Webhook Protocol (webhook/)

- **SessionCreatedEvent** - Session created notification (event, session_id, session_type_id, client_id, user_id, tenant_id, timestamp)
- **SessionCreatedResponse** - Capabilities list (enabled_capabilities)
- **MessageNewEvent** - New message for processing (event, session_id, message_id, session_metadata, enabled_capabilities, message, history, timestamp)
- **MessageNewResponse** - Assistant response (message_id, role, content, metadata)
- **MessageRecreateEvent** - Recreate request (event, session_id, message_id, enabled_capabilities, history, timestamp)
- **MessageRecreateResponse** - Recreated response (same as MessageNewResponse)
- **MessageAbortedEvent** - Streaming cancelled (event, session_id, message_id, partial_content, timestamp)
- **SessionDeletedEvent** - Session deleted (event, session_id, timestamp)
- **SessionSummaryEvent** - Summary request (event, session_id, enabled_capabilities, history, summarization_settings, timestamp)
- **SessionSummaryResponse** - Summary text (summary, metadata)
- **SessionTypeHealthCheckEvent** - Health check (event, session_type_id, timestamp)
- **SessionTypeHealthCheckResponse** - Health status (status, version, available_capabilities)

#### Common Types (common/)

##### Session

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-design-entity-session`

Session entity (session_id, client_id, user_id, tenant_id, session_type_id, enabled_capabilities, metadata, created_at, updated_at, share_token)

##### Message

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-design-entity-message`

Message entity (message_id, session_id, parent_message_id, role, content, file_ids, variant_index, is_active, is_complete, metadata, created_at)

##### SessionType

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-design-entity-session-type`

Binding of a plugin reference and session type identity (session_type_id, name, plugin_instance_id, available_capabilities, retention_policy). Plugin-specific configuration is stored separately in `PluginConfig` entity (see `cpt-cf-chat-engine-dbtable-plugin-configs`)

##### Capability

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-design-entity-capability`

Typed capability definition (id, name, type: `bool|enum|str|int`, default_value, enum_values when type=enum)

##### CapabilityValue

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-design-entity-capability-value`

Per-message capability setting (id, value: boolean|string|integer)

##### ContentPart

Abstract content type (type, ...). Subtypes:
- **TextContent** - Plain text content (type: "text", text)
- **CodeContent** - Code block (type: "code", language, code)
- **ImageContent** - Image content (type: "image", image_id: uuid, mime_type)
- **AudioContent** - Audio content (type: "audio", audio_id: uuid, mime_type)
- **VideoContent** - Video content (type: "video", video_id: uuid, mime_type)
- **DocumentContent** - Document content (type: "document", document_id: uuid, mime_type)

##### Supporting Types

- **Usage** - Backend processing metrics (input_units, output_units)
- **VariantInfo** - Variant metadata (variant_index, total_variants, is_active)
- **SearchResult** - Search match (message_id, content, context)
- **SessionSearchResult** - Session match (session_id, metadata, matched_messages)
- **Role** - Enum: user, assistant, system
- **ErrorCode** - Enum: AUTH_REQUIRED, SESSION_NOT_FOUND, MESSAGE_NOT_FOUND, INVALID_REQUEST, BACKEND_TIMEOUT, BACKEND_ERROR, RATE_LIMIT_EXCEEDED, INTERNAL_ERROR
- **ErrorDetails** - Safe error details (trace_id, validation_errors, retry_after_seconds, limit_type, quota_reset_at, timeout_ms, resource_id)
- **ExportFormat** - Enum: json, markdown, txt
- **ExportScope** - Enum: active, all
- **SummarizationSettings** - Summary config (enabled, service_url, config)

##### MessageReaction

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-design-entity-message-reaction`

Reaction record (message_id, user_id, reaction_type, created_at, updated_at)
- **ReactionType** - Enum: like, dislike, none
- **MessageReactionRequest** - HTTP request (reaction_type: ReactionType)
- **MessageReactionResponse** - HTTP response (message_id, reaction_type, applied: boolean)
- **MessageReactionEvent** - Webhook event (event, session_id, message_id, user_id, reaction_type, previous_reaction_type, timestamp)

##### ShareToken

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-design-entity-share-token`

Cryptographic share token (share_token, session_id, created_at, expires_at)

**Relationships**:

HTTP Protocol:
- StreamingStartEvent, StreamingChunkEvent, StreamingCompleteEvent, StreamingErrorEvent → message_id: linked sequence
- SessionCreateRequest → SessionType: references via session_type_id
- MessageSendRequest → Session: references via session_id
- MessageSendRequest → Message: optional parent via parent_message_id
- MessageSendRequest → CapabilityValue: per-message capability settings via enabled_capabilities
- MessageGetResponse → VariantInfo: includes variant metadata
- SessionSearchResponse, SessionsSearchResponse → SearchResult/SessionSearchResult: contains results

Webhook Protocol:
- SessionCreatedEvent → Session: creates
- SessionCreatedResponse → Capability: returns enabled_capabilities list (typed Capability definitions)
- MessageNewEvent, MessageRecreateEvent → Message: references
- MessageNewEvent, MessageRecreateEvent → Session: context
- MessageNewEvent, MessageRecreateEvent, SessionSummaryEvent → CapabilityValue: per-message capability settings via enabled_capabilities
- MessageNewResponse, MessageRecreateResponse → ContentPart: contains array
- MessageNewResponse, MessageRecreateResponse → Usage: includes metadata
- SessionSummaryEvent → SummarizationSettings: includes config

Common Types:
- Session → SessionType: references via session_type_id
- Session → Capability: contains enabled_capabilities (typed Capability definitions confirmed for this session)
- SessionType → Capability: contains available_capabilities (maximum set the plugin can provide)
- Message → Session: belongs to via session_id
- Message → Message: tree structure via parent_message_id
- Message → Role: has role enum
- Message → ContentPart: contains content array
- Message → Usage: optional in metadata
- SessionType → SummarizationSettings: optional config
- ContentPart ← TextContent, CodeContent, ImageContent, AudioContent, VideoContent, DocumentContent: polymorphic
- MessageReaction → Message: references via message_id
- MessageReaction → ReactionType: uses type enum
- MessageReactionEvent → MessageReaction: notifies on change

### 3.2 Architecture Overview

```mermaid
flowchart TB
    subgraph Client Applications
        WebClient[Web Client]
        MobileClient[Mobile Client]
    end

    subgraph Chat Engine
        Core[Core Service]
        PluginRegistry[Plugin Registry]
        LLMPlugin[LLM Gateway Plugin]
        WebhookCompat[Webhook Compat Plugin]
    end

    subgraph Infrastructure
        DB[(PostgreSQL)]
        Storage[File Storage<br/>Service]
    end

    subgraph External Services
        LLMGateway[LLM Gateway<br/>Service]
        LegacyBackend[Legacy HTTP<br/>Backend]
    end

    WebClient -.HTTP.-> Core
    MobileClient -.HTTP.-> Core

    Core --> PluginRegistry
    PluginRegistry --> LLMPlugin
    PluginRegistry --> WebhookCompat

    LLMPlugin -.HTTP.-> LLMGateway
    WebhookCompat -.HTTP.-> LegacyBackend

    Core --> DB
    Core --> Storage

    Core -.HTTP chunks.-> WebClient
    Core -.HTTP chunks.-> MobileClient
```

**System Architecture**:

Chat Engine handles all chat-related operations. It is deployed as a unified monolithic service, not as separate microservices. Each instance includes an HTTP server with chunked streaming support for client connections and provides the following core functionality through internal modules.

**Core Functionality**:

#### Session Management

<!-- fdd-id-content -->
Chat Engine manages session lifecycle operations including create, delete, and retrieve. It invokes the backend plugin with `session.created` event and stores returned capabilities. This functionality handles session type switching and share token generation.
<!-- fdd-id-content -->

#### Message Processing

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-message-tree-structure` (tree management), `cpt-cf-chat-engine-adr-message-variants` (variant assignment), `cpt-cf-chat-engine-adr-message-recreation` (recreation logic)

Chat Engine orchestrates message creation, persistence, and tree management. It validates parent references, assigns variant_index, and enforces tree constraints. Message processing integrates with webhook invocation functionality for backend communication.
<!-- fdd-id-content -->

#### Plugin Integration

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-routing-layer` (zero business logic), `cpt-cf-chat-engine-adr-plugin-backend-integration` (plugin system)

Chat Engine's plugin invocation layer. Resolves `dyn ChatEngineBackendPlugin` by `plugin_instance_id`, constructs call context, and invokes plugin methods (`on_session_type_configured`, `on_session_created`, `on_session_updated`, `on_message`, `on_message_recreate`, `on_session_summary`). On `on_session_created` and `on_session_updated`, the plugin returns `Vec<Capability>` stored as `Session.enabled_capabilities`. Auth, retry, circuit breaker, and timeouts are the plugin's responsibility.

**N:1 session type → plugin relationship**: Multiple differently-configured session types can share the same `plugin_instance_id`. Plugin configuration is stored separately in the `plugin_configs` table (keyed by `plugin_instance_id` + `session_type_id`). The call context always includes `session_type_id` and `plugin_config` (the `config` JSONB from the `plugin_configs` table), allowing a single plugin instance to serve multiple session types with different behaviour (e.g., different configuration, different capability set, different processing strategy).
<!-- fdd-id-content -->

#### Response Streaming

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-streaming-architecture` (streaming architecture), `cpt-cf-chat-engine-adr-streaming-cancellation` (cancellation), `cpt-cf-chat-engine-adr-backpressure-handling` (backpressure)

Chat Engine manages HTTP chunked streaming functionality. It pipes data from backend plugin to client via HTTP streaming responses. This handles stateless request processing, partial response saving on connection close, and backpressure control. Each stream is identified by unique message_id.
<!-- fdd-id-content -->

#### Conversation Export

<!-- fdd-id-content -->
Chat Engine provides conversation export functionality that traverses the message tree, formats content to JSON/Markdown/TXT, and uploads to file storage. Supports active path filtering and full tree export.
<!-- fdd-id-content -->

#### Message Search

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-search-strategy` (search strategy)

Chat Engine provides full-text search capabilities across messages. It implements session-scoped and cross-session search with ranking, pagination, and context window retrieval.
<!-- fdd-id-content -->

#### Message Reactions

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-message-reactions` (message reactions design)

Chat Engine allows users to react to messages with simple like/dislike feedback. Reactions are stored per-user per-message with UPSERT semantics, and backend systems are notified via fire-and-forget webhook events.

**Key Features**:
- One reaction per user per message (can be changed or removed)
- UPSERT semantics: changing reaction overwrites previous
- HTTP API: `POST /messages/{id}/reaction` with `{reaction_type: "like"|"dislike"|"none"}`
- Webhook notification: `message.reaction` event sent to backend after storage
- Fire-and-forget pattern: webhook failures don't affect client response
- Database: Composite primary key (message_id, user_id) ensures uniqueness
- Cascade delete: reactions removed when message is deleted
<!-- fdd-id-content -->

**Key Interactions**:
- Client → Chat Engine: Session and message operations via HTTP REST API
- Chat Engine → Backend Plugin: internal trait call with context (in-process)
- Chat Engine → Client: HTTP chunked streaming with NDJSON messages
- Chat Engine → File Storage: File upload with signed URL generation for exports
- Chat Engine → Database: All persistence operations for sessions, messages, and metadata
- Chat Engine → Summarization Service: Context summarization requests

### 3.2.1 Component Model

Chat Engine is deployed as a unified monolithic service. All functionality is implemented as internal modules within the same deployment unit. See Section 3.2 Architecture Overview for detailed module descriptions.

#### Chat Engine Service

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-component-service`

##### Why this component exists

Chat Engine Service is the top-level orchestrator that owns the session lifecycle and message routing pipeline, decoupling client applications from backend plugin implementations.

##### Responsibility scope

Persistence, routing, and message tree management. Chat Engine does not interpret message content.

##### Responsibility boundaries

Content moderation, AI processing, and summarization logic belong to backend plugins. File content storage belongs to File Storage Service. See `cpt-cf-chat-engine-principle-zero-business-logic`.

##### Related components (by ID)

- `cpt-cf-chat-engine-actor-backend-plugin` — processes messages; called by Webhook Integration module
- `cpt-cf-chat-engine-actor-file-storage` — stores file content; called by Conversation Export module
- `cpt-cf-chat-engine-actor-database` — persists all session and message state

#### Session Management Module

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-component-session-management`

Session lifecycle operations: create, delete, retrieve, type switching, share token generation. Invokes webhook with `session.created` event.

#### Message Processing Module

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-component-message-processing`

Message tree management: creation, persistence, parent validation, variant_index assignment, tree constraints. **ADRs**: `cpt-cf-chat-engine-adr-message-tree-structure`, `cpt-cf-chat-engine-adr-message-variants`, `cpt-cf-chat-engine-adr-message-recreation`.

#### Plugin Integration Module

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-component-webhook-integration`

Plugin registry and trait dispatch: resolves `dyn ChatEngineBackendPlugin` by `plugin_instance_id`, invokes trait methods (`on_session_created`, `on_session_updated`, `on_message`, etc.), delegates all transport/auth/retry to the plugin implementation. The first-party `webhook-compat` plugin wraps legacy HTTP webhook backends. **ADRs**: `cpt-cf-chat-engine-adr-plugin-backend-integration`, `cpt-cf-chat-engine-adr-routing-layer`.

#### Response Streaming Module

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-component-response-streaming`

HTTP chunked streaming: webhook-to-client pipe, backpressure control, connection cancellation, partial response saving. **ADRs**: `cpt-cf-chat-engine-adr-streaming-architecture`, `cpt-cf-chat-engine-adr-streaming-cancellation`, `cpt-cf-chat-engine-adr-backpressure-handling`.

#### Conversation Export Module

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-component-conversation-export`

Message tree traversal, format rendering (JSON/Markdown/TXT), file storage upload. Supports active path and full tree export.

#### Message Search Module

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-component-message-search`

Full-text search across messages: session-scoped and cross-session search, ranking, pagination, context window retrieval. **ADRs**: `cpt-cf-chat-engine-adr-search-strategy`.

#### Message Reactions Module

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-component-message-reactions`

Per-user per-message reactions with UPSERT semantics. Fire-and-forget webhook notification. Cascade delete on message removal. **ADRs**: `cpt-cf-chat-engine-adr-message-reactions`.

### 3.3 API Contracts

See [`api/README.md`](api/README.md) for comprehensive protocol documentation.

#### 3.3.1 HTTP REST API (Client ↔ Chat Engine)

**Specification**: [`api/http-protocol.json`](api/http-protocol.json) (OpenAPI 3.0.3)

**Base URL**: `https://chat-engine/api/v1`

**Authentication**: JWT Bearer token in Authorization header

**15 REST endpoints** across 3 categories:
- **Session Management (10)**: Create, get, delete, switch type, export, share, access shared, search, summarize (streaming)
- **Message Operations (5)**: Send (streaming), recreate (streaming), list, get, variants, reaction

**HTTP Streaming**:
- Content-Type: `application/x-ndjson` (newline-delimited JSON)
- Transfer-Encoding: chunked
- Cancellation: Close HTTP connection
- Events: start, chunk, complete, error

For complete endpoint definitions, request/response schemas, and examples, see the OpenAPI specification file.

#### 3.3.2 Plugin API (Chat Engine ↔ Backend Plugin)

**Interface**: `dyn ChatEngineBackendPlugin` (Rust trait, `chat-engine-sdk` crate)

**Discovery**: Plugin implementations are internal code modules registered in Chat Engine's plugin registry at startup by `plugin_instance_id`.

**Plugin methods**:
- `on_session_type_configured(ctx)` → `Vec<Capability>` — optional static capabilities stored as `SessionType.available_capabilities`; plugins may return empty and defer resolution to session creation
- `on_session_created(ctx)` → `Vec<Capability>` — capabilities resolved at session creation time, stored as `Session.enabled_capabilities`
- `on_session_updated(ctx)` → `Vec<Capability>` — called when user updates session capabilities; plugin re-resolves capabilities (e.g., model change triggers capability refresh from Model Registry), result overwrites `Session.enabled_capabilities`
- `on_message(ctx, stream)` → streams response chunks
- `on_message_recreate(ctx, stream)` → streams regenerated response
- `on_session_summary(ctx, stream)` → streams session summary
- `health_check()` → HealthStatus (optional)

**Streaming**: Plugin writes chunks to `ResponseStream`; Chat Engine pipes to client via HTTP chunked transfer (NDJSON)


### 3.3.1 Internal Dependencies

Chat Engine depends on the following internal modules at runtime.

| Dependency Module | Interface Used | Purpose |
|-------------------|----------------|---------|
| Plugin Registry | Internal registry | Resolve `ChatEngineBackendPlugin` implementations by `plugin_instance_id` at startup and on session type configuration |
| Backend Plugin modules | `dyn ChatEngineBackendPlugin` (chat-engine-sdk) | Internal trait implementations that process messages, provide capabilities, and generate summaries |

### 3.3.2 External Dependencies

| Dependency | Interface | Purpose |
|------------|-----------|---------|
| PostgreSQL | SQL over TLS | Primary persistence for sessions, messages, session types, reactions |
| File Storage Service | HTTP REST | File upload for exports; file access via UUID |

### 3.4 Interactions & Sequences

#### S1: Configure Session Type

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-seq-configure-session-type`
**Use Case**: Admin configures new session type
**Actors**: `cpt-cf-chat-engine-actor-developer`
**PRD Reference**: Backend configuration (implicit in `cpt-cf-chat-engine-fr-create-session`)

```mermaid
sequenceDiagram
    participant Admin
    participant Chat Engine
    participant Backend Plugin

    Admin->>Chat Engine: Submit Session Type Config (plugin_instance_id)
    Chat Engine->>Chat Engine: Resolve plugin by plugin_instance_id

    opt Plugin health check enabled
        Chat Engine->>Backend Plugin: health_check()
        Backend Plugin-->>Chat Engine: HealthStatus
    end

    Chat Engine->>Chat Engine: Store Configuration

    Chat Engine-->>Admin: Session Type Created
```

#### S2: Create Session and Send First Message

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-seq-create-session`
**Use Case**: `cpt-cf-chat-engine-usecase-create-session`
**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`

```mermaid
sequenceDiagram
    participant Client
    participant Chat Engine
    participant Backend Plugin
    participant Model Registry

    Client->>Chat Engine: List Session Types
    Chat Engine-->>Client: Available Session Types

    Client->>Chat Engine: Create Session

    Chat Engine->>Chat Engine: Store Session
    Chat Engine->>Backend Plugin: on_session_created(ctx)
    Backend Plugin->>Model Registry: Get available models
    Model Registry-->>Backend Plugin: Models list
    Backend Plugin->>Model Registry: Get capabilities for default model
    Model Registry-->>Backend Plugin: Model capabilities
    Backend Plugin-->>Chat Engine: Vec<Capability>

    Chat Engine->>Chat Engine: Store Session Capabilities
    Chat Engine-->>Client: Session Created (enabled_capabilities)

    Client->>Chat Engine: Send Message

    Chat Engine->>Backend Plugin: Process Message

    loop Streaming Response
        Backend Plugin-->>Chat Engine: Stream chunk
        Chat Engine-->>Client: Stream chunk
    end

    Backend Plugin-->>Chat Engine: Stream complete
    Chat Engine-->>Client: Stream complete
```

#### S3: Send Message with File Attachments

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-seq-send-message-with-files`
**Use Case**: `cpt-cf-chat-engine-fr-attach-files`
**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-file-storage`

```mermaid
sequenceDiagram
    participant Client
    participant File Storage
    participant Chat Engine
    participant Backend Plugin

    Note over Client,Chat Engine: Session already exists

    Client->>File Storage: Upload File
    File Storage-->>Client: File UUID

    Client->>Chat Engine: Send Message (file_ids: [uuid])
    Note over Chat Engine: Store UUIDs in message
    Chat Engine->>Backend Plugin: Forward Message (file_ids: [uuid])

    Backend Plugin->>File Storage: GET /files/{uuid}
    File Storage-->>Backend Plugin: File Stream

    loop Streaming Response
        Backend Plugin-->>Chat Engine: Stream chunk
        Chat Engine-->>Client: Stream chunk
    end

    Backend Plugin-->>Chat Engine: Stream complete
    Chat Engine-->>Client: Message Complete
```

#### S4: Switch Session Type Mid-Conversation

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-seq-switch-session-type`
**Use Case**: `cpt-cf-chat-engine-fr-switch-session-type`
**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`

```mermaid
sequenceDiagram
    participant Client
    participant Chat Engine
    participant Backend Plugin A
    participant Backend Plugin B

    Note over Client,Backend Plugin A: Previous messages sent to Backend A

    Client->>Chat Engine: Switch Session Type
    Chat Engine-->>Client: Session Updated

    Client->>Chat Engine: Send Message
    Chat Engine->>Backend Plugin B: Process Message

    loop Streaming Response
        Backend Plugin B-->>Chat Engine: Stream chunk
        Chat Engine-->>Client: Stream chunk
    end

    Backend Plugin B-->>Chat Engine: Stream complete
    Chat Engine-->>Client: Stream complete
```

#### S5: Recreate Assistant Response (Variant Creation)

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-seq-recreate-response`
**Use Case**: `cpt-cf-chat-engine-usecase-recreate-response`
**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`

```mermaid
sequenceDiagram
    participant Client
    participant Chat Engine
    participant Backend Plugin

    Note over Client,Chat Engine: Session with messages exists

    Client->>Chat Engine: Recreate Message
    Chat Engine->>Chat Engine: Mark old response as inactive
    Note over Chat Engine: Old response preserved with same parent
    Chat Engine->>Backend Plugin: Request Recreation

    loop Streaming New Response
        Backend Plugin-->>Chat Engine: Stream chunk
        Chat Engine-->>Client: Stream chunk
    end

    Backend Plugin-->>Chat Engine: Stream complete
    Chat Engine-->>Client: Variant Created
```

#### S6: Branch from Historical Message

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-seq-branch-message`
**Use Case**: `cpt-cf-chat-engine-usecase-branch-message`
**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`

```mermaid
sequenceDiagram
    participant Client
    participant Chat Engine
    participant Backend Plugin

    Note over Client,Chat Engine: Session with messages exists

    Client->>Chat Engine: Select Branch Point
    Client->>Chat Engine: Send Message from Branch Point

    Chat Engine->>Chat Engine: Create Message Branch
    Chat Engine->>Chat Engine: Load Context
    Chat Engine->>Backend Plugin: Process Message

    loop Streaming Response
        Backend Plugin-->>Chat Engine: Stream chunk
        Chat Engine-->>Client: Stream chunk
    end

    Backend Plugin-->>Chat Engine: Stream complete
    Chat Engine-->>Client: Branch Created

    Note over Client,Chat Engine: Both message paths preserved
```

#### S7: Navigate Message Variants

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-seq-navigate-variants`
**Use Case**: `cpt-cf-chat-engine-fr-navigate-variants`
**Actors**: `cpt-cf-chat-engine-actor-client`

```mermaid
sequenceDiagram
    participant Client
    participant Chat Engine

    Note over Client,Chat Engine: Session with message variants exists

    Client->>Chat Engine: Get Message Variants
    Chat Engine->>Chat Engine: Query Siblings
    Chat Engine-->>Client: Variants List

    Client->>Chat Engine: Get Specific Variant
    Chat Engine->>Chat Engine: Load Variant
    Chat Engine-->>Client: Variant Content
```

#### S8: Export Session

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-seq-export-session`
**Use Case**: `cpt-cf-chat-engine-usecase-export-session`
**Actors**: `cpt-cf-chat-engine-actor-client`

```mermaid
sequenceDiagram
    participant Client
    participant Chat Engine
    participant File Storage

    Note over Client,Chat Engine: Session with messages exists

    Client->>Chat Engine: Export Session
    Chat Engine->>Chat Engine: Retrieve Messages
    Chat Engine->>Chat Engine: Apply Path Filter
    Chat Engine->>Chat Engine: Format Data
    Chat Engine->>File Storage: Upload Export
    File Storage-->>Chat Engine: Download URL
    Chat Engine-->>Client: Export Ready
```

#### S9: Share Session

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-seq-share-session`
**Use Case**: `cpt-cf-chat-engine-usecase-share-session`
**Actors**: `cpt-cf-chat-engine-actor-end-user`, `cpt-cf-chat-engine-actor-backend-plugin`

```mermaid
sequenceDiagram
    participant User A
    participant Chat Engine
    participant User B
    participant Backend Plugin

    User A->>Chat Engine: Share Session
    Chat Engine-->>User A: Share Link Created

    Note over User A,User B: User A shares link with User B

    User B->>Chat Engine: Access Shared Session
    Chat Engine->>Chat Engine: Validate Link
    Chat Engine-->>User B: Session Data

    User B->>Chat Engine: Send Message
    Chat Engine->>Chat Engine: Create Message Branch
    Chat Engine->>Chat Engine: Load Context
    Chat Engine->>Backend Plugin: Process Message

    loop Streaming Response
        Backend Plugin-->>Chat Engine: Stream chunk
        Chat Engine-->>User B: Stream chunk
    end

    Backend Plugin-->>Chat Engine: Stream complete
    Chat Engine-->>User B: Stream complete

    Note over User B,Chat Engine: New message path created in shared session
```

#### S10: Stop Streaming Response (Connection Close)

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-seq-stop-streaming`
**Use Case**: `cpt-cf-chat-engine-fr-stop-streaming`
**Actors**: `cpt-cf-chat-engine-actor-client`

**Note**: With HTTP streaming, cancellation is achieved by closing the connection, not by sending a separate API call.

```mermaid
sequenceDiagram
    participant Client
    participant Chat Engine
    participant Backend Plugin

    Note over Client,Chat Engine: Session already exists

    Client->>Chat Engine: Send Message
    Chat Engine->>Backend Plugin: Process Message

    loop Streaming Response
        Backend Plugin-->>Chat Engine: Stream chunk
        Chat Engine-->>Client: Stream chunk
    end

    Note over Client: User cancels streaming
    Client->>Client: Close Connection

    Note over Chat Engine: Connection close detected
    Chat Engine->>Chat Engine: Cancel Request
    Chat Engine->>Chat Engine: Save Partial Response
    Chat Engine->>Backend Plugin: Close Connection

    Note over Chat Engine: Message marked incomplete
```

#### S11: Search Session History

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-seq-search-session`
**Use Case**: `cpt-cf-chat-engine-fr-search-session`
**Actors**: `cpt-cf-chat-engine-actor-client`

```mermaid
sequenceDiagram
    participant Client
    participant Chat Engine

    Note over Client,Chat Engine: Session with messages exists

    Client->>Chat Engine: Search Session
    Chat Engine->>Chat Engine: Search Messages
    Chat Engine->>Chat Engine: Rank Results
    Chat Engine->>Chat Engine: Load Context
    Chat Engine-->>Client: Search Results
```

#### S12: Search Across Sessions

- [ ] `p3` - **ID**: `cpt-cf-chat-engine-seq-search-sessions`
**Use Case**: `cpt-cf-chat-engine-fr-search-sessions`
**Actors**: `cpt-cf-chat-engine-actor-client`

```mermaid
sequenceDiagram
    participant Client
    participant Chat Engine

    Client->>Chat Engine: Search Across Sessions
    Chat Engine->>Chat Engine: Search All Sessions
    Chat Engine->>Chat Engine: Rank Sessions
    Chat Engine->>Chat Engine: Prepare Metadata
    Chat Engine-->>Client: Session Results
```

#### S13: Generate Session Summary

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-seq-generate-summary`
**Use Case**: `cpt-cf-chat-engine-fr-session-summary`
**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`

```mermaid
sequenceDiagram
    participant Client
    participant Chat Engine
    participant Summarization Service
    participant Backend Plugin

    Note over Client,Chat Engine: Session with messages exists

    Client->>Chat Engine: Summarize Session
    Chat Engine->>Chat Engine: Validate Summarization Support

    alt Summarization supported
        Chat Engine->>Chat Engine: Retrieve Session History
        Chat Engine->>Chat Engine: Apply Settings
        Chat Engine->>Chat Engine: Determine Target

        alt Dedicated summarization service configured
            Chat Engine->>Summarization Service: Request Summary

            loop Streaming Summary
                Summarization Service-->>Chat Engine: Stream chunk
                Chat Engine-->>Client: Stream chunk
            end

            Summarization Service-->>Chat Engine: Stream complete
            Chat Engine-->>Client: Stream complete
        else Use backend plugin for summarization
            Chat Engine->>Backend Plugin: Request Summary

            loop Streaming Summary
                Backend Plugin-->>Chat Engine: Stream chunk
                Chat Engine-->>Client: Stream chunk
            end

            Backend Plugin-->>Chat Engine: Stream complete
            Chat Engine-->>Client: Stream complete
        end
    else Summarization not supported
        Chat Engine-->>Client: Error Response
    end
```

#### S14: Add Message Reaction (HTTP)

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-seq-add-reaction`
**Use Case**: `cpt-cf-chat-engine-fr-message-feedback`
**Actors**: `cpt-cf-chat-engine-actor-client`, `cpt-cf-chat-engine-actor-backend-plugin`

```mermaid
sequenceDiagram
    participant C as Client
    participant CE as Chat Engine
    participant WH as Backend Plugin

    C->>CE: Submit Reaction
    CE->>CE: Extract User Identity
    CE->>CE: Validate Access

    alt Add or change reaction
        CE->>CE: Store Reaction
        CE->>C: Reaction Applied
    else Remove reaction
        CE->>CE: Remove Reaction
        CE->>C: Reaction Removed
    end

    Note over CE: Client response sent before webhook

    CE->>WH: Notify Reaction Change
    Note over WH: Backend processes reaction event
```

**Flow**:
1. Client submits reaction with reaction_type
2. Chat Engine validates JWT and message access
3. Database stores or removes reaction based on type
4. Client receives immediate confirmation
5. Webhook notification sent asynchronously (fire-and-forget)

#### S15: Remove Message with Reactions (Cascade Delete)

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-seq-delete-message-cascade`
**Use Case**: Message deletion with reaction cleanup
**Actors**: `cpt-cf-chat-engine-actor-client`

```mermaid
sequenceDiagram
    participant C as Client
    participant CE as Chat Engine

    C->>CE: Delete Message
    CE->>CE: Validate Ownership
    CE->>CE: Delete Message

    Note over CE: CASCADE DELETE cleanup

    CE->>CE: Remove Reactions
    CE->>C: Deletion Confirmed
```

**Flow**:
1. Client requests message deletion
2. Database CASCADE DELETE automatically removes all reactions
3. No orphaned reactions remain in database

### 3.4.1 Database schemas & tables

**Schema location**: `migrations/` (versioned migration files)

#### Table: sessions

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dbtable-sessions`

| Column | Type | Description |
|--------|------|-------------|
| session_id | UUID PK | Unique session identifier |
| tenant_id | VARCHAR NOT NULL | Owning tenant identifier (from JWT `tenant_id` claim) |
| user_id | VARCHAR NOT NULL | Owning user identifier (from JWT `user_id` claim) |
| client_id | VARCHAR | Calling application identifier (from JWT `client_id` claim) |
| session_type_id | UUID FK | References session_types |
| enabled_capabilities | JSONB | Capabilities returned by webhook at session creation |
| metadata | JSONB | Client-defined session metadata |
| lifecycle_state | VARCHAR | `active` / `archived` / `soft_deleted` / `hard_deleted` |
| share_token | VARCHAR UNIQUE NULL | Generated share token for session sharing |
| created_at | TIMESTAMPTZ | Creation timestamp |
| updated_at | TIMESTAMPTZ | Last modification timestamp |

#### Table: messages

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dbtable-messages`

| Column | Type | Description |
|--------|------|-------------|
| message_id | UUID PK | Unique message identifier |
| session_id | UUID FK | References sessions |
| parent_message_id | UUID FK NULL | Parent in message tree (NULL for root) |
| role | VARCHAR | `user` / `assistant` / `system` |
| content | JSONB | Array of ContentPart objects |
| file_ids | UUID[] | File UUID references |
| variant_index | INT | Variant position among siblings |
| is_active | BOOL | Whether this is the active variant in the tree |
| is_complete | BOOL | Whether streaming completed (false = partial/aborted) |
| is_hidden_from_user | BOOL | Excluded from client-facing APIs |
| is_hidden_from_backend | BOOL | Excluded from webhook context |
| metadata | JSONB | Backend-supplied message metadata |
| created_at | TIMESTAMPTZ | Creation timestamp |

**Constraints**: UNIQUE (session_id, parent_message_id, variant_index)

#### Table: message_reactions

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dbtable-reactions`

| Column | Type | Description |
|--------|------|-------------|
| message_id | UUID FK | References messages (CASCADE DELETE) |
| user_id | VARCHAR | Reacting user identifier |
| reaction_type | VARCHAR | `like` / `dislike` / `none` |
| created_at | TIMESTAMPTZ | First reaction timestamp |
| updated_at | TIMESTAMPTZ | Last update timestamp |

**PK**: (message_id, user_id)

#### Table: session_types

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dbtable-session-types`

| Column | Type | Description |
|--------|------|-------------|
| session_type_id | UUID PK | Unique session type identifier |
| name | VARCHAR | Human-readable name |
| plugin_instance_id | VARCHAR | GTS plugin instance ID — references an internal ChatEngineBackendPlugin implementation (see `cpt-cf-chat-engine-adr-plugin-backend-integration`) |
| created_at | TIMESTAMPTZ | Creation timestamp |
| updated_at | TIMESTAMPTZ | Last modification timestamp |

##### plugin_configs

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dbtable-plugin-configs`

| Column | Type | Description |
|--------|------|-------------|
| plugin_instance_id | VARCHAR | Plugin instance identifier (composite PK) |
| session_type_id | UUID FK | References session_types (composite PK) |
| config | JSONB | Plugin-specific configuration — opaque to Chat Engine, validated by the plugin against its registered GTS schema |
| created_at | TIMESTAMPTZ | Creation timestamp |
| updated_at | TIMESTAMPTZ | Last modification timestamp |

### 3.5 Authorization Model

**ID**: `cpt-cf-chat-engine-design-auth-model`

#### Authentication

All client requests require a valid JWT Bearer token in the `Authorization` header. Chat Engine validates JWT signature and expiration, and extracts the `client_id` claim to establish request identity.

#### Authorization Rules

| Resource | Operation | Requirement | Validation |
|----------|-----------|-------------|------------|
| Session | Create | JWT valid | `client_id` from JWT becomes session owner |
| Session | Read / Delete | JWT + ownership | `client_id` must match session `client_id` |
| Message | Send | JWT + session ownership | Session must belong to `client_id` |
| Message | Delete | JWT + ownership | Only message author can delete |
| Message | React | JWT + session access | Session must be accessible to `client_id` |
| Shared session | Read | Share token | Valid non-expired share token required |
| Session type | Configure | Admin role | Elevated admin claim in JWT |

#### Inter-Service Authentication

Chat Engine does not add authentication headers to webhook requests. Webhook endpoint security (API keys, mTLS) is the responsibility of the session type administrator. Webhook URLs must use HTTPS in production environments.

### 3.6 Data Protection

**ID**: `cpt-cf-chat-engine-design-data-protection`

#### Personal Data Classification

| Data Type | Classification | Storage Location | Retention |
|-----------|---------------|-----------------|-----------|
| `client_id` | Pseudonymous identifier | Sessions, Messages | Session lifecycle |
| Message content | Potentially personal | Messages table | FR-020 retention policy |
| Session metadata | Potentially personal | Sessions table | Session lifecycle |
| File UUIDs | Reference only (not content) | Messages table | Session lifecycle |
| Reaction `user_id` | Pseudonymous identifier | Reactions table | Message lifecycle |
| Share tokens | Non-personal | Sessions table | Session lifecycle |

#### Data Erasure

- **Soft delete**: Marks session as `soft_deleted`; data preserved for recovery window
- **Hard delete**: Permanently removes session, messages, reactions, and metadata
- **Individual message deletion**: `cpt-cf-chat-engine-fr-delete-message` enables targeted erasure
- **Automated cleanup**: `cpt-cf-chat-engine-fr-message-retention` for age-based or count-based cleanup

#### Data in Transit

All external communication requires TLS: Client ↔ Chat Engine (HTTPS), Chat Engine ↔ Webhook (HTTPS in production), Chat Engine ↔ Database (encrypted connection).

#### Data at Rest

Database-level encryption is an infrastructure concern configured at the database cluster level. Application-level field encryption is excluded (see Section 5: Intentional Exclusions).

### 3.7 Data Consistency

**ID**: `cpt-cf-chat-engine-design-data-consistency`

#### Transaction Boundaries

| Operation | Scope | Isolation |
|-----------|-------|-----------|
| Message creation (send/recreate) | Single message INSERT + variant_index assignment | SERIALIZABLE on (session_id, parent_message_id) |
| Message subtree delete | Recursive CTE + DELETE reactions + DELETE messages | Single transaction, READ COMMITTED |
| Session soft/hard delete | Session UPDATE/DELETE + cascade messages + reactions | Single transaction |
| Reaction UPSERT | Single row INSERT ON CONFLICT UPDATE | Row-level lock on (message_id, user_id) |

#### Variant Index Concurrency

The UNIQUE constraint `(session_id, parent_message_id, variant_index)` requires safe concurrent variant_index assignment when multiple recreate or branch operations target the same parent message simultaneously.

**Strategy**: SELECT MAX(variant_index) + 1 within a serializable sub-transaction scoped to (session_id, parent_message_id). On constraint violation (concurrent race), retry with fresh MAX. Maximum 3 retries before returning 409 Conflict.

#### Idempotency

Message creation is not idempotent — each POST creates a new message node with a new UUID. Client-side deduplication (request ID header) is the responsibility of the client SDK. Reaction UPSERT is idempotent by design (INSERT ON CONFLICT UPDATE).

### 3.8 Observability

**ID**: `cpt-cf-chat-engine-design-observability`

#### Structured Logging

All request handling emits structured log events with the following fields: `trace_id`, `client_id`, `session_id`, `operation`, `duration_ms`, `status`. Message content and personal data are never logged.

#### Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `request_duration_seconds` | Histogram | HTTP latency by endpoint and status code |
| `webhook_duration_seconds` | Histogram | Webhook backend call latency by session_type_id |
| `circuit_breaker_state` | Gauge | Circuit state per session_type_id (closed/open/half-open) |
| `active_streams` | Gauge | Concurrent streaming connections |
| `session_operations_total` | Counter | Session operations by type and result |

#### Health Endpoints

- `GET /health/live` — liveness probe (returns 200 if process is running)
- `GET /health/ready` — readiness probe (includes database connectivity check)

#### Distributed Tracing

`trace_id` is generated per request and propagated in all outbound calls (webhook, database). Included in error responses for support correlation without exposing internal details.

### 3.9 Testing Architecture

**ID**: `cpt-cf-chat-engine-design-testing-arch`

| Layer | Scope | Approach |
|-------|-------|----------|
| Unit | Domain logic, message tree operations, validation rules | Pure function tests, no I/O |
| Integration | Database operations, webhook client | Real test database, mock webhook server |
| API | HTTP endpoints, streaming, auth | Test HTTP server, mock webhook, test database |
| Contract | Webhook API schema conformance | Schema-based tests against `webhook-protocol.json` |

Test isolation: each test case uses independent database state (transaction rollback or dedicated schema). Webhook backends are replaced by configurable mock servers. Coverage targets: 90%+ for domain layer, 100% endpoint coverage including error paths and all authorization boundaries.

## 4. Additional Context

#### Context: Message Tree Traversal

**ID**: `cpt-cf-chat-engine-design-context-tree-traversal`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-message-tree-structure` (tree structure)

Message tree traversal follows parent_message_id references. Active path is computed by following is_active = true flags from root. Full tree export requires recursive CTE queries to traverse all branches. Database indexes on parent_message_id are critical for performance.
<!-- fdd-id-content -->

#### Context: Webhook Circuit Breaker

**ID**: `cpt-cf-chat-engine-design-context-circuit-breaker`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-plugin-backend-integration`

Circuit breaker pattern prevents cascade failures from slow/failing backend plugins. The circuit opens after reaching a configured failure threshold. Half-open state allows a single probe request to test recovery. Success closes circuit; failure reopens. Implemented per session_type_id.
<!-- fdd-id-content -->

#### Context: Streaming Backpressure

**ID**: `cpt-cf-chat-engine-design-context-backpressure`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-backpressure-handling`

Streaming implementation uses bidirectional data streams with backpressure handling. If client is slow, Chat Engine buffers chunks in memory up to a configured limit. If the buffer fills, the webhook request is paused via flow control mechanisms. Client disconnect cancels the webhook request immediately.
<!-- fdd-id-content -->

#### Context: Search Performance

**ID**: `cpt-cf-chat-engine-design-context-search`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-search-strategy`

Full-text search is implemented using database full-text search capabilities with inverted indexes on message content. Search is case-insensitive with language stemming. Results are ranked by relevance with document length normalization. Cross-session search is partitioned by client_id to prevent noisy neighbors. Pagination uses cursor-based queries for consistency.
<!-- fdd-id-content -->

#### Context: File Storage Integration

**ID**: `cpt-cf-chat-engine-design-context-file-storage`

<!-- fdd-id-content -->
**ADRs**: `cpt-cf-chat-engine-adr-file-handling`

Chat Engine never stores file content. Clients upload directly to File Storage Service and receive stable UUID identifiers. Chat Engine stores file UUIDs (not URLs) in messages and forwards them to backend plugins. Webhook backends fetch files from File Storage Service using UUIDs. This approach provides stable identifiers, centralized access control, and enables transparent storage migration. File access is controlled through File Storage Service authentication, and clients request temporary signed URLs when displaying files.
<!-- fdd-id-content -->

#### Context: Session Type Configuration Security

**ID**: `cpt-cf-chat-engine-design-context-security`

<!-- fdd-id-content -->
Session type webhook URLs are stored in plaintext in database. Webhook backends must implement their own authentication (API keys, mutual TLS). Chat Engine does not validate webhook responses beyond HTTP status codes. Malicious backend plugins can return arbitrary content. Session type creation should be restricted to admin users only.
<!-- fdd-id-content -->

#### Context: Error Response Security Pattern

**ID**: `cpt-cf-chat-engine-design-context-error-security`

<!-- fdd-id-content -->
Error responses use the `ErrorDetails` schema to prevent leaking internal implementation details to clients. The schema enforces `additionalProperties: false` and defines explicit fields for each error scenario:

**Error Code to Details Mapping**:
- `INVALID_REQUEST` → validation_errors (field-level validation failures)
- `RATE_LIMIT_EXCEEDED` → retry_after_seconds, limit_type, quota_reset_at
- `BACKEND_TIMEOUT` → timeout_ms
- `SESSION_NOT_FOUND` / `MESSAGE_NOT_FOUND` → resource_id (UUID format only)
- `AUTH_REQUIRED` / `BACKEND_ERROR` / `INTERNAL_ERROR` → trace_id only (for support correlation)

**Security Constraints**:
- No arbitrary data allowed in error details (prevents stack trace leaks)
- trace_id limited to alphanumeric characters (no file paths or SQL fragments)
- resource_id validated as UUID format only
- Sensitive debugging information (stack traces, database errors, internal paths) must only appear in secure internal logs

This pattern follows RFC 9457 (Problem Details) and ensures compliance with security requirements for user-facing errors while maintaining full debugging capability through internal logging.
<!-- fdd-id-content -->

## 5. Intentional Exclusions

Aspects acknowledged and intentionally excluded from this DESIGN.

| Category | Exclusion | Reason |
|----------|-----------|--------|
| **Content Safety** | Content moderation, toxicity filtering | Delegated to backend plugins (Principle: Zero Business Logic in Routing — `cpt-cf-chat-engine-principle-zero-business-logic`) |
| **Accessibility** | UI/UX accessibility requirements | Backend service; client application responsibility |
| **Internationalization** | Multi-language UI, locale handling | Not applicable; message content is opaque to Chat Engine |
| **Rate Limiting** | Throttling algorithms, quota management | Handled at API gateway layer upstream of Chat Engine |
| **Application Caching** | In-process or distributed cache | Excluded per `cpt-cf-chat-engine-constraint-single-database` |
| **Message Encryption** | Application-level field encryption | Infrastructure-level database encryption handles data-at-rest |
| **Async Queue** | Message queue / event bus integration | Excluded per `cpt-cf-chat-engine-constraint-sync-webhooks` |
| **Deployment** | Container orchestration, cloud-specific config | Infrastructure concern; out of DESIGN scope |
| **Client SDKs** | SDK implementation details | Covered by developer experience NFR; not a design deliverable |
| **Compliance Architecture** | GDPR/CCPA compliance framework | Chat Engine acts as data processor; regulatory compliance is the responsibility of data controllers (client applications). Technical mechanisms (hard delete, retention policies) are documented in §3.6 Data Protection |
| **Usability / UX** | User interface design, accessibility | Backend API service; UX is a client application responsibility |
| **Business Alignment** | Business capability mapping, cost analysis | Addressed via PRD traceability in §1.2; detailed business mapping maintained in PRD |
| **Capacity Planning** | Connection limits, storage projections | Deferred to operational planning phase; database scaling bounded by `cpt-cf-chat-engine-constraint-single-database` |
| **Threat Modeling** | STRIDE analysis, attack surface mapping | Conducted separately as part of security review process; not embedded in DESIGN artifact |
