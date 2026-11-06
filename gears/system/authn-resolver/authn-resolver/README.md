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

## Plugin catalogue

The resolver and its plugins use `PlatformTypesRegistryApi`, wired by their `gts_publication(...)`
publication capability. None depends on the registry gear crate or declares it in `deps`.

- The resolver and plugins are application gears. `gts_publication(...)` declares the resolver's SDK
  schemas and the plugins' configured instances; runtime publication precedes application `init`.
- Plugins build and bind their scoped `AuthNResolverPluginClient` in `init`, after admission.
  Refusal or publication-budget expiry prevents application initialization.
- First authentication lists the platform catalogue, retains locally bound clients and selects
  by vendor/lowest priority. Failures are uncached; legacy is never consulted.
- Publication uses the local platform API or a remote endpoint with platform credentials.
  An embedded full registry needs a database even in a resolver-only `auth_disabled` host;
  the resolver itself publishes its plugin specification.

## Usage

The primary consumer is the API Gateway's authentication middleware:

```rust
use authn_resolver_sdk::AuthNResolverClient;

let authn = hub.get::<dyn AuthNResolverClient>()?;

// Authenticate a bearer token
let result = authn.authenticate("eyJhbGciOiJSUzI1NiIs...").await?;
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
