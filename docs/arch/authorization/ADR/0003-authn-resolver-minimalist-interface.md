---
status: accepted
date: 2026-01-30
decision-makers: Constructor Fabric Steering Committee
---

# AuthN Resolver Minimalist Interface

## Context and Problem Statement

AuthN Resolver is a gear that validates bearer tokens and produces `SecurityContext` for downstream gears (PEPs). The gear needs to define an interface for vendor-specific plugins that integrate with various Identity Providers (IdPs).

**Key question:** How prescriptive should the gear interface be? Should it define separate methods for different authentication mechanisms, or provide a minimal abstraction that leaves implementation details to plugins?

Different IdPs use different protocols and token formats:

- JWT tokens with local validation (signature verification via JWKS)
- Opaque tokens requiring introspection (RFC 7662)
- Hybrid approaches (JWT validation + introspection for enrichment)
- Custom protocols (mTLS, API keys, PASETO, vendor-specific flows)

## Decision Drivers

- **Vendor Neutrality** — Gears must integrate with any vendor's IdP without assuming specific protocols
- **Plugin Flexibility** — Plugins should choose validation strategies based on their IdP's capabilities
- **Separation of Concerns** — Gear defines *what* authentication produces, plugins define *how* tokens are validated
- **Caching Autonomy** — Different validation methods have different caching strategies (JWKS caching vs introspection result caching)
- **Future-Proofing** — New authentication methods should be addable without changing the main gear interface

## Considered Options

- **Option A**: Prescriptive interface with separate methods for each authentication mechanism
- **Option B**: Minimalist interface with single `authenticate` method

## Decision Outcome

Chosen option: **Option B — Minimalist interface**, because it provides maximum flexibility for vendor plugins while maintaining a clear contract for what authentication produces.

**Interface:**

```rust
#[async_trait]
pub trait AuthNResolverPluginClient: Send + Sync {
    async fn authenticate(&self, bearer_token: &str) -> Result<AuthenticationResult, AuthNResolverError>;
}
```

**What the gear specifies:**

- Output format: `AuthenticationResult` containing `SecurityContext`
- Error semantics: `AuthNResolverError` (invalid token, unauthorized, service unavailable, no plugin available)
- Security boundaries: token is a credential, must be handled securely

**What the gear does NOT specify:**

- Token format (JWT, opaque, custom)
- Validation method (local, introspection, hybrid)
- Claim structure (vendor-specific)
- Caching strategy (plugin decision)
- Discovery mechanisms (OIDC, custom)

**Amendment (#5566):** the token now arrives in an `AuthenticationRequest` (`authenticate(&self, request: &AuthenticationRequest)`), which also carries `RequestCacheControl` so callers can require a fresh validation or a result newer than a known `CacheTag`. Caching strategy remains the plugin's decision; a plugin with an authenticate cache must honour the request's refresh semantics.

### Consequences

**Good:**

- **Vendor neutrality** — Any IdP can be integrated without gear changes
- **Plugin flexibility** — Plugins implement exactly the validation logic their IdP requires
- **Caching autonomy** — Plugins implement caching strategies appropriate to their validation method
- **Future-proof** — New authentication methods (e.g., PASETO, WebAuthn) can be added as new plugins
- **Simple gear** — Gear code is minimal and stable

**Bad:**

- **Less guidance** — Plugin developers must understand their IdP's requirements without gear-level hints
- **Potential inconsistency** — Different plugins might handle edge cases differently

**Mitigations:**

- The current OIDC AuthN plugin design ([DESIGN.md](../../../../gears/system/authn-resolver/plugins/oidc-authn-plugin/docs/DESIGN.md)) provides canonical patterns
- Clear documentation of `SecurityContext` field semantics
- Explicit error type definitions guide plugin error handling

## Pros and Cons of the Options

### Option A: Prescriptive Interface

Gear defines separate methods for different authentication mechanisms:

```rust
trait AuthNResolverPluginClient {
    async fn validate_jwt(&self, token: &str) -> Result<Claims, Error>;
    async fn introspect(&self, token: &str) -> Result<IntrospectionResponse, Error>;
    async fn authenticate(&self, token: &str) -> Result<AuthenticationResult, Error>;
}
```

- Good, because provides clear guidance for common patterns
- Good, because gear can optimize for specific mechanisms
- Bad, because **assumes specific protocols** — what about mTLS, API keys, PASETO?
- Bad, because plugins for non-standard IdPs must shoehorn into predefined methods
- Bad, because adding new authentication methods requires gear interface changes
- Bad, because caching logic would need to be duplicated or abstracted separately

### Option B: Minimalist Interface

Gear defines single method, plugins handle all details:

```rust
trait AuthNResolverPluginClient {
    async fn authenticate(&self, bearer_token: &str) -> Result<AuthenticationResult, Error>;
}
```

- Good, because **maximum flexibility** — plugins implement exactly what their IdP needs
- Good, because **vendor neutral** — no assumption about protocols or token formats
- Good, because **stable interface** — new auth methods don't require gear changes
- Good, because **caching is plugin's decision** — optimal strategies per validation method
- Bad, because less guidance for plugin developers (mitigated by reference implementation)

## More Information

**Related Documentation:**

- [DESIGN.md](../DESIGN.md) — Authentication and authorization design specification
- [gears/system/authn-resolver/plugins/oidc-authn-plugin/docs/DESIGN.md](../../../../gears/system/authn-resolver/plugins/oidc-authn-plugin/docs/DESIGN.md) — OIDC AuthN Resolver plugin design
- [ADR 0002: Split AuthN and AuthZ Resolvers](./0002-split-authn-authz-resolvers.md) — Why AuthN and AuthZ are separate gears

**Standards References:**

- OpenID Connect Core 1.0: https://openid.net/specs/openid-connect-core-1_0.html
- RFC 7519 (JSON Web Token): https://datatracker.ietf.org/doc/html/rfc7519
- RFC 7662 (Token Introspection): https://datatracker.ietf.org/doc/html/rfc7662
