//! Run with:
//! `cargo run -p cf-gears-authn-resolver-sdk --example cached_client_credentials`.
//!
//! The resolver here is an in-memory stand-in. In a gear, obtain the real
//! `AuthNResolverClient` from `ClientHub`; the provider calls stay the same.

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, CacheControl,
    ClientCredentialsProvider, ClientCredentialsRequest, ScopeSet,
};
use secrecy::SecretString;
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use uuid::Uuid;

#[derive(Default)]
struct ExampleResolver {
    exchanges: AtomicUsize,
}

#[async_trait]
impl AuthNResolverClient for ExampleResolver {
    async fn authenticate(
        &self,
        _request: &authn_resolver_sdk::AuthenticationRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::Internal(
            "this example only exchanges client credentials".into(),
        ))
    }

    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.exchanges.fetch_add(1, Ordering::Relaxed);
        let scopes = request.scopes.as_ref().map_or_else(
            || vec!["idp:default".to_owned()],
            |scopes| scopes.iter().map(str::to_owned).collect(),
        );
        let deadline = OffsetDateTime::now_utc() + Duration::from_secs(300);
        let context = SecurityContext::builder()
            .subject_id(Uuid::from_u128(1))
            .subject_tenant_id(Uuid::from_u128(2))
            .token_scopes(scopes)
            .expires_at(deadline)
            .build()
            .map_err(|error| AuthNResolverError::Internal(error.to_string()))?;
        Ok(AuthenticationResult::new(context).with_cache_control(CacheControl::until(deadline)))
    }
}

/// The exact example workflow is also executed by the integration test.
///
/// # Errors
///
/// Propagates scope validation, provider configuration and acquisition errors.
///
/// # Panics
///
/// If the demonstrated cache reuse, scope isolation or invalidation checks fail.
pub async fn run() -> Result<(), Box<dyn Error>> {
    let resolver = Arc::new(ExampleResolver::default());
    let provider = ClientCredentialsProvider::builder(
        resolver.clone(),
        "example-service",
        SecretString::from("example-only-secret"),
    )
    .with_default_scopes(ScopeSet::new(["orders:write"])?)
    .build()?;

    let first = provider.get().await?;
    let _reused = provider.get().await?;
    assert_eq!(resolver.exchanges.load(Ordering::Relaxed), 1);

    let read = provider
        .get_with_scopes(&ScopeSet::new(["orders:read"])?)
        .await?;
    assert_eq!(read.security_context.token_scopes(), &["orders:read"]);
    assert_eq!(resolver.exchanges.load(Ordering::Relaxed), 2);

    // Model a downstream rejection of the first result.
    assert!(provider.invalidate(&first));
    let _refreshed = provider.get().await?;
    assert_eq!(resolver.exchanges.load(Ordering::Relaxed), 3);
    // A late rejection of the replaced result leaves the new one cached.
    assert!(!provider.invalidate(&first));

    provider.shutdown().await;
    Ok(())
}

#[cfg(not(test))]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    run().await?;
    println!("Reused credentials, requested another scope set, and refreshed a rejected result.");
    Ok(())
}
