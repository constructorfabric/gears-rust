# AuthN Resolver

Authentication resolution for CyberFabric — validates bearer tokens and produces a `SecurityContext`.

## Overview

The authorization design that CyberFabric operates on is described in [DESIGN.md](../../../docs/arch/authorization/DESIGN.md).
The decision to split authentication and authorization into separate resolvers is documented in
[ADR-0002](../../../docs/arch/authorization/adr/0002-split-authn-and-authz-resolvers.md) and
the minimalist interface choice in [ADR-0003](../../../docs/arch/authorization/adr/0003-authn-resolver-minimalist-interface.md).

The **authn_resolver** module provides a single responsibility: convert a bearer token into a validated identity (`SecurityContext`). The resolver is a integration point — it discovers and delegates to a vendor-specific plugin via the types-registry. The actual token validation logic lives in the plugin.

## Public API

The module registers [`AuthNResolverClient`](authn-resolver-sdk/src/api.rs) in ClientHub:

- `authenticate(bearer_token)` — Validate bearer token and return `AuthenticationResult`

### `AuthenticationResult`

Contains a `SecurityContext` with:
- `subject_id` — Authenticated user/service identity
- `subject_type` — Optional GTS type identifier
- `subject_tenant_id` — Home tenant of the subject (required)
- `token_scopes` — Permission scopes (`["*"]` for first-party apps)
- `bearer_token` — Optionally preserved for downstream PDP forwarding

### Errors

See [`error.rs`](authn-resolver-sdk/src/error.rs): `Unauthorized`, `NoPluginAvailable`, `ServiceUnavailable`, `Internal`

## Plugin API

Plugins implement [`AuthNResolverPluginClient`](authn-resolver-sdk/src/plugin_api.rs) and register via GTS.

CyberFabric includes one plugin out of the box:
- [`static_authn_plugin`](plugins/static-authn-plugin/) — Config-based plugin for development and testing

## Configuration

### AuthN Resolver Module

See [`config.rs`](authn-resolver/src/config.rs)

```yaml
modules:
  authn_resolver:
    vendor: "cyberfabric"  # Selects plugin by matching vendor
```

### Static AuthN Plugin

See [`config.rs`](plugins/static-authn-plugin/src/config.rs)

```yaml
modules:
  static_authn_plugin:
    vendor: "cyberfabric"
    priority: 100
    mode: accept_all          # accept_all | static_tokens
    default_identity:
      subject_id: "00000000-0000-0000-0000-000000000001"
      subject_tenant_id: "00000000-0000-0000-0000-000000000001"
      token_scopes: ["*"]
    tokens: []                # Used in static_tokens mode
```

**Modes:**
- **`accept_all`** — Accepts any non-empty token, returns the default identity (development convenience)
- **`static_tokens`** — Maps specific tokens to specific identities; returns `Unauthorized` on mismatch

## Usage

```rust
// API Gateway middleware
let authn = hub.get::<dyn AuthNResolverClient>()?;

let result = authn.authenticate("Bearer abc123").await?;
// result.security_context contains validated identity
```

## Usage with API Gateway

The API Gateway consumes `AuthNResolverClient` in its authentication middleware. When `auth_disabled: true` is set in the gateway config, a default `SecurityContext` is injected without calling the resolver (development convenience).

## Implementation Phases

### Phase 1: Core (Current)

- Single `authenticate` API
- Plugin discovery via types-registry
- Static plugin with `accept_all` and `static_tokens` modes
- ClientHub registration for in-process consumption

### Phase 2: JWT/OIDC Plugin (Planned)

- JWKS-based token validation
- Standard OIDC claims mapping to `SecurityContext`
