use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use construct_sdk::reason;
use serde_json::Value;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::DbProvider;
use super::error::DomainError;
use super::subject_settings::{SubjectSettingsRepository, SubjectSettingsService};

/// Authorization resource type for sending records, scoped by tenant. The
/// tenant is the one the connector names, not the connector's own.
pub(crate) const RECORD_RESOURCE: ResourceType =
    ResourceType::from_static("construct.record", &[pep_properties::OWNER_TENANT_ID]);

pub(crate) mod actions {
    pub const SEND: &str = "send";
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

/// Why intake refuses a record.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// The record breaks a rule of its type's schema, or of the base envelope.
    SchemaViolation,
    /// The record names a type the types registry does not know.
    UnknownType,
    /// The record's type does not derive from the record base type.
    NotARecordType,
    /// The record names an abstract type.
    AbstractType,
    /// The record carries an `id`, which a push must not.
    IdInPush,
    /// The connector is turned off for the tenant.
    ConnectorOff,
    /// Personalization is off for the record's subject.
    PersonalizationOff,
    /// An erasure of the record's subject is under way.
    ErasureInProgress,
}

impl RefusalReason {
    /// The reason code a refusal carries, from the SDK's `reason` module.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::SchemaViolation => reason::SCHEMA_VIOLATION,
            Self::UnknownType => reason::UNKNOWN_TYPE,
            Self::NotARecordType => reason::NOT_A_RECORD_TYPE,
            Self::AbstractType => reason::ABSTRACT_TYPE,
            Self::IdInPush => reason::ID_IN_PUSH,
            Self::ConnectorOff => reason::CONNECTOR_OFF,
            Self::PersonalizationOff => reason::PERSONALIZATION_OFF,
            Self::ErasureInProgress => reason::ERASURE_IN_PROGRESS,
        }
    }
}

/// Where in the record a refusal applies.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// The record as a whole.
    Whole,
    /// One member, as a JSON pointer such as `/payload/role`.
    Pointer(String),
}

impl Place {
    pub fn pointer(pointer: impl Into<String>) -> Self {
        Self::Pointer(pointer.into())
    }
}

/// The wire name of a place: the JSON pointer, or `(record)` for the record as
/// a whole.
impl std::fmt::Display for Place {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Whole => f.write_str("(record)"),
            Self::Pointer(pointer) => f.write_str(pointer),
        }
    }
}

/// A refused record. It names the record's type once the type is known to be
/// a well-formed type id, the place in the record, and the broken rule. It
/// never repeats a value from the record.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: RefusalReason,
    pub type_id: Option<String>,
    pub place: Place,
    pub rule: String,
}

impl Refusal {
    pub fn new(
        reason: RefusalReason,
        type_id: Option<&str>,
        place: Place,
        rule: impl Into<String>,
    ) -> Self {
        Self {
            reason,
            type_id: type_id.map(str::to_owned),
            place,
            rule: rule.into(),
        }
    }
}

impl From<Refusal> for DomainError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// The envelope fields intake reads from a record that passed its type.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub type_id: String,
    pub provenance: String,
    pub version: String,
    pub subject_id: Option<Uuid>,
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
        Ok(Self {
            type_id,
            provenance,
            version,
            subject_id,
        })
    }
}

/// The identity intake keeps of a received record: the tenant, the connector,
/// the record's provenance and version, and the subject it names. It holds no
/// content.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordId {
    pub tenant_id: Uuid,
    pub connector: String,
    pub provenance: String,
    pub version: String,
    pub subject_id: Option<Uuid>,
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
}

/// Why processing dropped a received record.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropCause {
    /// Processing could not finish.
    ProcessingFailed,
    RoundCap,
    TokenCap,
    ModelFailed,
}

/// A content-free event for a dropped record: who sent it, for which tenant
/// and subject, of which type, and why it was dropped. The Planner, the
/// Sensitive-Data Checks, Admission and the Profile Writer emit it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropEvent {
    pub tenant_id: Uuid,
    pub connector: String,
    pub type_id: String,
    pub subject_id: Option<Uuid>,
    pub cause: DropCause,
}

impl DropEvent {
    #[must_use]
    pub fn for_record(record: &ReceivedRecord, cause: DropCause) -> Self {
        Self {
            tenant_id: record.tenant_id,
            connector: record.connector.clone(),
            type_id: record.envelope.type_id.clone(),
            subject_id: record.envelope.subject_id,
            cause,
        }
    }
}

/// Checks a record against its GTS type and the types it derives from.
#[async_trait]
pub trait RecordTypes: Send + Sync {
    /// `Ok(())` when the record passes. A broken rule is
    /// [`DomainError::Refused`]; any other error means the check could not run.
    async fn check(&self, record: &Value) -> Result<(), DomainError>;
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
/// because its bounded queue is full or no processing is wired in; a refusal
/// is an error, and intake then keeps no identity of the record, so the
/// connector can send it again.
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
}

#[domain_model]
pub struct RecordIntakeService<I: RecordIdRepository + 'static, S: SubjectSettingsRepository> {
    db: Arc<DbProvider>,
    ids: Arc<I>,
    settings: Arc<SubjectSettingsService<S>>,
    ports: IntakePorts,
    policy_enforcer: PolicyEnforcer,
}

/// @cpt-dod:cpt-cf-construct-dod-record-intake-checks:p1
impl<I: RecordIdRepository + 'static, S: SubjectSettingsRepository> RecordIntakeService<I, S> {
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
        }
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
        self.check_tenant_and_subject(ctx, &scope, tenant_id, connector, &envelope)
            .await?;

        let received = ReceivedRecord {
            tenant_id,
            connector: connector.to_string(),
            envelope,
            record,
        };
        self.take(scope, received).await
    }

    /// Insert the record's identity and hand the record off, in one
    /// transaction: a repeat changes nothing, and a hand-off that refuses the
    /// record rolls the identity back.
    async fn take(
        &self,
        scope: AccessScope,
        received: ReceivedRecord,
    ) -> Result<IntakeOutcome, DomainError> {
        let ids = Arc::clone(&self.ids);
        let hand_off = Arc::clone(&self.ports.hand_off);
        self.db
            .db()
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-insert
                    let id = RecordId {
                        tenant_id: received.tenant_id,
                        connector: received.connector.clone(),
                        provenance: received.envelope.provenance.clone(),
                        version: received.envelope.version.clone(),
                        subject_id: received.envelope.subject_id,
                    };
                    let inserted = ids.insert(tx, &scope, id).await?;
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
            .await
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
        self.ports.types.check(record).await?;
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-type

        // @cpt-begin:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-envelope
        Ok(Envelope::read(record)?)
        // @cpt-end:cpt-cf-construct-algo-record-intake-check-record:p1:inst-check-envelope
    }

    /// The tenant's and the subject's checks: the connector is on, and, for a
    /// record about a subject, personalization is on with no erasure under way.
    async fn check_tenant_and_subject(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        tenant_id: Uuid,
        connector: Uuid,
        envelope: &Envelope,
    ) -> Result<(), DomainError> {
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

        // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-subject
        let Some(subject_id) = envelope.subject_id else {
            return Ok(());
        };
        let settings = self
            .settings
            .settings_within(ctx, scope, tenant_id, subject_id)
            .await?;
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
        Ok(())
        // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-subject
    }
}
