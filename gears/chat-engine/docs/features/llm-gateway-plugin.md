Created:  2026-03-20 by Constructor Tech
Updated:  2026-03-20 by Constructor Tech
# Feature: LLM Gateway Plugin


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Register Schemas at Startup](#register-schemas-at-startup)
  - [On Session Type Configured](#on-session-type-configured)
  - [On Session Created](#on-session-created)
  - [On Session Updated](#on-session-updated)
  - [On Message](#on-message)
  - [On Message Recreate](#on-message-recreate)
  - [On Session Summary](#on-session-summary)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Resolve Capabilities from Model Registry](#resolve-capabilities-from-model-registry)
  - [Refresh Capabilities on Model Change](#refresh-capabilities-on-model-change)
  - [Forward Message to LLM Gateway](#forward-message-to-llm-gateway)
  - [Generate Summary](#generate-summary)
  - [Plugin Resilience](#plugin-resilience)
- [4. States (CDSL)](#4-states-cdsl)
  - [None](#none)
- [5. Definitions of Done](#5-definitions-of-done)
  - [GTS Schema Registration](#gts-schema-registration)
  - [Model Registry Capability Resolution](#model-registry-capability-resolution)
  - [LLM Gateway Message Forwarding](#llm-gateway-message-forwarding)
  - [Context Overflow Summarization](#context-overflow-summarization)
  - [Message Visibility Flags](#message-visibility-flags)
  - [Plugin-Owned Resilience](#plugin-owned-resilience)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-featstatus-llm-gateway-plugin`

## 1. Feature Context

- [ ] `p1` - `cpt-cf-chat-engine-feature-llm-gateway-plugin`

### 1.1 Overview

First concrete `ChatEngineBackendPlugin` implementation: integrates with Model Registry for capability resolution, forwards messages to LLM Gateway service with streaming response, handles context overflow via summarization flow, and registers GTS-derived schemas for LLM-specific configuration and metadata.

### 1.2 Purpose

Provide a production-ready LLM backend for Chat Engine that resolves model capabilities dynamically from Model Registry, streams LLM responses through the plugin trait interface, and manages context overflow through automatic summarization — all without modifying Chat Engine core.

Success criteria: LLM plugin registers GTS schemas at startup; capabilities are resolved from Model Registry at session creation; messages are forwarded to LLM Gateway with streaming response; context overflow triggers summarization and automatic retry.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-chat-engine-actor-backend-plugin` | Implements `ChatEngineBackendPlugin` trait; owns all communication with Model Registry and LLM Gateway |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADRs**: [ADR-0022](../ADR/0022-plugin-backend-integration.md), [ADR-0023](../ADR/0023-llm-gateway-plugin.md)
- **Decomposition Entry**: [2.10 LLM Gateway Plugin](../DECOMPOSITION.md#210-llm-gateway-plugin--high) (`cpt-cf-chat-engine-feature-llm-gateway-plugin`)
- **Dependencies**: `cpt-cf-chat-engine-feature-plugin-system`

**Traces to**:
- `cpt-cf-chat-engine-fr-send-message` — message forwarding to LLM Gateway
- `cpt-cf-chat-engine-fr-session-summary` — summary generation via plugin
- `cpt-cf-chat-engine-fr-context-overflow` — context overflow handling and summarization
- `cpt-cf-chat-engine-fr-schema-extensibility` — GTS schema registration for LLM-specific types
- `cpt-cf-chat-engine-fr-conversation-memory` — memory strategy applied during plugin invocation
- `cpt-cf-chat-engine-nfr-streaming` — streaming response performance
- `cpt-cf-chat-engine-nfr-backend-isolation` — plugin-owned resilience, no core retry logic
- `cpt-cf-chat-engine-nfr-availability` — circuit breaker and timeout for external services

## 2. Actor Flows (CDSL)

### Register Schemas at Startup

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-llm-gateway-plugin-register-schemas`

**Actor**: `cpt-cf-chat-engine-actor-backend-plugin`

**Success Scenarios**:
- Plugin registers all GTS schemas at startup; session types referencing this plugin can be created

**Error Scenarios**:
- GTS schema registry unavailable at startup (plugin fails to initialize)

**Steps**:
1. [ ] - `p1` - Register `LlmPluginConfig` schema (`gtx.cf.chat_engine.llm_gateway_plugin_config.v1~`) in GTS schema registry — validates `PluginConfig.config` for this plugin - `inst-reg-plugin-config`
2. [ ] - `p1` - Register `LlmSummarizationSettings` schema (`gtx.cf.chat_engine.llm_gateway.summarization_settings.v1~`) — nested in `LlmPluginConfig.summarization_settings` - `inst-reg-summarization`
3. [ ] - `p1` - Register `LlmMessageMetadata` schema (`gtx.cf.chat_engine.llm_gateway.message_metadata.v1~`) — validates `Message.metadata` for LLM responses - `inst-reg-message-metadata`
4. [ ] - `p1` - Register `LlmUsage` schema (`gtx.cf.chat_engine.llm_gateway.usage.v1~`) — nested in `LlmMessageMetadata.usage` - `inst-reg-usage`
5. [ ] - `p1` - Register entity extension schemas (`LlmMessage`, `LlmMessageGetResponse`, `LlmMessageNewResponse`, `LlmMessageRecreateResponse`, `LlmStreamingCompleteEvent`, `LlmMessageNewEvent`, `LlmSessionSummaryEvent`) — extend base Chat Engine schemas via JSON Schema `allOf` - `inst-reg-entity-schemas`
6. [ ] - `p1` - **RETURN** plugin initialized and ready to receive trait method calls - `inst-reg-return`

### On Session Type Configured

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-session-type-configured`

**Actor**: `cpt-cf-chat-engine-actor-backend-plugin`

**Success Scenarios**:
- Plugin validates `PluginConfig.config` against `LlmPluginConfig` schema; returns empty capabilities (deferred to session creation)

**Error Scenarios**:
- `PluginConfig.config` fails validation against `LlmPluginConfig` schema

**Steps**:
1. [ ] - `p1` - Receive `on_session_type_configured(ctx)` call from Chat Engine - `inst-stc-receive`
2. [ ] - `p1` - Validate `ctx.plugin_config.config` against `LlmPluginConfig` GTS schema - `inst-stc-validate`
3. [ ] - `p1` - **IF** validation fails **RETURN** error (invalid plugin configuration) - `inst-stc-invalid`
4. [ ] - `p1` - **RETURN** empty `Vec<Capability>` — capability resolution deferred to `on_session_created` - `inst-stc-return`

### On Session Created

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-session-created`

**Actor**: `cpt-cf-chat-engine-actor-backend-plugin`

**Success Scenarios**:
- Plugin queries Model Registry for available models and default model capabilities; returns `Vec<Capability>` to Chat Engine

**Error Scenarios**:
- Model Registry unavailable (returns error; Chat Engine returns 502 to client)
- Model Registry returns empty models list (returns error)

**Steps**:
1. [ ] - `p1` - Receive `on_session_created(ctx)` call from Chat Engine - `inst-sc-receive`
2. [ ] - `p1` - Algorithm: resolve capabilities from Model Registry using `cpt-cf-chat-engine-algo-llm-gateway-plugin-resolve-capabilities` - `inst-sc-resolve`
3. [ ] - `p1` - **RETURN** `Vec<Capability>` containing model selection and model-specific capabilities - `inst-sc-return`

### On Session Updated

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-session-updated`

**Actor**: `cpt-cf-chat-engine-actor-backend-plugin`

**Success Scenarios**:
- User changes model selection; plugin queries Model Registry for new model capabilities; returns refreshed `Vec<Capability>`

**Error Scenarios**:
- Model Registry unavailable on capability refresh (returns error; Chat Engine returns 502 to client)

**Steps**:
1. [ ] - `p1` - Receive `on_session_updated(ctx)` call from Chat Engine with updated `CapabilityValue[]` - `inst-su-receive`
2. [ ] - `p1` - Algorithm: refresh capabilities from Model Registry using `cpt-cf-chat-engine-algo-llm-gateway-plugin-refresh-capabilities` - `inst-su-refresh`
3. [ ] - `p1` - **RETURN** `Vec<Capability>` — Chat Engine overwrites `Session.enabled_capabilities` - `inst-su-return`

### On Message

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-message`

**Actor**: `cpt-cf-chat-engine-actor-backend-plugin`

**Success Scenarios**:
- Plugin forwards message and history to LLM Gateway; streams response chunks back through `ResponseStream`

**Error Scenarios**:
- LLM Gateway unavailable (returns error)
- LLM Gateway returns `context_overflow` (signals recoverable error to Chat Engine)
- LLM Gateway timeout (returns error)

**Steps**:
1. [ ] - `p1` - Receive `on_message(ctx, &mut stream)` call from Chat Engine with `messages: Message[]` and `CapabilityValue[]` - `inst-om-receive`
2. [ ] - `p1` - Algorithm: forward to LLM Gateway using `cpt-cf-chat-engine-algo-llm-gateway-plugin-forward-to-gateway` - `inst-om-forward`
3. [ ] - `p1` - **IF** LLM Gateway returns `context_overflow` error: emit `StreamingErrorEvent` whose `error` field is prefixed with the `context_overflow:` discriminator (e.g. `error: "context_overflow: <detail>"`) to `ResponseStream` - `inst-om-overflow`
4. [ ] - `p1` - **IF** LLM Gateway stream disconnects mid-response: emit `StreamingErrorEvent` whose `error` field is prefixed with the `stream_interrupted:` discriminator (e.g. `error: "stream_interrupted: <detail>"`) to client, persist partial response as failed message (`finish_reason: "error"`), log stream interruption details (session_id, message_id, bytes_received, disconnect_reason) - `inst-om-stream-disconnect`
5. [ ] - `p1` - **RETURN** stream closed - `inst-om-return`

### On Message Recreate

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-message-recreate`

**Actor**: `cpt-cf-chat-engine-actor-backend-plugin`

**Success Scenarios**:
- Plugin forwards recreate request with history to LLM Gateway; streams new response variant back

**Error Scenarios**:
- Same as On Message (LLM Gateway unavailable, context_overflow, timeout)

**Steps**:
1. [ ] - `p1` - Receive `on_message_recreate(ctx, &mut stream)` call from Chat Engine with `messages: Message[]` and `CapabilityValue[]` - `inst-omr-receive`
2. [ ] - `p1` - Algorithm: forward to LLM Gateway using `cpt-cf-chat-engine-algo-llm-gateway-plugin-forward-to-gateway` (same algorithm as on_message) - `inst-omr-forward`
3. [ ] - `p1` - **IF** LLM Gateway returns `context_overflow` error: emit `StreamingErrorEvent` whose `error` field is prefixed with the `context_overflow:` discriminator (e.g. `error: "context_overflow: <detail>"`) to `ResponseStream` - `inst-omr-overflow`
4. [ ] - `p1` - **IF** LLM Gateway stream disconnects mid-response: emit `StreamingErrorEvent` whose `error` field is prefixed with the `stream_interrupted:` discriminator (e.g. `error: "stream_interrupted: <detail>"`) to client, persist partial response as failed message (`finish_reason: "error"`), log stream interruption details (session_id, message_id, bytes_received, disconnect_reason) - `inst-omr-stream-disconnect`
5. [ ] - `p1` - **RETURN** stream closed - `inst-omr-return`

### On Session Summary

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-session-summary`

**Actor**: `cpt-cf-chat-engine-actor-backend-plugin`

**Success Scenarios**:
- Plugin receives full visible history; splits messages based on `summarization_settings`; generates summary via LLM Gateway; returns `SummaryResult`

**Error Scenarios**:
- `summarization_settings` is null in plugin config (returns error indicating summarization not supported)
- LLM Gateway unavailable during summary generation (returns error)

**Steps**:
1. [ ] - `p2` - Receive `on_session_summary(ctx, &mut stream)` call from Chat Engine with `messages: Message[]` (full visible history) - `inst-os-receive`
2. [ ] - `p2` - Algorithm: generate summary using `cpt-cf-chat-engine-algo-llm-gateway-plugin-generate-summary` - `inst-os-generate`
3. [ ] - `p2` - **RETURN** `SummaryResult` with `summary_text` and `summarized_message_ids` via `ResponseStream` - `inst-os-return`

## 3. Processes / Business Logic (CDSL)

### Resolve Capabilities from Model Registry

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-llm-gateway-plugin-resolve-capabilities`

**Input**: Plugin context with `plugin_config` containing `LlmPluginConfig`
**Output**: `Vec<Capability>` with model selection and model-specific capabilities, or error

**Steps**:
1. [ ] - `p1` - **TRY** - `inst-rc-try`
   1. [ ] - `p1` - HTTP: GET Model Registry — retrieve list of available models and designated default model - `inst-rc-get-models`
   2. [ ] - `p1` - Build `model` capability: `{ id: "model", type: "enum", enum_values: [models from registry], default_value: [default from registry] }` - `inst-rc-build-model-cap`
   3. [ ] - `p1` - HTTP: GET Model Registry — retrieve capabilities for the default model (temperature, max_tokens, web_search, etc.) - `inst-rc-get-model-caps`
   4. [ ] - `p1` - Map model-specific parameters to additional `Capability` entries (type, default_value, constraints per parameter) - `inst-rc-map-caps`
   5. [ ] - `p1` - **RETURN** combined `Vec<Capability>` (model + model-specific capabilities) - `inst-rc-return`
2. [ ] - `p1` - **CATCH** HTTP error (timeout, connection refused, non-2xx) - `inst-rc-catch`
   1. [ ] - `p1` - **RETURN** error (Model Registry unavailable) - `inst-rc-error`

### Refresh Capabilities on Model Change

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-llm-gateway-plugin-refresh-capabilities`

**Input**: Plugin context with updated `CapabilityValue[]`, current `Session.enabled_capabilities`
**Output**: `Vec<Capability>` with refreshed model-specific capabilities, or error

**Steps**:
1. [ ] - `p1` - Extract current `model` capability value from `ctx.enabled_capabilities` - `inst-ref-extract-current`
2. [ ] - `p1` - Extract new `model` capability value from updated `CapabilityValue[]` - `inst-ref-extract-new`
3. [ ] - `p1` - **IF** model value has not changed **RETURN** existing capabilities unchanged - `inst-ref-no-change`
4. [ ] - `p1` - **TRY** - `inst-ref-try`
   1. [ ] - `p1` - HTTP: GET Model Registry — retrieve capabilities for the newly selected model - `inst-ref-get-new-caps`
   2. [ ] - `p1` - Rebuild capabilities: preserve `model` capability with existing `enum_values`, update `default_value` to new model, replace model-specific capabilities with new model's parameters - `inst-ref-rebuild`
   3. [ ] - `p1` - **RETURN** updated `Vec<Capability>` - `inst-ref-return`
5. [ ] - `p1` - **CATCH** HTTP error - `inst-ref-catch`
   1. [ ] - `p1` - **RETURN** error (Model Registry unavailable) - `inst-ref-error`

### Forward Message to LLM Gateway

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-llm-gateway-plugin-forward-to-gateway`

**Input**: `messages: Message[]`, `CapabilityValue[]`, plugin config
**Output**: Streaming response chunks written to `ResponseStream`, or error

**Steps**:
1. [ ] - `p1` - Extract LLM parameters from `CapabilityValue[]` (model, temperature, max_tokens, web_search) - `inst-fwd-extract-params`
2. [ ] - `p1` - Build LLM Gateway request payload: messages list, model parameters, plugin config options - `inst-fwd-build-request`
3. [ ] - `p1` - **TRY** - `inst-fwd-try`
   1. [ ] - `p1` - HTTP: POST LLM Gateway — send request with streaming response enabled - `inst-fwd-post`
   2. [ ] - `p1` - **FOR EACH** chunk received from LLM Gateway response stream - `inst-fwd-chunks`
      1. [ ] - `p1` - Write chunk to `ResponseStream` - `inst-fwd-write-chunk`
   3. [ ] - `p1` - Extract response metadata: `model_used`, `finish_reason`, `temperature_used`, `LlmUsage` (prompt_tokens, completion_tokens, total_tokens, cached_tokens) - `inst-fwd-extract-metadata`
   4. [ ] - `p1` - Write `LlmMessageMetadata` to `ResponseStream` as final metadata - `inst-fwd-write-metadata`
4. [ ] - `p1` - **CATCH** context_overflow error from LLM Gateway - `inst-fwd-catch-overflow`
   1. [ ] - `p1` - **RETURN** `context_overflow` error signal (recoverable) - `inst-fwd-overflow`
5. [ ] - `p1` - **CATCH** HTTP error (timeout, connection refused, non-2xx) - `inst-fwd-catch-http`
   1. [ ] - `p1` - **RETURN** error (LLM Gateway unavailable) - `inst-fwd-error`

### Generate Summary

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-algo-llm-gateway-plugin-generate-summary`

**Input**: `messages: Message[]` (full visible history), `LlmPluginConfig` from plugin config
**Output**: `SummaryResult` (summary_text, summarized_message_ids) via `ResponseStream`, or error

**Steps**:
1. [ ] - `p2` - Read `summarization_settings` from `LlmPluginConfig` in `ctx.plugin_config.config` - `inst-gs-read-settings`
2. [ ] - `p2` - **IF** `summarization_settings` is null **RETURN** error (summarization not supported for this plugin config) - `inst-gs-not-supported`
3. [ ] - `p2` - Read `recent_messages_to_keep` from `summarization_settings` (default: 10, min: 2) - `inst-gs-read-keep`
4. [ ] - `p2` - Split messages: `to_summarize = messages[0..len-recent_messages_to_keep]`, `to_keep = messages[len-recent_messages_to_keep..len]` - `inst-gs-split`
5. [ ] - `p2` - **TRY** - `inst-gs-try`
   1. [ ] - `p2` - HTTP: POST LLM Gateway — send `to_summarize` messages for summary generation - `inst-gs-post`
   2. [ ] - `p2` - Receive summary text from LLM Gateway response - `inst-gs-receive`
   3. [ ] - `p2` - Build `SummaryResult`: `summary_text` from LLM response, `summarized_message_ids` from IDs of `to_summarize` messages - `inst-gs-build-result`
   4. [ ] - `p2` - **RETURN** `SummaryResult` via `ResponseStream` - `inst-gs-return`
6. [ ] - `p2` - **CATCH** HTTP error - `inst-gs-catch`
   1. [ ] - `p2` - **RETURN** error (LLM Gateway unavailable during summarization) - `inst-gs-error`

### Plugin Resilience

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-llm-gateway-plugin-resilience`

**Input**: Outbound HTTP request to Model Registry or LLM Gateway
**Output**: HTTP response or error after resilience policies applied

**Steps**:
**Configurable Defaults**: `retry_count: 3`, `retry_delay_ms: 1000` (base delay before exponential backoff), `timeout_ms: 30000` (per-request deadline), `circuit_breaker_failure_threshold: 5`, `circuit_breaker_cooldown_ms: 60000`

1. [ ] - `p1` - Apply per-service timeout: configurable deadline for each HTTP call to Model Registry and LLM Gateway (default: `timeout_ms: 30000`) - `inst-res-timeout`
2. [ ] - `p1` - Apply retry policy: configurable retry count with exponential backoff for transient failures (5xx, connection reset) — non-streaming calls only (Model Registry queries) (defaults: `retry_count: 3`, `retry_delay_ms: 1000`) - `inst-res-retry`
3. [ ] - `p1` - Apply circuit breaker: per-service circuit breaker (Model Registry, LLM Gateway) that opens after configurable failure threshold (default: `circuit_breaker_failure_threshold: 5`) and half-opens after cooldown period (default: `circuit_breaker_cooldown_ms: 60000`) - `inst-res-circuit-breaker`
4. [ ] - `p1` - **IF** circuit breaker is open **RETURN** error immediately without attempting HTTP call - `inst-res-cb-open`
5. [ ] - `p1` - **RETURN** HTTP response from downstream service or error after policies exhausted - `inst-res-return`

## 4. States (CDSL)

### None

No plugin-specific state machines. The LLM Gateway Plugin is stateless; session and streaming state machines are owned by `cpt-cf-chat-engine-feature-session-lifecycle` and `cpt-cf-chat-engine-feature-message-processing` respectively.

## 5. Definitions of Done

### GTS Schema Registration

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-llm-gateway-plugin-schema-registration`

The system **MUST** register all LLM-specific GTS schemas (`LlmPluginConfig`, `LlmSummarizationSettings`, `LlmMessageMetadata`, `LlmUsage`, and entity extension schemas) at plugin startup, isolated under the `gtx.cf.chat_engine.llm_gateway.*` namespace, before any session type referencing this plugin can be created.

**Implements**:
- `cpt-cf-chat-engine-flow-llm-gateway-plugin-register-schemas`

**Touches**:
- GTS Schema Registry: `gtx.cf.chat_engine.llm_gateway.*` namespace
- Entities: `LlmPluginConfig`, `LlmSummarizationSettings`, `LlmMessageMetadata`, `LlmUsage`

### Model Registry Capability Resolution

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-llm-gateway-plugin-capability-resolution`

The system **MUST** query Model Registry during `on_session_created` to resolve available models (Step 1) and default model capabilities (Step 2), returning a `Vec<Capability>` that Chat Engine stores as `Session.enabled_capabilities`. On `on_session_updated` with a changed model value, the system **MUST** query Model Registry for the new model's capabilities and return an updated `Vec<Capability>`.

**Implements**:
- `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-session-created`
- `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-session-updated`
- `cpt-cf-chat-engine-algo-llm-gateway-plugin-resolve-capabilities`
- `cpt-cf-chat-engine-algo-llm-gateway-plugin-refresh-capabilities`

**Touches**:
- External: Model Registry HTTP API
- Entities: `Capability`, `CapabilityValue`

### LLM Gateway Message Forwarding

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-llm-gateway-plugin-message-forwarding`

The system **MUST** forward messages to the LLM Gateway service via HTTP on `on_message` and `on_message_recreate`, stream response chunks back through `ResponseStream`, and populate `LlmMessageMetadata` (model_used, finish_reason, temperature_used, LlmUsage) in the response metadata.

**Implements**:
- `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-message`
- `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-message-recreate`
- `cpt-cf-chat-engine-algo-llm-gateway-plugin-forward-to-gateway`

**Touches**:
- External: LLM Gateway HTTP API
- Entities: `LlmMessageMetadata`, `LlmUsage`, `ResponseStream`

### Context Overflow Summarization

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-llm-gateway-plugin-summarization`

The system **MUST** handle `context_overflow` errors from LLM Gateway by signaling the error to Chat Engine, which triggers `on_session_summary`. The plugin **MUST** split messages based on `summarization_settings.recent_messages_to_keep`, generate a summary via LLM Gateway, and return a `SummaryResult`. When `summarization_settings` is null, the plugin **MUST** return an error indicating summarization is not supported.

**Implements**:
- `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-message` (context_overflow path)
- `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-session-summary`
- `cpt-cf-chat-engine-algo-llm-gateway-plugin-generate-summary`

**Touches**:
- External: LLM Gateway HTTP API
- Entities: `SummaryResult`, `LlmSummarizationSettings`

### Message Visibility Flags

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-llm-gateway-plugin-visibility-flags`

The system **MUST** respect `is_hidden_from_backend` when constructing the `messages[]` list for plugin calls (exclude hidden messages) and set `is_hidden_from_user=true` on summary messages so they are invisible to clients but visible to the backend. After summarization, summarized messages **MUST** be marked `is_hidden_from_backend=true` by Chat Engine based on `summarized_message_ids` returned by the plugin.

**Implements**:
- `cpt-cf-chat-engine-flow-llm-gateway-plugin-on-session-summary`
- `cpt-cf-chat-engine-algo-llm-gateway-plugin-generate-summary`

**Touches**:
- DB: `messages.is_hidden_from_backend`, `messages.is_hidden_from_user`
- Entities: `Message`

### Plugin-Owned Resilience

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-llm-gateway-plugin-resilience`

The system **MUST** implement plugin-owned resilience for all outbound HTTP calls: configurable timeout per service, retry with exponential backoff for non-streaming calls (Model Registry), and per-service circuit breaker (Model Registry, LLM Gateway). Chat Engine core **MUST NOT** contain any retry, circuit breaker, or timeout logic for plugin external calls.

**Implements**:
- `cpt-cf-chat-engine-algo-llm-gateway-plugin-resilience`

**Touches**:
- External: Model Registry HTTP API, LLM Gateway HTTP API

## 6. Acceptance Criteria

- [ ] LLM plugin registers all GTS schemas (`LlmPluginConfig`, `LlmSummarizationSettings`, `LlmMessageMetadata`, `LlmUsage`, entity extensions) under `gtx.cf.chat_engine.llm_gateway.*` namespace at startup; non-LLM session types are unaffected
- [ ] Creating a session with an LLM-backed session type queries Model Registry and returns capabilities including model selection, temperature, max_tokens, and web_search
- [ ] Changing the model capability value on a session triggers `on_session_updated`, queries Model Registry for the new model's capabilities, and returns refreshed capabilities
- [ ] Sending a message to an LLM-backed session forwards the request to LLM Gateway and streams response chunks back to the client via NDJSON
- [ ] Assistant message metadata includes `model_used`, `finish_reason`, and `LlmUsage` token counts
- [ ] When LLM Gateway returns `context_overflow`, Chat Engine triggers summarization; the plugin splits messages, generates summary, and returns `SummaryResult`; Chat Engine persists the summary message with `is_hidden_from_user=true`, marks summarized messages with `is_hidden_from_backend=true`, and retries the original request
- [ ] When `summarization_settings` is null in plugin config, `context_overflow` is propagated to the client
- [ ] Plugin circuit breaker opens after repeated failures to Model Registry or LLM Gateway; subsequent calls fail fast without network attempt until cooldown expires
- [ ] Plugin timeout and retry policies are independently configurable per external service (Model Registry, LLM Gateway)

## 7. Non-Functional Considerations

- **Performance**: Model Registry queries during session creation add latency; capabilities are resolved once per session and cached in `Session.enabled_capabilities`. LLM Gateway streaming starts immediately; no buffering before first chunk relay.
- **Reliability**: Plugin owns all resilience: HTTP retry with exponential backoff for Model Registry queries, per-service circuit breaker for both Model Registry and LLM Gateway, configurable timeouts. Chat Engine isolates LLM plugin failures from other session types.
- **Security**: Plugin config (`PluginConfig.config`) may contain sensitive service URLs and credentials. Chat Engine treats config as opaque JSONB; only the plugin interprets it. All outbound HTTP calls use TLS.
- **Data**: `LlmMessageMetadata` stored in `messages.metadata` JSONB field. `LlmPluginConfig` stored in `plugin_configs.config` JSONB field. GTS schema validation ensures type safety. No new database tables.
- **Observability**: Structured log events for Model Registry calls, LLM Gateway calls, and summarization flows with `trace_id`, `session_id`, `duration_ms`, `model_used`. Metrics: `llm_gateway_request_duration_seconds`, `model_registry_request_duration_seconds`, circuit breaker state transitions.
- **Compliance / UX / Business**: Not applicable — see session-lifecycle section 7.
