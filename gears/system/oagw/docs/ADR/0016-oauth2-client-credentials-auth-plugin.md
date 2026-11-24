---
status: accepted
date: 2026-02-24
decision-makers: Constructor Fabric Steering Committee
---

# OAuth2 Client Credentials Auth Plugin — Internal Token Cache with `fetch_token`


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
  - [Why `fetch_token()` over `TokenProvider`](#why-fetch_token-over-tokenprovider)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Plugin Variants](#plugin-variants)
  - [Amendment: `private_key_jwt` client authentication](#amendment-private_key_jwt-client-authentication)
  - [Plugin Config (ctx.config keys)](#plugin-config-ctxconfig-keys)
  - [Gear-Level Configuration (OagwConfig)](#gear-level-configuration-oagwconfig)
  - [Cache Key Design](#cache-key-design)
  - [Hash-Collision Safety via CachedToken Wrapper](#hash-collision-safety-via-cachedtoken-wrapper)
  - [Authentication Flow](#authentication-flow)
  - [Plugin Implementation](#plugin-implementation)
  - [Registry Integration](#registry-integration)
  - [Upstream Configuration Example](#upstream-configuration-example)
  - [Retry on Upstream 401 (Deferred)](#retry-on-upstream-401-deferred)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Internal `pingora-memory-cache` token cache](#internal-pingora-memory-cache-token-cache)
  - [Generic caching decorator over `AuthPlugin`](#generic-caching-decorator-over-authplugin)
  - [No caching (per-request IdP call)](#no-caching-per-request-idp-call)
- [Out of Scope](#out-of-scope)
- [Future Considerations](#future-considerations)
- [Related ADRs](#related-adrs)
- [References](#references)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-oauth2-client-credentials-auth-plugin`

## Context and Problem Statement

OAGW needs to authenticate outbound proxy requests to upstream services that require OAuth2 Client Credentials flow (RFC 6749 §4.4). The caller exchanges `client_id` + `client_secret` for a short-lived access token at a token endpoint, then injects it as `Authorization: Bearer <token>` on each proxied request.

Current built-in auth plugins (`ApiKeyAuthPlugin`, `NoopAuthPlugin`) handle static secrets only. Without a dedicated OAuth2 plugin, operators must pre-compute tokens externally and supply them as static bearer credentials — breaking automatic token rotation.

**Existing infrastructure**:

- `AuthPlugin` trait in `oagw/src/domain/plugin/mod.rs` — `authenticate(&self, ctx: &mut AuthContext)` interface
- `CredStoreClientV1` in `credstore-sdk` — resolves `cred://` references to secret values
- `toolkit_auth::oauth2::fetch_token` in `libs/toolkit-auth` — one-shot token exchange returning bearer + `expires_in`, OIDC Discovery, `Basic`/`Form` client auth (no background watcher); since #4519 also `private_key_jwt`
- GTS identifiers already reserved in `gts_helpers.rs`:
  - `gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1` (Form)
  - `gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_basic.v1` (Basic)

### Why `fetch_token()` over `TokenProvider`

`toolkit-auth` offers two token acquisition APIs. `TokenProvider` manages acquisition, caching and automatic background refresh for a long-lived service identity. Its synchronous `new(config)` constructor performs no network requests; `get().await` returns a usable cached token or awaits shared acquisition. Clones share one worker, `shutdown().await` stops and joins it, and dropping the final handle cancels it as a fallback. `fetch_token()` performs a single acquisition, including OIDC discovery when configured, and returns the bearer value alongside `expires_in` without creating a background worker.

The OAGW plugin is multi-tenant: each cache miss resolves a different (tenant, subject, config) tuple. Retaining a `TokenProvider` per tuple would require managing a separate background worker and its credentials for every cached identity. Creating and dropping a provider per miss would provide no benefit from its background refresh. OAGW instead uses `fetch_token()` and manages its own token cache with `pingora-memory-cache`; credentials are dropped after acquisition and no provider workers need lifecycle management.

## Decision Drivers

* Credential safety: secrets sourced via CredStore, wrapped in `SecretString`, never logged
* Per-request IdP calls incur 100–500ms latency and risk IdP rate limits — caching required
* Dual auth methods: `Form` and `Basic` as separate registered plugin IDs
* Cross-tenant and cross-subject credential isolation must be guaranteed by cache key design
* Strategic alignment: `pingora-memory-cache` aligns with planned Pingora adoption

## Considered Options

* Plugin with internal `pingora-memory-cache` token cache
* Generic caching decorator wrapping any `AuthPlugin`
* No caching (per-request IdP call)

## Decision Outcome

> **Amended by [ADR-0019](./0019-upstream-proxy-engine.md)** (`cpt-cf-oagw-adr-upstream-proxy-engine`): Pingora left the platform, so the token cache is now OAGW's own bounded, sharded LRU with per-entry TTL (`infra/ttl_cache.rs`). The decision below (an internal, capacity- and TTL-bounded cache inside the plugin, zero TTL not cached, zeroize on eviction) stands; references to `pingora-memory-cache` and TinyUFO describe the original implementation.

Chosen option: "Plugin with internal `pingora-memory-cache` token cache". The plugin fetches a token from the IdP on the first request for a given (tenant, subject, config) tuple, caches it with a configurable TTL (default 5 minutes), and serves subsequent requests from cache until expiry.

Caching is an internal concern of the OAuth2 CC plugin — not a generic decorator. `ApiKeyAuthPlugin` and `NoopAuthPlugin` have no expensive fetch and carry no caching infrastructure. The `AuthPlugin` trait remains unchanged.

### Plugin Variants

| GTS Plugin ID | Client Auth Method |
|---|---|
| `gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1` | `Form` (credentials in request body) |
| `gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_basic.v1` | `Basic` (credentials in `Authorization` header) |
| `gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_private_key_jwt.v1` | `PrivateKeyJwt` (signed JWT assertion, no secret; see the amendment below) |

All registered in `AuthPluginRegistry::with_builtins`. Only `auth_method` differs; all share the same cache configuration.

### Amendment: `private_key_jwt` client authentication

> **Amended by [#4520](https://github.com/constructorfabric/gears-rust/issues/4520)** (2026-11): the third variant above. The original text said `private_key_jwt` is not a value the plugin can hold; that no longer applies.

Some identity providers require the client to authenticate with a JWT signed by its private key (`private_key_jwt`, RFC 7523 §2.2, OIDC Core §9) instead of a client secret. `toolkit-auth` supports it since #4519 (`ClientCredentials::PrivateKeyJwt`).

Decisions:

- **Own GTS id, same plugin type.** `oauth2_client_cred_private_key_jwt.v1` is a third instance of `OAuth2ClientCredAuthPlugin`, parameterised by `ClientAuthMethod { Basic, Form, PrivateKeyJwt }` (formerly `SecretAuthMethod`). The variants differ only in where the client credential comes from and how it is presented, so the config parser has a branch and the token fetch, cache and error mapping are shared. A separate id, rather than a config switch on the existing ids, keeps the existing configs and their meaning unchanged.
- **Config.** `token_endpoint` / `issuer_url` (exactly one), `client_id_ref` and `scopes` as for the other variants; `private_key_ref` instead of `client_secret_ref`; `signing_alg`, one of `ES256`, `RS256`, `PS256` (the algorithms `toolkit-auth` signs with), exact case; optional `kid`, sent in the assertion header (1 to 256 characters, no control characters); optional `audience`, the assertion's `aud` when the identity provider expects something other than the token endpoint, such as its issuer (1 to 2048 characters, no control characters). Without `audience`, `aud` is the token endpoint the request goes to, the discovered one with `issuer_url`.
- **Checked on write.** `PrivateKeyJwtConfig::parse` (`domain/plugin/credentials.rs`) refuses a config with neither or both of `token_endpoint` and `issuer_url` (or one that is not an absolute URL), with `client_secret_ref` as well, without `private_key_ref` or `signing_alg`, with an unknown `signing_alg`, or a bad `kid` or `audience`; upstream create and update run it and answer `400` with a field violation on the key, and the plugin runs the same parser on each request. `signing_alg` is a closed enum, so a typo fails at write time, not on the first proxied request. The GTS instance carries the config as a JSON Schema (`config_schema`, generated from the same constants) for discovery; the write check is what enforces it. The client-secret variants keep their behaviour: their endpoint keys are still checked only per request.
- **Key in the credential store.** `private_key_ref` is a credential reference like `client_secret_ref`: it is in `CREDENTIAL_KEYS`, so writes check its syntax and that the writer can read it (`400 failed_precondition`, subject `auth.config.private_key_ref`), and requests read it as the caller, with the same per-subject access. The secret is an unencrypted PKCS#8 PEM (`-----BEGIN PRIVATE KEY-----`): for `RS256` and `PS256` an RSA key with the `rsaEncryption` algorithm identifier, for `ES256` a P-256 EC key with the public key embedded, as `openssl genpkey` writes it. The public key is not derived from the private scalar, so a key exported without it (`openssl ec -no_public`, some HSM/KMS exports) is refused. The base64 may be wrapped at any width or sit on one line: the gateway re-wraps it at 64 columns before decoding. An RSA key with the `id-RSASSA-PSS` identifier, as `openssl genpkey -algorithm RSA-PSS` writes it, is refused for `PS256` too; `openssl genpkey -algorithm RSA` writes a usable one. Inline key material in the config is not supported.
- **Unusable key is a misconfiguration.** A secret that is not a PKCS#8 PEM key of the type `signing_alg` needs (an RSA key with `ES256`, say) is `PluginError::InvalidConfig` (500 `internal`), with a message naming `auth.config.private_key_ref`, the algorithm and the key type it needs, logged as `auth plugin misconfigured`. Neither the message nor the response carries key material; the PEM is held in a `SecretString` until parsed, and `Debug` of the parsed key prints the algorithm and `kid` only.
- **Token cache unchanged.** The cached artefact is the access token, not the assertion; the TTL rule and the safety margin are as above. The cache key's `auth_method` segment is `private_key_jwt`, and its config hash covers `private_key_ref`, `signing_alg`, `kid` and `audience`, so configs that differ in any of them never share a token. The key value is not part of the key, as the secret value is not for the other variants: a rotated key under the same reference is used from the next cache miss.
- **Assertions.** `toolkit-auth` signs a new assertion for each token request (`iss` = `sub` = client id, `aud` = `audience` or else the token endpoint, 60 s lifetime, random `jti`) and disables HTTP retries of the token request, since a retry would replay the `jti`.
- **No certificate thumbprint.** The assertion header carries `alg`, `typ` and the optional `kid` only; `x5t` and `x5t#S256` are not supported. Identity providers that require a certificate thumbprint in the header, such as Microsoft Entra ID, cannot use this plugin yet.
- **Not a signing plugin.** This plugin signs the token request, not the proxied request: its `signs` flag stays `false` and the ADR-0023 signing phase does not run for it.

Error mapping is unchanged (`token_error`): the identity provider unreachable or answering 408/429/5xx is `IdpUnavailable`; 400/401/403 to the token request (the assertion rejected) is `AuthFailed`; an unusable answer is `IdpInvalidResponse`; `TokenError::ConfigError` and `TokenError::ClientAssertion` are `InvalidConfig`.

### Plugin Config (ctx.config keys)

| Key | Required | Description |
|---|---|---|
| `token_endpoint` | Mutually exclusive with `issuer_url` | Direct token endpoint URL |
| `issuer_url` | Mutually exclusive with `token_endpoint` | OIDC issuer URL (Discovery) |
| `client_id_ref` | Yes | `cred://` reference for `client_id` |
| `client_secret_ref` | Yes (`Form`, `Basic`); refused (`PrivateKeyJwt`) | `cred://` reference for `client_secret` |
| `private_key_ref` | Yes (`PrivateKeyJwt` only) | `cred://` reference for the signing key (PKCS#8 PEM) |
| `signing_alg` | Yes (`PrivateKeyJwt` only) | `ES256`, `RS256` or `PS256` |
| `kid` | No (`PrivateKeyJwt` only) | `kid` header of the assertion |
| `audience` | No (`PrivateKeyJwt` only) | `aud` of the assertion; default: the token endpoint |
| `scopes` | No | Space-separated OAuth2 scopes |

### Gear-Level Configuration (OagwConfig)

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
        auth_method.tag(),
        hash_config(&ctx.config),
    )
}
```

The key includes:
- `subject_tenant_id` — cross-tenant isolation
- `subject_id` — cross-subject isolation for CredStore `private` sharing mode
- `auth_method` — prevents collisions if the Form, Basic and `private_key_jwt` plugins share identical config
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
  │    (private_key_jwt: resolve_secret(private_key_ref), parse the PEM for signing_alg)
  ├─ fetch_token(OAuthClientConfig)    → FetchedToken { bearer, expires_in }
  ├─ ttl = min(config_ttl, expires_in − 30s safety margin)
  ├─ cache.put(&key, CachedToken { key, token }, ttl)
  ├─ Inject Authorization: Bearer <token> into ctx.headers
  └─ Return Ok(())
```

Failed token fetches are **not** cached — the next request for the same key retries the IdP.

### Plugin Implementation

`ClientAuthMethod { Basic, Form, PrivateKeyJwt }` is the plugin's own enum
(`SecretAuthMethod { Basic, Form }` before the amendment above). The secret
methods map to `toolkit_auth::ClientCredentials::SecretBasic` / `SecretPost`,
`PrivateKeyJwt` to `ClientCredentials::PrivateKeyJwt` with the key read from
`private_key_ref`, when building the `OAuthClientConfig`.
`ClientAuthMethod::tag()` gives the cache-key segment (`"basic"` / `"form"` /
`"private_key_jwt"`).

```rust
use pingora_memory_cache::MemoryCache;

pub struct OAuth2ClientCredAuthPlugin {
    credstore: Arc<dyn CredStoreClientV1>,
    auth_method: ClientAuthMethod,
    http_config: Option<toolkit_http::HttpClientConfig>,
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
        token_http_config: Option<toolkit_http::HttpClientConfig>,
        token_cache_config: TokenCacheConfig,
    ) -> Self {
        // ... apikey and noop plugins unchanged ...

        for (plugin_id, method) in [
            (OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, ClientAuthMethod::Form),
            (OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID, ClientAuthMethod::Basic),
            (OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID, ClientAuthMethod::PrivateKeyJwt),
        ] {
            let plugin = OAuth2ClientCredAuthPlugin::new(
                credstore.clone(),
                method,
                token_cache_config.ttl,
                token_cache_config.capacity,
            );
            // ... register under plugin_id ...
        }
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
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1",
    "config": {
      "token_endpoint": "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token",
      "client_id_ref": "cred://ms-graph-client-id",
      "client_secret_ref": "cred://ms-graph-client-secret",
      "scopes": "https://graph.microsoft.com/.default"
    }
  }
}
```

`private_key_jwt` variant (the key is a PKCS#8 PEM secret in the credential store):

```json
{
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_private_key_jwt.v1",
    "config": {
      "token_endpoint": "https://idp.example.com/oauth2/token",
      "client_id_ref": "cred://vendor-client-id",
      "private_key_ref": "cred://vendor-signing-key",
      "signing_alg": "RS256",
      "kid": "vendor-2026-11",
      "scopes": "vendor.read"
    }
  }
}
```

OIDC Discovery variant (endpoint resolved automatically):

```json
{
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_basic.v1",
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

### Consequences

#### Positive

- Good, because it eliminates per-request IdP calls — cached tokens served in microseconds (vs 100–500ms)
- Good, because it reduces IdP rate-limit pressure — one call per unique (tenant, subject, config) per TTL window
- Good, because cross-tenant and cross-subject isolation is guaranteed by cache key design + `CachedToken` key verification
- Good, because `SecretString` (`ZeroizeOnDrop`) securely zeroes token buffers on cache eviction
- Good, because failed token fetches are not cached — transient IdP errors self-heal on the next request
- Good, because no changes to the `AuthPlugin` trait — existing plugins (`ApiKeyAuthPlugin`, `NoopAuthPlugin`) unchanged
- Good, because `pingora-memory-cache` aligns with planned Pingora adoption (S3-FIFO + TinyLFU eviction, stampede protection)
- Good, because `expires_in`-aware cache TTL via `min(config_ttl, expires_in − 30s)` prevents serving tokens near expiry when the IdP issues short-lived tokens
- Good, because no orphaned background tasks — `fetch_token()` performs a single HTTP exchange and returns; no `TokenWatcher` is spawned

#### Negative

- Bad, because the plugin is no longer stateless — it carries an in-memory cache with security-sensitive material
- Bad, because CredStore lookups still happen on every cache miss (not cached separately)
- Bad, because there is no automatic recovery from upstream 401 — the Data Plane has no metadata to decide whether retrying with fresh credentials is meaningful (see [Retry on Upstream 401 (Deferred)](#retry-on-upstream-401-deferred))
- Neutral, because `TinyUfo`'s lazy expiry means tokens may linger in memory slightly past TTL until the next `get()` check

#### Risks

- **CredStore unreachable**: returns `PluginError::CredentialStoreUnavailable` (503 `CREDENTIAL_STORE_UNAVAILABLE`) on cache miss; cached tokens continue to be served until TTL expiry
- **IdP unavailable**: `fetch_token()` fails with a transport error, 408, 429 or 5xx → `PluginError::IdpUnavailable` (503 `IDENTITY_PROVIDER_UNAVAILABLE`); not cached; next request retries
- Amended by [#4985](https://github.com/constructorfabric/gears-rust/issues/4985): both cases were `PluginError::Internal` (401 `AUTH_PLUGIN_INTERNAL`) before; see the OAGW DESIGN "Credential Failures" for the full mapping
- **Hash collision in TinyUfo**: mitigated by `CachedToken` key verification — a collision returns a cache miss, never another tenant's token

#### Known Residual Plaintext

- The `format!("Bearer {}", token.expose())` string in `ctx.headers` is a plain `String` — not zeroed, but short-lived (scoped to the request).
- Inside `OAuthTokenSource::request_token()`, the access token is a plain `String` — not zeroed. However, the source is created, used once, and dropped immediately by `fetch_token()`, so the plaintext lifetime is limited to the fetch call.
- The long-lived cache entry itself IS zeroed on eviction via `SecretString`'s `ZeroizeOnDrop`. `pingora-memory-cache` uses flurry/seize for deferred reclamation, so there is a small delay between eviction and actual `Drop`. For bearer tokens with ~1 hour TTL, this delay (milliseconds to seconds) is negligible.

### Confirmation

Code review confirms: `OAuth2ClientCredAuthPlugin` implemented in `oagw/src/infra/plugin/oauth2_client_cred_auth.rs` using `pingora_memory_cache::MemoryCache<String, CachedToken>`, `toolkit_auth::oauth2::fetch_token`, and the `min(config_ttl, expires_in − 30s)` TTL rule. The `Form`, `Basic` and `PrivateKeyJwt` variants are registered in `AuthPluginRegistry::with_builtins` (`oagw/src/infra/plugin/registry.rs`) under `OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID`, `OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID` and `OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID`. The multi-tenant isolation and cache behaviour are covered by unit tests in the same module (`cache_hit_skips_idp_call`, `different_tenant_id_gets_separate_cache_entry`, `cache_does_not_leak_token_across_subjects`, `failed_auth_does_not_pollute_cache`). The `private_key_jwt` variant is covered by the unit tests there (assertion fields, key and config errors, `kid` and key-reference cache separation; key loading, the key type a refused key needs and `Debug` without key material also under the `fips` feature) and by `oagw/src/infra/plugin/private_key_jwt_tests.rs` through the gateway (assertion signature and claims verified with the public key for each algorithm, a configured `audience` as `aud`, the bearer token on the upstream request, one token request for two proxied requests, separate tokens per `kid`, key reference, `audience` and algorithm, write-time refusals, cross-subject isolation of the key reference, and no key material in responses or in any crate's log events).

## Pros and Cons of the Options

### Internal `pingora-memory-cache` token cache

The plugin owns a `MemoryCache<String, CachedToken>` and manages token lifetime itself.

* Good, because caching lives only where an expensive fetch exists (no cost for `ApiKey`/`Noop`)
* Good, because the `AuthPlugin` trait stays unchanged
* Good, because `pingora-memory-cache` aligns with the planned Pingora data plane
* Bad, because the plugin becomes stateful and holds security-sensitive material in memory

### Generic caching decorator over `AuthPlugin`

A wrapper type caches the output of any `AuthPlugin`.

* Good, because caching logic is written once and reused
* Bad, because most plugins (`ApiKey`, `Noop`) have nothing expensive to cache — the abstraction is dead weight
* Bad, because credential-injection outputs are not uniformly cacheable (static vs rotating), so the decorator needs per-plugin policy anyway
* Bad, because it complicates the trait/registry wiring for no MVP benefit

### No caching (per-request IdP call)

Fetch a fresh token from the IdP on every proxied request.

* Good, because it is the simplest possible implementation and always current
* Bad, because it adds 100–500ms latency to every request
* Bad, because it risks tripping IdP rate limits under load

## Out of Scope

- **Authorization Code Grant** (RFC 6749 §4.1): Requires user consent, redirect callbacks (OAuth Router), and refresh token storage across data centers. Separate ADR needed.

## Future Considerations

- **Retry on upstream 401**: The `AuthPlugin` trait needs to return a richer response so the Data Plane can determine whether retrying with fresh credentials is meaningful. This requires revisiting the trait signature (e.g. returning metadata alongside injected headers). See [Retry on Upstream 401 (Deferred)](#retry-on-upstream-401-deferred).
- **Event-driven cache invalidation**: Once OAGW has access to the Event Broker, consuming `oauth_client.updated` and `oauth_client.deleted` events to trigger immediate cache eviction would reduce the staleness window from TTL to event propagation latency (~seconds).
- ~~**Per-IdP TTL from token response**~~: Implemented — `fetch_token()` returns `expires_in` from the IdP response, and the plugin caches with `min(config_ttl, expires_in − 30s safety margin)`. Tokens with `expires_in ≤ 30s` are not cached.

## Related ADRs

- [ADR: Plugin System](./0003-plugin-system.md) — `AuthPlugin` trait and execution model
- [ADR: Component Architecture](./0001-component-architecture.md) — `DataPlaneService`, `AuthPluginRegistry`
- [ADR: Control Plane Caching](./0007-data-plane-caching.md) — Cache invalidation pattern (superseded by ADR-0020)
- [ADR: Proxy Configuration Cache](./0020-proxy-config-cache.md) — Per-replica proxy configuration cache, a separate cache from the token cache
- [ADR: State Management](./0008-state-management.md) — Cache eviction and state coordination
- [ADR: Request Signing](./0023-request-signing.md) — The signing phase; the `private_key_jwt` variant signs its token request only and does not use it

## References

- [RFC 6749: OAuth 2.0 Authorization Framework](https://datatracker.ietf.org/doc/html/rfc6749) — §4.4 Client Credentials Grant
- [RFC 7523: JWT Profile for OAuth 2.0 Client Authentication](https://datatracker.ietf.org/doc/html/rfc7523) — §2.2 `private_key_jwt`
- [OpenID Connect Discovery 1.0](https://openid.net/specs/openid-connect-discovery-1_0.html) — Token endpoint discovery
- [pingora-memory-cache](https://github.com/cloudflare/pingora/tree/main/pingora-memory-cache) — S3-FIFO + TinyLFU eviction, cache stampede protection
- `libs/toolkit-auth` (`oauth2/config.rs`, `oauth2/source.rs`, `oauth2/token.rs`, `oauth2/fetch.rs`) — Token management library
- `oagw/src/infra/plugin/apikey_auth.rs` — Reference `AuthPlugin` implementation

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)
- **Related ADR**: [ADR: Plugin System](./0003-plugin-system.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-auth-injection` — Auth plugins handle credential injection
* `cpt-cf-oagw-fr-plugin-system` — OAuth2 CC plugin registered as a built-in auth plugin
* `cpt-cf-oagw-fr-builtin-plugins` — Built-in plugin implementation with internal token caching
