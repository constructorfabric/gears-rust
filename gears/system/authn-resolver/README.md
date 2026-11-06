# AuthN Resolver

Authentication resolution for Gears — validates bearer tokens and produces a `SecurityContext`.

## Overview

The authorization design that Gears operate on is described in [DESIGN.md](../../../docs/arch/authorization/DESIGN.md).
The decision to split authentication and authorization into separate resolvers is documented in
[ADR-0002](../../../docs/arch/authorization/ADR/0002-split-authn-authz-resolvers.md) and
the minimalist interface choice in [ADR-0003](../../../docs/arch/authorization/ADR/0003-authn-resolver-minimalist-interface.md).

The **authn_resolver** gear provides a single responsibility: convert a bearer token or service credentials into a validated identity (`SecurityContext`). The resolver is an integration point — it discovers and delegates to a vendor-specific plugin via the types-registry. The actual token validation logic lives in the plugin.

## Public API

The gear registers [`AuthNResolverClient`](authn-resolver-sdk/src/api.rs) in ClientHub:

- `authenticate(bearer_token)` — Validate a bearer token and return `AuthenticationResult`
- `exchange_client_credentials(request)` — Obtain a token and validated identity for S2S work

### `AuthenticationResult`

Contains a `SecurityContext` with:
- `subject_id` — Authenticated user/service identity
- `subject_type` — Optional GTS type identifier
- `subject_tenant_id` — Home tenant of the subject (required)
- `token_scopes` — Permission scopes (`["*"]` for first-party apps)
- `bearer_token` — Optionally preserved for downstream PDP forwarding
- `expires_at` — Optional verified/exchanged credential expiry

The result also carries the plugin's reuse policy in `cache_control`; NoStore is the default.

### Errors

See [`error.rs`](authn-resolver-sdk/src/error.rs): `Unauthorized`, `NoPluginAvailable`, `ServiceUnavailable` (including unavailable IdP dependencies), `TokenAcquisitionFailed`, `Internal`

## Plugin API

Plugins implement [`AuthNResolverPluginClient`](authn-resolver-sdk/src/plugin_api.rs) and register via GTS.

Gears include one plugin out of the box:
- [`static_authn_plugin`](plugins/static-authn-plugin/) — Config-based plugin for development and testing

## Configuration

### AuthN Resolver Gear

See [`config.rs`](authn-resolver/src/config.rs)

```yaml
gears:
  authn_resolver:
    vendor: "constructorfabric"  # Selects plugin by matching vendor
```

### Static AuthN Plugin

See [`config.rs`](plugins/static-authn-plugin/src/config.rs)

```yaml
gears:
  static_authn_plugin:
    vendor: "constructorfabric"
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

// Raw token without the `Bearer ` prefix; `new` always revalidates.
let request = AuthenticationRequest::new("abc123");
let result = authn.authenticate(&request).await?;
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

## Cached client credentials

Use `authn_resolver_sdk::ClientCredentialsProvider::builder(...)` for client-side S2S caching. See the [SDK example](authn-resolver-sdk/README.md#cached-s2s-credentials) and [design](../../../docs/arch/authorization/DESIGN.md#s2s-caching-provider).
