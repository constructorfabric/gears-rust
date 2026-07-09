Created:  2026-03-20 by Constructor Tech
Updated:  2026-03-20 by Constructor Tech
# Feature: Plugin System Infrastructure


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Plugin SDK Contract](#15-plugin-sdk-contract)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Register Plugin at Startup](#register-plugin-at-startup)
  - [Resolve Plugin](#resolve-plugin)
  - [Invoke Plugin Method](#invoke-plugin-method)
  - [Webhook-Compat Plugin](#webhook-compat-plugin)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Resolve Plugin and Config](#resolve-plugin-and-config)
  - [Validate Plugin Availability](#validate-plugin-availability)
  - [Store Plugin Config](#store-plugin-config)
- [4. States (CDSL)](#4-states-cdsl)
  - [Plugin Registration State Machine](#plugin-registration-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [ChatEngineBackendPlugin Trait Definition](#chatenginebackendplugin-trait-definition)
  - [Plugin Registry via ClientHub](#plugin-registry-via-clienthub)
  - [Plugin Config Table](#plugin-config-table)
  - [N:1 Session Type to Plugin Relationship](#n1-session-type-to-plugin-relationship)
  - [Webhook-Compat First-Party Plugin](#webhook-compat-first-party-plugin)
  - [Plugin Health Check](#plugin-health-check)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Functional Considerations](#7-non-functional-considerations)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-featstatus-plugin-system`

## 1. Feature Context

- [ ] `p1` - `cpt-cf-chat-engine-feature-plugin-system`

### 1.1 Overview

Internal infrastructure layer for backend plugin integration: defines the `ChatEngineBackendPlugin` trait with all lifecycle and message methods, implements the plugin registry with ClientHub/GTS-based discovery and resolution by `plugin_instance_id`, manages per-session-type plugin configuration via the `plugin_configs` table (composite PK: `plugin_instance_id` + `session_type_id`), supports N:1 session type to plugin relationships, ships the first-party `webhook-compat` plugin for legacy HTTP backends, and provides a plugin health check mechanism for session type configuration.

**Traces to**: `cpt-cf-chat-engine-fr-schema-extensibility`, `cpt-cf-chat-engine-nfr-backend-isolation`, `cpt-cf-chat-engine-nfr-availability`, `cpt-cf-chat-engine-nfr-response-time`

### 1.2 Purpose

Provide a type-safe, transport-agnostic integration boundary between Chat Engine core and backend processing logic. Chat Engine calls trait methods; plugins own all outbound communication, auth, retry, and resilience patterns.

Success criteria: Plugins are registered at startup via ClientHub, resolved by `plugin_instance_id` on every session and message operation, and invoked with full call context including per-session-type `plugin_config`. The `webhook-compat` plugin wraps legacy HTTP webhook endpoints without any changes to Chat Engine core.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-chat-engine-actor-developer` | Registers session types referencing a `plugin_instance_id`; configures per-session-type plugin config |
| `cpt-cf-chat-engine-actor-backend-plugin` | Implements `ChatEngineBackendPlugin` trait; receives lifecycle and message events via trait methods |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md) -- Component Model (Plugin Integration Gear), plugin_configs table, Plugin API Contract
- **ADR**: [ADR-0022](../ADR/0022-plugin-backend-integration.md) -- Internal Plugin Interface for Backend Integration
- **SDK source**: [`chat-engine-sdk`](../../chat-engine-sdk/src) — `lib.rs`, `models.rs`, `plugin.rs`, `error.rs`
- **Dependencies**: `cpt-cf-chat-engine-feature-session-lifecycle`

### 1.5 Plugin SDK Contract

This section documents the public Rust contract that `ChatEngineBackendPlugin` authors program against. The crate `chat-engine-sdk` is the single source of truth; this section restates it. When in doubt, the SDK source wins.

**Call-context wrappers.** Each trait method receives one of two wrapper structs that bundle the per-call `PluginCallContext` with method-specific identifiers:

- `SessionPluginCtx { session_type_id, session_id: Option<Uuid>, call_ctx }` — passed to lifecycle hooks. `session_id` is `None` exclusively during `on_session_type_configured` (the session does not exist yet); for `on_session_created`, `on_session_updated`, and `on_session_summary` it is always `Some`.
- `MessagePluginCtx { session_id, message_id, messages, call_ctx }` — passed to `on_message` and `on_message_recreate`. `messages` is the filtered history (excluding `is_hidden_from_backend = true` entries).

#### PluginCallContext

`PluginCallContext` (`chat-engine-sdk::plugin::PluginCallContext`) is attached to every plugin invocation and carries the following fields:

| Field | Type | Notes |
|-------|------|-------|
| `request_id` | `Uuid` | Correlation ID for this plugin invocation; a fresh UUIDv4 per call (or propagated from upstream). Plugins **MUST** include it in every log line emitted while handling the call. This is the field referred to as `trace_id` elsewhere in the docs — both names denote the same per-call correlation handle. |
| `tenant_id` | `TenantId` | Tenant that owns the session issuing the call. |
| `user_id` | `UserId` | End-user behind the call. |
| `plugin_instance_id` | `String` | GTS plugin instance ID that is handling the call (matches the bound `SessionType.plugin_instance_id`). |
| `session_type_id` | `Uuid` | Session type the call is scoped to. |
| `plugin_config` | `Option<serde_json::Value>` | Per-`(plugin_instance_id, session_type_id)` configuration from `plugin_configs`. Opaque to Chat Engine. |
| `enabled_capabilities` | `Option<Vec<CapabilityValue>>` | Capability values selected by the client for this call. |
| `deadline` | `Option<Instant>` | Absolute monotonic deadline for this call. Plugins **MUST** bound long-running work (HTTP requests, retries) to remain within this budget. `None` means Chat Engine did not set a deadline. |
| `cancel` | `tokio_util::sync::CancellationToken` | Cooperative cancellation signal — see below. |

`PluginCallContext::remaining() -> Option<Duration>` has **three distinct states** that callers MUST handle individually:

| Return value | Meaning |
|--------------|---------|
| `None` | No deadline was set. The plugin may use its own default budget. |
| `Some(Duration::ZERO)` | Deadline has already elapsed. The plugin **MUST** abort immediately, typically returning `PluginError::timeout()`. |
| `Some(d > 0)` | `d` of budget remains. Pass this to `tokio::time::timeout(...)` or `reqwest::Client::timeout(...)`. |

Collapsing "no deadline" and "elapsed" into a single `None` would be a footgun: `.unwrap_or(default_budget)` would let elapsed deadlines silently extend their budget.

`PluginCallContext::is_cancelled() -> bool` is a pre-flight check; for cooperative cancellation use `select!` over `cancel.cancelled()` alongside the plugin's work.

**Sources of cancellation**. Chat Engine cancels the token when:
1. The client disconnects (HTTP stream closed).
2. The deadline elapses (Chat Engine bridges `deadline → cancel`).
3. An explicit `DELETE /streaming` is invoked on the session.

When the signal fires, plugins should return `PluginError::transient("cancelled")` (or `PluginError::timeout()` if cancellation was deadline-driven) and stop emitting events.

**Clone semantics**. Clones of `CancellationToken` share the same cancellation state. Calling `.cancel()` on any clone — including one obtained by cloning the enclosing `PluginCallContext` — cancels every other holder, **including Chat Engine's parent token**. If a plugin fans out concurrent sub-tasks that need independent cancellation, it **MUST** derive child tokens with `CancellationToken::child_token` rather than cloning.

#### PluginStream and the `'static` requirement

Streaming plugin responses are returned as `PluginStream`:

```rust
pub type PluginStream = BoxStream<'static, Result<StreamingEvent, PluginError>>;
```

The outer `Result<PluginStream, _>` returned by trait methods represents errors that occur **before** the stream starts (e.g., invalid config, plugin unavailable). Once a stream is returned, individual items may be `Err` to signal mid-stream failures (e.g., upstream disconnect) without aborting the stream.

The `'static` bound is load-bearing: Chat Engine drives the stream to completion **after** the trait method returns, so the stream may not borrow from `&self` — any reference would dangle once the call frame unwinds. The compiler error is a lifetime mismatch pointing inside the `async_stream::stream! { … }` / `futures::stream::unfold(…)` body, **not** at the trait signature. Two idiomatic fixes:

```rust
// ❌ Captures `&self.config` — won't satisfy `'static`.
async fn on_message(&self, ctx: MessagePluginCtx)
    -> Result<PluginStream, PluginError>
{
    Ok(async_stream::stream! {
        let response = self.config.client.send(&ctx.messages).await?;
        // ...
    }.boxed())
}

// ✅ Clone the bits you need out of `self` first.
async fn on_message(&self, ctx: MessagePluginCtx)
    -> Result<PluginStream, PluginError>
{
    let client = self.config.client.clone();
    Ok(async_stream::stream! {
        let response = client.send(&ctx.messages).await?;
        // ...
    }.boxed())
}

// ✅ Or hold the plugin in an `Arc<Self>` and clone the handle.
let me = Arc::clone(&self);
Ok(async_stream::stream! { me.do_things(...).await; }.boxed())
```

For non-streaming responses, the SDK exposes two helpers:

- `chat_engine_sdk::plugin::empty_stream() -> PluginStream` — used when a lifecycle hook has nothing to emit.
- `chat_engine_sdk::plugin::stream_from_events(events: Vec<StreamingEvent>) -> PluginStream` — collects all events synchronously before returning; sidesteps the `'static` issue entirely for stub or non-streaming implementations.

#### PluginError taxonomy

`chat-engine-sdk::error::PluginError` is the single error type returned by every fallible plugin method. Chat Engine's reaction to each variant is fixed:

| Variant | Suggested HTTP | Retryable | User-facing? | Typical cause |
|---------|----------------|-----------|--------------|---------------|
| `Transient` | 503 | yes | no | network blip, upstream 5xx |
| `RateLimited` | 429 | yes | yes | upstream `Retry-After` / 429 |
| `Timeout` | 504 | yes | no | request exceeded the deadline |
| `InvalidInput` | 400 | no | yes | bad request payload, validation failure |
| `Unauthorized` | 401 | no | yes | auth token missing/expired/insufficient |
| `NotFound` | 404 | no | yes | model / resource does not exist |
| `Internal` | 500 | no | no (page on-call) | misconfiguration, plugin bug |

The SDK exposes routing helpers; do not duplicate the table above in code:

- `PluginError::suggested_status() -> u16` — HTTP status to surface.
- `PluginError::is_retryable() -> bool` — whether Chat Engine should retry (with backoff).
- `PluginError::is_user_facing() -> bool` — whether the error message is safe to surface to the end user. "User-facing" means the message describes a user mistake; non-user-facing variants may leak operator details and **MUST** be replaced by a generic message at the API boundary.
- `PluginError::retry_after() -> Option<Duration>` — `Retry-After` hint when the variant carries one (currently only `RateLimited`).

Every variant has paired constructors: `PluginError::transient(msg)` and `PluginError::transient_with(msg, source)` (and the same pattern for `invalid_input`, `unauthorized`, `not_found`, `internal`, `timeout`, `rate_limited`). The `_with` variants attach an upstream `std::error::Error + Send + Sync + 'static` as `#[source]`, preserving the cause chain across the SDK boundary — callers walk it via `Error::source()`.

**RateLimited.retry_after** is `Option<Duration>`. When present, Chat Engine surfaces it as the `Retry-After` HTTP header (or `retry_after_seconds` in the error body for REST clients).

#### Default trait implementations

Every method on `ChatEngineBackendPlugin` except `plugin_instance_id` has a default implementation that returns the empty/no-op result:

| Method | Default body |
|--------|--------------|
| `on_session_type_configured` | `Ok(vec![])` — no capabilities declared |
| `on_session_created` | `Ok(vec![])` |
| `on_session_updated` | `Ok(vec![])` |
| `on_message` | `Ok(empty_stream())` |
| `on_message_recreate` | `Ok(empty_stream())` |
| `on_session_summary` | `Ok(empty_stream())` |
| `health_check` | `Ok(HealthStatus::Healthy)` |
| `plugin_instance_id` | **required — no default** |

Plugins **MUST** implement `plugin_instance_id` and override only the lifecycle and message methods they actually need.

#### Debug-redaction contract

The SDK's `Debug` impls intentionally redact secrets and PII so that plugin logs cannot leak data:

| Type | Redacted field | Surfaced summary |
|------|----------------|------------------|
| `Session` | `share_token` | `Some("<redacted>")` / `None` |
| `PluginCallContext` | `plugin_config` | `Some("<redacted>")` / `None` |
| `MessagePluginCtx` | `messages` (entire vec) | `"<redacted: N message(s); user=X, assistant=Y, system=Z>"` |

Plugin authors **MUST NOT** add their own `Debug`/`Display` impls (or `tracing` fields) that bypass this redaction — that would defeat the contract. When custom diagnostics are needed, log derived statistics (counts, sizes, durations) rather than payload content. Wrappers `SessionPluginCtx` and `MessagePluginCtx` derive `Debug`, transitively inheriting the redaction from `PluginCallContext`.

## 2. Actor Flows (CDSL)

### Register Plugin at Startup

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-plugin-system-register`

**Actor**: `cpt-cf-chat-engine-actor-backend-plugin`

**Success Scenarios**:
- Plugin implementation is discovered and registered in the plugin registry at startup; available for resolution by `plugin_instance_id`

**Error Scenarios**:
- Plugin fails to initialize (missing configuration, dependency error) -- startup continues without this plugin; log error
- Plugin is already in `registered` state (duplicate registration attempt) -- skip re-registration, log info-level notice that the plugin is already active
- Plugin is in `failed` state from a previous attempt -- re-attempt initialization (transition failed → unregistered → initializing)

**Steps**:
1. [ ] - `p1` - On Chat Engine startup: scan all `ChatEngineBackendPlugin` trait implementations registered with ClientHub - `inst-reg-scan`
2. [ ] - `p1` - **FOR EACH** plugin implementation discovered - `inst-reg-foreach`
   1. [ ] - `p1` - Extract `plugin_instance_id` (GTS ID) from the registration - `inst-reg-extract-id`
   2. [ ] - `p1` - **TRY** initialize plugin: call plugin's initialization logic - `inst-reg-try-init`
   3. [ ] - `p1` - **CATCH** initialization error: log error with `plugin_instance_id`, skip this plugin, continue with remaining plugins - `inst-reg-catch-init`
   4. [ ] - `p1` - Register plugin in ClientHub under its `plugin_instance_id` scope: `hub.register_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(&plugin_instance_id), plugin)` - `inst-reg-register`
3. [ ] - `p1` - Log summary: count of successfully registered plugins vs total discovered - `inst-reg-summary`

### Resolve Plugin

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-plugin-system-resolve`

**Actor**: `cpt-cf-chat-engine-actor-developer` (indirectly, via session type operations)

**Success Scenarios**:
- Plugin is resolved by `plugin_instance_id` and returned to the caller with its per-session-type config

**Error Scenarios**:
- `plugin_instance_id` not found in ClientHub (plugin not registered or failed to initialize)

**Steps**:
1. [ ] - `p1` - Receive resolution request with `plugin_instance_id` and `session_type_id` - `inst-resolve-input`
2. [ ] - `p1` - Resolve plugin: `hub.get_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(&plugin_instance_id))` - `inst-resolve-lookup`
3. [ ] - `p1` - **IF** plugin not found **RETURN** error (plugin not registered) - `inst-resolve-not-found`
4. [ ] - `p1` - DB: Load the plugin config (JSONB) for the given plugin_instance_id and session_type_id from the plugin_configs table - `inst-resolve-load-config`
5. [ ] - `p1` - **RETURN** resolved plugin handle and plugin_config (JSONB, may be NULL if no config row exists) - `inst-resolve-return`

### Invoke Plugin Method

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-plugin-system-invoke`

**Actor**: `cpt-cf-chat-engine-actor-backend-plugin` (called by Chat Engine core on behalf of client or developer operations)

**Success Scenarios**:
- Plugin method is invoked with full call context; result is returned to the caller

**Error Scenarios**:
- Plugin not found during resolution (503 Service Unavailable propagated to caller)
- Plugin method returns an error (propagated to caller as 502 Bad Gateway)

**Steps**:
1. [ ] - `p1` - Receive invocation request: method_name, session_type_id, plugin_instance_id, caller-specific context - `inst-invoke-input`
2. [ ] - `p1` - Algorithm: resolve plugin and config using `cpt-cf-chat-engine-algo-plugin-system-resolve` - `inst-invoke-resolve`
3. [ ] - `p1` - Build call context: {session_type_id, plugin_config, tenant_id, user_id, client_id, session_id (if applicable), method-specific payload, timestamp} - `inst-invoke-build-ctx`
4. [ ] - `p1` - **TRY** - `inst-invoke-try`
   1. [ ] - `p1` - Dispatch to plugin trait method (on_session_type_configured / on_session_created / on_session_updated / on_message / on_message_recreate / on_session_summary / health_check) - `inst-invoke-dispatch`
   2. [ ] - `p1` - **RETURN** method result to caller (Vec<Capability>, ResponseStream, HealthStatus, or void) - `inst-invoke-return`
5. [ ] - `p1` - **CATCH** plugin error - `inst-invoke-catch`
   1. [ ] - `p1` - Log error with trace_id, plugin_instance_id, session_type_id, method_name, error details - `inst-invoke-log-error`
   2. [ ] - `p1` - **RETURN** error to caller for upstream handling (caller decides HTTP status: 502, 503, or fire-and-forget) - `inst-invoke-return-error`

### Webhook-Compat Plugin

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-flow-plugin-system-webhook-compat`

**Actor**: `cpt-cf-chat-engine-actor-backend-plugin` (first-party plugin implementation)

**Success Scenarios**:
- Legacy HTTP webhook endpoint is called via the `webhook-compat` plugin; response is forwarded back through the trait interface

**Error Scenarios**:
- Remote endpoint unreachable or returns an error response (plugin returns error; plugin owns retry/timeout internally)

**Steps**:
1. [ ] - `p1` - Receive trait method call with call context (includes `plugin_config` containing endpoint address, authentication config, timeout, retry config) - `inst-wc-input`
2. [ ] - `p1` - Extract transport configuration from `plugin_config.config` JSONB: endpoint address, auth_type, auth_credentials, timeout_ms, retry_count, retry_backoff_ms - `inst-wc-extract-config`
3. [ ] - `p1` - Map trait method to webhook event type (on_session_created -> session.created, on_message -> message.new, etc.) - `inst-wc-map-event`
4. [ ] - `p1` - Build outbound request with event payload conforming to Chat Engine webhook schemas - `inst-wc-build-request`
5. [ ] - `p1` - **IF** auth_type is configured: apply authentication credentials to the outbound request - `inst-wc-apply-auth`
6. [ ] - `p1` - **TRY** - `inst-wc-try`
   1. [ ] - `p1` - Send request to the configured endpoint with configured timeout; for streaming methods (on_message, on_message_recreate, on_session_summary), read chunked response and pipe chunks to ResponseStream - `inst-wc-send`
   2. [ ] - `p1` - **IF** endpoint returns error response: **IF** retry_count > 0, retry with backoff; else return error - `inst-wc-retry`
   3. [ ] - `p1` - Parse response body according to expected schema for the event type - `inst-wc-parse`
   4. [ ] - `p1` - **RETURN** parsed result (Vec<Capability>, ResponseStream, HealthStatus) - `inst-wc-return`
7. [ ] - `p1` - **CATCH** transport error (connection refused, timeout, security handshake failure) - `inst-wc-catch`
   1. [ ] - `p1` - **RETURN** error with detail (endpoint unreachable, timeout exceeded, transport failure) - `inst-wc-return-error`

## 3. Processes / Business Logic (CDSL)

### Resolve Plugin and Config

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-plugin-system-resolve`

**Input**: plugin_instance_id, session_type_id
**Output**: Plugin handle + plugin_config JSONB, or error

**Steps**:
1. [ ] - `p1` - Resolve plugin from ClientHub: `hub.get_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(&plugin_instance_id))` - `inst-algo-resolve-hub`
2. [ ] - `p1` - **IF** plugin not found in registry **RETURN** error (plugin_instance_id not registered; plugin may have failed initialization or is not deployed) - `inst-algo-resolve-not-found`
3. [ ] - `p1` - DB: Load the plugin config (JSONB) for the given plugin_instance_id and session_type_id from the plugin_configs table - `inst-algo-resolve-config`
4. [ ] - `p1` - **RETURN** (plugin_handle, plugin_config) -- plugin_config is NULL if no config row exists for this session_type_id - `inst-algo-resolve-return`

### Validate Plugin Availability

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-plugin-system-validate-availability`

**Input**: plugin_instance_id
**Output**: Confirmation that plugin is registered and responsive, or error

**Steps**:
1. [ ] - `p1` - Resolve plugin from ClientHub: `hub.get_scoped::<dyn ChatEngineBackendPlugin>(ClientScope::gts_id(&plugin_instance_id))` - `inst-algo-avail-resolve`
2. [ ] - `p1` - **IF** plugin not found **RETURN** error (plugin not registered) - `inst-algo-avail-not-found`
3. [ ] - `p2` - **TRY** call `plugin.health_check()` - `inst-algo-avail-health`
4. [ ] - `p2` - **CATCH** health check error: log warning, but do not fail -- health_check is optional - `inst-algo-avail-health-catch`
5. [ ] - `p1` - **RETURN** plugin is available - `inst-algo-avail-return`

### Store Plugin Config

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-algo-plugin-system-store-config`

**Input**: plugin_instance_id, session_type_id, config JSONB
**Output**: Stored plugin_config record

**Steps**:
1. [ ] - `p1` - Validate plugin_instance_id is registered in ClientHub using `cpt-cf-chat-engine-algo-plugin-system-validate-availability` - `inst-algo-store-validate`
2. [ ] - `p1` - DB: Upsert the plugin_configs record for the given plugin_instance_id and session_type_id — create if not exists, otherwise update the config and refresh updated_at - `inst-algo-store-upsert`
3. [ ] - `p1` - **RETURN** stored plugin_config record - `inst-algo-store-return`

## 4. States (CDSL)

### Plugin Registration State Machine

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-state-plugin-system-registration`

**States**: unregistered, initializing, registered, failed
**Initial State**: unregistered

**Transitions**:
1. [ ] - `p1` - **FROM** unregistered **TO** initializing **WHEN** Chat Engine startup discovers the plugin implementation - `inst-st-to-initializing`
2. [ ] - `p1` - **FROM** initializing **TO** registered **WHEN** plugin initialization succeeds and plugin is added to ClientHub registry - `inst-st-to-registered`
3. [ ] - `p1` - **FROM** initializing **TO** failed **WHEN** plugin initialization throws an error - `inst-st-to-failed`
4. [ ] - `p2` - **FROM** failed **TO** unregistered **WHEN** admin triggers a plugin reset (e.g., via service restart or admin API); the plugin re-enters the discovery flow on the next startup cycle - `inst-st-failed-to-unregistered`

## 5. Definitions of Done

### ChatEngineBackendPlugin Trait Definition

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-plugin-system-trait`

The system **MUST** define the `ChatEngineBackendPlugin` trait in the `chat-engine-sdk` crate with all lifecycle and message methods: `on_session_type_configured`, `on_session_created`, `on_session_updated`, `on_message`, `on_message_recreate`, `on_session_summary`, and `health_check`. Each method receives a typed call context (`SessionPluginCtx` / `MessagePluginCtx`, both carrying a `PluginCallContext`) and returns the appropriate result type (`Vec<Capability>`, `PluginStream`, or `HealthStatus`). All methods except `plugin_instance_id() -> &str` have default no-op implementations (`Ok(vec![])` / `Ok(empty_stream())` / `Ok(HealthStatus::Healthy)`); plugins override only what they need. See [§1.5 Plugin SDK Contract](#15-plugin-sdk-contract) for the full call-context, stream, error-taxonomy, and Debug-redaction contracts.

**Implements**:
- `cpt-cf-chat-engine-flow-plugin-system-invoke`

**Touches**:
- Crate: `chat-engine-sdk`
- Entities: `ChatEngineBackendPlugin` (trait), `SessionPluginCtx`, `MessagePluginCtx`, `PluginCallContext`, `PluginStream`, `PluginError`, `Capability`, `CapabilityValue`, `HealthStatus`, `StreamingEvent`

### Plugin Registry via ClientHub

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-plugin-system-registry`

The system **MUST** discover and register all `ChatEngineBackendPlugin` implementations via ClientHub at startup, making them resolvable by `plugin_instance_id` (GTS ID). A failed plugin initialization must not prevent other plugins from registering.

**Implements**:
- `cpt-cf-chat-engine-flow-plugin-system-register`
- `cpt-cf-chat-engine-algo-plugin-system-resolve`
- `cpt-cf-chat-engine-state-plugin-system-registration`

**Touches**:
- ClientHub: `dyn ChatEngineBackendPlugin` scoped by `plugin_instance_id`
- Entities: `ChatEngineBackendPlugin`

### Plugin Config Table

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-plugin-system-config-table`

The system **MUST** persist per-session-type plugin configuration in the `plugin_configs` table with composite PK `(plugin_instance_id, session_type_id)` and an opaque JSONB `config` column. Config is forwarded to the plugin in every call context. Chat Engine never interprets the config contents.

**Implements**:
- `cpt-cf-chat-engine-flow-plugin-system-resolve`
- `cpt-cf-chat-engine-algo-plugin-system-store-config`

**Touches**:
- DB: `plugin_configs`
- Entities: `PluginConfig`

### N:1 Session Type to Plugin Relationship

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-plugin-system-n1-relationship`

The system **MUST** support multiple session types sharing the same `plugin_instance_id` with different `plugin_configs` entries. A single plugin instance serves multiple session types, differentiated by the `plugin_config` passed in each call context.

**Implements**:
- `cpt-cf-chat-engine-flow-plugin-system-resolve`
- `cpt-cf-chat-engine-algo-plugin-system-resolve`

**Touches**:
- DB: `plugin_configs`, `session_types`
- Entities: `PluginConfig`, `SessionType`

### Webhook-Compat First-Party Plugin

- [ ] `p1` - **ID**: `cpt-cf-chat-engine-dod-plugin-system-webhook-compat`

The system **MUST** ship a first-party `webhook-compat` plugin that implements `ChatEngineBackendPlugin` by forwarding trait method calls to legacy HTTP webhook endpoints. The plugin owns all transport concerns: HTTP client, auth (Bearer, API key, mTLS), retry, timeout, and resilience patterns. Chat Engine core contains zero webhook or HTTP client logic.

**Implements**:
- `cpt-cf-chat-engine-flow-plugin-system-webhook-compat`

**Touches**:
- Plugin: `webhook-compat`
- DB: `plugin_configs` (webhook_url, auth config stored in config JSONB)
- Entities: `ChatEngineBackendPlugin` (implementation)

### Plugin Health Check

- [ ] `p2` - **ID**: `cpt-cf-chat-engine-dod-plugin-system-health-check`

The system **MUST** support an optional `health_check()` method on `ChatEngineBackendPlugin`. When a session type is configured, the system may call `health_check()` to verify plugin availability. The method returns a `HealthStatus` with three possible values:

| Value | Chat Engine reaction |
|-------|----------------------|
| `Healthy` | Plugin is fully operational. No action. |
| `Degraded` | Plugin is reachable but reporting partial degradation. Logged at WARN; session type configuration is **not** blocked. Treated as available for routing — degradation is the plugin's signal that operators should investigate, but traffic still flows. |
| `Unhealthy` | Plugin is unreachable or reporting failure. Logged at WARN; session type configuration is **not** blocked. Downstream `on_session_*` / `on_message_*` calls are expected to fail with `PluginError::Transient` until the plugin recovers. |

A `health_check()` call that itself returns `Err(PluginError)` is logged as a warning and treated equivalently to `Unhealthy` for routing purposes — it does not block session type configuration. The default implementation in `chat-engine-sdk` returns `Ok(HealthStatus::Healthy)`.

**Implements**:
- `cpt-cf-chat-engine-algo-plugin-system-validate-availability`

**Touches**:
- Entities: `ChatEngineBackendPlugin` (health_check method), `HealthStatus`

## 6. Acceptance Criteria

- [ ] All `ChatEngineBackendPlugin` implementations are discovered and registered via ClientHub at startup; each is resolvable by its `plugin_instance_id`
- [ ] A plugin that fails initialization does not prevent other plugins from registering; the failure is logged with the `plugin_instance_id`
- [ ] Session type configuration with a non-existent `plugin_instance_id` is rejected (plugin not found in registry)
- [ ] Plugin config is persisted in `plugin_configs` with composite PK `(plugin_instance_id, session_type_id)` and forwarded to the plugin in every call context
- [ ] Multiple session types can reference the same `plugin_instance_id` with different configs; the plugin receives the correct config for each session type
- [ ] The `webhook-compat` plugin forwards trait method calls to legacy HTTP webhook endpoints; Chat Engine core contains no HTTP client or webhook logic
- [ ] Plugin errors are isolated per session type: a failing plugin does not affect sessions using other plugins or other session types

## 7. Non-Functional Considerations

- **Performance**: Plugin resolution via ClientHub is O(1) by `plugin_instance_id`. Plugin config lookup adds one DB query per invocation (cacheable at application layer if needed). Trait method dispatch has negligible overhead compared to plugin execution time.
- **Security**: Plugin config JSONB may contain sensitive data (API keys, service URLs). Chat Engine treats it as opaque and never logs its contents. Session type configuration and plugin config management are restricted to developer/admin actors.
- **Reliability**: Plugin failures are isolated per session type. A failing plugin does not affect other session types or other plugins. Plugins own their own resilience patterns (circuit breaker, retry, timeout) for outbound communication.
- **Data**: Composite PK `(plugin_instance_id, session_type_id)` on `plugin_configs` ensures uniqueness. FK on `session_type_id` cascades deletes when a session type is removed.
- **Observability**: Structured log events for plugin registration (success/failure), plugin resolution, and plugin invocation with `trace_id`, `plugin_instance_id`, `session_type_id`, `method_name`, `duration_ms`.
- **Compliance / UX / Business**: Not applicable -- internal infrastructure feature; see session-lifecycle section 7.
