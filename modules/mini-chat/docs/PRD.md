# PRD - Mini Chat

## 1. Overview

### 1.1 Purpose

Mini Chat is a multi-tenant AI chat module that provides users with a conversational interface backed by a large language model. Users can send messages, receive streamed responses in real time, upload documents, and ask questions about uploaded content. The module enforces strict tenant isolation, usage-based cost controls, and emits audit events.

Parent tenant / MSP administrators MUST NOT have access to chat content. Admin visibility is limited to aggregated usage and operational metrics.

#### Mini Chat vs Main Chat

Mini Chat is a lightweight, self-contained chat module designed for rapid delivery. The platform roadmap also includes a full-featured Main Chat module. The two modules differ in scope and extensibility:

| Aspect | Mini Chat | Main Chat (future) |
|--------|-----------|---------------------|
| Agentic flows | None — single LLM call per turn | Custom agentic flows with tool orchestration |
| File storage | External only (provider-hosted: Azure / OpenAI Files API) | Pluggable storage providers via plugins |
| Search / retrieval | External only (provider-hosted vector stores, web search via Azure Foundry) | Pluggable search providers via plugins |
| Model orchestration | Single model per chat, locked at creation | Multi-model orchestration, dynamic routing |

Mini Chat is NOT a stepping stone to Main Chat — it is a separate, simpler product. P2+ items in this PRD are design considerations only; feature parity with Main Chat is explicitly out of scope.

### 1.2 Background / Problem Statement

The platform requires an integrated AI assistant that gives users the ability to have multi-turn conversations with an LLM and ground those conversations in their own documents. Without this capability, users must rely on external tools (ChatGPT, etc.), which creates data governance risks, lacks integration with platform access controls, and provides no visibility into aggregated usage and operational metrics for tenant administrators.

Current gaps: no native chat experience within the platform; no way to query uploaded documents via LLM; no per-user usage tracking or quota enforcement for AI features; no audit events emitted for AI interactions.

### 1.3 Goals (Business Outcomes)

- Provide a stable, production-ready AI chat with real-time streaming and persistent conversation history
- Enable document-aware conversations: users upload files and ask questions grounded in document content
- Guarantee tenant data isolation and enforce access control via `ai_chat` license feature
- Control operational costs through per-user quotas, token budgets, and tool-call limits
- Emit audit events to platform `audit_service` for completed chat turns and policy decisions (one structured event per turn; see `cpt-cf-mini-chat-fr-audit`)

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Chat | A persistent conversation between a user and the AI assistant |
| Message | A single turn within a chat (user input or assistant response) |
| Attachment | A file uploaded to a chat. Each attachment has zero or more *purpose flags* that determine how it is used in the LLM pipeline (e.g., file search indexing, code interpreter input). |
| Attachment Purpose | Boolean flags stored per attachment (`for_file_search`, `for_code_interpreter`) that determine which LLM tool(s) it feeds into. A single attachment may serve multiple purposes (both flags `true`), and all flags may be `false` (e.g., image attachments handled as multimodal input). |
| Code Interpreter | An LLM tool that executes code in a sandboxed environment; used for data analysis of spreadsheets and other structured files. |
| Thread Summary | A compressed representation of older messages, used to keep long conversations within token limits |
| Vector Store | A provider-hosted index of document embeddings (OpenAI or Azure OpenAI), scoped per chat, used for document search |
| Vector Store Scope | In P1, one vector store is created per chat (on first document upload). Each chat with documents gets its own dedicated provider-hosted vector store. Physical and logical isolation are both per chat. |
| File Search | An LLM tool call that retrieves relevant excerpts from uploaded documents |
| Token Budget | The maximum number of input/output tokens allowed per request, computed from the effective_model's context window and deployment configuration |
| Temporary Chat | A chat marked for automatic deletion after 24 hours (P2) |
| OAGW | Outbound API Gateway - platform service that handles external API calls and credential injection |
| Multimodal Input | Responses API input that includes both text and image references (file IDs) in the content array |
| Image Attachment | An image file (PNG, JPEG, WebP) uploaded to a chat via the provider Files API, included in LLM requests as multimodal input; not indexed in vector stores and not eligible for file_search |
| Model Catalog | Deployment-configured list of available LLM models with tier labels, capabilities, and UI metadata (display_name, description). Stored in config file or ConfigMap. |
| Model Tier | One of two cost/capability levels: premium or standard. Determines downgrade cascade order |
| Web Search | An LLM tool call that retrieves information from the public web during a chat turn; explicitly enabled per request via API parameter |
| Selected Model | The model chosen by the user (or resolved via the `is_default` premium model algorithm) at chat creation and stored in `chat.model`. Immutable for the chat lifetime. |
| Effective Model | The model actually used for a specific turn after quota and policy evaluation. Equals the selected model unless a quota-driven downgrade or kill switch overrides it. Recorded per assistant message. |
| Chat Knowledge Base | The set of all document attachments currently present in a chat's vector store. Documents are added on upload and removed on deletion. The assistant may reference any document in the chat knowledge base when generating answers. |

## 2. Actors

### 2.1 Human Actors

#### Chat User

**ID**: `cpt-cf-mini-chat-actor-chat-user`

**Role**: End user who creates chats, sends messages, uploads documents, and receives AI responses. Belongs to a tenant and is subject to that tenant's license and quota policies.
**Needs**: Real-time conversational AI; ability to ask questions about uploaded documents; persistent chat history; clear feedback when quotas are exceeded.

### 2.2 System Actors

#### Cleanup Scheduler

**ID**: `cpt-cf-mini-chat-actor-cleanup-scheduler`

**Role**: Scheduled process that purges soft-deleted chats and associated external resources (files, vector stores) after the retention period. Temporary chat auto-deletion is deferred to P2.

## 3. Operational Concept & Environment

No module-specific environment constraints beyond platform defaults.

## 4. Scope

This PRD uses **P1/P2** to describe phased scope. The `p1`/`p2` tags on requirement checkboxes are internal priority markers and do not define release phase.

### 4.1 In Scope

- Chat CRUD (create, list, get, update title, delete) API; chat detail returns metadata + message_count (no embedded messages)
- Paginated message history via cursor-based pagination with OData v4 query support
- Attachment status polling endpoint
- Real-time streamed AI responses (SSE)
- Persistent conversation history
- Document upload and document-aware question answering via file search
- Chat-scoped document retrieval: all uploaded documents are searchable in all future turns via `file_search` over the chat vector store
- Document summary on upload
- Thread summary compression for long conversations
- Per-user credit-based rate limits across multiple periods (daily, monthly) tracked in real-time; credits are computed from provider-reported tokens using model credit multipliers from the active policy snapshot; premium models have stricter limits, standard-tier models have separate, higher limits; two-tier downgrade cascade (premium → standard); when all tiers are exhausted, the system rejects with `quota_exceeded`
- Model selection per chat at creation time (locked for conversation lifetime)
- Binary like/dislike reactions on assistant messages (persisted, API-accessible)
- File search call limits per message and per user/day
- Web search via provider tooling (Azure Foundry), explicitly enabled per request via API parameter, with per-turn and per-day call limits and a global kill switch
- Token budget enforcement and context truncation
- License feature gate (`ai_chat`)
- Emit audit events to platform `audit_service` (append-only semantics owned by `audit_service`)
- Retry, edit, and delete for the last turn only (tail-only mutation)
- Streaming cancellation when client disconnects
- Image upload and image-aware chat (PNG/JPEG/WebP) via multimodal Responses API, stored via provider Files API
- Images are supported as attachments; they are not searchable via file_search and not indexed in vector stores
- Code interpreter tool support: XLSX spreadsheet uploads are routed to the `code_interpreter` tool for data analysis; the model can execute code in a sandboxed environment to process the file. Kill switch and per-model capability gating apply.
- Multi-purpose attachment routing: each attachment carries boolean purpose flags (`for_file_search`, `for_code_interpreter`) derived from MIME type. A single attachment may serve multiple purposes (both flags `true`).
- Cleanup of external resources (provider files, chat vector stores) on chat deletion

### 4.2 Out of Scope

- Temporary chats with 24h auto-deletion (schema column `is_temporary` reserved; feature deferred to P2)
- Mid-conversation model switching by the user (model is locked at chat creation; only system-driven quota downgrade is allowed mid-chat)
- Projects or shared/collaborative chats
- Full-text search across chat history
- Non-OpenAI-compatible provider support (e.g., Anthropic, Google) - OpenAI and Azure OpenAI are supported at P1 via a shared API surface
- Complex retrieval policies beyond simple limits
- Per-workspace vector store aggregation — P1 uses one vector store per chat. Per-workspace aggregation is deferred.
- Non-image, non-document file support (e.g., audio, video, executables)
- Custom audit storage (audit events are emitted to platform `audit_service`)
- Chat export or migration
- Full conversation history editing (editing or deleting arbitrary historical messages)
- Thread versioning / branching (multi-branch conversations, history forks)
- Multi-branch recovery or resume-from-middle editing
- Web search auto-triggering (P1 requires explicit API parameter; implicit query-based triggering is deferred)
- Automatic filename or document-reference resolution from free-form user text (P1 requires explicit `attachment_ids` resolved by the UI)
- URL content extraction
- Admin configuration UI for AI policies, model selection, or provider settings (P1 uses deployment configuration; see DESIGN.md Section 2.2 constraints and emergency flags)
- Additional quota periods beyond the P1 set (4-hourly rolling windows, weekly periods, 12h rolling windows)
- Per-tenant quota timezone configuration (P1 uses UTC for all calendar-based period boundaries)
- Quota warning thresholds and `quota_warnings` in SSE done events (deferred to P2+)
- Module-specific multi-lingual support (LLM handles languages natively; no module-level i18n)
- Per-feature dynamic feature flags beyond the `ai_chat` license gate and emergency kill switches (DESIGN.md lines 166-168)

### 4.3 Deferred (P2+)

- Group chats and chat sharing (projects) are deferred to P2+ and are out of scope for P1 (see `cpt-cf-mini-chat-fr-group-chats`).

## 5. Functional Requirements

### 5.1 Core Chat

#### Chat CRUD

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-chat-crud`

The system MUST allow authenticated users to create, list, retrieve, update title, and delete chats. Each chat belongs to exactly one user within one tenant. At creation, the user MAY specify a model from the model catalog; if omitted, the default is resolved via the `is_default` premium model algorithm (see `cpt-cf-mini-chat-fr-model-selection`). The selected model is locked for the chat lifetime (see `cpt-cf-mini-chat-constraint-model-locked-per-chat`). Chat content (messages, attachments, summaries, citations) MUST be accessible only to the owning user within their tenant. Listing returns chats for the current user ordered by most recent activity. Retrieval returns chat metadata (including selected model) and `message_count`; messages are NOT embedded in the chat detail response — the UI MUST call `GET /v1/chats/{id}/messages` to load conversation history with cursor pagination. The user MAY rename a chat by updating its `title` via `PATCH /v1/chats/{id}`. Only `title` is updatable in P1; the endpoint MUST NOT modify `model`, `is_temporary`, or any other field. Updating the title sets `updated_at` to the current time; `message_count` is unaffected. The update does not touch messages or attachments. Deletion soft-deletes the chat and triggers cleanup of associated external resources.

**Rationale**: Users need to manage their conversations - create new ones, resume existing ones, and remove ones they no longer need.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Model Selection Per Chat

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-model-selection`

The system MUST allow users to select a model from the model catalog when creating a new chat. If no model is specified, the system MUST resolve the default model using the following deterministic algorithm: (1) the model marked `is_default: true` in the premium tier; (2) if no premium model is marked `is_default`, the first enabled premium model; (3) if no premium models exist, the first enabled standard model; (4) if no enabled models exist, reject with HTTP 400. The selected model MUST be locked for the lifetime of the chat — the user MUST NOT be able to change the model within an existing chat. All user-initiated messages in a chat use the same model.

Quota-driven automatic downgrade within the two-tier cascade IS permitted mid-conversation as a system decision (not user-initiated model switching). The effective model used for each turn is recorded on the assistant message.

**Rationale**: Users benefit from choosing the appropriate model for their use case (premium for complex tasks, standard for everyday tasks), while model locking per chat ensures consistent conversation context.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Streamed Chat Responses

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-chat-streaming`

The system MUST deliver AI responses as a real-time SSE stream. The user sends a message and immediately begins receiving `delta` events as they are generated. The stream terminates with exactly one terminal `done` or `error` event. The terminal `done` event contains the message ID and token usage. The terminal `error` event uses the same error envelope as JSON error responses: `code` and `message`, plus `quota_scope` when `code = "quota_exceeded"`.

**Error model (Option A)**: If request validation, authorization, or quota preflight fails before any streaming begins, the system MUST return a normal JSON error response with the appropriate HTTP status and MUST NOT open an SSE stream. If a failure occurs after streaming has started, the system MUST terminate the stream with a terminal `event: error`.

The request body MAY include a client-generated `request_id` used as an idempotency key (if omitted, the server MUST generate a UUID v4); MAY include `attachment_ids` for attachments (documents or images) explicitly associated with the current message; and MAY include `web_search` to explicitly enable web search for the turn (see `cpt-cf-mini-chat-fr-web-search`). In every Message response DTO, `request_id` is always present and non-null (a required UUID). Within a normal turn, the user message and assistant response share the same `request_id` (the turn correlation key). System/background messages (e.g. `doc_summary`) carry an independently server-generated UUID v4. P1 enforces **at most one running turn per chat**: if any turn in the chat is currently `running`, the system MUST reject the new request with `409 Conflict`, regardless of the `request_id` value. Additionally, if a `chat_turns` record with `state=running` exists for the same `(chat_id, request_id)`, the system MUST reject with `409 Conflict`. If a completed generation exists for the same `(chat_id, request_id)`, the system MUST replay the completed assistant response rather than starting a new provider request. Replay MUST be side-effect-free: no new quota reserve, no quota settlement, no billing/outbox event emission.

Clients must not auto-retry with the same `request_id` after disconnect; recovery is via the Turn Status API (`GET /v1/chats/{chat_id}/turns/{request_id}`). Retry and edit operations both create a new turn and therefore require a new `request_id`. A completed `(chat_id, request_id)` pair is replay-only — reusing it will return the previously generated result instead of starting a new generation.

**Rationale**: Streaming provides perceived low latency and matches user expectations from consumer AI chat products.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Conversation History

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-conversation-history`

The system MUST persist all user and assistant messages. Conversation history access MUST be limited to the owning user within their tenant. On each new user message, the system MUST include relevant conversation history in the LLM context to maintain conversational coherence.

The system MUST expose conversation history via `GET /v1/chats/{id}/messages` with cursor-based pagination (Page + PageInfo pattern) and OData v4 query support (`$orderby`, `$filter`, `$select`). Each message MUST include: a required `request_id` (UUID, always present and non-null — within a normal turn, user and assistant messages share the same value; system/background messages use an independently server-generated UUID v4) and a required `attachments` field (always-present array of associated attachment summaries, empty array when none). The `attachments` array MUST be derived only from `message_attachments` (populated from `attachment_ids` at send time). Attachment details are not embedded; the UI fetches them individually via `GET /v1/chats/{id}/attachments/{attachment_id}` if needed.

**Rationale**: Multi-turn conversations require the AI to remember prior context within the same chat. Cursor pagination ensures efficient history loading for long conversations.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Streaming Cancellation

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-streaming-cancellation`

The system MUST detect client disconnection during a streaming response and cancel the in-flight LLM request. Cancellation MUST propagate through the entire request chain to terminate the external API call. The server MUST NOT emit an SSE `event: error` for a client disconnect — the SSE stream is already broken. The turn transitions to `cancelled` internally, and the Turn Status API is the authoritative source of final state after disconnect.

When a stream is cancelled or disconnects before a terminal completion, the system MUST apply a bounded best-effort debit for quota enforcement so cancellation cannot be used to evade usage limits. If the provider already emitted a terminal `done` or `error` before the disconnect, that terminal outcome stands and the disconnect does not alter the billing state.

**Rationale**: Prevents wasted compute and cost when the user navigates away or closes the browser.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

### 5.2 Document Support

#### File Upload

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-file-upload`

The system MUST allow users to upload document files to a chat. Uploaded documents are extracted, chunked, and indexed into the chat's dedicated vector store with `attachment_id` metadata. Exception: files routed exclusively to `code_interpreter` (currently XLSX) are NOT extracted, chunked, or indexed. The system does NOT include full extracted file text in prompts; only relevant retrieved excerpts (top-k chunks) are included during file search. Attachment access MUST be limited to the owning user within their tenant. The system MUST return an attachment identifier and processing status (`pending`).

The UI polls `GET /v1/chats/{id}/attachments/{attachment_id}` until the attachment status transitions to `ready` or `failed`. `doc_summary` is server-generated asynchronously by a background task and is never provided by the client; it is null until processing completes. If status is `failed`, the response includes an `error_code` field (stable internal code, no provider identifiers).

**Rationale**: Users need to ground AI conversations in their own documents (contracts, policies, reports).
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Image Upload

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-image-upload`

The system MUST allow users to upload image files (PNG, JPEG/JPG, WebP) to a chat as image attachments. Image attachments are stored via the provider Files API and referenced in Responses API calls as multimodal input. Image attachments are NOT indexed in vector stores and do NOT participate in file_search tool calls. The system MUST return an attachment identifier and processing status (`pending`). The UI polls `GET /v1/chats/{id}/attachments/{attachment_id}` until the status transitions to `ready` or `failed`. For image attachments, the server MAY return `img_thumbnail` (a server-generated preview thumbnail sized to configured WxH); null otherwise. `img_thumbnail` is server-generated only (never provided by the client); maximum decoded size (raw bytes) is 128 KiB by default (configurable via `thumbnail_max_bytes`); stored internally in Mini Chat database only (never uploaded to provider); contains no provider identifiers. `doc_summary` remains always null for images.

**Image upload rules**:

- Supported image types: `image/png`, `image/jpeg`, `image/webp`.
- Maximum file size per image: configurable per deployment (default: 16 MiB; PM target: 25 MiB). Uses the same `uploaded_file_max_size_kb` config as documents unless overridden by `uploaded_image_max_size_kb`.
- Maximum image inputs per message: configurable (default: 4).
- Maximum image inputs per user per day: configurable (default: 50).
- Images are uploaded to the provider via Files API. Upload fields (including `purpose`) are controlled by a static per-provider mapping shipped with deployment configuration and applied by OAGW (documents: `assistants`; images: OpenAI `vision` when required by the configured endpoint/model, Azure OpenAI `assistants`).
- Images are included in the Responses API request input as multimodal content items (file ID references), allowing the assistant to reason about image content for that chat turn.
- Images are NOT summarized on upload (no background summary task for images at P1).
- Attachment access remains owner-only and tenant-isolated (same access rules as document attachments).
- If the effective model (after any quota-driven downgrade) does not support image input, the system MUST reject with `unsupported_media` error (HTTP 415) before any provider call. This applies even when the user's selected model is image-capable but the effective model after downgrade is not. The system MUST NOT silently drop images or auto-upgrade to an image-capable model. **P1 catalog invariant**: all enabled models in the P1 catalog include `VISION_INPUT` capability (see DESIGN.md Model Catalog Configuration), so this rejection path is defensive and not expected to trigger in P1 deployments. It activates automatically if a future catalog introduces a model without `VISION_INPUT`.

**Rationale**: Users need to share visual content (screenshots, diagrams, photos) with the AI assistant and ask questions about what they see.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

All `attachment_ids` submitted with a message are strictly scoped to `(tenant_id, user_id, chat_id)` and validated before LLM invocation. Each array MUST contain unique attachment IDs; duplicate IDs within `attachment_ids` MUST be rejected with HTTP 400 before quota reserve and before any provider call. No attachment validation may rely on provider-side failure; all checks MUST complete before any quota reserve or provider request is issued.

#### Document Question Answering (File Search)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-file-search`

The system MUST support answering questions about uploaded documents by retrieving relevant excerpts during chat. In P1, retrieval always covers all documents currently present in the chat vector store — `attachment_ids` does not scope or filter retrieval. The system MUST NOT inject full file contents into the prompt; only top-k retrieved chunks are included. File search MUST be scoped to the user's tenant. Retrieved excerpts and citations MUST be returned only to the owning user within their tenant. The system MUST enforce a configurable per-turn file search call limit (default: 2 retrieval calls per turn).

The backend MUST NOT include the `file_search` tool before the first document attachment reaches `ready` status in the chat (no vector store exists). Once document attachments exist, the backend includes `file_search` on every model request with the chat vector store ID attached via `tool_resources`, without metadata filtering (P1). The backend MUST resolve the provider vector store internally from `(tenant_id, chat_id)` and MUST NOT require or accept provider vector store identifiers from clients. Attachment-scoped retrieval (narrowing to documents referenced in `attachment_ids`) is deferred to P2.

When users upload files to a chat, those files become part of the chat's knowledge base. The assistant may reference any uploaded file during future responses. Deleting a file removes it from the assistant's knowledge.

In P1, the backend MUST NOT attempt filename or document-reference resolution from free-form user text. Fuzzy filename matching, multilingual entity resolution, and hidden helper LLM calls to infer intended files from message text are explicitly out of scope for P1.

**Rationale**: The primary value of document upload is the ability to ask questions and get answers grounded in document content.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Code Interpreter (Spreadsheet Support)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-code-interpreter`

The system MUST support the `code_interpreter` LLM tool for data analysis of uploaded spreadsheet files. XLSX files (`application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`) are uploaded with `for_code_interpreter = true` and are passed to the LLM as code interpreter file inputs rather than being indexed in the vector store.

**Purpose routing**: Each attachment carries two boolean purpose flags (`for_file_search`, `for_code_interpreter`) derived from its MIME type at upload time. A single attachment may serve multiple purposes (both flags `true`). Current assignments:

- XLSX → `for_code_interpreter = true` (file is available to the code interpreter tool; NOT extracted, chunked, or indexed in the vector store)
- Other document types → `for_file_search = true` (file is indexed in the vector store for retrieval)
- Images → both flags `false` (handled as multimodal input, not routed to any tool)

**Kill switch**: The `disable_code_interpreter` kill switch MUST prevent code interpreter usage at runtime. When active, uploads where `for_code_interpreter` would be the only purpose (currently: XLSX) MUST be rejected with a validation error. If the attachment also has `for_file_search = true`, `for_code_interpreter` is set to `false` and the upload proceeds.

**Model capability gating**: If the effective model does not support code interpreter (`tool_support.code_interpreter = false` in the model catalog), the same filtering logic applies: `for_code_interpreter` is set to `false`, and if no purposes remain, the upload is rejected.

**Tool assembly**: When a chat contains ready `code_interpreter` attachments, the `disable_code_interpreter` kill switch is `false`, and the effective model supports code interpreter (`tool_support.code_interpreter = true`), the backend includes the `code_interpreter` tool in the Responses API request with the corresponding provider file IDs (via `tools[].container.file_ids`). The provider decides whether to invoke the tool.

**Rationale**: Users need to analyze spreadsheet data (pivot tables, charts, statistical analysis) through conversational interaction with the AI assistant.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Web Search

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-web-search`

The system MUST support web search as an LLM tool, explicitly enabled per request via an API parameter (`web_search.enabled`). When enabled, the backend includes the `web_search` tool in the provider request (Azure Foundry API tooling). The provider decides whether to invoke the tool based on the query; explicit enablement means "tool is available and allowed", not "force a call every time". Web search MUST be disabled by default (safe default for backward compatibility).

**Rate limits**: The system MUST enforce configurable per-turn web search call limits (default: 2 calls per turn) and per-user daily web search quota (default: 75 calls per day), tracked in `quota_usage.web_search_calls`. When the daily web search quota is exhausted, the system MUST reject with `quota_exceeded` and `quota_scope = "web_search"` at preflight (before any provider call). This is part of cost control / quotas and MUST NOT be reported as `quota_scope = "tokens"`.

**Kill switch**: A global `disable_web_search` flag MUST allow operators to disable web search at runtime. When the kill switch is active and a request includes `web_search.enabled=true`, the system MUST reject with HTTP 400 and error code `web_search_disabled` before opening an SSE stream. The system MUST NOT silently ignore the parameter.

**System prompt guard**: When web search is enabled for a turn, the system prompt MUST instruct the model: *"Use web_search only if the answer cannot be obtained from the provided context or your training data. Never use it for general knowledge questions. At most one web_search call per request."* **Two enforcement layers**: (1) system prompt soft guidance — at most 1 call; (2) `quota_service` hard limit — configurable, default 2 calls per message. The soft constraint reduces unnecessary calls; the hard limit is the backstop. Tests MUST NOT assume exactly 1 call per turn — up to 2 calls are valid under the hard limit.

**Citations**: When web search results contribute to the assistant response, the system MUST include citations with `source: "web"`, `url`, `title`, and `snippet` in the existing SSE `citations` event.

**Rationale**: Users need to augment AI responses with up-to-date web information for questions beyond the scope of uploaded documents.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Document Summary on Upload

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-doc-summary`

The system MUST generate a brief summary of each uploaded document. Summary generation is triggered upon upload and runs asynchronously as a background task (`requester_type=system`). The summary (`doc_summary`) is stored and used in the conversation context to give the AI general awareness of attached documents without requiring a search call. `doc_summary` is server-generated and MUST NOT be provided by the client. The `doc_summary` field on the Attachment object is null until background processing completes; its current value is available via `GET /v1/chats/{id}/attachments/{attachment_id}`.

Document summary generation MUST run as a background/system task (`requester_type=system`) and MUST NOT be charged to an arbitrary end user.

Background/system tasks MUST NOT create `chat_turns` records. `chat_turns` idempotency and replay semantics apply only to user-initiated streaming turns.

**Rationale**: Improves AI response quality when the user asks general questions about attached documents.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Per-Chat Document Limits

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-per-chat-doc-limits`

The system MUST enforce per-chat limits on document uploads to prevent RAG quality degradation and uncontrolled cost growth:

- Maximum number of document attachments per chat: configurable (default: 50).
- Maximum total uploaded file size per chat: configurable (default: 100 MB).
- Maximum indexed chunks per chat: configurable (default: 10,000). The system MUST prevent indexing beyond this limit.

The system MUST reject upload requests that would exceed any per-chat limit with an appropriate error. These limits apply to document attachments only; image attachments have separate limits (see `cpt-cf-mini-chat-fr-quota-enforcement`).

**Rationale**: Prevents RAG retrieval degradation from overly large document sets and bounds vector store size per chat.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Attachment Deletion

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-attachment-deletion`

The system MUST allow users to delete individual attachments from a chat via `DELETE /v1/chats/{id}/attachments/{attachment_id}`. Deleting an attachment MUST:

1. Soft-delete the attachment record locally and immediately exclude it from future retrieval and active chat metadata.
2. Return `204 No Content` after the local transaction commits.
3. Perform provider-side cleanup asynchronously — file deletion via the provider Files API and document removal from the chat vector store are executed via transactional outbox workers and MUST NOT block the API response.
4. Re-deleting an already soft-deleted attachment is idempotent and returns `204 No Content`.

Historical messages that reference deleted attachments MUST NOT be modified. Messages may still reference deleted attachments in their `attachments` array, but the file will no longer be available for retrieval or download.

**Attachment Removal Rules**: Users may remove attachments while composing a message. After a message is sent, its attachment references become immutable. An attachment cannot be deleted if it is referenced by any submitted message. An attachment that is not referenced by any submitted message may still be deleted.

**Rationale**: Users need the ability to remove documents from a chat's knowledge base without deleting the entire chat.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

### 5.3 Conversation Management

#### Thread Summary Compression

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-thread-summary`

The system MUST compress older conversation history into a summary when the conversation exceeds defined thresholds (message count, token count, or turn count). Thread summary access MUST be limited to the owning user within their tenant. The summary MUST preserve key facts, decisions, names, and document references. Summarized messages are retained in storage but replaced by the summary in the LLM context.

**P1 scope — simple summarization**: The background worker calls the LLM with a summarization prompt and stores the result. If the provider call fails, the previous summary is kept and the message batch is not marked as compressed. No quality gate (length or entropy validation) is applied in P1.

**P2+ scope — quality gate**: Length and entropy validation with automatic regeneration on obviously-bad summaries is deferred to P2+. See DESIGN.md `cpt-cf-mini-chat-seq-thread-summary` for the full P2+ specification.

**Rationale**: Long conversations would exceed LLM context limits and increase costs without compression. The simple P1 variant prevents context window exhaustion while keeping implementation risk low.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Temporary Chats (P2)

- [ ] `p2` - **ID**: `cpt-cf-mini-chat-fr-temporary-chat`

The system MUST allow users to mark a chat as temporary. Temporary chats MUST be automatically deleted (including all associated external resources) after 24 hours.

**Rationale**: Users need disposable conversations for quick questions without cluttering their chat list.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`, `cpt-cf-mini-chat-actor-cleanup-scheduler`

#### Message Actions (P1 Scope)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-turn-mutations`

P1 supports retry, edit, and delete for the **last turn only**. Full message history editing is deferred to P2.

**Supported actions (P1)**:

- **Retry last turn**: Re-submit the last user message to generate a new assistant response. Original attachment associations from `attachment_ids` (images and documents) are preserved — copied to the new user message via `message_attachments` (deleted attachments are silently excluded). Retrieval operates over the entire chat vector store (P1). The previous turn is soft-deleted and a new turn is created with a fresh assistant response.
- **Edit last user turn**: Replace the content of the last user message and regenerate the assistant response. Original attachment associations from `attachment_ids` (images and documents) are preserved — copied to the new user message via `message_attachments` (deleted attachments are silently excluded). Retrieval operates over the entire chat vector store (P1). The previous turn is soft-deleted and a new turn is created with the updated content.
- **Delete last turn**: Remove the most recent turn (user message + assistant response) from the active conversation. The turn is soft-deleted.

**Functional constraints**:

- Only the most recent turn may be retried, edited, or deleted.
- The server MUST determine the most recent turn deterministically as the non-deleted turn with the greatest `(started_at, id)`.
- The target turn MUST be in a terminal state (`completed`, `failed`, or `cancelled`) before retry, edit, or delete is allowed. A running turn must complete or be cancelled (via client disconnect) first.
- The target turn MUST belong to the requesting user.
- Conversations remain strictly linear. These operations do not create branches.

**Explicitly out of scope (P1)**:

- Editing or deleting arbitrary historical messages
- Thread branching or history forks
- Multi-version conversations
- Purging subsequent messages after editing middle history

**Rationale**: Users commonly need to correct a typo, rephrase a question, or retry after a poor response. Restricting mutations to the last turn keeps the conversation model simple and linear while covering the most frequent use cases.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Message Reactions (Like/Dislike)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-message-reactions`

The system MUST allow users to add a binary like or dislike reaction to assistant messages within their own chats. Each user may have at most one reaction per assistant message. Users MUST be able to change their reaction (from like to dislike or vice versa) and remove their reaction entirely.

Reactions are persisted in backend storage (`message_reactions` table) and accessible via API. Reactions on user messages or system messages MUST NOT be allowed.

**Rationale**: Binary feedback on assistant responses enables quality tracking and provides signal for future model/prompt improvements.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

### 5.4 Cost Control & Governance

#### Per-User Usage Quotas

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-quota-enforcement`

The system MUST enforce per-user credit-based rate limits across multiple time periods (daily, monthly). Credits are computed from provider-reported token usage using the model credit multipliers from the active policy snapshot. Rate limits apply per user and track model usage in real-time per tier. Premium models have stricter limits; standard-tier models have separate, higher limits. Tracked metrics: input tokens, output tokens, credits, file search calls, web search calls, per-tier model calls (premium, standard), image inputs, image upload bytes.

**Tier availability rule**: a tier is considered available only if it has remaining quota in **all** configured periods (daily, monthly) for that tier. If any single period is exhausted, the entire tier is treated as exhausted and the system MUST auto-downgrade to the next tier in the cascade (premium → standard). When all tier quotas are exhausted across all periods, the system MUST reject with `quota_exceeded` (HTTP 429).

Quota counting MUST use two phases: Preflight (reserve) before the provider call, and commit actual usage after completion.

The provider-reported token usage (`usage.input_tokens`, `usage.output_tokens`) is the source of truth; the system converts it to credits deterministically using the applied policy version.

**Period reset rules**: Daily and monthly periods are calendar-based in UTC, resetting at midnight UTC (daily) and 1st-of-month midnight UTC (monthly). Additional periods (4-hourly, weekly) and per-tenant timezone configuration are deferred to P2+.

**Warning thresholds (P2+)**: Quota warning thresholds (`quota_warnings` in the SSE `done` event) are deferred to P2+. P1 does not emit warning notifications.

Operational configuration of rate limits, quota allocations, and model catalog is managed by Product Operations. See **#CON-001** for configuration management details.

If quota preflight rejects a send-message request, the system MUST return a normal JSON error response with the appropriate HTTP status (typically `quota_exceeded` 429) and MUST NOT open an SSE stream.

**Image-specific quota limits** (configurable per deployment):

- Maximum image inputs per message: default 4.
- Maximum image inputs per user per day: default 50. **Whole-request rejection policy**: if the number of images in the request would cause the daily quota to be exceeded (e.g., remaining daily quota is 2 but request contains 4 images), the entire request MUST be rejected with `quota_exceeded` (`quota_scope = "image_inputs"`) before any provider call. No partial acceptance of images within a single request.
- Optional: maximum total image bytes per message (default: uncapped; operator may configure).
- Token accounting: `usage.input_tokens` / `usage.output_tokens` from the provider already includes image token costs as the provider defines them. The system enforces these via the same preflight/commit mechanism. Additionally, the system MUST track and enforce explicit image counters (`image_inputs` per day, `image_upload_bytes` per day/month, counted on upload) independent of token quotas to prevent abuse via large or frequent image uploads.

**Rationale**: Prevents runaway costs from individual users and ensures fair resource distribution across a tenant.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Token Budget Enforcement

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-token-budget`

The system MUST enforce a maximum input token budget per request. When the assembled context exceeds the budget, the system MUST truncate lower-priority content (old messages, document summaries, retrieval excerpts) while preserving the system prompt and thread summary. A reserve for output tokens MUST always be maintained.

**Rationale**: Prevents requests from exceeding provider context limits and controls per-request cost.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### License Gate

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-license-gate`

The system MUST verify that the user's tenant has the `ai_chat` feature enabled via the platform's `license_manager`. Requests from tenants without this feature MUST be rejected with HTTP 403.

**Rationale**: AI chat is a premium feature gated by the tenant's license agreement. License verification is delegated to the platform `license_manager`.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Audit Events

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-audit`

The system MUST emit structured audit events to the platform's `audit_service` for completed chat turns and policy decisions (one structured event per completed turn). Each event MUST include: tenant, user, chat reference, event type, model used, token counts, latency metrics, and policy decisions (quota checks, license gate results). Mini Chat does not store audit data locally.

Before emitting events, the mini-chat module MUST redact obvious secret patterns from any included content. Redaction is best-effort and pattern-based. It is designed to catch common secret formats but does not guarantee detection of all sensitive data (e.g., obfuscated tokens, custom credential formats). Audit payloads containing customer content MUST be treated as sensitive data by `audit_service`. P1 redaction rules MUST include at least:

- Replace any `Authorization: Bearer <...>` header value with `Authorization: Bearer [REDACTED]`
- Replace any `api_key`, `x-api-key`, `client_secret`, `access_token`, `refresh_token` values with `[REDACTED]` when they appear in `key=value` or JSON string field form
- Replace any `api-key: <...>` or `Ocp-Apim-Subscription-Key: <...>` header value with `[REDACTED_AZURE_KEY]`
- Replace OpenAI-style API keys with prefix `sk-` with `[REDACTED_OPENAI_KEY]`
- Replace AWS access key IDs (for example values matching `AKIA...`) with `[REDACTED_AWS_ACCESS_KEY_ID]`
- Replace JWT-like tokens (`header.payload.signature`) with `[REDACTED_JWT]`
- Replace any `password` values with `[REDACTED]` when they appear in `key=value` or JSON string field form
- Replace PEM private key blocks (lines between `-----BEGIN` and `-----END` containing `PRIVATE KEY`) with `[REDACTED_PRIVATE_KEY]`

Audit events MUST NOT include raw attachment file bytes. Audit events MAY include attachment metadata and document summaries. Any included string content MUST be truncated after redaction to a configurable maximum per field (default: 8 KiB, append `…[TRUNCATED]`). The total audit event payload MUST NOT exceed the `audit_service` event size limit.

Audit payload retention and deletion semantics are owned by platform `audit_service`.

- `audit_service` is the system of record for audit TTL and deletion semantics.
- For P1, `audit_service` MUST retain Mini Chat audit payloads for at least 90 days by default (configurable).
- Mini Chat MUST NOT attempt to delete or mutate audit records after emission.

**Rationale**: Compliance and security incident response require a record of AI usage with policy decisions. Audit storage and append-only semantics are the platform `audit_service` responsibility. Cost analytics and billing attribution are driven by internal usage records and Prometheus metrics (see `cpt-cf-mini-chat-fr-cost-metrics`), not by audit events.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

#### Cost Metrics

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-cost-metrics`

The system MUST log the following metrics for every LLM request: model, input tokens, output tokens, file search call count, time to first token, total latency. Tenant and user attribution MUST be available via audit events and internal usage records; Prometheus labels MUST NOT include `tenant_id` or `user_id`.

**Rationale**: Enables cost monitoring, budget alerts, and billing attribution per tenant/user.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

### 5.5 Data Lifecycle

#### Chat Deletion with Resource Cleanup

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-chat-deletion-cleanup`

When a chat is deleted, the system MUST mark attachments for asynchronous cleanup and return without blocking on external provider operations. A cleanup worker MUST perform idempotent retries to delete the chat's vector store and provider files. Local data MUST be soft-deleted or anonymized per the retention policy and hard-purged by a periodic cleanup job after a configurable grace period.

**Rationale**: Prevents orphaned external resources and ensures data governance compliance on deletion.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`, `cpt-cf-mini-chat-actor-cleanup-scheduler`

### 5.6 Quota and Billing Architecture

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-quota-billing-architecture`

**P1 scope**:

- Mini Chat enforces credit-based quotas (daily, monthly) and performs downgrade: premium → standard → reject (`quota_exceeded`).
- Integration is asynchronous: Mini Chat enqueues a usage event in a transactional outbox after each turn reaches a terminal state. A background dispatcher publishes it via the selected `mini-chat-model-policy-plugin` plugin (`publish_usage(payload)`). CyberChatManager consumes these events and updates credit balances.
- Usage events MUST be idempotent (keyed by `turn_id` / `request_id`).
- No synchronous billing RPC is required during message execution.
- All LLM invocations that take a quota reserve produce exactly one terminal billing event (completed, failed, or aborted), ensuring no credit drift under disconnect or crash scenarios. Pre-reserve failures (validation, authorization, quota preflight rejection) are not part of reserve settlement and do not require a billing event.
- Exactly one terminal settlement per reserved invocation, enforced via DB-atomic conditional finalization (CAS guard on turn state). No in-memory locks; all finalization paths — including the orphan watchdog — use the same database-level mutual exclusion.
- Failed LLM invocations that reached the provider may incur token charges (input and/or output) and are billed accordingly based on actual consumption or a bounded estimate when actual usage is unavailable.

**Background/system task billing rules (P1)**:

- Background tasks (thread summary update, document summary generation) are `requester_type=system`.
- They MUST NOT create `chat_turns` rows.
- They MUST NOT reserve user quota buckets (`tenant_id`, `user_id`). Per-user quota enforcement does not apply to system tasks.
- They MUST emit usage events attributed to a system bucket (or system actor) and MUST follow the same provider-id sanitization rules as user-initiated turns.
- They MUST still obey global cost controls (tenant-level token budgets, kill switches) but are not part of per-user quota enforcement.

**P1 mandatory**: the transactional usage outbox (`modkit_outbox_events`), CAS-guarded finalization, and the orphan turn watchdog are P1 requirements — they are required for billing event completeness (see DESIGN.md sections 5.2–5.5 and [outbox-pattern.md](features/outbox-pattern.md)).

**Deferred to P2+**: detailed billing integration contracts (formal event payload schemas, RPC interfaces, credit proxy endpoints). See DESIGN.md section 5.6 for the full deferral list.

### 5.7 Collaboration (P2+)

#### Group Chats

- [ ] `p2` - **ID**: `cpt-cf-mini-chat-fr-group-chats`

Group chats and chat sharing (projects) are deferred to P2+ and are out of scope for P1.

**Rationale**: Collaborative chat scenarios require shared access control, presence awareness, and conflict resolution that add significant complexity beyond the P1 single-user model.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

### 5.8 UX Recovery Contract (P1)

#### UX Recovery

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-fr-ux-recovery`

The UI experience MUST be resilient to SSE disconnects and idempotency conflicts.

##### Disconnect before terminal event

- If the SSE stream disconnects before `done`/`error`, the UI MUST treat the send as indeterminate and MUST NOT auto-retry `POST /messages:stream` with the same `request_id`.
- After disconnect, the UI MUST call `GET /v1/chats/{chat_id}/turns/{request_id}` to determine whether the turn completed.
- The UI MUST show a user-visible banner with the exact text: `Connection lost. Message delivery is uncertain. You can resend.`
- If the user chooses to resend, the UI MUST generate a new `request_id`.

##### 409 Conflict (active generation)

- On `409 Conflict` for `(chat_id, request_id)`, the UI MUST show a user-visible banner with the exact text: `A response is already in progress for this message. Please wait.`

##### Completed replay (idempotent replay)

- If the server replays a completed generation for an existing `(chat_id, request_id)`, the UI MUST render the response without duplicating the message in the timeline.
- The UI MUST show a non-blocking banner with the exact text: `Recovered a previously completed response.`

**Rationale**: Users need deterministic recovery paths after network interruptions to avoid duplicate messages, lost responses, or confusion about message delivery state.
**Actors**: `cpt-cf-mini-chat-actor-chat-user`

## 6. Non-Functional Requirements

### 6.1 Module-Specific NFRs

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-nfr-tenant-isolation`

Tenant data MUST never be accessible to users from another tenant. All data queries, file operations, and vector store searches MUST be scoped by tenant. The API MUST NOT accept or return raw external resource identifiers (file IDs, vector store IDs, provider response IDs, or any other provider-scoped identifier) from or to clients. All client-visible identifiers MUST be internal UUIDs only (`chat_id`, `attachment_id`, `message_id`, `request_id`). Error messages returned to clients MUST NOT contain provider identifiers; provider error messages that include provider-scoped IDs MUST be sanitized before being returned.

Parent tenant / MSP administrators MUST NOT have access to chat content. Admin visibility is limited to aggregated usage and operational metrics.

Authorization follows the platform PDP/PEP fail-closed rules (including 404 masking for denied requests with a concrete resource ID); see DESIGN.md (Authorization / Fail-Closed Behavior).

**Threshold**: Zero cross-tenant data leaks
**Rationale**: Multi-tenant SaaS with sensitive documents requires strict data boundaries.
**Architecture Allocation**: See DESIGN.md section 2.1 (Tenant-Scoped Everything principle)

#### Authorization Alignment

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-nfr-authz-alignment`

Authorization MUST follow the platform PDP/PEP model, including query-level constraints compiled to SQL by the PEP and fail-closed behavior on PDP errors or unreachability.

**Threshold**: Zero unauthorized reads/writes; fail-closed on 100% of PDP failures
**Rationale**: Chat content is sensitive and access must be enforced consistently at the query layer.
**Architecture Allocation**: See DESIGN.md section 3.8 (Authorization (PEP)) and Authorization Design (platform)

#### Cost Predictability

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-nfr-cost-control`

Per-user LLM costs MUST be bounded by configurable token-based rate limits across multiple periods (daily, monthly), tracked in real-time. Premium models have stricter limits; standard-tier models have separate, higher limits. File search and web search costs MUST be bounded by per-turn and per-day call limits. The system MUST track actual costs with tenant aggregation and per-user attribution for quota enforcement. Administrator visibility is limited to aggregated usage and operational metrics.

**Threshold**: No user exceeds configured quota; estimated cost available for 100% of requests
**Rationale**: Unbounded LLM usage can generate unexpected costs; tenants need cost predictability.
**Architecture Allocation**: See DESIGN.md section 3.2 (quota_service component)

#### Streaming Latency

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-nfr-streaming-latency`

The system MUST minimize platform overhead beyond provider latency. Define `mini_chat_ttft_overhead_ms = t_first_token_ui - t_first_byte_from_provider`. Streaming events MUST be relayed without buffering.

**Threshold**: `mini_chat_ttft_overhead_ms` p99 < 50 ms (platform overhead excluding provider latency)
**Rationale**: Users expect near-instant response start in a chat interface.
**Architecture Allocation**: See DESIGN.md section 2.1 (Streaming-First principle)

#### Data Retention Compliance

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-nfr-data-retention`

Deleted chat resources (files, vector stores) at the external provider MUST be removed on a best-effort basis (target: within 1 hour under normal conditions; eventual with retry/backoff on provider errors). This is an operational target, not a guaranteed SLA. Temporary chat auto-deletion (24h TTL) is deferred to P2.

**Threshold**: Best-effort target: external resource cleanup within 1 hour under normal conditions. Not a guaranteed SLA; eventual consistency with retry/backoff on provider errors
**Rationale**: Regulatory and customer contractual requirements for data lifecycle management.
**Architecture Allocation**: See DESIGN.md section 4 (Cleanup on Chat Deletion)

### 6.2 Observability and Supportability (P1)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-nfr-observability-supportability`

Mini Chat MUST provide an explicit operational contract to support on-call, SRE, and cost governance. This includes:

#### Required support signals (P1)

- Every chat turn MUST have a stable `request_id` (client idempotency key) and a persisted internal turn state (`running|completed|failed|cancelled`) that is exposed via the Turn Status API as (`running|done|error|cancelled`).
- A turn in `completed` state MUST have its full assistant message content durably persisted in the database, guaranteeing that idempotent replay for `(chat_id, request_id)` always returns the stored result; if persistence fails, the turn MUST be finalized as `failed`, never `completed`.
- Every completed provider request MUST be correlated via `provider_response_id` and MUST be persisted and searchable by operators.
- Support tooling MUST be able to determine turn state using server-side state (not inferred from client retry behavior).

#### Prometheus metrics contract (P1)

The service MUST expose Prometheus metrics with the following series names (types and label sets as specified in DESIGN.md):

Prometheus labels MUST NOT include high-cardinality identifiers such as `tenant_id`, `user_id`, `chat_id`, `request_id`, or `provider_response_id`.

##### Streaming and UX health

- `mini_chat_stream_started_total{provider,model}`
- `mini_chat_stream_completed_total{provider,model}`
- `mini_chat_stream_failed_total{provider,model,error_code}`
- `mini_chat_stream_disconnected_total{stage}`
- `mini_chat_stream_replay_total{reason}`
- `mini_chat_active_streams{instance}`
- `mini_chat_ttft_provider_ms{provider,model}`
- `mini_chat_ttft_overhead_ms{provider,model}`
- `mini_chat_stream_total_latency_ms{provider,model}`

##### Cancellation

- `mini_chat_cancel_requested_total{trigger}`
- `mini_chat_cancel_effective_total{trigger}`
- `mini_chat_tokens_after_cancel{trigger}`
- `mini_chat_time_to_abort_ms{trigger}`
- `mini_chat_time_from_ui_disconnect_to_cancel_ms{trigger}`
- `mini_chat_cancel_orphan_total`
- `mini_chat_streams_aborted_total{trigger}`

##### Quota and cost control

- `mini_chat_quota_preflight_total{decision,model}`
- `mini_chat_quota_preflight_v2_total{kind,decision,model}`
- `mini_chat_quota_preflight_v2_total` exists to add `{kind}` without changing the label set of `mini_chat_quota_preflight_total`.
- `mini_chat_quota_reserve_total{period}`
- `mini_chat_quota_commit_total{period}`
- `mini_chat_quota_overshoot_total{period}`
- `mini_chat_quota_negative_total{period}`
- `mini_chat_quota_estimated_tokens`
- `mini_chat_quota_actual_tokens`
- `mini_chat_quota_overshoot_tokens`
- `mini_chat_quota_reserved_tokens{period}`

##### Tools and retrieval

- `mini_chat_tool_calls_total{tool,phase}` (`tool`: `file_search|web_search`)
- `mini_chat_tool_call_limited_total{tool}` (`tool`: `file_search|web_search`)
- `mini_chat_file_search_latency_ms{provider,model}`
- `mini_chat_web_search_latency_ms{provider,model}`
- `mini_chat_web_search_disabled_total`
- `mini_chat_citations_count`
- `mini_chat_citations_by_source_total{source}` (`source`: `file|web`)

##### Summarization health

- `mini_chat_summary_regen_total{reason}`
- `mini_chat_summary_fallback_total`

##### Turn mutations

- `mini_chat_turn_mutation_total{op,result}`
- `mini_chat_turn_mutation_latency_ms{op}`

##### Provider / OAGW interaction

- `mini_chat_provider_requests_total{provider,endpoint}`
- `mini_chat_provider_errors_total{provider,status}`
- `mini_chat_oagw_retries_total{provider,reason}`
- `mini_chat_oagw_circuit_open_total{provider}`
- `mini_chat_provider_latency_ms{provider,endpoint}`
- `mini_chat_oagw_upstream_latency_ms{provider,endpoint}`

##### Upload and attachments

- `mini_chat_attachment_upload_total{kind,result}` (`kind`: `document|image`)
- `mini_chat_attachment_index_total{result}`
- `mini_chat_attachment_summary_total{result}`
- `mini_chat_attachments_pending{instance}`
- `mini_chat_attachments_failed{instance}`
- `mini_chat_attachment_upload_bytes{kind}` (`kind`: `document|image`)
- `mini_chat_attachment_index_latency_ms`

##### Image usage (per-turn)

- `mini_chat_image_inputs_per_turn` (histogram; number of images in a single Responses API call)
- `mini_chat_image_turns_total{model}` (counter; turns that included >=1 image)
- `mini_chat_media_rejected_total{reason}` (counter; `reason`: `too_many_images|image_bytes_exceeded|unsupported_media`)

##### Image quota enforcement

- `mini_chat_quota_preflight_v2_total{kind,decision,model}` (counter; `kind`: `text|image`; `decision`: `allow|downgrade|reject`) - see Quota and cost control section above
- `mini_chat_quota_image_commit_total{period}` (counter; `period`: `daily|monthly`)

##### Cleanup and drift

- `mini_chat_cleanup_job_runs_total{kind}`
- `mini_chat_cleanup_attempts_total{op,result}`
- `mini_chat_cleanup_orphan_found_total{kind}`
- `mini_chat_cleanup_orphan_fixed_total{kind}`
- `mini_chat_cleanup_backlog{state}`
- `mini_chat_cleanup_latency_ms{op}`

##### Audit emission health

- `mini_chat_audit_emit_total{result}`
- `mini_chat_audit_redaction_hits_total{pattern}`
- `mini_chat_finalization_latency_ms`

##### DB health (infra/storage)

- `mini_chat_db_query_latency_ms{query}`
- `mini_chat_db_errors_total{query,code}`

#### SLOs / thresholds (P1)

- `mini_chat_ttft_overhead_ms` p99 < 50 ms
- `mini_chat_time_to_abort_ms` p99 < 200 ms
- Provider cleanup target completion within 1 hour under normal conditions (eventual with retry)

### 6.3 RAG Scalability (P1)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-nfr-rag-scalability`

RAG retrieval costs and quality MUST remain bounded as document volume grows within a chat. The system MUST enforce per-chat document count, total file size, and indexed chunk limits (see `cpt-cf-mini-chat-fr-per-chat-doc-limits`). Retrieval parameters (top-k, max retrieved tokens per turn) MUST be configurable. Each chat with documents MUST use a dedicated per-chat vector store to ensure isolation and predictable retrieval latency.

**Threshold**: Per-chat limit enforcement with zero breaches; `mini_chat_file_search_latency_ms` p95 within configured threshold
**Rationale**: Unbounded document ingestion degrades retrieval relevance and inflates per-turn costs via excessive chunk processing.
**Architecture Allocation**: See DESIGN.md section 1.2 (NFR Allocation Matrix) and section on per-chat vector stores

### 6.4 Resilience and Recovery (P1)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-nfr-resilience-recovery`

#### Pod restart / service crash during streaming

If the chat service pod crashes or restarts while an SSE stream is active, the connection drops without a terminal `done` or `error` event. The user may not know whether the response completed. The system MUST allow the user to recover safely without data loss or duplicate messages.

#### Turn recovery contract

After a disconnect, the client MUST call `GET /v1/chats/{chat_id}/turns/{request_id}` to determine the turn outcome:

| Turn state | Client action |
|------------|---------------|
| `completed` | Replay the completed response (idempotent) |
| `running` | Wait and poll again, or inform the user that generation is still in progress |
| `failed` or `cancelled` | Resend with a **new** `request_id` |

The client MUST NOT automatically resend `POST /messages:stream` with the same `request_id` after a disconnect. Retry and edit operations both create a new turn and MUST use a new server-generated `request_id`. Reusing a previously completed `request_id` will result in replay of the existing result.

#### Orphan turn handling

Turns stuck in `running` state beyond a configurable timeout (e.g. pod crash with no graceful shutdown) MUST be automatically transitioned to `failed` by a background process. This ensures the user is never permanently blocked by a stale turn.

#### P1 constraints

- No partial streaming replay: if a response was partially streamed before crash, the partial content is lost. The user must retry.
- No automatic continuation after crash: the system does not resume generation from where it left off.
- Idempotency via `request_id`: duplicate `(chat_id, request_id)` never creates a new turn; completed turns are replayed, running turns return 409.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Chat REST API

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-interface-public-api`

**Type**: REST API
**Stability**: stable
**Description**: Public HTTP API for chat management, message listing with cursor pagination, message streaming, file upload, attachment status polling, and message reactions. All endpoints require authentication and tenant license verification.
**Breaking Change Policy**: Versioned via URL prefix (`/v1/`). Breaking changes require new version.

#### Turn Status (read-only) API

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-interface-turn-status`

Support and UX recovery flows MUST be able to query authoritative turn state backed by `chat_turns`.

**Endpoint**: `GET /v1/chats/{chat_id}/turns/{request_id}`

**Response** (`chat_id` is not included — it is already present in the URL path):

- `request_id`
- `state`: `running|done|error|cancelled`
- `error_code` (nullable string) — terminal error code when `state` is `error` (e.g. `provider_error`, `orphan_timeout`). Null for non-error states and while running. Provider identifiers and billing outcome are not exposed.
- `assistant_message_id` (nullable UUID) — persisted assistant message ID when `state` is `done`. Null while running, on error, or on cancellation. Allows clients to fetch the assistant message directly via `GET /v1/chats/{id}/messages?$filter=id eq '{assistant_message_id}'` without scanning full history.
- `updated_at`

**Internal-to-API state mapping**:

| Internal State (`chat_turns.state`) | Turn Status API | SSE Terminal Event |
|-------------------------------------|-----------------|-------------------|
| `running` | `running` | _(not terminal)_ |
| `completed` | `done` | `done` |
| `failed` | `error` | `error` |
| `cancelled` | `cancelled` | _(none; stream already disconnected)_ |

### 7.2 External Integration Contracts

#### SSE Streaming Contract

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-contract-sse-streaming`

**Direction**: provided by library
**Protocol/Format**: Server-Sent Events (SSE) over HTTP
**Compatibility**: Event types (`delta`, `tool`, `citations`, `done`, `error`, `ping`) and their payload schemas are stable within a major API version.

**Ordering (P1)**: `ping* delta* tool* citations? (done | error)`. Zero or more `ping` events may appear at any point. `delta` and `tool` events may interleave in any order. At most one `citations` event, emitted after all `delta` events and before the terminal event. Exactly one terminal event (`done` or `error`) ends the stream. Broader interleaving (multiple `citations` events interleaved with content) is forward-compatible for P2+.

**Stream close**: the server MUST close the SSE connection immediately after emitting the terminal event. No further events are permitted after the terminal `done` or `error`.

**Error model (Option A)**: If the request fails validation, authorization, or quota preflight before streaming begins, the server MUST return a normal JSON error response with the appropriate HTTP status and MUST NOT open an SSE stream. If the stream has started, the server MUST report failure via a terminal `event: error`.

**Error Codes**:

| Code | HTTP Status | Description |
|------|-------------|-------------|
| `invalid_request` | 400 | Request body fails validation (e.g. missing required field, value out of range) |
| `feature_not_licensed` | 403 | Tenant lacks `ai_chat` feature |
| `insufficient_permissions` | 403 | Subject lacks permission for the requested action (AuthZ Resolver denied) |
| `chat_not_found` | 404 | Chat does not exist or not accessible under current authorization constraints |
| `generation_in_progress` | 409 | A generation is already running for this chat (one running turn per chat policy) |
| `request_id_conflict` | 409 | The same `(chat_id, request_id)` is already in a non-replayable state (`running`, `failed`, or `cancelled`) |
| `not_latest_turn` | 409 | Target `request_id` is not the most recent non-deleted turn; retry/edit/delete mutations apply only to the latest turn |
| `quota_exceeded` | 429 | Quota exhaustion. Always accompanied by `quota_scope`: `"tokens"` (token rate limits across all tiers exhausted, emergency flags, or all models disabled), `"uploads"` (daily upload quota exceeded), `"web_search"` (per-user daily web search call quota exhausted), or `"image_inputs"` (per-turn or per-day image input limit exceeded) |
| `rate_limited` | 429 | Provider upstream throttling (provider 429 after OAGW retry exhaustion) |
| `file_too_large` | 413 | Uploaded file exceeds size limit |
| `unsupported_file_type` | 415 | File type not supported for upload |
| `web_search_disabled` | 400 | Request includes `web_search.enabled=true` but the global `disable_web_search` kill switch is active |
| `too_many_images` | 400 | Request includes more than the configured maximum images for a single turn |
| `image_bytes_exceeded` | 413 | Request includes images whose total configured per-turn byte limit is exceeded |
| `unsupported_media` | 415 | Request includes image input but the effective model does not support multimodal input. Defensive under P1 catalog invariant (all enabled models include `VISION_INPUT`); expected only on catalog misconfiguration or future non-vision models. |
| `provider_error` | 502 | LLM provider returned an error |
| `provider_timeout` | 504 | LLM provider request timed out |

HTTP 429 responses may carry either `quota_exceeded` (with `quota_scope`) for user quota exhaustion or `rate_limited` for upstream provider throttling; clients MUST use the `code` field to distinguish between the two.

Provider identifiers (`provider_file_id`, `provider_response_id`, `vector_store_id`, and any other provider-issued ID) are internal-only and MUST NOT be exposed in any API response, SSE event payload, or error message. Error `message` fields MUST be sanitized to remove any provider-issued identifiers before being returned to clients. All client-visible identifiers are internal UUIDs only (`chat_id`, `turn_id`, `request_id`, `attachment_id`, `message_id`).

`tenant_id` and `user_id` are NOT returned in API response bodies. User and tenant identity is derived exclusively from the authentication context (Platform AuthN JWT). These fields are stored internally but are not part of the public Chat API contract.

## 8. Use Cases

### UC-001: Send Message and Receive Streamed Response

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-usecase-send-message`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Preconditions**:
- User is authenticated and tenant has `ai_chat` license
- Chat exists and belongs to the user

**Main Flow**:
1. User sends a message to an existing chat
2. System checks and reserves user quota (Preflight (reserve))
3. System assembles conversation context (summary, recent messages, document summaries)
4. System streams AI response SSE events back to the user in real-time
5. System persists both user message and assistant response
6. System emits audit events with usage metrics

**Postconditions**:
- Message and response persisted in chat history
- Usage counters updated
- Audit events emitted to platform `audit_service`

**Alternative Flows**:
- **Quota exceeded**: System rejects request with `quota_exceeded` error (HTTP 429 JSON error response); no LLM call made and no SSE stream is opened
- **Client disconnects**: System cancels in-flight LLM request; partial response may be persisted. Delivery is indeterminate; the UI SHOULD first query `GET /v1/chats/{chat_id}/turns/{request_id}` to determine whether the turn completed. If the user resends, resend MUST use a new `request_id`.

#### UC-006: Reconnect After Network Loss (Turn Status Check)

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-usecase-reconnect-turn-status`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Preconditions**:

- The UI previously started a streaming send with a `request_id`.
- The SSE stream disconnected before terminal `done`/`error`.

**Main Flow**:

1. The UI calls `GET /v1/chats/{chat_id}/turns/{request_id}`.
2. If `state=done`, the UI renders the previously completed response and shows `Recovered a previously completed response.`
3. If `state=running`, the UI informs the user that a response is still in progress and does not resend.
4. If `state=error|cancelled`, the UI allows the user to resend using a new `request_id`.

#### UC-002: Send Message with Document Search

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-usecase-doc-search`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Preconditions**:
- Same as UC-001
- At least one document is attached to the chat and has `ready` status

**Main Flow**:
1. User sends a message that references document content.
2. System searches across all documents currently present in the chat vector store.
3. System retrieves relevant excerpts from the chat's vector store
4. System includes excerpts in the LLM context alongside conversation history
5. System streams AI response grounded in document content

**Postconditions**:
- Response incorporates information from uploaded documents in the chat knowledge base
- File search call counted against user quota

**Alternative Flows**:
- **File search limit reached**: System proceeds without retrieval; response based on conversation context and document summaries only

#### UC-003: Upload Document

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-usecase-upload-document`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Preconditions**:
- User is authenticated and tenant has `ai_chat` license
- Chat exists and belongs to the user
- File is a supported document type and within size limits

**Main Flow**:
1. User uploads a document file to a chat
2. System stores the file with the external provider
3. System indexes the file in the tenant's document search index
4. System enqueues a brief summary generation of the document (background, `requester_type=system`)
5. System returns attachment ID and processing status (`pending`)
6. UI polls `GET /v1/chats/{id}/attachments/{attachment_id}` until status is `ready` or `failed`
7. `doc_summary` is populated asynchronously by the server when processing completes

**Postconditions**:
- Document is searchable in subsequent chat messages
- Document summary available for context assembly (once background processing completes)

**Alternative Flows**:
- **Unsupported file type**: System rejects with `unsupported_file_type` error (HTTP 415)
- **File too large**: System rejects with `file_too_large` error (HTTP 413)
- **Processing failure**: Attachment status set to `failed` with `error_code`; user informed via polling

#### UC-010: Upload Image and Ask About It

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-usecase-upload-image`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Preconditions**:
- User is authenticated and tenant has `ai_chat` license
- Chat exists and belongs to the user
- File is a supported image type (PNG, JPEG, WebP) and within size limits
- Effective model supports image input

**Main Flow**:
1. User uploads an image file to a chat
2. System stores the image with the external provider via Files API
3. System does NOT add the image to any vector store
4. System returns attachment ID and processing status (`pending`)
5. UI polls `GET /v1/chats/{id}/attachments/{attachment_id}` until status is `ready` or `failed`
6. User sends a message with the image explicitly attached to that turn via `attachment_ids` (message `content` remains plain text)
7. System includes the image as a multimodal input (file ID reference) in the Responses API call
8. System streams AI response that describes or reasons about the image content

**Postconditions**:
- Image attachment persisted with `attachment_kind=image`
- AI response references image content
- Image usage counters updated in quota_usage

**Alternative Flows**:
- **Unsupported image type**: System rejects with `unsupported_file_type` error (HTTP 415)
- **Image too large**: System rejects with `file_too_large` error (HTTP 413)
- **Model does not support images**: System rejects with `unsupported_media` error (HTTP 415)
- **Per-message image limit exceeded**: System rejects with `too_many_images` error (HTTP 400)
- **Per-message image bytes limit exceeded**: System rejects with `image_bytes_exceeded` error (HTTP 413)
- **Daily image quota exceeded**: System rejects with `quota_exceeded` error (HTTP 429)

#### UC-004: Delete Chat

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-usecase-delete-chat`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Preconditions**:
- Chat exists and belongs to the user

**Main Flow**:
1. User requests chat deletion
2. System soft-deletes the chat
3. System marks attachments for cleanup and returns
4. Cleanup worker deletes the chat's vector store (entire store) and provider files (idempotent retries)
5. System emits audit events

**Postconditions**:
- Chat no longer appears in user's chat list
- External resources cleaned up
- Audit events emitted to platform `audit_service`

#### UC-005: Temporary Chat Auto-Deletion (P2)

- [ ] `p2` - **ID**: `cpt-cf-mini-chat-usecase-temporary-chat-cleanup`

**Actor**: `cpt-cf-mini-chat-actor-cleanup-scheduler`

**Preconditions**:
- Temporary chat exists with creation time > 24 hours ago

**Main Flow**:
1. Scheduler identifies expired temporary chats
2. System executes the same deletion flow as UC-004 for each expired chat

**Postconditions**:
- All expired temporary chats and their external resources are removed

#### UC-007: Retry Last Turn

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-usecase-retry-turn`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Preconditions**:
- Chat exists and belongs to the user
- The last turn is in a terminal state (`completed`, `failed`, or `cancelled`)

**Main Flow**:
1. User requests retry of the last turn
2. System verifies the target turn is the most recent and in a terminal state
3. System soft-deletes the previous turn and creates a new turn
4. System re-submits the original user message for a new assistant response (same streaming flow as UC-001)
5. System emits `turn_retry` audit event

**Postconditions**:
- New assistant response persisted as a new turn; previous turn soft-deleted but retained for audit
- Audit event emitted

**Alternative Flows**:
- **Not the latest turn**: System rejects with `409 Conflict`
- **Turn still running**: System rejects with `400 Bad Request`. Client may cancel streaming by disconnecting the SSE stream; once the turn reaches a terminal state, mutation is allowed.

#### UC-008: Edit Last User Turn

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-usecase-edit-turn`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Preconditions**:
- Chat exists and belongs to the user
- The last turn is in a terminal state (`completed`, `failed`, or `cancelled`)

**Main Flow**:
1. User submits edited content for the last turn
2. System verifies the target turn is the most recent and in a terminal state
3. System soft-deletes the previous turn
4. System creates a new turn with the updated user message content
5. System generates a new assistant response (same streaming flow as UC-001)
6. System emits `turn_edit` audit event

**Postconditions**:
- New turn with updated content and new assistant response persisted
- Previous turn soft-deleted but retained for audit
- Audit event emitted

**Alternative Flows**:
- **Not the latest turn**: System rejects with `409 Conflict`
- **Turn still running**: System rejects with `400 Bad Request`. Client may cancel streaming by disconnecting the SSE stream; once the turn reaches a terminal state, mutation is allowed.

#### UC-009: Delete Last Turn

- [ ] `p1` - **ID**: `cpt-cf-mini-chat-usecase-delete-turn`

**Actor**: `cpt-cf-mini-chat-actor-chat-user`

**Preconditions**:
- Chat exists and belongs to the user
- The last turn is in a terminal state (`completed`, `failed`, or `cancelled`)

**Main Flow**:
1. User requests deletion of the last turn
2. System verifies the target turn is the most recent and in a terminal state
3. System soft-deletes the turn (user message + assistant response)
4. System emits `turn_delete` audit event

**Postconditions**:
- Turn no longer visible in active conversation history
- Soft-deleted turn retained for audit
- Audit event emitted

**Alternative Flows**:
- **Not the latest turn**: System rejects with `409 Conflict`
- **Turn still running**: System rejects with `400 Bad Request`. Client may cancel streaming by disconnecting the SSE stream; once the turn reaches a terminal state, mutation is allowed.

## 9. Acceptance Criteria

- [ ] User can create a chat, send messages, and receive streamed AI responses with `mini_chat_ttft_overhead_ms` p99 < 50 ms platform overhead (excluding provider latency)
- [ ] Cancellation propagation meets design thresholds: `mini_chat_time_to_abort_ms` p99 < 200 ms and `mini_chat_tokens_after_cancel` p99 < 50 tokens
- [ ] User can upload a document and ask questions that are answered using document content
- [ ] Users from different tenants cannot access each other's chats, documents, or search results
- [ ] User exceeding premium-tier quota (in any period: daily or monthly) is auto-downgraded to the standard tier; standard-tier models have separate, higher limits; when all tiers are exhausted, the system rejects with `quota_exceeded`
- [ ] Effective model used for each turn is recorded in `messages.model`, SSE `done` event (`effective_model` + `selected_model` fields), and audit event payload; downgrade decisions are surfaced via optional `quota_decision`/`downgrade_from`/`downgrade_reason` fields
- [ ] When premium quota is exhausted, `effective_model != selected_model` in the SSE `done` event; the UI can display a downgrade banner based on this metadata
- [ ] When `web_search.enabled=true` and the `disable_web_search` kill switch is OFF, the provider request includes the `web_search` tool and citations can include web sources (`source: "web"` with `url`, `title`, `snippet`)
- [ ] When the `disable_web_search` kill switch is ON, requests with `web_search.enabled=true` are rejected with HTTP 400 and error code `web_search_disabled`
- [ ] Standard-tier usage is bounded by configured per-tier caps (not unlimited); exceeding all tier caps yields `quota_exceeded` rejection
- [ ] User can select a model from the catalog when creating a chat; the model is locked for the chat lifetime; all turns use the selected model (except system-driven quota downgrades)
- [ ] User can like or dislike an assistant message; reaction is persisted and retrievable via API; changing reaction replaces the previous one; removing reaction deletes it
- [ ] Deleted chat resources are removed from the external provider (best-effort target: within 1 hour under normal conditions; eventual with retry/backoff; not a guaranteed SLA)
- [ ] Every completed chat turn emits a structured audit event to platform `audit_service` (one event per completed turn) including usage metrics
- [ ] Long conversations (50+ turns) remain functional via thread summary compression; compression triggers when message count exceeds 20, token count exceeds budget, or every 15 user turns (see DESIGN.md `cpt-cf-mini-chat-seq-thread-summary` for threshold details)
- [ ] User can retry, edit, or delete the last turn; operations on non-latest turns are rejected with `409 Conflict`
- [ ] User can upload an image attachment (PNG/JPEG/WebP) and ask "what is in this image" and receive a relevant answer
- [ ] Image attachments do not appear in file_search citations
- [ ] Quota limits for images are enforced: per-turn image input limit and per-day image input limit reject requests that exceed configured caps
- [ ] Audit events for turns with image input do not include raw image bytes; only attachment metadata (attachment_id, content_type, size_bytes, filename) is included
- [ ] Submitting an image to a model that does not support multimodal input returns `unsupported_media` error (HTTP 415) — defensive check; not expected under the P1 catalog invariant (all enabled models include `VISION_INPUT`)
- [ ] If a future catalog introduces a non-vision model, the downgrade cascade selecting that model for an image-bearing turn MUST reject with `unsupported_media` (HTTP 415) before any outbound provider call; images are never silently dropped. Under the P1 catalog invariant this path is unreachable.
- [ ] User can delete an attachment via `DELETE /v1/chats/{id}/attachments/{attachment_id}`; after deletion the attachment is immediately excluded from future `file_search` retrieval on subsequent turns
- [ ] Deleting an attachment does not modify historical messages that reference it; the `attachments` array on past messages still includes the deleted attachment's metadata
- [ ] Re-deleting an already-deleted attachment is idempotent (returns 204 No Content)
- [ ] Given a message that has not yet been sent, when the user removes an attachment from the draft, then the attachment is removed successfully
- [ ] Given an attachment that is not referenced by any submitted message, when the user calls `DELETE /v1/chats/{id}/attachments/{attachment_id}`, then the attachment is deleted successfully and the API returns 204 No Content
- [ ] Given an attachment that is referenced by a submitted message, when the user calls `DELETE /v1/chats/{id}/attachments/{attachment_id}`, then the API returns HTTP 409 Conflict with error code `attachment_locked`
- [ ] Provider-side cleanup (file deletion, vector store removal) is performed asynchronously via transactional outbox; partial provider failure does not block the API response or leave the attachment visible to retrieval
- [ ] File search retrieval only considers documents attached to the current chat (each chat has its own dedicated vector store; no cross-chat leakage by design)
- [ ] Full file text is not injected into the prompt; only top-k retrieved chunks are included
- [ ] Per-chat document count and total file size limits are enforced; uploads exceeding limits are rejected

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| Platform API Gateway | HTTP routing, SSE transport | `p1` |
| Platform AuthN | User authentication, tenant resolution | `p1` |
| Outbound API Gateway (OAGW) | External API egress, credential injection | `p1` |
| OpenAI-compatible Responses API (OpenAI / Azure OpenAI) | LLM chat completion (streaming and non-streaming) | `p1` |
| OpenAI-compatible Files API (OpenAI / Azure OpenAI) | Document and image upload and storage | `p1` |
| Responses API multimodal input (OpenAI / Azure OpenAI) | Image-aware chat via file ID references in request content | `p1` |
| OpenAI-compatible Vector Stores / File Search (OpenAI / Azure OpenAI) | Document indexing and retrieval | `p1` |
| PostgreSQL | Primary data storage | `p1` |
| Platform license_manager | Tenant feature flag resolution (`ai_chat`) | `p1` |
| Platform audit_service | Audit event ingestion (prompts, responses, usage, policy decisions) | `p1` |

## 11. Assumptions

- OpenAI-compatible Responses API (including multimodal input), Files API, and File Search remain stable and available (OpenAI or Azure OpenAI)
- OAGW supports streaming SSE relay and credential injection for OpenAI and Azure OpenAI endpoints
- OAGW owns Azure OpenAI endpoint details including required `api-version` parameters and path variants
- Platform AuthN provides `user_id` and `tenant_id` in the security context for every request
- Platform `license_manager` can resolve the `ai_chat` feature flag synchronously
- Platform `audit_service` is available to receive audit events
- One provider vector store per chat is sufficient for P1 document volumes
- Files (documents and images) are stored in the LLM provider's storage (OpenAI / Azure OpenAI via Files API); Mini Chat does not operate first-party object storage (no S3 or equivalent)
- Thread summary quality is adequate for maintaining conversational coherence over long chats

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| OpenAI-compatible provider API changes or deprecation (OpenAI / Azure OpenAI) | Feature breakage; requires rework | Pin API versions; monitor deprecation notices; design for eventual multi-provider |
| Provider outage or degraded performance (OpenAI / Azure OpenAI) | Chat unavailable or slow | Circuit breaking via OAGW; clear error messaging to users; eventual fallback provider (P2+) |
| Cost overruns from unexpected usage patterns | Budget exceeded at tenant level | Per-user quotas; file search call limits; token budgets; cost monitoring and alerts |
| Thread summary loses critical context | Degraded conversation quality over long chats | Include explicit instructions to preserve decisions, facts, names, document refs; allow users to start new chats |
| Vector store data consistency on deletion | Orphaned files at provider | Idempotent cleanup with retry; reconciliation job for detecting orphans |
| Large number of chats with documents creating many vector stores | Provider API limits on vector store count; increased storage costs | Monitor vector store count per user via metrics; enforce per-chat document limits; plan per-workspace aggregation (P2) |
| Image spam / abuse driving excessive provider costs | Unexpected cost spikes from high-volume or large image uploads | Per-message image input cap (default: 4); per-user daily image input cap (default: 50); configurable byte limits; image-specific quota counters and metrics |
| Provider model does not support multimodal input | Image-bearing requests fail | The domain service checks model capability before outbound call; rejects with `unsupported_media` (HTTP 415) if effective model lacks image support; operator configures which models support images. P1 catalog invariant: all enabled models include `VISION_INPUT`, so this risk applies only if a future catalog introduces a non-vision model. |

## 13. Open Questions

- What document file types are supported in P1 beyond `pdf`, `docx`, and plain text?
- What is the exact UX when `state=running` is returned from Turn Status API (poll cadence, max wait, and banner text)?
- Thread summary trigger thresholds are defined in DESIGN.md (msg count > 20 OR tokens > budget OR every 15 user turns)
- Is the system prompt configurable per tenant, or fixed platform-wide?

### 13.1 P1 Defaults (configurable)

These defaults are used for P1 planning and MUST be configurable per tenant/operator:

- Model catalog (ordered by tier):
  - Premium tier: `gpt-5.2` (default for new chats)
  - Standard tier: `gpt-5-mini`
- Downgrade cascade: premium → standard; when all tiers exhausted → reject with `quota_exceeded`
- Default premium-tier token rate limits: daily `50_000`, monthly `1_000_000`
- Default standard-tier token rate limits: daily `200_000`, monthly `5_000_000` (configurable per deployment)
- Web search per-turn call limit: 2 (deployment config: `web_search.max_calls_per_turn: 2`)
- Web search per-user daily quota: 75 (deployment config: `web_search.daily_quota: 75`)
- Web search provider parameters: **Deferred to P2+**. P1 uses provider defaults. When implemented, configurable via `web_search.provider_parameters` (search_depth, max_results, include_answer, include_raw_content, include_images, auto_parameters).
- Upload size limit: 16 MiB (deployment config example: `uploaded_file_max_size_kb: 16384`); PM target: 25 MiB (configurable)
- Image upload size limit: same as above unless overridden (deployment config example: `uploaded_image_max_size_kb: 16384`)
- Max image inputs per message: 1 (deployment config example: `max_images_per_message: 1`)
- Max image inputs per user per day: 50 (deployment config example: `max_images_per_user_daily: 50`)
- Temporary chat retention window: 24 hours (P2; deployment config example: `temporary_chat_retention_hours: 24`)

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/) (planned)
