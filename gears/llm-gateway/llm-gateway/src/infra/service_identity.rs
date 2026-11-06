//! Service identity adapter: the gear's own `SecurityContext` from
//! `AuthNResolverClient::exchange_client_credentials` (DESIGN §3.2 Quota
//! Guard "Execution").

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverClient, ClientCredentialsRequest};
use secrecy::SecretString;
use tokio::time::Instant;
use toolkit_security::SecurityContext;

use crate::config::ClientCredentialsConfig;
use crate::domain::ports::{IdentityContext, ServiceIdentity};

/// The last exchange attempt: when it started and, if it failed, its message
/// (the negative cache; never carries the secret).
struct Attempt {
    at: Instant,
    failure: Option<String>,
}

struct Cached {
    ctx: SecurityContext,
    exchanged_at: Instant,
    generation: u64,
}

#[derive(Default)]
struct State {
    cached: Option<Cached>,
    last_attempt: Option<Attempt>,
    /// Generation of the most recent successful exchange.
    generation: u64,
}

/// Exchanges the configured client credentials and caches the resulting
/// context for `refresh_interval` (the context carries no expiry).
///
/// Concurrent callers share one exchange. Exchanges are at least
/// `min_exchange_interval` apart, failures included: inside the interval a
/// caller gets the previous failure (or a rate-limit error) instead of
/// another call to the `IdP`. One exchange is bounded by `exchange_timeout`.
/// Every context carries a generation; `invalidate(generation)` drops the
/// cache only when that generation is still the cached one.
pub struct AuthNServiceIdentity {
    authn: Arc<dyn AuthNResolverClient>,
    client_id: String,
    client_secret: SecretString,
    scopes: Vec<String>,
    refresh_interval: Duration,
    min_exchange_interval: Duration,
    exchange_timeout: Duration,
    /// Short, never held across an await (so `invalidate` never waits on a
    /// hung exchange).
    state: Mutex<State>,
    /// Serializes exchanges; held across the exchange call.
    exchange: tokio::sync::Mutex<()>,
}

impl AuthNServiceIdentity {
    /// Identity for `credentials`, exchanged through `authn`.
    #[must_use]
    pub fn new(authn: Arc<dyn AuthNResolverClient>, credentials: &ClientCredentialsConfig) -> Self {
        Self {
            authn,
            client_id: credentials.client_id.clone(),
            client_secret: credentials.client_secret.clone(),
            scopes: credentials.scopes.clone(),
            refresh_interval: credentials.refresh_interval,
            min_exchange_interval: credentials.min_exchange_interval,
            exchange_timeout: credentials.exchange_timeout,
            state: Mutex::new(State::default()),
            exchange: tokio::sync::Mutex::new(()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A fresh cached context, or the reason no exchange may run now.
    fn ready(&self) -> Ready {
        let state = self.state();
        if let Some(cached) = state.cached.as_ref()
            && cached.exchanged_at.elapsed() < self.refresh_interval
        {
            return Ready::Cached(IdentityContext {
                ctx: cached.ctx.clone(),
                generation: cached.generation,
            });
        }
        match state.last_attempt.as_ref() {
            Some(attempt) if attempt.at.elapsed() < self.min_exchange_interval => {
                Ready::Blocked(attempt.failure.clone())
            }
            _ => Ready::Exchange,
        }
    }

    async fn do_exchange(&self) -> Result<SecurityContext, String> {
        let request = ClientCredentialsRequest {
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            scopes: self.scopes.clone(),
        };
        match tokio::time::timeout(
            self.exchange_timeout,
            self.authn.exchange_client_credentials(&request),
        )
        .await
        {
            Ok(Ok(result)) => Ok(result.security_context),
            Ok(Err(e)) => Err(format!("client credentials exchange failed: {e}")),
            Err(_) => Err(format!(
                "client credentials exchange timed out after {:?}",
                self.exchange_timeout
            )),
        }
    }
}

enum Ready {
    Cached(IdentityContext),
    /// Too soon after the last attempt; carries its failure, if it failed.
    Blocked(Option<String>),
    Exchange,
}

#[async_trait]
impl ServiceIdentity for AuthNServiceIdentity {
    async fn context(&self) -> anyhow::Result<IdentityContext> {
        // Waiters queue here and re-check afterwards: they read the result
        // (or the negative cache) of the exchange they waited for.
        let _exchanging = self.exchange.lock().await;
        match self.ready() {
            Ready::Cached(identity) => return Ok(identity),
            Ready::Blocked(Some(failure)) => anyhow::bail!("{failure}"),
            Ready::Blocked(None) => {
                anyhow::bail!("client credentials exchange rate-limited: retry later")
            }
            Ready::Exchange => {}
        }
        let outcome = self.do_exchange().await;
        let mut state = self.state();
        match outcome {
            Ok(ctx) => {
                state.generation += 1;
                let generation = state.generation;
                state.cached = Some(Cached {
                    ctx: ctx.clone(),
                    exchanged_at: Instant::now(),
                    generation,
                });
                state.last_attempt = Some(Attempt {
                    at: Instant::now(),
                    failure: None,
                });
                Ok(IdentityContext { ctx, generation })
            }
            Err(failure) => {
                state.last_attempt = Some(Attempt {
                    at: Instant::now(),
                    failure: Some(failure.clone()),
                });
                anyhow::bail!("{failure}")
            }
        }
    }

    async fn invalidate(&self, generation: u64) {
        let mut state = self.state();
        if state
            .cached
            .as_ref()
            .is_some_and(|cached| cached.generation == generation)
        {
            state.cached = None;
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_identity_tests.rs"]
mod service_identity_tests;
