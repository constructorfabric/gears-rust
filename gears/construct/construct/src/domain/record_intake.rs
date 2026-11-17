use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use construct_sdk::gts::{RecordTraits, check_type_rules};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::DbProvider;
use super::deletion::{ProfileGenerations, RetentionPeriods, period_start};
use super::error::DomainError;
use super::plan::{Origin, Plan};
pub use super::refusal::{Place, Refusal, RefusalReason};
use super::subject_settings::{SubjectKey, SubjectSettingsRepository, SubjectSettingsService};

/// The GTS type a policy names records by: the connector record base type,
/// which every record type derives from.
pub(crate) const RECORD_TYPE: &str = construct_sdk::gts::RECORD_BASE_TYPE;

/// Authorization resource type for sending records, scoped by tenant. The
/// tenant is the one the connector names, not the connector's own.
pub(crate) const RECORD_RESOURCE: ResourceType =
    ResourceType::from_static(RECORD_TYPE, &[pep_properties::OWNER_TENANT_ID]);

pub(crate) mod actions {
    pub const SEND: &str = "send";
}

/// The longest a record takes from the read of its subject's settings to the
/// insert of its identity, which the hand-off and the commit follow at once.
/// Past it the record is not taken, so a record checked before an erasure's
/// mark keeps its identity soon after this after the mark.
pub const RECEIVE_WINDOW: Duration = Duration::from_secs(10);

/// A record not taken within the receive window: nothing was kept.
fn not_taken() -> DomainError {
    DomainError::Unavailable(format!(
        "the record was not taken within {RECEIVE_WINDOW:?}; nothing is kept, send it again"
    ))
}

/// What intake answers for a record it takes. The crate owns this enum, so a
/// new outcome has to be handled everywhere it is matched; the SDK's
/// `RecordOutcome` is derived from it at the client boundary.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeOutcome {
    /// The record passed the checks and was handed to processing.
    Received,
    /// A record with the same identity was received before. Nothing changes.
    Repeat,
}

/// The envelope fields intake reads from a record that passed its type.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub type_id: String,
    pub provenance: String,
    pub version: String,
    pub subject_id: Option<Uuid>,
    /// When the connector formed the record, or observed it at the source.
    /// The profile writer stores it as the fact's `produced_at`.
    pub observed_at: OffsetDateTime,
}

impl Envelope {
    /// Read the envelope of a record that passed the base type's schema, which
    /// requires these members and fixes their shapes.
    fn read(record: &Value) -> Result<Self, Refusal> {
        let text = |member: &str| {
            record[member].as_str().map(str::to_owned).ok_or_else(|| {
                Refusal::new(
                    RefusalReason::SchemaViolation,
                    None,
                    Place::pointer(format!("/{member}")),
                    "required",
                )
            })
        };
        let type_id = text("type")?;
        let subject_id = match record.get("subject_id") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .and_then(|raw| Uuid::parse_str(raw).ok())
                    .ok_or_else(|| {
                        Refusal::new(
                            RefusalReason::SchemaViolation,
                            Some(&type_id),
                            Place::pointer("/subject_id"),
                            "format",
                        )
                    })?,
            ),
        };
        let provenance = text("provenance")?;
        let version = text("version")?;
        let observed_at = OffsetDateTime::parse(&text("observed_at")?, &Rfc3339).map_err(|_| {
            Refusal::new(
                RefusalReason::SchemaViolation,
                Some(&type_id),
                Place::pointer("/observed_at"),
                "format",
            )
        })?;
        Ok(Self {
            type_id,
            provenance,
            version,
            subject_id,
            observed_at,
        })
    }
}

/// The parts of a record identity, by name, so they cannot be swapped: the
/// connector and the record's provenance and version, as the record base type
/// states. They are only hashed into a [`RecordKey`]; nothing stores or logs
/// them, since a provenance can hold record content (a fact candidate's is the
/// fact key and its value).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityParts<'a> {
    pub connector: &'a str,
    pub provenance: &'a str,
    pub version: &'a str,
}

/// The key of a record identity: the SHA-256, in hex, of its connector,
/// provenance and version, each preceded by its length in bytes (8 bytes,
/// big-endian). The lengths make the encoding injective, so no two different
/// identities hash the same input, whatever bytes the parts hold, NUL included.
/// It is the only form of the identity intake stores or a drop event names.
/// The encoding must not change: the parts are not stored, so a stored key
/// cannot be computed again.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RecordKey(String);

impl RecordKey {
    /// The key of an identity.
    #[must_use]
    pub fn of(parts: IdentityParts<'_>) -> Self {
        let mut hash = toolkit_security::crypto::hash::Sha256::new();
        for part in [parts.connector, parts.provenance, parts.version] {
            let bytes = part.as_bytes();
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        Self(hex::encode(hash.finalize()))
    }

    /// The key of a received record's identity.
    #[must_use]
    pub fn of_record(record: &ReceivedRecord) -> Self {
        Self::of(IdentityParts {
            connector: &record.connector,
            provenance: &record.envelope.provenance,
            version: &record.envelope.version,
        })
    }

    /// The key in hex, 64 characters.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RecordKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What intake stores of a received record: its tenant, its [`RecordKey`],
/// the connector and the subject it names. It holds neither the provenance
/// nor the version, so no repository can store them.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordId {
    pub tenant_id: Uuid,
    pub key: RecordKey,
    pub connector: String,
    pub subject_id: Option<Uuid>,
}

impl RecordId {
    /// The identity intake stores for a received record.
    #[must_use]
    pub fn of_record(record: &ReceivedRecord) -> Self {
        Self {
            tenant_id: record.tenant_id,
            key: RecordKey::of_record(record),
            connector: record.connector.clone(),
            subject_id: record.envelope.subject_id,
        }
    }
}

/// What inserting a record identity found.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdInsert {
    /// The identity is new.
    Inserted,
    /// The identity was there already: the record is a repeat.
    AlreadyThere,
}

/// A received record on its way to processing. Its content lives only in
/// memory; nothing writes it to a store.
///
/// It carries no security context or scope: the connector's token can expire
/// before processing runs, so processing builds its own for the tenant. The
/// connector is kept for the plan's origin and the audit events.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ReceivedRecord {
    pub tenant_id: Uuid,
    pub connector: String,
    pub envelope: Envelope,
    pub record: Value,
    /// The subject's profile generation when the record was taken; 0 for a
    /// record without a subject. Processing drops the record once an erasure
    /// moved the generation on, so a record taken before an erasure is never
    /// stored after it, also once the subject turned personalization on
    /// again.
    pub generation: u64,
}

/// Where processing dropped a received record.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropStage {
    /// The planner, or the hand-off to it.
    Planner,
    /// The sensitive-data checks.
    Checks,
    /// Admission.
    Admission,
    /// The profile writer.
    Writer,
    /// The processing worker, outside any one stage: the record's processing
    /// panicked.
    Worker,
}

impl DropStage {
    /// The stage's name in the drop event, in logs and in metrics.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Checks => "checks",
            Self::Admission => "admission",
            Self::Writer => "writer",
            Self::Worker => "worker",
        }
    }
}

/// Why processing dropped a received record.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropReason {
    /// No planner is wired in yet.
    NoPlanner,
    /// Processing could not finish.
    ProcessingFailed,
    /// Planner: the last round still ended in tool calls.
    RoundCap,
    /// Planner: the model calls used more tokens than its cap.
    TokenCap,
    /// Planner: a model call failed, reported no usage, or answered in a form the planner cannot use.
    ModelFailed,
    /// Writer: the profile changed under every rerun the planner was allowed.
    RerunLimit,
    /// Processing: the planner does not decide on this record, because of its type or because it has no subject.
    NotPlanned,
    /// A sensitive-data check failed, or answered outside the plan.
    CheckFailed,
    /// Admission: personalization is off for the subject.
    PersonalizationOff,
    /// Admission: an erasure of the subject is under way.
    ErasureInProgress,
    /// Admission: the subject was erased after the record was taken.
    ErasedSinceTaken,
    /// Admission: the tenant does not trust the connector.
    UntrustedSource,
    /// Admission: a step is below the tenant's confidence floor.
    BelowConfidenceFloor,
    /// Admission: a step stores an entity kind the tenant does not allow.
    EntityKindNotAllowed,
    /// Admission: the plan is over the tenant's write caps.
    WriteCapExceeded,
}

impl DropReason {
    /// The reason's name in the drop event and in logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NoPlanner => "no_planner",
            Self::ProcessingFailed => "processing_failed",
            Self::RoundCap => "round_cap",
            Self::TokenCap => "token_cap",
            Self::ModelFailed => "model_failed",
            Self::CheckFailed => "check_failed",
            Self::PersonalizationOff => "personalization_off",
            Self::ErasureInProgress => "erasure_in_progress",
            Self::ErasedSinceTaken => "erased_since_taken",
            Self::UntrustedSource => "untrusted_source",
            Self::BelowConfidenceFloor => "below_confidence_floor",
            Self::EntityKindNotAllowed => "entity_kind_not_allowed",
            Self::WriteCapExceeded => "write_cap_exceeded",
            Self::RerunLimit => "rerun_limit",
            Self::NotPlanned => "not_planned",
        }
    }
}

/// A content-free event for a dropped record: the tenant, the subject, the
/// connector, the record key (never the provenance or the version, which can
/// hold content), its type, the stage that dropped it and why. It carries no
/// record content. Record intake
/// owns its shape; the Planner, the Sensitive-Data Checks, Admission and the
/// Profile Writer emit it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropEvent {
    pub tenant_id: Uuid,
    pub subject_id: Option<Uuid>,
    pub connector: String,
    pub record_key: RecordKey,
    pub type_id: String,
    pub stage: DropStage,
    pub reason: DropReason,
}

impl DropEvent {
    #[must_use]
    pub fn for_record(record: &ReceivedRecord, stage: DropStage, reason: DropReason) -> Self {
        Self {
            tenant_id: record.tenant_id,
            subject_id: record.envelope.subject_id,
            connector: record.connector.clone(),
            record_key: RecordKey::of_record(record),
            type_id: record.envelope.type_id.clone(),
            stage,
            reason,
        }
    }

    /// The drop event for a plan built from a connector's record. A plan with
    /// no record behind it (an agent's, a reviewer's or an administrator's)
    /// has none: its caller gets the reason in the result and audits it.
    #[must_use]
    pub fn for_plan(plan: &Plan, stage: DropStage, reason: DropReason) -> Option<Self> {
        let Origin::Connector {
            connector,
            provenance,
            version,
            type_id,
            ..
        } = &plan.origin
        else {
            return None;
        };
        Some(Self {
            tenant_id: plan.tenant_id,
            subject_id: Some(plan.subject_id),
            connector: connector.clone(),
            record_key: RecordKey::of(IdentityParts {
                connector,
                provenance,
                version,
            }),
            type_id: type_id.clone(),
            stage,
            reason,
        })
    }
}

/// One type's own value of the GTS trait `subject_required`, as it declares
/// it, before anything is inherited.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectTrait {
    /// The type does not declare it.
    Unset,
    On,
    Off,
    /// The type declares something that is not a boolean.
    Malformed,
}

/// What a record's type and the types it derives from state about the record
/// beyond its schema, the record's type first and the record base type last:
/// their type ids, and each type's own trait values.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeTraits {
    pub type_chain: Vec<String>,
    pub subject_chain: Vec<SubjectTrait>,
}

/// The rules a generated schema cannot carry, for the record's type and every
/// type it derives from (`construct_sdk::gts::check_type_rules`). A broken rule
/// is a refusal `SCHEMA_VIOLATION` at its place, naming the record's type.
pub(crate) fn check_type_rules_of(traits: &TypeTraits, record: &Value) -> Result<(), Refusal> {
    let record_type = traits.type_chain.first().map(String::as_str);
    for type_id in &traits.type_chain {
        check_type_rules(type_id, record).map_err(|broken| {
            Refusal::new(
                RefusalReason::SchemaViolation,
                record_type,
                Place::Pointer(broken.place),
                broken.rule.keyword(),
            )
        })?;
    }
    Ok(())
}

impl TypeTraits {
    /// Whether a record of the type must name its subject. A type derived from
    /// a type about a person stays about a person: any type below the base
    /// that turns the trait on requires a subject, whatever its descendants
    /// say. Otherwise the closest declared value wins, a malformed one
    /// requires a subject, and with none the SDK's default applies.
    #[must_use]
    pub fn subject_required(&self) -> bool {
        let below_base = self
            .subject_chain
            .split_last()
            .map_or(&[][..], |(_, below)| below);
        if below_base.contains(&SubjectTrait::On) {
            return true;
        }
        match self
            .subject_chain
            .iter()
            .find(|value| **value != SubjectTrait::Unset)
        {
            Some(SubjectTrait::Off) => false,
            Some(SubjectTrait::On | SubjectTrait::Malformed) => true,
            Some(SubjectTrait::Unset) | None => RecordTraits::default().subject_required,
        }
    }
}

/// Checks a record against its GTS type and the types it derives from.
#[async_trait]
pub trait RecordTypes: Send + Sync {
    /// The type's traits when the record passes its schema. A broken rule is
    /// [`DomainError::Refused`]; any other error means the check could not run.
    async fn check(&self, record: &Value) -> Result<TypeTraits, DomainError>;
}

/// Whether a connector is on for a tenant, a tenant setting. The connector is
/// the subject id of its login.
#[async_trait]
pub trait ConnectorSwitch: Send + Sync {
    async fn is_on(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        connector: Uuid,
    ) -> Result<bool, DomainError>;
}

#[async_trait]
pub trait RecordIdRepository: Send + Sync {
    /// Insert a record identity. The insert is the repeat check: an identity
    /// that is there already answers [`IdInsert::AlreadyThere`] and changes
    /// nothing. The scope must cover the identity's tenant.
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: RecordId,
    ) -> Result<IdInsert, DomainError>;
}

/// Hands a received record to the background processing, which the Planner
/// builds. It returns at once. It takes the record or refuses it, for example
/// because its bounded queue is full; a refusal is an error, and intake then
/// keeps no identity of the record, so the connector can send it again.
pub trait RecordHandOff: Send + Sync {
    /// # Errors
    ///
    /// [`DomainError::Unavailable`] when processing cannot take the record now.
    fn hand_off(&self, record: ReceivedRecord) -> Result<(), DomainError>;
}

/// Where content-free intake events go. The Audit gear takes them once it
/// ships.
pub trait IntakeEvents: Send + Sync {
    fn dropped(&self, event: DropEvent);
}

/// The collaborators of record intake.
#[domain_model]
pub struct IntakePorts {
    pub types: Arc<dyn RecordTypes>,
    pub switch: Arc<dyn ConnectorSwitch>,
    pub hand_off: Arc<dyn RecordHandOff>,
    /// The tenants' retention periods: a record observed before its
    /// tenant's period began is refused, so retention's removal of a record
    /// identity does not let it be stored again.
    pub retention: Arc<dyn RetentionPeriods>,
    /// The subjects' profile generations, which a taken record carries.
    pub generations: Arc<dyn ProfileGenerations>,
}

#[domain_model]
pub struct RecordIntakeService<
    I: RecordIdRepository + 'static,
    S: SubjectSettingsRepository + 'static,
> {
    db: Arc<DbProvider>,
    ids: Arc<I>,
    settings: Arc<SubjectSettingsService<S>>,
    ports: IntakePorts,
    policy_enforcer: PolicyEnforcer,
    /// [`RECEIVE_WINDOW`]; a test shortens it to run on the real clock.
    receive_window: Duration,
}

/// @cpt-dod:cpt-cf-construct-dod-record-intake-checks:p1
impl<I: RecordIdRepository + 'static, S: SubjectSettingsRepository + 'static>
    RecordIntakeService<I, S>
{
    pub fn new(
        db: Arc<DbProvider>,
        ids: Arc<I>,
        settings: Arc<SubjectSettingsService<S>>,
        ports: IntakePorts,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            db,
            ids,
            settings,
            ports,
            policy_enforcer,
            receive_window: RECEIVE_WINDOW,
        }
    }

    /// The same intake with a shorter receive window, so a test of the
    /// window runs on the real clock: a paused clock jumps to the deadline
    /// while the database works on its own thread.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_receive_window(mut self, window: Duration) -> Self {
        self.receive_window = window;
        self
    }

    /// Take one record a connector sends for `tenant_id`. Answers received or
    /// repeat; a refused record is [`DomainError::Refused`].
    ///
    /// Cancel-safe: the identity insert and the hand-off run in one
    /// transaction that commits only after processing took the record. A
    /// request dropped before the commit leaves no identity behind, so the
    /// connector can send the record again. A request dropped while the commit
    /// is under way can leave the record handed off without its identity; a
    /// resend is then processed twice, which the Profile Writer's idempotent
    /// write tolerates.
    #[tracing::instrument(skip_all, fields(%tenant_id, connector = %ctx.subject_id()))]
    pub async fn submit(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        record: Value,
    ) -> Result<IntakeOutcome, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-authorize
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &RECORD_RESOURCE,
                actions::SEND,
                None,
                &AccessRequest::new()
                    .context_tenant_id(tenant_id)
                    .tenant_mode(TenantMode::RootOnly)
                    .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?;
        // The connector is the caller the platform authenticated.
        let connector = ctx.subject_id();
        // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-authorize

        let envelope = self.check_record(&record).await?;
        // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-window
        // From the read of the subject's settings to the insert of the
        // identity, a record takes at most the receive window. The hand-off
        // and the commit follow at once, so an erasure that waits several
        // windows after its mark also finds this record's identity.
        let deadline = tokio::time::Instant::now() + self.receive_window;
        let generation = tokio::time::timeout_at(
            deadline,
            self.check_tenant_and_subject(ctx, &scope, tenant_id, connector, &envelope),
        )
        .await
        .map_err(|_| not_taken())??;
        let received = ReceivedRecord {
            tenant_id,
            connector: connector.to_string(),
            envelope,
            record,
            generation,
        };
        self.take(scope, received, deadline).await
        // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-window
    }

    /// Insert the record's identity and hand the record off, in one
    /// transaction: a repeat changes nothing, and a hand-off that refuses the
    /// record rolls the identity back. An insert that does not end by
    /// `deadline` rolls back too, before the hand-off, so nothing is kept.
    async fn take(
        &self,
        scope: AccessScope,
        received: ReceivedRecord,
        deadline: tokio::time::Instant,
    ) -> Result<IntakeOutcome, DomainError> {
        let ids = Arc::clone(&self.ids);
        let hand_off = Arc::clone(&self.ports.hand_off);
        let tenant_id = received.tenant_id;
        // Named in the late-commit warning, so the erasure to look at is known.
        let (subject_id, record_key) = (
            received.envelope.subject_id,
            RecordId::of_record(&received).key,
        );
        let taken = self
            .db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-insert
                    let id = RecordId::of_record(&received);
                    // Dropping the insert at the deadline is safe: it runs in
                    // this transaction, which then rolls back, so a half-done
                    // insert keeps nothing.
                    let inserted = tokio::time::timeout_at(deadline, ids.insert(tx, &scope, id))
                        .await
                        .map_err(|_| not_taken())??;
                    // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-insert
                    match inserted {
                        // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-repeat
                        IdInsert::AlreadyThere => Ok(IntakeOutcome::Repeat),
                        // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-repeat
                        // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-received
                        IdInsert::Inserted => {
                            hand_off.hand_off(received)?;
                            Ok(IntakeOutcome::Received)
                        } // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-received
                    }
                })
            })
            .await;
        warn_if_committed_late(deadline, &taken, tenant_id, subject_id, &record_key);
        taken
    }

    /// The record's own checks: no `id`, and its type and the types it derives
    /// from.
    async fn check_record(&self, record: &Value) -> Result<Envelope, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-id
        // The type is not named: nothing has checked it yet.
        if record.get("id").is_some() {
            return Err(Refusal::new(
                RefusalReason::IdInPush,
                None,
                Place::pointer("/id"),
                "a pushed record must not carry an id",
            )
            .into());
        }
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-id

        // @cpt-begin:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-type
        let traits = self.ports.types.check(record).await?;
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-type

        // @cpt-begin:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-type-rules
        check_type_rules_of(&traits, record)?;
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-type-rules

        let envelope = Envelope::read(record)?;

        // @cpt-begin:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-subject
        // A derived schema cannot require `subject_id`, so the type's trait
        // says it, and it is checked here, after the schema.
        if traits.subject_required() && envelope.subject_id.is_none() {
            return Err(Refusal::new(
                RefusalReason::SchemaViolation,
                Some(&envelope.type_id),
                Place::pointer("/subject_id"),
                "required",
            )
            .into());
        }
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-subject

        // @cpt-begin:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-envelope
        Ok(envelope)
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-envelope
    }

    /// The tenant's and the subject's checks: the connector is on, the record
    /// was observed within the tenant's retention period, and, for a record
    /// about a subject, personalization is on with no erasure under way. Gives
    /// the subject's profile generation, 0 for a record without a subject.
    // cancel-safe: it only reads the connector switch and the subject's
    // settings, so the receive window can drop it at any point.
    async fn check_tenant_and_subject(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant_id: Uuid,
        connector: Uuid,
        envelope: &Envelope,
    ) -> Result<u64, DomainError> {
        let refuse = |reason: RefusalReason, place: Place, rule: &str| {
            Err(Refusal::new(reason, Some(&envelope.type_id), place, rule).into())
        };

        // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-connector
        if !self.ports.switch.is_on(ctx, tenant_id, connector).await? {
            return refuse(
                RefusalReason::ConnectorOff,
                Place::Whole,
                "the connector is off for the tenant",
            );
        }
        // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-connector

        // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-retention
        // Retention removed what was observed before the period began, and the
        // record's identity with it; such a record taken again would be
        // stored afresh and kept another whole period.
        if let Some(period) = self.ports.retention.period(tenant_id).await?
            && period_start(OffsetDateTime::now_utc(), period)
                .is_some_and(|start| envelope.observed_at < start)
        {
            return refuse(
                RefusalReason::PastRetention,
                Place::pointer("/observed_at"),
                "the record was observed before the tenant's retention period began",
            );
        }
        // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-retention

        // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-subject
        let Some(subject_id) = envelope.subject_id else {
            return Ok(0);
        };
        let key = SubjectKey {
            tenant_id,
            subject_id,
        };
        let settings = self.settings.settings_within(ctx, scope, key).await?;
        if settings.erasure_in_progress {
            return refuse(
                RefusalReason::ErasureInProgress,
                Place::pointer("/subject_id"),
                "an erasure of the subject is under way",
            );
        }
        if !settings.personalization_enabled {
            return refuse(
                RefusalReason::PersonalizationOff,
                Place::pointer("/subject_id"),
                "personalization is off for the subject",
            );
        }
        // An erasure moves the generation on only after it waited out every
        // record taken before its mark, so this read, inside the receive
        // window, gives the generation the record was taken in.
        self.ports.generations.generation(key).await
        // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-subject
    }
}

/// Warn when a record was taken but its identity was committed after
/// `deadline`. The commit is not bounded, and one that ended past the
/// deadline leaves less of an erasure's wait than it counts on: an erasure
/// of the subject may have missed the identity. The subject and the record
/// are named, so the erasure to look at is known.
pub(crate) fn warn_if_committed_late(
    deadline: tokio::time::Instant,
    taken: &Result<IntakeOutcome, DomainError>,
    tenant_id: Uuid,
    subject_id: Option<Uuid>,
    record_key: &RecordKey,
) {
    let late = tokio::time::Instant::now().saturating_duration_since(deadline);
    if !late.is_zero() && matches!(taken, Ok(IntakeOutcome::Received)) {
        tracing::warn!(
            %tenant_id,
            subject_id = subject_id.map(|subject_id| subject_id.to_string()),
            %record_key,
            late_ms = u64::try_from(late.as_millis()).unwrap_or(u64::MAX),
            "construct: a record's identity was committed after its receive window; an \
             erasure of its subject may have missed it"
        );
    }
}
