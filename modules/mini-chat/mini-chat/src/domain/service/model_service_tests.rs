use std::sync::Arc;

use mini_chat_sdk::{ModelCatalogEntry, ModelTier};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::service::model_service::ModelService;
use crate::domain::service::test_helpers::{
    MockModelResolver, TestCatalogEntryParams, inmem_db, mock_db_provider, mock_denying_enforcer,
    mock_enforcer, test_catalog_entry, test_security_ctx,
};

// ── Test Helpers ──

fn mock_catalog() -> Vec<ModelCatalogEntry> {
    vec![
        test_catalog_entry(TestCatalogEntryParams {
            model_id: "gpt-5.2".to_owned(),
            provider_model_id: "gpt-5.2-2025-03-26".to_owned(),
            display_name: "GPT-5.2".to_owned(),
            tier: ModelTier::Premium,
            enabled: true,
            is_default: true,
            input_tokens_credit_multiplier_micro: 2_000_000,
            output_tokens_credit_multiplier_micro: 6_000_000,
            multimodal_capabilities: vec!["VISION_INPUT".to_owned(), "RAG".to_owned()],
            context_window: 128_000,
            max_output_tokens: 16_384,
            description: "Best for complex reasoning".to_owned(),
            provider_display_name: "OpenAI".to_owned(),
            multiplier_display: "2x".to_owned(),
            provider_id: "openai".to_owned(),
        }),
        test_catalog_entry(TestCatalogEntryParams {
            model_id: "gpt-5-mini".to_owned(),
            provider_model_id: "gpt-5-mini-2025-03-26".to_owned(),
            display_name: "GPT-5 Mini".to_owned(),
            tier: ModelTier::Standard,
            enabled: true,
            is_default: false,
            input_tokens_credit_multiplier_micro: 1_000_000,
            output_tokens_credit_multiplier_micro: 3_000_000,
            multimodal_capabilities: vec!["VISION_INPUT".to_owned()],
            context_window: 64_000,
            max_output_tokens: 8_192,
            description: String::new(),
            provider_display_name: "OpenAI".to_owned(),
            multiplier_display: "1x".to_owned(),
            provider_id: "openai".to_owned(),
        }),
        test_catalog_entry(TestCatalogEntryParams {
            model_id: "disabled-model".to_owned(),
            provider_model_id: "disabled-model-2025-03-26".to_owned(),
            display_name: "Disabled Model".to_owned(),
            tier: ModelTier::Standard,
            enabled: false,
            is_default: false,
            input_tokens_credit_multiplier_micro: 1_000_000,
            output_tokens_credit_multiplier_micro: 3_000_000,
            multimodal_capabilities: vec![],
            context_window: 32_000,
            max_output_tokens: 4_096,
            description: "Should not be visible".to_owned(),
            provider_display_name: "OpenAI".to_owned(),
            multiplier_display: "1x".to_owned(),
            provider_id: "openai".to_owned(),
        }),
    ]
}

async fn build_service(catalog: Vec<ModelCatalogEntry>) -> ModelService {
    build_service_with_enforcer(catalog, mock_enforcer()).await
}

async fn build_service_with_enforcer(
    catalog: Vec<ModelCatalogEntry>,
    enforcer: authz_resolver_sdk::PolicyEnforcer,
) -> ModelService {
    let db = mock_db_provider(inmem_db().await);
    let resolver = Arc::new(MockModelResolver::new(catalog));

    ModelService::new(db, enforcer, resolver)
}

fn test_ctx() -> modkit_security::SecurityContext {
    test_security_ctx(Uuid::new_v4())
}

// ── Tests ──

#[tokio::test]
async fn list_models_returns_only_enabled() {
    let svc = build_service(mock_catalog()).await;
    let ctx = test_ctx();

    let models = svc.list_models(&ctx).await.expect("list_models failed");

    assert_eq!(models.len(), 2, "should exclude disabled model");
    assert_eq!(models[0].model_id, "gpt-5.2");
    assert_eq!(models[1].model_id, "gpt-5-mini");
}

#[tokio::test]
async fn list_models_maps_fields_correctly() {
    let svc = build_service(mock_catalog()).await;
    let ctx = test_ctx();

    let models = svc.list_models(&ctx).await.expect("list_models failed");
    let premium = &models[0];

    assert_eq!(premium.display_name, "GPT-5.2");
    assert_eq!(premium.tier, "premium");
    assert_eq!(premium.multiplier_display, "2x");
    assert_eq!(
        premium.description.as_deref(),
        Some("Best for complex reasoning")
    );
    assert_eq!(premium.multimodal_capabilities, vec!["VISION_INPUT", "RAG"]);
    assert_eq!(premium.context_window, 128_000);
}

#[tokio::test]
async fn list_models_empty_description_becomes_none() {
    let svc = build_service(mock_catalog()).await;
    let ctx = test_ctx();

    let models = svc.list_models(&ctx).await.expect("list_models failed");
    let standard = &models[1];

    assert!(standard.description.is_none());
}

#[tokio::test]
async fn get_model_returns_enabled_model() {
    let svc = build_service(mock_catalog()).await;
    let ctx = test_ctx();

    let model = svc
        .get_model(&ctx, "gpt-5.2")
        .await
        .expect("get_model failed");

    assert_eq!(model.model_id, "gpt-5.2");
    assert_eq!(model.tier, "premium");
}

#[tokio::test]
async fn get_model_returns_not_found_for_disabled() {
    let svc = build_service(mock_catalog()).await;
    let ctx = test_ctx();

    let err = svc
        .get_model(&ctx, "disabled-model")
        .await
        .expect_err("should fail for disabled model");

    assert!(
        matches!(err, DomainError::ModelNotFound { .. }),
        "expected ModelNotFound, got: {err:?}"
    );
}

#[tokio::test]
async fn get_model_returns_not_found_for_nonexistent() {
    let svc = build_service(mock_catalog()).await;
    let ctx = test_ctx();

    let err = svc
        .get_model(&ctx, "nonexistent")
        .await
        .expect_err("should fail for nonexistent model");

    assert!(
        matches!(err, DomainError::ModelNotFound { .. }),
        "expected ModelNotFound, got: {err:?}"
    );
}

#[tokio::test]
async fn list_models_empty_catalog() {
    let svc = build_service(vec![]).await;
    let ctx = test_ctx();

    let models = svc.list_models(&ctx).await.expect("list_models failed");
    assert!(models.is_empty());
}

// ── Authorization denial tests ──

#[tokio::test]
async fn list_models_denied_returns_forbidden() {
    let svc = build_service_with_enforcer(mock_catalog(), mock_denying_enforcer()).await;
    let ctx = test_ctx();

    let err = svc
        .list_models(&ctx)
        .await
        .expect_err("should fail when enforcer denies");

    assert!(
        matches!(err, DomainError::Forbidden),
        "expected Forbidden, got: {err:?}"
    );
}

#[tokio::test]
async fn get_model_denied_returns_forbidden() {
    let svc = build_service_with_enforcer(mock_catalog(), mock_denying_enforcer()).await;
    let ctx = test_ctx();

    let err = svc
        .get_model(&ctx, "gpt-5.2")
        .await
        .expect_err("should fail when enforcer denies");

    assert!(
        matches!(err, DomainError::Forbidden),
        "expected Forbidden, got: {err:?}"
    );
}
