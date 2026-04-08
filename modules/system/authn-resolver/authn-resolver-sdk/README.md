# AuthN Resolver SDK

SDK crate for the AuthN Resolver module, providing public API contracts for authentication in CyberFabric.

## Overview

This crate defines the transport-agnostic interface for the AuthN Resolver module:

- **`AuthNResolverClient`** — Async trait for consumers (authenticate bearer tokens)
- **`AuthNResolverPluginClient`** — Async trait for plugin implementations
- **`AuthenticationResult`** — Result containing the validated `SecurityContext`
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

```rust
let result = authn.authenticate("eyJhbGciOiJSUzI1NiIs...").await?;
let ctx = result.security_context;

println!("Subject: {}", ctx.subject_id());
println!("Tenant: {}", ctx.subject_tenant_id());
println!("Scopes: {:?}", ctx.token_scopes());
```

### AuthenticationResult

```rust
pub struct AuthenticationResult {
    /// Contains: subject_id, subject_tenant_id, token_scopes, bearer_token
    pub security_context: SecurityContext,
}
```

The `SecurityContext` carries the authenticated identity through the request pipeline. The original bearer token is preserved for downstream PDP forwarding.

## Error Handling

```rust
use authn_resolver_sdk::AuthNResolverError;

match authn.authenticate(token).await {
    Ok(result) => { /* use result.security_context */ },
    Err(AuthNResolverError::Unauthorized(msg)) => { /* invalid/expired token */ },
    Err(AuthNResolverError::NoPluginAvailable) => { /* no AuthN plugin registered */ },
    Err(AuthNResolverError::ServiceUnavailable(msg)) => { /* plugin not ready */ },
    Err(AuthNResolverError::Internal(msg)) => { /* unexpected error */ },
}
```

## Implementing a Plugin

Implement `AuthNResolverPluginClient` and register with a GTS instance ID:

```rust
use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverPluginClient, AuthenticationResult, AuthNResolverError,
    ClientCredentialsRequest,
};

struct MyOidcPlugin { /* ... */ }

#[async_trait]
impl AuthNResolverPluginClient for MyOidcPlugin {
    async fn authenticate(&self, bearer_token: &str)
        -> Result<AuthenticationResult, AuthNResolverError> {
        // Validate token, extract claims, build SecurityContext
    }

    async fn exchange_client_credentials(&self, request: &ClientCredentialsRequest)
        -> Result<AuthenticationResult, AuthNResolverError> {
        // OAuth2 client_credentials flow → SecurityContext
    }
}
```

## License

Apache-2.0
