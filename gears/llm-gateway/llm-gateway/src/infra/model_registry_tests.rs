//! `ModelRegistryResolver`: Model Registry error mapping (DESIGN §4.1, §4.4).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use llm_gateway_sdk::LlmGatewayError;
use model_registry_sdk::{
    CreateModelRequestV1, CreateProviderRequestV1, ModelRegistryClientV1, ModelRegistryError,
    ModelV1, ProviderV1, UpdateModelRequestV1, UpdateProviderRequestV1,
};
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::ModelRegistryResolver;
use crate::domain::ports::ModelResolver;
use crate::domain::test_support::{OPENAI_TYPE, ctx, sample_model};

/// Model Registry fake: only `get_tenant_model` is exercised.
struct FakeRegistry {
    outcome: Box<dyn Fn() -> Result<ModelV1, ModelRegistryError> + Send + Sync>,
    requested: Mutex<Vec<String>>,
    /// Answers only after this long (virtual time under a paused clock).
    delay: Duration,
}

impl FakeRegistry {
    fn new(
        outcome: impl Fn() -> Result<ModelV1, ModelRegistryError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::slow(Duration::ZERO, outcome)
    }

    fn slow(
        delay: Duration,
        outcome: impl Fn() -> Result<ModelV1, ModelRegistryError> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            outcome: Box::new(outcome),
            requested: Mutex::new(Vec::new()),
            delay,
        })
    }
}

#[async_trait]
impl ModelRegistryClientV1 for FakeRegistry {
    async fn get_tenant_model(
        &self,
        _ctx: &SecurityContext,
        canonical_id: &str,
    ) -> Result<ModelV1, ModelRegistryError> {
        self.requested
            .lock()
            .expect("requested")
            .push(canonical_id.to_owned());
        tokio::time::sleep(self.delay).await;
        (self.outcome)()
    }

    async fn list_tenant_models(
        &self,
        _ctx: &SecurityContext,
        _query: ODataQuery,
    ) -> Result<Page<ModelV1>, ModelRegistryError> {
        unimplemented!("not used by the resolver")
    }

    async fn create_model(
        &self,
        _ctx: &SecurityContext,
        _req: CreateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError> {
        unimplemented!("not used by the resolver")
    }

    async fn update_model(
        &self,
        _ctx: &SecurityContext,
        _canonical_id: &str,
        _req: UpdateModelRequestV1,
    ) -> Result<ModelV1, ModelRegistryError> {
        unimplemented!("not used by the resolver")
    }

    async fn delete_model(
        &self,
        _ctx: &SecurityContext,
        _canonical_id: &str,
    ) -> Result<(), ModelRegistryError> {
        unimplemented!("not used by the resolver")
    }

    async fn get_provider(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<ProviderV1, ModelRegistryError> {
        unimplemented!("not used by the resolver")
    }

    async fn list_providers(
        &self,
        _ctx: &SecurityContext,
        _query: ODataQuery,
    ) -> Result<Page<ProviderV1>, ModelRegistryError> {
        unimplemented!("not used by the resolver")
    }

    async fn create_provider(
        &self,
        _ctx: &SecurityContext,
        _req: CreateProviderRequestV1,
    ) -> Result<ProviderV1, ModelRegistryError> {
        unimplemented!("not used by the resolver")
    }

    async fn update_provider(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
        _req: UpdateProviderRequestV1,
    ) -> Result<ProviderV1, ModelRegistryError> {
        unimplemented!("not used by the resolver")
    }

    async fn delete_provider(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<(), ModelRegistryError> {
        unimplemented!("not used by the resolver")
    }
}

fn approved_model() -> ModelV1 {
    sample_model(OPENAI_TYPE)
}

#[tokio::test]
async fn resolves_the_model_info_of_the_canonical_id() {
    let registry = FakeRegistry::new(|| Ok(approved_model()));
    let resolver = ModelRegistryResolver::new(registry.clone());

    let info = resolver
        .resolve(&ctx(), "openai::gpt-4o")
        .await
        .expect("model resolves");

    assert_eq!(info.gts_type.as_ref(), OPENAI_TYPE);
    assert_eq!(
        *registry.requested.lock().expect("requested"),
        vec!["openai::gpt-4o".to_owned()]
    );
}

#[tokio::test]
async fn unknown_unapproved_and_deprecated_models_are_all_model_not_found() {
    let outcomes: Vec<fn() -> ModelRegistryError> = vec![
        || ModelRegistryError::model_not_found("openai::gpt-4o"),
        || ModelRegistryError::model_not_approved("openai::gpt-4o"),
        || ModelRegistryError::model_deprecated("openai::gpt-4o"),
    ];
    for make in outcomes {
        let resolver = ModelRegistryResolver::new(FakeRegistry::new(move || Err(make())));
        let err = resolver
            .resolve(&ctx(), "openai::gpt-4o")
            .await
            .expect_err("model is not available");
        assert!(
            matches!(err, LlmGatewayError::ModelNotFound { .. }),
            "{err}"
        );
    }
}

#[tokio::test]
async fn model_registry_failure_is_model_resolution_unavailable() {
    let resolver = ModelRegistryResolver::new(FakeRegistry::new(|| {
        Err(ModelRegistryError::Internal {
            detail: "db down".to_owned(),
            source: None,
        })
    }));
    let err = resolver
        .resolve(&ctx(), "openai::gpt-4o")
        .await
        .expect_err("registry failed");
    assert!(
        matches!(err, LlmGatewayError::ModelResolutionUnavailable),
        "{err}"
    );
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_registry_failure_is_logged_with_its_source_chain() {
    let resolver = ModelRegistryResolver::new(FakeRegistry::new(|| {
        Err(ModelRegistryError::internal_from(
            "lookup failed",
            std::io::Error::other("connection refused by db"),
        ))
    }));
    resolver
        .resolve(&ctx(), "openai::gpt-4o")
        .await
        .expect_err("registry failed");
    assert!(logs_contain("lookup failed: connection refused by db"));
}

#[tokio::test]
async fn provider_and_access_problems_are_model_not_found() {
    let outcomes: Vec<fn() -> ModelRegistryError> = vec![
        || ModelRegistryError::provider_disabled(Uuid::nil()),
        || ModelRegistryError::provider_not_found(Uuid::nil()),
        || ModelRegistryError::forbidden("no access"),
        || ModelRegistryError::validation("bad id"),
    ];
    for make in outcomes {
        let resolver = ModelRegistryResolver::new(FakeRegistry::new(move || Err(make())));
        let err = resolver
            .resolve(&ctx(), "openai::gpt-4o")
            .await
            .expect_err("model is not available");
        assert!(
            matches!(err, LlmGatewayError::ModelNotFound { .. }),
            "{err}"
        );
    }
}

#[tokio::test]
async fn unauthenticated_registry_call_fails_closed_as_permission_denied() {
    let resolver = ModelRegistryResolver::new(FakeRegistry::new(|| {
        Err(ModelRegistryError::unauthenticated("no subject"))
    }));
    let err = resolver
        .resolve(&ctx(), "openai::gpt-4o")
        .await
        .expect_err("unauthenticated");
    assert!(matches!(err, LlmGatewayError::PermissionDenied), "{err}");
}

#[tokio::test]
async fn unrelated_registry_variants_are_model_resolution_unavailable() {
    let outcomes: Vec<fn() -> ModelRegistryError> =
        vec![|| ModelRegistryError::invalid_transition("x"), || {
            ModelRegistryError::provider_conflict("slug")
        }];
    for make in outcomes {
        let resolver = ModelRegistryResolver::new(FakeRegistry::new(move || Err(make())));
        let err = resolver
            .resolve(&ctx(), "openai::gpt-4o")
            .await
            .expect_err("registry failed");
        assert!(
            matches!(err, LlmGatewayError::ModelResolutionUnavailable),
            "{err}"
        );
    }
}

// ── timeouts.dependency.model_registry ──────────────────────────────────────

mod timeout {
    use super::*;

    #[tokio::test(start_paused = true)]
    #[tracing_test::traced_test]
    async fn a_lookup_over_the_bound_is_model_resolution_unavailable_at_the_bound() {
        let registry = FakeRegistry::slow(Duration::from_mins(1), || Ok(approved_model()));
        let resolver = ModelRegistryResolver::new(registry).with_timeout(Duration::from_secs(3));
        let started = tokio::time::Instant::now();

        let err = resolver
            .resolve(&ctx(), "openai::gpt-4o")
            .await
            .expect_err("timed out");

        assert!(
            matches!(err, LlmGatewayError::ModelResolutionUnavailable),
            "{err:?}"
        );
        assert_eq!(started.elapsed(), Duration::from_secs(3));
        assert!(logs_contain("openai::gpt-4o"), "logged with the model");
    }

    #[tokio::test(start_paused = true)]
    async fn the_default_bound_is_five_seconds() {
        let registry = FakeRegistry::slow(Duration::from_mins(1), || Ok(approved_model()));
        let resolver = ModelRegistryResolver::new(registry);
        let started = tokio::time::Instant::now();

        let err = resolver
            .resolve(&ctx(), "openai::gpt-4o")
            .await
            .expect_err("timed out");

        assert!(matches!(err, LlmGatewayError::ModelResolutionUnavailable));
        assert_eq!(started.elapsed(), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn a_lookup_answering_just_before_the_bound_succeeds() {
        let registry = FakeRegistry::slow(Duration::from_secs(2), || Ok(approved_model()));
        let resolver = ModelRegistryResolver::new(registry).with_timeout(Duration::from_secs(3));

        resolver
            .resolve(&ctx(), "openai::gpt-4o")
            .await
            .expect("answered in time");
    }
}
