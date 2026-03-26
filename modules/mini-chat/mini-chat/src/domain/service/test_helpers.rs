use std::sync::atomic::{AtomicU32, Ordering};
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
    AttachmentCleanupEvent, ModelResolver, OutboxEnqueuer, PolicySnapshotProvider,
    ThreadSummaryRepository, UserLimitsProvider,
};
use crate::domain::service::AuditEnvelope;

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
                    .find(|m| m.preference.as_ref().is_some_and(|p| p.is_default) && m.enabled)
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

    async fn get_kill_switches(
        &self,
        _user_id: Uuid,
    ) -> Result<mini_chat_sdk::KillSwitches, DomainError> {
        Ok(mini_chat_sdk::KillSwitches::default())
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

/// Tenant-only `AuthZ` resolver for services that mix owned and `no_owner` entities.
///
/// Returns `OWNER_TENANT_ID` constraint only (no `OWNER_ID`).
/// Use for `AttachmentService` tests where the attachment entity has `no_owner`
/// and would fail `secure_insert` scope validation if `OWNER_ID` is present.
struct TenantOnlyAuthZResolver;

#[async_trait]
impl AuthZResolverClient for TenantOnlyAuthZResolver {
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

        if request.context.require_constraints {
            let mut predicates = Vec::new();
            if let Some(tid) = subject_tenant_id {
                predicates.push(Predicate::Eq(EqPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    tid,
                )));
            }
            // Deliberately omit OWNER_ID so `no_owner` entities pass secure_insert.
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

/// Enforcer returning only tenant-level constraints.
///
/// Use for services that operate on `no_owner` entities (e.g. `AttachmentService`).
pub fn mock_tenant_only_enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverClient> = Arc::new(TenantOnlyAuthZResolver);
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

// ── Stream helpers ──

/// Convert `Bytes` into a `FileStream` for test use.
pub fn bytes_to_stream(data: bytes::Bytes) -> crate::domain::ports::FileStream {
    Box::pin(futures::stream::once(async { Ok(data) }))
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
        preference: Some(ModelPreference {
            is_default: params.is_default,
            sort_order: 0,
        }),
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

// ── Shared DB insertion helpers for tests ──

use modkit_db::secure::secure_insert;
use sea_orm::Set;
use time::OffsetDateTime;

use crate::infra::db::entity::attachment::{
    ActiveModel as AttachmentAM, AttachmentKind, AttachmentStatus, Entity as AttachmentEntity,
};
use crate::infra::db::entity::chat_vector_store::{
    ActiveModel as VectorStoreAM, Entity as VectorStoreEntity,
};
use crate::infra::db::entity::message_attachment::{
    ActiveModel as MessageAttachmentAM, Entity as MessageAttachmentEntity,
};

type TestDb = Arc<DBProvider<modkit_db::DbError>>;

/// Insert a parent chat row (required by FK constraints).
pub async fn insert_chat(db: &TestDb, tenant_id: Uuid, chat_id: Uuid) {
    insert_chat_for_user(db, tenant_id, chat_id, Uuid::new_v4()).await;
}

/// Insert a parent chat row owned by a specific user.
pub async fn insert_chat_for_user(db: &TestDb, tenant_id: Uuid, chat_id: Uuid, user_id: Uuid) {
    insert_chat_with_model(db, tenant_id, chat_id, user_id, "gpt-5.2").await;
}

/// Insert a parent chat row owned by a specific user with a given model.
pub async fn insert_chat_with_model(
    db: &TestDb,
    tenant_id: Uuid,
    chat_id: Uuid,
    user_id: Uuid,
    model: &str,
) {
    use crate::infra::db::entity::chat::{ActiveModel, Entity as ChatEntity};

    let now = OffsetDateTime::now_utc();
    let am = ActiveModel {
        id: Set(chat_id),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        model: Set(model.to_owned()),
        title: Set(Some("test".to_owned())),
        is_temporary: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
    };
    let conn = db.conn().unwrap();
    secure_insert::<ChatEntity>(am, &modkit_security::AccessScope::allow_all(), &conn)
        .await
        .expect("insert chat");
}

/// Parameters for inserting a test attachment.
pub struct InsertTestAttachmentParams {
    pub tenant_id: Uuid,
    pub chat_id: Uuid,
    pub uploaded_by_user_id: Uuid,
    pub kind: AttachmentKind,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub status: AttachmentStatus,
    pub provider_file_id: Option<String>,
    pub storage_backend: String,
    pub doc_summary: Option<String>,
    pub error_code: Option<String>,
    pub deleted_at: Option<OffsetDateTime>,
    pub for_file_search: bool,
    pub for_code_interpreter: bool,
}

impl InsertTestAttachmentParams {
    /// Convenience: ready document with sensible defaults.
    pub fn ready_document(tenant_id: Uuid, chat_id: Uuid) -> Self {
        Self {
            tenant_id,
            chat_id,
            uploaded_by_user_id: Uuid::new_v4(),
            kind: AttachmentKind::Document,
            filename: "test.pdf".to_owned(),
            content_type: "application/pdf".to_owned(),
            size_bytes: 1024,
            status: AttachmentStatus::Ready,
            provider_file_id: Some(format!("file-{}", Uuid::new_v4())),
            storage_backend: "openai".to_owned(),
            doc_summary: None,
            error_code: None,
            deleted_at: None,
            for_file_search: true,
            for_code_interpreter: false,
        }
    }
}

/// Insert an attachment row. Returns the attachment ID.
pub async fn insert_test_attachment(db: &TestDb, params: InsertTestAttachmentParams) -> Uuid {
    let now = OffsetDateTime::now_utc();
    let att_id = Uuid::now_v7();
    let am = AttachmentAM {
        id: Set(att_id),
        tenant_id: Set(params.tenant_id),
        chat_id: Set(params.chat_id),
        uploaded_by_user_id: Set(params.uploaded_by_user_id),
        filename: Set(params.filename),
        content_type: Set(params.content_type),
        size_bytes: Set(params.size_bytes),
        storage_backend: Set(params.storage_backend),
        provider_file_id: Set(params.provider_file_id),
        status: Set(params.status),
        error_code: Set(params.error_code),
        attachment_kind: Set(params.kind),
        for_file_search: Set(params.for_file_search),
        for_code_interpreter: Set(params.for_code_interpreter),
        doc_summary: Set(params.doc_summary),
        img_thumbnail: Set(None),
        img_thumbnail_width: Set(None),
        img_thumbnail_height: Set(None),
        summary_model: Set(None),
        summary_updated_at: Set(None),
        cleanup_status: Set(None),
        cleanup_attempts: Set(0),
        last_cleanup_error: Set(None),
        cleanup_updated_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(params.deleted_at),
    };
    let conn = db.conn().unwrap();
    secure_insert::<AttachmentEntity>(am, &modkit_security::AccessScope::allow_all(), &conn)
        .await
        .expect("insert test attachment");
    att_id
}

/// Insert a minimal message row (required as FK parent for `message_attachments`).
pub async fn insert_test_message(db: &TestDb, tenant_id: Uuid, chat_id: Uuid, message_id: Uuid) {
    use crate::infra::db::entity::message::{
        ActiveModel as MessageAM, Entity as MessageEntity, MessageRole,
    };

    let now = OffsetDateTime::now_utc();
    let am = MessageAM {
        id: Set(message_id),
        tenant_id: Set(tenant_id),
        chat_id: Set(chat_id),
        request_id: Set(None),
        role: Set(MessageRole::User),
        content: Set("test".to_owned()),
        content_type: Set("text".to_owned()),
        token_estimate: Set(1),
        provider_response_id: Set(None),
        request_kind: Set(None),
        features_used: Set(serde_json::json!([])),
        input_tokens: Set(0),
        output_tokens: Set(0),
        cache_read_input_tokens: Set(0),
        cache_write_input_tokens: Set(0),
        reasoning_tokens: Set(0),
        model: Set(None),
        is_compressed: Set(false),
        created_at: Set(now),
        deleted_at: Set(None),
    };
    let conn = db.conn().unwrap();
    secure_insert::<MessageEntity>(am, &modkit_security::AccessScope::allow_all(), &conn)
        .await
        .expect("insert test message");
}

/// Insert a `chat_vector_stores` row.
pub async fn insert_test_vector_store(
    db: &TestDb,
    tenant_id: Uuid,
    chat_id: Uuid,
    vector_store_id: Option<String>,
) -> Uuid {
    insert_test_vector_store_with_provider(db, tenant_id, chat_id, vector_store_id, "openai").await
}

pub async fn insert_test_vector_store_with_provider(
    db: &TestDb,
    tenant_id: Uuid,
    chat_id: Uuid,
    vector_store_id: Option<String>,
    provider: &str,
) -> Uuid {
    let now = OffsetDateTime::now_utc();
    let id = Uuid::now_v7();
    let am = VectorStoreAM {
        id: Set(id),
        tenant_id: Set(tenant_id),
        chat_id: Set(chat_id),
        vector_store_id: Set(vector_store_id),
        provider: Set(provider.to_owned()),
        file_count: Set(0),
        created_at: Set(now),
    };
    let conn = db.conn().unwrap();
    secure_insert::<VectorStoreEntity>(am, &modkit_security::AccessScope::allow_all(), &conn)
        .await
        .expect("insert test vector store");
    id
}

/// Link a message to an attachment via `message_attachments`.
pub async fn insert_test_message_attachment(
    db: &TestDb,
    tenant_id: Uuid,
    chat_id: Uuid,
    message_id: Uuid,
    attachment_id: Uuid,
) {
    let now = OffsetDateTime::now_utc();
    let am = MessageAttachmentAM {
        tenant_id: Set(tenant_id),
        chat_id: Set(chat_id),
        message_id: Set(message_id),
        attachment_id: Set(attachment_id),
        created_at: Set(now),
    };
    let conn = db.conn().unwrap();
    secure_insert::<MessageAttachmentEntity>(am, &modkit_security::AccessScope::allow_all(), &conn)
        .await
        .expect("insert test message attachment");
}

// ── Noop & Recording OutboxEnqueuer ──

/// No-op outbox enqueuer for tests that don't need outbox assertions.
#[allow(de0309_must_have_domain_model)]
pub struct NoopOutboxEnqueuer;

#[async_trait]
impl OutboxEnqueuer for NoopOutboxEnqueuer {
    async fn enqueue_usage_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: mini_chat_sdk::UsageEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }
    async fn enqueue_attachment_cleanup(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: AttachmentCleanupEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }
    async fn enqueue_audit_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: AuditEnvelope,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }
    fn flush(&self) {}
}

/// Recording outbox enqueuer that captures events for test assertions.
#[allow(de0309_must_have_domain_model)]
pub struct RecordingOutboxEnqueuer {
    pub usage_events: Mutex<Vec<mini_chat_sdk::UsageEvent>>,
    pub cleanup_events: Mutex<Vec<AttachmentCleanupEvent>>,
    recorded_audit_events: Mutex<Vec<AuditEnvelope>>,
    recorded_flush_count: AtomicU32,
}

impl RecordingOutboxEnqueuer {
    pub fn new() -> Self {
        Self {
            usage_events: Mutex::new(Vec::new()),
            cleanup_events: Mutex::new(Vec::new()),
            recorded_audit_events: Mutex::new(Vec::new()),
            recorded_flush_count: AtomicU32::new(0),
        }
    }

    pub fn audit_events(&self) -> Vec<AuditEnvelope> {
        self.recorded_audit_events.lock().unwrap().clone()
    }

    pub fn clear_audit_events(&self) {
        self.recorded_audit_events.lock().unwrap().clear();
    }

    pub fn flush_count(&self) -> u32 {
        self.recorded_flush_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl OutboxEnqueuer for RecordingOutboxEnqueuer {
    async fn enqueue_usage_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        event: mini_chat_sdk::UsageEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        self.usage_events.lock().unwrap().push(event);
        Ok(())
    }
    async fn enqueue_attachment_cleanup(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        event: AttachmentCleanupEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        self.cleanup_events.lock().unwrap().push(event);
        Ok(())
    }
    async fn enqueue_audit_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        event: AuditEnvelope,
    ) -> Result<(), crate::domain::error::DomainError> {
        self.recorded_audit_events.lock().unwrap().push(event);
        Ok(())
    }
    fn flush(&self) {
        self.recorded_flush_count.fetch_add(1, Ordering::SeqCst);
    }
}

/// Outbox enqueuer that fails on `enqueue_attachment_cleanup` for rollback testing.
#[allow(de0309_must_have_domain_model)]
pub struct FailingOutboxEnqueuer;

#[async_trait]
impl OutboxEnqueuer for FailingOutboxEnqueuer {
    async fn enqueue_usage_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: mini_chat_sdk::UsageEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }
    async fn enqueue_attachment_cleanup(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: AttachmentCleanupEvent,
    ) -> Result<(), crate::domain::error::DomainError> {
        Err(crate::domain::error::DomainError::database(
            "simulated outbox enqueue failure".to_owned(),
        ))
    }
    async fn enqueue_audit_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: AuditEnvelope,
    ) -> Result<(), crate::domain::error::DomainError> {
        Ok(())
    }
    fn flush(&self) {}
}

// ── Mock OAGW Gateway ──

use std::collections::VecDeque;

use oagw_sdk::error::ServiceGatewayError;
use oagw_sdk::{Body, ServiceGatewayClientV1};

/// Captured proxy request (URI, body string).
#[derive(Debug, Clone)]
pub struct CapturedRequest {
    pub uri: String,
    pub body: String,
}

/// Multi-response OAGW gateway mock for upload integration tests.
///
/// Supports multiple sequential `proxy_request` calls (e.g. upload file →
/// create vector store → add file to vector store). Responses are consumed
/// in FIFO order. Each call's URI and body are captured for assertions.
pub struct MockOagwGateway {
    responses: Mutex<VecDeque<Result<serde_json::Value, ServiceGatewayError>>>,
    pub captured_requests: Mutex<Vec<CapturedRequest>>,
}

impl MockOagwGateway {
    /// Create with a queue of JSON responses that will be returned in order.
    pub fn with_responses(
        responses: Vec<Result<serde_json::Value, ServiceGatewayError>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(VecDeque::from(responses)),
            captured_requests: Mutex::new(Vec::new()),
        })
    }

    /// Create that always errors.
    pub fn single_error(err: ServiceGatewayError) -> Arc<Self> {
        Self::with_responses(vec![Err(err)])
    }
}

#[async_trait]
impl ServiceGatewayClientV1 for MockOagwGateway {
    async fn create_upstream(
        &self,
        _: modkit_security::SecurityContext,
        _: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, ServiceGatewayError> {
        unimplemented!()
    }
    async fn get_upstream(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
    ) -> Result<oagw_sdk::Upstream, ServiceGatewayError> {
        unimplemented!()
    }
    async fn list_upstreams(
        &self,
        _: modkit_security::SecurityContext,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Upstream>, ServiceGatewayError> {
        unimplemented!()
    }
    async fn update_upstream(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
        _: oagw_sdk::UpdateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, ServiceGatewayError> {
        unimplemented!()
    }
    async fn delete_upstream(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
    ) -> Result<(), ServiceGatewayError> {
        unimplemented!()
    }
    async fn create_route(
        &self,
        _: modkit_security::SecurityContext,
        _: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, ServiceGatewayError> {
        unimplemented!()
    }
    async fn get_route(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
    ) -> Result<oagw_sdk::Route, ServiceGatewayError> {
        unimplemented!()
    }
    async fn list_routes(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Route>, ServiceGatewayError> {
        unimplemented!()
    }
    async fn update_route(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
        _: oagw_sdk::UpdateRouteRequest,
    ) -> Result<oagw_sdk::Route, ServiceGatewayError> {
        unimplemented!()
    }
    async fn delete_route(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
    ) -> Result<(), ServiceGatewayError> {
        unimplemented!()
    }
    async fn resolve_proxy_target(
        &self,
        _: modkit_security::SecurityContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), ServiceGatewayError> {
        unimplemented!()
    }
    async fn proxy_request(
        &self,
        _ctx: modkit_security::SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, ServiceGatewayError> {
        let uri = req.uri().to_string();
        let (_parts, body) = req.into_parts();
        let body_bytes = body
            .into_bytes()
            .await
            .expect("MockOagwGateway: failed to read request body");
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();
        self.captured_requests
            .lock()
            .unwrap()
            .push(CapturedRequest {
                uri,
                body: body_str,
            });

        let resp = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("MockOagwGateway: no more responses queued");

        match resp {
            Ok(json) => {
                let body = Body::Bytes(bytes::Bytes::from(serde_json::to_vec(&json).unwrap()));
                Ok(http::Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(body)
                    .unwrap())
            }
            Err(e) => Err(e),
        }
    }
}

// ── Mock User Limits Provider ──

// ── TestMetrics — recording implementation for metric assertions ─────

use std::sync::atomic::{AtomicI64, AtomicU64};

/// Lightweight `MiniChatMetricsPort` that records counter increments
/// and histogram observation counts via atomics. Used to verify that
/// service code emits the expected metrics.
pub struct TestMetrics {
    pub turn_mutation: AtomicU64,
    pub turn_mutation_latency_ms: AtomicU64,
    pub audit_emit: AtomicU64,
    pub finalization_latency_ms: AtomicU64,
    pub quota_commit: AtomicU64,
    pub quota_overshoot: AtomicU64,
    pub quota_actual_tokens: AtomicU64,
    pub streams_aborted: AtomicU64,
    pub attachment_upload: AtomicU64,
    pub attachment_upload_bytes: AtomicU64,
    pub attachments_pending: AtomicI64,
    pub code_interpreter_calls: AtomicU64,
}

impl TestMetrics {
    pub fn new() -> Self {
        Self {
            turn_mutation: AtomicU64::new(0),
            turn_mutation_latency_ms: AtomicU64::new(0),
            audit_emit: AtomicU64::new(0),
            finalization_latency_ms: AtomicU64::new(0),
            quota_commit: AtomicU64::new(0),
            quota_overshoot: AtomicU64::new(0),
            quota_actual_tokens: AtomicU64::new(0),
            streams_aborted: AtomicU64::new(0),
            attachment_upload: AtomicU64::new(0),
            attachment_upload_bytes: AtomicU64::new(0),
            attachments_pending: AtomicI64::new(0),
            code_interpreter_calls: AtomicU64::new(0),
        }
    }
}

impl crate::domain::ports::MiniChatMetricsPort for TestMetrics {
    fn record_stream_started(&self, _: &str, _: &str) {}
    fn record_stream_completed(&self, _: &str, _: &str) {}
    fn record_stream_failed(&self, _: &str, _: &str, _: &str) {}
    fn record_stream_disconnected(&self, _: &str) {}
    fn increment_active_streams(&self) {}
    fn decrement_active_streams(&self) {}
    fn record_ttft_provider_ms(&self, _: &str, _: &str, _: f64) {}
    fn record_ttft_overhead_ms(&self, _: &str, _: &str, _: f64) {}
    fn record_stream_total_latency_ms(&self, _: &str, _: &str, _: f64) {}
    fn record_turn_mutation(&self, _: &str, _: &str) {
        self.turn_mutation.fetch_add(1, Ordering::Relaxed);
    }
    fn record_turn_mutation_latency_ms(&self, _: &str, _: f64) {
        self.turn_mutation_latency_ms
            .fetch_add(1, Ordering::Relaxed);
    }
    fn record_audit_emit(&self, _: &str) {
        self.audit_emit.fetch_add(1, Ordering::Relaxed);
    }
    fn record_finalization_latency_ms(&self, _: f64) {
        self.finalization_latency_ms.fetch_add(1, Ordering::Relaxed);
    }
    fn record_quota_preflight(&self, _: &str, _: &str, _: &str) {}
    fn record_quota_reserve(&self, _: &str) {}
    fn record_quota_commit(&self, _: &str) {
        self.quota_commit.fetch_add(1, Ordering::Relaxed);
    }
    fn record_quota_overshoot(&self, _: &str) {
        self.quota_overshoot.fetch_add(1, Ordering::Relaxed);
    }
    fn record_quota_estimated_tokens(&self, _: f64) {}
    fn record_quota_actual_tokens(&self, _: f64) {
        self.quota_actual_tokens.fetch_add(1, Ordering::Relaxed);
    }
    fn record_stream_incomplete(&self, _: &str, _: &str, _: &str) {}
    fn record_cancel_requested(&self, _: &str) {}
    fn record_cancel_effective(&self, _: &str) {}
    fn record_time_to_abort_ms(&self, _: &str, _: f64) {}
    fn record_streams_aborted(&self, _: &str) {
        self.streams_aborted.fetch_add(1, Ordering::Relaxed);
    }
    fn record_attachment_upload(&self, _: &str, _: &str) {
        self.attachment_upload.fetch_add(1, Ordering::Relaxed);
    }
    fn record_attachment_upload_bytes(&self, _: &str, _: f64) {
        self.attachment_upload_bytes.fetch_add(1, Ordering::Relaxed);
    }
    fn increment_attachments_pending(&self) {
        self.attachments_pending.fetch_add(1, Ordering::Relaxed);
    }
    fn decrement_attachments_pending(&self) {
        self.attachments_pending.fetch_add(-1, Ordering::Relaxed);
    }
    fn record_image_inputs_per_turn(&self, _count: u32) {}
    fn record_code_interpreter_calls(&self, _: &str, _: u32) {
        self.code_interpreter_calls.fetch_add(1, Ordering::Relaxed);
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
