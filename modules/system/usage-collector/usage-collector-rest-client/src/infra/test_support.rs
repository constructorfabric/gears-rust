#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, ClientCredentialsRequest,
};
use chrono::Utc;
use modkit_security::SecurityContext;
use serde_json::json;
use usage_collector_sdk::models::{Subject, UsageKind, UsageRecord};
use uuid::Uuid;

use crate::config::UsageCollectorRestClientConfig;

pub enum AuthNOutcome {
    WithToken(String),
    WithoutToken,
    Unauthorized,
    NoPlugin,
    TokenAcquisitionFailed,
    ServiceUnavailable,
}

pub struct MockAuthN {
    outcome: AuthNOutcome,
}

impl MockAuthN {
    pub fn with_token(token: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            outcome: AuthNOutcome::WithToken(token.into()),
        })
    }

    pub fn without_token() -> Arc<Self> {
        Arc::new(Self {
            outcome: AuthNOutcome::WithoutToken,
        })
    }

    pub fn unauthorized() -> Arc<Self> {
        Arc::new(Self {
            outcome: AuthNOutcome::Unauthorized,
        })
    }

    pub fn no_plugin() -> Arc<Self> {
        Arc::new(Self {
            outcome: AuthNOutcome::NoPlugin,
        })
    }

    pub fn token_acquisition_failed() -> Arc<Self> {
        Arc::new(Self {
            outcome: AuthNOutcome::TokenAcquisitionFailed,
        })
    }

    pub fn service_unavailable() -> Arc<Self> {
        Arc::new(Self {
            outcome: AuthNOutcome::ServiceUnavailable,
        })
    }
}

#[async_trait]
impl AuthNResolverClient for MockAuthN {
    async fn authenticate(
        &self,
        _bearer_token: &str,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        panic!(
            "MockAuthN::authenticate not supported in these tests; use exchange_client_credentials"
        )
    }

    async fn exchange_client_credentials(
        &self,
        _request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        let nil = Uuid::nil();
        match &self.outcome {
            AuthNOutcome::WithToken(token) => {
                let ctx = SecurityContext::builder()
                    .subject_id(nil)
                    .subject_tenant_id(nil)
                    .bearer_token(token.clone())
                    .build()
                    .unwrap();
                Ok(AuthenticationResult {
                    security_context: ctx,
                })
            }
            AuthNOutcome::WithoutToken => {
                let ctx = SecurityContext::builder()
                    .subject_id(nil)
                    .subject_tenant_id(nil)
                    .build()
                    .unwrap();
                Ok(AuthenticationResult {
                    security_context: ctx,
                })
            }
            AuthNOutcome::Unauthorized => Err(AuthNResolverError::Unauthorized(
                "bad credentials".to_owned(),
            )),
            AuthNOutcome::NoPlugin => Err(AuthNResolverError::NoPluginAvailable),
            AuthNOutcome::TokenAcquisitionFailed => Err(
                AuthNResolverError::TokenAcquisitionFailed("invalid client credentials".to_owned()),
            ),
            AuthNOutcome::ServiceUnavailable => Err(AuthNResolverError::ServiceUnavailable(
                "identity service temporarily unreachable".to_owned(),
            )),
        }
    }
}

pub fn test_cfg(collector_url: &str) -> UsageCollectorRestClientConfig {
    serde_json::from_value(json!({
        "collector_url": collector_url,
        "oauth": {
            "client_id": "test-client",
            "client_secret": "test-secret"
        }
    }))
    .unwrap()
}

pub fn test_record() -> UsageRecord {
    UsageRecord {
        module: "test-module".to_owned(),
        tenant_id: Uuid::nil(),
        resource_type: "vm".to_owned(),
        resource_id: Uuid::nil(),
        subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
        metric: "cpu.usage".to_owned(),
        kind: UsageKind::Gauge,
        idempotency_key: "idem-1".to_owned(),
        value: 1.5,
        timestamp: Utc::now(),
        metadata: None,
    }
}
