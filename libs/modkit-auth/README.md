# ModKit Auth

Authentication infrastructure for CyberFabric / ModKit.

## Overview

The `cf-modkit-auth` crate provides:

- **JWT / JWKS** — `KeyProvider` trait, `JwksKeyProvider` with background key refresh, `ValidationConfig`, standard claim constants
- **Token validation** — `TokenValidator` trait, `ClaimsError` / `AuthError` error types
- **Auth configuration** — `AuthConfig` (issuers, audiences, leeway, JWKS endpoint)
- **Outbound OAuth2 client credentials** — `Token` handle with automatic refresh and invalidation, `OAuthClientConfig`, `BearerAuthLayer` (tower), `HttpClientBuilderExt` for `modkit-http` integration
- **Auth metrics** — `AuthMetrics` trait with `LoggingMetrics` and `NoOpMetrics` implementations

## Outbound OAuth2 quick start

```rust
use modkit_auth::{HttpClientBuilderExt, OAuthClientConfig, SecretString, Token};
use modkit_http::HttpClientBuilder;

let token = Token::new(OAuthClientConfig {
    token_endpoint: Some("https://idp.example.com/oauth/token".parse()?),
    client_id: "my-service".into(),
    client_secret: SecretString::new("my-secret"),
    scopes: vec!["api.read".into()],
    ..Default::default()
})
.await?;

let client = HttpClientBuilder::new()
    .with_bearer_auth(token)
    .build()?;

// Every request gets Authorization: Bearer <token> automatically
let resp = client.get("https://api.example.com/resource").send().await?;
```

See `examples/` for more patterns (OIDC discovery, token invalidation, shared token, form auth).

## License

Licensed under Apache-2.0.
