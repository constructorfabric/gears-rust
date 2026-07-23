# ADR: OAuth2 Authorization Code Auth Plugin

- **Status**: Draft (not final - see [Open Questions](#open-questions))
- **Date**: 2026-07-17
- **Deciders**: OAGW Team

> **This is a draft.** It is the first coherent cut of the design, not a settled
> or implementation-ready decision. Several load-bearing choices are still open
> (see [Open Questions](#open-questions)) and are expected to change after
> review. Follow-up ADRs are likely.

## Open Questions

These must be resolved before implementation; each can change the shape of the
design.

1. **Multi-instance pending store.** `authorize` and the callback are separate
   requests that may reach different OAGW instances. Does the pending store
   become a shared ephemeral backend, or does the BFF route callbacks to the
   issuing instance by `state`? Unresolved.
2. **Callback exposure policy.** We may decide to never expose OAGW's callback
   publicly. The internal/BFF-relay path is designed to stand alone, but the
   relay contract with the BFF (path mapping, header/`code` handling, session
   binding) is not specified here.
3. **Stateless-`state` variant.** A pre-registered client plus a self-contained,
   encrypted `state` would remove the pending store and the multi-instance
   constraint entirely, at the cost of dropping dynamic client registration.
   Evaluate before committing to the stateful shape.
4. **Durable CredStore backend.** This design assumes a durable, write-capable
   CredStore backend with `Secrets:Write` enforcement. That backend is a
   prerequisite, not part of this design, and its absence blocks implementation.
5. **`toolkit-auth` additions.** The authorization-code primitives
   (discovery / DCR / PKCE / exchange / refresh) are named as needed additions
   but are not yet designed in detail; their exact API is open.
6. **Re-auth challenge shape.** Modeling the consumer-facing `401` as a
   `WWW-Authenticate` challenge is proposed, but the exact challenge parameters
   and how the `authorize` action is referenced still need to be pinned and
   round-trip-tested.

## Context and Problem Statement

OAGW authenticates outbound proxy requests to upstreams using built-in auth
plugins. The existing plugins cover credentials that OAGW can supply on its own:
static material (`apikey`, `basic`, `bearer`) and the OAuth2 **Client
Credentials** grant, where OAGW itself is the OAuth client (see
[ADR: OAuth2 Client Credentials Auth Plugin](./adr-oauth2-client-credentials-auth-plugin.md)).

A distinct class of upstreams requires a token minted **on behalf of an end
user** rather than the service - the OAuth2 **Authorization Code** grant
(RFC 6749 §4.1) with PKCE (RFC 7636). This is the standard delegated-access flow
behind consumer "Sign in with ..." integrations and per-user SaaS/MCP APIs.
OAGW needs a plugin that injects a per-user bearer token on proxied requests,
refreshes it transparently, and drives the one-time interactive consent required
to obtain it.

Two properties distinguish this grant from client-credentials and shape the
design:

1. **Out-of-band enrollment.** The token cannot be minted at proxy time; it
   requires a browser redirect and user consent that happen beforehand.
   Enrollment is a management interaction, not a data-plane one.
2. **A long-lived, per-user secret.** The result is a refresh token that must be
   stored per subject and survive process restarts.

OAGW acts as a **standard OAuth 2.1 client** toward the authorization server and
implements no bespoke protocol mechanics: authorization-server discovery
(RFC 8414 / RFC 9728), dynamic client registration (RFC 7591), PKCE, and token
exchange/refresh live in `toolkit-auth`; the plugin orchestrates them. This is
the same profile the MCP authorization specification mandates.

**Existing infrastructure**:

- `AuthPlugin` trait (`oagw/src/domain/plugin/mod.rs`) -
  `authenticate(&self, ctx: &mut AuthContext)`, with a
  `PluginError::AuthorizationRequired(resource)` variant that signals the caller
  must (re-)consent.
- `AuthPluginRegistry::with_builtins(...)` (`infra/plugin/registry.rs`) - the
  single wiring point for built-in plugins.
- `CredStoreClientV1` (`credstore-sdk`) - per-tenant secret storage with
  `get`/`put`/`delete`, `Private`/`Tenant`/`Shared` sharing, and an opaque
  `SecretValue`. The designated durable home for secret material.
- `ServiceGatewayClientV1` (`oagw-sdk`) - the cross-gear proxy/management client;
  fallible methods return `Result<_, CanonicalError>`, and `oagw-sdk::reason`
  carries the wire vocabulary consumers dispatch on.
- `toolkit-auth` (`libs/toolkit-auth`) - OAuth2 token management library.

## Decision Drivers

- **Standards-first**: OAGW is an ordinary OAuth 2.1 client; the token flow and
  the callback follow published RFCs, and the re-authorization signal uses a
  standard `WWW-Authenticate` challenge (RFC 6750 / RFC 9728).
- **Topology-agnostic**: work identically whether OAGW's OAuth callback is
  reached **directly** (OAGW exposed) or **relayed by the BFF** (OAGW internal).
  Nothing in the token or enrollment logic may depend on OAGW being publicly
  reachable - exposing the callback is an optional deployment choice that may be
  permanently disallowed on security grounds.
- **Minimal usage surface**: a consumer already calling `proxy` learns nothing
  new for the happy path and handles re-authorization through the existing
  `CanonicalError` channel; the cross-gear `ServiceGatewayClientV1` contract does
  not grow.
- **Credential isolation** (`cpt-cf-oagw-principle-cred-isolation`): OAGW does
  not become a secret store; token material is delegated to CredStore and held
  in-process only transiently.
- **Design-consistent storage**: durable material lives in CredStore within its
  defined API; transient state that needs expiry does not (CredStore does not
  provide automatic expiration).
- **Confidentiality**: PKCE verifiers, authorization codes, and tokens never
  cross the consumer boundary and never appear in logs.

## Decision Outcome

Introduce a built-in auth plugin, `OAuth2AuthCodeAuthPlugin`, an internal
`OAuthEnrollmentService`, and a standard OAuth **callback** endpoint on OAGW.
The data-plane plugin injects and refreshes the stored token; enrollment is a
textbook authorization-code flow in which OAGW is the client. The `AuthPlugin`
and `ServiceGatewayClientV1` traits are unchanged.

### Deployment topologies

OAGW is a standard OAuth client: it initiates the flow (`authorize`) and hosts
the redirect **callback** that exchanges the code. The end-user browser and the
authorization server are public; OAGW may or may not be. The callback handler is
**identical** in both topologies - only the configured `redirect_uri` and the
network path to the callback differ:

- **BFF + internal OAGW (default).** `redirect_uri` is a public BFF URL. The BFF
  **reverse-proxies** that callback request - carrying `code` and `state`
  unchanged - to OAGW's callback endpoint, and relays OAGW's redirect response
  back to the browser. The BFF is a dumb pass-through; it holds no tokens and
  runs no OAuth logic.
- **BFF + exposed OAGW (optional).** `redirect_uri` is OAGW's own public callback
  URL; the authorization server reaches it directly. Identical handler, no relay.

```mermaid
graph LR
    subgraph Public
      B[Browser]
      AS[Authorization Server]
      subgraph BFF["BFF (public ingress)"]
        CB[callback path]
      end
    end
    subgraph Internal
      G[calling gear]
      O["OAGW<br/>(authorize, callback, proxy)"]
    end
    B <--> BFF --> G --> O
    B <-->|consent| AS
    AS -->|redirect code+state| CB
    CB -.->|internal reverse-proxy| O
    AS -. exposed OAGW only .->|redirect code+state| O
    O -->|server-side: discovery / exchange / refresh| AS
```

Responsibilities:

| Actor | Responsibility |
|---|---|
| Calling gear | Call `proxy`; on the `WWW-Authenticate` re-auth challenge, surface it outward. Nothing OAuth-specific. |
| BFF (platform) | Trigger `authorize`, redirect the browser, and host/relay the callback path to OAGW; bind the browser session to `state`. Written once for all OAuth upstreams. |
| OAGW | Discovery, PKCE, client registration, code/refresh exchange, callback handling, token storage, bearer injection. Never depends on being publicly reachable. |

### Plugin variant

| GTS Plugin ID | Grant |
|---|---|
| `gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_auth_code.v1` | Authorization Code + PKCE (RFC 6749 §4.1, RFC 7636) |

Registered in `AuthPluginRegistry::with_builtins` alongside the
`oauth2_client_cred*` variants.

### Plugin config (`ctx.config` keys)

| Key | Required | Description |
|---|---|---|
| `resource` | No | Protected-resource URL for authorization-server discovery (RFC 9728 → RFC 8414). If omitted, discovered from the upstream's own `401` `WWW-Authenticate` metadata. |
| `client_id_ref` | No | `cred://` reference for a pre-registered `client_id`. Omit to use Dynamic Client Registration. |
| `client_secret_ref` | No | `cred://` reference for a confidential-client secret. Omit for public/PKCE clients. |
| `scope` | No | Space-separated scopes; intersected with what the server advertises. |

There is no per-upstream token-location key: the token's storage key is derived
from `(subject, upstream_id)`, so it cannot be misconfigured or collide.

### REST surface

| Method + path | Purpose |
|---|---|
| `POST /oagw/v1/upstreams/{id}/oauth/authorize` | Begin the flow (side-effecting): discovery + DCR + PKCE/`state`, returns `{authorization_url, state}`. |
| `GET  /oagw/v1/oauth/callback` | Standard OAuth redirect endpoint: `?code&state` → exchange, store token, redirect the browser to `return_to`. Reached directly or via BFF relay. |
| `GET  /oagw/v1/upstreams/{id}/oauth` | Status: `{connected, expires_at_unix}` (safe/idempotent). |
| `DELETE /oagw/v1/upstreams/{id}/oauth` | Revoke: delete the caller's stored token. |

There is no `complete` endpoint: the callback *is* the completion, exactly as in
a standard OAuth client. `authorize` is a `POST` because it has side effects
(a DCR network call, and minting + stashing single-use PKCE/`state`); those must
not ride the data-plane `401` or the safe `GET` status.

### Consumer contract (the usage surface)

A consumer calls `proxy` as for any upstream. When the calling subject has no
usable authorization, the proxy responds with `401` and a standard
`WWW-Authenticate` challenge; in the SDK this projects to a typed reason:

```rust
use oagw_sdk::reason::auth::FailureReason;

match oagw.proxy(ctx, req).await {
    Ok(resp) => resp,
    Err(e) => match FailureReason::from(&e) {
        // Carries the upstream id + a link to the `authorize` action — not a
        // pre-minted URL, so the data plane performs no side effects.
        FailureReason::AuthorizationRequired { upstream_id, authorize } => {
            return needs_consent(upstream_id, authorize);
        }
        _ => return Err(e),
    },
}
```

The BFF turns that signal into consent: `POST authorize` → open
`authorization_url` in the browser → the callback (relayed to OAGW) exchanges and
stores the token → the browser returns to `return_to` → retry `proxy`. A backend
caller with no user context simply propagates the error like any other
`Unauthenticated`. No new SDK methods; no HTTP-status inspection.

### Domain model

```rust
/// Transient. Held in the ephemeral pending store, keyed by CSRF `state`.
struct PendingAuthorization {
    upstream_id: Uuid,
    token_endpoint: String,
    client_id: String,
    client_secret: Option<SecretString>,
    code_verifier: SecretString,   // PKCE; never leaves OAGW
    redirect_uri: String,          // the value registered with the AS (BFF or OAGW)
    return_to: String,             // where the callback sends the browser afterwards
    scopes: Vec<String>,
}

/// Durable. Persisted in CredStore under a per-(subject, upstream) SecretRef.
struct UserTokenRecord {
    client_id: String,
    client_secret: Option<SecretString>,
    token_endpoint: String,
    access_token: SecretString,
    refresh_token: Option<SecretString>,
    expires_at_unix: i64,
    scope: Option<String>,
}
```

The callback reads `state` → pending → `redirect_uri`, and sends that same
`redirect_uri` in the token exchange (OAuth requires it to match). Because the
value comes from the pending record, the handler is identical whether it was
reached directly or via relay.

### Token storage — CredStore

The per-user token record is persisted in CredStore, the platform's durable,
encrypted secret store, using its defined write path:

- **Opaque value**: `UserTokenRecord` is serialized to JSON and stored as a
  `SecretValue` (opaque bytes) - CredStore-agnostic.
- **Per-user scope**: `SharingMode::Private`, keyed by
  `owner_id = SecurityContext.subject_id()`; a subject reads back only their own
  token, and cross-subject/cross-tenant isolation is enforced by CredStore.
- **Designed API**: `get` / `put` / `delete` are part of CredStore's ClientHub
  contract; OAGW uses them through `CredStoreClientV1`.
- **Storage key**: OAGW maps `(subject, upstream_id)` to a deterministic
  `SecretRef`; the plugin and enrollment service never handle raw refs.

CredStore does not auto-expire secrets, and none is needed: the record carries
`expires_at_unix`, the plugin refreshes proactively, and revocation is an
explicit `delete`. A thin internal `UserTokenStore` seam wraps these calls and
the key derivation.

### Pending-state storage — ephemeral, OAGW-owned

`PendingAuthorization` is short-lived, single-use, and requires a TTL. Because
automatic expiration is outside CredStore's scope, it is held in OAGW's own
in-memory, TTL'd store (default 5 minutes), consistent with the
`pingora-memory-cache` the client-credentials plugin uses. `take(state)` is
consuming, so a `state` cannot be replayed.

> **Multi-instance requirement.** `authorize` and the callback are separate
> requests that may reach different OAGW instances (the BFF relays the callback).
> The pending store must therefore be shared across instances, or the BFF must
> route the callback by `state` to the instance that issued it. An in-process
> store is correct only for a single instance.

### redirect_uri and return_to

`redirect_uri` is deployment configuration (a value or allowlist), never taken
from the caller - closing open-redirect / token-exfiltration vectors. It is the
BFF callback URL (internal topology) or OAGW's own callback URL (exposed
topology). `return_to` (where the callback lands the browser afterwards) is
supplied by the consumer and validated against an allowlist. Both are captured at
`authorize` and stored in the pending record.

### Re-authorization signal — standard challenge

`PluginError::AuthorizationRequired` maps at the proxy boundary to `401` with a
standard `WWW-Authenticate: Bearer` challenge (RFC 6750) whose parameters name
the upstream resource and point to OAGW's `authorize` action; OAGW here is the
protected resource. In the SDK this becomes
`CanonicalError::Unauthenticated { reason = AUTHORIZATION_REQUIRED }` with
`resource_name = upstream_id`, projected to
`FailureReason::AuthorizationRequired { upstream_id, authorize }`. The challenge
is static (no side effects); the side-effecting `begin` runs only when the
consumer calls `authorize`. A round-trip test pins the wire value, as for the
other reasons.

Symmetrically, OAGW discovers the *upstream's* authorization server from the
upstream's own `401` `WWW-Authenticate` + protected-resource metadata (RFC 9728)
when `resource` is not configured - OAGW consuming the standard challenge as a
client.

### Enrollment flow

```mermaid
sequenceDiagram
    participant BFF as BFF
    participant O as OAGW
    participant P as Pending store
    participant AS as Authorization Server
    participant CS as CredStore

    BFF->>O: POST /upstreams/{id}/oauth/authorize {scope, return_to}
    O->>AS: discover + reuse/register client
    O->>O: PKCE + CSRF state
    O->>P: stash(state, pending{redirect_uri, return_to}, ttl=5m)
    O-->>BFF: {authorization_url, state}
    BFF->>AS: browser redirect → user consents
    AS-->>BFF: redirect_uri?code&state
    BFF->>O: GET /oagw/v1/oauth/callback?code&state  (relay; or AS direct if exposed)
    O->>P: take(state)  (single-use, validates CSRF)
    O->>AS: exchange code (+PKCE verifier, redirect_uri from pending)
    O->>CS: put(Private, UserTokenRecord)
    O-->>BFF: 302 → return_to
```

### Proxy-time flow

```text
authenticate(ctx):
  UserTokenStore.load(ctx, upstream_id)
    None                    -> Err(AuthorizationRequired(resource))
    valid (> now + margin)  -> inject Authorization: Bearer; Ok
    expired, has refresh    -> refresh_token(...)
        ok        -> UserTokenStore.save(rotated); inject Bearer; Ok
        rejected  -> UserTokenStore.delete(); Err(AuthorizationRequired(resource))
```

A 60-second refresh margin prevents a token lapsing mid-request; rotation
persists the new record.

### Library additions (toolkit-auth)

Standard authorization-code primitives are added under
`libs/toolkit-auth/src/oauth2/` (peers of the client-credentials `fetch`/`token`
modules): protected-resource/authorization-server discovery (RFC 9728 → 8414),
dynamic client registration (RFC 7591), PKCE and CSRF `state`, authorize-URL
construction, and code/refresh exchange.

### Registry integration

```rust
impl AuthPluginRegistry {
    pub fn with_builtins(
        credstore: Arc<dyn CredStoreClientV1>,
        token_store: Arc<dyn UserTokenStore>,
        token_http_config: Option<toolkit_http::HttpClientConfig>,
        token_cache_config: TokenCacheConfig,
    ) -> Self {
        // apikey, noop, oauth2_client_cred* unchanged ...
        let authcode = OAuth2AuthCodeAuthPlugin::new(token_store.clone(), token_http_config.clone());
        // register under the oauth2_auth_code GTS id
    }
}
```

### Upstream configuration example

```json
{
  "alias": "mcp.example.com",
  "server": {
    "endpoints": [{ "scheme": "https", "host": "mcp.example.com", "port": 443 }]
  },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "auth": {
    "type": "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_auth_code.v1",
    "config": {
      "resource": "https://mcp.example.com/",
      "scope": "profile email"
    }
  }
}
```

## Scenarios

- [Authorization-code enrollment](../scenarios/proxy-api/authentication/positive-9.11-oauth2-authorization-code-enrollment.md)
- [Bearer injection and transparent refresh](../scenarios/proxy-api/authentication/positive-9.12-oauth2-authorization-code-refresh.md)
- [Authorization required signals re-consent](../scenarios/proxy-api/authentication/negative-9.13-oauth2-authorization-code-authorization-required.md)
- [Refresh rejection forces re-authorization](../scenarios/proxy-api/authentication/negative-9.14-oauth2-authorization-code-refresh-rejected.md)
- [Cross-subject token isolation](../scenarios/proxy-api/authentication/negative-9.15-oauth2-authorization-code-cross-subject-isolation.md)

## Consequences

### Positive

- OAGW is a plain OAuth 2.1 client; the token flow and callback are pure RFC, and
  the re-auth signal is a standard `WWW-Authenticate` challenge.
- One callback handler serves both the internal (BFF-relayed) and exposed
  topologies; the token logic never depends on exposing OAGW, so exposure can be
  deferred or permanently refused as a policy decision.
- No `complete` endpoint - the custom broker surface reduces to a "start login"
  action plus ordinary status/revoke.
- Consumer usage is `proxy` + one typed error arm; the cross-gear SDK is
  unchanged.
- Refresh tokens are durable and encrypted per subject in CredStore; restarts do
  not force re-consent. Transient PKCE/CSRF state stays out of CredStore, in line
  with its non-goals.

### Negative

- Requires the BFF to reverse-proxy a callback path to OAGW in the internal
  topology.
- Correct multi-instance operation requires a shared pending store or
  `state`-routed callbacks.
- Requires a durable, write-capable CredStore backend.

### Risks

- **Pending state lost / wrong instance**: the callback fails to match `state`;
  the user restarts `authorize`. No durable data lost.
- **No DCR and no static client configured**: `authorize` fails fast before any
  redirect.
- **Refresh token revoked upstream**: refresh rejected → record deleted → next
  proxy call challenges for re-authorization. Self-healing.

## Out of Scope

- The BFF's callback relay/session binding and public ingress configuration.
- Device Authorization Grant (RFC 8628) and other non-redirect grants.
- Automatic secret rotation/expiration inside CredStore.

## Future Considerations

- Shared/distributed pending store for horizontally scaled OAGW.
- Cached dynamic client registrations per `(upstream, authorization-server)`.
- A stateless variant (pre-registered client + self-contained encrypted `state`)
  that removes both the pending store and the multi-instance constraint.
- Event-driven invalidation of token reads once OAGW consumes the Event Broker.

## Related ADRs

- [ADR: OAuth2 Client Credentials Auth Plugin](./adr-oauth2-client-credentials-auth-plugin.md) - sibling grant.
- [ADR: Plugin System](./adr-plugin-system.md) - `AuthPlugin` trait and execution model.
- [ADR: Component Architecture](./adr-component-architecture.md) - `DataPlaneService`, `AuthPluginRegistry`, `AppState`.
- [ADR: State Management](./adr-state-management.md) - cache/state coordination.

## References

- [RFC 6749: OAuth 2.0 Authorization Framework](https://datatracker.ietf.org/doc/html/rfc6749) - §4.1 Authorization Code Grant
- [RFC 6750: OAuth 2.0 Bearer Token Usage](https://datatracker.ietf.org/doc/html/rfc6750) - `WWW-Authenticate` challenge
- [RFC 7636: Proof Key for Code Exchange (PKCE)](https://datatracker.ietf.org/doc/html/rfc7636)
- [RFC 7591: OAuth 2.0 Dynamic Client Registration](https://datatracker.ietf.org/doc/html/rfc7591)
- [RFC 8414: OAuth 2.0 Authorization Server Metadata](https://datatracker.ietf.org/doc/html/rfc8414)
- [RFC 9728: OAuth 2.0 Protected Resource Metadata](https://datatracker.ietf.org/doc/html/rfc9728)
- [OAuth 2.0 for Browser-Based Apps](https://datatracker.ietf.org/doc/html/draft-ietf-oauth-browser-based-apps) - backend-for-frontend pattern
- `libs/toolkit-auth` - OAuth2 token management library
- `oagw/src/infra/plugin/oauth2_client_cred_auth.rs` - reference OAuth2 `AuthPlugin` implementation
