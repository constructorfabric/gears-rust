use mini_chat_sdk::{
    EstimationBudgets, MiniChatModelPolicyPluginClientV1, MiniChatModelPolicyPluginError,
    ModelCatalogEntry, ModelGeneralConfig, ModelPreference, ModelTier,
    models::{
        ModelApiParams, ModelFeatures, ModelInputType, ModelPerformance, ModelSupportedEndpoints,
        ModelTokenPolicy, ModelToolSupport,
    },
};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::config::StaticMiniChatPolicyPluginConfig;
use super::service::Service;

fn make_entry(model_id: &str, tier: ModelTier) -> ModelCatalogEntry {
    ModelCatalogEntry {
        model_id: model_id.to_owned(),
        provider_model_id: format!("{model_id}-v1"),
        display_name: model_id.to_owned(),
        description: String::new(),
        version: String::new(),
        provider_id: "default".to_owned(),
        provider_display_name: "Default".to_owned(),
        icon: String::new(),
        tier,
        enabled: true,
        multimodal_capabilities: vec![],
        context_window: 128_000,
        max_output_tokens: 16_384,
        max_input_tokens: 128_000,
        input_tokens_credit_multiplier_micro: 1_000_000,
        output_tokens_credit_multiplier_micro: 3_000_000,
        multiplier_display: "1x".to_owned(),
        estimation_budgets: EstimationBudgets::default(),
        max_retrieved_chunks_per_turn: 5,
        max_tool_calls: 2,
        general_config: ModelGeneralConfig {
            config_type: String::new(),
            available_from: OffsetDateTime::UNIX_EPOCH,
            max_file_size_mb: 25,
            api_params: ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
            },
            features: ModelFeatures {
                streaming: true,
                function_calling: true,
                structured_output: true,
                fine_tuning: false,
                distillation: false,
                fim_completion: false,
                chat_prefix_completion: false,
            },
            input_type: ModelInputType {
                text: true,
                image: false,
                audio: false,
                video: false,
            },
            tool_support: ModelToolSupport {
                web_search: false,
                file_search: false,
                image_generation: false,
                code_interpreter: false,
                computer_use: false,
                mcp: false,
            },
            supported_endpoints: ModelSupportedEndpoints {
                chat_completions: true,
                responses: false,
                realtime: false,
                assistants: false,
                batch_api: false,
                fine_tuning: false,
                embeddings: false,
                videos: false,
                image_generation: false,
                image_edit: false,
                audio_speech_generation: false,
                audio_transcription: false,
                audio_translation: false,
                moderations: false,
                completions: false,
            },
            token_policy: ModelTokenPolicy {
                input_tokens_credit_multiplier: 1.0,
                output_tokens_credit_multiplier: 3.0,
            },
            performance: ModelPerformance {
                response_latency_ms: 500,
                speed_tokens_per_second: 100,
            },
        },
        preference: Some(ModelPreference {
            is_default: false,
            sort_order: 0,
        }),
        system_prompt: String::new(),
        thread_summary_prompt: String::new(),
    }
}

fn test_service() -> Service {
    let cfg = StaticMiniChatPolicyPluginConfig::default();
    Service::new(
        vec![
            make_entry("standard-model", ModelTier::Standard),
            make_entry("premium-model", ModelTier::Premium),
        ],
        cfg.kill_switches,
        cfg.default_standard_limits,
        cfg.default_premium_limits,
    )
}

fn token() -> CancellationToken {
    CancellationToken::new()
}

// ── get_current_policy_version ──

#[tokio::test]
async fn policy_version_echoes_user_id() {
    let svc = test_service();
    let user_id = Uuid::new_v4();
    let info = svc
        .get_current_policy_version(user_id, token())
        .await
        .unwrap();

    assert_eq!(info.user_id, user_id);
    assert_eq!(info.policy_version, 1);
}

#[tokio::test]
async fn policy_version_timestamp_is_recent() {
    let before = OffsetDateTime::now_utc();
    let svc = test_service();
    let info = svc
        .get_current_policy_version(Uuid::new_v4(), token())
        .await
        .unwrap();
    let after = OffsetDateTime::now_utc();

    assert!(info.generated_at >= before);
    assert!(info.generated_at <= after);
}

// ── get_policy_snapshot: version gating ──

#[tokio::test]
async fn snapshot_version_1_returns_catalog() {
    let svc = test_service();
    let user_id = Uuid::new_v4();
    let snap = svc.get_policy_snapshot(user_id, 1, token()).await.unwrap();

    assert_eq!(snap.user_id, user_id);
    assert_eq!(snap.policy_version, 1);
    assert_eq!(snap.model_catalog.len(), 2);
}

#[tokio::test]
async fn snapshot_wrong_version_returns_not_found() {
    let svc = test_service();
    for version in [0, 2, 100, u64::MAX] {
        let result = svc
            .get_policy_snapshot(Uuid::new_v4(), version, token())
            .await;
        assert!(
            matches!(result, Err(MiniChatModelPolicyPluginError::NotFound)),
            "version {version} should return NotFound"
        );
    }
}

#[tokio::test]
async fn snapshot_preserves_kill_switch_state() {
    let mut cfg = StaticMiniChatPolicyPluginConfig::default();
    cfg.kill_switches.disable_premium_tier = true;
    cfg.kill_switches.disable_web_search = true;

    let svc = Service::new(
        cfg.model_catalog,
        cfg.kill_switches,
        cfg.default_standard_limits,
        cfg.default_premium_limits,
    );
    let snap = svc
        .get_policy_snapshot(Uuid::new_v4(), 1, token())
        .await
        .unwrap();

    assert!(snap.kill_switches.disable_premium_tier);
    assert!(snap.kill_switches.disable_web_search);
    assert!(!snap.kill_switches.force_standard_tier);
}

#[tokio::test]
async fn snapshot_contains_both_tiers() {
    let svc = test_service();
    let snap = svc
        .get_policy_snapshot(Uuid::new_v4(), 1, token())
        .await
        .unwrap();

    let has_premium = snap
        .model_catalog
        .iter()
        .any(|m| m.tier == ModelTier::Premium);
    let has_standard = snap
        .model_catalog
        .iter()
        .any(|m| m.tier == ModelTier::Standard);

    assert!(has_premium, "catalog must include a premium model");
    assert!(has_standard, "catalog must include a standard model");
}

// ── check_user_license ──

#[tokio::test]
async fn check_user_license_returns_active() {
    let svc = test_service();
    let status = svc
        .check_user_license(Uuid::new_v4(), token())
        .await
        .unwrap();

    assert!(status.active, "static plugin should always return active");
}

// ── get_user_limits: version gating ──

#[tokio::test]
async fn user_limits_version_1_returns_configured_limits() {
    let svc = test_service();
    let user_id = Uuid::new_v4();
    let limits = svc.get_user_limits(user_id, 1, token()).await.unwrap();

    assert_eq!(limits.user_id, user_id);
    assert_eq!(limits.policy_version, 1);
    assert_eq!(limits.standard.limit_daily_credits_micro, 100_000_000);
    assert_eq!(limits.standard.limit_monthly_credits_micro, 1_000_000_000);
    assert_eq!(limits.premium.limit_daily_credits_micro, 50_000_000);
    assert_eq!(limits.premium.limit_monthly_credits_micro, 500_000_000);
}

#[tokio::test]
async fn user_limits_wrong_version_returns_not_found() {
    let svc = test_service();
    for version in [0, 2, 100, u64::MAX] {
        let result = svc.get_user_limits(Uuid::new_v4(), version, token()).await;
        assert!(
            matches!(result, Err(MiniChatModelPolicyPluginError::NotFound)),
            "version {version} should return NotFound"
        );
    }
}

#[tokio::test]
async fn user_limits_reflect_custom_config() {
    let mut cfg = StaticMiniChatPolicyPluginConfig::default();
    cfg.default_standard_limits.limit_daily_credits_micro = 42;
    cfg.default_premium_limits.limit_monthly_credits_micro = 99;

    let svc = Service::new(
        cfg.model_catalog,
        cfg.kill_switches,
        cfg.default_standard_limits,
        cfg.default_premium_limits,
    );
    let limits = svc
        .get_user_limits(Uuid::new_v4(), 1, token())
        .await
        .unwrap();

    assert_eq!(limits.standard.limit_daily_credits_micro, 42);
    assert_eq!(limits.premium.limit_monthly_credits_micro, 99);
}
