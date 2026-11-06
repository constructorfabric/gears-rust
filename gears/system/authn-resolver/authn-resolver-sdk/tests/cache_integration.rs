//! Public cache contracts across the SDK providers and a caching resolver.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverBearerAuthenticator, AuthNResolverClient, AuthNResolverError,
    AuthenticationRequest, AuthenticationResult, CacheControl, CachingAuthenticator,
    ClientCredentialsProvider, ClientCredentialsProviderConfig,
    ClientCredentialsProviderConfigError, ClientCredentialsRequest, RequestCacheControl, ScopeSet,
};
use parking_lot::Mutex;
use secrecy::{ExposeSecret as _, SecretString};
use time::OffsetDateTime;
use toolkit_security::{BearerAuthenticator, SecurityContext};
use uuid::Uuid;

/// A caching resolver that always reacquires on refresh.
#[derive(Default)]
struct CachingBackend(Mutex<BackendState>);

#[derive(Default)]
struct BackendState {
    serial: u128,
    bearer: HashMap<String, AuthenticationResult>,
    credentials: HashMap<Option<ScopeSet>, AuthenticationResult>,
    minted: HashMap<Option<ScopeSet>, usize>,
    requests: Vec<RequestCacheControl>,
}

impl BackendState {
    fn mint(&mut self) -> Result<AuthenticationResult, AuthNResolverError> {
        self.serial += 1;
        let context = SecurityContext::builder()
            .subject_id(Uuid::from_u128(self.serial))
            .subject_tenant_id(Uuid::from_u128(99))
            .build()
            .map_err(|error| AuthNResolverError::Internal(error.to_string()))?;
        Ok(
            AuthenticationResult::new(context).with_cache_control(CacheControl::until(
                OffsetDateTime::now_utc() + Duration::from_secs(3600),
            )),
        )
    }
}

#[async_trait]
impl AuthNResolverClient for CachingBackend {
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        let mut state = self.0.lock();
        state.requests.push(request.cache_control.clone());
        let token = request.bearer_token.expose_secret();
        if request.cache_control == RequestCacheControl::AllowCached
            && let Some(result) = state.bearer.get(token)
        {
            return Ok(result.clone());
        }
        let result = state.mint()?;
        state.bearer.insert(token.to_owned(), result.clone());
        Ok(result)
    }

    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        let mut state = self.0.lock();
        if request.cache_control == RequestCacheControl::AllowCached
            && let Some(result) = state.credentials.get(&request.scopes)
        {
            return Ok(result.clone());
        }
        let result = state.mint()?;
        *state.minted.entry(request.scopes.clone()).or_default() += 1;
        state
            .credentials
            .insert(request.scopes.clone(), result.clone());
        Ok(result)
    }
}

fn credentials_provider(
    backend: Arc<CachingBackend>,
    capacity: usize,
) -> Result<ClientCredentialsProvider, ClientCredentialsProviderConfigError> {
    ClientCredentialsProvider::builder(backend, "service", SecretString::from("secret"))
        .with_config(ClientCredentialsProviderConfig::default().with_max_entries(capacity))
        .build()
}

fn identity(result: &AuthenticationResult) -> Uuid {
    result.security_context.subject_id()
}

#[tokio::test]
async fn results_cannot_invalidate_a_different_kind_of_cache() {
    let backend = Arc::new(CachingBackend::default());
    let auth = CachingAuthenticator::builder(backend.clone())
        .build()
        .unwrap();
    let identity = credentials_provider(backend, 1).unwrap();
    let inbound = auth.authenticate("token").await.unwrap();
    let outbound = identity.get().await.unwrap();
    assert!(!auth.invalidate(&outbound));
    assert!(!identity.invalidate(&inbound));
    assert!(auth.invalidate(&inbound));
    assert!(identity.invalidate(&outbound));
}

#[tokio::test]
async fn direct_bearer_adapter_forces_validation_but_requests_can_allow_cached() {
    let backend = Arc::new(CachingBackend::default());
    let adapter = AuthNResolverBearerAuthenticator::new(backend.clone());
    let first = adapter.authenticate("token").await.unwrap();
    let request = AuthenticationRequest::new(SecretString::from("token"))
        .with_cache_control(RequestCacheControl::AllowCached);
    let reused = backend.authenticate(&request).await.unwrap();
    assert_eq!(first.subject_id(), reused.security_context.subject_id());
    let refreshed = adapter.authenticate("token").await.unwrap();
    assert_ne!(first.subject_id(), refreshed.subject_id());
    assert_eq!(
        backend.0.lock().requests,
        [
            RequestCacheControl::refresh(),
            RequestCacheControl::AllowCached,
            RequestCacheControl::refresh()
        ]
    );
}

#[tokio::test]
async fn rejected_token_cannot_return_from_plugin_cache_after_eviction_and_recreation() {
    for conditional in [false, true] {
        let backend = Arc::new(CachingBackend::default());
        let provider = credentials_provider(backend.clone(), 2).unwrap();
        let a = ScopeSet::new(["a"]).unwrap();
        let b = ScopeSet::new(["b"]).unwrap();
        let rejected = provider.get_with_scopes(&a).await.unwrap();
        if conditional {
            assert!(provider.invalidate(&rejected));
        } else {
            provider.clear();
        }
        provider.get_with_scopes(&b).await.unwrap();
        let fresh = provider.get_with_scopes(&a).await.unwrap();
        assert_ne!(identity(&rejected), identity(&fresh));
        assert!(!provider.invalidate(&rejected));
        assert_eq!(
            identity(&provider.get_with_scopes(&a).await.unwrap()),
            identity(&fresh)
        );
        assert_eq!(backend.0.lock().minted.get(&Some(a)), Some(&2));
    }
}
