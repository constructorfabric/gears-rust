//! The Review Queue: the subject flags one of its facts as incorrect, which
//! opens a review request, and a reviewer of the tenant resolves it as
//! corrected, deleted or rejected. While a request is open its fact is not
//! served to applications and agents, and no record or agent changes it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::time::Instant;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, ScopeFilter, ScopeValue, SecurityContext, pep_properties};
use uuid::Uuid;

use super::admission::{Admission, AdmissionOutcome};
use super::error::DomainError;
use super::plan::{Change, NodeKey, Origin, Plan, Property, Step};
use super::planner::catalog::PropertyCatalog;
use super::planner::processing::Written;
use super::profile::Category;
use super::profile_writes::{
    Attempt, ProfileWrites, Removed, SubjectFacts, WRITE_BUDGET, WRITER_LIMIT, WriteError,
};
use super::record_intake::DropReason;
use super::sensitive_data::{Checked, SensitiveDataChecks};
use super::subject_access;
use super::subject_settings::SubjectKey;

/// GTS type id of a review request: the authorization resource type of the
/// reviewers' routes, and the resource type their errors name.
pub(crate) const REVIEW_REQUEST_TYPE: &str =
    toolkit_gts::gts_id!("cf.construct.review.request.v1~");

/// Authorization resource type for review requests, scoped by tenant: a
/// reviewer resolves the requests of a whole tenant, in every category, as
/// the tenant administrators and data protection officers it assigns read
/// every fact anyway.
pub(crate) const REVIEW_REQUEST_RESOURCE: ResourceType =
    ResourceType::from_static(REVIEW_REQUEST_TYPE, &[pep_properties::OWNER_TENANT_ID]);

pub(crate) mod actions {
    /// The subject flags one of its own facts; on the subject facts resource.
    pub const FLAG: &str = "flag";
    /// The subject reads its own requests; on the subject facts resource.
    pub use super::super::subject_control::actions::READ;
    /// A reviewer lists and resolves the tenant's review requests.
    pub const RESOLVE: &str = "resolve";
}

/// The longest comment a subject gives with a flag, and the longest reason a
/// reviewer gives with a resolution, in characters.
pub const MAX_TEXT_CHARS: usize = 1000;

/// How long a reviewer holds a request it is resolving: the budget before a
/// write, the longest the writer is awaited, and a margin for a statement
/// graph storage still runs after that. So a resolution under way is never
/// taken from its reviewer; a claim left behind by a reviewer that went away
/// frees itself.
pub const CLAIM_HOLD: Duration =
    Duration::from_secs(WRITE_BUDGET.as_secs() + WRITER_LIMIT.as_secs() + IN_FLIGHT.as_secs());

/// How long graph storage may still run a statement whose call the writer
/// gave up on.
const IN_FLIGHT: Duration = Duration::from_secs(30);

/// Why a text is not a [`ReviewText`].
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRule {
    Blank,
    TooLong,
}

impl TextRule {
    /// The rule as an answer states it.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Blank => "must not be blank",
            Self::TooLong => "at most 1000 characters",
        }
    }
}

/// A subject's comment or a reviewer's reason: trimmed, not blank, and at
/// most [`MAX_TEXT_CHARS`] characters.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewText(String);

impl ReviewText {
    /// # Errors
    ///
    /// [`TextRule`] for a blank text or one past the limit.
    pub fn new(text: impl Into<String>) -> Result<Self, TextRule> {
        let text = text.into();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(TextRule::Blank);
        }
        if trimmed.chars().count() > MAX_TEXT_CHARS {
            return Err(TextRule::TooLong);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// An optional text, where a blank one is none.
    ///
    /// # Errors
    ///
    /// [`TextRule::TooLong`] for a text past the limit.
    pub fn optional(text: Option<String>) -> Result<Option<Self>, TextRule> {
        match text.map(Self::new) {
            None | Some(Err(TextRule::Blank)) => Ok(None),
            Some(Ok(text)) => Ok(Some(text)),
            Some(Err(rule)) => Err(rule),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// One review request of a tenant.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestKey {
    pub tenant_id: Uuid,
    pub id: Uuid,
}

/// How a request was closed.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// A reviewer stored a new value in place of the fact, under this id.
    Corrected(NodeKey),
    /// The fact was deleted: by a reviewer, by the subject, or before a
    /// correction could be stored.
    Deleted,
    /// A reviewer kept the fact as it is.
    Rejected,
}

impl ReviewOutcome {
    /// The outcome's name on the wire, in the table and in events.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Corrected(_) => "corrected",
            Self::Deleted => "deleted",
            Self::Rejected => "rejected",
        }
    }

    /// The id of the fact a correction stored.
    #[must_use]
    pub fn corrected_fact_id(&self) -> Option<&NodeKey> {
        match self {
            Self::Corrected(key) => Some(key),
            Self::Deleted | Self::Rejected => None,
        }
    }

    /// The outcome [`Self::name`] names, with the corrected fact's id that a
    /// correction, and only a correction, carries. None for any other pair.
    #[must_use]
    pub fn from_parts(name: &str, corrected_fact_id: Option<NodeKey>) -> Option<Self> {
        match (name, corrected_fact_id) {
            ("corrected", Some(key)) => Some(Self::Corrected(key)),
            ("deleted", None) => Some(Self::Deleted),
            ("rejected", None) => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// One resolution's hold on a request: a fresh id for each resolution, so
/// two resolutions by the same reviewer do not share it.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimId(pub Uuid);

/// What a claim on a request did.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum Claimed {
    /// The open request is held by the claim. `pending` is what an earlier
    /// resolution was about to write when it ended without closing it.
    Taken {
        request: Box<ReviewRequest>,
        pending: Option<Closed>,
    },
    /// The request is closed, or not there.
    Closed,
    /// Another resolution holds it; the hold ends within [`CLAIM_HOLD`].
    Held,
}

/// Why a review request cannot be opened or resolved now.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewConflict {
    /// The fact already has an open request.
    AlreadyOpen(NodeKey),
    /// The request is closed.
    Closed(Uuid),
    /// Another resolution holds the request; a retry after it may succeed.
    Held(Uuid),
    /// The resolution was written, but another close, the subject's delete,
    /// closed the request meanwhile.
    ClosedAfterWrite {
        request_id: Uuid,
        outcome: ReviewOutcome,
    },
    /// The resolution was written, or may have been, but the request is not
    /// closed. It stays held until [`CLAIM_HOLD`] has passed; the next
    /// resolution then closes it as this one, when its write is there.
    NotClosed {
        request_id: Uuid,
        outcome: ReviewOutcome,
    },
}

impl ReviewConflict {
    /// The reason a caller tells the conflicts apart by.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::AlreadyOpen(_) => "ALREADY_OPEN",
            Self::Closed(_) => "CLOSED",
            Self::Held(_) => "BEING_RESOLVED",
            Self::ClosedAfterWrite { .. } => "WRITTEN_BUT_CLOSED",
            Self::NotClosed { .. } => "WRITTEN_NOT_CLOSED",
        }
    }
}

impl std::fmt::Display for ReviewConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyOpen(fact) => {
                write!(f, "fact {} already has an open review request", fact.0)
            }
            Self::Closed(id) => write!(f, "review request {id} is closed"),
            Self::Held(id) => write!(f, "review request {id} is being resolved; try again later"),
            Self::ClosedAfterWrite {
                request_id,
                outcome,
            } => {
                stored_as(f, *request_id, outcome)?;
                write!(f, ", but the subject's delete closed the request meanwhile")
            }
            Self::NotClosed {
                request_id,
                outcome,
            } => {
                stored_as(f, *request_id, outcome)?;
                write!(
                    f,
                    ", or may have been, but the request is not closed; resolve it again after \
                     {} seconds to close it",
                    CLAIM_HOLD.as_secs()
                )
            }
        }
    }
}

/// "the resolution of review request … was stored as …", with the new
/// fact's id of a correction.
fn stored_as(
    f: &mut std::fmt::Formatter<'_>,
    request_id: Uuid,
    outcome: &ReviewOutcome,
) -> std::fmt::Result {
    write!(
        f,
        "the resolution of review request {request_id} was stored as {}",
        outcome.name()
    )?;
    if let Some(key) = outcome.corrected_fact_id() {
        write!(f, " under fact {}", key.0)?;
    }
    Ok(())
}

/// A closed request's resolution: how, by whom, when and why.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closed {
    pub outcome: ReviewOutcome,
    /// The reviewer, or the subject that deleted the fact.
    pub by: Uuid,
    pub at: OffsetDateTime,
    pub reason: Option<ReviewText>,
}

/// Whether a request still waits for a reviewer.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewStatus {
    /// The fact is under review; `value` is the flagged value.
    Open { value: Value },
    /// The flagged value is not kept.
    Closed(Closed),
}

impl ReviewStatus {
    /// The state's name on the wire and in the table.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open",
            Self::Closed(_) => "closed",
        }
    }
}

/// One review request, open or closed.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewRequest {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub fact_id: NodeKey,
    pub category: Category,
    pub property: String,
    pub comment: Option<ReviewText>,
    pub created_at: OffsetDateTime,
    pub status: ReviewStatus,
}

impl ReviewRequest {
    #[must_use]
    pub fn key(&self) -> RequestKey {
        RequestKey {
            tenant_id: self.tenant_id,
            id: self.id,
        }
    }

    #[must_use]
    pub fn subject(&self) -> SubjectKey {
        SubjectKey {
            tenant_id: self.tenant_id,
            subject_id: self.subject_id,
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self.status, ReviewStatus::Open { .. })
    }
}

/// A request to open: the fact as the subject flagged it.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct NewReviewRequest {
    pub id: Uuid,
    pub key: SubjectKey,
    pub fact_id: NodeKey,
    pub category: Category,
    pub property: String,
    pub value: Value,
    pub comment: Option<ReviewText>,
    pub created_at: OffsetDateTime,
}

/// What opening a request did.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum Opened {
    Opened(Box<ReviewRequest>),
    /// The fact already has an open request; nothing was stored.
    AlreadyOpen,
}

/// The fields the subject's list of its requests filters and orders by.
/// Exists only to derive [`SubjectReviewField`].
#[domain_model]
#[derive(toolkit_odata_macros::ODataFilterable)]
pub struct SubjectReviewQuery {
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,
    #[odata(filter(kind = "String"))]
    pub fact_id: String,
    /// `open` or `closed`.
    #[odata(filter(kind = "String"))]
    pub state: String,
    /// `corrected`, `deleted` or `rejected`.
    #[odata(filter(kind = "String"))]
    pub outcome: String,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: OffsetDateTime,
}

pub use SubjectReviewQueryFilterField as SubjectReviewField;

/// The fields the reviewers' list of open requests filters and orders by.
/// Exists only to derive [`OpenReviewField`].
#[domain_model]
#[derive(toolkit_odata_macros::ODataFilterable)]
pub struct OpenReviewQuery {
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,
    #[odata(filter(kind = "Uuid"))]
    pub subject_id: Uuid,
    #[odata(filter(kind = "String"))]
    pub fact_id: String,
    /// `identity`, `role`, `skill` or `preference`.
    #[odata(filter(kind = "String"))]
    pub entity_kind: String,
    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: OffsetDateTime,
}

pub use OpenReviewQueryFilterField as OpenReviewField;

/// The facts under review: what the profile read hides and processing
/// leaves alone. A planner can ask it too, to leave such facts out of the
/// model's choices.
#[async_trait]
pub trait FactsUnderReview: Send + Sync {
    /// The subject's facts that have an open request, under `scope`.
    async fn under_review(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<BTreeSet<NodeKey>, DomainError>;
}

/// Construct's review requests. Every call runs under the caller's scope:
/// the platform's for the subject's and the reviewers' calls, Construct's own
/// for processing's.
#[async_trait]
pub trait ReviewRequests: FactsUnderReview {
    /// Store a new open request, unless the fact has one.
    async fn open(
        &self,
        scope: &AccessScope,
        request: NewReviewRequest,
    ) -> Result<Opened, DomainError>;

    async fn find(
        &self,
        scope: &AccessScope,
        key: RequestKey,
    ) -> Result<Option<ReviewRequest>, DomainError>;

    /// Claim the open request `key` as `claim`, at `at`, unless another
    /// claim holds it since less than `hold` ago. The claim and the read of
    /// the request it holds are one step.
    async fn claim(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
        at: OffsetDateTime,
        hold: Duration,
    ) -> Result<Claimed, DomainError>;

    /// Record on the request `key`, while `claim` holds it, what the
    /// resolution is about to write. Whether `claim` still holds it.
    async fn pend(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
        pending: &Closed,
    ) -> Result<bool, DomainError>;

    /// Give back `claim` on the request `key`, and forget what it recorded.
    async fn release(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
    ) -> Result<(), DomainError>;

    /// Close the request `key` while it is open and held by `claim`. None
    /// when it is not.
    async fn close(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        claim: ClaimId,
        closed: &Closed,
    ) -> Result<Option<ReviewRequest>, DomainError>;

    /// Close the open request on the subject's fact, claimed or not, if
    /// there is one.
    async fn close_open_on(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        fact_id: &NodeKey,
        closed: &Closed,
    ) -> Result<Option<ReviewRequest>, DomainError>;

    /// The latest request on each of `fact_ids` that has one.
    async fn latest_on(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        fact_ids: &[NodeKey],
    ) -> Result<Vec<ReviewRequest>, DomainError>;

    /// All the subject's requests, newest first.
    async fn of_subject(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
    ) -> Result<Vec<ReviewRequest>, DomainError>;

    /// The page of the subject's requests that `query` asks for.
    async fn subject_page(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError>;

    /// The page of the tenant's open requests that `query` asks for.
    async fn open_page(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError>;
}

/// What the Review Queue did, for the audit log. No event holds a value, a
/// comment or a reviewer's reason.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewQueueEvent {
    /// The subject flagged a fact.
    Requested {
        tenant_id: Uuid,
        subject_id: Uuid,
        request_id: Uuid,
        fact_id: NodeKey,
        category: Category,
    },
    /// A request was closed, by a reviewer or by the subject's delete.
    Resolved {
        tenant_id: Uuid,
        subject_id: Uuid,
        request_id: Uuid,
        fact_id: NodeKey,
        outcome: ReviewOutcome,
        by: Uuid,
    },
    /// The checks or Admission refused a reviewer's correction; the request
    /// stays open.
    CorrectionRefused {
        tenant_id: Uuid,
        subject_id: Uuid,
        request_id: Uuid,
        fact_id: NodeKey,
        reason: NotStored,
        by: Uuid,
    },
    /// A reviewer read a page of the tenant's open requests, with their
    /// flagged values.
    Listed {
        tenant_id: Uuid,
        by: Uuid,
        request_ids: Vec<Uuid>,
    },
}

impl ReviewQueueEvent {
    /// The resolved event of `request`, closed as `closed`.
    #[must_use]
    pub fn resolved(request: &ReviewRequest, closed: &Closed) -> Self {
        Self::Resolved {
            tenant_id: request.tenant_id,
            subject_id: request.subject_id,
            request_id: request.id,
            fact_id: request.fact_id.clone(),
            outcome: closed.outcome.clone(),
            by: closed.by,
        }
    }
}

/// Where the Review Queue's events go: the structured log until the Audit
/// gear ships.
pub trait ReviewQueueEvents: Send + Sync {
    fn record(&self, event: ReviewQueueEvent);
}

/// Close the open request on a fact the subject deleted, and record it.
/// Whether there was one.
///
/// # Errors
///
/// When the request store cannot be reached.
///
/// @cpt-dod:cpt-cf-construct-dod-review-queue-subject-control:p1
pub(crate) async fn close_on_delete(
    requests: &dyn ReviewRequests,
    events: &dyn ReviewQueueEvents,
    scope: &AccessScope,
    key: SubjectKey,
    fact_id: &NodeKey,
) -> Result<bool, DomainError> {
    let closed = Closed {
        outcome: ReviewOutcome::Deleted,
        by: key.subject_id,
        at: OffsetDateTime::now_utc(),
        reason: None,
    };
    let Some(request) = requests.close_open_on(scope, key, fact_id, &closed).await? else {
        return Ok(false);
    };
    events.record(ReviewQueueEvent::resolved(&request, &closed));
    Ok(true)
}

/// A reviewer's decision on an open request.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// Keep the fact as it is.
    Rejected { reason: ReviewText },
    /// Delete the fact.
    Deleted { reason: Option<ReviewText> },
    /// Store `value` for `property` in place of the fact.
    Corrected {
        property: String,
        value: Value,
        reason: Option<ReviewText>,
    },
}

/// Why a correction stored nothing. The request stays open.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotStored {
    /// The checks failed, or Admission dropped the plan.
    Dropped(DropReason),
    /// The checks blocked the value.
    Blocked,
}

impl NotStored {
    /// The reason's name in the answer.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dropped(reason) => reason.name(),
            Self::Blocked => "value_blocked",
        }
    }
}

/// A correction's plan, ready to write, or why there is none.
#[domain_model]
enum Prepared {
    Gone,
    NotStored(NotStored),
    Write(Box<Plan>, NodeKey),
}

/// What a correction did.
#[domain_model]
enum Corrected {
    Stored(NodeKey),
    /// The fact is gone; nothing was stored.
    Gone,
}

/// The stages a corrected value passes before it is written: the same checks
/// and Admission as any plan, and the person types' schemas. Built only where
/// the deployment opted in to storing values without sensitive-data checks,
/// as record processing is.
#[domain_model]
pub struct Corrections {
    pub checks: SensitiveDataChecks,
    pub admission: Admission,
    pub catalog: Arc<PropertyCatalog>,
}

/// @cpt-dod:cpt-cf-construct-dod-review-queue-subject:p1
#[domain_model]
pub struct ReviewQueueService {
    writes: Arc<ProfileWrites>,
    requests: Arc<dyn ReviewRequests>,
    events: Arc<dyn ReviewQueueEvents>,
    corrections: Option<Corrections>,
    policy_enforcer: PolicyEnforcer,
}

impl ReviewQueueService {
    #[must_use]
    pub fn new(
        writes: Arc<ProfileWrites>,
        requests: Arc<dyn ReviewRequests>,
        events: Arc<dyn ReviewQueueEvents>,
        corrections: Option<Corrections>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            writes,
            requests,
            events,
            corrections,
            policy_enforcer,
        }
    }

    /// Open a review request on one of the subject's live facts.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] when the caller is not the subject or the
    /// platform refuses; [`DomainError::NoSuchFact`] for a fact the subject
    /// does not have, also one removed while the request was opened;
    /// [`DomainError::ReviewConflict`] when the fact already has an open
    /// request; not configured without a processing identity.
    ///
    /// NOT cancel-safe: a drop after the request is stored and before the
    /// re-read leaves it open with its requested event, also when the fact
    /// went away meanwhile; such a request closes as deleted when the
    /// subject deletes the fact again or a reviewer resolves it. A re-read
    /// that fails leaves it the same way.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id))]
    pub async fn flag(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        fact_id: &NodeKey,
        comment: Option<ReviewText>,
    ) -> Result<ReviewRequest, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-read
        let (key, scope) =
            subject_access::own(&self.policy_enforcer, ctx, subject_id, actions::FLAG).await?;
        let fact = self
            .writes
            .facts(key)
            .await?
            .facts
            .into_iter()
            .find(|fact| &fact.node_key == fact_id)
            .ok_or_else(|| no_such_fact(fact_id))?;
        // @cpt-end:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-read
        // @cpt-begin:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-store
        let opened = self
            .requests
            .open(
                &scope,
                NewReviewRequest {
                    id: Uuid::new_v4(),
                    key,
                    fact_id: fact.node_key,
                    category: fact.category,
                    property: fact.property,
                    value: fact.value,
                    comment,
                    created_at: OffsetDateTime::now_utc(),
                },
            )
            .await?;
        let Opened::Opened(request) = opened else {
            return Err(DomainError::ReviewConflict(ReviewConflict::AlreadyOpen(
                fact_id.clone(),
            )));
        };
        // Recorded as soon as the request is stored, so every request in the
        // table has its event, whatever happens after.
        self.events.record(ReviewQueueEvent::Requested {
            tenant_id: request.tenant_id,
            subject_id,
            request_id: request.id,
            fact_id: request.fact_id.clone(),
            category: request.category,
        });
        // A record or a delete can remove the fact between the read and the
        // insert. A request on a fact that is gone is closed at once, so
        // nothing waits for a review of nothing. When the read fails, the
        // request is stored and answered: a resolution of a fact gone closes
        // it as deleted.
        let reread = match self.writes.facts(key).await {
            Ok(reread) => reread,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    request_id = %request.id,
                    "construct: a flagged fact could not be read again; the request stays open"
                );
                return Ok(*request);
            }
        };
        if !reread.facts.iter().any(|fact| &fact.node_key == fact_id) {
            close_on_delete(
                self.requests.as_ref(),
                self.events.as_ref(),
                &scope,
                key,
                fact_id,
            )
            .await?;
            return Err(no_such_fact(fact_id));
        }
        // @cpt-end:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-store
        // @cpt-begin:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-return
        Ok(*request)
        // @cpt-end:cpt-cf-construct-flow-review-queue-flag:p1:inst-flag-return
    }

    /// The page of the subject's own requests that `query` asks for.
    ///
    /// # Errors
    ///
    /// As [`Self::flag`] for the caller; [`DomainError::InvalidQuery`] for a
    /// filter, order or cursor the list does not take.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %subject_id))]
    pub async fn own_requests(
        &self,
        ctx: &SecurityContext,
        subject_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-review-queue-list-own:p1:inst-own-authorize
        let (key, scope) =
            subject_access::own(&self.policy_enforcer, ctx, subject_id, actions::READ).await?;
        // @cpt-end:cpt-cf-construct-flow-review-queue-list-own:p1:inst-own-authorize
        // @cpt-begin:cpt-cf-construct-flow-review-queue-list-own:p1:inst-own-read
        self.requests.subject_page(&scope, key, query).await
        // @cpt-end:cpt-cf-construct-flow-review-queue-list-own:p1:inst-own-read
    }

    /// The page of the caller's tenant's open requests that `query` asks
    /// for, oldest first by default.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] without the permission to resolve review
    /// requests; [`DomainError::InvalidQuery`] for a query the list does not
    /// take.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-review-queue-reviewer:p1
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), caller = %ctx.subject_id()))]
    pub async fn open_requests(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ReviewRequest>, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-review-queue-list-open:p1:inst-open-authorize
        let scope = self.reviewer(ctx).await?;
        // @cpt-end:cpt-cf-construct-flow-review-queue-list-open:p1:inst-open-authorize
        // @cpt-begin:cpt-cf-construct-flow-review-queue-list-open:p1:inst-open-read
        let tenant_id = ctx.subject_tenant_id();
        let page = self.requests.open_page(&scope, tenant_id, query).await?;
        self.events.record(ReviewQueueEvent::Listed {
            tenant_id,
            by: ctx.subject_id(),
            request_ids: page.items.iter().map(|request| request.id).collect(),
        });
        Ok(page)
        // @cpt-end:cpt-cf-construct-flow-review-queue-list-open:p1:inst-open-read
    }

    /// Resolve an open request of the caller's tenant. The request is
    /// claimed before the fact is written, so two reviewers never both
    /// resolve it, and what is about to be written is recorded on the claim
    /// first; a resolution that stores nothing gives the claim back.
    ///
    /// NOT cancel-safe: a drop after a delete or a correction was recorded
    /// leaves the request claimed until [`CLAIM_HOLD`] has passed; its next
    /// resolution closes it as the dropped one when that one's write is
    /// there, and goes on as asked when it is not.
    ///
    /// # Errors
    ///
    /// [`DomainError::Forbidden`] without the permission, or for a request
    /// on the caller's own fact; [`DomainError::NoSuchReviewRequest`] for an
    /// unknown request; [`DomainError::ReviewConflict`] for a closed one, one
    /// another reviewer is resolving, or one written but not closed;
    /// [`DomainError::InvalidValue`] for a correction of another property or
    /// a value its schema does not take; [`DomainError::NotStored`] when the
    /// checks or Admission refuse a correction; not configured for a
    /// correction without the opt-in, or without a processing identity;
    /// unavailable when the profile kept changing.
    #[tracing::instrument(skip_all, fields(tenant_id = %ctx.subject_tenant_id(), %request_id, caller = %ctx.subject_id()))]
    pub async fn resolve(
        &self,
        ctx: &SecurityContext,
        request_id: Uuid,
        resolution: Resolution,
    ) -> Result<ReviewRequest, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-read
        let deadline = self.writes.deadline();
        let scope = self.reviewer(ctx).await?;
        let reviewer = ctx.subject_id();
        let key = RequestKey {
            tenant_id: ctx.subject_tenant_id(),
            id: request_id,
        };
        let request = self.requests.find(&scope, key).await?.ok_or_else(|| {
            DomainError::NoSuchReviewRequest(format!(
                "tenant {} has no review request {request_id}",
                key.tenant_id
            ))
        })?;
        if !request.is_open() {
            return Err(DomainError::ReviewConflict(ReviewConflict::Closed(
                request_id,
            )));
        }
        if request.subject_id == reviewer {
            return Err(DomainError::forbidden(format!(
                "reviewer {reviewer} may not resolve review request {request_id} on its own fact"
            )));
        }
        let hold = Hold {
            requests: self.requests.as_ref(),
            scope: &scope,
            key,
            claim: ClaimId(Uuid::new_v4()),
            reviewer,
        };
        let (request, pending) = match self
            .requests
            .claim(
                &scope,
                key,
                hold.claim,
                OffsetDateTime::now_utc(),
                CLAIM_HOLD,
            )
            .await?
        {
            Claimed::Taken { request, pending } => (*request, pending),
            Claimed::Closed => {
                return Err(DomainError::ReviewConflict(ReviewConflict::Closed(
                    request_id,
                )));
            }
            Claimed::Held => {
                return Err(DomainError::ReviewConflict(ReviewConflict::Held(
                    request_id,
                )));
            }
        };
        // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-read
        let closed = match self
            .decide(&request, &hold, resolution, pending, deadline)
            .await
        {
            Ok(closed) => closed,
            // Nothing was written, so another resolution may take it now.
            Err(WriteError::NotWritten(err)) => {
                if let Err(release) = self.requests.release(&scope, key, hold.claim).await {
                    tracing::warn!(
                        error = %release,
                        "construct: a review request's claim was not given back; it frees itself"
                    );
                }
                if let DomainError::NotStored(reason) = &err {
                    self.events.record(ReviewQueueEvent::CorrectionRefused {
                        tenant_id: request.tenant_id,
                        subject_id: request.subject_id,
                        request_id,
                        fact_id: request.fact_id.clone(),
                        reason: *reason,
                        by: reviewer,
                    });
                }
                return Err(err);
            }
            // The write may have landed: the claim keeps what it recorded
            // until CLAIM_HOLD, and the next resolution closes the request
            // as this one when its write is there.
            Err(WriteError::MaybeWritten(err)) => {
                tracing::error!(
                    error = %err,
                    "construct: a resolution's write failed and may have landed; the request \
                     stays held until its claim frees itself"
                );
                return Err(err);
            }
        };
        // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-return
        self.close(&hold, closed).await
        // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-return
    }

    /// Close the request `hold` holds as `closed`, and record it.
    async fn close(&self, hold: &Hold<'_>, closed: Closed) -> Result<ReviewRequest, DomainError> {
        let request_id = hold.key.id;
        match self
            .requests
            .close(hold.scope, hold.key, hold.claim, &closed)
            .await
        {
            Ok(Some(closed_request)) => {
                self.events
                    .record(ReviewQueueEvent::resolved(&closed_request, &closed));
                Ok(closed_request)
            }
            Ok(None) => self.closed_elsewhere(hold, closed).await,
            Err(err) => {
                written_not_closed(request_id, &closed, &err.to_string());
                Err(DomainError::ReviewConflict(ReviewConflict::NotClosed {
                    request_id,
                    outcome: closed.outcome,
                }))
            }
        }
    }

    /// The answer to a resolution whose claim no longer held its request
    /// when it was written: the subject's delete closed it, or the claim
    /// lapsed and another resolution took the request.
    async fn closed_elsewhere(
        &self,
        hold: &Hold<'_>,
        closed: Closed,
    ) -> Result<ReviewRequest, DomainError> {
        let request_id = hold.key.id;
        let found = match self.requests.find(hold.scope, hold.key).await {
            Ok(found) => found,
            Err(err) => {
                written_not_closed(request_id, &closed, &err.to_string());
                None
            }
        };
        let Some(found) = found else {
            return Err(DomainError::ReviewConflict(ReviewConflict::NotClosed {
                request_id,
                outcome: closed.outcome,
            }));
        };
        match &found.status {
            // The next resolution closed it as this one: done.
            ReviewStatus::Closed(other) if other.outcome == closed.outcome => Ok(found),
            ReviewStatus::Closed(_) => {
                written_not_closed(request_id, &closed, "it was closed meanwhile");
                Err(DomainError::ReviewConflict(
                    ReviewConflict::ClosedAfterWrite {
                        request_id,
                        outcome: closed.outcome,
                    },
                ))
            }
            ReviewStatus::Open { .. } => {
                written_not_closed(request_id, &closed, "its claim lapsed");
                Err(DomainError::ReviewConflict(ReviewConflict::NotClosed {
                    request_id,
                    outcome: closed.outcome,
                }))
            }
        }
    }

    /// Carry out `resolution` on the fact of the claimed `request`, and say
    /// how to close it. When an earlier resolution's write is there, it is
    /// closed as that one; a fact found gone closes it as deleted.
    async fn decide(
        &self,
        request: &ReviewRequest,
        hold: &Hold<'_>,
        resolution: Resolution,
        pending: Option<Closed>,
        deadline: Instant,
    ) -> Result<Closed, WriteError> {
        // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-settle
        if let Some(pending) = pending {
            let facts = ProfileWrites::before_write(
                deadline,
                "resolution",
                self.writes.facts(request.subject()),
            )
            .await?;
            if landed(request, &pending, &facts) {
                tracing::warn!(
                    request_id = %request.id,
                    outcome = pending.outcome.name(),
                    "construct: an earlier resolution's write is in the profile; the request \
                     closes as that one"
                );
                return Ok(pending);
            }
        }
        // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-settle
        Ok(match resolution {
            // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-reject
            Resolution::Rejected { reason } => {
                let facts = ProfileWrites::before_write(
                    deadline,
                    "rejection",
                    self.writes.facts(request.subject()),
                )
                .await?;
                if facts
                    .facts
                    .iter()
                    .any(|fact| fact.node_key == request.fact_id)
                {
                    hold.closing(ReviewOutcome::Rejected, Some(reason))
                } else {
                    found_gone(request);
                    hold.closing(ReviewOutcome::Deleted, Some(reason))
                }
            }
            // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-reject
            // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-delete
            Resolution::Deleted { reason } => {
                let closing = hold.closing(ReviewOutcome::Deleted, reason);
                hold.pend(&closing).await?;
                self.remove(request, hold.reviewer, deadline).await?;
                closing
            }
            // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-delete
            // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-correct
            Resolution::Corrected {
                property,
                value,
                reason,
            } => match self
                .correct(request, hold, &property, &value, reason.as_ref(), deadline)
                .await?
            {
                Corrected::Stored(key) => hold.closing(ReviewOutcome::Corrected(key), reason),
                Corrected::Gone => {
                    found_gone(request);
                    hold.closing(ReviewOutcome::Deleted, reason)
                }
            },
            // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-correct
        })
    }

    /// The platform's scope for resolving the caller's tenant's review
    /// requests.
    async fn reviewer(&self, ctx: &SecurityContext) -> Result<AccessScope, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-reviewer-only:p1:inst-reviewer-permission
        let tenant_id = ctx.subject_tenant_id();
        let scope = self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &REVIEW_REQUEST_RESOURCE,
                actions::RESOLVE,
                None,
                &AccessRequest::new()
                    .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id)
                    .require_constraints(true),
            )
            .await?;
        if !reaches_tenant(&scope, tenant_id) {
            return Err(DomainError::forbidden(format!(
                "the policy scope for resolving review requests does not reach tenant {tenant_id}"
            )));
        }
        Ok(scope)
        // @cpt-end:cpt-cf-construct-algo-review-queue-reviewer-only:p1:inst-reviewer-permission
    }

    /// Delete the fact under review, as the subject's delete does, with the
    /// reviewer as origin. A fact already gone is done.
    async fn remove(
        &self,
        request: &ReviewRequest,
        reviewer: Uuid,
        deadline: Instant,
    ) -> Result<(), WriteError> {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-read
        let key = request.subject();
        let (construct, _) = self.writes.construct(key.tenant_id).await?;
        // @cpt-end:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-read
        // @cpt-begin:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-write
        match self
            .writes
            .remove(
                &construct,
                key,
                &request.fact_id,
                &Origin::Reviewer { reviewer },
                deadline,
            )
            .await?
        {
            Removed::Removed(_) => {}
            Removed::Gone => found_gone(request),
        }
        Ok(())
        // @cpt-end:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-write
    }

    /// Store `value` in place of the fact under review: one replace step with
    /// a new node key and the reviewer as origin, through the checks,
    /// Admission and the writer, against the profile's version. Each write
    /// is recorded on the claim first, with its new key.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-review-queue-one-step:p1
    async fn correct(
        &self,
        request: &ReviewRequest,
        hold: &Hold<'_>,
        property: &str,
        value: &Value,
        reason: Option<&ReviewText>,
        deadline: Instant,
    ) -> Result<Corrected, WriteError> {
        let reviewer = hold.reviewer;
        // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-start
        let Some(corrections) = &self.corrections else {
            return Err(DomainError::NotConfigured(
                "a correction needs processing.run_without_sensitive_data_checks while no \
                 sensitive-data check exists"
                    .to_owned(),
            )
            .into());
        };
        if property != request.property {
            return Err(DomainError::InvalidValue(format!(
                "a correction sets the fact's own property, `{}`",
                request.property
            ))
            .into());
        }
        let spec = corrections.catalog.get(property).ok_or_else(|| {
            DomainError::internal(format!(
                "stored fact {} has property `{property}`, which no person type has",
                request.fact_id.0
            ))
        })?;
        spec.check(value).map_err(DomainError::InvalidValue)?;
        let key = request.subject();
        let (construct, scope) = self.writes.construct(key.tenant_id).await?;
        let (construct, scope) = (&construct, &scope);
        let writes = self.writes.as_ref();
        // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-start
        let corrected = writes
            .with_reruns(deadline, "correction", move || async move {
                let prepared = ProfileWrites::before_write(deadline, "correction", async move {
                    // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-read
                    let current = writes.read(construct, key).await?;
                    let Some(fact) = current
                        .facts
                        .into_iter()
                        .find(|fact| fact.node_key == request.fact_id)
                    else {
                        return Ok(Prepared::Gone);
                    };
                    // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-read
                    // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-plan
                    let new_key = NodeKey::new_in(key.tenant_id);
                    let plan = Plan {
                        tenant_id: key.tenant_id,
                        subject_id: key.subject_id,
                        origin: Origin::Reviewer { reviewer },
                        profile_version: current.version,
                        steps: vec![Step {
                            entity_kind: fact.category.name().to_owned(),
                            change: Change::ReplaceEntity {
                                old: fact.node_key,
                                key: new_key.clone(),
                                properties: vec![Property::new(fact.property, value.clone())],
                            },
                            confidence: None,
                        }],
                    };
                    // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-plan
                    // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-admit
                    let Checked::Verdicts(verdicts) = corrections.checks.run(&plan).await else {
                        return Ok(Prepared::NotStored(NotStored::Dropped(
                            DropReason::CheckFailed,
                        )));
                    };
                    let admitted = match corrections
                        .admission
                        .admit(construct, scope, plan, &verdicts)
                        .await?
                    {
                        AdmissionOutcome::Admitted(admitted) => admitted.plan,
                        AdmissionOutcome::Dropped(reason) => {
                            return Ok(Prepared::NotStored(NotStored::Dropped(reason)));
                        }
                    };
                    if admitted.steps.is_empty() {
                        return Ok(Prepared::NotStored(NotStored::Blocked));
                    }
                    // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-admit
                    Ok(Prepared::Write(Box::new(admitted), new_key))
                })
                .await?;
                let (admitted, new_key) = match prepared {
                    Prepared::Gone => return Ok(Attempt::Done(Ok(Corrected::Gone))),
                    Prepared::NotStored(why) => return Ok(Attempt::Done(Err(why))),
                    Prepared::Write(admitted, new_key) => (admitted, new_key),
                };
                // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-write
                let pending =
                    hold.closing(ReviewOutcome::Corrected(new_key.clone()), reason.cloned());
                hold.pend(&pending).await?;
                let written = writes
                    .write(construct, &admitted, |now| {
                        now.facts.iter().any(|fact| fact.node_key == new_key)
                    })
                    .await?;
                Ok(match written {
                    Written::Stored => Attempt::Done(Ok(Corrected::Stored(new_key))),
                    Written::Conflict => Attempt::Conflict,
                })
                // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-write
            })
            .await?;
        corrected.map_err(|why| DomainError::NotStored(why).into())
    }
}

/// The claim a resolution holds on its request.
struct Hold<'a> {
    requests: &'a dyn ReviewRequests,
    scope: &'a AccessScope,
    key: RequestKey,
    claim: ClaimId,
    reviewer: Uuid,
}

impl Hold<'_> {
    /// The reviewer's close of the request, now.
    fn closing(&self, outcome: ReviewOutcome, reason: Option<ReviewText>) -> Closed {
        Closed {
            outcome,
            by: self.reviewer,
            at: OffsetDateTime::now_utc(),
            reason,
        }
    }

    /// Record `pending` on the request before it is written, so a
    /// resolution that ends without closing the request is closed later as
    /// this one. Nothing is written when the claim no longer holds.
    async fn pend(&self, pending: &Closed) -> Result<(), DomainError> {
        if self
            .requests
            .pend(self.scope, self.key, self.claim, pending)
            .await?
        {
            Ok(())
        } else {
            Err(DomainError::ReviewConflict(ReviewConflict::Held(
                self.key.id,
            )))
        }
    }
}

/// Whether the write of `pending`, an earlier resolution of `request`, is
/// in `facts`: its corrected fact is there, or its deleted fact is gone.
fn landed(request: &ReviewRequest, pending: &Closed, facts: &SubjectFacts) -> bool {
    let has = |key: &NodeKey| facts.facts.iter().any(|fact| &fact.node_key == key);
    match &pending.outcome {
        ReviewOutcome::Corrected(new) => has(new),
        ReviewOutcome::Deleted => !has(&request.fact_id),
        ReviewOutcome::Rejected => false,
    }
}

/// Whether some constraint of `scope` grants the whole tenant: it has
/// filters, and each names `tenant_id` as the owner tenant. A reviewer
/// resolves a whole tenant's requests, so a constraint that names anything
/// narrower, or a group or subtree that cannot be decided here, does not
/// make one.
pub(crate) fn reaches_tenant(scope: &AccessScope, tenant_id: Uuid) -> bool {
    let tenant = ScopeValue::Uuid(tenant_id);
    scope.constraints().iter().any(|constraint| {
        !constraint.filters().is_empty()
            && constraint.filters().iter().all(|filter| {
                matches!(filter, ScopeFilter::Eq(_) | ScopeFilter::In(_))
                    && filter.property() == pep_properties::OWNER_TENANT_ID
                    && filter.values().contains(&tenant)
            })
    })
}

/// Log a resolution that found its fact gone: the subject's delete, or a
/// record that landed in the moment before the request opened, removed it.
fn found_gone(request: &ReviewRequest) {
    tracing::warn!(
        request_id = %request.id,
        fact_id = %request.fact_id.0,
        "construct: a resolution found its fact gone; the request closes as deleted"
    );
}

/// Log a resolution that was written, or may have been, but whose request
/// was not closed.
fn written_not_closed(request_id: Uuid, closed: &Closed, why: &str) {
    tracing::error!(
        %request_id,
        outcome = closed.outcome.name(),
        corrected_fact_id = closed
            .outcome
            .corrected_fact_id()
            .map(|key| tracing::field::display(&key.0)),
        why,
        "construct: a resolution was written, but its request was not closed"
    );
}

fn no_such_fact(fact_id: &NodeKey) -> DomainError {
    DomainError::NoSuchFact(format!("the subject has no live fact {}", fact_id.0))
}

#[cfg(test)]
mod tests {
    use toolkit_security::ScopeConstraint;

    use super::*;

    #[test]
    fn a_review_text_is_trimmed_and_bounded_in_characters_not_bytes() {
        let at_limit = "\u{e9}".repeat(MAX_TEXT_CHARS);

        assert_eq!(ReviewText::new("  why  ").unwrap().as_str(), "why");
        assert_eq!(
            ReviewText::new(at_limit.clone()).unwrap().as_str(),
            at_limit
        );
        assert_eq!(
            ReviewText::new(format!("{at_limit}\u{e9}")),
            Err(TextRule::TooLong)
        );
        assert_eq!(ReviewText::new(" \n "), Err(TextRule::Blank));
        assert_eq!(ReviewText::optional(Some("  ".to_owned())), Ok(None));
        assert_eq!(ReviewText::optional(None), Ok(None));
    }

    fn scope_of(constraints: Vec<Vec<ScopeFilter>>) -> AccessScope {
        AccessScope::from_constraints(
            constraints
                .into_iter()
                .map(|filters| ScopeConstraint::try_new(filters).unwrap())
                .collect(),
        )
    }

    #[test]
    fn only_a_constraint_on_the_whole_tenant_makes_a_reviewer() {
        let tenant = Uuid::new_v4();
        let on = |id: Uuid| ScopeFilter::eq(pep_properties::OWNER_TENANT_ID, id);

        assert!(reaches_tenant(&scope_of(vec![vec![on(tenant)]]), tenant));
        assert!(
            reaches_tenant(
                &scope_of(vec![vec![on(Uuid::new_v4())], vec![on(tenant)]]),
                tenant
            ),
            "one constraint is enough"
        );
        assert!(!reaches_tenant(
            &scope_of(vec![vec![on(Uuid::new_v4())]]),
            tenant
        ));
        assert!(
            !reaches_tenant(
                &scope_of(vec![vec![
                    on(tenant),
                    ScopeFilter::eq(pep_properties::OWNER_ID, Uuid::new_v4())
                ]]),
                tenant
            ),
            "narrower than the tenant"
        );
        assert!(
            !reaches_tenant(
                &scope_of(vec![vec![ScopeFilter::in_tenant_subtree(
                    pep_properties::OWNER_TENANT_ID,
                    tenant,
                    false,
                    Vec::new()
                )]]),
                tenant
            ),
            "a subtree is not decided here"
        );
        assert!(!reaches_tenant(&AccessScope::deny_all(), tenant));
        assert!(
            !reaches_tenant(&AccessScope::allow_all(), tenant),
            "an allow without constraints is no grant"
        );
    }
}
