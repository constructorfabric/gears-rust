use crate::error::HttpError;
use http::{HeaderValue, Request, Response};
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// Tower layer that adds User-Agent header to all requests
#[derive(Clone)]
pub struct UserAgentLayer {
    user_agent: HeaderValue,
}

impl UserAgentLayer {
    /// Create a new `UserAgentLayer` with the specified user agent string
    ///
    /// # Errors
    /// Returns `HttpError::InvalidHeaderValue` if the user agent string is not valid
    pub fn try_new(user_agent: impl AsRef<str>) -> Result<Self, HttpError> {
        let user_agent =
            HeaderValue::from_str(user_agent.as_ref()).map_err(HttpError::InvalidHeaderValue)?;
        Ok(Self { user_agent })
    }
}

impl<S> Layer<S> for UserAgentLayer {
    type Service = UserAgentService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        UserAgentService {
            inner,
            user_agent: self.user_agent.clone(),
        }
    }
}

/// Service that adds User-Agent header to requests
#[derive(Clone)]
pub struct UserAgentService<S> {
    inner: S,
    user_agent: HeaderValue,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for UserAgentService<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<ReqBody>) -> Self::Future {
        // Only add User-Agent if not already present
        if !req.headers().contains_key(http::header::USER_AGENT) {
            req.headers_mut()
                .insert(http::header::USER_AGENT, self.user_agent.clone());
        }
        self.inner.call(req)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "user_agent_tests.rs"]
mod tests;
