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
        let hand_off: Arc<dyn RecordHandOff> = if self.dropping {
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
