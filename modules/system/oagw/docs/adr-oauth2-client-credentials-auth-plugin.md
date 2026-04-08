# ADR: OAuth2 Client Credentials Auth Plugin

- **Status**: Proposal
- **Date**: 2026-02-24 (initial), 2026-03-02 (updated — token caching implemented), 2026-03-03 (updated — switched to `fetch_token`, per-IdP TTL)
- **Deciders**: OAGW Team

## Context and Problem Statement

OAGW needs to authenticate outbound proxy requests to upstream services that require OAuth2 Client Credentials flow (RFC 6749 §4.4). The caller exchanges `client_id` + `client_secret` for a short-lived access token at a token endpoint, then injects it as `Authorization: Bearer <token>` on each proxied request.

Current built-in auth plugins (`ApiKeyAuthPlugin`, `NoopAuthPlugin`) handle static secrets only. Without a dedicated OAuth2 plugin, operators must pre-compute tokens externally and supply them as static bearer credentials — breaking automatic token rotation.

**Existing infrastructure**:

- `AuthPlugin` trait in `oagw/src/domain/plugin/mod.rs` — `authenticate(&self, ctx: &mut AuthContext)` interface
- `CredStoreClientV1` in `credstore-sdk` — resolves `cred://` references to secret values
- `modkit_auth::oauth2::fetch_token` in `libs/modkit-auth` — one-shot token exchange returning bearer + `expires_in`, OIDC Discovery, `Basic`/`Form` client auth (no background watcher)
- GTS identifiers already reserved in `gts_helpers.rs`:
  - `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred.v1` (Form)
  - `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred_basic.v1` (Basic)

### Why `fetch_token()` over `Token`

`modkit-auth` offers two token acquisition APIs. `Token` is a long-lived handle that spawns an `aliri_tokens::TokenWatcher` background task for automatic refresh — designed for service-level singletons where one identity authenticates to one upstream for the process lifetime. `fetch_token()` performs a single HTTP exchange and returns the bearer value alongside `expires_in`, spawning nothing.

The OAGW plugin is multi-tenant: each cache miss resolves a different (tenant, subject, config) tuple. Using `Token` here would spawn a background watcher per miss — potentially thousands of orphaned tokio tasks sleeping until their next refresh cycle, each holding `client_id` and `client_secret` in the captured `source_factory` closure. `fetch_token()` avoids both problems: credentials are transient (dropped after the fetch), and no background tasks are created. The plugin manages its own cache with `pingora-memory-cache`, making `Token`'s built-in refresh redundant.

## Decision Drivers

- Credential safety: secrets sourced via CredStore, wrapped in `SecretString`, never logged
- Per-request IdP calls incur 100–500ms latency and risk IdP rate limits — caching required
- Dual auth methods: `Form` and `Basic` as separate registered plugin IDs
- Cross-tenant and cross-subject credential isolation must be guaranteed by cache key design
- Strategic alignment: `pingora-memory-cache` aligns with planned Pingora adoption

## Decision Outcome

**Plugin with internal `pingora-memory-cache` token cache**. The plugin fetches a token from the IdP on the first request for a given (tenant, subject, config) tuple, caches it with a configurable TTL (default 5 minutes), and serves subsequent requests from cache until expiry.

Caching is an internal concern of the OAuth2 CC plugin — not a generic decorator. `ApiKeyAuthPlugin` and `NoopAuthPlugin` have no expensive fetch and carry no caching infrastructure. The `AuthPlugin` trait remains unchanged.

### Plugin Variants

| GTS Plugin ID | Client Auth Method |
|---|---|
| `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred.v1` | `Form` (credentials in request body) |
| `gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred_basic.v1` | `Basic` (credentials in `Authorization` header) |

Both registered in `AuthPluginRegistry::with_builtins`. Only `auth_method` differs; both share the same cache configuration.

### Plugin Config (ctx.config keys)

| Key | Required | Description |
|---|---|---|
| `token_endpoint` | Mutually exclusive with `issuer_url` | Direct token endpoint URL |
| `issuer_url` | Mutually exclusive with `token_endpoint` | OIDC issuer URL (Discovery) |
| `client_id_ref` | Yes | `cred://` reference for `client_id` |
| `client_secret_ref` | Yes | `cred://` reference for `client_secret` |
| `scopes` | No | Space-separated OAuth2 scopes |

### Module-Level Configuration (OagwConfig)

| Key | Default | Description |
|---|---|---|
| `token_cache_ttl_secs` | 300 (5 min) | Ceiling for cached access token TTL. The actual TTL is `min(config_ttl, expires_in − 30s safety margin)`, where `expires_in` is reported by the IdP. Kept short because there is no cache-invalidation mechanism yet — a revoked or rotated token remains cached until expiry. |
| `token_cache_capacity` | 10,000 | Maximum entries in the token cache. |

These are bundled into a `TokenCacheConfig` struct and threaded through `DataPlaneServiceImpl::new()` → `AuthPluginRegistry::with_builtins()` → plugin constructors.

### Cache Key Design

`TinyUfo` (used internally by `pingora-memory-cache`) hashes keys to `u64` and does **not** use `Eq` for collision resolution. To avoid silent collisions, the cache uses a `String` key encoding all identity components:

```rust
fn build_cache_key(ctx: &AuthContext, auth_method: ClientAuthMethod) -> String {
    format!(
        "{}:{}:{}:{}",
        ctx.security_context.subject_tenant_id(),
        ctx.security_context.subject_id(),
        auth_method_tag(auth_method),
        hash_config(&ctx.config),
    )
}
```

The key includes:
- `subject_tenant_id` — cross-tenant isolation
- `subject_id` — cross-subject isolation for CredStore `private` sharing mode
- `auth_method` — prevents collisions if Form and Basic plugins share identical config
- `config_hash` — sorted deterministic hash of all plugin config key/value pairs; different upstream configs (e.g. different scopes) get different entries

### Hash-Collision Safety via CachedToken Wrapper

To eliminate the (2^-45 probability) risk of `TinyUfo` silently returning another tenant's token on a `u64` hash collision, the cache stores a `CachedToken` wrapper that includes the original key for verification on hit:

```rust
#[derive(Clone)]
struct CachedToken {
    key: String,
    token: SecretString,
}
```

On cache hit, `entry.key == lookup_key` is verified before using the token. A mismatch is treated as a cache miss. This provides defense-in-depth for the multi-tenant security boundary.

### Authentication Flow

```text
authenticate(ctx) called
  ├─ Parse OAuth2PluginConfig from ctx.config
  ├─ build_cache_key(ctx, auth_method)
  ├─ cache.get(&key) → CachedToken?
  │   ├─ Hit + key matches → inject cached token, return Ok(())
  │   └─ Miss (or key mismatch) → continue
  ├─ resolve_secret(client_id_ref)     → CredStore lookup
  ├─ resolve_secret(client_secret_ref) → CredStore lookup
  ├─ fetch_token(OAuthClientConfig)    → FetchedToken { bearer, expires_in }
  ├─ ttl = min(config_ttl, expires_in − 30s safety margin)
  ├─ cache.put(&key, CachedToken { key, token }, ttl)
  ├─ Inject Authorization: Bearer <token> into ctx.headers
  └─ Return Ok(())
```

Failed token fetches are **not** cached — the next request for the same key retries the IdP.

### Plugin Implementation

```rust
use pingora_memory_cache::MemoryCache;

pub struct OAuth2ClientCredAuthPlugin {
    credstore: Arc<dyn CredStoreClientV1>,
    auth_method: ClientAuthMethod,
    http_config: Option<modkit_http::HttpClientConfig>,
    cache: MemoryCache<String, CachedToken>,
    cache_ttl: Duration,
}

impl OAuth2ClientCredAuthPlugin {
    pub fn new(
        credstore: Arc<dyn CredStoreClientV1>,
        auth_method: ClientAuthMethod,
        cache_ttl: Duration,
        cache_capacity: usize,
    ) -> Self {
        Self {
            credstore,
            auth_method,
            http_config: None,
            cache: MemoryCache::new(cache_capacity),
            cache_ttl,
        }
    }
}
```

### Registry Integration

```rust
impl AuthPluginRegistry {
    pub fn with_builtins(
        credstore: Arc<dyn CredStoreClientV1>,
        token_http_config: Option<modkit_http::HttpClientConfig>,
        token_cache_config: TokenCacheConfig,
    ) -> Self {
        // ... apikey and noop plugins unchanged ...

        let form_plugin = OAuth2ClientCredAuthPlugin::new(
            credstore.clone(),
            ClientAuthMethod::Form,
            token_cache_config.ttl,
            token_cache_config.capacity,
        );
        let basic_plugin = OAuth2ClientCredAuthPlugin::new(
            credstore.clone(),
            ClientAuthMethod::Basic,
            token_cache_config.ttl,
            token_cache_config.capacity,
        );
        // ... register both ...
    }
}
```

### Upstream Configuration Example

```json
{
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "graph.microsoft.com", "port": 443 }
    ]
  },
  "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
  "auth": {
    "type": "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred.v1",
    "config": {
      "token_endpoint": "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token",
      "client_id_ref": "cred://ms-graph-client-id",
      "client_secret_ref": "cred://ms-graph-client-secret",
      "scopes": "https://graph.microsoft.com/.default"
    }
  }
}
```

OIDC Discovery variant (endpoint resolved automatically):

```json
{
  "auth": {
    "type": "gts.x.core.oagw.auth_plugin.v1~x.core.oagw.oauth2_client_cred_basic.v1",
    "config": {
      "issuer_url": "https://accounts.google.com",
      "client_id_ref": "cred://google-client-id",
      "client_secret_ref": "cred://google-client-secret",
      "scopes": "https://www.googleapis.com/auth/cloud-platform"
    }
  }
}
```

### Retry on Upstream 401 (Deferred)

The current design does **not** implement retry logic when the upstream rejects credentials with a 401. In order to support retries, the `AuthPlugin` trait needs to return a meaningful response that consumers (the Data Plane) can use to decide whether a retry with fresh credentials is warranted. Today's trait returns `Result<(), PluginError>`, which provides no such signal.

Key considerations for future retry design:

- Not all plugins benefit from retry. A static API key plugin would produce the same credential on retry, making a second attempt pointless. An OAuth2 plugin with cached tokens could produce a fresh token, making retry meaningful.
- The Data Plane is the only layer that sees both the plugin's output and the upstream's response, so retry orchestration belongs there.
- The plugin must communicate enough metadata for the Data Plane to make the retry decision without the Data Plane needing to understand plugin internals.

This is an area of active design. The trait shape, the metadata returned, and the retry policy are all open questions that will be addressed in a dedicated iteration.

## Consequences

### Positive

- Eliminates per-request IdP calls — cached tokens served in microseconds (vs 100–500ms)
- Reduces IdP rate-limit pressure — one call per unique (tenant, subject, config) per TTL window
- Cross-tenant and cross-subject isolation guaranteed by cache key design + CachedToken key verification
- `SecretString` (`ZeroizeOnDrop`) securely zeroes token buffers on cache eviction
- Failed token fetches are not cached — transient IdP errors self-heal on the next request
- No changes to the `AuthPlugin` trait — existing plugins (`ApiKeyAuthPlugin`, `NoopAuthPlugin`) unchanged
- `pingora-memory-cache` aligns with planned Pingora adoption (S3-FIFO + TinyLFU eviction, stampede protection)
- `expires_in`-aware cache TTL via `min(config_ttl, expires_in − 30s)` prevents serving tokens near expiry when IdP issues short-lived tokens
- No orphaned background tasks — `fetch_token()` performs a single HTTP exchange and returns; no `aliri_tokens::TokenWatcher` is spawned

### Negative

- Plugin is no longer stateless — carries an in-memory cache with security-sensitive material
- CredStore lookups still happen on every cache miss (not cached separately)
- No automatic recovery from upstream 401 — the Data Plane has no metadata to decide whether retrying with fresh credentials is meaningful (see [Retry on Upstream 401 (Deferred)](#retry-on-upstream-401-deferred))
- `TinyUfo`'s lazy expiry means tokens may linger in memory slightly past TTL until the next `get()` check

### Risks

- **CredStore unreachable**: returns `PluginError::Internal` on cache miss; cached tokens continue to be served until TTL expiry
- **IdP unavailable**: `fetch_token()` fails → `PluginError::Internal`; not cached; next request retries
- **Hash collision in TinyUfo**: mitigated by `CachedToken` key verification — collision returns a cache miss, never another tenant's token

### Known Residual Plaintext

- The `format!("Bearer {}", token.expose())` string in `ctx.headers` is a plain `String` — not zeroed, but short-lived (scoped to the request).
- Inside `OAuthTokenSource::request_token()`, the `AccessToken` from `aliri_tokens` is a plain `String` — not zeroed. However, the source is created, used once, and dropped immediately by `fetch_token()`, so the plaintext lifetime is limited to the fetch call.
- The long-lived cache entry itself IS zeroed on eviction via `SecretString`'s `ZeroizeOnDrop`. `pingora-memory-cache` uses flurry/seize for deferred reclamation, so there is a small delay between eviction and actual `Drop`. For bearer tokens with ~1 hour TTL, this delay (milliseconds to seconds) is negligible.

## Out of Scope

- **Authorization Code Grant** (RFC 6749 §4.1): Requires user consent, redirect callbacks (OAuth Router), and refresh token storage across data centers. Separate ADR needed.

## Future Considerations

- **Retry on upstream 401**: The `AuthPlugin` trait needs to return a richer response so the Data Plane can determine whether retrying with fresh credentials is meaningful. This requires revisiting the trait signature (e.g. returning metadata alongside injected headers). See [Retry on Upstream 401 (Deferred)](#retry-on-upstream-401-deferred).
- **Event-driven cache invalidation**: Once OAGW has access to the Event Broker, consuming `oauth_client.updated` and `oauth_client.deleted` events to trigger immediate cache eviction would reduce the staleness window from TTL to event propagation latency (~seconds).
- ~~**Per-IdP TTL from token response**~~: Implemented — `fetch_token()` returns `expires_in` from the IdP response, and the plugin caches with `min(config_ttl, expires_in − 30s safety margin)`. Tokens with `expires_in ≤ 30s` are not cached.

## Related ADRs

- [ADR: Plugin System](./adr-plugin-system.md) — `AuthPlugin` trait and execution model
- [ADR: Component Architecture](./adr-component-architecture.md) — `DataPlaneService`, `AuthPluginRegistry`
- [ADR: Control Plane Caching](./adr-data-plane-caching.md) — Cache invalidation pattern
- [ADR: State Management](./adr-state-management.md) — Cache eviction and state coordination

## References

- [RFC 6749: OAuth 2.0 Authorization Framework](https://datatracker.ietf.org/doc/html/rfc6749) — §4.4 Client Credentials Grant
- [OpenID Connect Discovery 1.0](https://openid.net/specs/openid-connect-discovery-1_0.html) — Token endpoint discovery
- [pingora-memory-cache](https://github.com/cloudflare/pingora/tree/main/pingora-memory-cache) — S3-FIFO + TinyLFU eviction, cache stampede protection
- `libs/modkit-auth` (`oauth2/config.rs`, `oauth2/source.rs`, `oauth2/token.rs`, `oauth2/fetch.rs`) — Token management library
- `oagw/src/infra/plugin/apikey_auth.rs` — Reference `AuthPlugin` implementation
