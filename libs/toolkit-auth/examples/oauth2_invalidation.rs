//! Token invalidation on 401 (force refresh).
//!
//! `send()` returns `Ok(Response)` for ALL HTTP statuses, including 401.
//! Check the status on the response and call `token.invalidate()` to
//! force re-acquisition before retrying.
//!
//! NOTE: Requires a running IDP. Meant as an API reference, not a runnable demo.

use toolkit_auth::{HttpClientBuilderExt, OAuthClientConfig, SecretString, TokenProvider};
use toolkit_http::HttpClientBuilder;

/// Demonstrate provider construction and authenticated requests to an external service.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = TokenProvider::new(OAuthClientConfig {
        token_endpoint: Some("https://idp.example.com/oauth/token".parse()?),
        client_id: "my-service".into(),
        client_secret: SecretString::new("my-secret"),
        ..Default::default()
    })?;

    let client = HttpClientBuilder::new()
        .with_bearer_auth(token.clone())
        .build()?;

    let resp = client
        .get("https://api.example.com/resource")
        .send()
        .await?;

    // send() returns Ok for ALL HTTP statuses (including 401).
    // Check the status directly on the response.
    if resp.status() == http::StatusCode::UNAUTHORIZED {
        println!("Got 401 -- invalidating token...");
        token.invalidate().await?;
        // Retry with a fresh token (the client re-reads automatically)
        let _retry = client
            .get("https://api.example.com/resource")
            .send()
            .await?;
        println!("Retry succeeded with new token");
    }

    Ok(())
}
