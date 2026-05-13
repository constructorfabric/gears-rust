# cf-chat-engine-sdk

SDK crate for the **chat-engine** module: plugin traits, shared models, and error types used by backend plugin implementations.

The Chat Engine module is a multi-tenant conversational infrastructure with a plugin-driven backend. Chat Engine owns session state, message trees, streaming, and routing — but **zero business logic**. All message processing is delegated to backend plugins that implement the `ChatEngineBackendPlugin` trait defined in this crate.

## Purpose

This crate is the **contract** between Chat Engine and plugin authors. It is intentionally minimal:

- No HTTP, database, or framework dependencies
- Only `async-trait`, `thiserror`, `uuid`, `time`, `serde`, `serde_json`
- Stable API that plugin implementations compile against

## Installation

```toml
[dependencies]
chat-engine-sdk = { package = "cf-chat-engine-sdk", version = "0.1.0" }
async-trait = "0.1"
```

## Core trait: `ChatEngineBackendPlugin`

Plugins implement this trait to hook into the Chat Engine lifecycle:

```rust
use async_trait::async_trait;
use chat_engine_sdk::{
    stream_from_events, Capability, ChatEngineBackendPlugin, HealthStatus,
    MessagePluginCtx, PluginError, PluginStream, SessionPluginCtx, StreamingChunkEvent,
    StreamingCompleteEvent, StreamingEvent, StreamingStartEvent,
};
use uuid::Uuid;

pub struct MyPlugin {
    instance_id: String,
}

#[async_trait]
impl ChatEngineBackendPlugin for MyPlugin {
    async fn on_session_type_configured(
        &self,
        ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        // Validate plugin_config; return supported capabilities.
        Ok(vec![])
    }

    async fn on_session_created(
        &self,
        ctx: SessionPluginCtx,
    ) -> Result<Vec<Capability>, PluginError> {
        // Resolve capabilities for this session (e.g., available models).
        Ok(vec![])
    }

    async fn on_message(
        &self,
        ctx: MessagePluginCtx,
    ) -> Result<PluginStream, PluginError> {
        // Emit a well-formed stream: Start -> Chunk(s) -> Complete.
        let events = vec![
            StreamingEvent::Start(StreamingStartEvent {
                message_id: ctx.message_id,
            }),
            StreamingEvent::Chunk(StreamingChunkEvent {
                message_id: ctx.message_id,
                chunk: "hello from my plugin".into(),
            }),
            StreamingEvent::Complete(StreamingCompleteEvent {
                message_id: ctx.message_id,
                metadata: None,
            }),
        ];
        Ok(stream_from_events(events))
    }

    async fn on_message_recreate(
        &self,
        ctx: MessagePluginCtx,
    ) -> Result<PluginStream, PluginError> {
        // Regenerate: same shape as on_message — delegate.
        self.on_message(ctx).await
    }

    async fn on_session_summary(
        &self,
        ctx: SessionPluginCtx,
    ) -> Result<PluginStream, PluginError> {
        let summary_id = ctx.session_id.unwrap_or_else(Uuid::new_v4);
        let events = vec![
            StreamingEvent::Start(StreamingStartEvent { message_id: summary_id }),
            StreamingEvent::Chunk(StreamingChunkEvent {
                message_id: summary_id,
                chunk: "session summary".into(),
            }),
            StreamingEvent::Complete(StreamingCompleteEvent {
                message_id: summary_id,
                metadata: None,
            }),
        ];
        Ok(stream_from_events(events))
    }

    async fn health_check(&self) -> Result<HealthStatus, PluginError> {
        Ok(HealthStatus::Healthy)
    }

    fn plugin_instance_id(&self) -> &str {
        &self.instance_id
    }
}
```

All trait methods have **default no-op implementations** — override only the hooks you need.

## Lifecycle hooks

| Method | When Chat Engine calls it |
|--------|--------------------------|
| `on_session_type_configured` | A developer registers a new session type bound to this plugin |
| `on_session_created` | A client creates a new session of this plugin's session type |
| `on_session_updated` | Session metadata or session-type changes mid-session |
| `on_message` | A user sends a new message to a session |
| `on_message_recreate` | A user requests regeneration of an existing assistant message (new variant) |
| `on_session_summary` | Session summary is requested or context overflow triggers summarization |
| `health_check` | Chat Engine polls plugin readiness |

## Domain types

Re-exported at crate root:

**Entities**
- `Session` — session record (tenant, user, lifecycle state, metadata)
- `SessionType` — registered session type referencing a plugin
- `Message` — message node in the immutable conversation tree
- `MessageRole` — `User` | `Assistant` | `System`
- `Capability`, `CapabilityValue` — capability model for sessions
- `VariantInfo` — variant metadata (index, total, is_active)

**Configuration enums**
- `MemoryStrategy` — `Full` | `SlidingWindow { window_size }` | `Summarized { recent_messages_to_keep }`
- `RetentionPolicy` — `None` | `AgeBased { max_age_days }` | `CountBased { max_message_count }`

**Streaming**
- `StreamingEvent` — tagged union: `Start` | `Chunk` | `Complete` | `Error`
- `StreamingStartEvent`, `StreamingChunkEvent`, `StreamingCompleteEvent`, `StreamingErrorEvent`

**Plugin call contexts**
- `PluginCallContext` — tenant, user, plugin instance id, session type, plugin config, enabled capabilities
- `SessionPluginCtx` — session-scoped call wrapping `PluginCallContext`
- `MessagePluginCtx` — message-scoped call including message history

**Health**
- `HealthStatus` — `Healthy` | `Degraded` | `Unhealthy`

## Error model

```rust
pub enum PluginError {
    Transient(String),   // retryable (network blip, temporary upstream error)
    Permanent(String),   // non-retryable (invalid input, auth failure)
    Timeout(String),     // upstream timeout
    Internal(String),    // bug or unexpected state
}
```

Chat Engine inspects the variant to decide whether to retry, surface the error to the client, or circuit-break.

## Design principles

- **Backend authority** — Chat Engine stays out of business logic; plugins own response generation
- **Immutable tree** — messages are never mutated; variants are new siblings
- **Zero business logic in the SDK** — the SDK contains only data types and the plugin trait
- **Stateless-friendly** — all state passed through context structs; plugins should not hold per-session state unless they own it

## Examples in the tree

Two first-party plugin implementations live in the main `chat-engine` crate and serve as reference implementations:

- `chat_engine::infra::webhook_compat::WebhookCompatPlugin` — forwards events to legacy HTTP webhook backends
- `chat_engine::infra::llm_gateway::LlmGatewayPlugin` — integrates with the internal LLM Gateway service

## License

Same as the parent workspace.
