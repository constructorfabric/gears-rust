use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, CacheControl,
    ClientCredentialsProvider, ClientCredentialsProviderConfig, ClientCredentialsProviderError,
    ClientCredentialsRequest,
};
use parking_lot::Mutex;
use secrecy::SecretString;
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use tracing::Instrument as _;
use uuid::Uuid;

#[derive(Default)]
struct SpanRecordingResolver {
    observed: Mutex<Vec<String>>,
}

#[async_trait]
impl AuthNResolverClient for SpanRecordingResolver {
    async fn authenticate(
        &self,
        _request: &authn_resolver_sdk::AuthenticationRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::Internal("unused bearer path".into()))
    }

    async fn exchange_client_credentials(
        &self,
        _request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        let span_name = tracing::Span::current().metadata().map_or_else(
            || "<none>".to_owned(),
            |metadata| metadata.name().to_owned(),
        );
        let attempt = {
            let mut observed = self.observed.lock();
            observed.push(span_name);
            observed.len()
        };
        tracing::info!(attempt, "PLID resolver acquisition");
        if attempt == 1 {
            return Err(AuthNResolverError::ServiceUnavailable("offline".into()));
        }
        let context = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::from_u128(2))
            .build()
            .map_err(|error| AuthNResolverError::Internal(error.to_string()))?;
        Ok(
            AuthenticationResult::new(context).with_cache_control(CacheControl::until(
                OffsetDateTime::now_utc() + Duration::from_secs(300),
            )),
        )
    }
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn owned_acquisitions_and_their_logs_keep_the_initiating_caller_span() {
    let resolver = Arc::new(SpanRecordingResolver::default());
    let provider = ClientCredentialsProvider::builder(
        resolver.clone(),
        "service",
        SecretString::from("example-only-secret"),
    )
    .with_config(
        ClientCredentialsProviderConfig::default()
            .with_backoff(Duration::from_secs(1), Duration::from_secs(1)),
    )
    .build()
    .unwrap();

    // Instrument the caller futures, never hold an entered span across await.
    let first = provider
        .get()
        .instrument(tracing::info_span!("caller_alpha", request_id = "alpha"))
        .await;
    assert!(matches!(
        first,
        Err(ClientCredentialsProviderError::Resolver(
            AuthNResolverError::ServiceUnavailable(_)
        ))
    ));
    tokio::time::advance(Duration::from_secs(1)).await;
    provider
        .get()
        .instrument(tracing::info_span!("caller_beta", request_id = "beta"))
        .await
        .unwrap();

    assert_eq!(
        resolver.observed.lock().as_slice(),
        ["caller_alpha", "caller_beta"]
    );
    logs_assert(|lines| {
        for (message, caller) in [
            ("S2S credential acquisition failed", "caller_alpha"),
            ("S2S credential acquisition recovered", "caller_beta"),
        ] {
            let matching: Vec<_> = lines.iter().filter(|line| line.contains(message)).collect();
            if matching.len() != 1 || !matching[0].contains(caller) {
                return Err(format!(
                    "expected {message:?} in {caller:?}, got {matching:?}"
                ));
            }
        }
        Ok(())
    });
}
