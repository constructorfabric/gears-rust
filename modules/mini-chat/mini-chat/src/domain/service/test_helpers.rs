use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverClient, AuthZResolverError, PolicyEnforcer,
    constraints::{Constraint, EqPredicate, Predicate},
    models::{DenyReason, EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use modkit_db::{
    ConnectOpts, DBProvider, Db, connect_db, migration_runner::run_migrations_for_testing,
};
use modkit_security::{SecurityContext, pep_properties};
use sea_orm_migration::MigratorTrait;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::models::ResolvedModel;
use crate::domain::repos::{
    ModelResolver, PolicySnapshotProvider, ThreadSummaryRepository, UserLimitsProvider,
};

// ── Mock AuthZ Resolver ──

pub struct MockAuthZResolver;

#[async_trait]
impl AuthZResolverClient for MockAuthZResolver {
    async fn evaluate(
        &self,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        let subject_tenant_id = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());

        let subject_id = request.subject.id;

        // Deny when resource tenant_id differs from subject tenant_id
        if let Some(res_tenant) = request
            .resource
            .properties
            .get(pep_properties::OWNER_TENANT_ID)
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            && subject_tenant_id.is_some_and(|st| st != res_tenant)
        {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    deny_reason: Some(DenyReason {
                        error_code: "tenant_mismatch".to_owned(),
                        details: Some("subject tenant does not match resource tenant".to_owned()),
                    }),
                    ..Default::default()
                },
            });
        }

        // Deny when resource owner_id differs from subject id
        if let Some(res_owner) = request
            .resource
            .properties
            .get(pep_properties::OWNER_ID)
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            && res_owner != subject_id
        {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    deny_reason: Some(DenyReason {
                        error_code: "owner_mismatch".to_owned(),
                        details: Some("subject id does not match resource owner".to_owned()),
                    }),
                    ..Default::default()
                },
            });
        }

        // Build constraints from subject identity
        if request.context.require_constraints {
            let mut predicates = Vec::new();

            if let Some(tid) = subject_tenant_id {
                predicates.push(Predicate::Eq(EqPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    tid,
                )));
            }

            predicates.push(Predicate::Eq(EqPredicate::new(
                pep_properties::OWNER_ID,
                subject_id,
            )));

            let constraints = vec![Constraint { predicates }];

            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints,
                    ..Default::default()
                },
            })
        } else {
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext::default(),
            })
        }
    }
}

// ── Mock Model Resolver ──

use mini_chat_sdk::ModelCatalogEntry;

/// Mock model resolver with a configurable catalog.
///
/// Default catalog: `gpt-5.2` (enabled, default) and `gpt-5-mini` (disabled).
pub struct MockModelResolver {
    catalog: Mutex<Vec<ModelCatalogEntry>>,
}

impl MockModelResolver {
    pub fn new(catalog: Vec<ModelCatalogEntry>) -> Self {
        Self {
            catalog: Mutex::new(catalog),
        }
    }
}

impl Default for MockModelResolver {
    fn default() -> Self {
        Self::new(vec![
            test_catalog_entry(TestCatalogEntryParams {
                model_id: "gpt-5.2".to_owned(),
                provider_model_id: "gpt-5.2-2025-03-26".to_owned(),
                display_name: "GPT-5.2".to_owned(),
                tier: mini_chat_sdk::ModelTier::Premium,
                enabled: true,
                is_default: true,
                input_tokens_credit_multiplier_micro: 2_000_000,
                output_tokens_credit_multiplier_micro: 6_000_000,
                multimodal_capabilities: vec![],
                context_window: 128_000,
                max_output_tokens: 16_384,
                description: String::new(),
                provider_display_name: "OpenAI".to_owned(),
                multiplier_display: "2x".to_owned(),
                provider_id: "openai".to_owned(),
            }),
            test_catalog_entry(TestCatalogEntryParams {
                model_id: "gpt-5-mini".to_owned(),
                provider_model_id: "gpt-5-mini-2025-03-26".to_owned(),
                display_name: "GPT-5 Mini".to_owned(),
                tier: mini_chat_sdk::ModelTier::Standard,
                enabled: false,
                is_default: false,
                input_tokens_credit_multiplier_micro: 1_000_000,
                output_tokens_credit_multiplier_micro: 3_000_000,
                multimodal_capabilities: vec![],
                context_window: 64_000,
                max_output_tokens: 8_192,
                description: String::new(),
                provider_display_name: "OpenAI".to_owned(),
                multiplier_display: "1x".to_owned(),
                provider_id: "openai".to_owned(),
            }),
        ])
    }
}

#[async_trait]
impl ModelResolver for MockModelResolver {
    async fn resolve_model(
        &self,
        _user_id: Uuid,
        model: Option<String>,
    ) -> Result<ResolvedModel, DomainError> {
        let catalog = self.catalog.lock().unwrap();
        match model {
            None => {
                let default = catalog
                    .iter()
                    .find(|m| m.preference.is_default && m.enabled)
                    .or_else(|| catalog.iter().find(|m| m.enabled));
                match default {
                    Some(e) => Ok(ResolvedModel::from(e)),
                    None => Err(DomainError::invalid_model("no models available in catalog")),
                }
            }
            Some(m) if m.is_empty() => Err(DomainError::invalid_model("model must not be empty")),
            Some(m) => {
                let entry = catalog.iter().find(|e| e.model_id == m && e.enabled);
                match entry {
                    Some(e) => Ok(ResolvedModel::from(e)),
                    None => Err(DomainError::invalid_model(&m)),
                }
            }
        }
    }

    async fn list_visible_models(&self, _user_id: Uuid) -> Result<Vec<ResolvedModel>, DomainError> {
        let catalog = self.catalog.lock().unwrap();
        Ok(catalog
            .iter()
            .filter(|m| m.enabled)
            .map(ResolvedModel::from)
            .collect())
    }

    async fn get_visible_model(
        &self,
        _user_id: Uuid,
        model_id: &str,
    ) -> Result<ResolvedModel, DomainError> {
        let catalog = self.catalog.lock().unwrap();
        catalog
            .iter()
            .find(|m| m.model_id == model_id && m.enabled)
            .map(ResolvedModel::from)
            .ok_or_else(|| DomainError::model_not_found(model_id))
    }
}

// ── Test Helpers ──

pub async fn inmem_db() -> Db {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("Failed to connect to in-memory database");

    run_migrations_for_testing(&db, crate::infra::db::migrations::Migrator::migrations())
        .await
        .expect("Failed to run migrations");

    db
}

pub fn test_security_ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("failed to build SecurityContext")
}

pub fn test_security_ctx_with_id(tenant_id: Uuid, subject_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject_id)
        .subject_tenant_id(tenant_id)
        .build()
        .expect("failed to build SecurityContext")
}

pub fn mock_enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(MockAuthZResolver);
    PolicyEnforcer::new(authz)
}

/// Always-deny `AuthZ` resolver for authorization denial tests.
struct DenyingAuthZResolver;

#[async_trait]
impl AuthZResolverClient for DenyingAuthZResolver {
    async fn evaluate(
        &self,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, AuthZResolverError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext {
                deny_reason: Some(DenyReason {
                    error_code: "access_denied".to_owned(),
                    details: Some("mock: always deny".to_owned()),
                }),
                ..Default::default()
            },
        })
    }
}

pub fn mock_denying_enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(DenyingAuthZResolver);
    PolicyEnforcer::new(authz)
}

pub fn mock_model_resolver() -> Arc<dyn ModelResolver> {
    Arc::new(MockModelResolver::default())
}

pub struct MockThreadSummaryRepo;
#[async_trait::async_trait]
impl ThreadSummaryRepository for MockThreadSummaryRepo {
    async fn get_latest<C: modkit_db::secure::DBRunner>(
        &self,
        _runner: &C,
        _scope: &modkit_security::AccessScope,
        _chat_id: uuid::Uuid,
    ) -> Result<Option<crate::domain::repos::ThreadSummaryModel>, crate::domain::error::DomainError>
    {
        Ok(None)
    }
}

pub fn mock_thread_summary_repo() -> Arc<MockThreadSummaryRepo> {
    Arc::new(MockThreadSummaryRepo)
}

pub fn mock_db_provider(db: Db) -> Arc<DBProvider<modkit_db::DbError>> {
    Arc::new(DBProvider::new(db))
}

// ── Mock Policy Snapshot Provider ──

use mini_chat_sdk::{PolicySnapshot, UserLimits};

/// Parameters for building a test [`ModelCatalogEntry`].
pub struct TestCatalogEntryParams {
    pub model_id: String,
    pub provider_model_id: String,
    pub display_name: String,
    pub tier: mini_chat_sdk::ModelTier,
    pub enabled: bool,
    pub is_default: bool,
    pub input_tokens_credit_multiplier_micro: u64,
    pub output_tokens_credit_multiplier_micro: u64,
    pub multimodal_capabilities: Vec<String>,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub description: String,
    pub provider_display_name: String,
    pub multiplier_display: String,
    pub provider_id: String,
}

/// Build a [`ModelCatalogEntry`] for tests, filling in new required fields with defaults.
#[allow(clippy::cast_precision_loss)]
pub fn test_catalog_entry(params: TestCatalogEntryParams) -> ModelCatalogEntry {
    use mini_chat_sdk::models::*;
    use time::OffsetDateTime;

    let tier_str = match params.tier {
        mini_chat_sdk::ModelTier::Premium => "premium",
        mini_chat_sdk::ModelTier::Standard => "standard",
    };
    let input_mult = params.input_tokens_credit_multiplier_micro as f64 / 1_000_000.0;
    let output_mult = params.output_tokens_credit_multiplier_micro as f64 / 1_000_000.0;
    let has_vision = params
        .multimodal_capabilities
        .iter()
        .any(|c| c == "VISION_INPUT");

    ModelCatalogEntry {
        model_id: params.model_id,
        provider_model_id: params.provider_model_id,
        display_name: params.display_name,
        description: params.description,
        version: String::new(),
        provider_id: params.provider_id,
        provider_display_name: params.provider_display_name,
        icon: String::new(),
        tier: params.tier,
        enabled: params.enabled,
        multimodal_capabilities: params.multimodal_capabilities,
        context_window: params.context_window,
        max_output_tokens: params.max_output_tokens,
        max_input_tokens: params.context_window,
        input_tokens_credit_multiplier_micro: params.input_tokens_credit_multiplier_micro,
        output_tokens_credit_multiplier_micro: params.output_tokens_credit_multiplier_micro,
        multiplier_display: params.multiplier_display.clone(),
        estimation_budgets: EstimationBudgets::default(),
        max_retrieved_chunks_per_turn: 5,
        general_config: ModelGeneralConfig {
            config_type: String::new(),
            tier: tier_str.to_owned(),
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
                image: has_vision,
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
                input_tokens_credit_multiplier: input_mult,
                output_tokens_credit_multiplier: output_mult,
            },
            performance: ModelPerformance {
                response_latency_ms: 500,
                speed_tokens_per_second: 100,
            },
        },
        preference: ModelPreference {
            is_default: params.is_default,
            sort_order: 0,
        },
        system_prompt: String::new(),
        thread_summary_prompt: String::new(),
    }
}

pub struct MockPolicySnapshotProvider {
    snapshot: Mutex<PolicySnapshot>,
}

impl MockPolicySnapshotProvider {
    pub fn new(snapshot: PolicySnapshot) -> Self {
        Self {
            snapshot: Mutex::new(snapshot),
        }
    }
}

#[async_trait]
impl PolicySnapshotProvider for MockPolicySnapshotProvider {
    async fn get_snapshot(
        &self,
        _user_id: Uuid,
        _policy_version: u64,
    ) -> Result<PolicySnapshot, DomainError> {
        Ok(self.snapshot.lock().unwrap().clone())
    }

    async fn get_current_version(&self, _user_id: Uuid) -> Result<u64, DomainError> {
        Ok(self.snapshot.lock().unwrap().policy_version)
    }
}

// ── Mock User Limits Provider ──

pub struct MockUserLimitsProvider {
    limits: Mutex<UserLimits>,
}

impl MockUserLimitsProvider {
    pub fn new(limits: UserLimits) -> Self {
        Self {
            limits: Mutex::new(limits),
        }
    }
}

#[async_trait]
impl UserLimitsProvider for MockUserLimitsProvider {
    async fn get_limits(
        &self,
        _user_id: Uuid,
        _policy_version: u64,
    ) -> Result<UserLimits, DomainError> {
        Ok(self.limits.lock().unwrap().clone())
    }
}
