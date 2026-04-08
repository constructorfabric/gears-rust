---
status: accepted
date: 2026-02-09
decision-makers: OAGW Team
---

# Plugin System — Three Plugin Types with Trait-Based Extensibility


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Plugin Types](#plugin-types)
  - [Plugin Traits](#plugin-traits)
  - [Execution Order](#execution-order)
  - [Built-in Plugins](#built-in-plugins)
  - [External Plugins](#external-plugins)
  - [Plugin Loading](#plugin-loading)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Three plugin types with separate traits](#three-plugin-types-with-separate-traits)
  - [Single generic Extension trait](#single-generic-extension-trait)
  - [Starlark-only interpreted plugins](#starlark-only-interpreted-plugins)
- [Appendix A: Starlark Custom Plugin Examples](#appendix-a-starlark-custom-plugin-examples)
  - [Custom Guard Plugin](#custom-guard-plugin)
  - [Custom Transform Plugin — PII Redactor](#custom-transform-plugin--pii-redactor)
  - [Custom Transform Plugin — Path Rewriter](#custom-transform-plugin--path-rewriter)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-plugin-system`

## Context and Problem Statement

OAGW needs extensibility for request/response processing. Different use cases require different behaviors: authentication (API key, OAuth2, JWT), validation (timeouts, CORS, rate limiting), and transformation (logging, metrics, request ID). The question is how to structure the plugin system to support both built-in and external plugins with clear boundaries.

## Decision Drivers

* Clear trait boundaries for each plugin purpose
* Same traits for built-in and external plugins (no special-casing)
* ModKit integration for external plugins
* Native Rust performance (no WASM overhead for MVP)
* Compile-time type safety for built-in plugins
* Deterministic execution order

## Considered Options

* Three plugin types with separate traits (Auth, Guard, Transform)
* Single generic Extension trait for all purposes
* Starlark-only interpreted plugins

## Decision Outcome

Chosen option: "Three plugin types with separate traits", because it provides clear semantic boundaries, type safety, and deterministic execution order.

### Plugin Types

**AuthPlugin** (`gts.x.core.oagw.auth_plugin.v1~*`): Injects authentication credentials. Executed once per request, before guards. Examples: API key, OAuth2, Bearer token, Basic auth.

**GuardPlugin** (`gts.x.core.oagw.guard_plugin.v1~*`): Validates requests and enforces policies (can reject). Executed after auth, before transform. Examples: Timeout enforcement, CORS validation, rate limiting.

**TransformPlugin** (`gts.x.core.oagw.transform_plugin.v1~*`): Modifies request/response/error data. Executed before and after proxy call. Examples: Logging, metrics collection, request ID propagation.

### Plugin Traits

```rust
// oagw-sdk/src/plugins.rs

#[async_trait]
pub trait AuthPlugin: Send + Sync {
    fn id(&self) -> &str;
    fn plugin_type(&self) -> &str;
    async fn authenticate(&self, ctx: &mut RequestContext) -> Result<()>;
}

#[async_trait]
pub trait GuardPlugin: Send + Sync {
    fn id(&self) -> &str;
    fn plugin_type(&self) -> &str;
    async fn guard_request(&self, ctx: &RequestContext) -> Result<GuardDecision>;
    async fn guard_response(&self, ctx: &ResponseContext) -> Result<GuardDecision>;
}

#[async_trait]
pub trait TransformPlugin: Send + Sync {
    fn id(&self) -> &str;
    fn plugin_type(&self) -> &str;
    async fn transform_request(&self, ctx: &mut RequestContext) -> Result<()>;
    async fn transform_response(&self, ctx: &mut ResponseContext) -> Result<()>;
    async fn transform_error(&self, ctx: &mut ErrorContext) -> Result<()>;
}
```

### Execution Order

```text
Incoming Request
  → Auth Plugin (credential injection)
  → Guard Plugins (validation, can reject)
  → Transform Plugins (modify request)
  → HTTP call to external service
  → Transform Plugins (modify response)
  → Return to client
```

Upstream plugins execute before route plugins. Plugin definitions are immutable after creation; updates are performed by creating a new plugin version and re-binding references.

### Built-in Plugins

Included in `oagw` crate (`infra/plugin/`):

**Auth Plugins**:

- `ApiKeyAuthPlugin`: API key injection (header/query)
- `BasicAuthPlugin`: HTTP Basic authentication
- `BearerTokenAuthPlugin`: Bearer token injection
- `OAuth2ClientCredPlugin`: OAuth2 client credentials flow

**Guard Plugins**:

- `TimeoutGuardPlugin`: Request timeout enforcement
- `CorsGuardPlugin`: CORS preflight validation
- `RateLimitGuardPlugin`: Rate limiting (token bucket)

**Transform Plugins**:

- `LoggingTransformPlugin`: Request/response logging
- `MetricsTransformPlugin`: Prometheus metrics collection
- `RequestIdTransformPlugin`: X-Request-ID propagation

### External Plugins

Separate ModKit modules implementing plugin traits from `oagw-sdk`:

```rust
// cf-oagw-plugin-oauth2-pkce/src/lib.rs

pub struct OAuth2PkceAuthPlugin {
    // ...
}

#[async_trait]
impl AuthPlugin for OAuth2PkceAuthPlugin {
    fn id(&self) -> &str { "oauth2-pkce" }
    fn plugin_type(&self) -> &str {
        "gts.x.core.oagw.auth_plugin.v1~custom.oauth2.oagw.pkce.v1"
    }
    async fn authenticate(&self, ctx: &mut RequestContext) -> Result<()> {
        // Custom OAuth2 PKCE flow
    }
}
```

### Plugin Loading

Data Plane loads and registers all plugins during initialization:

```rust
pub struct ControlPlane {
    auth_plugins: HashMap<String, Arc<dyn AuthPlugin>>,
    guard_plugins: HashMap<String, Arc<dyn GuardPlugin>>,
    transform_plugins: HashMap<String, Arc<dyn TransformPlugin>>,
}

impl ControlPlane {
    pub fn new(external_plugins: Vec<Box<dyn AuthPlugin>>) -> Self {
        let mut auth_plugins = HashMap::new();

        // Register built-in plugins
        auth_plugins.insert("apikey".into(), Arc::new(ApiKeyAuthPlugin));
        auth_plugins.insert("basic".into(), Arc::new(BasicAuthPlugin));

        // Register external plugins from modkit
        for plugin in external_plugins {
            auth_plugins.insert(plugin.id().to_string(), Arc::from(plugin));
        }

        Self { auth_plugins, /* ... */ }
    }
}
```

### Consequences

* Good, because extensibility without modifying OAGW core
* Good, because built-in plugins have zero overhead (native code)
* Good, because external plugins integrate via ModKit (standard pattern)
* Good, because clear execution order and lifecycle
* Bad, because external plugins require Rust implementation (no scripting languages yet)
* Bad, because plugin changes require recompilation (acceptable for MVP)

### Confirmation

Code review confirms: `AuthPlugin`, `GuardPlugin`, and `TransformPlugin` traits defined in `oagw-sdk/src/plugins.rs`. Built-in implementations in `infra/plugin/`. External plugins register via ModKit dependency injection.

## Pros and Cons of the Options

### Three plugin types with separate traits

Separate `AuthPlugin`, `GuardPlugin`, `TransformPlugin` traits with specific method signatures.

* Good, because each type has clear purpose and lifecycle
* Good, because compile-time type safety for plugin implementations
* Good, because deterministic execution order (Auth → Guard → Transform)
* Bad, because three traits to maintain instead of one

### Single generic Extension trait

One `Extension` trait with generic hooks for all purposes.

* Good, because simpler trait hierarchy
* Bad, because loses type safety and clear semantics
* Bad, because execution ordering becomes configuration-dependent
* Bad, because harder to reason about plugin interactions

### Starlark-only interpreted plugins

All plugins as interpreted Starlark scripts.

* Good, because no recompilation for plugin changes
* Good, because sandboxed execution
* Bad, because too slow for hot path operations (auth, guards)
* Bad, because limited expressiveness for complex auth flows

## Appendix A: Starlark Custom Plugin Examples

### Custom Guard Plugin

**Definition**:

```json
{
  "id": "gts.x.core.oagw.guard_plugin.v1~550e8400-e29b-41d4-a716-446655440000",
  "tenant_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "name": "request_validator",
  "description": "Validates request headers and body size",
  "plugin_type": "guard",
  "config_schema": {
    "type": "object",
    "properties": {
      "max_body_size": { "type": "integer", "default": 1048576 },
      "required_headers": { "type": "array", "items": { "type": "string" } }
    }
  },
  "source_code": "..."
}
```

**Source** (fetched via `GET /api/oagw/v1/plugins/{id}/source`):

```starlark
def on_request(ctx):
    # Guards only implement on_request phase
    for h in ctx.config.get("required_headers", []):
        if not ctx.request.headers.get(h):
            return ctx.reject(400, "MISSING_HEADER", "Required header: " + h)

    if len(ctx.request.body) > ctx.config.get("max_body_size", 1048576):
        return ctx.reject(413, "BODY_TOO_LARGE", "Body exceeds limit")

    return ctx.next()
```

### Custom Transform Plugin — PII Redactor

**Definition**:

```json
{
  "id": "gts.x.core.oagw.transform_plugin.v1~6ba7b810-9dad-11d1-80b4-00c04fd430c8",
  "tenant_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "name": "redact_pii",
  "description": "Redacts PII fields from response",
  "plugin_type": "transform",
  "phases": [ "on_response" ],
  "config_schema": {
    "type": "object",
    "properties": {
      "fields": { "type": "array", "items": { "type": "string" } }
    }
  },
  "source_code": "..."
}
```

**Source**:

```starlark
def on_response(ctx):
    # Redact PII fields from JSON response
    data = ctx.response.json()
    for field in ctx.config.get("fields", []):
        if field in data:
            data[field] = "[REDACTED]"
    ctx.response.set_json(data)
    return ctx.next()
```

### Custom Transform Plugin — Path Rewriter

**Definition**:

```json
{
  "id": "gts.x.core.oagw.transform_plugin.v1~8f8e8400-e29b-41d4-a716-446655440001",
  "tenant_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7",
  "name": "path_rewriter",
  "description": "Rewrites request paths and adds API version",
  "plugin_type": "transform",
  "phases": [ "on_request" ],
  "config_schema": {
    "type": "object",
    "properties": {
      "path_prefix": { "type": "string" },
      "add_api_version": { "type": "boolean" }
    }
  },
  "source_code": "..."
}
```

**Source**:

```starlark
def on_request(ctx):
    # Transform path: prepend custom prefix
    prefix = ctx.config.get("path_prefix", "")
    if prefix:
        new_path = prefix + ctx.request.path
        ctx.request.set_path(new_path)
        ctx.log.info("Rewrote path", {"old": ctx.request.path, "new": new_path})

    # Transform query: add API version if configured
    if ctx.config.get("add_api_version", False):
        ctx.request.add_query("api_version", "2024-01")

    # Transform query: remove internal parameters
    query = ctx.request.query
    if "internal_debug" in query:
        del query["internal_debug"]
        ctx.request.set_query(query)

    return ctx.next()
```

## More Information

Future enhancements:
- **Starlark plugins** (p3): For simple transforms that don't need native performance
- **WASM plugins** (p3): For sandboxed untrusted code

Circuit breaker is a core gateway resilience capability (configured as core policy), not a plugin.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related ADR**: [ADR: Component Architecture](./0001-component-architecture.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-plugin-system` — Plugin system architecture with three types
* `cpt-cf-oagw-fr-builtin-plugins` — Built-in plugin implementations
* `cpt-cf-oagw-fr-auth-injection` — Auth plugins handle credential injection
* `cpt-cf-oagw-fr-rate-limiting` — Guard plugins enforce rate limits
