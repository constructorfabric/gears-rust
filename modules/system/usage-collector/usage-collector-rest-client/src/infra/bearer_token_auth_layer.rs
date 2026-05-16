use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use authn_resolver_sdk::{AuthNResolverClient, AuthNResolverError, ClientCredentialsRequest};
use http::header::AUTHORIZATION;
use http::{HeaderValue, Request, Response};
use modkit_http::HttpError;
use secrecy::ExposeSecret;
use tower::{Layer, Service};

#[derive(Clone)]
pub struct BearerTokenAuthLayer {
    authn_client: Arc<dyn AuthNResolverClient>,
    credentials: Arc<ClientCredentialsRequest>,
}

impl BearerTokenAuthLayer {
    pub fn new(
        authn_client: Arc<dyn AuthNResolverClient>,
        credentials: ClientCredentialsRequest,
    ) -> Self {
        Self {
            authn_client,
            credentials: Arc::new(credentials),
        }
    }
}

impl<S> Layer<S> for BearerTokenAuthLayer {
    type Service = BearerTokenAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BearerTokenAuthService {
            inner,
            authn_client: Arc::clone(&self.authn_client),
            credentials: Arc::clone(&self.credentials),
        }
    }
}

pub struct BearerTokenAuthService<S> {
    inner: S,
    authn_client: Arc<dyn AuthNResolverClient>,
    credentials: Arc<ClientCredentialsRequest>,
}

impl<S: Clone> Clone for BearerTokenAuthService<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            authn_client: Arc::clone(&self.authn_client),
            credentials: Arc::clone(&self.credentials),
        }
    }
}

impl<S, B, ResBody> Service<Request<B>> for BearerTokenAuthService<S>
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
        let authn_client = Arc::clone(&self.authn_client);
        let credentials = Arc::clone(&self.credentials);

        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move {
            // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-2
            // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-2
            // Acquire a bearer token via OAuth2 client-credentials before each delivery /
            // module-config request. Both REST flows share this single Tower layer so the
            // same code implements step `inst-rem-2` (remote emit) and `inst-cfg-rem-2`
            // (module-config fetch).
            let auth_result = authn_client
                .exchange_client_credentials(&credentials)
                .await
                // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-3
                // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-4
                // Both transient AuthN failures (`inst-rem-3`) and permanent credential
                // rejection (`inst-rem-4`: `Unauthorized` / `NoPluginAvailable`) collapse to
                // `HttpError::Transport`, which the REST client maps to
                // `UsageCollectorError::ServiceUnavailable` so the outbox retries (the
                // RETURN actions `inst-rem-3a` / `inst-rem-4a` happen in `rest_client.rs`).
                // Permanent rejection is **not** auto-dead-lettered; operators recover via
                // the `WARN` log here plus retry-rate metrics.
                .map_err(|e| {
                    // Logged at DEBUG, not WARN: under an IdP outage the outbox can
                    // burst-drain pending records and would otherwise emit one identical
                    // WARN per attempt. Operators rely on outbox retry-rate metrics for
                    // the alerting signal; the per-attempt event stays around as
                    // correlation context (including the failing `client_id`).
                    tracing::debug!(
                        client_id = %credentials.client_id,
                        error = ?e,
                        "token acquisition failed",
                    );
                    HttpError::Transport(Box::new(e))
                })?;
            // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-4
            // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-3

            let token = auth_result.security_context.bearer_token().ok_or_else(|| {
                // Symmetric with the `exchange_client_credentials` failure arm above:
                // both transient outages and this misconfiguration collapse to
                // `HttpError::Transport` → `ServiceUnavailable` and retry forever at
                // the outbox. Without a log, the only operator-visible signal is a
                // retry-rate spike. WARN (not DEBUG) because — unlike a transient IdP
                // outage — this is a permanent setup error that needs human action,
                // so the per-attempt amplification under burst-drain is acceptable.
                tracing::warn!(
                    client_id = %credentials.client_id,
                    "client credentials exchange succeeded but SecurityContext has no bearer token",
                );
                HttpError::Transport(Box::new(AuthNResolverError::Internal(
                    "client credentials exchange succeeded but SecurityContext has no bearer token"
                        .to_owned(),
                )))
            })?;
            // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-2
            // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-2

            let raw = zeroize::Zeroizing::new(format!("Bearer {}", token.expose_secret()));
            let mut hv = HeaderValue::from_str(&raw).map_err(HttpError::InvalidHeaderValue)?;
            hv.set_sensitive(true);

            req.headers_mut().insert(AUTHORIZATION, hv);

            inner.call(req).await
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "bearer_token_auth_layer_tests.rs"]
mod bearer_token_auth_layer_tests;
