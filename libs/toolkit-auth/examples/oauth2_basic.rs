//! Direct token endpoint -- the simplest `OAuth2` client-credentials flow.
//!
//! This is the Rust equivalent of the Go pattern:
//!
//! ```go
//! tokenProvider, _ := tokenprovider.New(tokenURL, clientID, clientSecret, ...)
//! resolverClient, _ := tenantsresolver.NewClient(address, tokenProvider, ...)
//! ```
//!
//! NOTE: Requires a running IDP. Meant as an API reference, not a runnable demo.

use toolkit_auth::{HttpClientBuilderExt, OAuthClientConfig, SecretString, TokenProvider};
use toolkit_http::HttpClientBuilder;

/// Demonstrate provider construction and authenticated requests to an external service.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Step 1 -- create token provider (like tokenprovider.New in Go)
    let token = TokenProvider::new(OAuthClientConfig {
        token_endpoint: Some("https://idp.example.com/oauth/token".parse()?),
        client_id: "my-service".into(),
        client_secret: SecretString::new("my-secret"),
        scopes: vec!["tenants.read".into(), "tenants.write".into()],
        ..Default::default()
    })?;

    // Step 2 -- create HTTP client with automatic Bearer injection
    // (like tenantsresolver.NewClient(addr, tokenProvider, ...) in Go)
    let client = HttpClientBuilder::new().with_bearer_auth(token).build()?;

    // Step 3 -- every request gets Authorization: Bearer <token> automatically
    let _resp = client
        .get("https://resolver.example.com/api/v1/tenants")
        .send()
        .await?;

    println!("Request sent with auto-injected Bearer token");
    Ok(())
}
