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

Pass the raw token, without the `Bearer ` prefix, in an `AuthenticationRequest`. `AuthenticationRequest::new` asks for a fresh validation, so the plugin never answers from its own cache; a caller that accepts a plugin-cached result opts in with `.with_cache_control(RequestCacheControl::AllowCached)`:

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
use authn_resolver_sdk::{AuthNResolverClient, ClientCredentialsProvider, ClientCredentialsProviderConfig, ScopeSet};
use secrecy::SecretString;

let provider = ClientCredentialsProvider::builder(
    hub.get::<dyn AuthNResolverClient>()?,
    "my-service",
    SecretString::from(client_secret),
)
.with_default_scopes(ScopeSet::new(["usage:write"])?) // optional: IdP defaults otherwise
.with_config(ClientCredentialsProviderConfig::default()) // optional
.build()?;
let result = provider.get().await?;
// Invalidate the rejected result without clearing a newer entry.
if call_downstream(&result.security_context).await.is_unauthorized() {
    let _invalidated = provider.invalidate(&result);
}
// Exactly these scopes, cached separately from the default ones.
let read_only = provider.get_with_scopes(&ScopeSet::new(["usage:read"])?).await?;
// At component shutdown:
provider.shutdown().await;
```

`ClientCredentialsProvider::builder(client, client_id, client_secret)` is the only way to create a provider; credentials, default scopes and `ClientCredentialsProviderConfig` are fixed at `build()`, which validates the config and neither spawns nor contacts the resolver. The builder's `Debug` omits the secret. Clones share the cache and acquisitions. `get()` uses the default scopes from `with_default_scopes` (without it, the IdP defaults). `get_with_scopes(&ScopeSet)` uses exactly that set: the same set as the default one shares its slot, while subsets and supersets never reuse another set's result. Scopes are sent in canonical form (sorted, deduplicated, case-sensitive). `ScopeSet` accepts only RFC 6749 scope-tokens, without trimming, and bounds input to 64 items, 256 bytes each and 4096 bytes in total.

`ClientCredentialsProviderConfig::max_entries` (default 17, `1..=1024`) bounds the scope-set slots in total, including the always-present default slot: the default leaves 16 for other sets, and `1` allows only the default scopes (an explicit set equal to them still shares the default slot). The limit counts slots, not bytes: each holds at most one result, one failure and one current acquisition (a cancelled acquisition may still be ending, so concurrent IdP requests are not bounded by it). Results already returned to callers are not counted. A new set evicts an idle slot (no call in progress, no acquisition task, even a cancelled one still ending, and backoff elapsed), preferring one without a usable entry, then the least recently used. If none is idle, `get_with_scopes` fails at once with `ClientCredentialsProviderError::CapacityExhausted { max_entries }`, reporting the configured total (temporary: retry later; permanent for other sets when `max_entries` is 1). A new dynamic slot's first acquisition requests plugin refresh, so eviction cannot resurrect a cleared result from the plugin cache; this costs a plugin-cache bypass per admission. Do not derive scope sets from untrusted input unless the capacity is sized for it.

The provider builds each `ClientCredentialsRequest` itself, setting scopes and `cache_control` per acquisition. Plugins opt into reuse through `CacheControl::cache_until` (`None` means NoStore). Reuse ends at the earlier of that deadline and known credential expiry, minus a safety margin. Valid results too short for the margin are delivered without caching; an elapsed reuse deadline becomes NoStore. The reuse window and the margin bound only reuse: the callers waiting on an acquisition receive its result until the credential's known expiry.

`get()` triggers jittered early refresh with single flight; no periodic worker runs. All acquisition errors trigger jittered backoff. Only `ServiceUnavailable` permits fallback to a usable entry. Failures log at `warn` with error kind, count, retry delay and fallback status; recovery logs at `info`. When reusable results start arriving too short-lived for `safety_margin`, so that every call acquires, a `warn` names the margin once per run of such results; any other success (cached, no-store or with a closed reuse window) ends the run.

`ClientCredentialsProviderConfig::default()` supplies every setting; override only what the caller needs:

| Setting | Default | Meaning |
| --- | --- | --- |
| `safety_margin` | 30s | Stop cache reuse this far before the earlier reuse or credential deadline. Shorter-lived valid results are delivered to current waiters without caching; the margin never bounds a fresh acquisition's own waiters. |
| `acquisition_timeout` | 30s | Bound one shared resolver acquisition, including any plugin retries. |
| `min_backoff` | 1s | First failure delay and minimum jittered delay after later failures. |
| `max_backoff` | 60s | Cap the exponential failure delay. |
| `max_entries` | 17 | Bound exact scope-set slots in total, the default one included (`1..=1024`). |

Early-refresh timing and jitter are internal policy. The current policy opens the
refresh window after 70–80% of the entry's usable lifetime (after the safety
margin); the random offset is drawn once per entry. The first call in that window
starts refresh and returns the usable result. Independent providers therefore
spread their refresh requests over time. Backoff jitter is also automatic and
stays within the configured backoff bounds.

`clear()` covers every slot; `invalidate(&result)` only the slot that returned `result`, and only while it is that slot's latest result. Both invalidate pending deliveries and acquisitions, request plugin refresh next, and preserve backoff. Keep the result until the downstream call completes: it carries a private local origin (provider, slot, publication), so a late rejection cannot clear a newer entry or another scope set, and results straight from the resolver or from another provider are no-ops. This differs from the plugin's optional `CacheTag`.

Caller cancellation leaves shared acquisition running. Final handle drop cancels it; `shutdown().await` also waits for cancellation. `shutdown()` closes admission first. Subsequent `get()` and `get_with_scopes()` calls fail with `ClientCredentialsProviderError::ShutDown`.

Acquisition and tracing: the shared acquisition runs in the tracing span of the call that started it and, if that call had an active subscriber, under that subscriber; with none active at that point it uses the ambient (global) subscriber when it runs, so logging initialized later still sees it. Resolver and failure/recovery events therefore stay attached to the initiating caller's trace; waiters that joined the same acquisition do not receive them.

[`examples/cached_client_credentials.rs`](examples/cached_client_credentials.rs) runs the complete workflow (build, cached `get`, rejection and `invalidate`, dynamic scopes, shutdown) against an in-process resolver: `cargo run -p cf-gears-authn-resolver-sdk --example cached_client_credentials`. It is executed by `tests/client_credentials_example.rs`.

See the [design](../../../../docs/arch/authorization/DESIGN.md#s2s-caching-provider) for the plugin refresh contract and [rustdoc](src/client_credentials/mod.rs) for configuration and method details.

### Cached inbound authentication (opt-in)

By default every inbound token is validated on every call (`AuthNResolverBearerAuthenticator` and the API gateway send `RequestCacheControl::refresh()`). `CachingAuthenticator` is an explicit opt-in that reuses a successful validation of the same token, but only when the producing plugin allows it. The OIDC plugin allows nothing by default:

```yaml
# OIDC plugin configuration: opt in to caller reuse (default "0s" = no-store, max 300s)
jwt:
  authenticate_result_cache_ttl: 30s
```

```rust
use authn_resolver_sdk::{AuthNResolverClient, CachingAuthenticator, CachingAuthenticatorConfig};
use toolkit_security::DynBearerAuthenticator;

let authenticator = CachingAuthenticator::builder(hub.get::<dyn AuthNResolverClient>()?)
    .with_config(CachingAuthenticatorConfig::default()) // optional
    .build()?;
let result = authenticator.authenticate(token).await?;
if call_downstream(&result.security_context).await.is_unauthorized() {
    // Drop this result's entry; a newer one for the same token is untouched.
    let _dropped = authenticator.invalidate(&result); // or authenticator.clear()
}
// Or hand it to transport middleware instead of the always-revalidating adapter:
let bearer = DynBearerAuthenticator::new(authenticator.clone());
// At component shutdown:
authenticator.shutdown().await;
```

Errors:

```rust
use authn_resolver_sdk::CachingAuthenticatorError;

match authenticator.authenticate(token).await {
    Ok(result) => { /* use result.security_context */ },
    Err(CachingAuthenticatorError::Resolver(source)) => { /* invalid, expired or backend failure */ },
    Err(CachingAuthenticatorError::Overloaded { .. }) => { /* too many new tokens at once: 503 */ },
    Err(CachingAuthenticatorError::ShutDown) => { /* stopped */ },
}
```

A result is cached only if its `cache_control.cache_until` is still ahead; reuse ends at the earliest of that deadline, the known credential expiry and `max_ttl` after the validation completed. A finite `cache_until` without known expiry is honoured (bounded by `max_ttl`). Everything else (no-store, an elapsed deadline, every failure) reaches only the callers of that validation: there is no negative caching, background refresh or token renewal, stale fallback or backoff. A success whose expiry has already passed when validation completes is refused as `Unauthorized`, even if the backend accepted it within a clock-skew leeway. The callers of a validation receive its result until the credential's known expiry even if the reuse window closed meanwhile; only later calls are bound by the entry. A call whose result is discarded before it collects it (invalidated, cleared, or its credential expired) validates again, at most three validations in total, and then fails with `Resolver(ServiceUnavailable)`. A revoked token stays accepted by new calls until its entry's deadline unless `invalidate` or `clear` is called, and a caller already waiting on a validation may still receive it until the credential expires; the cache TTL is not a maximum age for every returned result.

Every validation it starts asks the plugin for a refresh, never `AllowCached`. It keeps the plugin's `CacheControl::tag` of the latest result per token, also after the entry expires, is capped by `max_ttl`, or is invalidated, or when the result was no-store, and sends `Refresh` with that tag, so a plugin with its own cache may answer only with a provably newer result. Without a retained tag (first sight, evicted, cleared, untagged) it sends an unconditional refresh: a forgotten record can never bring back a result the plugin cached and the caller rejected, at the cost of no plugin-cache reuse on such misses. A failed validation keeps the tag for the retry. A retained tag is never served as a result.

Concurrent calls with the same token share one validation; a cancelled caller leaves it running so its result can be cached. Cache keys are SHA-256 digests (platform crypto facade); the validation holds the raw token only while it runs, but a cached result may still carry it in its `SecurityContext` for forwarding. Neither token nor digest is logged.

| Setting | Default | Meaning |
| --- | --- | --- |
| `max_entries` | 10 000 | Retained records (`1..=100000`): cached results plus tag-only refresh hints. A full cache evicts hints first, then the entry expiring first. |
| `max_in_flight` | 256 | Validations running at once, cancelled ones counted until their task ends (`1..=4096`). A token needing a new validation beyond it fails at once with `Overloaded`; cached tokens and tokens already being validated still work, and nothing bypasses the bound. |
| `max_ttl` | 30s | Consumer cap on reuse (up to one day); the producer's `cache_until` and expiry can only shorten it. |
| `acquisition_timeout` | 30s | Bound on one validation; on expiry it fails with `ServiceUnavailable`. |

`invalidate(&result)` drops only the entry that produced `result`, if it is still cached, and fences its deliveries not yet collected; results from the resolver directly, from another cache, already replaced or evicted are no-ops. It keeps the result's tag (the one the plugin returned, not the caller's copy) as a refresh hint. `clear()` drops all entries and hints, cancels validations and fences all uncollected deliveries. `shutdown().await` also waits for validation tasks; later calls fail with `ShutDown`.

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

`ClientCredentialsProvider` calls return `ClientCredentialsProviderError`, which keeps the resolver error as its `source()`:

```rust
use authn_resolver_sdk::{AuthNResolverError, ClientCredentialsProviderError};

match provider.get_with_scopes(&scopes).await {
    Ok(result) => { /* use result.security_context */ },
    // Backend failure, shared with the attempt's waiters and repeated during backoff;
    // a later attempt may succeed.
    Err(ClientCredentialsProviderError::Resolver(AuthNResolverError::ServiceUnavailable(_))) => { /* retry later */ },
    Err(ClientCredentialsProviderError::Resolver(other)) => { /* see AuthNResolverError above */ },
    // Every slot besides the default one is busy (always so when max_entries is 1).
    Err(ClientCredentialsProviderError::CapacityExhausted { max_entries }) => { /* retry later or resize */ },
    Err(ClientCredentialsProviderError::ShutDown) => { /* provider stopped: do not retry */ },
}
```

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
        // Validate `request.bearer_token`, extract claims, build SecurityContext.
        // A plugin with its own authenticate cache must honour `request.cache_control`:
        // AllowCached may reuse; Refresh(None) must not; Refresh(Some(tag)) may reuse
        // only a result provably newer than `tag`. Without such a cache, ignore it.
    }

    async fn exchange_client_credentials(&self, request: &ClientCredentialsRequest)
        -> Result<AuthenticationResult, AuthNResolverError> {
        // OAuth2 client_credentials flow → SecurityContext.
        // `request.scopes` is a validated `Option<ScopeSet>`; `None` means IdP defaults,
        // `Some(set)` is sent as `set.to_string()` (space-separated, canonical).
    }
}
```

## License

Apache-2.0
