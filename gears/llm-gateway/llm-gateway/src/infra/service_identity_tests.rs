//! Service identity adapter: client-credentials exchange, caching and
//! refresh (DESIGN §3.2 Quota Guard "Execution": token from
//! `AuthNResolverClient::exchange_client_credentials`, cached and refreshed).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, ClientCredentialsRequest,
};
use secrecy::ExposeSecret;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::AuthNServiceIdentity;
use crate::config::ClientCredentialsConfig;
use crate::domain::ports::ServiceIdentity;

const SECRET: &str = "s3cr3t-value-for-tests";

/// `(client_id, client_secret, scopes)` of one exchange.
type Request = (String, String, Vec<String>);

/// Exchange fake: the n-th successful exchange yields subject `100 + n`.
struct FakeAuthN {
    calls: AtomicUsize,
    fail_first: AtomicUsize,
    delay: Duration,
    requests: Mutex<Vec<Request>>,
}

impl FakeAuthN {
    fn new(fail_first: usize, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            fail_first: AtomicUsize::new(fail_first),
            delay,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AuthNResolverClient for FakeAuthN {
    async fn authenticate(
        &self,
        _bearer: &str,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::Internal("not used".to_owned()))
    }

    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().expect("requests").push((
            request.client_id.clone(),
            request.client_secret.expose_secret().to_owned(),
            request.scopes.clone(),
        ));
        tokio::time::sleep(self.delay).await;
        let failing = self
            .fail_first
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |f| f.checked_sub(1))
            .is_ok();
        if failing {
            return Err(AuthNResolverError::TokenAcquisitionFailed(
                "idp down".to_owned(),
            ));
        }
        Ok(AuthenticationResult {
            security_context: SecurityContext::builder()
                .subject_id(Uuid::from_u128(100 + n as u128))
                .subject_tenant_id(Uuid::from_u128(3))
                .subject_type("service")
                .build()
                .expect("context"),
        })
    }
}

fn credentials(refresh: Duration) -> ClientCredentialsConfig {
    serde_json::from_value(serde_json::json!({
        "client_id": "llm-gateway",
        "client_secret": SECRET,
        "scopes": ["quota.settle"],
        "refresh_interval": format!("{}s", refresh.as_secs()),
    }))
    .expect("credentials deserialize")
}

fn identity(authn: &Arc<FakeAuthN>, refresh: Duration) -> AuthNServiceIdentity {
    AuthNServiceIdentity::new(authn.clone(), &credentials(refresh))
}

#[tokio::test]
async fn the_configured_credentials_and_scopes_are_exchanged() {
    let authn = FakeAuthN::new(0, Duration::ZERO);
    let ctx = identity(&authn, Duration::from_secs(300))
        .context()
        .await
        .expect("exchanged");

    assert_eq!(ctx.ctx.subject_id(), Uuid::from_u128(100));
    let requests = authn.requests.lock().expect("requests").clone();
    assert_eq!(
        requests,
        [(
            "llm-gateway".to_owned(),
            SECRET.to_owned(),
            vec!["quota.settle".to_owned()]
        )]
    );
}

#[tokio::test(start_paused = true)]
async fn the_context_is_cached_until_the_refresh_interval_elapses() {
    let authn = FakeAuthN::new(0, Duration::ZERO);
    let identity = identity(&authn, Duration::from_secs(300));

    let first = identity.context().await.expect("first");
    tokio::time::advance(Duration::from_secs(299)).await;
    let cached = identity.context().await.expect("cached");
    assert_eq!(authn.calls(), 1, "no second exchange inside the interval");
    assert_eq!(cached.ctx.subject_id(), first.ctx.subject_id());
    assert_eq!(cached.generation, first.generation);

    tokio::time::advance(Duration::from_secs(2)).await;
    let refreshed = identity.context().await.expect("refreshed");
    assert_eq!(authn.calls(), 2, "refreshed after the interval");
    assert_eq!(refreshed.ctx.subject_id(), Uuid::from_u128(101));
    assert!(refreshed.generation > first.generation);
}

#[tokio::test(start_paused = true)]
async fn concurrent_callers_share_one_exchange() {
    let authn = FakeAuthN::new(0, Duration::from_millis(50));
    let identity = Arc::new(identity(&authn, Duration::from_secs(300)));

    let callers: Vec<_> = (0..8)
        .map(|_| {
            let identity = identity.clone();
            tokio::spawn(async move { identity.context().await })
        })
        .collect();
    for caller in callers {
        caller.await.expect("joined").expect("context");
    }
    assert_eq!(authn.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_failed_exchange_is_an_error_and_is_negative_cached_for_the_min_interval() {
    let authn = FakeAuthN::new(1, Duration::ZERO);
    let identity = identity(&authn, Duration::from_secs(300));

    let err = identity.context().await.expect_err("idp down");
    let text = format!("{err:#}");
    assert!(text.contains("credentials exchange failed"), "{text}");
    assert!(
        !text.contains(SECRET),
        "the secret never reaches an error: {text}"
    );

    // Inside the interval: the same failure, no second call to the IdP.
    let again = identity.context().await.expect_err("negative-cached");
    assert_eq!(format!("{again:#}"), text);
    assert!(!format!("{again:#}").contains(SECRET));
    assert_eq!(authn.calls(), 1);

    // The failure is not cached beyond the interval.
    tokio::time::advance(Duration::from_millis(1_001)).await;
    identity
        .context()
        .await
        .expect("exchanges again after the interval");
    assert_eq!(authn.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn invalidate_drops_the_cached_context_so_the_next_call_exchanges_again() {
    let authn = FakeAuthN::new(0, Duration::ZERO);
    let identity = identity(&authn, Duration::from_secs(300));

    let first = identity.context().await.expect("first");
    tokio::time::advance(Duration::from_millis(1_001)).await;
    identity.invalidate(first.generation).await;
    let second = identity.context().await.expect("re-exchanged");

    assert_eq!(
        authn.calls(),
        2,
        "exchanged again inside the refresh interval"
    );
    assert_ne!(first.ctx.subject_id(), second.ctx.subject_id());
    assert!(second.generation > first.generation);
    identity.context().await.expect("cached again");
    assert_eq!(authn.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_stale_invalidate_does_not_clear_a_newer_context() {
    let authn = FakeAuthN::new(0, Duration::ZERO);
    let identity = identity(&authn, Duration::from_secs(300));

    let first = identity.context().await.expect("first");
    tokio::time::advance(Duration::from_millis(1_001)).await;
    identity.invalidate(first.generation).await;
    let second = identity.context().await.expect("second");

    // A late refusal of the first generation arrives.
    identity.invalidate(first.generation).await;
    let still = identity.context().await.expect("still cached");
    assert_eq!(still.generation, second.generation);
    assert_eq!(authn.calls(), 2, "no third exchange");
}

#[tokio::test(start_paused = true)]
async fn an_invalidated_context_is_not_re_exchanged_inside_the_min_interval() {
    let authn = FakeAuthN::new(0, Duration::ZERO);
    let identity = identity(&authn, Duration::from_secs(300));

    let first = identity.context().await.expect("first");
    identity.invalidate(first.generation).await;
    let err = identity.context().await.expect_err("rate-limited");
    assert!(format!("{err:#}").contains("rate-limited"), "{err:#}");
    assert_eq!(authn.calls(), 1);

    tokio::time::advance(Duration::from_millis(1_001)).await;
    identity
        .context()
        .await
        .expect("exchanged after the interval");
    assert_eq!(authn.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn repeated_invalidation_is_bounded_by_the_min_interval_not_the_retry_count() {
    let authn = FakeAuthN::new(0, Duration::ZERO);
    let identity = identity(&authn, Duration::from_secs(300));

    // A settle loop that is refused every time: 200 retries, 100 ms apart.
    let started = tokio::time::Instant::now();
    for _ in 0..200 {
        if let Ok(current) = identity.context().await {
            identity.invalidate(current.generation).await;
        }
        tokio::time::advance(Duration::from_millis(100)).await;
    }
    let elapsed_secs = started.elapsed().as_secs();
    let exchanges = u64::try_from(authn.calls()).expect("count fits");
    assert!(
        exchanges <= elapsed_secs + 1,
        "{exchanges} exchanges in {elapsed_secs}s with a 1s min interval"
    );
    assert!(
        exchanges * 2 >= elapsed_secs,
        "still exchanges regularly: {exchanges} in {elapsed_secs}s"
    );
}

#[tokio::test(start_paused = true)]
async fn concurrent_callers_share_one_failed_exchange() {
    let authn = FakeAuthN::new(usize::MAX, Duration::from_millis(50));
    let identity = Arc::new(identity(&authn, Duration::from_secs(300)));

    let callers: Vec<_> = (0..8)
        .map(|_| {
            let identity = identity.clone();
            tokio::spawn(async move { identity.context().await })
        })
        .collect();
    for caller in callers {
        caller.await.expect("joined").expect_err("idp down");
    }
    assert_eq!(authn.calls(), 1, "waiters read the negative cache");
}

#[tokio::test(start_paused = true)]
async fn a_hung_exchange_times_out_and_a_later_call_retries() {
    let authn = FakeAuthN::new(0, Duration::from_secs(3_600));
    let identity = identity(&authn, Duration::from_secs(300));

    let started = tokio::time::Instant::now();
    let err = identity.context().await.expect_err("hung idp");
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");
    assert!(!format!("{err:#}").contains(SECRET));
    assert_eq!(started.elapsed(), Duration::from_secs(10));

    tokio::time::advance(Duration::from_millis(1_001)).await;
    identity.context().await.expect_err("still hung, retried");
    assert_eq!(authn.calls(), 2);
}

#[test]
fn the_secret_is_redacted_in_debug_output() {
    let debug = format!("{:?}", credentials(Duration::from_secs(300)));
    assert!(!debug.contains(SECRET), "{debug}");
    assert!(debug.contains("llm-gateway"));
}
