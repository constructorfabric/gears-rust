//! Test helpers shared by the service and repository tests: an in-memory
//! database with the gear's migrations, a security context, and stub policy
//! services.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, PolicyEnforcer,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::ScopeError;
use toolkit_db::{ConnectOpts, Db, connect_db};
use toolkit_security::{AccessScope, PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::api::rest::types::ConcreteIntake;
use crate::domain::error::DomainError;
use crate::domain::record_intake::{
    IntakePorts, ReceivedRecord, RecordHandOff, RecordIntakeService, RecordTypes,
};
use crate::domain::subject_settings::{SubjectSettings, TenantDefaults};
use crate::infra::connector_switch::ConfigConnectorSwitch;
use crate::infra::record_types::RegistryRecordTypes;
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::subject_settings_entity;

/// Allows every request and constrains the answer to the subject's tenant and,
/// when the request names one, the resource id (like a real PDP).
pub struct AllowResolver;

#[async_trait]
impl AuthZResolverApi for AllowResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let root_id = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|tc| tc.root_id)
            .or_else(|| {
                request
                    .subject
                    .properties
                    .get("tenant_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            .ok_or_else(|| {
                CanonicalError::internal("tenant context is required".to_owned()).create()
            })?;

        let mut predicates = vec![Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            [root_id],
        ))];
        if let Some(resource_id) = request.resource.id {
            predicates.push(Predicate::In(InPredicate::new(
                pep_properties::RESOURCE_ID,
                [resource_id],
            )));
        }

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                ..Default::default()
            },
        })
    }
}

/// Denies every request: a subject without permission.
pub struct DenyResolver;

#[async_trait]
impl AuthZResolverApi for DenyResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

pub async fn inmem_db() -> Db {
    use sea_orm_migration::MigratorTrait;

    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect in-memory database");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");
    db
}

pub fn context_in(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap()
}

/// Stores one subject's settings row in `tenant_id`, through the secure ORM
/// with a scope for that tenant. `None` for the erasure flag leaves the column
/// out of the insert, so the table default applies. The insert's error is
/// returned, not unwrapped, so a test can expect a refusal.
pub async fn insert_subject_settings(
    db: &Db,
    tenant_id: Uuid,
    subject_id: Uuid,
    personalization_enabled: bool,
    erasure_in_progress: Option<bool>,
) -> Result<(), ScopeError> {
    use sea_orm::{ActiveValue, EntityTrait};
    use toolkit_db::secure::SecureInsertExt;

    let row = subject_settings_entity::ActiveModel {
        tenant_id: ActiveValue::Set(tenant_id),
        subject_id: ActiveValue::Set(subject_id),
        personalization_enabled: ActiveValue::Set(personalization_enabled),
        erasure_in_progress: erasure_in_progress.map_or(ActiveValue::NotSet, ActiveValue::Set),
    };
    let conn = db.conn().expect("connection");
    subject_settings_entity::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&AccessScope::for_tenant(tenant_id), &row)?
        .exec(&conn)
        .await?;
    Ok(())
}

/// Stores one subject's settings, both columns set.
pub async fn seed_subject_settings(
    db: &Db,
    tenant_id: Uuid,
    subject_id: Uuid,
    settings: SubjectSettings,
) {
    insert_subject_settings(
        db,
        tenant_id,
        subject_id,
        settings.personalization_enabled,
        Some(settings.erasure_in_progress),
    )
    .await
    .expect("insert subject settings");
}

/// One request to the policy service: resource type, action, the context
/// tenant and the tenant named as the owner of the resource.
pub type AskedOfPolicyService = (String, String, Option<Uuid>, Option<String>);

/// Records what is asked of the policy service, then allows it like
/// [`AllowResolver`].
#[derive(Default)]
pub struct RecordingResolver {
    pub asked: std::sync::Mutex<Vec<AskedOfPolicyService>>,
}

#[async_trait]
impl AuthZResolverApi for RecordingResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.asked.lock().unwrap().push((
            request.resource.resource_type.clone(),
            request.action.name.clone(),
            request
                .context
                .tenant_context
                .as_ref()
                .and_then(|tc| tc.root_id),
            request
                .resource
                .properties
                .get(pep_properties::OWNER_TENANT_ID)
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        ));
        AllowResolver.evaluate(ctx, request).await
    }
}

/// Allows every request but constrains it to the caller's own tenant,
/// ignoring the tenant the request names: a connector the platform does not
/// authorize for another tenant.
pub struct OwnTenantOnlyResolver;

#[async_trait]
impl AuthZResolverApi for OwnTenantOnlyResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        mut request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        request.context.tenant_context = None;
        AllowResolver.evaluate(ctx, request).await
    }
}

/// A chat message record, valid for its type, about `subject_id`.
pub fn chat_record(subject_id: Uuid, provenance: &str, version: &str) -> serde_json::Value {
    serde_json::json!({
        "type": construct_sdk::gts::CHAT_MESSAGE_TYPE,
        "provenance": provenance,
        "version": version,
        "observed_at": "2026-09-10T09:13:05Z",
        "subject_id": subject_id.to_string(),
        "payload": {
            "source": "chat_engine",
            "message_id": "msg-7",
            "thread_id": "thread-42",
            "role": "user",
            "text": "Which of these two papers contradict each other?"
        }
    })
}

/// One valid record of each derived type: the rolos-cyber examples
/// (`docs/construct/GTS/examples/valid/`) without their store `id`.
pub fn example_records() -> Vec<serde_json::Value> {
    let subject = "5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b";
    vec![
        chat_record(
            Uuid::parse_str(subject).expect("uuid"),
            "chat_engine/thread-42/msg-7",
            "2026-09-10T09:13:00Z",
        ),
        serde_json::json!({
            "type": construct_sdk::gts::ENRICHMENT_FACT_CANDIDATE_TYPE,
            "provenance": "openalex/A5012345678/works/W2741809807",
            "version": "2026-09-09T18:00:00Z",
            "observed_at": "2026-09-10T07:30:00Z",
            "subject_id": subject,
            "payload": {
                "claim": "Jane Doe is a professor of computational biology at ETH Zurich.",
                "fact_type": "role",
                "confidence": 0.83,
                "trust_tier": "found",
                "source_url": "https://openalex.org/A5012345678",
                "date": "2026-09-09",
                "citations": [{
                    "source_url": "https://openalex.org/A5012345678",
                    "chunk_id": "affiliations-0",
                    "verbatim_quote": "ETH Zurich, Department of Biosystems Science and Engineering",
                    "char_start": 0,
                    "char_end": 60
                }]
            }
        }),
        serde_json::json!({
            "type": construct_sdk::gts::MASTERY_COURSE_CATALOG_TYPE,
            "provenance": "lms-eu/course-101",
            "version": "2026-09-01T12:00:00Z",
            "observed_at": "2026-09-10T08:05:00Z",
            "payload": {
                "course_id": "course-101",
                "title": "Introduction to Computational Biology",
                "sections": [{ "section_index": 0, "title": "Sequence alignment" }],
                "concepts": [
                    { "concept_index": 0, "name": "Dynamic programming", "section_index": 0 },
                    { "concept_index": 1, "name": "Substitution matrices", "section_index": 0 }
                ]
            }
        }),
        serde_json::json!({
            "type": construct_sdk::gts::MASTERY_STUDENT_MASTERY_TYPE,
            "provenance": "lms-eu/course-101/student-5d1f2c3a-8b4e-4c6d-9a1b-2c3d4e5f6a7b",
            "version": "2026-09-10T08:00:00Z",
            "observed_at": "2026-09-10T08:05:00Z",
            "subject_id": subject,
            "payload": {
                "course_id": "course-101",
                "model_name": "bkt",
                "model_version": 3,
                "concepts": [
                    { "concept_index": 0, "score": 0.81, "signal_count": 12 },
                    { "concept_index": 1, "score": 0.35, "signal_count": 4 }
                ]
            }
        }),
    ]
}

/// The record types of the SDK, as the types registry serves them after it
/// drained the link-time inventory: the base, and each derived type with the
/// base as its parent.
pub fn sdk_record_type_schemas() -> Vec<types_registry_sdk::GtsTypeSchema> {
    use types_registry_sdk::{GtsTypeId, GtsTypeSchema};

    let base = Arc::new(
        GtsTypeSchema::try_new(
            GtsTypeId::new(construct_sdk::gts::RECORD_BASE_TYPE),
            serde_json::from_str(construct_sdk::gts::RECORD_BASE_SCHEMA).expect("base schema"),
            None,
            None,
        )
        .expect("base type"),
    );
    let mut schemas = vec![(*base).clone()];
    for record_type in construct_sdk::gts::RECORD_TYPES
        .iter()
        .filter(|record_type| record_type.type_id != construct_sdk::gts::RECORD_BASE_TYPE)
    {
        schemas.push(
            GtsTypeSchema::try_new(
                GtsTypeId::new(record_type.type_id),
                serde_json::from_str(record_type.schema).expect("derived schema"),
                None,
                Some(Arc::clone(&base)),
            )
            .expect("derived type"),
        );
    }
    schemas
}

/// A `ClientHub` with a types registry that knows the SDK's record types.
pub fn hub_with_record_types() -> Arc<toolkit::client_hub::ClientHub> {
    use types_registry_sdk::TypesRegistryClient;
    use types_registry_sdk::testing::MockTypesRegistryClient;

    let hub = Arc::new(toolkit::client_hub::ClientHub::new());
    let registry: Arc<dyn TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new().with_type_schemas(sdk_record_type_schemas()));
    hub.register::<dyn TypesRegistryClient>(registry);
    hub
}

/// Keeps every record handed off, so a test can see what was received.
#[derive(Default)]
pub struct RecordingHandOff {
    pub received: std::sync::Mutex<Vec<ReceivedRecord>>,
}

impl RecordHandOff for RecordingHandOff {
    fn hand_off(&self, record: ReceivedRecord) -> Result<(), DomainError> {
        self.received.lock().unwrap().push(record);
        Ok(())
    }
}

/// The policy service answers with an error instead of a decision.
pub struct ErroringResolver(pub fn() -> CanonicalError);

#[async_trait]
impl AuthZResolverApi for ErroringResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err((self.0)())
    }
}

/// The policy service never answers.
pub struct PendingResolver;

#[async_trait]
impl AuthZResolverApi for PendingResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        std::future::pending().await
    }
}

/// The tenant default every test intake uses.
pub struct FixedDefaults(pub bool);

#[async_trait]
impl TenantDefaults for FixedDefaults {
    async fn personalization_default(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
    ) -> Result<bool, DomainError> {
        Ok(self.0)
    }
}

/// What a test intake is built from. The defaults take every record a valid
/// connector sends: the SDK's record types, every connector on, and
/// personalization on for new subjects.
pub struct IntakeFixture {
    pub resolver: Arc<dyn AuthZResolverApi>,
    pub types: Arc<dyn RecordTypes>,
    pub connectors_off: Vec<Uuid>,
    pub personalization_default: bool,
    pub hand_off: Arc<RecordingHandOff>,
    /// Wire the production stub, which refuses every record, instead of the
    /// recording hand-off.
    pub no_processing: bool,
    /// A deadline for the policy service, shorter than the enforcer's default.
    pub policy_deadline: Option<std::time::Duration>,
}

impl Default for IntakeFixture {
    fn default() -> Self {
        Self {
            resolver: Arc::new(AllowResolver),
            types: Arc::new(
                RegistryRecordTypes::new(hub_with_record_types()).expect("record types"),
            ),
            connectors_off: Vec::new(),
            personalization_default: true,
            hand_off: Arc::new(RecordingHandOff::default()),
            no_processing: false,
            policy_deadline: None,
        }
    }
}

impl IntakeFixture {
    pub fn build(&self, db: &Db) -> ConcreteIntake {
        use crate::domain::subject_settings::SubjectSettingsService;
        use crate::infra::storage::record_ids_repo::SeaOrmRecordIdRepository;
        use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;

        let db = Arc::new(toolkit_db::DBProvider::new(db.clone()));
        let mut enforcer = PolicyEnforcer::new(Arc::clone(&self.resolver));
        if let Some(deadline) = self.policy_deadline {
            enforcer = enforcer.with_deadline(deadline);
        }
        let hand_off: Arc<dyn RecordHandOff> = if self.no_processing {
            Arc::new(crate::infra::planner_stub::NoProcessingHandOff)
        } else {
            Arc::clone(&self.hand_off) as Arc<dyn RecordHandOff>
        };
        let settings = Arc::new(SubjectSettingsService::new(
            Arc::clone(&db),
            Arc::new(SeaOrmSubjectSettingsRepository::new()),
            Arc::new(FixedDefaults(self.personalization_default)),
            enforcer.clone(),
        ));
        RecordIntakeService::new(
            db,
            Arc::new(SeaOrmRecordIdRepository::new()),
            settings,
            IntakePorts {
                types: Arc::clone(&self.types),
                switch: Arc::new(ConfigConnectorSwitch::new(self.connectors_off.clone())),
                hand_off,
            },
            enforcer,
        )
    }

    pub fn received(&self) -> Vec<ReceivedRecord> {
        self.hand_off.received.lock().unwrap().clone()
    }
}

/// How many record identities are stored, in every tenant.
pub async fn stored_record_ids(db: &Db) -> usize {
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;

    let conn = db.conn().expect("connection");
    crate::infra::storage::record_ids_entity::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .all(&conn)
        .await
        .expect("list record ids")
        .len()
}
