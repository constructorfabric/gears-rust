# AuthN Resolver SDK

SDK crate for the AuthN Resolver gear, providing public API contracts for authentication in Gears.

## Overview

This crate defines the transport-agnostic interface for the AuthN Resolver gear:

- **`AuthNResolverClient`** — Async trait for consumers (authenticate bearer tokens)
- **`AuthNResolverPluginClient`** — Async trait for plugin implementations
- **`AuthenticationRequest`** — Bearer token (secret) plus what the plugin may serve from its own cache
- **`AuthenticationResult`** — Validated `SecurityContext` and explicit reuse policy
- **`ClientCredentialsProvider`** — Shared S2S cache, one entry per exact scope set, with on-demand refresh
- **`CachingAuthenticator`** — Opt-in inbound bearer-token validation with bounded, producer-authorized result reuse
- **`ScopeSet`** — Validated, canonical `OAuth2` scope set (RFC 6749 section 3.3)
- **`AuthNResolverError`** — Error types for authentication failures
- **`AuthNResolverPluginSpecV1`** — GTS schema for plugin registration

## Usage

### Getting the Client

Consumers obtain the client from `ClientHub`:

```rust
use authn_resolver_sdk::AuthNResolverClient;

let authn = hub.get::<dyn AuthNResolverClient>()?;
```

### Authenticating a Token

Pass the token without the `Bearer ` prefix. `AuthenticationRequest::new` requires fresh validation. To allow plugin-cache hits, set `.with_cache_control(RequestCacheControl::AllowCached)`.

```rust
use authn_resolver_sdk::AuthenticationRequest;

let request = AuthenticationRequest::new("eyJhbGciOiJSUzI1NiIs...");
let result = authn.authenticate(&request).await?;
let ctx = result.security_context;

println!("Subject: {}", ctx.subject_id());
println!("Tenant: {}", ctx.subject_tenant_id());
println!("Scopes: {:?}", ctx.token_scopes());
```

### AuthenticationResult

```rust
let result = AuthenticationResult::new(security_context); // NoStore by default
let cacheable = result.with_cache_control(CacheControl::until(deadline));
```

`SecurityContext::expires_at()` is the optional credential expiry, never the `client_secret` lifetime. Revocation may happen earlier; unknown expiry does not permit indefinite reuse.

### Cached S2S credentials

```rust
use authn_resolver_sdk::{AuthNResolverClient, ClientCredentialsProvider, ScopeSet};
use secrecy::SecretString;

let provider = ClientCredentialsProvider::builder(
    hub.get::<dyn AuthNResolverClient>()?,
    "my-service",
    SecretString::from(client_secret),
)
.with_default_scopes(ScopeSet::new(["usage:write"])?) // optional: IdP defaults otherwise
.build()?;
let result = provider.get().await?;
// Invalidate the rejected result without clearing a newer entry.
if call_downstream(&result.security_context).await.is_unauthorized() {
    let _invalidated = provider.invalidate(&result);
}
let read_only = provider.get_with_scopes(&ScopeSet::new(["usage:read"])?).await?;
provider.shutdown().await;
```

Clones share credentials, cache entries and acquisitions. `build()` validates configuration locally. `get()` uses the configured default scopes (otherwise the IdP defaults). `get_with_scopes` shares an entry only for an exact match. `ScopeSet` sorts and deduplicates case-sensitive RFC 6749 scope-tokens without trimming. Input limits are 64 tokens, 256 bytes each and 4096 bytes total.

A slot is idle once all calls and acquisition tasks have ended and backoff has elapsed. At capacity, the provider evicts an idle non-default slot, preferring one without a usable entry, then the least recently used. If none qualifies, the call immediately returns `CapacityExhausted { max_entries }`. With a limit of 1, only the default scopes fit. New dynamic slots request fresh credentials so eviction cannot restore a rejected token. Size the slot limit before accepting caller-supplied scopes. Memory use and outstanding IdP requests can vary independently.

Plugins permit reuse with a finite `cache_until`. The provider subtracts `safety_margin` from the earlier of that deadline and known credential expiry. Callers waiting on the exchange may collect a valid result even inside the margin or after the reuse window closes.

Use `.with_config(...)` to override the defaults.

| Setting | Default | Meaning |
| --- | --- | --- |
| `safety_margin` | 30s | Time reserved before the reuse deadline. |
| `acquisition_timeout` | 30s | Bound one shared resolver acquisition, including any plugin retries. |
| `min_backoff` | 1s | First failure delay and minimum jittered delay after later failures. |
| `max_backoff` | 60s | Cap the exponential failure delay. |
| `max_entries` | 17 | Bound exact scope-set slots in total, the default one included (`1..=1024`). |

The refresh window opens 70–80% into an entry's usable lifetime. The first call in that window starts a shared refresh and returns the usable entry. Every acquisition failure triggers jittered exponential backoff. Only `ServiceUnavailable` permits fallback before the deadline.

Failures log at `warn` with error kind, consecutive count, retry delay and fallback status. Recovery logs at `info`. A run of cacheable results too short for the safety margin produces one warning. Acquisition uses the initiating caller's span and active subscriber, or the ambient subscriber when none was active.

Keep the result until the downstream call completes. `invalidate(&result)` affects only the issuing slot's latest result. `clear()` affects every slot. Both discard pending deliveries, cancel affected acquisitions and request plugin refresh while preserving backoff.

Cancelling a caller leaves the acquisition running. Dropping the last handle cancels it. `shutdown().await` closes admission, cancels acquisitions and waits for their tasks. Later calls return `ShutDown`.

Run the [complete example](examples/cached_client_credentials.rs) with `cargo run -p cf-gears-authn-resolver-sdk --example cached_client_credentials`. See the [provider reference](src/client_credentials/mod.rs) for method contracts.

### Cached inbound authentication (opt-in)

The API gateway and default bearer adapter validate every call. To reuse a successful validation, install `CachingAuthenticator` and enable reuse in the plugin. For OIDC, set `jwt.authenticate_result_cache_ttl` (default `0s`, maximum `300s`).

```yaml
jwt:
  authenticate_result_cache_ttl: 30s
```

```rust
use authn_resolver_sdk::{AuthNResolverClient, CachingAuthenticator};
use toolkit_security::DynBearerAuthenticator;

let authenticator = CachingAuthenticator::builder(hub.get::<dyn AuthNResolverClient>()?)
    .build()?;
let result = authenticator.authenticate(token).await?;
if call_downstream(&result.security_context).await.is_unauthorized() {
    let _dropped = authenticator.invalidate(&result);
}
// Adapter for transport middleware.
let bearer = DynBearerAuthenticator::new(authenticator.clone());
authenticator.shutdown().await;
```

Calls for the same token share one validation, which continues if a caller cancels. The cache uses SHA-256 keys and logs neither tokens nor digests. The returned `SecurityContext` may carry the raw token for forwarding.

Only successful validations with a future `cache_until` can be reused. Reuse ends at the earliest of that deadline, known credential expiry and `max_ttl` after completion. A credential already expired at completion yields `Unauthorized`, even within backend clock-skew leeway. Waiters on the original validation may collect until credential expiry. If invalidation or expiry discards three consecutive deliveries, the call returns `Resolver(ServiceUnavailable)`. Failures trigger neither caching nor backoff.

Every validation requests refresh. A retained plugin tag allows reuse only of a provably newer result. Expiry, invalidation, no-store results and failures can leave a tag for the next call. Without a tag the refresh is unconditional.

| Setting | Default | Meaning |
| --- | --- | --- |
| `max_entries` | 10 000 | Results and tag-only records (`1..=100000`). Evict tags first, then the earliest-expiring result. |
| `max_in_flight` | 256 | Concurrent validations (`1..=4096`), including cancelled tasks until they exit. At capacity, new validations fail with `Overloaded` (503). Hits and callers joining an existing validation proceed. |
| `max_ttl` | 30s | Maximum reuse from validation completion, up to one day. |
| `acquisition_timeout` | 30s | Timeout per validation, reported as `ServiceUnavailable`. |

Timeouts and the start of each overload period log at `warn`. `invalidate(&result)` drops the matching resident entry, preserves its original plugin tag and discards its pending deliveries. `clear()` also forgets all tags, cancels validations and discards all pending successes and failures. `shutdown().await` waits for tasks to exit. Later calls return `ShutDown`.

## Error Handling

Resolver calls return `AuthNResolverError`:

```rust
use authn_resolver_sdk::AuthNResolverError;

match authn.authenticate(&request).await {
    Ok(result) => { /* use result.security_context */ },
    Err(AuthNResolverError::Unauthorized(msg)) => { /* invalid/expired token */ },
    Err(AuthNResolverError::NoPluginAvailable) => { /* no AuthN plugin registered */ },
    Err(AuthNResolverError::ServiceUnavailable(msg)) => { /* plugin/IdP temporarily unavailable */ },
    Err(AuthNResolverError::TokenAcquisitionFailed(msg)) => { /* exchange rejected/unusable */ },
    Err(AuthNResolverError::Internal(msg)) => { /* unexpected error */ },
}
```

`ClientCredentialsProviderError::Resolver` and `CachingAuthenticatorError::Resolver` preserve the underlying resolver error as their source. Retry `CapacityExhausted` or `Overloaded` when capacity frees up. `ShutDown` is permanent.

## Implementing a Plugin

Implement `AuthNResolverPluginClient` and register with a GTS instance ID:

```rust
use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverPluginClient, AuthenticationRequest, AuthenticationResult, AuthNResolverError,
    ClientCredentialsRequest,
};

struct MyOidcPlugin { /* ... */ }

#[async_trait]
impl AuthNResolverPluginClient for MyOidcPlugin {
    async fn authenticate(&self, request: &AuthenticationRequest)
        -> Result<AuthenticationResult, AuthNResolverError> {
        // Validate the token and map its claims. Honour request.cache_control on cache hits.
    }

    async fn exchange_client_credentials(&self, request: &ClientCredentialsRequest)
        -> Result<AuthenticationResult, AuthNResolverError> {
        // Exchange credentials with the IdP and validate the token.
        // Send scope = set.to_string() when requested. Omit scope for IdP defaults.
    }
}
```

## License

Apache-2.0
