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
use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
use toolkit_security::{AccessScope, PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::api::rest::types::ConcreteIntake;
use crate::domain::admission::{
    Admission, AdmissionEvents, AdmissionPorts, AdmissionRules, AdmissionRulesSource, Allowed,
    SensitiveDataEvent,
};
use crate::domain::error::DomainError;
use crate::domain::plan::{Change, NodeKey, Origin, Plan, PlanValue, Property, Step};
use crate::domain::record_intake::{
    DropEvent, Envelope, IntakeEvents, IntakePorts, ReceivedRecord, RecordHandOff,
    RecordIntakeService, RecordTypes,
};
use crate::domain::sensitive_data::{Found, SensitiveDataCheck, SensitiveDataKind, Verdict};
use crate::domain::subject_settings::{
    SettingsLookup, SubjectKey, SubjectSettings, SubjectSettingsReader, SubjectSettingsService,
    TenantDefaults,
};
use crate::infra::connector_switch::ConfigConnectorSwitch;
use crate::infra::record_types::RegistryRecordTypes;
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::subject_settings_entity;
use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;

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

/// Allows every request like [`AllowResolver`] and adds an owner constraint for
/// the caller, as a policy that says "owner only" would.
pub struct OwnerOnlyResolver;

#[async_trait]
impl AuthZResolverApi for OwnerOnlyResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let caller = request.subject.id;
        let mut response = AllowResolver.evaluate(ctx, request).await?;
        for constraint in &mut response.context.constraints {
            constraint.predicates.push(Predicate::In(InPredicate::new(
                pep_properties::OWNER_ID,
                [caller],
            )));
        }
        Ok(response)
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

/// Answers as [`AllowResolver`] once `delay` has passed: a stalled policy
/// service.
pub struct SlowResolver(pub std::time::Duration);

#[async_trait]
impl AuthZResolverApi for SlowResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        tokio::time::sleep(self.0).await;
        AllowResolver.evaluate(ctx, request).await
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
    context_of(Uuid::new_v4(), tenant_id)
}

/// The security context of one subject in one tenant.
pub fn context_of(subject_id: Uuid, tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject_id)
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap()
}

/// The same personalization default for every tenant.
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

/// A subject settings service on `db`, with `resolver` as the policy service
/// and `personalization_default` as every tenant's default.
pub fn settings_service(
    db: &Db,
    resolver: Arc<dyn AuthZResolverApi>,
    personalization_default: bool,
) -> SubjectSettingsService<SeaOrmSubjectSettingsRepository> {
    SubjectSettingsService::new(
        Arc::new(DBProvider::new(db.clone())),
        Arc::new(SeaOrmSubjectSettingsRepository::new()),
        Arc::new(FixedDefaults(personalization_default)),
        PolicyEnforcer::new(resolver),
    )
}

/// Stores one subject's settings row, through the secure ORM with a scope for
/// the key's tenant. `None` for the erasure flag leaves the column
/// out of the insert, so the table default applies. The insert's error is
/// returned, not unwrapped, so a test can expect a refusal.
pub async fn insert_subject_settings(
    db: &Db,
    key: SubjectKey,
    personalization_enabled: bool,
    erasure_in_progress: Option<bool>,
) -> Result<(), ScopeError> {
    use sea_orm::{ActiveValue, EntityTrait};
    use toolkit_db::secure::SecureInsertExt;

    let row = subject_settings_entity::ActiveModel {
        tenant_id: ActiveValue::Set(key.tenant_id),
        subject_id: ActiveValue::Set(key.subject_id),
        personalization_enabled: ActiveValue::Set(personalization_enabled),
        erasure_in_progress: erasure_in_progress.map_or(ActiveValue::NotSet, ActiveValue::Set),
    };
    let conn = db.conn().expect("connection");
    subject_settings_entity::Entity::insert(row.clone())
        .secure()
        .scope_with_model(&AccessScope::for_tenant(key.tenant_id), &row)?
        .exec(&conn)
        .await?;
    Ok(())
}

/// Stores one subject's settings, both columns set.
pub async fn seed_subject_settings(db: &Db, key: SubjectKey, settings: SubjectSettings) {
    insert_subject_settings(
        db,
        key,
        settings.personalization_enabled,
        Some(settings.erasure_in_progress),
    )
    .await
    .expect("insert subject settings");
}

/// A lookup read as "the stored row, or none": for tests whose scope covers
/// the key, so the lookup is `Stored` or `Absent`.
pub trait CoveredLookup {
    fn stored(self) -> Option<SubjectSettings>;
}

impl CoveredLookup for SettingsLookup {
    fn stored(self) -> Option<SubjectSettings> {
        match self {
            Self::Stored(settings) => Some(settings),
            Self::Absent => None,
            Self::OutOfScope(refusal) => {
                panic!("the scope should cover the key, got {refusal:?}")
            }
        }
    }
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

/// A received chat message for `subject_id` in `tenant_id`, as intake hands it on.
pub fn received_chat_record(tenant_id: Uuid, subject_id: Uuid) -> ReceivedRecord {
    let provenance = "chat_engine/thread-42/msg-7";
    ReceivedRecord {
        tenant_id,
        connector: "chat_engine".to_owned(),
        envelope: Envelope {
            type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
            provenance: provenance.to_owned(),
            version: "v1".to_owned(),
            subject_id: Some(subject_id),
            observed_at: time::OffsetDateTime::UNIX_EPOCH,
        },
        record: chat_record(subject_id, provenance, "v1"),
    }
}

/// One valid record of each derived type.
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
            "provenance": "sha256:274b22ce748a45969ec5ead82845fc9324ffa01755758d9f2ab44c98f690fc15",
            "version": "sha256:f0606b676be2895716327ce8c55faa64bfbcd9e6231d2a25f081e6e8bb716e79",
            "observed_at": "2026-09-10T07:30:00Z",
            "subject_id": subject,
            "payload": {
                "field": "board",
                "value": "Jane Doe serves on the scientific advisory board of the Swiss Institute of Bioinformatics.",
                "evidence": "Jane Doe joined the institute's scientific advisory board in 2024.",
                "source_url": "https://example.org/news/sib-advisory-board",
                "confidence": 0.83,
                "trust_tier": "found",
                "identity_confidence": 0.91
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

    let registered =
        toolkit_gts::all_inventory_type_schemas().expect("the inventory schemas parse");
    let schema = |type_id: &str| -> serde_json::Value {
        registered
            .iter()
            .find(|schema| schema["$id"] == format!("gts://{type_id}"))
            .cloned()
            .expect("the SDK submits every record type")
    };
    let base = Arc::new(
        GtsTypeSchema::try_new(
            GtsTypeId::new(construct_sdk::gts::RECORD_BASE_TYPE),
            schema(construct_sdk::gts::RECORD_BASE_TYPE),
            None,
            None,
        )
        .expect("base type"),
    );
    let mut schemas = vec![(*base).clone()];
    for type_id in construct_sdk::gts::RECORD_TYPES
        .into_iter()
        .filter(|type_id| *type_id != construct_sdk::gts::RECORD_BASE_TYPE)
    {
        schemas.push(
            GtsTypeSchema::try_new(
                GtsTypeId::new(type_id),
                schema(type_id),
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

/// Keeps every drop event, so a test can see what was dropped.
#[derive(Default)]
pub struct RecordingEvents {
    pub dropped: std::sync::Mutex<Vec<DropEvent>>,
}

impl RecordingEvents {
    pub fn taken(&self) -> Vec<DropEvent> {
        self.dropped.lock().unwrap().clone()
    }
}

impl IntakeEvents for RecordingEvents {
    fn dropped(&self, event: DropEvent) {
        self.dropped.lock().unwrap().push(event);
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

/// What a test intake is built from. The defaults take every record a valid
/// connector sends: the SDK's record types, every connector on, and
/// personalization on for new subjects.
pub struct IntakeFixture {
    pub resolver: Arc<dyn AuthZResolverApi>,
    pub types: Arc<dyn RecordTypes>,
    pub connectors_off: Vec<Uuid>,
    pub personalization_default: bool,
    pub hand_off: Arc<RecordingHandOff>,
    /// Wire the production stub, which drops every record with a drop event,
    /// instead of the recording hand-off. The events go to `events`.
    pub dropping: bool,
    pub events: Arc<RecordingEvents>,
    /// A deadline for the policy service, shorter than the enforcer's default.
    pub policy_deadline: Option<std::time::Duration>,
    /// Hand received records here instead, such as the processing queue.
    pub hand_off_to: Option<Arc<dyn RecordHandOff>>,
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
            dropping: false,
            events: Arc::new(RecordingEvents::default()),
            policy_deadline: None,
            hand_off_to: None,
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
        let hand_off: Arc<dyn RecordHandOff> = if let Some(hand_off) = &self.hand_off_to {
            Arc::clone(hand_off)
        } else if self.dropping {
            Arc::new(crate::infra::planner_stub::DroppingHandOff::new(
                Arc::clone(&self.events) as Arc<dyn IntakeEvents>,
            ))
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

// --- Plans, checks and Admission ---

/// A plan from a connector's record for a new subject, with the given steps.
pub fn connector_plan(steps: Vec<Step>) -> Plan {
    Plan {
        tenant_id: Uuid::new_v4(),
        subject_id: Uuid::new_v4(),
        origin: Origin::Connector {
            connector: "connector-a".to_owned(),
            provenance: "chat_engine/thread-42/msg-7".to_owned(),
            version: "v1".to_owned(),
            type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
            observed_at: time::OffsetDateTime::UNIX_EPOCH,
        },
        profile_version: Some(3),
        steps,
    }
}

/// The same plan, written by a person with `origin`: no step has a confidence.
pub fn by_a_person(mut plan: Plan, origin: Origin) -> Plan {
    plan.origin = origin;
    for step in &mut plan.steps {
        step.confidence = None;
    }
    plan
}

/// A model's step that adds an entity of `kind` with `properties`.
pub fn add_entity(kind: &str, properties: &[(&str, &str)]) -> Step {
    Step {
        entity_kind: kind.to_owned(),
        change: Change::AddEntity {
            key: NodeKey(Uuid::new_v4().to_string()),
            properties: properties
                .iter()
                .map(|(name, value)| Property::new(*name, *value))
                .collect(),
        },
        confidence: Some(0.9),
    }
}

/// A model's step that adds an entity of `kind` with one property whose value
/// is `value`, such as an object of fields.
pub fn add_value(kind: &str, name: &str, value: serde_json::Value) -> Step {
    Step {
        entity_kind: kind.to_owned(),
        change: Change::AddEntity {
            key: NodeKey(Uuid::new_v4().to_string()),
            properties: vec![Property::new(name, value)],
        },
        confidence: Some(0.9),
    }
}

/// A model's step that gives an existing property of `kind` a new value.
pub fn replace_property(kind: &str, name: &str, value: &str) -> Step {
    Step {
        entity_kind: kind.to_owned(),
        change: Change::ReplaceProperty {
            entity: NodeKey("entity-1".to_owned()),
            property: Property::new(name, value),
        },
        confidence: Some(0.9),
    }
}

/// A model's step that removes an entity of `kind`.
pub fn remove_entity(kind: &str) -> Step {
    Step {
        entity_kind: kind.to_owned(),
        change: Change::RemoveEntity {
            entity: NodeKey("entity-2".to_owned()),
        },
        confidence: Some(0.9),
    }
}

/// Byte ranges of found content, from `(start, end)` pairs.
pub fn spans(pairs: &[(usize, usize)]) -> Vec<std::ops::Range<usize>> {
    pairs.iter().map(|&(start, end)| start..end).collect()
}

/// A test check: finds every occurrence of `needle` in a value, as content of
/// its kind. It never runs in a deployment.
pub struct SubstringCheck {
    pub kind: SensitiveDataKind,
    pub needle: &'static str,
}

#[async_trait]
impl SensitiveDataCheck for SubstringCheck {
    fn kind(&self) -> SensitiveDataKind {
        self.kind
    }

    async fn check(&self, values: &[PlanValue]) -> Result<Verdict, DomainError> {
        let found = values
            .iter()
            .filter_map(|value| {
                let spans: Vec<_> = value
                    .text
                    .match_indices(self.needle)
                    .map(|(start, hit)| start..start + hit.len())
                    .collect();
                (!spans.is_empty()).then_some(Found {
                    at: value.at,
                    spans,
                })
            })
            .collect();
        Ok(Verdict {
            kind: self.kind,
            found,
        })
    }
}

/// A check that cannot answer, as a model check whose call failed.
pub struct FailingCheck(pub SensitiveDataKind);

#[async_trait]
impl SensitiveDataCheck for FailingCheck {
    fn kind(&self) -> SensitiveDataKind {
        self.0
    }

    async fn check(&self, _values: &[PlanValue]) -> Result<Verdict, DomainError> {
        Err(DomainError::Unavailable("model check timed out".to_owned()))
    }
}

/// A check that answers with a fixed verdict, whatever the values.
pub struct FixedCheck(pub SensitiveDataKind, pub Verdict);

#[async_trait]
impl SensitiveDataCheck for FixedCheck {
    fn kind(&self) -> SensitiveDataKind {
        self.0
    }

    async fn check(&self, _values: &[PlanValue]) -> Result<Verdict, DomainError> {
        Ok(self.1.clone())
    }
}

/// Keeps every block and redaction event, with its action.
#[derive(Default)]
pub struct RecordingAdmissionEvents {
    pub events: std::sync::Mutex<Vec<(&'static str, SensitiveDataEvent)>>,
}

impl RecordingAdmissionEvents {
    pub fn taken(&self) -> Vec<(&'static str, SensitiveDataEvent)> {
        self.events.lock().unwrap().clone()
    }
}

impl AdmissionEvents for RecordingAdmissionEvents {
    fn blocked(&self, event: SensitiveDataEvent) {
        self.events.lock().unwrap().push(("blocked", event));
    }

    fn redacted(&self, event: SensitiveDataEvent) {
        self.events.lock().unwrap().push(("redacted", event));
    }
}

/// The subject's settings, the same for every subject; or a failed read.
pub struct FixedSubjectSettings(pub Result<SubjectSettings, fn() -> DomainError>);

#[async_trait]
impl SubjectSettingsReader for FixedSubjectSettings {
    async fn read(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _key: SubjectKey,
    ) -> Result<SubjectSettings, DomainError> {
        self.0.map_err(|make| make())
    }
}

/// The tenant's admission rules, the same for every tenant; or a failed read.
pub struct FixedRules(pub Result<AdmissionRules, fn() -> DomainError>);

#[async_trait]
impl AdmissionRulesSource for FixedRules {
    async fn rules(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
    ) -> Result<AdmissionRules, DomainError> {
        self.0.clone().map_err(|make| make())
    }
}

/// Rules that let every plan in the tests through, with a confidence floor
/// of 0.6.
pub fn open_rules() -> AdmissionRules {
    AdmissionRules {
        trusted_connectors: Allowed::Any,
        confidence_floor: 0.6,
        entity_kinds: Allowed::Any,
        max_steps: 100,
        max_value_chars: 1000,
    }
}

/// Admission over fixed settings and rules, with recording events.
pub struct AdmissionFixture {
    pub settings: SubjectSettings,
    pub rules: AdmissionRules,
    pub events: Arc<RecordingAdmissionEvents>,
    pub drops: Arc<RecordingEvents>,
}

impl Default for AdmissionFixture {
    fn default() -> Self {
        Self {
            settings: SubjectSettings {
                personalization_enabled: true,
                erasure_in_progress: false,
            },
            rules: open_rules(),
            events: Arc::default(),
            drops: Arc::default(),
        }
    }
}

impl AdmissionFixture {
    pub fn build(&self) -> Admission {
        Admission::new(AdmissionPorts {
            settings: Arc::new(FixedSubjectSettings(Ok(self.settings))),
            rules: Arc::new(FixedRules(Ok(self.rules.clone()))),
            events: Arc::clone(&self.events) as Arc<dyn AdmissionEvents>,
            drops: Arc::clone(&self.drops) as Arc<dyn IntakeEvents>,
        })
    }
}

/// The security context and scope processing builds for a tenant.
pub fn processing_context(tenant_id: Uuid) -> (SecurityContext, AccessScope) {
    (context_in(tenant_id), AccessScope::for_tenant(tenant_id))
}

// --- The profile read ---

/// Stored facts a test gives, for every subject. It keeps each call: the
/// subject of the context it was called with, and the tenant and subject read.
pub struct FixedFacts {
    facts: Vec<crate::domain::profile_read::StoredFact>,
    pub calls: std::sync::Mutex<Vec<(Uuid, SubjectKey)>>,
}

impl FixedFacts {
    pub fn new(facts: Vec<crate::domain::profile_read::StoredFact>) -> Self {
        Self {
            facts,
            calls: std::sync::Mutex::default(),
        }
    }
}

#[async_trait]
impl crate::domain::profile_read::StoredProfiles for FixedFacts {
    async fn stored(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
    ) -> Result<Vec<crate::domain::profile_read::StoredFact>, DomainError> {
        self.calls.lock().unwrap().push((ctx.subject_id(), key));
        Ok(self.facts.clone())
    }
}

/// Keeps every profile read event.
#[derive(Default)]
pub struct RecordingProfileEvents {
    pub events: std::sync::Mutex<Vec<crate::domain::profile_read::ProfileReadEvent>>,
}

impl RecordingProfileEvents {
    pub fn taken(&self) -> Vec<crate::domain::profile_read::ProfileReadEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl crate::domain::profile_read::ProfileEvents for RecordingProfileEvents {
    fn read(&self, event: crate::domain::profile_read::ProfileReadEvent) {
        self.events.lock().unwrap().push(event);
    }
}

/// The profile read over `facts`, the subject settings in `db` with
/// personalization on by default, and the policy service `resolver`.
pub fn profile_service(
    db: &Db,
    resolver: Arc<dyn AuthZResolverApi>,
    facts: Vec<crate::domain::profile_read::StoredFact>,
    events: Arc<RecordingProfileEvents>,
    check: crate::domain::profile_read::CategoryCheck,
) -> crate::domain::profile_read::ProfileReadService {
    profile_service_over(
        db,
        resolver,
        Arc::new(FixedFacts::new(facts)),
        Uuid::new_v4(),
        events,
        check,
    )
}

/// The profile read over `profiles`, read as the processing identity
/// `identity`.
pub fn profile_service_over(
    db: &Db,
    resolver: Arc<dyn AuthZResolverApi>,
    profiles: Arc<dyn crate::domain::profile_read::StoredProfiles>,
    identity: Uuid,
    events: Arc<RecordingProfileEvents>,
    check: crate::domain::profile_read::CategoryCheck,
) -> crate::domain::profile_read::ProfileReadService {
    profile_service_reviewed(
        db,
        resolver,
        profiles,
        identity,
        events,
        check,
        Arc::default(),
    )
}

/// [`profile_service_over`] with the review requests `reviews`.
pub fn profile_service_reviewed(
    db: &Db,
    resolver: Arc<dyn AuthZResolverApi>,
    profiles: Arc<dyn crate::domain::profile_read::StoredProfiles>,
    identity: Uuid,
    events: Arc<RecordingProfileEvents>,
    check: crate::domain::profile_read::CategoryCheck,
    reviews: Arc<MemoryReviews>,
) -> crate::domain::profile_read::ProfileReadService {
    use crate::domain::profile_read::{ProfileEvents, ProfileReadPorts, ProfileReadService};
    ProfileReadService::new(
        ProfileReadPorts {
            settings: Arc::new(settings_service(db, Arc::clone(&resolver), true)),
            profiles,
            contexts: Arc::new(crate::infra::processing_contexts::ServiceIdentity::new(
                identity,
            )),
            events: events as Arc<dyn ProfileEvents>,
            reviews,
        },
        PolicyEnforcer::new(resolver),
        check,
    )
}

/// A profile read with nothing stored, for tests of other routes.
pub fn idle_profile(
    db: &Db,
    resolver: Arc<dyn AuthZResolverApi>,
) -> Arc<crate::domain::profile_read::ProfileReadService> {
    Arc::new(profile_service(
        db,
        resolver,
        Vec::new(),
        Arc::default(),
        crate::domain::profile_read::CategoryCheck::OneCall,
    ))
}

// --- Graph storage ---

/// Admits every call, scoped to the caller's own tenant, like a real PDP.
pub struct AllowInOwnTenant;

#[async_trait]
impl AuthZResolverApi for AllowInOwnTenant {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .unwrap_or_else(Uuid::nil);
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tenant],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

/// Graph storage's real service over its in-memory store, with a policy
/// service that admits every call in the caller's tenant.
pub fn fake_graph_storage() -> Arc<dyn graph_storage_sdk::GraphStorageClientV1> {
    use graph_storage::config::GraphStorageConfig;
    use graph_storage::domain::embedding::{EmbeddingCoordinator, SpaceState};
    use graph_storage::domain::local_client::GraphStorageLocalClient;
    use graph_storage::domain::service::GraphServices;
    use graph_storage::infra::embedding::fake::FakeEmbeddingProvider;
    use graph_storage::infra::fake_store::{FakeGraphEngine, FakeGraphStore};

    let store = Arc::new(FakeGraphStore::new());
    let engine = Arc::new(FakeGraphEngine::new(Arc::clone(&store)));
    let embedding = EmbeddingCoordinator::new(
        Arc::new(FakeEmbeddingProvider::new(384)),
        SpaceState::Active { epoch: 1 },
        8 * 1024,
    );
    let services = Arc::new(GraphServices::new(
        GraphStorageConfig::default()
            .validated()
            .expect("the default configuration is valid"),
        store,
        engine,
        PolicyEnforcer::new(Arc::new(AllowInOwnTenant)),
        embedding,
    ));
    Arc::new(GraphStorageLocalClient::new(services))
}

/// Processing that runs without sensitive-data checks, as a deployment that
/// opted in.
pub fn unchecked_processing() -> crate::config::ProcessingConfig {
    crate::config::ProcessingConfig {
        run_without_sensitive_data_checks: true,
        ..crate::config::ProcessingConfig::default()
    }
}

/// Every header the security layer puts on Construct's answers, with its
/// value.
pub const SECURITY_HEADERS: [(&str, &str); 9] = [
    ("cache-control", "no-store"),
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
    ("referrer-policy", "no-referrer"),
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), interest-cohort=()",
    ),
    ("cross-origin-resource-policy", "same-origin"),
    ("x-permitted-cross-domain-policies", "none"),
    ("x-dns-prefetch-control", "off"),
];

// --- Subject control ---

/// A stored fact of `category` with one property, stored at `stored_at`
/// seconds after the epoch.
pub fn stored_fact(
    category: crate::domain::profile::Category,
    property: &str,
    value: serde_json::Value,
    stored_at: i64,
) -> crate::domain::profile_read::StoredFact {
    crate::domain::profile_read::StoredFact {
        node_key: NodeKey(format!("construct:t:{}", Uuid::new_v4())),
        category,
        property: property.to_owned(),
        value,
        origin: crate::domain::profile_read::OriginKind::Connector,
        stored_at: time::OffsetDateTime::from_unix_timestamp(stored_at).unwrap(),
        produced_at: None,
    }
}

/// One subject's profile in memory: its version and live facts. It keeps
/// each read: the subject of the context it was read with, and the key.
/// Each read takes `delay` first, and fails as larger than one read takes
/// while `too_large` is set.
#[derive(Default)]
pub struct MemoryProfile {
    pub version: std::sync::Mutex<Option<u64>>,
    pub facts: std::sync::Mutex<Vec<crate::domain::profile_read::StoredFact>>,
    pub reads: std::sync::Mutex<Vec<(Uuid, SubjectKey)>>,
    pub delay: std::sync::Mutex<std::time::Duration>,
    pub too_large: std::sync::atomic::AtomicBool,
}

impl MemoryProfile {
    pub fn with(facts: Vec<crate::domain::profile_read::StoredFact>) -> Arc<Self> {
        Arc::new(Self {
            version: std::sync::Mutex::new(Some(1)),
            facts: std::sync::Mutex::new(facts),
            reads: std::sync::Mutex::default(),
            delay: std::sync::Mutex::default(),
            too_large: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn live(&self) -> Vec<crate::domain::profile_read::StoredFact> {
        self.facts.lock().unwrap().clone()
    }
}

#[async_trait]
impl crate::domain::profile_writes::SubjectProfiles for MemoryProfile {
    async fn facts(
        &self,
        ctx: &SecurityContext,
        key: SubjectKey,
    ) -> Result<crate::domain::profile_writes::SubjectFacts, DomainError> {
        self.reads.lock().unwrap().push((ctx.subject_id(), key));
        let delay = *self.delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if self.too_large.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(DomainError::ProfileFull(
                crate::domain::profile::FullProfile {
                    subject: key,
                    facts: crate::domain::profile::FactCount::AtLeast(1000),
                    limit: 999,
                },
            ));
        }
        Ok(crate::domain::profile_writes::SubjectFacts {
            version: *self.version.lock().unwrap(),
            facts: self.live(),
        })
    }
}

/// A plan writer over a [`MemoryProfile`]: it answers a conflict for the
/// first `conflicts` writes, each time as if someone else had written, and
/// then removes the plan's facts and moves the version on. It keeps every
/// plan and the subject of the context it was written with.
pub struct MemoryWriter {
    profile: Arc<MemoryProfile>,
    conflicts: std::sync::Mutex<u32>,
    /// Whether the write that answers a conflict also removed the plan's
    /// facts, as another writer removing the same fact would.
    racing: bool,
    pub plans: std::sync::Mutex<Vec<(Uuid, Plan)>>,
}

impl MemoryWriter {
    pub fn new(profile: Arc<MemoryProfile>, conflicts: u32) -> Arc<Self> {
        Arc::new(Self {
            profile,
            conflicts: std::sync::Mutex::new(conflicts),
            racing: false,
            plans: std::sync::Mutex::default(),
        })
    }

    /// A writer whose first write conflicts because another writer removed
    /// the plan's facts just before.
    pub fn racing(profile: Arc<MemoryProfile>) -> Arc<Self> {
        Arc::new(Self {
            profile,
            conflicts: std::sync::Mutex::new(1),
            racing: true,
            plans: std::sync::Mutex::default(),
        })
    }

    /// Apply the plan's removals, and its entity replaces as a reviewer's
    /// correction makes them.
    fn remove(&self, plan: &Plan) {
        let removed: Vec<&NodeKey> = plan
            .steps
            .iter()
            .filter_map(|step| match &step.change {
                Change::RemoveEntity { entity } | Change::ReplaceEntity { old: entity, .. } => {
                    Some(entity)
                }
                _ => None,
            })
            .collect();
        let mut facts = self.profile.facts.lock().unwrap();
        let replaced: Vec<crate::domain::profile_read::StoredFact> = plan
            .steps
            .iter()
            .filter_map(|step| match &step.change {
                Change::ReplaceEntity {
                    old,
                    key,
                    properties,
                } => {
                    let category = facts.iter().find(|fact| &fact.node_key == old)?.category;
                    let property = properties.first()?;
                    Some(crate::domain::profile_read::StoredFact {
                        node_key: key.clone(),
                        category,
                        property: property.name.clone(),
                        value: property.value.clone(),
                        origin: crate::domain::profile_read::OriginKind::Reviewer,
                        stored_at: time::OffsetDateTime::now_utc(),
                        produced_at: None,
                    })
                }
                _ => None,
            })
            .collect();
        facts.retain(|fact| !removed.contains(&&fact.node_key));
        facts.extend(replaced);
    }

    pub fn plans(&self) -> Vec<Plan> {
        self.plans
            .lock()
            .unwrap()
            .iter()
            .map(|(_, plan)| plan.clone())
            .collect()
    }
}

#[async_trait]
impl crate::domain::planner::processing::PlanWriter for MemoryWriter {
    async fn write(
        &self,
        ctx: &SecurityContext,
        plan: &Plan,
    ) -> Result<crate::domain::planner::processing::Written, DomainError> {
        use crate::domain::planner::processing::Written;
        self.plans
            .lock()
            .unwrap()
            .push((ctx.subject_id(), plan.clone()));
        let mut version = self.profile.version.lock().unwrap();
        if *version != plan.profile_version {
            return Ok(Written::Conflict);
        }
        let mut conflicts = self.conflicts.lock().unwrap();
        *version = Some(version.unwrap_or(0) + 1);
        if *conflicts > 0 {
            *conflicts -= 1;
            if self.racing {
                self.remove(plan);
            }
            return Ok(Written::Conflict);
        }
        self.remove(plan);
        Ok(Written::Stored)
    }
}

/// Keeps every Subject Control event.
#[derive(Default)]
pub struct RecordingSubjectControlEvents {
    pub events: std::sync::Mutex<Vec<crate::domain::subject_control::SubjectControlEvent>>,
}

impl RecordingSubjectControlEvents {
    pub fn taken(&self) -> Vec<crate::domain::subject_control::SubjectControlEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl crate::domain::subject_control::SubjectControlEvents for RecordingSubjectControlEvents {
    fn record(&self, event: crate::domain::subject_control::SubjectControlEvent) {
        self.events.lock().unwrap().push(event);
    }
}

/// The profile writes over `profile` and `writer`, read and written as the
/// processing identity `identity`, with at most two reruns.
pub fn profile_writes(
    profile: Arc<MemoryProfile>,
    writer: Arc<MemoryWriter>,
    identity: Option<Uuid>,
) -> crate::domain::profile_writes::ProfileWrites {
    let contexts: Arc<dyn crate::domain::planner::processing::ProcessingContexts> = match identity {
        Some(identity) => Arc::new(crate::infra::processing_contexts::ServiceIdentity::new(
            identity,
        )),
        None => Arc::new(NoProcessingIdentity),
    };
    crate::domain::profile_writes::ProfileWrites::new(profile, writer, contexts, 2)
}

/// Subject Control over `profile` and `writer`, read and written as the
/// processing identity `identity`, with the settings `settings`, the policy
/// service `resolver`, and at most two reruns.
pub fn subject_control(
    resolver: Arc<dyn AuthZResolverApi>,
    profile: Arc<MemoryProfile>,
    writer: Arc<MemoryWriter>,
    settings: Arc<dyn SubjectSettingsReader>,
    events: Arc<RecordingSubjectControlEvents>,
    identity: Option<Uuid>,
) -> crate::domain::subject_control::SubjectControlService {
    subject_control_reviewed(
        resolver,
        Arc::new(profile_writes(profile, writer, identity)),
        settings,
        events,
        Arc::default(),
        Arc::default(),
    )
}

/// Subject Control over `writes`, with the review requests `reviews` and
/// their events.
pub fn subject_control_reviewed(
    resolver: Arc<dyn AuthZResolverApi>,
    writes: Arc<crate::domain::profile_writes::ProfileWrites>,
    settings: Arc<dyn SubjectSettingsReader>,
    events: Arc<RecordingSubjectControlEvents>,
    reviews: Arc<MemoryReviews>,
    review_events: Arc<RecordingReviewEvents>,
) -> crate::domain::subject_control::SubjectControlService {
    use crate::domain::subject_control::{SubjectControlPorts, SubjectControlService};
    SubjectControlService::new(
        SubjectControlPorts {
            writes,
            settings,
            events,
            reviews,
            review_events,
        },
        PolicyEnforcer::new(resolver),
    )
}

/// No processing identity: every tenant is not configured.
pub struct NoProcessingIdentity;

#[async_trait]
impl crate::domain::planner::processing::ProcessingContexts for NoProcessingIdentity {
    async fn for_tenant(
        &self,
        _tenant_id: Uuid,
    ) -> Result<(SecurityContext, AccessScope), DomainError> {
        Err(DomainError::NotConfigured(
            "no processing identity is configured".to_owned(),
        ))
    }
}

// --- Review requests ---

/// One stored request with the claim that holds it, if any: the claim's id
/// and when it was taken.
pub type HeldRequest = (
    crate::domain::review_queue::ReviewRequest,
    Option<(Uuid, time::OffsetDateTime)>,
);

/// [`ReviewRequests`](crate::domain::review_queue::ReviewRequests) in memory,
/// newest first, with each request's claim. Pages hold every match; paging is
/// the store's own test. The scope is not applied; the store's tests show it.
#[derive(Default)]
pub struct MemoryReviews {
    pub requests: std::sync::Mutex<Vec<HeldRequest>>,
    /// Fail every call as unavailable.
    pub down: std::sync::atomic::AtomicBool,
    /// Close each request as deleted by its subject right after its claim
    /// recorded a write, as the subject's delete would while a reviewer's
    /// write runs.
    pub deleted_while_written: std::sync::atomic::AtomicBool,
    /// Fail a reviewer's close as unavailable, and nothing else.
    pub close_fails: std::sync::atomic::AtomicBool,
    /// Close each request as deleted by its subject right before a claim
    /// records a write, so the record finds it closed.
    pub deleted_before_pend: std::sync::atomic::AtomicBool,
    /// The scope of each read of the facts under review.
    pub under_review_scopes: std::sync::Mutex<Vec<AccessScope>>,
    /// What each request's claim recorded, by request id.
    pub pending:
        std::sync::Mutex<std::collections::HashMap<Uuid, crate::domain::review_queue::Closed>>,
}

impl MemoryReviews {
    pub fn all(&self) -> Vec<crate::domain::review_queue::ReviewRequest> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(request, _)| request.clone())
            .collect()
    }

    /// Whether each request is held by a claim, in the order of [`Self::all`].
    pub fn held(&self) -> Vec<bool> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, claim)| claim.is_some())
            .collect()
    }

    /// Hold the first request by another resolution, claimed at `at`.
    pub fn hold_first(&self, at: time::OffsetDateTime) {
        self.requests.lock().unwrap()[0].1 = Some((Uuid::new_v4(), at));
    }

    /// Move the claim on the first request back to `at`, as if it had been
    /// taken then.
    pub fn claimed_first_at(&self, at: time::OffsetDateTime) {
        if let Some((_, since)) = &mut self.requests.lock().unwrap()[0].1 {
            *since = at;
        }
    }

    /// What the claim on `request_id` recorded.
    pub fn pending_of(&self, request_id: Uuid) -> Option<crate::domain::review_queue::Closed> {
        self.pending.lock().unwrap().get(&request_id).cloned()
    }

    /// An open request on `fact`, created `seconds` after the epoch, as if
    /// the subject had flagged it.
    pub fn open_on_at(
        &self,
        key: SubjectKey,
        fact: &crate::domain::profile_read::StoredFact,
        seconds: i64,
    ) -> crate::domain::review_queue::ReviewRequest {
        use crate::domain::review_queue::{ReviewRequest, ReviewStatus};
        let request = ReviewRequest {
            id: Uuid::new_v4(),
            tenant_id: key.tenant_id,
            subject_id: key.subject_id,
            fact_id: fact.node_key.clone(),
            category: fact.category,
            property: fact.property.clone(),
            comment: None,
            created_at: time::OffsetDateTime::from_unix_timestamp(seconds).unwrap(),
            status: ReviewStatus::Open {
                value: fact.value.clone(),
            },
        };
        self.requests
            .lock()
            .unwrap()
            .insert(0, (request.clone(), None));
        request
    }

    /// An open request on `fact`, as if the subject had flagged it.
    pub fn open_on(&self, key: SubjectKey, fact: &crate::domain::profile_read::StoredFact) {
        self.open_on_at(key, fact, 1_000);
    }

    fn check(&self) -> Result<(), DomainError> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            Err(DomainError::Unavailable("the database is down".to_owned()))
        } else {
            Ok(())
        }
    }

    fn close_where(
        &self,
        matches: impl Fn(
            &crate::domain::review_queue::ReviewRequest,
            Option<(Uuid, time::OffsetDateTime)>,
        ) -> bool,
        closed: &crate::domain::review_queue::Closed,
    ) -> Option<crate::domain::review_queue::ReviewRequest> {
        use crate::domain::review_queue::ReviewStatus;
        let mut requests = self.requests.lock().unwrap();
        let (request, claim) = requests
            .iter_mut()
            .find(|(r, claim)| r.is_open() && matches(r, *claim))?;
        request.status = ReviewStatus::Closed(closed.clone());
        *claim = None;
        self.pending.lock().unwrap().remove(&request.id);
        Some(request.clone())
    }

    fn page(
        items: Vec<crate::domain::review_queue::ReviewRequest>,
    ) -> toolkit_odata::Page<crate::domain::review_queue::ReviewRequest> {
        let limit = items.len() as u64;
        toolkit_odata::Page {
            items,
            page_info: toolkit_odata::PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit,
            },
        }
    }
}

#[async_trait]
impl crate::domain::review_queue::FactsUnderReview for MemoryReviews {
    async fn under_review(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<std::collections::BTreeSet<crate::domain::plan::NodeKey>, DomainError> {
        self.under_review_scopes.lock().unwrap().push(scope.clone());
        self.check()?;
        Ok(self
            .all()
            .into_iter()
            .filter(|r| r.subject() == key && r.is_open())
            .map(|r| r.fact_id)
            .collect())
    }
}

#[async_trait]
impl crate::domain::review_queue::ReviewRequests for MemoryReviews {
    async fn open(
        &self,
        _scope: &AccessScope,
        new: crate::domain::review_queue::NewReviewRequest,
    ) -> Result<crate::domain::review_queue::Opened, DomainError> {
        use crate::domain::review_queue::{Opened, ReviewRequest, ReviewStatus};
        self.check()?;
        let mut requests = self.requests.lock().unwrap();
        if requests.iter().any(|(r, _)| {
            r.tenant_id == new.key.tenant_id && r.fact_id == new.fact_id && r.is_open()
        }) {
            return Ok(Opened::AlreadyOpen);
        }
        let request = ReviewRequest {
            id: new.id,
            tenant_id: new.key.tenant_id,
            subject_id: new.key.subject_id,
            fact_id: new.fact_id,
            category: new.category,
            property: new.property,
            comment: new.comment,
            created_at: new.created_at,
            status: ReviewStatus::Open { value: new.value },
        };
        requests.insert(0, (request.clone(), None));
        Ok(Opened::Opened(Box::new(request)))
    }

    async fn find(
        &self,
        _scope: &AccessScope,
        key: crate::domain::review_queue::RequestKey,
    ) -> Result<Option<crate::domain::review_queue::ReviewRequest>, DomainError> {
        self.check()?;
        Ok(self.all().into_iter().find(|r| r.key() == key))
    }

    async fn claim(
        &self,
        _scope: &AccessScope,
        key: crate::domain::review_queue::RequestKey,
        claim: crate::domain::review_queue::ClaimId,
        at: time::OffsetDateTime,
        hold: std::time::Duration,
    ) -> Result<crate::domain::review_queue::Claimed, DomainError> {
        use crate::domain::review_queue::Claimed;
        self.check()?;
        let mut requests = self.requests.lock().unwrap();
        let Some((request, holder)) = requests.iter_mut().find(|(r, _)| r.key() == key) else {
            return Ok(Claimed::Closed);
        };
        if !request.is_open() {
            return Ok(Claimed::Closed);
        }
        if holder.is_some_and(|(_, since)| since >= at - hold) {
            return Ok(Claimed::Held);
        }
        *holder = Some((claim.0, at));
        let claimed = request.clone();
        let pending = self.pending_of(request.id);
        Ok(Claimed::Taken {
            request: Box::new(claimed),
            pending,
        })
    }

    async fn pend(
        &self,
        _scope: &AccessScope,
        key: crate::domain::review_queue::RequestKey,
        claim: crate::domain::review_queue::ClaimId,
        pending: &crate::domain::review_queue::Closed,
    ) -> Result<bool, DomainError> {
        self.check()?;
        if self
            .deleted_before_pend
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            let subject = |r: &crate::domain::review_queue::ReviewRequest| r.subject_id;
            let by = self.all().iter().find(|r| r.key() == key).map(subject);
            self.close_where(
                |r, _| r.key() == key,
                &crate::domain::review_queue::Closed {
                    outcome: crate::domain::review_queue::ReviewOutcome::Deleted,
                    by: by.unwrap_or_default(),
                    at: time::OffsetDateTime::now_utc(),
                    reason: None,
                },
            );
        }
        let held = self.requests.lock().unwrap().iter().any(|(request, held)| {
            request.key() == key && request.is_open() && held.is_some_and(|(id, _)| id == claim.0)
        });
        if held {
            self.pending.lock().unwrap().insert(key.id, pending.clone());
        }
        if held
            && self
                .deleted_while_written
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            let subject = |r: &crate::domain::review_queue::ReviewRequest| r.subject_id;
            let by = self.all().iter().find(|r| r.key() == key).map(subject);
            self.close_where(
                |r, _| r.key() == key,
                &crate::domain::review_queue::Closed {
                    outcome: crate::domain::review_queue::ReviewOutcome::Deleted,
                    by: by.unwrap_or_default(),
                    at: time::OffsetDateTime::now_utc(),
                    reason: None,
                },
            );
        }
        Ok(held)
    }

    async fn release(
        &self,
        _scope: &AccessScope,
        key: crate::domain::review_queue::RequestKey,
        claim: crate::domain::review_queue::ClaimId,
    ) -> Result<(), DomainError> {
        self.check()?;
        for (request, held) in self.requests.lock().unwrap().iter_mut() {
            if request.key() == key && held.is_some_and(|(id, _)| id == claim.0) {
                *held = None;
                self.pending.lock().unwrap().remove(&key.id);
            }
        }
        Ok(())
    }

    async fn close(
        &self,
        _scope: &AccessScope,
        key: crate::domain::review_queue::RequestKey,
        claim: crate::domain::review_queue::ClaimId,
        closed: &crate::domain::review_queue::Closed,
    ) -> Result<Option<crate::domain::review_queue::ReviewRequest>, DomainError> {
        self.check()?;
        if self.close_fails.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(DomainError::Unavailable("the close failed".to_owned()));
        }
        Ok(self.close_where(
            |r, held| r.key() == key && held.is_some_and(|(id, _)| id == claim.0),
            closed,
        ))
    }

    async fn open_request_on(
        &self,
        _scope: &AccessScope,
        key: SubjectKey,
        fact_id: &crate::domain::plan::NodeKey,
    ) -> Result<
        Option<(
            crate::domain::review_queue::ReviewRequest,
            Option<crate::domain::review_queue::Closed>,
        )>,
        DomainError,
    > {
        self.check()?;
        let Some(request) = self
            .all()
            .into_iter()
            .find(|r| r.is_open() && r.subject() == key && &r.fact_id == fact_id)
        else {
            return Ok(None);
        };
        let recorded = self.pending.lock().unwrap().get(&request.id).cloned();
        Ok(Some((request, recorded)))
    }

    async fn close_open(
        &self,
        _scope: &AccessScope,
        key: crate::domain::review_queue::RequestKey,
        recorded: Option<&crate::domain::review_queue::Closed>,
        closed: &crate::domain::review_queue::Closed,
    ) -> Result<Option<crate::domain::review_queue::ReviewRequest>, DomainError> {
        self.check()?;
        let now = self.pending.lock().unwrap().get(&key.id).cloned();
        if now.as_ref() != recorded {
            return Ok(None);
        }
        Ok(self.close_where(|r, _| r.key() == key, closed))
    }

    async fn latest_on(
        &self,
        _scope: &AccessScope,
        key: SubjectKey,
        fact_ids: &[crate::domain::plan::NodeKey],
    ) -> Result<Vec<crate::domain::review_queue::ReviewRequest>, DomainError> {
        self.check()?;
        let mut requests: Vec<_> = self
            .all()
            .into_iter()
            .filter(|r| r.subject() == key && fact_ids.contains(&r.fact_id))
            .collect();
        requests.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        let mut seen = std::collections::BTreeSet::new();
        requests.retain(|r| seen.insert(r.fact_id.clone()));
        Ok(requests)
    }

    async fn of_subject(
        &self,
        _scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<Vec<crate::domain::review_queue::ReviewRequest>, DomainError> {
        self.check()?;
        Ok(self
            .all()
            .into_iter()
            .filter(|r| r.subject() == key)
            .collect())
    }

    async fn subject_page(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        _query: &toolkit_odata::ODataQuery,
    ) -> Result<toolkit_odata::Page<crate::domain::review_queue::ReviewRequest>, DomainError> {
        Ok(Self::page(
            crate::domain::review_queue::ReviewRequests::of_subject(self, scope, key).await?,
        ))
    }

    async fn open_page(
        &self,
        _scope: &AccessScope,
        tenant_id: Uuid,
        _query: &toolkit_odata::ODataQuery,
    ) -> Result<toolkit_odata::Page<crate::domain::review_queue::ReviewRequest>, DomainError> {
        self.check()?;
        let mut open: Vec<_> = self
            .all()
            .into_iter()
            .filter(|r| r.tenant_id == tenant_id && r.is_open())
            .collect();
        open.reverse();
        Ok(Self::page(open))
    }
}

/// Keeps every Review Queue event.
#[derive(Default)]
pub struct RecordingReviewEvents {
    pub events: std::sync::Mutex<Vec<crate::domain::review_queue::ReviewQueueEvent>>,
}

impl RecordingReviewEvents {
    pub fn taken(&self) -> Vec<crate::domain::review_queue::ReviewQueueEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl crate::domain::review_queue::ReviewQueueEvents for RecordingReviewEvents {
    fn record(&self, event: crate::domain::review_queue::ReviewQueueEvent) {
        self.events.lock().unwrap().push(event);
    }
}
