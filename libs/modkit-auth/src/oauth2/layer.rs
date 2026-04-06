use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use http::header::{AUTHORIZATION, HeaderName};
use http::{HeaderValue, Request, Response};
use tower::{Layer, Service};

use super::token::Token;
use modkit_http::HttpError;

/// Tower layer that injects a bearer token into outbound HTTP requests.
///
/// Wraps an [`Token`] handle and sets the `Authorization: Bearer <token>`
/// header (or a custom header) on every request before forwarding it to the
/// inner service.
#[derive(Clone, Debug)]
pub struct BearerAuthLayer {
    token: Token,
    header_name: HeaderName,
}

impl BearerAuthLayer {
    /// Create a layer that injects `Authorization: Bearer <token>`.
    #[must_use]
    pub fn new(token: Token) -> Self {
        Self {
            token,
            header_name: AUTHORIZATION,
        }
    }

    /// Create a layer that injects `<header_name>: Bearer <token>`.
    #[must_use]
    pub fn with_header_name(token: Token, header_name: HeaderName) -> Self {
        Self { token, header_name }
    }
}

impl<S> Layer<S> for BearerAuthLayer {
    type Service = BearerAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BearerAuthService {
            inner,
            token: self.token.clone(),
            header_name: self.header_name.clone(),
        }
    }
}

/// Tower service that injects a bearer token header before forwarding the
/// request to the inner service.
///
/// Created by [`BearerAuthLayer`].
#[derive(Clone, Debug)]
pub struct BearerAuthService<S> {
    inner: S,
    token: Token,
    header_name: HeaderName,
}

impl<S, B, ResBody> Service<Request<B>> for BearerAuthService<S>
where
    S: Service<Request<B>, Response = Response<ResBody>, Error = HttpError>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
    B: Send + 'static,
    ResBody: Send + 'static,
{
    type Response = Response<ResBody>;
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<Response<ResBody>, HttpError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        let mut bearer_value = match self.token.get() {
            Ok(secret) => {
                let raw = zeroize::Zeroizing::new(format!("Bearer {}", secret.expose()));
                match HeaderValue::from_str(&raw) {
                    Ok(v) => v,
                    Err(e) => return Box::pin(async { Err(HttpError::InvalidHeaderValue(e)) }),
                }
            }
            Err(e) => {
                return Box::pin(async { Err(HttpError::Transport(Box::new(e))) });
            }
        };
        bearer_value.set_sensitive(true);

        req.headers_mut()
            .insert(self.header_name.clone(), bearer_value);

        // Clone-swap pattern (Tower Service contract).
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move { inner.call(req).await })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "layer_tests.rs"]
mod tests;
