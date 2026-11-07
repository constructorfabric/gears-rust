# AuthN Resolver

Main gear for authentication in Gears. Discovers AuthN plugins via GTS types-registry and routes token validation to the selected plugin.

## Overview

The `cf-gears-authn-resolver` gear provides:

- **Plugin discovery** — Finds AuthN plugins via GTS types-registry
- **Vendor-based selection** — Selects plugin by vendor and priority
- **Token validation routing** — Delegates bearer token authentication to the active plugin
- **ClientHub integration** — Registers `AuthNResolverClient` for inter-gear use

This is a **main gear** — it contains no authentication logic itself. All operations are delegated to the active plugin (e.g., `cf-gears-static-authn-plugin` for development, or a custom OIDC/JWT implementation).

## Architecture

```
API Gateway (middleware)
    │
    ▼
AuthNResolverClient  (SDK trait, registered in ClientHub)
    │
    ▼
authn-resolver gateway  (this crate — discovers & routes)
    │
    ▼
AuthNResolverPluginClient  (SDK trait, scoped by GTS instance ID)
    │
    ▼
Plugin implementation  (validates tokens, returns SecurityContext)
```

## Usage

The primary consumer is the API Gateway's authentication middleware:

```rust
use authn_resolver_sdk::{AuthNResolverClient, AuthenticationRequest};

let authn = hub.get::<dyn AuthNResolverClient>()?;

// Authenticate a bearer token; the request (and its cache control) reaches the plugin unchanged.
let request = AuthenticationRequest::new("eyJhbGciOiJSUzI1NiIs...");
let result = authn.authenticate(&request).await?;
let security_context = result.security_context;
// security_context contains: subject_id, subject_tenant_id, token_scopes, bearer_token
```

## Configuration

The gear is configured via the server's YAML config. Plugin selection is automatic based on GTS registration. Use the `static-authn` feature flag to compile in the development plugin.

## Writing a Plugin

Implement the `AuthNResolverPluginClient` trait from `cf-gears-authn-resolver-sdk` and register it with a GTS instance ID derived from the `AuthNResolverPluginSpecV1` schema.

## Testing

```bash
cargo test -p cf-gears-authn-resolver
```

## License

Apache-2.0
