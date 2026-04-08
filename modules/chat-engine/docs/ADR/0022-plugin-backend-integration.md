Created:  2026-02-23 by Constructor Tech
Updated:  2026-03-09 by Constructor Tech
# ADR-0022: Internal Plugin Interface for Backend Integration


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: HTTP webhooks (former approach)](#option-1-http-webhooks-former-approach)
  - [Option 2: Internal plugin trait (pure)](#option-2-internal-plugin-trait-pure)
  - [Option 3: Hybrid (chosen)](#option-3-hybrid-chosen)
- [Plugin API Contract](#plugin-api-contract)
- [N:1 Session Types → Plugin](#n1-session-types--plugin)
  - [Plugin Config Entity](#plugin-config-entity)
- [Related Design Elements](#related-design-elements)

<!-- /toc -->

**Date**: 2026-02-23

**Status**: accepted — supersedes former ADR "Synchronous HTTP Webhooks with Streaming", "Circuit Breaker per Webhook Backend", "Per-Session-Type Timeout Configuration" (removed)

**ID**: `cpt-cf-chat-engine-adr-plugin-backend-integration`

## Context and Problem Statement

The former "Synchronous HTTP Webhooks" ADR established HTTP webhooks as the integration mechanism between Chat Engine and message-processing backends: Chat Engine made outbound HTTP POST requests to a `webhook_url` configured per session type and handled all resilience concerns (auth, retry, circuit breaker, timeout) itself. This approach couples Chat Engine to transport-level details and forces every deployment to duplicate resilience infrastructure.

In practice, backend integrations are implemented as **code modules within Chat Engine** — not as independently deployed external services. A plugin is a Rust module inside the Chat Engine codebase that implements the `ChatEngineBackendPlugin` trait. The plugin decides how to communicate with external services (HTTP, gRPC, vector DB, etc.); Chat Engine is not involved in that transport. How should Chat Engine integrate with backend plugins while keeping its core free of transport, auth, and resilience logic?

## Decision Drivers

* Chat Engine core must not contain transport, auth, retry, or circuit breaker logic
* Plugins are internal code within Chat Engine — a plugin is a trait implementation, not an external service
* Each plugin independently decides how to communicate with external services it depends on
* Request/response format between a plugin and its external service must conform to Chat Engine schemas
* Session type configuration must reference a specific plugin by `plugin_instance_id`
* A compatibility path for legacy HTTP webhook-based services must exist without changes to Chat Engine core

## Considered Options

* **Option 1: HTTP webhooks (former approach)** — Chat Engine makes outbound HTTP POST to `webhook_url` per session type; manages auth, retry, circuit breaker itself
* **Option 2: Internal plugin trait** — plugins are code inside Chat Engine implementing `ChatEngineBackendPlugin`; Chat Engine calls trait methods directly; each plugin manages its own outbound communication
* **Option 3: Hybrid** — internal plugin trait as primary; a first-party `webhook-compat` plugin wraps legacy HTTP webhook-based external services

## Decision Outcome

Chosen option: **Option 3 (Hybrid)**, with the internal plugin interface as the primary integration mechanism.

Chat Engine defines a `ChatEngineBackendPlugin` trait. Plugins are Rust modules inside the Chat Engine codebase that implement this trait and are registered in Chat Engine's internal plugin registry at startup. A session type references its plugin via `plugin_instance_id` (a GTS ID string). On each operation, Chat Engine looks up the plugin by `plugin_instance_id` and calls the appropriate trait method.

Plugins own all outbound communication — each plugin decides how to reach its external dependencies (HTTP, gRPC, direct DB, etc.). Chat Engine only calls trait methods; it never interprets transport details, auth tokens, or resilience strategies. The first-party `webhook-compat` plugin wraps legacy HTTP webhook endpoints, keeping Chat Engine core free of any webhook logic.

### Consequences

* Good, because Chat Engine core has zero auth, retry, circuit breaker, or timeout code
* Good, because plugin-to-external-service communication is fully encapsulated in the plugin
* Good, because request/response format is governed by Chat Engine schemas — plugins cannot break the contract
* Good, because `Session.enabled_capabilities` is populated by the plugin via `on_session_created`, ensuring capabilities are fresh at session creation time
* Good, because the `webhook-compat` plugin preserves backward compatibility for existing HTTP webhook services
* Bad, because adding a new plugin requires a code change and rebuild of Chat Engine
* Bad, because the `webhook-compat` plugin adds a thin indirection layer for legacy webhook services

### Confirmation

Confirmed when:

- A session type configured with `plugin_instance_id` calls the correct plugin trait method on each operation
- `on_session_type_configured` is called on session type setup; plugins may return capabilities or defer resolution to session creation
- `on_session_created` returns `Vec<Capability>` stored as `Session.enabled_capabilities`
- `on_session_updated` returns `Vec<Capability>` that overwrites `Session.enabled_capabilities`
- `on_message` receives call context (including `plugin_config`) and streams response back through Chat Engine
- Plugin configuration is stored in `plugin_configs` table, separate from `session_types`
- `webhook-compat` plugin wraps a legacy HTTP webhook service without any changes to Chat Engine core

## Pros and Cons of the Options

### Option 1: HTTP webhooks (former approach)

Chat Engine makes outbound HTTP POST to `webhook_url` per session type and manages all resilience logic.

* Good, because any HTTP server can serve as a backend without code inside Chat Engine
* Bad, because Chat Engine must implement and maintain auth, retry, circuit breaker, and timeout per session type
* Bad, because `webhook_url` is an unversioned string with no schema enforcement

### Option 2: Internal plugin trait (pure)

Plugins are code inside Chat Engine; no compatibility mechanism for external HTTP services.

* Good, because zero transport boilerplate in Chat Engine core
* Good, because trait interface is type-safe and schema-enforced at compile time
* Bad, because legacy webhook-based services cannot integrate without being rewritten as plugins

### Option 3: Hybrid (chosen)

Internal plugin trait as primary; `webhook-compat` plugin wraps legacy HTTP webhook services.

* Good, because Chat Engine core stays transport-free
* Good, because legacy HTTP webhook services remain supported via the compat plugin
* Good, because plugin is authoritative for capabilities, schemas, and resilience strategy
* Bad, because new plugins require a Chat Engine code change and rebuild

## Plugin API Contract

Chat Engine defines the `ChatEngineBackendPlugin` trait in the `chat-engine-sdk` crate. Plugin methods:

| Method | Trigger | Returns |
|--------|---------|---------|
| `on_session_type_configured` | Session type is configured with this plugin | `Vec<Capability>` stored as `SessionType.available_capabilities` (may be empty if plugin defers resolution to session creation) |
| `on_session_created` | Session is created | `Vec<Capability>` stored as `Session.enabled_capabilities` — plugin resolves capabilities at session creation time |
| `on_session_updated` | User updates session capabilities | `Vec<Capability>` stored as `Session.enabled_capabilities` — plugin re-resolves capabilities based on the changed values |
| `on_message` | User sends a message | `ResponseStream` of content chunks |
| `on_message_recreate` | User recreates a message | `ResponseStream` of content chunks |
| `on_session_summary` | Summarization triggered | `ResponseStream` of summary content |
| `health_check` | Optional liveness probe | Health status |

Full trait and context struct definitions are in `chat-engine-sdk` and documented in DESIGN.md §3.3.2.

## N:1 Session Types → Plugin

Multiple session types can share the same `plugin_instance_id`. Plugin configuration is stored in a separate `plugin_configs` entity keyed by `(plugin_instance_id, session_type_id)`. This decouples plugin settings from the session type entity while preserving per-session-type configuration — a single plugin instance can serve multiple session types with different behaviour (e.g., different summarization strategy, different processing parameters).

The plugin config is forwarded to the plugin in every call context (`plugin_config` field). Chat Engine treats the config as an opaque JSONB blob — only the plugin interprets its contents.

### Plugin Config Entity

| Column | Type | Description |
|--------|------|-------------|
| plugin_instance_id | VARCHAR | Plugin instance identifier (composite PK) |
| session_type_id | UUID FK | References session_types (composite PK) |
| config | JSONB | Plugin-specific configuration — opaque to Chat Engine |
| created_at | TIMESTAMPTZ | Creation timestamp |
| updated_at | TIMESTAMPTZ | Last modification timestamp |

## Related Design Elements

**Actors**:

* `cpt-cf-chat-engine-actor-backend-plugin` — internal plugin code within Chat Engine; implements `ChatEngineBackendPlugin` trait

**Requirements**:

* `cpt-cf-chat-engine-fr-send-message` — plugin `on_message` call replaces webhook POST
* `cpt-cf-chat-engine-fr-create-session` — plugin `on_session_created` call replaces session.created webhook event
* `cpt-cf-chat-engine-fr-schema-extensibility` — plugin registers GTS derived schemas at startup

**Superseded ADRs** (removed):

* Former "Synchronous HTTP Webhooks with Streaming" — `webhook_url` replaced by `plugin_instance_id`
* Former "Circuit Breaker per Webhook Backend" — responsibility moved to plugin
* Former "Per-Session-Type Timeout Configuration" — responsibility moved to plugin

**Related ADRs**:

* ADR-0003 (Streaming Architecture) — unchanged; plugin provides `ResponseStream`
* ADR-0009 (Stateless Scaling) — unchanged; plugin resolved per-request from registry
* ADR-0023 (LLM Gateway Plugin) — first concrete plugin implementation