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
use parking_lot::Mutex;
use serde_json::Value;
use time::OffsetDateTime;
use tokio::time::Instant;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, ScopeFilter, ScopeValue, SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
pub use super::one_step::NotStored;
use super::one_step::{Gone, NewFact, OneStepDone, OneStepPath};
use super::plan::{NodeKey, Origin};
use super::profile::Category;
use super::profile_writes::{
    ProfileWrites, Removed, SubjectFacts, WRITE_BUDGET, WRITER_LIMIT, WriteError,
};
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

    /// The open request on the subject's fact, if there is one, with what
    /// its claim recorded as about to be written.
    async fn open_request_on(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        fact_id: &NodeKey,
    ) -> Result<Option<(ReviewRequest, Option<Closed>)>, DomainError>;

    /// Close the open request `key`, claimed or not, while what its claim
    /// recorded is still `recorded`. None when it is not.
    async fn close_open(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        recorded: Option<&Closed>,
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

/// A change of a fact outside a resolution: who made it, the outcome the
/// fact's open request closes as, and whether the change's own write
/// removed or replaced the fact.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OutsideChange {
    pub by: Uuid,
    pub outcome: ReviewOutcome,
    pub changed: bool,
}

/// Close the open request on a fact changed or deleted outside a
/// resolution as `change` says, and record it. Whether there was one, and
/// it was closed.
///
/// A reviewer's resolution whose write is in the profile closes the request
/// as that resolution instead, so a change outside a resolution never
/// overwrites it. When the change's own write removed or replaced the fact,
/// no reviewer's write had: each is written against the profile's version.
/// A close is taken only while the claim's record is what was read, so a
/// resolution recording its write meanwhile keeps the request.
///
/// # Errors
///
/// When the request store or the profile cannot be read.
///
/// @cpt-dod:cpt-cf-construct-dod-review-queue-subject-control:p1
pub(crate) async fn close_on_change(
    requests: &dyn ReviewRequests,
    events: &dyn ReviewQueueEvents,
    writes: &ProfileWrites,
    scope: &AccessScope,
    key: SubjectKey,
    fact_id: &NodeKey,
    change: OutsideChange,
) -> Result<bool, DomainError> {
    let Some((request, recorded)) = requests.open_request_on(scope, key, fact_id).await? else {
        return Ok(false);
    };
    let by_caller = Closed {
        outcome: change.outcome,
        by: change.by,
        at: OffsetDateTime::now_utc(),
        reason: None,
    };
    let closed = match &recorded {
        Some(pending)
            if !change.changed && landed(&request, pending, &writes.facts(key).await?) =>
        {
            tracing::warn!(
                request_id = %request.id,
                outcome = pending.outcome.name(),
                "construct: a reviewer's write is in the profile; the request closes as that one"
            );
            pending.clone()
        }
        _ => by_caller,
    };
    let Some(request) = requests
        .close_open(scope, request.key(), recorded.as_ref(), &closed)
        .await?
    else {
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

/// What a correction did.
#[domain_model]
enum Corrected {
    Stored(NewFact),
    /// The fact is gone; nothing was stored.
    Gone,
}

/// Who resolves a request: a reviewer, or a tenant administrator whose own
/// edit or delete of the fact closes it. Its id closes the request, and the
/// origin it names goes with what it writes.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolver {
    Reviewer(Uuid),
    Administrator(Uuid),
}

impl Resolver {
    /// Who closes the request.
    pub(crate) const fn by(self) -> Uuid {
        match self {
            Self::Reviewer(id) | Self::Administrator(id) => id,
        }
    }

    /// The origin of what the resolution writes.
    pub(crate) const fn origin(self) -> Origin {
        match self {
            Self::Reviewer(reviewer) => Origin::Reviewer { reviewer },
            Self::Administrator(administrator) => Origin::Administrator { administrator },
        }
    }
}

/// What a resolution wrote itself. An earlier resolution's write it closed
/// the request as, or a fact it found gone, is nothing.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Wrote {
    Nothing,
    /// The correction stored this fact.
    Corrected(NewFact),
    /// The delete removed the fact.
    Removed,
}

/// What a resolution wrote, and how its close ended: the closed request, or
/// the conflict that says it was written but not closed.
#[domain_model]
#[derive(Debug)]
pub(crate) struct Settled {
    pub wrote: Wrote,
    pub closed: Result<ReviewRequest, DomainError>,
}

/// @cpt-dod:cpt-cf-construct-dod-review-queue-subject:p1
#[domain_model]
pub struct ReviewQueueService {
    writes: Arc<ProfileWrites>,
    requests: Arc<dyn ReviewRequests>,
    events: Arc<dyn ReviewQueueEvents>,
    one_step: Option<Arc<OneStepPath>>,
    policy_enforcer: PolicyEnforcer,
}

impl ReviewQueueService {
    #[must_use]
    pub fn new(
        writes: Arc<ProfileWrites>,
        requests: Arc<dyn ReviewRequests>,
        events: Arc<dyn ReviewQueueEvents>,
        one_step: Option<Arc<OneStepPath>>,
        policy_enforcer: PolicyEnforcer,
    ) -> Self {
        Self {
            writes,
            requests,
            events,
            one_step,
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
            close_on_change(
                self.requests.as_ref(),
                self.events.as_ref(),
                &self.writes,
                &scope,
                key,
                fact_id,
                OutsideChange {
                    by: subject_id,
                    outcome: ReviewOutcome::Deleted,
                    changed: false,
                },
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
        // The budget bounds the permission check and the lookup too, as
        // every step before a write.
        let deadline = self.writes.deadline();
        let scope = ProfileWrites::before_write(deadline, "resolution", self.reviewer(ctx)).await?;
        let reviewer = ctx.subject_id();
        let key = RequestKey {
            tenant_id: ctx.subject_tenant_id(),
            id: request_id,
        };
        let request =
            ProfileWrites::before_write(deadline, "resolution", self.requests.find(&scope, key))
                .await?
                .ok_or_else(|| {
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
        // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-read
        self.settle(
            &scope,
            key,
            Resolver::Reviewer(reviewer),
            resolution,
            deadline,
        )
        .await?
        .closed
    }

    /// Claim the open request `key` for `resolver`, carry out `resolution`
    /// on its fact before `deadline`, and close it: a reviewer's resolution,
    /// or an administrator's edit or delete of a fact under review, which
    /// spends the budget the edit or delete started. What is about to be
    /// written is recorded on the claim first; a resolution that stores
    /// nothing gives the claim back.
    ///
    /// NOT cancel-safe, as [`Self::resolve`].
    ///
    /// # Errors
    ///
    /// As [`Self::resolve`], after the caller is authorized, when nothing was
    /// written or the write may have landed. A written resolution whose
    /// request did not close answers in [`Settled::closed`].
    pub(crate) async fn settle(
        &self,
        scope: &AccessScope,
        key: RequestKey,
        resolver: Resolver,
        resolution: Resolution,
        deadline: Instant,
    ) -> Result<Settled, DomainError> {
        // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-claim
        let request_id = key.id;
        let hold = Hold {
            requests: self.requests.as_ref(),
            scope,
            key,
            claim: ClaimId(Uuid::new_v4()),
            resolver,
            deadline,
            pended: Mutex::new(None),
        };
        // A claim answered after the budget may have been taken: it holds
        // the request, with what an earlier resolution recorded, until
        // CLAIM_HOLD, and the next resolution settles it.
        let claimed = ProfileWrites::before_write(
            deadline,
            "resolution",
            self.requests.claim(
                scope,
                key,
                hold.claim,
                OffsetDateTime::now_utc(),
                CLAIM_HOLD,
            ),
        )
        .await?;
        let (request, pending) = match claimed {
            Claimed::Taken { request, pending } => {
                // An earlier resolution's record stays with the claim until
                // it is settled.
                hold.pended.lock().clone_from(&pending);
                (*request, pending)
            }
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
        // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-claim
        let (closed, wrote) = match self
            .decide(&request, &hold, resolution, pending, deadline)
            .await
        {
            Ok(decided) => decided,
            // Nothing was written, so another resolution may take it now.
            Err(WriteError::NotWritten(err)) => {
                hold.release().await;
                // An administrator's refused edit is recorded by its own
                // service, once.
                if let (DomainError::NotStored(reason), Resolver::Reviewer(by)) =
                    (&err, hold.resolver)
                {
                    self.events.record(ReviewQueueEvent::CorrectionRefused {
                        tenant_id: request.tenant_id,
                        subject_id: request.subject_id,
                        request_id,
                        fact_id: request.fact_id.clone(),
                        reason: *reason,
                        by,
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
                // Not a plain 503: a retry now finds the request held, so
                // the answer says to resolve it again after the hold.
                return Err(match hold.pended() {
                    Some(pended) => DomainError::ReviewConflict(ReviewConflict::NotClosed {
                        request_id,
                        outcome: pended.outcome,
                    }),
                    None => err,
                });
            }
        };
        // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-return
        Ok(Settled {
            wrote,
            closed: self.close(&hold, closed).await,
        })
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
            // Nothing was written or is to be settled, as for a rejection:
            // the claim is given back, so the request can be resolved again
            // now.
            Err(err) if hold.pended().is_none() => {
                tracing::warn!(
                    error = %err,
                    %request_id,
                    "construct: a resolution that wrote nothing was not closed; its claim is \
                     given back"
                );
                hold.release().await;
                Err(err)
            }
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
    ) -> Result<(Closed, Wrote), WriteError> {
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
                return Ok((pending, Wrote::Nothing));
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
                    (
                        hold.closing(ReviewOutcome::Rejected, Some(reason)),
                        Wrote::Nothing,
                    )
                } else {
                    found_gone(request);
                    (
                        hold.closing(ReviewOutcome::Deleted, Some(reason)),
                        Wrote::Nothing,
                    )
                }
            }
            // @cpt-end:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-reject
            // @cpt-begin:cpt-cf-construct-flow-review-queue-resolve:p1:inst-resolve-delete
            Resolution::Deleted { reason } => {
                let closing = hold.closing(ReviewOutcome::Deleted, reason);
                hold.pend(&closing).await?;
                let wrote = if self
                    .remove(request, &hold.resolver.origin(), deadline)
                    .await?
                {
                    Wrote::Removed
                } else {
                    Wrote::Nothing
                };
                (closing, wrote)
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
                Corrected::Stored(fact) => (
                    hold.closing(ReviewOutcome::Corrected(fact.node_key.clone()), reason),
                    Wrote::Corrected(fact),
                ),
                // Closed as deleted, which nobody wrote here: the reason
                // says so, when the resolver gave none.
                Corrected::Gone => {
                    found_gone(request);
                    (
                        hold.closing(
                            ReviewOutcome::Deleted,
                            reason.or_else(|| Some(gone_before())),
                        ),
                        Wrote::Nothing,
                    )
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
    /// resolver as origin. A fact already gone is done. Whether this delete
    /// removed it.
    async fn remove(
        &self,
        request: &ReviewRequest,
        origin: &Origin,
        deadline: Instant,
    ) -> Result<bool, WriteError> {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-read
        let key = request.subject();
        let (construct, _) = self.writes.construct(key.tenant_id).await?;
        // @cpt-end:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-read
        // @cpt-begin:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-write
        match self
            .writes
            .remove(&construct, key, &request.fact_id, origin, deadline)
            .await?
        {
            Removed::Removed(_) => Ok(true),
            Removed::Gone => {
                found_gone(request);
                Ok(false)
            }
        }
        // @cpt-end:cpt-cf-construct-algo-review-queue-remove:p1:inst-remove-write
    }

    /// Store `value` in place of the fact under review: one replace step with
    /// a new node key and the resolver as origin, through the one-step plan
    /// path, against the profile's version. Each write is recorded on the
    /// claim first, with its new key.
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
        let Some(one_step) = &self.one_step else {
            return Err(DomainError::NotConfigured(
                "a correction needs processing.run_without_sensitive_data_checks while no \
                 sensitive-data check exists"
                    .to_owned(),
            )
            .into());
        };
        if property != request.property {
            return Err(DomainError::InvalidValue {
                field: "property",
                rule: format!(
                    "a correction sets the fact's own property, `{}`",
                    request.property
                ),
            }
            .into());
        }
        let done = one_step
            .replace(
                &self.writes,
                request.subject(),
                &hold.resolver.origin(),
                "correction",
                &request.fact_id,
                property,
                value,
                deadline,
                |new_key| {
                    let pending =
                        hold.closing(ReviewOutcome::Corrected(new_key.clone()), reason.cloned());
                    async move { hold.pend(&pending).await }
                },
            )
            .await?;
        match done {
            OneStepDone::Stored(fact) => Ok(Corrected::Stored(fact)),
            OneStepDone::Skipped(Gone) => Ok(Corrected::Gone),
            OneStepDone::NotStored(why) => Err(DomainError::NotStored(why).into()),
        }
    }

    /// Close the open request on a fact `by` replaced outside a resolution
    /// as `outcome`, when no resolution holds it or recorded a write on it.
    /// A request a reviewer holds is left to its resolution, which finds the
    /// fact gone; one with a recorded write is left to the next resolution,
    /// which settles the record. Whether one closed.
    ///
    /// # Errors
    ///
    /// When the request store cannot be reached. A close that failed gives
    /// its claim back first.
    pub(crate) async fn close_unclaimed(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        fact_id: &NodeKey,
        by: Uuid,
        outcome: ReviewOutcome,
    ) -> Result<bool, DomainError> {
        let Some((request_key, claim)) = self.claim_unclaimed(scope, key, fact_id).await? else {
            return Ok(false);
        };
        let closed = Closed {
            outcome,
            by,
            at: OffsetDateTime::now_utc(),
            reason: None,
        };
        let closed_request = match self
            .requests
            .close(scope, request_key, claim, &closed)
            .await
        {
            Ok(Some(closed_request)) => closed_request,
            Ok(None) => return Ok(false),
            // The claim recorded nothing, so it is given back: a reviewer
            // can resolve the request now, not only after CLAIM_HOLD.
            Err(err) => {
                if let Err(release) = self.requests.release(scope, request_key, claim).await {
                    tracing::warn!(
                        error = %release,
                        request_id = %request_key.id,
                        "construct: a review request's claim was not given back; it frees itself"
                    );
                }
                return Err(err);
            }
        };
        self.events
            .record(ReviewQueueEvent::resolved(&closed_request, &closed));
        Ok(true)
    }

    /// A claim on the open request on the fact, when no resolution holds it
    /// or recorded a write on it.
    ///
    /// A claim taken over a recorded write would take over the record, and
    /// giving it back forgets the record: so none is taken over one, and one
    /// that finds a record written after the read is not given back. It
    /// lapses after [`CLAIM_HOLD`], and the next resolution settles the
    /// record.
    async fn claim_unclaimed(
        &self,
        scope: &AccessScope,
        key: SubjectKey,
        fact_id: &NodeKey,
    ) -> Result<Option<(RequestKey, ClaimId)>, DomainError> {
        let Some((request, recorded)) = self.requests.open_request_on(scope, key, fact_id).await?
        else {
            return Ok(None);
        };
        let claim = ClaimId(Uuid::new_v4());
        let claimed = match recorded {
            Some(_) => None,
            None => Some(
                self.requests
                    .claim(
                        scope,
                        request.key(),
                        claim,
                        OffsetDateTime::now_utc(),
                        CLAIM_HOLD,
                    )
                    .await?,
            ),
        };
        let left_to = match claimed {
            Some(Claimed::Taken { pending: None, .. }) => {
                return Ok(Some((request.key(), claim)));
            }
            None | Some(Claimed::Taken { .. }) => "the next resolution, which settles its record",
            Some(Claimed::Closed | Claimed::Held) => "the resolution that holds it",
        };
        tracing::info!(
            request_id = %request.id,
            left_to,
            "construct: a request on a replaced fact is left to a resolution"
        );
        Ok(None)
    }

    /// The review requests this service resolves and closes.
    pub(crate) fn requests(&self) -> &dyn ReviewRequests {
        self.requests.as_ref()
    }

    /// Where this service records what it did.
    pub(crate) fn events(&self) -> &dyn ReviewQueueEvents {
        self.events.as_ref()
    }
}

/// The claim a resolution holds on its request.
struct Hold<'a> {
    requests: &'a dyn ReviewRequests,
    scope: &'a AccessScope,
    key: RequestKey,
    claim: ClaimId,
    resolver: Resolver,
    /// The end of the resolution's budget, which bounds each record.
    deadline: Instant,
    /// What the request records as about to be written: this resolution's
    /// last [`Hold::pend`], or an earlier resolution's record the claim took
    /// over. None when nothing was written or is to be settled.
    pended: Mutex<Option<Closed>>,
}

impl Hold<'_> {
    /// The resolver's close of the request, now.
    fn closing(&self, outcome: ReviewOutcome, reason: Option<ReviewText>) -> Closed {
        Closed {
            outcome,
            by: self.resolver.by(),
            at: OffsetDateTime::now_utc(),
            reason,
        }
    }

    /// Record `pending` on the request before it is written, so a
    /// resolution that ends without closing the request is closed later as
    /// this one. Nothing is written when the claim no longer holds, or when
    /// the record is not answered within the budget: the profile has not
    /// been written yet, so the resolution gives the claim back.
    async fn pend(&self, pending: &Closed) -> Result<(), DomainError> {
        if ProfileWrites::before_write(
            self.deadline,
            "resolution",
            self.requests
                .pend(self.scope, self.key, self.claim, pending),
        )
        .await?
        {
            *self.pended.lock() = Some(pending.clone());
            return Ok(());
        }
        // The claim no longer holds: the request was closed meanwhile, by
        // the subject's delete, or another resolution took it after the
        // claim lapsed. The answer tells the two apart.
        let open = ProfileWrites::before_write(
            self.deadline,
            "resolution",
            self.requests.find(self.scope, self.key),
        )
        .await?
        .is_some_and(|request| request.is_open());
        Err(DomainError::ReviewConflict(if open {
            ReviewConflict::Held(self.key.id)
        } else {
            ReviewConflict::Closed(self.key.id)
        }))
    }

    /// What the request records as about to be written, if anything.
    fn pended(&self) -> Option<Closed> {
        self.pended.lock().clone()
    }

    /// Give the claim back. A failure is logged: the claim frees itself
    /// after [`CLAIM_HOLD`].
    async fn release(&self) {
        if let Err(err) = self
            .requests
            .release(self.scope, self.key, self.claim)
            .await
        {
            tracing::warn!(
                error = %err,
                "construct: a review request's claim was not given back; it frees itself"
            );
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

/// The reason a correction that found its fact gone closes with: it
/// deleted nothing.
fn gone_before() -> ReviewText {
    ReviewText("the fact was gone before the correction; nothing was written".to_owned())
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
