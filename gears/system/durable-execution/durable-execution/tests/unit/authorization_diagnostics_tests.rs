//! Missing local credentials must explain why background authorization paused.
use super::*;
use authn_resolver_sdk::{AuthNResolverError, AuthenticationResult};
use authz_resolver_sdk::{
    AuthZResolverApi,
    models::{EvaluationRequest, EvaluationResponse},
};
use std::collections::BTreeMap;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use tracing::instrument::WithSubscriber;

type Fields = BTreeMap<String, String>;
#[derive(Clone, Default)]
struct Capture(Arc<parking_lot::Mutex<Vec<Fields>>>);
struct Visitor(Fields);
impl tracing::field::Visit for Visitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }
}
impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = Visitor(BTreeMap::new());
        event.record(&mut visitor);
        self.0.lock().push(visitor.0);
    }
}

struct UnexpectedDependencies;
#[async_trait]
impl AuthNResolverClient for UnexpectedDependencies {
    async fn authenticate(&self, _: &str) -> Result<AuthenticationResult, AuthNResolverError> {
        panic!("missing credentials must fail before AuthN");
    }
    async fn exchange_client_credentials(
        &self,
        _: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        panic!("missing credentials must fail before AuthN");
    }
}
#[async_trait]
impl AuthZResolverApi for UnexpectedDependencies {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        _: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        panic!("missing credentials must fail before PDP");
    }
}

#[tokio::test]
async fn missing_secret_logs_safe_configuration_reference_and_action() {
    let secret_env = format!("DURABLE_MISSING_SECRET_{}", uuid::Uuid::new_v4().simple());
    assert!(std::env::var_os(&secret_env).is_none());
    let config = Config {
        service_client_id: "private-client-identity".to_owned(),
        service_client_secret_env: secret_env.clone(),
        ..Config::default()
    };
    let authorization = ServiceAuthorization {
        authn: Arc::new(UnexpectedDependencies),
        enforcer: PolicyEnforcer::new(Arc::new(UnexpectedDependencies)),
        config,
    };
    let capture = Capture::default();
    let result = authorization
        .scope("definition:unregister")
        .with_subscriber(capture.clone())
        .await;
    assert!(matches!(result, Err(StoreError::AuthorizationUnavailable)));
    let events = capture.0.lock();
    assert_eq!(events.len(), 1);
    let fields = &events[0];
    assert_eq!(
        fields.get("action").map(String::as_str),
        Some("definition:unregister")
    );
    assert_eq!(fields.get("secret_env"), Some(&secret_env));
    assert_eq!(
        fields.get("error_category").map(String::as_str),
        Some("secret_environment_missing")
    );
    assert!(!format!("{fields:?}").contains("private-client-identity"));
    assert!(!fields.contains_key("client_secret"));
}
