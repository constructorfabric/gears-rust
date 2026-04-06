use http::header::HeaderName;
use tower::ServiceExt;

use super::layer::BearerAuthLayer;
use super::token::Token;

/// Extension trait for adding bearer auth to [`modkit_http::HttpClientBuilder`].
///
/// # Example
///
/// ```ignore
/// use modkit_auth::HttpClientBuilderExt;
///
/// let token = Token::new(config).await?;
/// let client = HttpClientBuilder::new()
///     .with_bearer_auth(token)
///     .build()?;
/// ```
pub trait HttpClientBuilderExt {
    /// Add `Authorization: Bearer <token>` injection to the HTTP client.
    #[must_use]
    fn with_bearer_auth(self, token: Token) -> Self;

    /// Add `<header_name>: Bearer <token>` injection to the HTTP client.
    #[must_use]
    fn with_bearer_auth_header(self, token: Token, header_name: HeaderName) -> Self;
}

impl HttpClientBuilderExt for modkit_http::HttpClientBuilder {
    fn with_bearer_auth(self, token: Token) -> Self {
        let layer = BearerAuthLayer::new(token);
        self.with_auth_layer(move |svc| {
            tower::ServiceBuilder::new()
                .layer(layer)
                .service(svc)
                .boxed_clone()
        })
    }

    fn with_bearer_auth_header(self, token: Token, header_name: HeaderName) -> Self {
        let layer = BearerAuthLayer::with_header_name(token, header_name);
        self.with_auth_layer(move |svc| {
            tower::ServiceBuilder::new()
                .layer(layer)
                .service(svc)
                .boxed_clone()
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "builder_ext_tests.rs"]
mod tests;
