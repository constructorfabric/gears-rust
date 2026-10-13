use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::models::TenantMode;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::DbProvider;
use super::error::DomainError;
use super::plan::{Origin, Plan};
pub use super::refusal::{Place, Refusal, RefusalReason};
use super::subject_settings::{SubjectKey, SubjectSettingsRepository, SubjectSettingsService};

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
}

/// Why processing dropped a received record.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropReason {
    /// No planner is wired in yet (until Story 5.2).
    NoPlanner,
    /// Processing could not finish.
    ProcessingFailed,
    /// Planner: the last round still ended in tool calls.
    RoundCap,
    /// Planner: the model calls used more tokens than its cap.
    TokenCap,
    /// Planner: a model call failed, reported no usage, or answered in a form the planner cannot use.
    ModelFailed,
    /// A sensitive-data check failed, or answered outside the plan.
    CheckFailed,
    /// Admission: personalization is off for the subject.
    PersonalizationOff,
    /// Admission: an erasure of the subject is under way.
    ErasureInProgress,
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
            Self::UntrustedSource => "untrusted_source",
            Self::BelowConfidenceFloor => "below_confidence_floor",
            Self::EntityKindNotAllowed => "entity_kind_not_allowed",
            Self::WriteCapExceeded => "write_cap_exceeded",
        }
    }
}

/// A content-free event for a dropped record: the tenant, the subject, the
/// connector, the record's identity (provenance and version), its type, the
/// stage that dropped it and why. It carries no record content. Record intake
/// owns its shape; the Planner, the Sensitive-Data Checks, Admission and the
/// Profile Writer emit it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropEvent {
    pub tenant_id: Uuid,
    pub subject_id: Option<Uuid>,
    pub connector: String,
    pub provenance: String,
    pub version: String,
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
            provenance: record.envelope.provenance.clone(),
            version: record.envelope.version.clone(),
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
            provenance: provenance.clone(),
            version: version.clone(),
            type_id: type_id.clone(),
            stage,
            reason,
        })
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
            .settings_within(
                ctx,
                scope,
                SubjectKey {
                    tenant_id,
                    subject_id,
                },
            )
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
