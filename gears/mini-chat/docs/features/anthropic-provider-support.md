# Feature: Anthropic Model Support

## 0. Implementation Status

This document started as the design for Anthropic support. Parts of it are implemented and parts are not. The table below is the current state; sections that describe unimplemented behaviour carry a **Not implemented** note. The P1 retrieval scope is recorded in [ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md); the provider adapter model in [ADR-0005](../ADR/0005-cpt-cf-mini-chat-adr-multi-provider-adapters.md).

| Capability | Status | Where |
|---|---|---|
| `ProviderKind::AnthropicMessages` adapter (`/v1/messages`, SSE, `complete()`) | Implemented | `infra/llm/providers/anthropic_messages.rs`, `infra/llm/providers/mod.rs` |
| Native `web_search_20260209` and `code_execution_20250825` tools | Implemented. Web search citations are **not implemented**: the adapter returns `citations: vec![]` and does not parse `web_search_tool_result` or citation blocks | `anthropic_messages.rs` (tool mapping) |
| Function tools (e.g. `search_knowledge`) | Implemented. The function-tool loop runs in `StreamService` (`domain/service/stream_service/provider_task.rs`), not inside the adapter | `provider_task.rs` |
| `rag_provider` on `ProviderEntry`, `ProviderResolver::resolve_rag_provider()` | Implemented | `config.rs`, `infra/llm/provider_resolver.rs` |
| Parallel upload to the Anthropic Files API | Implemented for **images only**, 30 s timeout, failure is non-fatal. Documents are not uploaded to Anthropic | `domain/service/attachment_service.rs`, `infra/llm/providers/anthropic_files_client.rs` |
| `attachments.secondary_file_id` / `secondary_status` / `secondary_provider_kind` | Implemented | `infra/db/entity/attachment.rs`, migration `m20260417_000004_add_secondary_upload_fields.rs` |
| Image blocks | Implemented. Images are sent automatically as `image` blocks with `source.type = "file"`, using the `provider_file_id → secondary_file_id` map; an image without an Anthropic copy is dropped from the request | `anthropic_messages.rs`, `AttachmentRepository::build_secondary_file_id_map` |
| Deleting the Anthropic copy on attachment/chat cleanup | Implemented in the attachment-cleanup outbox handler | `infra/workers/cleanup_worker.rs` |
| `search_files` tool loop and `vector_store_search.rs` | **Not implemented.** The adapter drops `LlmTool::FileSearch`, so documents indexed in the RAG provider's vector store are not searched in Anthropic chats | see [ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md) |
| `load_files` tool, `document` and `container_upload` blocks | **Not implemented.** Only the SSE tool-name mapping for `load_files` exists | see [ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md) |
| Prompt caching (`cache_control`) | Not implemented | — |

## 1. Feature Context

### 1.1 Overview

Add Anthropic Claude model support to mini-chat, enabling both Microsoft Foundry and Anthropic platform as LLM backends. Achieves feature parity with existing OpenAI providers: text streaming, native tools (web_search, code_execution), function calling, images, file access, and RAG.

### 1.2 Key Decisions

1. **One adapter, two platforms** — Microsoft Foundry and Anthropic Platform both use native Anthropic Messages API (`/v1/messages`). One `AnthropicMessagesProvider` serves both — the difference is only base URL and auth.

2. **Tool loop inside adapter** — Custom tool calls (`search_files`, `load_files`) are handled inside the adapter, invisible to `StreamService` and turn architecture. 1 turn = 1 `ProviderStream` regardless of internal LLM calls. *Not implemented:* neither tool exists ([ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md)). The only function-tool loop (`search_knowledge`) runs in `StreamService` (`provider_task.rs`).

3. **Files API on both platforms (beta)** — Both Microsoft Foundry and Anthropic Platform support Files API in beta. Confirmed in [Anthropic overview: Files and Assets](https://platform.claude.com/docs/en/build-with-claude/overview#files-and-assets) (as of March 2026). This enables `load_files` tool, `document`, `image`, and `container_upload` blocks on both platforms. *Implemented:* `image` blocks only.

4. **Model is immutable per chat** — `chat.model` is set at creation and cannot change (`cpt-cf-mini-chat-constraint-model-locked-per-chat`). This means: if a chat uses an Anthropic model, all files in that chat are for Anthropic. No model-switching scenarios to handle.

5. **Eager upload to both backends (Anthropic model chats only)** — Files always go to Azure/OpenAI (file store + vector store). When the chat model is Anthropic, also upload to Anthropic Files API **in parallel** — `secondary_file_id` is set immediately (images only; see §8.0). For OpenAI model chats, only the Azure/OpenAI upload happens (existing behavior, unchanged). Azure/OpenAI is the primary store: if it fails, the entire upload fails regardless of Anthropic result. If only the Anthropic upload fails, the file upload succeeds (`secondary_status=failed`) and the image is left out of Anthropic requests. This is possible because the chat model is immutable (decision #4).

6. **All files accessed via `load_files` tool** — Files (documents and images) are never included in requests automatically. Claude calls `load_files` when it needs file content. The adapter determines the correct content block type (`document`, `image`, or `container_upload`) based on attachment metadata. *Not implemented* ([ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md)). Images attached to a message are included automatically as `image` blocks; documents are not sent to Claude.

7. **Usage: sum all tokens** — Tool loop sums all token fields across internal LLM calls. Matches what Anthropic bills us and maintains parity with OpenAI (where retrieved chunks are baked into `input_tokens`).

### 1.3 Why Custom Tools + OpenAI/Azure Backend for RAG

Mini-chat's existing RAG flow delegates everything to the LLM provider: file upload → vector store creation → `file_search` tool → provider performs retrieval internally. OpenAI and Azure provide the full stack: file storage, embedding, vector indexing, and retrieval.

Anthropic **does not provide** this RAG infrastructure:

- **No vector store / search API** — no `/v1/vector_stores`, no server-side indexing or retrieval.
- **No `file_search` tool** — no native ability to search uploaded documents.
- **No embedding API** — [explicitly recommends](https://platform.claude.com/docs/en/build-with-claude/embeddings) third-party services (as of March 2026).
- **Files API is limited to content access** — [Files API](https://platform.claude.com/docs/en/build-with-claude/files) (beta, March 2026) allows file upload and referencing in messages, but provides no vector search or embedding.

Given these constraints, file access is split into two custom tools (**not implemented**, see [ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md)):

- **`search_files`** — semantic search via Azure/OpenAI vector store (`POST /v1/vector_stores/{vs_id}/search`)
- **`load_files`** — full file access via Anthropic Files API (`document` / `image` / `container_upload` blocks)

### 1.4 Two Classes of File Tasks

| Class | Examples | Mechanism | Tool |
|-------|---------|-----------|------|
| **Full file context** | "Summarize file", "Translate document", "Build chart from CSV", "Describe this image" | Anthropic Files API → `document` / `image` / `container_upload` | `load_files` |
| **Semantic search** | "What does the report say about revenue?", "Find risk mentions" | Azure/OpenAI vector store → scored chunks | `search_files` |

**Why both tools exist:**
- `search_files` is cheaper (only relevant chunks as input tokens) and scales to many files
- `load_files` gives complete content but is expensive (full file = input tokens) and limited by request size (32MB)
- Claude chooses the appropriate tool based on the user's request

### 1.5 Scope

| Capability | OpenAI (current) | Anthropic (this feature) |
|-----------|------------------|--------------------------|
| Text streaming | Native SSE | Native Anthropic SSE |
| Web search | Native tool | Native `web_search_20260209` (server-side). Web citations **not implemented** (no `source: "web"` citations) |
| Code execution | Native `code_interpreter` | Native `code_execution_20250825` (server-side) |
| Function calling | Native | Native `tool_use` (Anthropic format) |
| File search / RAG | Native `file_search` (server-side) | Planned: custom `search_files` tool loop via Azure vector store. **Not implemented** — `FileSearch` is dropped |
| Full file access | Native (file_id in request) | Planned: custom `load_files` tool → Files API → `document`/`image`/`container_upload`. **Not implemented** |
| Images | file_id in request (auto-included) | Implemented: auto-included `image` block with the Anthropic `secondary_file_id` |
| Code execution + files | file_id in request | Planned: `load_files` → `container_upload` in sandbox. **Not implemented** |

---

## 2. Architecture

### 2.1 Provider Adapter

New `AnthropicMessagesProvider` implementing the existing `LlmProvider` trait, following the same pattern as `OpenAiResponsesProvider`.

**Key decision: All tool loops live inside the adapter**, not in `StreamService`. *Current code differs:* the adapter has no tool loop; the `search_knowledge` function-tool loop runs in `StreamService` (`provider_task.rs`).

Rationale:
- `LlmProvider::stream()` returns `ProviderStream` yielding `ClientSseEvent` items. Tool loops are an implementation detail of the Anthropic adapter.
- From `StreamService`'s perspective, Anthropic tools look identical to OpenAI's native tools: `Tool { Start }`, `Tool { Done }`, then text deltas.
- Zero changes to `StreamService`, `ProviderStream`, `LlmProvider` trait, or existing providers.

### 2.2 Turn Architecture Compatibility

The tool loop is **completely transparent** to the turn layer.

- 1 turn = 1 user message + 1 streaming task → 1 `ProviderStream` → 1 finalization
- `ProviderStream` abstracts internal LLM calls — turn layer sees a single stream of `ClientSseEvent` items
- `accumulated_text` collects deltas from ALL internal calls
- `Usage` is summed across all calls (see §6.1)
- Finalization receives one `accumulated_text`, one `Usage`, one `response_id`

**Edit/Replay works unchanged:**
- Edit: soft-delete old turn + user message → new turn → new `run_stream()` → adapter re-executes tool loop from scratch
- Retry: same flow, reuses original user content
- Snapshot boundary ensures deterministic context (same attachments, same vector store)

**Vector store search call tracking** (not implemented for Anthropic, no `search_files` tool):
- Adapter emits `ClientSseEvent::Tool { phase: Start/Done, name: "file_search" }` for each search
- `StreamService` counts `Done` events → `file_search_completed_count` (same pattern as `web_search_completed_count`)
- Passed to `FinalizationInput` → `UsageEvent.file_search_calls`

### 2.3 Two Platform Support

| Platform | Base URL | Auth Header | API Path |
|----------|----------|-------------|----------|
| Anthropic Platform | `api.anthropic.com` | `x-api-key: {key}` | `/v1/messages` |
| Microsoft Foundry | `{resource}.services.ai.azure.com` | `api-key: {key}` or Entra ID Bearer | `/anthropic/v1/messages` |

Same adapter, different YAML config entries. OAGW handles routing via upstream aliases.

**Microsoft Foundry specifics:**
- Supported regions: East US2, Sweden Central
- Subscriptions: Enterprise and MCA-E only
- Known SSE bug: occasional concatenated events without proper `\n\n` delimiter

### 2.4 Anthropic Messages API Format

#### Key differences from OpenAI

| Aspect | OpenAI | Anthropic |
|--------|--------|-----------|
| System prompt | Message with `role: "system"` | Top-level `system` field (content block array) |
| Message content | String or array | Always content block array: `[{ type: "text", text }]` |
| Tool calls | Separate `tool_calls` array on assistant message | Content blocks with `type: "tool_use"` in `content` array |
| Tool results | Message with `role: "tool"` | Content blocks with `type: "tool_result"` in user message |
| Stop reasons | `stop`, `length`, `tool_calls`, `content_filter` | `end_turn`, `tool_use`, `max_tokens`, `stop_sequence` |
| Usage fields | `prompt_tokens`, `completion_tokens` | `input_tokens`, `output_tokens` |
| Streaming events | `data: {chunk}` + `data: [DONE]` | Named events: `message_start`, `content_block_delta`, etc. |
| Max tokens | Optional | **Required** (`max_tokens` field) |

#### SSE Event Flow

```
event: message_start       → Skip (capture message ID, input_tokens)
event: content_block_start → Start tracking block (text/tool_use/server_tool)
event: content_block_delta → Delta { text } or accumulate tool input JSON
event: content_block_stop  → Finalize block
event: message_delta       → Capture stop_reason + output token usage
event: message_stop        → Terminal(Completed/Incomplete) or enter tool loop
```

#### Event Translation to `TranslatedEvent`

| Anthropic Event | Condition | → TranslatedEvent |
|----------------|-----------|-------------------|
| `message_start` | always | `Skip` (capture message ID, input_tokens) |
| `content_block_start` | `type: "text"` | `Skip` (start text block tracking) |
| `content_block_start` | `type: "tool_use"` | `Skip` (start accumulating tool input JSON) |
| `content_block_start` | `type: "server_tool_use"`, name starts with `web_search` | `Sse(Tool { Start, "web_search" })` |
| `content_block_start` | `type: "server_tool_use"`, name starts with `code_execution` | `Sse(Tool { Start, "code_interpreter" })` (shared name, so the code interpreter limit and counters apply) |
| `content_block_delta` | `type: "text_delta"` | `Sse(Delta { "text", content })` |
| `content_block_delta` | `type: "input_json_delta"` | `Skip` (append to accumulated tool input) |
| `content_block_stop` | after web_search block | `Sse(Tool { Done, "web_search" })` |
| `content_block_stop` | after code_execution block | `Sse(Tool { Done, "code_interpreter" })` |
| `content_block_stop` | after text/tool_use block | `Skip` |
| `message_delta` | `stop_reason: "end_turn"` | `Skip` (prepare Completed) |
| `message_delta` | `stop_reason: "max_tokens"` | `Skip` (prepare Incomplete) |
| `message_delta` | `stop_reason: "tool_use"` | `Skip` (trigger tool loop) |
| `message_stop` | after `end_turn` | `Terminal(Completed)` |
| `message_stop` | after `max_tokens` | `Terminal(Incomplete)` |
| `message_stop` | after `tool_use` | **Do not emit Terminal** — enter tool loop |
| `error` | always | `Terminal(Failed)` |
| `ping` | always | `Skip` |

---

## 3. Custom Tool: `search_files`

> **Not implemented.** The Anthropic adapter drops `LlmTool::FileSearch` (`infra/llm/providers/anthropic_messages.rs`); `search_files`, `load_files` and `vector_store_search.rs` do not exist. See [ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md). The section below is the original design.

### 3.1 Purpose

Semantic search over uploaded documents via Azure/OpenAI vector store. Used when Claude needs to find specific information across files (vs reading the whole file).

**Why custom tool:** Anthropic has no native `file_search`, no vector store API, no embedding API.

### 3.2 Tool Definition

Configurable via provider config (see §5.4). Default:

```json
{
  "name": "search_files",
  "description": "Search the user's uploaded files for relevant information. Call this tool when the user asks about specific content in their uploaded documents.",
  "input_schema": {
    "type": "object",
    "properties": {
      "query": {
        "type": "string",
        "description": "A natural language search query describing the information to find."
      }
    },
    "required": ["query"]
  }
}
```

### 3.3 Tool Loop Flow

```
Claude → search_files({ query: "quarterly revenue" })
  ↓ stop_reason: "tool_use"
Adapter:
  ├─ Emit Tool { Start, "file_search" }
  ├─ POST /v1/vector_stores/{vs_id}/search { query, max_num_results, filters }
  │   → Azure/OpenAI returns scored chunks
  ├─ Emit Tool { Done, "file_search" }
  ├─ Build continuation request with tool_result (formatted chunks)
  └─ Send second request to Claude → stream final response
```

### 3.4 VectorStoreSearchClient

**New file:** `infra/llm/providers/vector_store_search.rs`

URI by `StorageKind`:
- OpenAI: `/{alias}/v1/vector_stores/{vs_id}/search`
- Azure: `/{alias}/openai/vector_stores/{vs_id}/search?api-version={ver}`

Reuses `RagHttpClient::json_post()`.

### 3.5 Search Result Formatting

```
[Source: report.pdf (relevance: 0.95)]
Q3 revenue was $12.5M, up 15% YoY...

[Source: summary.docx (relevance: 0.82)]
Annual revenue projections show...
```

---

## 4. Custom Tool: `load_files`

> **Not implemented.** The Anthropic adapter drops `LlmTool::FileSearch` (`infra/llm/providers/anthropic_messages.rs`); `search_files`, `load_files` and `vector_store_search.rs` do not exist. See [ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md). The section below is the original design.

### 4.1 Purpose

Load files (documents and images) into the conversation so Claude can see their content or process them with code execution. All file types — documents, images, spreadsheets — go through this single tool.

**Why a tool (not auto-include):**
- File content costs input tokens (full file tokenized by Anthropic). Auto-including all files on every request would be expensive.
- Claude decides **when** files are needed based on the user's request. "hello" → no files loaded. "summarize the attached file" → Claude calls `load_files`.

**Why `load_files` with Files API content blocks:**
- `document`/`image`/`container_upload` are native Anthropic content types — Claude processes them natively
- `container_upload` gives code execution sandbox access
- No manual text/binary encoding in tool_result needed

### 4.2 Tool Definition

```json
{
  "name": "load_files",
  "description": "Load files to see their content or process with code.",
  "input_schema": {
    "type": "object",
    "properties": {
      "filenames": {
        "type": "array",
        "items": {
          "type": "string",
          "enum": ["report.pdf", "data.csv", "photo.png"]
        },
        "description": "Names of files to load."
      }
    },
    "required": ["filenames"]
  }
}
```

**Dynamic enum:** The `filenames` enum is built at request time from the chat's attachments (all kinds — documents and images). If duplicate filenames exist, a display suffix is added: `report.pdf`, `report (2).pdf`. This deduplication is only in the tool definition — filenames in the DB and storage are unchanged.

**In-memory filename mapping:** The adapter loads all chat attachments at the start of the request and builds a `HashMap<String, Attachment>` mapping display names to attachment records. When Claude returns `filenames: ["report (2).pdf"]`, the adapter resolves the display name to the attachment via this map and reads `secondary_file_id` directly — no additional DB query needed. The map is transient and lives only for the duration of the request.

**No `mode` parameter.** The adapter determines the content block type automatically from attachment metadata:

| `attachment_kind` | `for_code_interpreter` | code_execution tool in request | → Content block |
|---|---|---|---|
| `Image` | — | — | `image` |
| `Document` | `true` | yes | `container_upload` |
| `Document` | any | no / `false` | `document` |

### 4.3 Tool Loop Flow

```
Claude → load_files({ filenames: ["report.pdf", "data.csv", "photo.png"] })
  ↓ stop_reason: "tool_use"
Adapter:
  ├─ Emit Tool { Start, "file_load" }
  ├─ Resolve filenames → attachments via filename map
  ├─ For each attachment: look up secondary_file_id (already set at upload time)
  ├─ Emit Tool { Done, "file_load" }
  ├─ Build continuation request with:
  │   ├─ tool_result { content: "Files loaded: report.pdf, data.csv, photo.png" }
  │   ├─ document block for report.pdf
  │   ├─ container_upload block for data.csv (for_code_interpreter + code_execution in tools)
  │   └─ image block for photo.png
  └─ Send second request → Claude sees all files → stream response
```

### 4.4 Continuation Request with File Blocks

```json
{
  "messages": [
    {
      "role": "assistant",
      "content": [
        { "type": "tool_use", "id": "toolu_abc", "name": "load_files",
          "input": { "filenames": ["report.pdf", "data.csv", "photo.png"] } }
      ]
    },
    {
      "role": "user",
      "content": [
        { "type": "tool_result", "tool_use_id": "toolu_abc",
          "content": "Files loaded: report.pdf, data.csv, photo.png" },
        { "type": "document", "source": { "type": "file", "file_id": "file_011C..." },
          "title": "report.pdf" },
        { "type": "container_upload", "file_id": "file_022D..." },
        { "type": "image", "source": { "type": "file", "file_id": "file_033E..." } }
      ]
    }
  ]
}
```

### 4.5 Beta Header

All requests using Files API require: `anthropic-beta: files-api-2025-04-14`

---

## 6. Multi-Turn Tool Loop Mechanics

> **Not implemented in the adapter.** The adapter has no internal tool loop ([ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md)). Prompt caching (§6.4) is not implemented. The rest of this section is the original design.

### 6.1 Usage Accumulation

Each Anthropic API response includes its own `usage`. The adapter **sums ALL fields** across tool loop iterations:

| Field | Rule | Rationale |
|-------|------|-----------|
| `input_tokens` | **Sum** | Anthropic bills per-request; user pays the same. Matches OpenAI parity (retrieved chunks included in input_tokens). |
| `output_tokens` | **Sum** | Each iteration generates new output |
| `cache_read_input_tokens` | **Sum** | Total cache hits. **Not included** in `input_tokens` — separate field. |
| `cache_creation_input_tokens` | **Sum** | Total cache writes. **Not included** in `input_tokens` — separate field. |

**Example:**

| Request | input_tokens | output_tokens |
|---------|-------------|---------------|
| 1st (→ tool_use) | 500 | 50 |
| 2nd (with tool_result/file) | 800 | 200 |
| **Reported to finalization** | **1300** | **250** |

**Cache tokens and credits:** In Anthropic's API, `cache_read_input_tokens` and `cache_creation_input_tokens` are **separate from** `input_tokens`. This differs from OpenAI, where cached tokens are included in `input_tokens`. Total actual input = `input_tokens` + `cache_read_input_tokens` + `cache_creation_input_tokens`.

The credits formula (`credits_micro_checked`) uses only `input_tokens` and `output_tokens`. For OpenAI this is correct — cached tokens are already included. For Anthropic, the adapter must **normalize** before passing to finalization: sum `input_tokens + cache_read_input_tokens + cache_creation_input_tokens` into the `input_tokens` field of `Usage`. This ensures the credits formula sees the same total regardless of provider. The raw cache breakdown is preserved separately for observability (see §6.1 table).

Anthropic bills cache_read at ~0.1x and cache_creation at ~1.25x of normal input price — applying these differential rates to credits is a separate concern (see open question #2).

### 6.2 State Machine

```rust
enum ToolLoopPhase { Streaming, ExecutingTool, Continuing, Done }
```

```
Streaming → (stop_reason: "end_turn") → Done [Terminal(Completed)]
Streaming → (stop_reason: "max_tokens") → Done [Terminal(Incomplete)]
Streaming → (stop_reason: "tool_use") → ExecutingTool
ExecutingTool → (tool executed) → Continuing
Continuing → Streaming [new SSE stream from continuation request]
```

The adapter does not send or apply `max_tool_calls`. The only implemented loop cap is the `search_knowledge` loop in `provider_task.rs` (`knowledge_search.max_calls_per_message + 2` iterations, then `agentic_iterations_exceeded`); see section 12.

### 6.3 Error Handling

| Scenario | Behavior |
|----------|----------|
| Vector store search fails | `tool_result { is_error: true }`, Claude continues without context |
| File download/upload fails | `tool_result { is_error: true }`, Claude continues without file |
| Continuation request fails | `Terminal(Failed)` with accumulated partial content and accumulated usage (see below) |
| Cancellation during tool execution | Check `cancel.is_cancelled()`, stop yielding events — CAS finalizer handles as `Cancelled` → billing ABORTED |
| Max iterations reached | Force `tool_result` with error, Claude responds with available context |

**Partial tool loop failure — usage for settlement:** When a continuation request fails at iteration N, the adapter has accumulated usage from iterations 1..N-1 (and possibly partial usage from iteration N's `message_start`). This accumulated usage constitutes "actual provider usage" for settlement purposes — `settlement_method="actual"` with the summed tokens. The adapter MUST pass the accumulated `Usage` to finalization regardless of which iteration failed. Do not fall back to the estimated formula or discard completed iterations' usage.

### 6.4 Prompt Caching

Add `cache_control: { type: "ephemeral" }` to system prompt and tool definitions. On continuation, cached content costs ~10% of normal input price. Only new content (tool_use + tool_result + file) incurs full cost.

### 6.5 Quota Reserve and Tool Loop Overshoot

**Known limitation:** Quota reserve is computed once at preflight (§5.4) from `ContextPlan + max_output_tokens`. Each tool loop iteration re-sends the full conversation context plus new content (tool_result, file blocks). With N iterations, cumulative `input_tokens` can significantly exceed the single-call reserve estimate.

**Comparison with OpenAI:** For OpenAI, `file_search` is server-side — one API call, chunks baked into `input_tokens`. For Anthropic, N custom tool iterations means N API calls with context re-send.

**Mitigating factors:**

1. **Prompt caching (§6.4)** — on iterations 2+, the bulk of re-sent context hits the cache (~10% cost). Financial overshoot is much smaller than raw token count suggests.
2. **Typical depth is 1–2 iterations** — `search_files` → response or `load_files` → response. Chains hitting `max_tool_calls` are rare.
3. **Overshoot tolerance (§5.8.1)** — the main design already handles reserve overruns; completed turns are not retroactively cancelled.

**Operator tuning:** `tool_surcharge_tokens` (§5.5.6) can be configured per-model to account for multi-iteration overhead. Operators deploying Anthropic models should set a higher `tool_surcharge_tokens` than for OpenAI to absorb the expected overshoot.

**P2: precise reserve formula.** A more accurate reserve accounting for `max_tool_calls` multiplier and cache hit rates is deferred to P2. The current approach (single-call reserve + overshoot tolerance + operator tuning) is sufficient for P1.

---

## 7. Configuration

### 7.1 Anthropic Platform

```yaml
providers:
  anthropic:
    kind: anthropic_messages
    host: "api.anthropic.com"
    api_path: "/v1/messages"
    storage_kind: openai        # required on every entry; registers the /v1/files routes
    rag_provider: "azure_openai"
    auth_plugin_type: "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1"
    auth_config:
      header: "x-api-key"
      prefix: ""
      secret_ref: "cred://anthropic-key"
```

`storage_kind` has no default: an entry without it fails to deserialize. On an `anthropic_messages` entry it does not route RAG storage (that is `rag_provider`), but it decides which RAG routes OAGW provisioning registers on the entry's upstream: `storage_kind: openai` registers `POST /v1/files` and `DELETE /v1/files/{file_id}` (plus the `POST`, `DELETE` and `GET` `/v1/vector_stores` routes). `AnthropicFilesClient` sends the image copy to `/{upstream_alias}/v1/files` on the Anthropic entry's upstream, so without these routes OAGW returns 404 and the attachment gets `secondary_status = failed`. `storage_kind: azure` would register `/openai/...` routes instead and would also require `api_version`.

### 7.2 Microsoft Foundry

```yaml
providers:
  azure_anthropic:
    kind: anthropic_messages
    host: "${AZURE_FOUNDRY_HOST}"
    api_path: "/anthropic/v1/messages"
    storage_kind: openai        # required; see 7.1
    rag_provider: "azure_openai"
    auth_plugin_type: "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1"
    auth_config:
      header: "api-key"
      prefix: ""
      secret_ref: "cred://azure-foundry-key"
```

### 7.3 Storage Backend (existing, shared)

```yaml
providers:
  azure_openai:
    kind: openai_responses
    host: "myinstance.openai.azure.com"
    storage_kind: azure
    api_version: "2025-03-01-preview"
    auth_plugin_type: "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1"
    auth_config:
      header: "api-key"
      prefix: ""
      secret_ref: "cred://azure-openai-key"
```

### 7.4 Custom Tool Configuration

> **Not implemented.** `search_files_tool` and `load_files_tool` are not configuration keys; `ProviderEntry` (the `providers.<id>` entry) uses `deny_unknown_fields`, so these keys are rejected. See [ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md).

```yaml
providers:
  anthropic:
    # ...
    search_files_tool:
      name: "search_files"
      description: "Search the user's uploaded files for relevant information."
      query_description: "A natural language search query."
      max_num_results: 10
      score_threshold: 0.5

    load_files_tool:
      name: "load_files"
      description: "Load files to see their content or process with code."
```

**`search_files_tool` parameters:**
- `name` — tool name exposed to Claude (default: `search_files`)
- `description` — tool description guiding when Claude calls it
- `query_description` — description of the `query` parameter
- `max_num_results` — max chunks returned from vector search (default: 10)
- `score_threshold` — minimum relevance score to include (default: 0.5, range 0.0–1.0)

**`load_files_tool` parameters:**
- `name` — tool name exposed to Claude (default: `load_files`)
- `description` — tool description guiding when Claude calls it

**Note:** The `filenames` enum is built dynamically by the adapter at request time from the chat's attachments (documents + images). Content block type (`document` / `image` / `container_upload`) is determined automatically by the adapter from attachment metadata — Claude does not choose it.

### 7.5 RAG Provider Separation

**Problem:** Currently `provider_id` from `ResolvedModel` is used for both LLM calls and storage operations (file upload, vector store). For OpenAI providers this works — one provider does everything. For Anthropic it breaks: `provider_id: "anthropic"` has no file storage or vector store API.

**Current flow (OpenAI):**
```
resolve_model() → provider_id: "azure_openai"
  → LLM: proxy to azure_openai upstream ✓
  → File upload: DispatchingFileStorage routes by "azure_openai" ✓
  → Vector store: DispatchingVectorStore routes by "azure_openai" ✓
  → storage_backend label: resolve_storage_backend("azure_openai") → "azure" ✓
```

**Problem with Anthropic:**
```
resolve_model() → provider_id: "anthropic"
  → LLM: proxy to anthropic upstream ✓
  → File upload: DispatchingFileStorage routes by "anthropic" ✗ (no file storage)
  → Vector store: DispatchingVectorStore routes by "anthropic" ✗ (no vector store)
```

**Solution (implemented):** New field `rag_provider: Option<String>` on `ProviderEntry`. When set, storage operations (file upload, vector store, cleanup) use this provider instead of the LLM provider.

```rust
// config.rs — new field on ProviderEntry
/// Provider ID for RAG operations (file storage and vector store).
/// When set, file upload / vector store / search / cleanup use this
/// provider's OAGW upstream and auth instead of the LLM provider's.
/// Required for providers that don't offer file storage (e.g., Anthropic).
#[serde(default)]
pub rag_provider: Option<String>,
```

**Updated flow (Anthropic):**
```
resolve_model() → provider_id: "anthropic"
  → config: rag_provider = Some("azure_openai")
  → LLM: proxy to "anthropic" upstream (Anthropic Messages API)
  → File upload: DispatchingFileStorage routes by "azure_openai" ✓
  → Vector store: DispatchingVectorStore routes by "azure_openai" ✓
  → storage_backend label: resolve_storage_backend("azure_openai") → "azure" ✓
  → Anthropic Files API: adapter uses "anthropic" upstream directly
```

**Changes required:**
1. `ProviderEntry` — add `rag_provider: Option<String>` field
2. `ProviderResolver` — add `resolve_rag_provider(provider_id) -> &str` method that returns `rag_provider` if set, otherwise `provider_id` itself (backward compatible)
3. `AttachmentService::resolve_model_limits()` — use `resolve_rag_provider()` instead of raw `provider_id` for storage operations
4. `DispatchingFileStorage` / `DispatchingVectorStore` — route by resolved storage provider
5. `resolve_storage_backend()` — resolve from storage provider entry, not LLM provider entry
6. Validation at startup — if `rag_provider` references a non-existent provider, fail fast

**Limitation — quota downgrade to another provider:** storage is resolved from the chat's model at upload time. After a quota downgrade the turn goes to the effective model's provider, but it still sends the vector store id and file ids of the chat model's storage provider, and the Anthropic file ids exist only if the chat model is on an `anthropic_messages` provider. A downgrade between models whose providers do not share storage does not see the attachments. See DESIGN "Provider after a downgrade".

---

## 8. File Upload Flow

### 8.0 Upload Strategy

**Model is immutable per chat** (`cpt-cf-mini-chat-constraint-model-locked-per-chat`). If the chat uses an Anthropic model, we know this at file upload time and can upload to Anthropic Files API immediately.

**Rule:** Files always go to Azure/OpenAI (file store + vector store). If the chat model is Anthropic, also upload to Anthropic Files API **in parallel**.

```
User uploads file (chat model = Anthropic)
    ↓
AttachmentService::upload_file()
    ├─ Upload to Azure/OpenAI Files API → provider_file_id          [EXISTING]
    ├─ Add to vector store (if document) → vector_store_id           [EXISTING]
    └─ Upload to Anthropic Files API → secondary_file_id             [images only, 30 s timeout]
```

**Implemented scope:** only images are uploaded to Anthropic, because their bytes are already buffered for thumbnail generation. Documents are skipped (`secondary_status` stays `not_attempted`); uploading them would need a re-download from Azure/OpenAI. Images larger than the thumbnail decode buffer are also skipped.

For OpenAI model chats, only Azure/OpenAI upload happens (existing behavior, unchanged).

**Why eager parallel upload:**
- **Model is immutable** (`cpt-cf-mini-chat-constraint-model-locked-per-chat`) — no risk of wasted uploads. If the chat is Anthropic, files will always be for Anthropic.
- **Zero latency at request time** — `secondary_file_id` is ready when the image block is built.
- **Azure/OpenAI as primary file store** — Anthropic Files API is still in beta. Azure/OpenAI is the stable backend. Anthropic file_id is a derived cache that can be re-created from Azure/OpenAI if needed.

**Failure policy:**

| Azure/OpenAI | Anthropic | Overall result |
|---|---|---|
| OK | OK | `status=ready`, `secondary_status=uploaded` — happy path |
| OK | Failed or timeout | `status=ready`, `secondary_status=failed` — the image is dropped from Anthropic requests |
| Failed | — | `status=failed` — whole upload failed. The Anthropic upload runs only after the primary upload succeeded; if the attachment then loses the CAS transition to `ready`, the Anthropic copy is deleted best-effort |

**Rule: Azure/OpenAI is the primary store.** If it fails, the entire upload fails regardless of Anthropic result.

When `secondary_status=failed`, the adapter drops the `image` block for that attachment (logged at `debug!`); the upload failure itself is logged at `warn!`. The user can re-upload. The planned `load_files` error path does not exist.

### 8.0.1 Streaming & Memory Considerations

**Parallel upload to Anthropic** reuses the same file bytes already buffered by `RagHttpClient::multipart_upload()` from the client HTTP request. The `Bytes` type uses reference counting, so `.clone()` is a cheap `Arc::clone` — no second full-buffer copy. Peak memory remains ~1x file size (collected once in `rag_http_client`), not 2x.

The oagw-sdk `Part::stream(name, BodyStream)` is fully implemented for streaming multipart upload if needed in the future. The comment in `rag_http_client.rs` (lines 38-40) about "OAGW chunked encoding issues" blocking `Part::stream` is outdated — streaming infrastructure is fully tested in `oagw-sdk/src/multipart.rs`.

### 8.1 Database Schema Changes

#### 8.1.1 `attachments` table — secondary-upload columns

The columns are provider-agnostic ("secondary upload"), so another provider can reuse them without a new migration. Today only `anthropic` is allowed.

```sql
-- Same DDL on PostgreSQL and SQLite
ALTER TABLE attachments ADD COLUMN secondary_file_id VARCHAR(128);
ALTER TABLE attachments ADD COLUMN secondary_status VARCHAR(16) NOT NULL DEFAULT 'not_attempted'
  CHECK (secondary_status IN ('not_attempted', 'pending', 'uploaded', 'failed'));
ALTER TABLE attachments ADD COLUMN secondary_provider_kind VARCHAR(32)
  CHECK (secondary_provider_kind IS NULL OR secondary_provider_kind IN ('anthropic'));
```

Entity fields (`infra/db/entity/attachment.rs`):

```rust
#[sea_orm(column_type = "String(StringLen::N(128))", nullable)]
pub secondary_file_id: Option<String>,

#[sea_orm(column_type = "String(StringLen::N(16))")]
pub secondary_status: SecondaryUploadStatus,

#[sea_orm(column_type = "String(StringLen::N(32))", nullable)]
pub secondary_provider_kind: Option<String>,
```

```rust
/// Lifecycle: not_attempted → pending → uploaded | failed.
#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum SecondaryUploadStatus {
    #[sea_orm(string_value = "not_attempted")]
    NotAttempted,
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "uploaded")]
    Uploaded,
    #[sea_orm(string_value = "failed")]
    Failed,
}
```

`secondary_provider_kind` values are string constants in `attachment::secondary_provider_kind` (`ANTHROPIC = "anthropic"`), not a sea-orm enum, because the column is nullable.

**Field semantics:**

| `secondary_status` | `secondary_file_id` | `secondary_provider_kind` | Meaning |
|---|---|---|---|
| `not_attempted` | `NULL` | `NULL` | Non-Anthropic chat, a document (documents are not uploaded to Anthropic), or a pre-migration row |
| `pending` | `NULL` | `anthropic` | Upload in progress (or the server stopped mid-upload) |
| `uploaded` | `"file_011C..."` | `anthropic` | The Anthropic copy exists; the image is sent as an `image` block |
| `failed` | `NULL` | `anthropic` | Upload failed or timed out (30 s); the image is dropped from Anthropic requests |

- No index — lookups are by primary key or by `(chat_id, tenant_id)`.
- `not_attempted` is the default, safe for existing rows.
- `provider_file_id` still holds the primary (Azure/OpenAI) file id.

#### 8.1.2 `chat_vector_stores` table — no changes

The vector store table is not affected. Vector stores remain in Azure/OpenAI and are referenced by the existing `vector_store_id` field. Documents are indexed there for Anthropic chats too, but nothing searches them (`search_files` is not implemented, see [ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md)).

#### 8.1.3 Migration

`infra/db/migrations/m20260417_000004_add_secondary_upload_fields.rs`. `up()` runs the DDL above on both backends. `down()` drops the three columns; on SQLite a `DROP COLUMN` failure (SQLite < 3.35.0) is logged and ignored, on PostgreSQL it is returned as an error.

#### 8.1.4 Cleanup

Implemented. When an attachment with `secondary_status = uploaded` is deleted, `AttachmentService` resolves the Anthropic upstream alias at enqueue time and puts a `SecondaryCleanupRef` into the attachment-cleanup outbox payload. `AttachmentCleanupHandler` (`infra/workers/cleanup_worker.rs`) calls `DELETE /v1/files/{id}` through `AnthropicFilesClient` after the primary file is deleted. Chat deletion does the same per attachment using `secondary_upstream_alias` in the chat-cleanup payload. If no Anthropic client is configured, the delete is skipped and `secondary_cleanup_skipped` is incremented.

---

## 9. Billing & Usage

### 9.1 Token Billing

**Not implemented.** The adapter has no tool loop (§6.1), so there are no adapter iterations to sum. In the `search_knowledge` loop in `provider_task.rs`, `TerminalOutcome::ToolUse` carries no usage, so only the usage of the final provider response is settled. Credit calculation unchanged — same formula with per-model multipliers.

### 9.2 Tool Call Tracking

**Implemented.** `UsageEvent` carries `file_search_calls: u32` (`mini-chat-sdk/src/models.rs`), reported from `chat_turns.file_search_completed_count`. The counter is incremented for provider-native `file_search` tool `done` events and for successful `search_knowledge` retrievals (the two tools are never sent in the same request). The Anthropic adapter sends no `file_search`, so on Anthropic chats the count comes from `search_knowledge` only.

### 9.3 Cost Considerations

| Component | Native file_search (OpenAI) | search_files tool loop (Anthropic) |
|-----------|---------------------------|-----------------------------------|
| Vector store search | ~$0.008/query | ~$0.008/query (same API) |
| LLM tokens | Single request | Two requests (context re-sent) |

| Component | Native file access (OpenAI) | load_files tool loop (Anthropic) |
|-----------|---------------------------|-----------------------------------|
| File read | file_id in request (free) | Files API upload + document block |
| LLM tokens | File tokens in input | File tokens in input (same) |

Prompt caching mitigates tool loop re-send cost (~10% for cached content).

---

## 10. Implementation Phases

### Phase 1: Core Adapter
- `ProviderKind::AnthropicMessages` + factory
- `anthropic_messages.rs`: SSE parsing, event translation, request building
- Both `stream()` and `complete()` implementations (`complete()` used for thread summaries; document summaries do not exist, [ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md))
- Native tools: `web_search_20260209`, `code_execution_20250825`
- `anthropic-version` header, `rag_provider` config field
- OAGW provisioning verification

### Phase 2: Files API + Parallel Upload
- `AnthropicFilesClient`: upload to Anthropic Files API
- Parallel upload in `AttachmentService`: Azure/OpenAI + Anthropic for Anthropic model chats
- `secondary_file_id` / `secondary_status` / `secondary_provider_kind` on the attachment entity + migration `m20260417_000004_add_secondary_upload_fields`
- Beta header handling

### Phase 3: search_files Tool Loop — not implemented ([ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md))
- `VectorStoreSearchClient` (reuses `RagHttpClient`)
- Tool loop state machine in adapter
- Configurable tool description/params
- `LlmRequest` storage context fields

### Phase 4: load_files Tool — not implemented ([ADR-0007](../ADR/0007-cpt-cf-mini-chat-adr-document-retrieval-scope.md))
- Tool definition with dynamic `filenames` enum from chat attachments
- Content block type resolution from attachment metadata (document/image/container_upload)
- Inject content blocks in continuation request
- Integration with code_execution

### Phase 5: SDK & Billing
- `file_search_calls` in `UsageEvent`
- Tracking in `stream_service` + finalization pass-through

### Phase 6: Testing — partly implemented
- Unit: request building, event translation, image handling — implemented (tests in `anthropic_messages.rs`). Tool-loop unit tests — not implemented (there is no adapter tool loop).
- Tool loops: search_files, load_files (mock OAGW + Files API) — not implemented
- Integration: provider resolver, config deserialization — covered by the `provider_resolver.rs` and `config.rs` tests

### Phase 7: Observability — partly implemented
- Metrics: per-provider labels exist (`provider` label on stream metrics). `file_search_tool_loop_latency_ms` and `load_files_latency_ms` — not implemented
- Logging: tool loop iterations at `debug!` (query, result count, latency) — not implemented (no adapter tool loop)
- Error sanitization: Anthropic `msg_` response IDs — implemented (`infra/llm/mod.rs` redacts `msg_` IDs)

---

## 11. File Change Summary

| File | Change | Description |
|------|--------|-------------|
| `infra/llm/providers/mod.rs` | Modify | Add `AnthropicMessages` variant, factory |
| `infra/llm/providers/anthropic_messages.rs` | **New** | Core adapter (no internal tool loop; `FileSearch` dropped) |
| `infra/llm/providers/vector_store_search.rs` | — | **Not implemented** (planned vector store search client) |
| `infra/llm/providers/anthropic_files_client.rs` | **New** | Upload to and delete from Anthropic Files API |
| `domain/service/attachment_service.rs` | Modify | Parallel upload to Anthropic for Anthropic model chats (images only) |
| `infra/llm/request.rs` | Modify | Storage context fields + builder methods |
| `infra/llm/provider_resolver.rs` | Modify | Handle AnthropicMessages, storage provider resolution |
| `config.rs` | Modify | `rag_provider` field |
| `infra/db/entity/attachment.rs` | Modify | `secondary_file_id`, `secondary_status`, `secondary_provider_kind` fields, `SecondaryUploadStatus` enum |
| `infra/db/migrations/m20260417_000004_add_secondary_upload_fields.rs` | **New** | Add the three `secondary_*` columns |
| `infra/workers/cleanup_worker.rs` | Modify | Delete the Anthropic copy on attachment and chat cleanup |
| `mini-chat-sdk/src/models.rs` | Modify | `file_search_calls` in `UsageEvent` |
| `domain/model/finalization.rs` | Modify | `file_search_calls` field |
| `domain/service/finalization_service.rs` | Modify | Pass `file_search_calls` |
| `domain/service/stream_service.rs` | Modify | Track file_search calls |

---

## 12. Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|-----------|
| Tool loop stream complexity | High dev effort | `async_stream::stream!` macro for generator-style impl |
| OAGW stripping `anthropic-version` header | Requests rejected | Set as an upstream header rule (`set`) when the gear provisions the OAGW upstream (`anthropic_messages.rs::upstream_headers`, applied in `oagw_provisioning.rs`), not per request |
| Microsoft Foundry SSE bug (concatenated events) | Parse failures | Resilient SSE parser handling missing `\n\n` |
| Tool loop infinite iteration | Runaway costs | The `search_knowledge` loop in `provider_task.rs` is hard-capped at `knowledge_search.max_calls_per_message + 2` iterations (then `agentic_iterations_exceeded`); `max_tool_calls` is not sent by the Anthropic adapter |
| Tool execution failure mid-loop | Lost context | Graceful degradation via error `tool_result` |
| Storage auth ≠ LLM auth | Search/upload fails | `rag_provider` config separation |
| Files API beta instability | Breaking changes | Azure/OpenAI is primary store; Anthropic Files API is a re-populatable cache |
| Claude not calling tools appropriately | Wrong tool for task | Configurable tool descriptions + system prompt guards |
| Microsoft Foundry Files API availability | `load_files` broken on Microsoft Foundry | [Official docs](https://learn.microsoft.com/en-us/azure/foundry/foundry-models/how-to/use-foundry-models-claude?tabs=python#call-the-claude-messages-api) confirm Files API is supported. Monitor for beta changes. |

---

## 13. Open Questions

1. **Extended Thinking** — Parse and ignore for now, or expose to users? Thinking tokens are billed as output tokens by Anthropic — the billing path must handle them from day one regardless of UI exposure decision. Ensure `output_tokens` in `Usage` includes thinking tokens.
2. **Prompt caching differential billing** — Anthropic bills cache_read at ~0.1x and cache_creation at ~1.25x of normal input price. The adapter normalizes cache tokens into `input_tokens` for credits (see §6.1), so total token count is correct. But credits are charged at the flat `input_mult` rate — no discount for cache hits, no surcharge for cache writes. If we want accurate cost attribution, the credits formula needs cache-specific multipliers. Not blocking for Phase 1.
3. **Rate limiting** — Anthropic 429 handling: OAGW retry sufficient or adapter-level backoff?
4. **Microsoft Foundry SSE bug** — Resilient parser or fail fast?
5. **Files API stability** — Beta on both platforms. How to handle breaking changes?
6. **Model catalog** — How to configure Claude models in policy catalog (provider_id, multipliers, tool support flags)?
7. **Storage context plumbing for tool loops** — The adapter needs vector_store_ids, attachment metadata, and OAGW gateway reference for `search_files` / `load_files` tool execution. `LlmRequest` currently has no storage-related fields. Options: (a) add storage context fields to `LlmRequest` + builder, (b) pass a separate `ToolLoopContext` alongside `LlmRequest` in `LlmProvider::stream()`, (c) inject storage context at adapter construction time (adapter becomes stateful per-request). Needs design decision before Phase 3.
8. **Lazy re-upload on `load_files`** — Currently, when `secondary_status=failed`, the image is not sent to Claude (user must re-upload). A future improvement: at request time, if `secondary_status=failed`, download from Azure/OpenAI store and re-upload to Anthropic Files API. On success, update `secondary_status=uploaded` + set `secondary_file_id`. Trade-off: adds latency to the tool loop (download + upload mid-stream) and complexity, but improves resilience against transient Anthropic Files API failures. Would also be required if model-switching per chat is added in the future (decision #4 currently prohibits this).
9. **`secondary_*` column scalability** — The columns hold one secondary copy per attachment, discriminated by `secondary_provider_kind`. If an attachment ever needs copies at more than one secondary provider, refactor to an `attachment_provider_files` join table with `(attachment_id, provider, file_id, status)`.

---

## 14. Verification

1. `cargo test -p cf-gears-mini-chat` — all new tests pass
2. `make dev-clippy && make dev-fmt`
3. **E2E Anthropic Platform:** text streaming → web_search → code_execution → image upload and image block (`search_files` / `load_files` not implemented)
4. **E2E Microsoft Foundry:** same flow (Files API beta)
5. **Billing:** verify `UsageEvent` token counts and `file_search_calls` across tool loop iterations
